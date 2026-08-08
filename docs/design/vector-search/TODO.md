# Vector Search Production Readiness & Roadmap TODO

This document tracks all remaining tasks, operational enhancements, and roadmap milestones for Vector Search in RocksGraph.

---

## 1. P0: 24/7 Production Durability & Storage Hygiene

These tasks are required for long-running production server deployments to bound disk growth and restart recovery time.

- [x] **WAL Trimming / Garbage Collection on Close (`gc_vector_wal`)**
  - *Context*: `CF_VECTOR_WAL` accumulates entries on every commit.
  - *Action*: Implemented in `gc_vector_wal`: after saving snapshots during `close()`, obsolete WAL records ($ts \le \text{cutoff\_ts}$) are purged from `CF_VECTOR_WAL`.
  - *Target*: [api.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/api.rs) `close()`.

- [ ] **Background / Periodic Checkpointing (Online Snapshotting)**
  - *Context*: Snapshots are currently saved on `Graph::close()`, `Graph::rebuild_vector_index()`, or explicit `Graph::save_vector_indexes()`. In long-running 24/7 server environments that risk ungraceful termination (SIGKILL / OOM / power loss), the lack of periodic snapshots means crash recovery must replay large volumes of accumulated WAL entries.
  - *Cost Characteristics*:
    - **Monolithic rewrite**: HNSW proximity graphs mutate in-place, so each snapshot writes the full index to a temporary file with CRC-32C before atomic rename ($\approx 76\text{ MB}$ for 100k 128-dim vectors, $\sim 38\text{ ms}$ on NVMe).
    - **Concurrency**: `save()` acquires only a shared read lock, so concurrent read searches (`nearest()`) are never blocked; only write transactions briefly wait if attempting to mutate during the writeout.
  - *Action*: Introduce configurable checkpoint policies in `GraphOptions`:
    - Time-based: e.g. `checkpoint_interval: Option<Duration>` (e.g. every 15–30 minutes).
    - Mutation-based: e.g. `checkpoint_mutation_threshold: Option<u64>` (e.g. every 50,000 vector writes).
    - Spawns a background maintenance worker on `Graph::open` that triggers `save_vector_indexes()` and `gc_vector_wal()`.
  - *Target*: [api.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/api.rs) background loop, `GraphOptions`.

---

## 2. P1: Index Maintenance & High-Volume Ingestion

- [ ] **Tombstone Compaction / Auto-Rebuild Trigger**
  - *Context*: Updates and deletions in HNSW increment `tombstone_count`. High update churn degrades search recall and memory efficiency.
  - *Action*: Expose a compaction/rebuild policy when `tombstone_ratio() > threshold` (e.g., > 20%), rebuilding from `CF_VERTICES` and replacing the active index atomically.
  - *Target*: [hnsw.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/vector/hnsw.rs), [api.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/api.rs).

- [x] **BulkLoader Automated Vector Index Rebuild on Commit**
  - *Context*: `BulkLoader` writes SST files directly, bypassing transactions and the vector WAL.
  - *Action*: Implemented in `BulkLoader::commit()`: after SST files are ingested, any declared vertex vector indexes are automatically rebuilt from `CF_VERTICES`, saved to snapshot files, and swapped into the live graph, making bulk-loaded vectors immediately searchable with zero manual steps.
  - *Target*: [loader.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/bulk/loader.rs).

---

## 3. P2: High-Concurrency & Online Schema Evolution

- [ ] **Online `add_vector_index` WAL Catch-Up**
  - *Context*: Adding a vector index on a live database with millions of existing vertices and concurrent write traffic.
  - *Action*: Implement the two-phase build from `design_vector_wal.md` §8:
    1. Mark `WAL_MARK = current_ts()`.
    2. Bulk-scan existing vertex properties without holding global write lock.
    3. Under a brief write lock, replay WAL entries with `ts > WAL_MARK` before publishing schema changes to `CF_SCHEMA`.
  - *Target*: [management.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/schema/management.rs).

---

## 4. P3: Observability, SRE & Python Ergonomics

- [ ] **Vector Index Stats & Diagnostics API**
  - *Context*: SREs and developers need visibility into memory consumption and index health.
  - *Action*: Expose `Graph::vector_index_stats(entity_type, property)` returning:
    - Current memory footprint vs configured limit (`memory_limit_bytes`).
    - Live vector count and internal capacity.
    - Tombstone count and tombstone ratio.
    - Last replayed WAL timestamp and HWM.
  - *Target*: [api.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/api.rs), [lib.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/lib.rs).

- [ ] **Structured Tracing & Telemetry**
  - *Context*: Lack of runtime telemetry during recovery and snapshot operations.
  - *Action*: Add `tracing::info!` / `tracing::warn!` instrumentation to snapshot saving, startup WAL replay duration/count, and memory limit rejections.
  - *Target*: [api.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/api.rs), [hnsw.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/vector/hnsw.rs).

- [ ] **Python Bindings Parity for Maintenance APIs**
  - *Context*: Python bindings currently support search and schema declaration, but not maintenance controls.
  - *Action*: Expose in Python:
    - `g.rebuild_vector_index(entity_type, property)`
    - `g.save_vector_indexes()`
    - `g.vector_index_stats(entity_type, property)`
    - `IndexOptions` parameter in `Graph.open(...)`.
  - *Target*: `bindings/python/src/lib.rs`.

---

## 5. P4: Vector Search v0.3 Roadmap & Performance Optimizations

- [ ] **PendingVectorOp Allocation Optimization**
  - *Context*: `PendingVectorOp::Inserted` clones embedding vectors (`vec.to_vec()`). In heavy batch write workloads, this creates temporary allocation overhead.
  - *Action*: Explore using `Arc<[f32]>` or reference counting for zero-copy sharing between overlay and pending vector ops.
  - *Target*: [logical.rs](file:///Users/austinhan/Workplace/RocksGraph/rocksgraph/src/graph/logical.rs).

- [ ] **BruteForceIndex Memory Limit Enforcement**
  - *Context*: `VectorIndex::set_memory_limit` is currently a no-op default on `BruteForceIndex`.
  - *Action*: Implement explicit memory bounding for `BruteForceIndex` for consistency with `UsearchHnswIndex`.
  - *Target*: `rocksgraph/src/vector/brute_force.rs`.

- [ ] **Edge Vector Indexing**
  - Support `VectorEntityType::Edge` using persistent `vector_edge_labels` column family mapping `u64` labels to `CanonicalEdgeKey`.
- [ ] **Pre-Filtered ANN**
  - Combining vector search with Gremlin graph predicate filters before graph traversal expansion.
- [ ] **Score Modulator Steps**
  - `withScore()` / `by(desc)` sorting integration in Gremlin traversal pipes.

---

## Snapshot Persistence Performance Risk

`save_vector_indexes()` / `save_vector_index()` are **synchronous — they run on the calling
thread and block vector insertions** on the index being saved (`.nearest()` queries are not
blocked).  For large indexes this can cause latency spikes:

- Serialises the entire usearch HNSW graph to a ~60 GB buffer for 10M×1536-dim indexes.
- Writes via tmp file + `fsync` + atomic rename.
- Holds a read lock on the index — vector insertions queue behind it.

**Mitigations (future):**
- [ ] Background / async snapshot writer (clone index under read lock, write in background thread).
- [ ] Incremental snapshot (only serialize nodes modified since last save).
- [ ] Periodic checkpoint policy in `GraphOptions` (e.g., every N minutes or every M inserts).

**Current guidance (v0.2):** Call `save_vector_indexes()` only during maintenance windows or at
shutdown (`Graph::close()`).  The WAL is the canonical durability mechanism; snapshots are a
cold-start optimization.
