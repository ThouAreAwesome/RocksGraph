# Session Workflows — End-to-End Patterns

Status: ground truth. This document shows the complete call sequence for every
common use pattern. Read this first to understand what to call and in what order;
read the sub-documents for why each decision was made.

All examples use Python. Rust equivalents use the same method names in `snake_case`.

---

## Table of Contents

- [Session types at a glance](#session-types-at-a-glance)
- [Scenario 1: Fresh graph — auto schema — incremental writes](#scenario-1-fresh-graph--auto-schema--incremental-writes)
- [Scenario 2: Fresh graph — strict schema — incremental writes](#scenario-2-fresh-graph--strict-schema--incremental-writes)
- [Scenario 3: Fresh graph — auto schema — bulk load then add index](#scenario-3-fresh-graph--auto-schema--bulk-load-then-add-index)
- [Scenario 4: Fresh graph — strict schema — bulk load (index auto-built)](#scenario-4-fresh-graph--strict-schema--bulk-load-index-auto-built)
- [Scenario 5: Add vector index to an already-populated graph](#scenario-5-add-vector-index-to-an-already-populated-graph)
- [Scenario 6: Model upgrade — replace embedding model](#scenario-6-model-upgrade--replace-embedding-model)
- [Scenario 7: Crash recovery — SIGKILL between commits](#scenario-7-crash-recovery--sigkill-between-commits)

---

## Session types at a glance

| Session | Opened via | Persists to | Purpose |
|---------|-----------|-------------|---------|
| `SchemaSession` | `g.open_schema()` | CF_SCHEMA | Declare labels, property keys, vector indexes. CAS-protected. |
| `TxSession` | `g.tx()` | CF_VERTICES, CF_EDGES, CF_VECTOR_WAL | OCC read-write transaction. |
| `ReadSession` | `g.read()` | Nothing | Point-in-time snapshot query. |
| `BulkLoader` | `g.open_bulk_loader(work_dir)` | CF_VERTICES, CF_EDGES (SST ingestion) | Large initial data load, bypasses WAL and OCC. |

**The key rule**: `GraphOptions` at `Graph::open` sets only `mode` (Auto/Strict) and
`edge_mode` (Single/Multi). It carries no schema content — no labels, no property
keys, no vector indexes. All structural declarations go through `SchemaSession`.

**Environmental config** (`memory_limit_bytes`) is never persisted to disk. Supply it
per-open via `GraphOptions(vector_runtime=[VectorIndexRuntimeOpts(...)])` so a database
file is portable across machines with different RAM. Structural config (dimension, metric,
algorithm) is baked into CF_SCHEMA and must not vary per machine — machine-specific limits
must not be.

---

## Scenario 1: Fresh graph — auto schema — incremental writes

**When**: prototyping, or when the schema should evolve naturally from the data.
Vertex labels and non-vector property keys register automatically on first write.
Only vector indexes require an explicit declaration before the first vector write.

```
[1]  g = Graph(path)
     ──▶ CF_SCHEMA created: mode=Auto
         No labels, no keys, no indexes yet.

[2]  with g.open_schema() as sess:
         sess.add_vector_index(VectorIndexConfig(
             entity_type = VectorEntityType.VERTEX,
             property    = "embedding",
             dimension   = 1536,
             metric      = DistanceMetric.COSINE,
             algorithm   = HnswConfig(m=16, ef_construction=200),
         )).commit()
     ──▶ CF_SCHEMA: vector index entry persisted (state=Ready)
         HNSW index: empty, loaded into memory

[3]  tx = g.tx()
     tx.traversal()
         .addV("doc")                              # label "doc" auto-registered in CF_SCHEMA
         .property("id", n)
         .property("title", "...")                 # key "title" auto-registered in CF_SCHEMA
         .property("embedding", Vector(v))         # CF_VECTOR_WAL entry written; HNSW updated
         .next()
     tx.commit()
     ──▶ CF_VERTICES: vertex written
         CF_VECTOR_WAL: WAL entry for "embedding"
         HNSW: vector inserted in-memory
         (repeat for each document)

[4]  g.read().traversal().vectorNear("embedding", q, k).to_list()
     ──▶ HNSW in-memory search (brute-force v0.1, ANN v0.2)
```

**Subsequent opens:**
```
g = Graph(path)
    ├── CF_SCHEMA loaded → mode=Auto + vector index config
    ├── HNSW snapshot loaded (state at last flush — clean shutdown or manual checkpoint)
    │     if no snapshot: full rebuild from CF_VERTICES (Strategy B)
    ├── CF_VECTOR_WAL replay: all entries with ts > snapshot.last_replayed_timestamp
    │     duration scales with write volume since last flush — seconds to minutes
    │     if memory_limit_bytes hit during replay: remaining entries skipped
    │     → index is partial; rebuild or reopen with higher limit (see Scenario 7)
    └── Ready
```

> **Memory cap (optional):** `Graph(path, vector_runtime=[VectorIndexRuntimeOpts(entity_type=VERTEX, property="embedding", memory_limit_bytes=5*GiB)])` — applied per-open, never saved to disk.

> **For crash recovery** (SIGKILL, OOM during WAL replay, BulkLoader crash), see [Scenario 7](#scenario-7-crash-recovery--sigkill-between-commits).

---

## Scenario 2: Fresh graph — strict schema — incremental writes

**When**: production deployments where schema must be locked down. Any write using an
undeclared label or property key is rejected with `StoreError::SchemaViolation`.
Labels, property keys, and vector indexes are all declared in the same `SchemaSession`.

```
[1]  g = Graph(path, options=GraphOptions(mode=SchemaMode.STRICT))
     ──▶ CF_SCHEMA created: mode=Strict
         No labels, no keys, no indexes yet.

[2]  with g.open_schema() as sess:
         sess.add_vertex_label("doc") \
             .add_edge_label("cites") \
             .add_property_key("title",     DataType.STRING) \
             .add_property_key("embedding", DataType.FLOAT_VECTOR) \  # optional — implied by add_vector_index
             .add_vector_index(VectorIndexConfig(
                 entity_type = VectorEntityType.VERTEX,
                 property    = "embedding",
                 dimension   = 1536,
                 metric      = DistanceMetric.COSINE,
                 algorithm   = HnswConfig(m=16, ef_construction=200),
             )).commit()
     ──▶ CF_SCHEMA: labels + keys + vector index persisted
         HNSW index: empty, loaded into memory

[3]  tx = g.tx()
     tx.traversal()
         .addV("doc")                              # ✓ "doc" declared
         .property("id", n)
         .property("title", "...")                 # ✓ "title" declared
         .property("embedding", Vector(v))         # ✓ "embedding" declared; HNSW updated
         .next()
     tx.commit()
     ──▶ CF_VERTICES + CF_VECTOR_WAL written; HNSW updated
         addV("unknown") or .property("new_key", ...) → StoreError::SchemaViolation

[4]  g.read().traversal().vectorNear("embedding", q, k).to_list()
     ──▶ HNSW in-memory search
```

**Subsequent opens:**
```
g = Graph(path, options=GraphOptions(mode=SchemaMode.STRICT))
    ├── CF_SCHEMA loaded → mode=Strict + all labels/keys + vector index config
    ├── HNSW snapshot loaded; CF_VECTOR_WAL replay (duration scales with write volume since last flush)
    └── Schema violations enforced immediately on first write
```

> **Note:** `SchemaSession` can be called again at any time to add new labels, property
> keys, or indexes — each call is its own CAS-protected commit. The `mode=Strict`
> applies only to data writes, not to schema evolution.

---

## Scenario 3: Fresh graph — auto schema — bulk load then add index

**When**: large initial dataset (millions of vertices/edges) where transaction overhead
is too high. Data is ingested via SST files; the vector index is built in one scan
after ingestion completes.

```
[1]  g = Graph(path)
     ──▶ CF_SCHEMA created: mode=Auto

[2]  with g.open_bulk_loader(work_dir="/tmp/bulk") as loader:
         loader.load_vertices(vertex_iter)
         loader.load_edges(edge_iter)
     # commit() runs automatically on __exit__:
     #   Phase 1: sort + generate SST files (bounded memory via ExternalSorter)
     #   Phase 2: write crash marker "pre-ingest"
     #   Phase 3: IngestExternalFile (atomic; marker updated to "post-ingest")
     #   Phase 4: skipped — no vector index declared in auto mode
     #   Phase 6: clear crash marker
     ──▶ CF_VERTICES + CF_EDGES: all data live (SST ingested)
         CF_SCHEMA: labels + non-vector keys auto-registered from data
         FloatVector blobs in CF_VERTICES — not yet indexed

[3]  with g.open_schema() as sess:
         sess.add_vector_index(VectorIndexConfig(
             entity_type = VectorEntityType.VERTEX,
             property    = "embedding",
             dimension   = 1536,
             metric      = DistanceMetric.COSINE,
             algorithm   = HnswConfig(m=16, ef_construction=200),
         )).commit()
     # commit() triggers a two-phase build:
     #   Phase 1 (fast CAS): CF_SCHEMA updated → state=Building
     #   Phase 2 (slow): CF_VERTICES scan → batch HNSW insert
     #                   WAL catch-up: any TxSession writes during scan included
     #                   CF_SCHEMA updated → state=Ready
     ──▶ CF_SCHEMA: vector index persisted (state=Ready)
         HNSW: all pre-existing FloatVector values indexed

[4]  g.read().traversal().vectorNear("embedding", q, k).to_list()
     ──▶ HNSW in-memory search
```

**Subsequent opens:**
```
g = Graph(path)
    ├── CF_SCHEMA loaded → mode=Auto + vector index config
    ├── HNSW snapshot loaded; CF_VECTOR_WAL replay (duration scales with write volume since last flush)
    └── Ready
```

> **Warning — blocking commit:** `commit()` on a session that includes `add_vector_index`
> is not a fast CAS. It blocks the calling thread for the full CF_VERTICES scan duration —
> minutes to tens of minutes for large datasets. This is fundamentally different from
> `commit()` on sessions that only add labels or property keys (those complete in milliseconds).
> Use `add_vector_index_async` (v0.3) to avoid blocking the caller.
>
> **Concurrent writes during the build:** TxSession writes using **already-registered**
> labels and property keys are not blocked — WAL catch-up handles them. However, the first
> write that would trigger **auto-registration of a new label or property key** during the
> build will encounter a schema CAS conflict (`StoreError::Conflict`) and must be retried
> after the build completes. In auto-schema mode, declare all labels and property keys you
> plan to use before calling `add_vector_index`, or use `add_vector_index_async` (v0.3) to
> narrow the CAS conflict window.

---

## Scenario 4: Fresh graph — strict schema — bulk load (index auto-built)

**When**: large initial dataset with a fully known, pre-declared schema. The vector
index is built inside `BulkLoader.commit()` in a single efficient batch pass —
no separate schema step after load.

```
[1]  g = Graph(path, options=GraphOptions(mode=SchemaMode.STRICT))
     ──▶ CF_SCHEMA created: mode=Strict

[2]  with g.open_schema() as sess:
         sess.add_vertex_label("doc") \
             .add_edge_label("cites") \
             .add_property_key("title",     DataType.STRING) \
             .add_property_key("embedding", DataType.FLOAT_VECTOR) \  # optional
             .add_vector_index(VectorIndexConfig(
                 entity_type = VectorEntityType.VERTEX,
                 property    = "embedding",
                 dimension   = 1536,
                 metric      = DistanceMetric.COSINE,
                 algorithm   = HnswConfig(m=16, ef_construction=200),
             )).commit()
     ──▶ CF_SCHEMA: all labels + keys + vector index persisted
         HNSW index: empty (no data yet), in-memory

[3]  with g.open_bulk_loader(work_dir="/tmp/bulk") as loader:
         loader.load_vertices(vertex_iter)   # validates against CF_SCHEMA; rejects unknown labels/keys
         loader.load_edges(edge_iter)
     # commit() runs automatically on __exit__ (strict mode):
     #   Phase 1: sort + generate SST files
     #   Phase 2: write crash marker "pre-ingest"
     #   Phase 3: IngestExternalFile (atomic; marker updated to "post-ingest")
     #   Phase 4: CF_VERTICES scan → batch HNSW build (no per-insert RwLock contention)
     #            marker updated to "post-index"
     #   Phase 5: write HNSW snapshot; marker updated to "post-snapshot"
     #   Phase 6: clear crash marker
     ──▶ CF_VERTICES + CF_EDGES: all data live
         HNSW: all vectors indexed in one pass
         HNSW snapshot: written to disk

[4]  g.read().traversal().vectorNear("embedding", q, k).to_list()
     ──▶ HNSW in-memory search (immediately, no rebuild needed)
```

**Subsequent opens:**
```
g = Graph(path, options=GraphOptions(mode=SchemaMode.STRICT))
    ├── CF_SCHEMA loaded → mode=Strict + labels/keys + vector index config
    ├── HNSW snapshot loaded (written during bulk load commit — recent)
    ├── CF_VECTOR_WAL replayed (only TxSession writes since bulk load; fast on first reopen)
    └── Index ready
```

> **Note:** Steps [2] and [3] are independent sessions and may run in separate
> process invocations (e.g., declare schema at provisioning time, run bulk load
> as a batch job). `BulkLoader` reads the schema from CF_SCHEMA at open time;
> it does not need the same `Graph` handle as step [2].

---

## Scenario 5: Add vector index to an already-populated graph

**When**: the graph has been running without a vector index. FloatVector property
values already exist in CF_VERTICES. You want to enable ANN search retroactively.

```
[0]  (existing graph — opened and used without vector index)
     CF_SCHEMA: labels + keys (no vector index entry)
     CF_VERTICES: FloatVector blobs present but unindexed

[1]  g = Graph(existing_path)   # or Graph(existing_path, options=...)
     ──▶ CF_SCHEMA loaded (no vector index → no HNSW in memory)

[2]  with g.open_schema() as sess:
         sess.add_vector_index(VectorIndexConfig(
             entity_type = VectorEntityType.VERTEX,
             property    = "embedding",
             dimension   = 1536,
             metric      = DistanceMetric.COSINE,
             algorithm   = HnswConfig(m=16, ef_construction=200),
         )).commit()
     # commit() two-phase build:
     #   Phase 1 (fast, ~ms): CF_SCHEMA → state=Building
     #   Phase 2 (slow, minutes): CF_VERTICES full scan → batch HNSW insert
     #                            WAL catch-up: TxSession writes during scan included
     #                            CF_SCHEMA → state=Ready
     ──▶ CF_SCHEMA: index entry persisted (state=Ready)
         HNSW: all pre-existing + concurrent writes indexed

     TxSession writes continue unblocked during Phase 2:
       ├── Each commit appends to CF_VECTOR_WAL
       └── After scan: WAL catch-up replays those entries into the new HNSW
           No writes lost. No writes blocked.

[3]  g.read().traversal().vectorNear("embedding", q, k).to_list()
     ──▶ HNSW in-memory search (all pre-existing + new data included)
```

**Subsequent opens:**
```
g = Graph(existing_path)
    ├── CF_SCHEMA loaded → existing labels/keys + new vector index config
    ├── HNSW snapshot loaded; CF_VECTOR_WAL replay (duration scales with write volume since last flush)
    └── Index fully operational
```

> **Warning — blocking commit:** Same as Scenario 3: `commit()` with `add_vector_index`
> on a populated graph blocks the caller for the full CF_VERTICES scan duration (minutes
> to tens of minutes). In strict mode, concurrent TxSession writes to declared labels/keys
> are never blocked — WAL catch-up handles them. Use `add_vector_index_async` (v0.3) to
> avoid blocking.
>
> **Dimension mismatch:** FloatVector values stored before the index was declared are
> included in the scan. Entries with a mismatched dimension are skipped (logged +
> counted in the `SkippedCount` field of the build result), not treated as fatal.

---

## Scenario 6: Replacing an index via add-new-then-drop-old

**When**: the embedding model has changed (new dimension, new semantic space).
There is no in-place replace operation. Replacement is achieved by declaring a
new index under a different property name, backfilling its data, then dropping
the old index. Both indexes coexist during the migration window so you can
canary-test the new model before discarding the old one.

```
[0]  (existing graph with "embedding" index, dim=384, Cosine)

[1]  with g.open_schema() as sess:
         sess.add_vector_index(VectorIndexConfig(
             entity_type = VectorEntityType.VERTEX,
             property    = "embedding_v2",          # new property name for new model
             dimension   = 1536,
             metric      = DistanceMetric.COSINE,
             algorithm   = HnswConfig(m=16, ef_construction=200),
         )).commit()
     ──▶ CF_SCHEMA: "embedding_v2" index entry persisted (empty)
         Both "embedding" (v1) and "embedding_v2" (v2) indexes live in memory

[2]  for vid, text in all_docs():
         tx = g.tx()
         tx.traversal().V(vid) \
             .property("embedding_v2", Vector(new_model.encode(text))) \
             .next()
         tx.commit()
     ──▶ CF_VERTICES: "embedding_v2" values written per vertex
         CF_VECTOR_WAL: WAL entry per write
         HNSW v2: vector inserted incrementally
         (v1 "embedding" index continues serving queries in parallel)

     # Optional canary test before dropping v1:
     old = g.read().traversal().V([sample_id]).vectorNear("embedding",    q, 5).to_list()
     new = g.read().traversal().V([sample_id]).vectorNear("embedding_v2", q, 5).to_list()
     # V() takes a list of IDs; V([id]) is the canonical single-ID form
     # compare quality, then proceed

[3]  with g.open_schema() as sess:
         sess.drop_vector_index(
             entity_type = VectorEntityType.VERTEX,
             property    = "embedding",
         ).commit()
     ──▶ CF_SCHEMA: "embedding" entry removed
         HNSW v1: freed from memory
         "embedding" blobs remain in CF_VERTICES (still readable via .values())

[4]  g.read().traversal().vectorNear("embedding_v2", q, k).to_list()
     ──▶ HNSW v2 in-memory search only
```

**Subsequent opens:**
```
g = Graph(path)
    ├── CF_SCHEMA: only "embedding_v2" index loaded (v1 dropped)
    ├── HNSW v2 snapshot loaded; CF_VECTOR_WAL replay (duration scales with write volume since last flush)
    └── Queries against "embedding" raise VectorError::NoVectorIndex
```

> **Large dataset shortcut:** replace step [2] with an SST bulk write of all new
> embeddings via `BulkLoader` (auto mode), then call
> `g.rebuild_vector_index(VERTEX, "embedding_v2")` — faster than per-transaction
> inserts for datasets over ~100K vectors.

---

## Scenario 7: Crash recovery — SIGKILL between commits

**When**: the process was killed (SIGKILL, OOM, power loss) at any point. Every
committed `TxSession` is durable — the WAL entry is in `CF_VECTOR_WAL`. Only the
in-memory HNSW state may be stale or absent.

Three distinct crash paths, each with different recovery behavior.

---

### Case A — SIGKILL during normal operation (most common)

```
[0]  (running graph — 10 minutes since last HNSW snapshot flush)
     CF_SCHEMA: "embedding" index (state=Ready)
     HNSW snapshot: last_replayed_timestamp = T₀  (10 minutes ago)
     CF_VECTOR_WAL: 30,000 entries written since T₀
     HNSW in-memory: includes those 30,000 — NOT flushed

     ─── SIGKILL ────────────────────────────────────────────────
     In-memory HNSW state lost. CF_VECTOR_WAL is intact —
     every TxSession::commit() wrote its WAL entry durably before
     updating the in-memory index.

[1]  g = Graph(path)
     ──▶ CF_SCHEMA loaded → index config present, state=Ready
         HNSW snapshot loaded: last_replayed_timestamp = T₀

         CF_VECTOR_WAL replay:
           seek to (prop_key_id=embedding, ts > T₀)
           normal path:   all 30,000 entries replayed (seconds–minutes)
           memory-limited: replay stops when memory_limit_bytes hit
                           remaining entries skipped
                           LOG WARN: "index 'embedding': N WAL entries
                                      skipped (memory limit exceeded)"
                           → index is partial

     ──▶ Ready (full) or Ready (partial) depending on memory

[2]  (if partial — two recovery options):

     # Option A: reopen with higher memory limit
     g = Graph(path, vector_runtime=[VectorIndexRuntimeOpts(
         entity_type=VectorEntityType.VERTEX, property="embedding",
         memory_limit_bytes=10*GiB,
     )])

     # Option B: rebuild from CF_VERTICES ground truth (always correct)
     g.rebuild_vector_index(VectorEntityType.VERTEX, "embedding")
     ──▶ full scan of CF_VERTICES → HNSW rebuilt from scratch
         slower than WAL replay but unaffected by WAL depth or memory
```

**Key invariant**: every `TxSession.commit()` that returned `Ok` wrote its
`CF_VECTOR_WAL` entry in the same `WriteBatch` as the graph mutation — one
`fsync` covers both. No committed vector is permanently lost; only the in-memory
HNSW state needs to be reconstructed. See `design_vector_wal.md` for the full
WAL key layout, snapshot format, and trimming policy.

---

### Case B — SIGKILL during BulkLoader commit (Scenario 4 path)

```
[0]  (BulkLoader.commit() in progress — crash marker written at each phase)

     ─── SIGKILL at any point ───────────────────────────────────

[1]  g = Graph(path, options=GraphOptions(mode=SchemaMode.STRICT))
     ──▶ CF_SCHEMA loaded → crash marker found
```

Recovery by crash marker:

| Marker found | What was completed | Recovery action |
|---|---|---|
| `pre-ingest` | SST files generated; not yet ingested | Discard SST files; treat as if BulkLoader never ran |
| `post-ingest` | Data ingested; HNSW build interrupted | Re-run batch HNSW build from CF_VERTICES; write snapshot; clear marker |
| `post-index` | HNSW built; snapshot write interrupted | Re-write HNSW snapshot; clear marker |
| `post-snapshot` | All phases done; marker not yet cleared | Clear marker — this is the only remaining step |
| *(absent)* | Clean shutdown | Normal WAL replay |

All recovery paths are idempotent — safe to re-run after any crash at any phase.
See `design_bulk_loader.md §7` for the full recovery table.

---

### Case C — SIGKILL during `add_vector_index` build (Scenario 3/5 path)

```
[0]  (SchemaSession.commit() with add_vector_index — Phase 2 CF_VERTICES scan
      running in-process)
     CF_SCHEMA: "embedding" state=Building  (Phase 1 CAS committed)
     HNSW build: partially complete in-memory — NOT persisted

     ─── SIGKILL ────────────────────────────────────────────────
     Partial HNSW data lost. CF_SCHEMA still shows state=Building.

[1]  g = Graph(path)
     ──▶ CF_SCHEMA loaded → "embedding" state=Building
         No snapshot file exists (build never completed)
         Recovery: auto-rebuild from CF_VERTICES
           same scan as Phase 2 of add_vector_index
           CF_SCHEMA updated → state=Ready
           HNSW snapshot written
     ──▶ Ready (rebuild duration same as original build)
```

> If auto-rebuild on open is not desired (build takes too long at startup), call
> `g.rebuild_vector_index(VectorEntityType.VERTEX, "embedding")` explicitly after
> opening. See `design_api_overview.md §5e` for the open question on auto-rebuild
> vs. `NotReady` state.
