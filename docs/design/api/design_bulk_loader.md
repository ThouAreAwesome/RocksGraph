# Design: BulkLoader — Initial Graph Bootstrap

Status: proposal.

---

## Table of Contents

- [1. Purpose](#1-purpose)
- [2. Constraints and positioning](#2-constraints-and-positioning)
- [3. Session interface](#3-session-interface)
  - [3a. Rust](#3a-rust)
  - [3b. Python](#3b-python)
  - [3c. TypeScript](#3c-typescript)
- [4. BulkVertex and BulkEdge types](#4-bulkvertex-and-bulkedge-types)
- [5. Commit pipeline](#5-commit-pipeline)
- [6. Schema mode interaction](#6-schema-mode-interaction)
  - [6a. Strict mode](#6a-strict-mode)
  - [6b. Auto mode](#6b-auto-mode)
- [7. Crash recovery](#7-crash-recovery)
- [8. Comparison with other sessions](#8-comparison-with-other-sessions)
- [9. Open questions](#9-open-questions)

---

## 1. Purpose

`BulkLoader` is the fast path for **initial graph bootstrap** — loading a large
dataset by bypassing the WAL, the OCC transaction engine, and per-row memtable
pressure entirely. It generates sorted SST files offline and ingests them
atomically via RocksDB's `IngestExternalFile`. Existing keys in the database
are overwritten by the ingest; the typical and recommended use is on a fresh
empty database.

It is not a replacement for `TxnSession` (incremental writes to a live graph) or
`open_schema()` (schema DDL). It is the `COPY` / `pg_dump --restore`
analogue for RocksGraph.

```
Input Data Sources                     BulkLoader (graph session)
------------------                     -------------------------

Any graph dataset / stream            graph.open_bulk_loader()
  |                                   |
  +- CSV / JSON / SNAP / Parquet      +- load_vertices(iter)   -> vertex SST
  |     -> vertices() iter            |  |
  |     -> edges() iter               |  +- load_edges(iter)    -> edge SSTs + degree
  |                                   |     |
  +- Direct in-memory collections     |     +- commit()         -> ingest atomically
        -> IntoIterator[BulkVertex]   |
        -> IntoIterator[BulkEdge]     |
```

Every graph dataset, regardless of format, decomposes into two iterators.
External format parsers or user code feed directly into `load_vertices(iter)`
and `load_edges(iter)`.

Thanks to the [`IntoBulkVertex`] and [`IntoBulkEdge`] traits, iterators can yield either
raw items (`BulkVertex`, `BulkEdge`) or fallible results (`Result<BulkVertex, E>`, `Result<BulkEdge, E>`),
enabling clean streaming error propagation without buffering intermediate records.

---

## 2. Constraints and positioning

| Constraint | Detail |
| ---------- | ------ |
| **Overwrites existing data** | `open_bulk_loader()` does not check for pre-existing graph data. Any keys produced by the SST build will overwrite colliding entries on `IngestExternalFile`. The caller is responsible for the overwrite semantics. Typical use is on an empty database; re-running on a live graph replaces all overlapping keys. |
| **Schema mode governs schema handling** | *Strict mode*: validates all labels and property keys against `graph.schema` during `load_vertices()`/`load_edges()` — undeclared names abort the current phase before its SSTs are written. Schema is never modified. *Auto mode*: collects new labels and property keys into a staging schema during load; synced to `graph.schema` and `CF_SCHEMA` atomically at `commit()`. |
| **No concurrent queries during loading** | `open_bulk_loader()` borrows `&mut self`, enforcing compile-time exclusion. For shared `Arc<Graph>` (Python/Node.js bindings, concurrent Rust), an internal `AtomicBool` rejects `g.read()`/`g.txn()` with `StoreError::BulkLoadInProgress`. |
| **Not transactional** | Data becomes visible all-at-once via `IngestExternalFile`. If the process is killed mid-pipeline, partial state may be left behind — see §7. |
| **No WAL** | SST files bypass the WAL entirely. After ingestion, the graph behaves as if it was written with WAL for all future incremental writes. |

---

## 3. Session interface

The `BulkLoader` follows the same session-open-commit pattern as
`open_schema()`, opened via a method on `Graph`.

### 3a. Rust

`BulkLoader` is opened as a session on an existing `Graph`. It holds a borrowed
reference to the active `Arc<DB>` and `Arc<RwLock<Schema>>` — it never re-opens
the database, so there is no file-lock contention.

```rust
let mut loader = graph.open_bulk_loader()?;
// or with explicit work_dir and performance tunables:
let mut loader = graph.open_bulk_loader()?
    .with_work_dir("/fast/ssd/bulk_scratch")
    .with_max_memory(1024 * 1024 * 1024)   // 1 GB RAM budget
    .with_max_sst_size(128 * 1024 * 1024)   // 128 MB SST split
    .with_rocks_options(rocks_options);

loader.load_vertices(vertex_iter)?;
loader.load_edges(edge_iter)?;

let stats = loader.commit()?;
println!("loaded {} vertices, {} edges in {:.1}s",
    stats.vertices_written, stats.edges_written, stats.duration_secs);
```

#### Builder Configuration Methods

| Method | Default | Description |
|---|---|---|
| `with_work_dir(path)` | System temp directory (`_bulk_work`) | Scratch directory for external merge sorting and staging SST files. Cleaned up on `commit()` or `Drop`. |
| `with_max_memory(bytes)` | `512 MiB` | Maximum in-memory RAM budget for `ExternalSorter` before spilling chunks to disk. |
| `with_max_sst_size(bytes)` | `58 MiB` (90% of RocksDB 64 MiB default) | Target split threshold for generated SST files to allow parallel ingestion. |
| `with_rocks_options(opts)` | `graph.storage_opts` | Sets custom `RocksOptions` (block size, bloom filters, block-based table options) used when creating `SstFileWriter` instances so SST block formats match the target column families. |

`Drop` on `BulkLoader` (without explicit `commit()`) automatically cleans up the work directory
and discards any in-progress SST files — no data is ingested into the database.

### 3b. Python

```python
# Strict mode — vector indexes declared beforehand, auto-built at commit
with g.open_schema() as mgmt:
    mgmt.add_vertex_label("doc")
    mgmt.add_property_key("title",     DataType.STRING)
    mgmt.add_property_key("embedding", DataType.FLOAT_VECTOR)
    mgmt.add_vector_index(VectorIndexConfig(
        property    = "embedding",
        entity_type = VectorEntityType.VERTEX,
        dimension   = 1536,
        metric      = DistanceMetric.COSINE,
    ))
    mgmt.commit()

with g.open_bulk_loader().with_work_dir("/tmp/bulk").with_max_memory(512 * MB) as bulk:
    bulk.load_vertices(document_iter)
    bulk.load_edges(citation_iter)
# __exit__ calls commit(): SST ingest → HNSW build → snapshot → marker cleared

# Auto mode — no vector index during bulk load
with g.open_bulk_loader() as bulk:
    bulk.load_vertices(document_iter)  # vertex labels, prop keys auto-registered
    bulk.load_edges(citation_iter)
# __exit__ commits: SST ingest only, no HNSW build

# After auto-mode load: add index explicitly
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(
        property    = "embedding",
        entity_type = VectorEntityType.VERTEX,
        dimension   = 1536,
        metric      = DistanceMetric.COSINE,
    ))
    mgmt.commit()  # scans props CF and builds index from already-ingested data
```

### 3c. TypeScript

```typescript
// Strict mode
const mgmt = g.openSchema();
mgmt.addVectorIndex({ property: "embedding", entityType: "vertex",
                      dimension: 1536, metric: "cosine" });
mgmt.commit();

const bulk = g.openBulkLoader({ workDir: "/tmp/bulk" });
bulk.loadVertices(documentIter);
bulk.loadEdges(citationIter);
bulk.commit();  // SST ingest + HNSW build

// Auto mode
const bulk = g.openBulkLoader({ workDir: "/tmp/bulk" });
bulk.loadVertices(documentIter);
bulk.loadEdges(citationIter);
bulk.commit();  // SST ingest only

// Add index after load
g.openSchema().addVectorIndex({...}).commit();
```

---

## 4. BulkVertex and BulkEdge types

Structural data-carrying types — no traversal builder, no OCC overhead.

```rust
pub struct BulkVertex {
    pub id:    VertexKey,
    pub label: String,
    pub props: HashMap<String, GValue>,  // GValue::FloatVector tracked for §5 phase 4
}

pub struct BulkEdge {
    pub src:   VertexKey,
    pub dst:   VertexKey,
    pub label: String,
    pub props: HashMap<String, GValue>,
    pub rank:  Option<Rank>,  // None = auto-assign in Multi mode; ignored in Single mode
}
```

Python:

```python
BulkVertex(
    id    = 1,
    label = "doc",
    props = {
        "title":     "Hello World",
        "embedding": Vector([0.1, 0.2, ...]),  # GValue::FloatVector
    }
)

BulkEdge(src=1, dst=2, label="cites", props={"weight": 1.0})
```

`Vector(...)` in `props` is a `GValue::FloatVector`. The bulk loader recognises
it as a vector property and handles it identically to a scalar property for the
SST write — it lands in the props CF as f32 bytes. Whether a HNSW index is
built from it depends entirely on the schema mode (§6), not on the presence of
`Vector(...)` in the data.

---

## 5. Commit pipeline

The work is done by three methods in sequence. `load_vertices` and `load_edges`
process iterators and write SST files to the work directory. `commit()` ingests
them atomically, syncs the schema, builds vector indexes, and writes snapshots.

### 5a. `load_vertices(iter)`

Streams the vertex iterator through an `ExternalSorter`, sorts by vertex key,
and writes `CF_VERTICES` SSTs to the work directory. Simultaneously writes a
temporary `vertex_labels.bin` — a sorted `(VertexKey, LabelId)` file used by
`load_edges()` to annotate destination vertex labels on edges.

```rust
// Called first. Consumes the iterator. Writes SSTs to _bulk_work/.
loader.load_vertices(vertex_iter)?;
```

### 5b. `load_edges(iter)`

Streams the edge iterator, performs a sort-merge join with `vertex_labels.bin`
to attach destination vertex labels, sorts edges, and writes `CF_EDGES_OUT`,
`CF_EDGES_IN`, and `CF_VERTEX_DEGREE` SSTs to the work directory.

In Strict mode: validates every edge label and property key against `graph.schema`
— undeclared names abort immediately, before any edge SSTs are written.

In Auto mode: collects newly-encountered labels and property keys into a
staging schema for sync-back at `commit()`.

```rust
// Called second. Must follow load_vertices() — needs vertex_labels.bin.
// Calling load_edges() before load_vertices() returns StoreError::VerticesNotLoaded.
loader.load_edges(edge_iter)?;
```

### 5c. `commit()`

Does not re-read any source data. All SST files already exist in the work
directory. `commit()` sequences the finalisation steps:

```
BulkLoader::commit()
│  Uses the existing Arc<DB> handle from Graph — never re-opens the database.
│
├── Phase 1 — Write crash marker
│     Atomic write to CF_SCHEMA: BULK_LOAD_IN_PROGRESS_KEY = "pre-ingest"
│
├── Phase 2 — IngestExternalFile (all CFs, atomic)
│     CF_VERTICES, CF_EDGES_OUT, CF_EDGES_IN, CF_VERTEX_DEGREE SSTs ingested.
│     Data becomes visible. Crash marker updated: "post-ingest"
│
├── Phase 3 — Schema sync  [AUTO MODE ONLY]
│     Write newly-discovered labels + property keys to CF_SCHEMA.
│     Atomically update graph.schema (Arc<RwLock<Schema>>) with the staging schema.
│     (Strict mode: no-op — validation already done in load_vertices/load_edges.)
│
├── Phase 4 — Vector index build  [STRICT MODE ONLY, skipped in AUTO]
│     For each VectorIndexConfig registered in schema:
│       Scan CF_VERTICES for FloatVector entries matching (entity_type, prop_key_id)
│       Batch-insert into usearch HNSW (no per-insert RwLock contention)
│     Crash marker updated: "post-index"
│
├── Phase 5 — Write index snapshots  [STRICT MODE ONLY]
│     For each built HNSW index: write snapshot file atomically.
│     Crash marker updated: "post-snapshot"
│
└── Phase 6 — Clear crash marker
      Delete BULK_LOAD_IN_PROGRESS_KEY from CF_SCHEMA.
      Clean up work directory (SST files + sort-spill chunks).
      Disarm WorkDirGuard — on error, work_dir is left for crash recovery (§7).
```

---

## 6. Schema mode interaction

### 6a. Strict mode

All schema elements — vertex labels, edge labels, property keys, and vector
indexes — **must be declared via `open_schema()` before `BulkLoader` starts**.
The loader holds a shared reference to `graph.schema` and validates every
`BulkVertex.label`, `BulkEdge.label`, and property key name against this
schema during `load_vertices()`/`load_edges()`. Unknown names are rejected
before edge SSTs are written — vertex SSTs from load_vertices() may already exist in the work directory, but they will be cleaned up by Drop (§3a).

The schema is **never modified** during bulk load. No labels, property keys,
or index entries are added to CF_SCHEMA. No sync-back to `graph.schema` is
needed at `commit()` — the schema is already authoritative.

At commit, all vector indexes registered in the schema are built automatically
from the ingested data (Phase 4–5, §5c).

After the bulk load completes, the user may add further indexes at any time:

```python
# e.g. to add a second vector property after initial load
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(property="summary_vec", ...))
    mgmt.commit()
```

### 6b. Auto mode

Vertex labels, edge labels, and non-vector property keys are **auto-registered
from the data** during `load_vertices()`/`load_edges()`. Newly-encountered
names are collected into a staging schema held by the `BulkLoader` session —
`graph.schema` is not modified during load.

At `commit()`, the staging schema is written to `CF_SCHEMA` and atomically
synced into `graph.schema` (`Arc<RwLock<Schema>>`). From that point forward,
the graph's schema includes all labels and property keys discovered during
the bulk load.

**Vector indexes are never built automatically in auto mode.** Even though the
loader can detect `GValue::FloatVector` values in `props`, it cannot infer the
index configuration (dimension, metric, algorithm, quantization) from the data
alone. No HNSW build occurs at commit time.

After the bulk load completes, the user builds any desired vector indexes
explicitly:

```python
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(
        property    = "embedding",
        entity_type = VectorEntityType.VERTEX,
        dimension   = 1536,
        metric      = DistanceMetric.COSINE,
    ))
    mgmt.commit()  # scans the already-ingested props CF and builds HNSW
```

`add_vector_index` implicitly registers the property key as
`DataType::FloatVector` if it was not already auto-registered during loading.

### Summary table

| | Strict mode | Auto mode |
| --- | --- | --- |
| Schema during load | Held via Arc<RwLock<Schema>>; validates writes | Auto-registers into staging schema; not yet visible |
| Schema at commit | No-op (schema unchanged) | Staging schema written to CF_SCHEMA + synced into graph.schema |
| Schema modifications during load | **None** | Labels + property keys collected into staging schema |
| Vector indexes during bulk load | Auto-built from declared schema | **Never built** |
| Adding indexes after bulk load | `open_schema().add_vector_index()` | Same |

---

## 7. Crash recovery

The crash marker `BULK_LOAD_IN_PROGRESS_KEY` in CF_SCHEMA records the pipeline
phase at the time of the crash. `Graph::open()` checks for this marker and
takes the appropriate recovery action before returning.

**Crash before `commit()`** (during `load_vertices()` or `load_edges()`): no
crash marker has been written yet. The database is untouched — nothing was
ingested. The work directory may contain partial SST files; these are stale
and can be safely deleted. `Graph::open()` sees a clean database with no
recovery action needed.

| Marker state | What happened | Recovery action |
| ------------ | ------------- | --------------- |
| `"pre-ingest"` | SST files ready but `IngestExternalFile` not completed | Delete work_dir; graph is empty and safe to use |
| `"post-ingest"` | Graph data live; HNSW build not started | Graph data is queryable; vector indexes are absent — user must call `open_schema().add_vector_index()` explicitly |
| `"post-index"` | HNSW built; snapshot not written | Rebuild index snapshots from in-memory HNSW (cheap); clear marker |
| `"post-snapshot"` | All data and indexes complete; marker not cleared | Clear marker — normal open |

> **Open question**: For `"post-ingest"` recovery, should `Graph::open()` automatically
> trigger `rebuild_vector_index` for all schema-declared indexes, or should it leave the
> indexes absent and require the user to trigger a rebuild? The answer depends on whether
> the rebuild blocks user access to the graph (queries would return stale/empty results
> from the vector index during rebuild). This is deferred to the implementation phase.

---

## 8. Comparison with other sessions

| | `open_schema()` | `BulkLoader` | `TxnSession` |
| --- | --- | --- | --- |
| **Purpose** | Schema DDL | Initial bulk data load | Incremental data writes |
| **Opened via** | `graph.open_schema()` | `graph.open_bulk_loader()` | `graph.begin()` |
| **Commit** | `.commit()` (or context manager) | `.commit()` (or context manager) | `.commit()` |
| **Crash safety** | Atomic (WAL-backed CAS) | Not transactional (SST ingest is atomic; HNSW build is not) | Atomic (WAL-backed OCC) |
| **Data path** | CF_SCHEMA only | SST files → `IngestExternalFile` | WAL → memtable → compaction |
| **Concurrent queries** | Allowed (reads see old schema until commit) | **Blocked** | Allowed |
| **Database state** | Any | Any (existing data overwritten) | Any |
| **Vector index built** | Yes, on `add_vector_index` | Strict: auto; Auto: never (explicit after) | Yes, per-commit incrementally |

---

## 9. Open questions

| Question | Context |
| -------- | ------- |
| **Crash recovery blocking** | After `"post-ingest"` crash, if `Graph::open()` auto-triggers `rebuild_vector_index`, does it block all queries until the rebuild completes, or serve queries with an absent/stale index while rebuilding in the background? Deferred to implementation. |
| **`open_bulk_loader` on non-empty database** | Current design requires empty database. A future "append bulk load" mode (loading new vertices/edges into a live graph via SST) would relax this but requires handling concurrent writers and partial index updates. Not in scope for v0.3. |
| **Work directory lifecycle** | Who cleans up the work directory after a crash? Currently left in place for inspection. Should `Graph::open()` clean it up automatically after successful crash recovery? |
| **BulkLoader stats and progress** | `commit()` returns `BulkLoadStats` (vertices_written, edges_written, sst_files, duration_secs). Should it also include HNSW build stats (vectors_indexed, index_build_duration)? |

---

## See also

- `design_api_overview.md` — session type overview and how `BulkLoader` fits alongside `SchemaSession` and `TxnSession`
- `design_session_workflows.md` — end-to-end call sequences for Scenario 3 (auto mode bulk load + index) and Scenario 4 (strict mode bulk load with auto-built index), including crash recovery in Scenario 7 Case B
