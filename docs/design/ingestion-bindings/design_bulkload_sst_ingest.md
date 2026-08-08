# Design: Bulk Load via SST Ingestion

Status: implemented (Phase 1 + Phase 2 external sort complete)

---


> **⚠️ API superseded.** This document describes the original standalone
> `SstBulkLoader` API. The current user-facing API is the graph-based session
> `graph.open_bulk_loader()` — see [`docs/api/design_bulk_loader.md`](../api/design_bulk_loader.md).
> The internal pipeline is the same; only the public interface has changed.
- Zero WAL overhead; zero memtable pressure; zero compaction during load
- Atomic visibility — data appears all-at-once
- Scale to datasets exceeding RAM via external merge sort

**Non-goals:**
- Online concurrent writes during load
- Automatic schema inference — caller declares labels and prop keys upfront

---

## API surface

```rust
pub struct SstBulkLoader {
    db_path:          PathBuf,
    work_dir:         PathBuf,   // temp directory for chunk/SST files
    max_memory_bytes: usize,     // ExternalSorter spill threshold; default 512 MiB
    max_sst_size:     usize,     // default: 90% of target_file_size_base (~58 MiB)
}

impl SstBulkLoader {
    pub fn new(db_path, work_dir) -> Self;
    pub fn with_max_memory(self, bytes: usize) -> Self;
    pub fn with_max_sst_size(self, bytes: usize) -> Self;

    pub fn load_initial(
        self,
        schema:     BulkSchema,
        vertices:   impl Iterator<Item = BulkVertex>,
        edges:      impl Iterator<Item = BulkEdge>,
        graph_opts: GraphOptions,   // edge_mode, schema_mode
        storage_opts: &RocksOptions,  // block_cache, write_buffer, etc.
    ) -> Result<BulkLoadStats, StoreError>;
}
```

**Input types:**

```rust
pub struct BulkSchema {
    pub vertex_labels: Vec<String>,
    pub edge_labels:   Vec<String>,
    pub prop_keys:     Vec<(String, DataType)>,   // IDs 1–3 reserved (id/label/rank)
}

pub struct BulkVertex {
    pub id: VertexKey, pub label: String,
    pub props: HashMap<String, Primitive>,
}

pub struct BulkEdge {
    pub src: VertexKey, pub dst: VertexKey, pub label: String,
    pub props: HashMap<String, Primitive>,
    /// `None` = auto-assign rank (Multi mode only).
    /// `Some(r)` = explicit rank; `r` must be < `u16::MAX` (65535 is reserved sentinel).
    /// Ignored in Single mode (rank always 0).
    pub rank: Option<Rank>,
}
```

---

## How it works

### The problem with transactional writes

At 90K edges/s, each edge costs ~11μs in OCC transaction overhead (begin →
coalesce src → coalesce dst → write edge → commit).  For 1B edges this is
~3 hours.  The bottleneck is not disk I/O — it's per-transaction overhead.

### How SST ingestion bypasses the write path

```
┌─────────────────────┐        ┌──────────────────────┐
│ SSTFileWriter       │        │ RocksDB instance      │
│ open(path)          │        │                       │
│ put(key1, val1)     │  ───►  │  (not visible yet)    │
│ put(key2, val2)     │        │                       │
│ finish()            │        │                       │
└─────────────────────┘        │                       │
                               │                       │
IngestExternalFile([path]) ──► │  atomic link-in       │
                               │  (now visible)        │
                               └──────────────────────┘
```

Keys in each SST file must be in **strictly ascending** order.  Multiple SST
files with non-overlapping key ranges can be ingested simultaneously.
`IngestExternalFile` is atomic: all files link in or none do.

### Pipeline

Both `EdgeMode::Single` and `EdgeMode::Multi` share the same vertex/degree/ingest
phases; they differ only in how edges are sorted and ranked.  All input is fully
streamed — there are no in-memory per-vertex or per-edge maps.

```
  INPUT: vertices iterator (streamed)    INPUT: edges iterator (streamed)
              │                                          │
   ┌──────────┴──────────────────────────────────────────┴──────────────┐
   │  Write schema CF (atomic WriteBatch + BULK_LOAD_IN_PROGRESS)        │
   └────────────────────────────────┬────────────────────────────────────┘
                                    │
   ┌────────────────────────────────┴────────────────────────────────────┐
   │  PHASE 1a — Stream vertices                                          │
   │  • Each vertex → vertex_sorter (for vertex CF SST)                  │
   │  • Each vertex → label_sorter  (for end_vertex_label annotation)    │
   │  • label_sorter → SortedLabelFile (deduped, on disk)                │
   └────────────────────────────────┬────────────────────────────────────┘
                                    │
   ┌────────────────────────────────┴────────────────────────────────────┐
   │  PHASE 1b — Stream edges                                             │
   │  • Each edge → dst_annot_sorter: (dst_id:8, out_edge_key:22)        │
   │  • Each edge → src_annot_sorter: (src_id:8, in_edge_key:22)         │
   │  • Each edge → out_deg_sorter (src_id:8) + in_deg_sorter (dst_id:8) │
   │  Multi only: edges → pre_sorter → rank assignment → annotation sorters │
   └────────────────────────────────┬────────────────────────────────────┘
                                    │
   ┌────────────────────────────────┴────────────────────────────────────┐
   │  PHASE 2 — Sort + write SST files (all sequential, full budget)     │
   │  • vertex_sorter  → vertices CF SST                                  │
   │  • (label_file, out_deg, in_deg) three-way merge → degree CF SST    │
   │  • dst_annot → sort-merge-join with label_file → out_edge_sorter    │
   │                → edges_out CF SST (+ Single-mode dedup check)       │
   │  • src_annot → sort-merge-join with label_file → in_edge_sorter     │
   │                → edges_in CF SST                                     │
   └────────────────────────────────┬────────────────────────────────────┘
                                    │
   ┌────────────────────────────────┴────────────────────────────────────┐
   │  PHASE 3 — IngestExternalFile (atomic)                               │
   │  All CFs simultaneously → data becomes visible all-at-once           │
   │  WorkDirGuard disarmed; work_dir removed on success                  │
   └─────────────────────────────────────────────────────────────────────┘
```

### Concrete walkthrough: LiveJournal 10M (measured)

```
Schema CF write:     one WriteBatch — meta + schema entries.          < 0.1s
Phase 1a (vertices): stream 4.85M vertices → vertex_sorter + label_sorter
                     → SortedLabelFile (58 MB on disk)                  1.7s
Phase 1b (edges):    stream 69M edges → 4 annotation/degree sorters   102s
Phase 2 (SSTs):      vertex SSTs + degree SSTs (three-way merge)         3s
                     edges_out annotate + sort + SST                    26s
                     edges_in  annotate + sort + SST                    24s
Phase 3 (ingest):    IngestExternalFile 53 SST files atomically        0.2s
─────────────────────────────────────────────────────────────────────────────
Total: ~193s for 69M edges, 4.85M vertices → ~358K edges/s
Peak memory: ~1.2 GB (sorter buffers + SortedLabelFile; no in-memory maps)
```

---

## Key design details

### Sort strategy

`ExternalSorter` is always used for edges.  It spills sorted chunks to disk when
the in-memory flat buffer exceeds `max_memory_bytes`, then K-way merges all chunks.
If only one chunk was ever written (dataset fits in the buffer), no disk merge is
needed.

| Buffer state | Strategy | Peak memory |
|---|---|---|
| All data fits in buffer | In-memory flat-buffer sort → SST write | ≤ `max_memory_bytes` |
| Spills occurred | Chunk files → cascaded K-way merge (≤ 128 open at once) → SST write | ~`max_memory_bytes / 4` per sorter |

**Flat buffer**: keys and values are appended to a single `Vec<u8>` (`raw_data`);
sorting operates on an `offsets: Vec<(usize, usize, usize, usize)>` index with zero
extra allocations during sort.

**Cascaded merge**: if chunk count exceeds `MAX_OPEN_CHUNKS` (128), intermediate
merge passes reduce fan-in before the final K-way merge, preventing OS
file-descriptor exhaustion on very large datasets.

**Budget allocation**: sorters do not all run concurrently.  The budget is split
across the sorters active at the same time:

| Phase | Concurrent sorters | Budget each |
|---|---|---|
| 1a — vertex streaming | `vertex_sorter` + `label_sorter` | `max_memory / 4` |
| 1b — Single edge streaming | `dst_annot` + `src_annot` + `out_deg` + `in_deg` | `/4`, `/4`, `/8`, `/8` |
| 1b — Multi edge streaming | `pre_sorter` only | `max_memory / 4` |
| Phase 2 — annotation sorters | one at a time (sequential) | **full `max_memory`** |

Phase 2 sorters run sequentially so each gets the full memory budget, minimising
disk spills during the annotation passes.

### `end_vertex_label` — why it matters

`EdgeValue` carries a 4-byte `end_vertex_label` prefix (dst label for `edges_out`,
src label for `edges_in`).  This powers the vertex-label-from-edge-prefix
optimisation: during edge scans, the engine reads the adjacent vertex's label
for free without loading the vertex record.  A wrong value silently disables
this optimisation permanently.

**Sort-merge join approach (implemented):**  During Phase 1a, each vertex also
pushes a compact `(vertex_id:8, label_id:4)` record to `label_sorter`.  This is
materialised into `SortedLabelFile` (12 bytes/vertex on disk, deduplicated).

During Phase 2 annotation, two sort-merge joins are performed:
- `dst_annot_sorter` output (sorted by `dst_id`) ← join with `SortedLabelFile` → attach `dst_label` to each `edges_out` record
- `src_annot_sorter` output (sorted by `src_id`) ← join with `SortedLabelFile` → attach `src_label` to each `edges_in` record

`SortedLabelFile` is read twice (once per direction) from disk using independent
sequential readers.  Both joins are O(V + E) time and O(1) memory.  If an edge
references a vertex not in `SortedLabelFile`, the join fails immediately with
`StoreError::SchemaViolation`.

### SST file size

RocksDB compacts files at or above `target_file_size_base`.  **Generate SSTs at
90% of this value** (e.g., 230 MB for a 256 MB target) to avoid triggering
immediate compaction.

### Schema CF — ID assignment

| Namespace | Reserved IDs | User IDs start at |
|---|---|---|
| Property keys | 1 = `id`, 2 = `label`, 3 = `rank` | 4 |
| Vertex labels | — | 1 |
| Edge labels | — | 1 |

IDs are assigned sequentially in declaration order.  Schema CF entries are
written as a single `WriteBatch` before any data SST is generated — this is
required because `LabelId` and prop-key `u16` values are embedded in
vertex/edge values.

### `SchemaMode` and `EdgeMode`

| Mode | Bulk load behaviour |
|---|---|
| **Strict** | All labels/prop keys must be declared in `BulkSchema`.  Unknown names → hard error in Pass 1, before any SST is written. |
| **Auto** | Labels not in `BulkSchema` are silently registered during Pass 1. |
| **Single** | All ranks = 0. Duplicate `(src, label, dst)` → `DuplicateEdge` (detected via consecutive key check during sorted SST write). |
| **Multi** | Supports both auto-assigned and explicit ranks in the same group. Sort key is `(src, label, dst, rank_or_sentinel)` where `rank_or_sentinel = r` for `Some(r)` or `u16::MAX` for `None`. Explicit ranks (sorted first) are used as-is; auto-rank edges (sentinel, sorted last) get ranks starting after the highest explicit rank. Consecutive duplicate explicit ranks → `DuplicateEdge`. **`Rank::MAX` (65535) is reserved as the auto-assign sentinel and must not be used as an explicit rank.** |

### Property encoding

String property names in `BulkVertex::props` / `BulkEdge::props` are resolved
to `u16` IDs via `ResolvedSchema` during encoding, then passed to
`prop_codec::encode_props` — identical to the transactional path.

---

## Crash recovery

### Atomicity guarantee

The design ensures the database is **never in a partially-loaded state visible
to readers**:

- Every phase before `IngestExternalFile` writes only to `work_dir` (temp files).
  The live DB is untouched.
- `IngestExternalFile` itself is atomic at the RocksDB level: either all SST files
  link in, or none do.

### Crash behaviour per phase

| Crash point | DB state | Recovery |
|---|---|---|
| Phase 1 (scanning, chunk writing) | Unchanged | Delete `work_dir`, restart Phase 1 |
| Schema `WriteBatch` | Atomic (WAL) — schema + marker written or not | If written: skip schema step on retry. If not: re-run schema write. SSTs in `work_dir` are unaffected. |
| Phase 2 (SST generation) | Schema + marker written | Delete incomplete SSTs in `work_dir`, restart Phase 2 from chunks |
| Phase 3 (`IngestExternalFile`) | Atomic — unchanged or complete | If incomplete: RocksDB recovery restores original state; restart Phase 2 |
| After Phase 3, before marker cleanup | Data loaded, marker present | On next `Graph::open`: data exists → auto-clear marker and open. **No data loss, no re-ingest needed.** |
| After marker cleanup | **Complete** | Nothing to do |

The process is **fully retryable from any crash point**:
- Crash before Phase 3 → DB unchanged → delete `work_dir`, restart
- Crash during Phase 3 → RocksDB atomicity restores unchanged state → restart Phase 2
- Crash after Phase 3 (marker still present) → `Graph::open` auto-clears marker if data exists; if DB is empty, returns `IncompleteLoad` for retry
- Crash after marker cleanup → load succeeded

### Crash detection: `BULK_LOAD_IN_PROGRESS` marker

A sentinel key is written to the schema CF **as part of the schema `WriteBatch`**
(before Phase 2) and deleted after a successful Phase 3:

```rust
// Schema write (Phase 1→2): marker + schema entries in one atomic WriteBatch
batch.put_cf(&cf_schema, BULK_LOAD_IN_PROGRESS_KEY, &[1u8]);
// ... other schema entries ...
db.write(batch)?;

// After successful IngestExternalFile (Phase 3)
let mut cleanup = WriteBatch::default();
cleanup.delete_cf(&cf_schema, BULK_LOAD_IN_PROGRESS_KEY);
db.write(cleanup)?;
```

When `Graph::open` detects the marker, it distinguishes two cases:

```rust
fn open(path: &Path) -> Result<Graph, StoreError> {
    let db = /* open RocksDB */;
    if db.get_cf(&cf_schema, BULK_LOAD_IN_PROGRESS_KEY)?.is_some() {
        // Marker present — check whether ingest actually succeeded
        let cf_v = db.cf_handle(CF_VERTICES).ok_or(...)?;
        if db.iterator_cf(&cf_v, IteratorMode::Start).next().is_some() {
            // DB has data → ingest succeeded, just marker cleanup crashed.
            // Clear the marker and open normally.
            let mut cleanup = WriteBatch::default();
            cleanup.delete_cf(&cf_schema, BULK_LOAD_IN_PROGRESS_KEY);
            db.write(cleanup).map_err(StoreError::RocksDb)?;
        } else {
            // DB is empty → ingest never completed.  Return an error
            // that tells the caller to retry load_initial.
            return Err(StoreError::IncompleteLoad {
                msg: "bulk load was interrupted before ingest — retry load_initial".into(),
            });
        }
    }
    // ... normal open ...
}
```

This means: if the marker exists but data is present, the interrupted load succeeded
and only the cleanup `WriteBatch` was lost — a trivial fix.  If the marker exists
but the DB is empty, the interruption happened before `IngestExternalFile` and the
caller must re-run `load_initial`.

### Resumability (optimization)

For large datasets where Phase 1 (scanning) is expensive, the chunk files in
`work_dir` serve as a checkpoint.  If they are intact after a crash, Phase 1 can
be skipped and the retry starts from Phase 2 directly.  This is an optimization;
correctness only requires that Phase 1 be re-run on retry.

The presence of valid chunk files (detected via a `PHASE1_COMPLETE` marker in
`work_dir`) triggers the resume path.  Absent or partial chunk files → full
restart.

---

## Expected throughput

| Phase | Bottleneck | Speed |
|---|---|---|
| Pass 1 (scan) | Sequential read | 1–5 GB/s |
| In-memory sort | CPU + RAM | 50–200 M records/s |
| External sort Phase 1 | Sequential write | 1–3 GB/s |
| External sort Phase 2 | K-way merge + SST write | 0.5–2 GB/s |
| `IngestExternalFile` | Metadata + link | Near-instant |
| **Total (in-memory path)** | Sort or SST write | **5–50 M records/s** |
| **Total (external path)** | Chunk write | **2–10 M records/s** |

At 5 M records/s: 69 M edge LiveJournal → ~15 s.  Transactional path at 90 K/s
→ ~3 hours.  **200–700× speedup.**

---

## Constraints / invariants

- Keys within each SST file must be in strictly ascending order
- SST files must use the same `BlockBasedTableOptions` as the target DB
- Schema CF must be written before vertex/edge data — IDs come from schema
- `max_sst_size ≤ target_file_size_base × 0.9` to avoid immediate compaction
- `EdgeValue::end_vertex_label` must be correct and non-zero for every edge
- In Strict mode: unknown labels/prop keys → hard error before any SST is written
- In Single mode: duplicate `(src, label, dst)` pairs → error
- In Multi mode: unique rank per `(src, label, dst)` group
- A `BULK_LOAD_IN_PROGRESS` marker is present in the schema CF between Phase 2 start
  and Phase 3 completion.  `Graph::open` auto-clears it if data exists (ingest succeeded,
  just cleanup was interrupted) or returns `IncompleteLoad` if the DB is empty (ingest
  never happened and the caller must retry `load_initial`).

---

## Comparison with WriteBatch bulk loader

| Dimension | WriteBatch | SST ingest |
|---|---|---|
| Throughput | 200–500 K records/s | 5–50 M records/s |
| WAL overhead | Yes | **No** |
| Compaction during load | Yes (background) | **No** |
| Peak memory | ~200 MB | Chunk size (configurable) |
| Complexity | Low | Medium |
| Works on non-empty DB | Yes | No — empty DB required |
| Incremental updates | Yes | No — use TxnSession for incremental writes |

---

## Complexity assessment

| Component | Est. lines | Risk |
|---|---|---|
| Schema resolution + `write_schema_cf` | ~150 | Low |
| Phase 1 scan (maps, validation) | ~150 | Low |
| In-memory sort + `SSTFileWriter` per CF | ~250 | Medium — SSTFileWriter API, CF options must match |
| SST splitting + `IngestExternalFile` | ~100 | Low |
| Crash marker + `Graph::open` guard | ~80 | Low |
| EdgeMode rank assignment | ~100 | Low |
| **Phase 1 total** | **~830** | **Manageable** |
| `ExternalSorter<K,V>` (chunk + K-way merge) | ~300 | High — standalone external sort library |
| External label/degree path (two-pass annotation + sort-merge join) | ~300 | High — two sort passes per edge stream, concurrent sorted streams |
| `BulkSource` trait + format adapters | ~600 | Medium–High |
| **Phase 2 additions** | **~1,200** | **Complex** |

The external sort subsystem is the hard part.  It is also deferrable: the in-memory
path already handles the 69 M-edge LiveJournal dataset on a 16 GB machine, which
covers the benchmark use case.

---

## Implementation plan

### Crate API verification — rocksdb 0.24 ✅

| API | Status | Notes |
|---|---|---|
| `SstFileWriter::create` / `put` / `finish` / `file_size` | ✅ | `src/sst_file_writer.rs` |
| `IngestExternalFileOptions::set_move_files` | ✅ | `src/db_options.rs` |
| `ingest_external_file_cf_opts` | ✅ | `impl<T,D> DBCommon<T,D>` — available on `OptimisticTransactionDB` |

**One implementation constraint:** `SstFileWriter::create(opts: &'a Options)` takes the per-CF
`Options` object.  The writer must be created with options **identical** to those used to open
the target CF (block size, bloom filter, prefix extractor) — the live DB does not expose these
back.

Required refactor: extract `vertex_cf_opts(opts: &RocksOptions) -> Options` and
`edge_cf_opts(opts: &RocksOptions) -> Options` out of `store.rs` into
`src/store/rocks/cf_options.rs` so both the store and the bulk loader share the same option
construction code.  The `'a` lifetime on `SstFileWriter<'a>` means these `Options` values
must remain alive for the duration of each writer — handled by binding them at the same scope.

---

### Phase 1 — In-memory initial load (~870 lines)

**Scope:** initial load only; dataset fits in RAM; `EdgeListSource` format.
Delivers the core 50–500× write speedup immediately.

**By pipeline step:**

| What | Lines |
|---|---|
| **Prerequisite refactor:** extract `vertex_cf_opts` / `edge_cf_opts` from `store.rs` into `src/store/rocks/cf_options.rs` | ~30 |
| **Data model:** `SstBulkLoader`, `BulkSchema`, `BulkVertex`, `BulkEdge`, `ResolvedSchema`, `resolve()` | ~100 |
| **Schema CF:** `write_schema_cf()` — meta, labels, prop keys + `BULK_LOAD_IN_PROGRESS` marker in one atomic `WriteBatch` | ~150 |
| **Phase 1a — stream vertices:** `vertex_sorter` + `label_sorter` → `SortedLabelFile`; `SchemaMode::Strict` validation | ~80 |
| **Phase 1b — stream edges:** `dst_annot_sorter` + `src_annot_sorter` + `out_deg_sorter` + `in_deg_sorter`; Single-mode duplicate detection at SST write; Multi-mode pre-sort + rank assignment | ~250 |
| **Phase 2 — sort-merge-join + SST write:** `vertex_sorter` → vertex CF; three-way merge → degree CF; `annotate_edges` × 2 → `SSTFileWriter` per edge CF | ~300 |
| **Phase 3 — ingest:** `IngestExternalFile` all CFs; delete `BULK_LOAD_IN_PROGRESS` marker via `WriteBatch` after ingest | ~80 |
| **Public API:** `load_initial()` wiring; `Graph::open` marker detection + auto-cleanup; `IncompleteLoad` error variant | ~100 |
| **Bench:** `EdgeListSource` in `bulk_source.rs`; `--bulk-sst` flag in `bench_write.rs` | ~50 |

**Verification:** 100-vertex unit test + `bench_integrity.sh` on 10M edge dataset.
Benchmark 1M, 10M, 69M datasets; update `BENCHMARKS.md`.

---

### Phase 2 — Scale + format library

**Status: complete for unbounded vertex counts.**

**Completed:**
- `ExternalSorter` with flat buffer + cascaded merge (`bulk_sort.rs`) ✅
- Both `EdgeMode::Single` and `EdgeMode::Multi` fully streaming (vertices + edges) ✅
- `EdgeMode::Multi` supports explicit ranks, auto-ranks, and mixed ✅
- `SortedLabelFile` + sort-merge join for `end_vertex_label` — no in-memory map ✅
- `DegreeCounter` three-way merge for degree CF — no in-memory map ✅
- `WorkDirGuard` + `Drop for ExternalSorter` RAII cleanup ✅
- Memory bounded by `max_memory_bytes` regardless of vertex or edge count ✅
- Measured: 69M edges, 4.85M vertices → 358K edges/s, ~1.2 GB peak RAM ✅

**Remaining:**
1. Optional resume path: `PHASE1_COMPLETE` marker in `work_dir`; skip Phase 1 on
   retry if chunks are intact.
2. Additional `BulkSource` formats: `CsvEdgeSource`, `JsonLinesSource`,
   `GraphSONSource`, `AdjacencyListSource`.

**Verification:** peak RSS ≤ `max_memory_bytes` for any dataset size.

---

## Test plan

| Phase | Category | Test |
|---|---|---|
| 1 | **Correctness — initial load** | 100-vertex / 500-edge known graph; verify counts, properties, adjacency; `bench_integrity.sh` degree consistency |
| 1 | **end_vertex_label** | Read raw `EdgeValue` from `edges_out` CF; verify label matches actual destination vertex label; confirm `hasLabel()` during edge scan equals full vertex load |
| 1 | **Crash recovery** | Simulate crash after schema WriteBatch, after SST write, mid-ingest; verify DB unchanged on retry; verify `Graph::open` returns `IncompleteLoad` with marker present |
| 1 | **Throughput** | 1M, 10M, 69M edge benchmarks; update `BENCHMARKS.md` |
| 2 | **External sort** | Set `max_memory_bytes=1` on the 5-vertex test graph; verify output matches in-memory path exactly (same counts, same properties) |
| 2 | **Memory bound** | Force external path (`max_memory_bytes=1`) on the 5-vertex test graph; verify peak RSS bounded regardless of vertex count |
| 2 | **Annotation correctness** | Load graph with multiple vertex labels; verify `end_vertex_label` in raw `EdgeValue` bytes matches the actual dst/src vertex label |
| 2 | **Missing vertex detection** | Supply an edge whose dst vertex is absent from the vertex iterator; verify `SchemaViolation` is returned |
| **Options propagation** | Verify `vertex_block_size` and bloom filter bits reflected in generated SST properties |
