# Design: API Overview & Data Pipeline

Status: proposal — living document. Update as the API surface evolves.

---

## Table of Contents

- [1. Purpose](#1-purpose)
- [2. Session model](#2-session-model)
  - [2a. Graph — the entry point](#2a-graph--the-entry-point)
  - [2b. ReadSession — point-in-time snapshot](#2b-readsession--point-in-time-snapshot)
  - [2c. TxnSession — transactional writes](#2c-txsession--transactional-writes)
  - [2d. SchemaSession — DDL](#2d-schemamanagement--ddl)
  - [2e. BulkLoader — initial graph bootstrap](#2e-bulkloader--initial-graph-bootstrap)
- [3. Operation taxonomy](#3-operation-taxonomy)
  - [3a. Split vs merge: index lifecycle design options](#3a-split-vs-merge-index-lifecycle-design-options)
- [4. Data pipeline](#4-data-pipeline)
  - [4a. Transactional write path](#4a-transactional-write-path)
  - [4b. Initial bulk load path (BulkLoader)](#4b-initial-bulk-load-path-bulkloader)
  - [4c. Read / traversal path](#4c-read--traversal-path)
- [5. Vector index integration](#5-vector-index-integration)
  - [5a. DataType::FloatVector](#5a-datatypefloatvector)
  - [5b. Index declaration via SchemaSession](#5b-index-declaration-via-schemamanagement)
  - [5c. Vector index rebuild after bulk load](#5c-vector-index-rebuild-after-bulk-load)
  - [5d. Operational methods on Graph](#5d-operational-methods-on-graph)
  - [5e. Index lifecycle states and crash recovery](#5e-index-lifecycle-states-and-crash-recovery)
- [6. Language binding surface](#6-language-binding-surface)
  - [6a. Rust (native)](#6a-rust-native)
  - [6b. Python (PyO3)](#6b-python-pyo3)
  - [6c. TypeScript / Node.js (napi-rs)](#6c-typescript--nodejs-napi-rs)
- [7. Open questions](#7-open-questions)

---

## 1. Purpose

This document describes the full user-facing API surface of RocksGraph and the
data pipelines that flow through it. It is the reference point for deciding
**where a new operation belongs** — which type it is added to, which pipeline
it participates in, and how it is exposed across language bindings.

For **end-to-end call sequences** showing what to call and in what order for each
common use pattern (fresh graph, bulk load, add index to existing graph, model
upgrade), see [`design_session_workflows.md`](design_session_workflows.md).

The guiding principle:

> **Gremlin traversal is for data. Management sessions are for administration.**
> Nothing that changes the schema or maintains an index should be reachable via
> a traversal step.

---

## 2. Session model

RocksGraph exposes five distinct entry points, each with a clear responsibility.

```
Graph::open(path)
  │
  ├── .read()                      → ReadSession       (snapshot, read-only)
  ├── .begin()                     → TxnSession         (OCC transaction, read-write)
  ├── .open_schema()               → SchemaSession  (DDL, atomic CAS commit)
  ├── .open_bulk_loader()          → BulkLoader        (SST bulk load, overwrites existing data)
  │
  └── (operational methods on Graph itself — see §3)
```

### 2a. Graph — the entry point

`Graph` is the top-level handle to a RocksDB-backed property graph. It is
cheap to clone (wraps an internal `Arc`).

```rust
// Default: SchemaMode::Auto, EdgeMode::Single
let g = Graph::open("./mydb")?;

// Explicit schema and storage options
let g = Graph::open_with_options("./mydb", GraphOptions {
    mode:      SchemaMode::Strict,
    edge_mode: EdgeMode::Multi,
})?;

// Full control including RocksDB tuning
let g = Graph::open_with_options("./mydb", schema_opts, storage_opts)?;
```

`Graph` also holds the handful of **operational / maintenance methods** that
work with existing indexes without modifying the schema (see §3 and §5d).

### 2b. ReadSession — point-in-time snapshot

`ReadSession` is a read-only view pinned to the committed state at the moment
`read()` was called. Later commits do not affect it.

```rust
let snap = g.read();
let names = snap.g().V([1]).out(["knows"]).values(["name"]).to_list()?;
```

`ReadSession` is dropped silently with no side effects.

### 2c. TxnSession — transactional writes

`TxnSession` is an optimistic concurrency control (OCC) transaction. Reads
inside the transaction see a consistent snapshot; writes are buffered and
applied atomically on `commit()`. On conflict, `commit()` returns
`StoreError::Conflict` and the session must be retried or rolled back.

```rust
let mut txn = g.begin();
txn.g().addV("person").property("name", "Alice").next()?;
txn.commit()?;
```

When a transaction commits a `FloatVector` property, the write path
automatically updates the in-memory vector index for that property (if one
is declared). See §4a.

### 2d. SchemaSession — DDL

`SchemaSession` is the exclusive entry point for all structural changes:
declaring or removing vertex/edge labels, property keys, and vector indexes.
It uses optimistic CAS versioning — `commit()` fails with
`StoreError::Conflict` if another management session committed concurrently.

```rust
let mut mgmt = g.open_schema();
mgmt.add_vertex_label("person")
    .add_edge_label("knows")
    .add_property_key("name", DataType::String)
    .add_property_key("embedding", DataType::FloatVector)
    .add_vector_index(VectorIndexConfig {
        property:   "embedding",
        entity_type: VectorEntityType::Vertex,
        dimension:  1536,
        metric:     DistanceMetric::Cosine,
        ..Default::default()      // quantization: F16 (default)
    });
mgmt.commit()?;
```

`SchemaSession` never enters the traversal pipeline — it talks directly to
the schema registry and RocksDB's schema CF.

### 2e. BulkLoader — initial graph bootstrap

`BulkLoader` is the fast path for loading a large dataset into an **empty**
RocksDB database. It streams vertex and edge data through an external sorter,
generates sorted SST files in `work_dir`, and ingests them atomically via
RocksDB's `IngestExternalFile` — bypassing the WAL, OCC, and per-row memtable
pressure entirely. See `docs/api/design_bulk_loader.md` for the full design.

```rust
let mut loader = g.open_bulk_loader()?;
loader.load_vertices(vertex_iter)?;
loader.load_edges(edge_iter)?;
let stats = loader.commit()?;
```

Key constraints:
- Existing graph data is overwritten — `open_bulk_loader()` does not reject a non-empty
  database; SST ingest overwrites colliding keys. Typical use is on a fresh empty database.
- No concurrent queries; `read()` and `begin()` return `StoreError::BulkLoadInProgress`
  while the loader is open.
- Not transactional — a 6-phase crash marker in CF_SCHEMA provides recovery.
- **Strict schema mode**: schema is read from the Graph at open time and frozen — no
  modifications occur during load or commit. Declared vector indexes are auto-built at commit.
- **Auto schema mode**: schema is assumed empty — labels and property keys are
  auto-registered from data. Vector indexes are never built; add them explicitly after load.

---

## 3. Operation taxonomy

| Category | Entry point | Examples | Why here |
| -------- | ----------- | -------- | -------- |
| **DDL** — changes what exists in the schema | `open_schema()` | `add_vector_index`, `drop_vector_index`, `change_vector_index_algorithm`, `add_vector_index_async`, `add_property_key`, `add_vertex_label` | Atomic CAS commit; must be visible to all sessions before taking effect |
| **Data reads** | `read()` / `begin()` | `.V().out().values()`, `.nearest()`, `.similarity()` | Gremlin traversal; consistent snapshot semantics |
| **Data writes** | `begin()` | `.addV()`, `.addE()`, `.property()` | OCC transaction; WAL-backed |
| **Operational / maintenance** | `Graph` directly | `rebuild_vector_index`, `export_vector_index`, `import_vector_index`, `vector_index_stats` | Work on existing indexes; don't change schema; not traversal operations |
| **Initial bulk load** | `open_bulk_loader()` | `load_vertices`, `load_edges`, `commit()` | Bypasses WAL and OCC entirely; overwrites existing data |

The key decision rule: **"Am I changing what indexes or schema elements exist?"
→ `open_schema()`. "Am I working with an existing index or reading data?"
→ `Graph` or a session.**

The SQL analogy holds cleanly:
- `CREATE INDEX` / `DROP INDEX` → `open_schema()`
- `REINDEX` / `pg_dump` / `pg_restore` → `Graph` maintenance methods
- `INSERT` / `SELECT` → `begin()` / `read()`
- `COPY` / bulk import → `BulkLoader`

### 3a. Split vs merge: index lifecycle design options

The current design splits index lifecycle between `open_schema()` (DDL: add,
drop, change) and `Graph` (operational: rebuild, export, import, stats). An
alternative merges all of them into `open_schema()`. The concerns and how
each approach handles them:

| Concern | Split (current) | Merge into `open_schema()` |
| ------- | --------------- | ------------------------------ |
| **CAS conflict window during long builds** | Present for `add_vector_index` and `change_vector_index_algorithm` — both trigger builds inside the session. Mitigated by two-phase commit: schema CAS first (fast, milliseconds), build runs post-commit outside the CAS window. | Same two operations have the same issue; same two-phase commit mitigation applies. `rebuild_vector_index` inside the session adds another trigger unless treated as immediate. |
| **`rebuild_vector_index` carries no schema change** | Not an issue — executes directly on `Graph`, no CAS or schema version involved. | Must execute immediately (not deferred to `commit()`) and must not bump the schema version. Deviates from the session's builder contract for the most-frequently-called index operation. |
| **`export` / `import` / `stats` don't fit deferred commit** | Not an issue — immediate operations on `Graph`. | Must also execute immediately inside the session. Creates a mixed contract: some session methods are deferred, others are immediate. |
| **Parallel index rebuilds** | `rebuild_vector_index` bypasses CAS entirely — concurrent rebuilds on different indexes work without conflict. | Preserved only if rebuild is an immediate operation (which resolves concern 2). If deferred through `commit()`, concurrent sessions conflict on the CAS version. |
| **Session semantics** | Session is a pure deferred builder — all operations queued until `commit()`, consistent and predictable. | Mixed contract: DDL operations deferred, rebuild/export/import/stats immediate. Must be documented explicitly; harder to reason about. |
| **Boundary between schema and operational** | `add_vector_index`, `drop_vector_index`, `change_vector_index_algorithm` all do schema + operational work but live in the schema session. `rebuild_vector_index` does only operational work but lives on `Graph`. The distinction feels arbitrary since all four ultimately trigger or redo an index build. | No boundary: all index lifecycle operations — whether they change the schema or not — live in one place. Matches the user's mental model that "managing vector indexes" is one concern. |

**Two-phase commit** is required for build-triggering operations regardless of
which approach is chosen. The schema session commits in two phases: (1) fast
CAS on the schema entry (marking the index `"building"` or `"dropping"`), then
(2) the actual build/teardown runs outside the CAS window. This keeps the
conflict window at milliseconds for both approaches.

**Residual trade-off**: the split has a clean, uniform session contract but an
arbitrary boundary (why does `rebuild` live outside when `add`/`drop`/`change`
live inside, if all four ultimately trigger a build?). The merge has a unified
entry point but a mixed deferred/immediate contract inside the session. Neither
concern is a correctness problem — it is a documentation and ergonomics choice.

---

## 4. Data pipeline

### 4a. Transactional write path

```
TxnSession::begin()
  │
  ▼
WriteTraversal (Gremlin builder — strings)
  │
  ▼
LogicalPlan / optimizer (strings, structural rewrites only)
  │  terminal call: .next() / .to_list() / .iter()
  ▼
PhysicalPlanBuilder::build_step
  │  resolve_vertex_label / resolve_edge_label / resolve_prop_key
  │  → label_id / prop_key_id   (once per LogicalStep)
  ▼
Volcano physical steps (numeric IDs only)
  │
  ▼
LogicalGraph (GraphCtx)
  │  detect FloatVector property → queue vector index update
  │
  ├──▶  WriteBatch → CF_VERTICES / CF_EDGES_OUT / CF_EDGES_IN
  │     (WAL-backed, OCC)
  │
  └──▶  VectorIndex::insert(entity_key, vector)
        (in-memory, under RwLock write lock — see design_vector_concurrency.md)
  │
  ▼
TxnSession::commit()  → RocksDB WAL flush + CF writes atomic
                        vector WAL entry written to CF_VECTOR_WAL
```

Crash recovery on reopen: WAL replay restores the vector index to the state
at the last snapshot, then replays `CF_VECTOR_WAL` entries forward.

### 4b. Initial bulk load path (BulkLoader)

`BulkLoader` is opened as a session on an existing (empty) `Graph` handle.
It writes at disk speed by bypassing the WAL, memtable, and OCC entirely.

```
graph.open_bulk_loader()  → BulkLoader
  │
  ├── .with_work_dir(path)  — sets custom scratch directory (defaults to temp)
  ├── .with_max_memory(b)   — caps in-memory sort budget before disk spill
  ├── .load_vertices(iter)  — streams vertices through ExternalSorter
  └── .load_edges(iter)     — streams edges through ExternalSorter

BulkLoader::commit()
  │
  ├── Phase 1: SST generation
  │     vertices + props → CF_VERTICES SST(s)
  │     edges            → CF_EDGES_OUT / CF_EDGES_IN SST(s)
  │     degree counts    → CF_VERTEX_DEGREE SST(s)
  │     schema (auto mode) → CF_SCHEMA SST
  │
  ├── Phase 2: write crash marker "pre-ingest" → CF_SCHEMA
  ├── Phase 3: IngestExternalFile (all CFs atomically) → marker "post-ingest"
  ├── Phase 4: [strict mode only] batch HNSW build → marker "post-index"
  ├── Phase 5: [strict mode only] write index snapshots → marker "post-snapshot"
  └── Phase 6: clear crash marker → session complete
```

**Schema mode behavior:**
- *Strict*: schema is read from the Graph at `open_bulk_loader()` time and frozen —
  no labels, property keys, or index entries are written during load or commit.
  Every declared vector index is batch-built from the ingested data (phases 4–5).
- *Auto*: schema is assumed to be empty; labels and property keys are auto-registered
  from data during SST generation. Vector indexes are never built — the loader cannot
  infer dimension/metric/algorithm from raw data. Build indexes explicitly after commit
  via `open_schema().add_vector_index().commit()` (see §5c).

Crash safety: `Graph::open()` reads the crash marker and recovers the appropriate
phase on restart. See `docs/api/design_bulk_loader.md` §7 for full recovery table.

### 4c. Read / traversal path

```
ReadSession / TxnSession
  │
  ▼
ReadTraversal / WriteTraversal (Gremlin builder)
  │
  ▼
PhysicalPlanBuilder (resolve strings → IDs, once per step)
  │
  ▼
Volcano steps
  │
  ├── Graph steps (V, E, out, in, has…)
  │     → LogicalSnapshot / LogicalGraph → CF point lookups / range scans
  │
  └── Vector steps (nearest, similarity, neighbors)
        → VectorIndex::search() (under RwLock read lock)
        → merge results with traverser pipeline
        → RYOW: pending_vector_ops merged in for uncommitted TxnSession
                (see design_vector_concurrency.md §5d)
```

---

## 5. Vector index integration

### 5a. DataType::FloatVector

`DataType::FloatVector` is added to the existing `DataType` enum alongside
`String`, `Int64`, etc. It carries no parameters — dimension is an index
concern, not a type concern.

```rust
pub enum DataType {
    Null, Bool, Int32, Int64, Float32, Float64,
    String, Uuid, UInt16, Bytes,
    FloatVector,   // ← new in v0.1
}
```

A property key declared as `DataType::FloatVector` accepts `GValue::FloatVector`
values. In `SchemaMode::Strict`, storing any other type in that property is
rejected at write time.

### 5b. Index declaration via SchemaSession

All four DDL operations belong in `SchemaSession`:

```rust
// Declare (creates index, triggers rebuild if property data already exists)
mgmt.add_vector_index(VectorIndexConfig { ... });

// Remove
mgmt.drop_vector_index(VectorEntityType::Vertex, "embedding");

// Change algorithm (e.g. adjust M or ef_construction; triggers rebuild)
mgmt.change_vector_index_algorithm(
    VectorEntityType::Vertex,
    "embedding",
    HnswConfig { m: 32, ef_construction: 400 },
);

// Non-blocking declare (v0.3) — commit() returns immediately,
// rebuild runs in background; check progress via graph.vector_index_stats()
mgmt.add_vector_index_async(VectorIndexConfig { ... });
```

All four go through `commit()` for CAS versioning. Index additions and
algorithm changes that trigger a rebuild use a **two-phase commit** inside
`commit()`: (1) a fast CAS writes the schema entry in a transitional state
(`Building` or `Rebuilding`), then (2) the actual HNSW build runs outside the
CAS window. The CAS conflict window stays at milliseconds regardless of build
duration. This follows the same pattern as `CREATE INDEX` in PostgreSQL and
`createIndex` in MongoDB: one user action, build is implicit. See §5e for the
full state machine and crash recovery behaviour.

> **UX note**: synchronous `add_vector_index` + `commit()` blocks until the
> index build completes, which can take minutes on a large dataset. There is
> currently no progress signal during the blocking build. `add_vector_index_async`
> (v0.3) addresses this: `commit()` returns immediately and the build runs in
> background, monitorable via `graph.vector_index_stats()`. For large initial
> loads, prefer the BulkLoader strict-mode workflow (§5c) which also builds in
> a single blocking phase but avoids double-touching the data.
>
> **Write consistency during the build**: concurrent `TxnSession` writes that
> arrive while the bulk scan is running are captured in `CF_VECTOR_WAL` and
> replayed into the new index under a brief write lock immediately before the
> schema CAS. Writes are never blocked for the full build duration. See
> `design_vector_wal.md` §8 for the full WAL catch-up protocol.

**Property key registration**: `add_vector_index` implicitly registers the
named property key as `DataType::FloatVector` if it is not already declared.
This means the separate `add_property_key("embedding", DataType::FloatVector)`
call is optional — it is only needed when you want to declare the property type
before the index (e.g. to enforce type-checking on writes before the index is
built). If the property key is already declared with a different `DataType`,
`add_vector_index` fails with `VectorError::PropertyTypeMismatch`.

### 5c. Vector index rebuild after bulk load

Two workflows depending on schema mode:

**Strict mode** — declare indexes beforehand; they auto-build at commit:

```rust
// Step 1: declare schema including vector index
let mut mgmt = g.open_schema();
mgmt.add_vertex_label("document")
    .add_property_key("embedding", DataType::FloatVector)
    .add_vector_index(VectorIndexConfig {
        property:    "embedding",
        entity_type: VectorEntityType::Vertex,
        dimension:   1536,
        metric:      DistanceMetric::Cosine,
    });
mgmt.commit()?;

// Step 2: bulk load — HNSW index built automatically at commit
let mut loader = g.open_bulk_loader()?;
loader.load_vertices(document_iter)?;
loader.load_edges(edge_iter)?;
loader.commit()?;  // phases 1–6, including batch HNSW build
```

**Auto mode** — load data first, declare and build index after:

```rust
// Step 1: bulk load — schema auto-registered, no vector index built
let mut loader = g.open_bulk_loader()?;
loader.load_vertices(document_iter)?;
loader.load_edges(edge_iter)?;
loader.commit()?;  // phases 1–3 only; FloatVector values in CF_VERTICES as blobs

// Step 2: declare index — scans CF_VERTICES and builds HNSW in one pass
g.open_schema()
    .add_vector_index(VectorIndexConfig {
        property:    "embedding",
        entity_type: VectorEntityType::Vertex,
        dimension:   1536,
        metric:      DistanceMetric::Cosine,
    })
    .commit()?;
```

The separation is clean: `BulkLoader` handles data ingestion,
`SchemaSession` handles structure.

### 5d. Operational methods on Graph

These methods work with existing indexes and do not modify the schema. They
belong on `Graph` directly — not in a session, not in `SchemaSession`.

```rust
// Full rebuild from props CF (use after SST load, tombstone accumulation,
// or quantization change)
g.rebuild_vector_index(VectorEntityType::Vertex, "embedding")?;

// Snapshot export / import (backup and restore)
g.export_vector_index(VectorEntityType::Vertex, "embedding", "/backup/emb.rgv")?;
g.import_vector_index(VectorEntityType::Vertex, "embedding", "/backup/emb.rgv")?;

// Introspection
let stats = g.vector_index_stats(VectorEntityType::Vertex, "embedding")?;
println!("{} vectors, {} tombstones, quantization: {:?}",
    stats.size, stats.tombstone_count, stats.quantization);
```

### 5e. Index lifecycle states and crash recovery

`add_vector_index`, `drop_vector_index`, and `change_vector_index_algorithm`
each combine a schema change with a long-running operational process. A crash
or process kill between the two phases must leave the graph in a recoverable
state, not a permanently broken one. Each index entry in CF_SCHEMA therefore
carries an explicit **lifecycle state** alongside its configuration.

#### Index states

| State | Set by | Meaning |
| ----- | ------ | ------- |
| `Ready` | Phase 2 of add/change; `rebuild_vector_index` | Fully built; accepts reads and writes |
| `Building` | Phase 1 of `add_vector_index` | Schema entry committed; HNSW build not yet complete |
| `Rebuilding` | Phase 1 of `change_vector_index_algorithm` | Algorithm config updated; rebuild not yet complete |
| `Dropping` | Phase 1 of `drop_vector_index` | Removal committed; in-memory index and snapshot not yet cleaned up |

#### Two-phase flow per operation

**`add_vector_index`**
```
Phase 1 — schema CAS (fast, milliseconds):
  Write index config + state = Building to CF_SCHEMA

Phase 2 — build (slow, no CAS):
  WAL_MARK = WAL_CLOCK.load()
  Scan CF_VERTICES → batch-insert into new HNSW
  WAL catch-up: replay CF_VECTOR_WAL entries with ts > WAL_MARK
  Write snapshot
  Update CF_SCHEMA: state = Ready
```

**`change_vector_index_algorithm`**
```
Phase 1 — schema CAS (fast):
  Write updated algorithm config + state = Rebuilding to CF_SCHEMA
  (old in-memory index remains live for reads during phase 2)

Phase 2 — rebuild (slow, no CAS):
  WAL_MARK = WAL_CLOCK.load()
  Scan CF_VERTICES → build new HNSW with updated parameters
  WAL catch-up: replay CF_VECTOR_WAL entries with ts > WAL_MARK
  Atomically swap new index into place
  Write snapshot
  Update CF_SCHEMA: state = Ready
```

**`drop_vector_index`**
```
Phase 1 — schema CAS (fast):
  Update CF_SCHEMA: state = Dropping
  Remove index from in-memory registry (queries immediately see it as absent)

Phase 2 — cleanup (fast, no CAS):
  Delete in-memory HNSW
  Delete snapshot file
  Delete index entry from CF_SCHEMA
```

#### Query behaviour during transitions

| State | nearest / similarity | Writes to indexed property |
| ----- | ----------------------------- | -------------------------- |
| `Building` | Index treated as absent — no results returned from HNSW; planner may fall back to brute force | WAL entries written normally; applied to index once it reaches `Ready` |
| `Rebuilding` | Served from the **old** in-memory index until phase 2 completes; then atomically swapped | WAL entries written to old index; WAL catch-up applied to new index before swap |
| `Dropping` | Index immediately absent from phase 1 onwards | WAL entries stop being generated once entry is removed from registry |

#### Crash recovery on `Graph::open`

`Graph::open` checks the lifecycle state of every index entry in CF_SCHEMA
before accepting queries.

| State found on open | What happened | Recovery action |
| ------------------- | ------------- | --------------- |
| `Building` | `add_vector_index` committed; process died before or during build | **Open question** — see §8: auto-trigger `rebuild_vector_index` from CF_VERTICES, or surface `IndexNotReady` until user calls it explicitly? |
| `Rebuilding` | `change_vector_index_algorithm` committed; process died during rebuild | **Open question** — same as `Building`: auto-rebuild with current schema config, or require explicit user action? |
| `Dropping` | `drop_vector_index` committed; process died before cleanup | Auto-complete: drop in-memory HNSW if present, delete snapshot file, remove CF_SCHEMA entry. All steps are idempotent and safe. |
| `Ready` | Clean state | Load snapshot + WAL replay as normal (see `design_vector_wal.md` §6) |

#### Retryability

- **`Building` or `Rebuilding`**: call `graph.rebuild_vector_index(entity_type, property)` to retry from CF_VERTICES ground truth. The current schema config (including any algorithm parameter changes from `change_vector_index_algorithm`) is used — no re-declaration needed.
- **Failed `drop_vector_index`**: auto-recovered by `Graph::open`; no user action required.
- **Abandon a `Building` index**: call `drop_vector_index` — it handles `Building` state gracefully, skipping cleanup of HNSW data that was never written.

---

## 6. Language binding surface

### 6a. Rust (native)

The Rust API is the authoritative surface described throughout this document.
All other bindings are thin wrappers.

```rust
// Opening
Graph::open(path)
Graph::open_with_options(path, GraphOptions)
Graph::open_with_options(path, GraphOptions, RocksOptions)

// Sessions
graph.read()             → ReadSession  { .g() → ReadTraversal }
graph.begin()            → TxnSession    { .g() → WriteTraversal, .commit(), .rollback() }
graph.open_schema()  → SchemaSession { builder methods, .commit() }

// Operational (on Graph)
graph.rebuild_vector_index(entity_type, property)
graph.export_vector_index(entity_type, property, path)
graph.import_vector_index(entity_type, property, path)
graph.vector_index_stats(entity_type, property)

// Bulk load (session on Graph)
graph.open_bulk_loader()  → BulkLoader
    { .with_work_dir(path), .load_vertices(iter), .load_edges(iter), .commit() }
```

### 6b. Python (PyO3)

The Python binding exposes the same session model as Rust through a pure-Python
traversal builder. The user writes idiomatic Python; the builder encodes each
step to bytecode internally and dispatches it via the PyO3 FFI layer.
`_execute(bytes, prop_keys)` is an implementation detail — it is not part of
the public API.

```python
# Opening
g = Graph.open("./mydb")
g = Graph.open_with_options("./mydb", GraphOptions(mode=SchemaMode.STRICT))

# Read session — pure-Python builder, bytecode encoding is internal
snap = g.read()
names = snap.g().V(1).out("knows").values("name").to_list()

# Transaction
txn = g.txn()
txn.g().addV("person").property("name", "Alice").next()
txn.commit()
txn.rollback()

# Schema management
mgmt = g.open_schema()
mgmt.add_property_key("embedding", DataType.FLOAT_VECTOR)
mgmt.add_vector_index(VectorIndexConfig(
    property    = "embedding",
    entity_type = VectorEntityType.VERTEX,
    dimension   = 1536,
    metric      = DistanceMetric.COSINE,
))
mgmt.commit()

# Operational
g.rebuild_vector_index(VectorEntityType.VERTEX, "embedding")
g.export_vector_index(VectorEntityType.VERTEX, "embedding", "/backup/emb.rgv")
stats = g.vector_index_stats(VectorEntityType.VERTEX, "embedding")

# Bulk load (session on Graph)
with g.open_bulk_loader(work_dir="/tmp/bulk") as bulk:
    bulk.load_vertices(document_iter)
    bulk.load_edges(edge_iter)
# __exit__ calls commit()
```

### 6c. TypeScript / Node.js (napi-rs)

The Node.js binding mirrors the Python surface via napi-rs. The traversal
builder is a pure-JS layer; bytecode encoding is internal. The execution path
returns native JS objects directly (no `JSON.parse`).

```typescript
const g = Graph.open("./mydb");

// Read session — pure-JS builder
const snap = g.read();
const names = snap.g().V(1).out("knows").values("name").toList();

// Transaction
const txn = g.txn();
txn.g().addV("person").property("name", "Alice").next();
txn.commit();

// Schema management
const mgmt = g.openSchema();
mgmt.addPropertyKey("embedding", DataType.FloatVector);
mgmt.addVectorIndex({ property: "embedding", entityType: "vertex",
                      dimension: 1536, metric: "cosine" });
mgmt.commit();

// Operational
g.rebuildVectorIndex("vertex", "embedding");
const stats = g.vectorIndexStats("vertex", "embedding");

// Bulk load (session on Graph)
const bulk = g.openBulkLoader({ workDir: "/tmp/bulk" });
bulk.loadVertices(vertexIter);
bulk.loadEdges(edgeIter);
bulk.commit();
```

Naming convention: Rust/Python use `snake_case`; TypeScript uses `camelCase`
(`open_schema` → `openSchema`, `load_vertices` → `loadVertices`).

---

## 7. Naming divergence between bindings

Several method and attribute names intentionally differ across language bindings
to match each language's ergonomic conventions. The canonical reference is Rust;
divergences are listed here so binding authors and documentation writers have
one place to check.

| Concept | Rust | Python | TypeScript |
| ------- | ---- | ------ | ---------- |
| Begin a transaction | `.begin()` | `.txn()` | `.txn()` |
| Get traversal source | `.g()` | `.g()` | `.g()` |
| Schema session | `open_schema()` | `open_schema()` | `openSchema()` |
| Open bulk load session | `graph.open_bulk_loader()` | `g.open_bulk_loader()` | `g.openBulkLoader()` |
| Method names generally | `snake_case` | `snake_case` | `camelCase` |

Python keeps `txn()` and `traversal()` for ergonomics — both read more naturally
than `begin()` / `g()` in Python call sites. TypeScript follows the same choice
and adds camelCase per JS convention.

---

## 8. Open questions

| Question | Context |
| -------- | ------- |
| **`SchemaSession` exposure in Python/TS** | Schema management is not yet exposed in the Python or Node.js bindings. Design for the binding layer (method names, enum types, error mapping) needs a separate doc. |
| **Async management operations** | `add_vector_index_async` (v0.3) returns a progress handle. How is this handle surfaced in Python/TS? Polling vs async/await vs callback. |
| **Operational methods placement** | `rebuild_vector_index`, `export_vector_index`, `import_vector_index` are on `Graph` directly. As the surface grows, consider whether a `graph.admin()` session makes sense for grouping. See §3a for the split-vs-merge comparison. |
| **Auto-rebuild on open for `Building` / `Rebuilding` state** | When `Graph::open` finds an index in `Building` or `Rebuilding` state (crash during phase 2), should it auto-trigger `rebuild_vector_index` from CF_VERTICES before accepting queries, or leave the index in a `NotReady` state and require explicit user action? Trade-off: auto-rebuild is safer but may block startup for minutes on a large index; `NotReady` is faster to open but forces users to know they must call `rebuild_vector_index`. Same question applies to `BulkLoader` `"post-ingest"` crash (see `docs/api/design_bulk_loader.md` §7). |
| **Query behaviour during `Building` state** | §5e currently specifies that `nearest` returns no HNSW results when an index is `Building`. Whether the planner silently falls back to brute force or surfaces an explicit `IndexNotReady` error to the caller is a UX decision that affects application error handling. |
| **BulkLoader crash recovery blocking** | After a `"post-ingest"` crash, if `Graph::open()` auto-triggers `rebuild_vector_index` for all schema-declared indexes, does it block queries until the rebuild completes? Deferred to implementation; see `docs/api/design_bulk_loader.md` §7. |
