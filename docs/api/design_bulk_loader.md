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

It is not a replacement for `TxSession` (incremental writes to a live graph) or
`open_schema()` (schema DDL). It is the `COPY` / `pg_dump --restore`
analogue for RocksGraph.

---

## 2. Constraints and positioning

| Constraint | Detail |
| ---------- | ------ |
| **Overwrites existing data** | `open_bulk_loader()` does not check for pre-existing graph data. Any keys produced by the SST build will overwrite colliding entries on `IngestExternalFile`. The caller is responsible for the overwrite semantics. Typical use is on an empty database; re-running on a live graph replaces all overlapping keys. |
| **Schema mode governs schema handling** | *Auto mode*: the schema is assumed to be empty at the start of bulk load — vertex labels, edge labels, and property keys are auto-registered from the data during SST generation, with no dependence on any prior schema state. *Strict mode*: the schema is read from the Graph at `open_bulk_loader()` time and is **never modified** during loading or at commit — no labels or keys are added, changed, or removed. |
| **No concurrent queries during loading** | The graph is exclusively owned by the `BulkLoader` session for the duration. `read()` and `begin()` calls on the same `Graph` handle return `StoreError::BulkLoadInProgress`. |
| **Not transactional** | Data becomes visible all-at-once via `IngestExternalFile`. If the process is killed mid-pipeline, partial state may be left behind — see §7. |
| **No WAL** | SST files bypass the WAL entirely. After ingestion, the graph behaves as if it was written with WAL for all future incremental writes. |

---

## 3. Session interface

The `BulkLoader` follows the same session-open-commit pattern as
`open_schema()`, opened via a method on `Graph`.

### 3a. Rust

```rust
let mut loader = graph.open_bulk_loader("/tmp/bulk_work")?;

loader.load_vertices(vertex_iter)?;
loader.load_edges(edge_iter)?;

let stats = loader.commit()?;
println!("loaded {} vertices, {} edges in {:.1}s",
    stats.vertices_written, stats.edges_written, stats.duration_secs);
```

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

with g.open_bulk_loader(work_dir="/tmp/bulk") as bulk:
    bulk.load_vertices(document_iter)
    bulk.load_edges(citation_iter)
# __exit__ calls commit(): SST ingest → HNSW build → snapshot → marker cleared

# Auto mode — no vector index during bulk load
with g.open_bulk_loader(work_dir="/tmp/bulk") as bulk:
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

```
BulkLoader::commit()
│
├── Phase 1 — SST generation (streaming, bounded memory via ExternalSorter)
│     vertices + all properties  → CF_VERTICES SST(s)
│     edges                      → CF_EDGES_OUT / CF_EDGES_IN SST(s)
│     degree counts              → CF_VERTEX_DEGREE SST(s)
│     schema (if auto mode)      → CF_SCHEMA SST (auto-registered labels/keys)
│     FloatVector values land in CF_VERTICES props blob — no special treatment
│
├── Phase 2 — Write crash marker
│     Atomic write to CF_SCHEMA: BULK_LOAD_IN_PROGRESS_KEY = "pre-ingest"
│     (Signals: SST files ready but not yet ingested)
│
├── Phase 3 — IngestExternalFile (all CFs, atomic)
│     Data becomes visible. Crash marker updated: "post-ingest"
│     (Signals: graph data live, HNSW build not yet started)
│
├── Phase 4 — Vector index build  [STRICT MODE ONLY, skipped in AUTO]
│     For each VectorIndexConfig registered in schema:
│       Scan CF_VERTICES for FloatVector entries matching (entity_type, prop_key_id)
│       Batch-insert into usearch HNSW (no per-insert RwLock contention)
│     Crash marker updated: "post-index"
│     (Signals: HNSW built, snapshot not yet written)
│
├── Phase 5 — Write index snapshots  [STRICT MODE ONLY]
│     For each built HNSW index: write snapshot file atomically
│     Crash marker updated: "post-snapshot"
│
└── Phase 6 — Clear crash marker
      Delete BULK_LOAD_IN_PROGRESS_KEY from CF_SCHEMA
      BulkLoader session complete — graph fully queryable
```

Work directory (containing SST files and sort-spill chunks) is cleaned up
automatically on success. On error or crash, it is left in place for crash
recovery (§7).

---

## 6. Schema mode interaction

### 6a. Strict mode

All schema elements — vertex labels, edge labels, property keys, and vector
indexes — **must be declared via `open_schema()` before `BulkLoader`
starts**. The loader reads the schema from the `Graph` handle at
`open_bulk_loader()` time.

**The schema is never modified during bulk load or at commit.** The loader
validates every `BulkVertex.label`, `BulkEdge.label`, and property key name
against the frozen schema; unknown names are rejected immediately. No labels,
property keys, or index entries are added to CF_SCHEMA during the pipeline.

At commit, all vector indexes registered in the schema are built automatically
from the ingested data (Phase 4–5 above). The user does not need to call
anything after the bulk load to have a queryable HNSW index.

After the bulk load completes, the user may add further indexes at any time:

```python
# e.g. to add a second vector property after initial load
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(property="summary_vec", ...))
    mgmt.commit()
```

### 6b. Auto mode

**The schema is assumed to be empty at the start of bulk load.** Vertex labels,
edge labels, and non-vector property keys are **auto-registered from the data**
during Phase 1 SST generation. Auto mode does not read or rely on any
schema that may already exist in the `Graph`.

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
| Schema at `open_bulk_loader()` | Read from Graph; frozen for the duration | Assumed empty; ignored |
| Vertex/edge labels | Must be declared beforehand; unknown names rejected | Auto-registered from data |
| Non-vector property keys | Must be declared beforehand; unknown names rejected | Auto-registered from data |
| Schema modifications during load | **None** | Labels + property keys written to CF_SCHEMA SST |
| Vector indexes during bulk load | Auto-built from declared schema | **Never built** |
| Adding indexes after bulk load | `open_schema().add_vector_index()` | Same |

---

## 7. Crash recovery

The crash marker `BULK_LOAD_IN_PROGRESS_KEY` in CF_SCHEMA records the pipeline
phase at the time of the crash. `Graph::open()` checks for this marker and
takes the appropriate recovery action before returning.

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

| | `open_schema()` | `BulkLoader` | `TxSession` |
| --- | --- | --- | --- |
| **Purpose** | Schema DDL | Initial bulk data load | Incremental data writes |
| **Opened via** | `graph.open_schema()` | `graph.open_bulk_loader(work_dir)` | `graph.begin()` |
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

- `design_api_overview.md` — session type overview and how `BulkLoader` fits alongside `SchemaSession` and `TxSession`
- `design_session_workflows.md` — end-to-end call sequences for Scenario 3 (auto mode bulk load + index) and Scenario 4 (strict mode bulk load with auto-built index), including crash recovery in Scenario 7 Case B
