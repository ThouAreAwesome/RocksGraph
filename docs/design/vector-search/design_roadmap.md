# Vector Search — Roadmap & Design Index

Status: living document. Update whenever a version milestone is re-scoped or a
decision is finalised.

---

## Table of Contents

- [1. Version roadmap](#1-version-roadmap)
- [2. Design documents](#2-design-documents)
- [3. Key decisions already settled](#3-key-decisions-already-settled)
- [4. Open questions / future documents](#4-open-questions--future-documents)

---

## 1. Version roadmap

| Version   | Query capabilities shipped                                                                                                                                                                                                     | Management APIs shipped                                                                                          |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------- |
| **v0.1**  | `similarity`, `nearest` via BruteForce (exact, O(N)); all P1–P10 patterns; `where(similarity.is_(gt(t)))` threshold; f32 quantization                                                                           | `SchemaSession`: `add_vector_index`; `drop_vector_index`. `Graph`: `rebuild_vector_index`; `export_vector_index`; `import_vector_index`; `vector_index_stats` |
| **v0.2**  | HNSW replaces BruteForce for `nearest` and `order().by(similarity).limit()`; `neighbors` (P11); similarity score cache on traverser (C4); planner overfetch rule (P12, C6); f16 quantization (default); `withEfSearch` | `SchemaSession`: `change_vector_index_algorithm`                                                              |
| **v0.3**  | Pre-filter ANN — `V().has*().order().by(similarity).limit()` uses HNSW with eligible-key filter; `add_vector_index_async` progress handle; `withOverfetch` modulator                                                     | `SchemaSession`: `add_vector_index_async`; offline batch reindex via SST bulk load + `rebuild_vector_index`   |
| **v0.4**  | RaBitQ quantization (`Quantization::RaBitQ`) with internal re-ranking; `VectorIndexStats.quantization` introspection                                                                                                           | `rebuild_vector_index` after quantization change                                                                 |
| **v0.5+** | Multi-query fusion: `nearest("emb", [q1, q2], k, fusion="rrf")`; streaming cursors for ANN results; DiskANN for beyond-RAM graphs                                                                                           | —                                                                                                                |

Key invariants across all versions:

- **v0.1 and v0.2 ship together before first public release.** BruteForce (v0.1) is an internal development milestone used to validate the full pipeline (WAL, codec, type system, API contracts) before the usearch integration is complete. Users will never see a published version without HNSW available.
- Public API is strictly f32 — quantization is an internal memory optimisation invisible to callers.
- `VectorError` variants are frozen at v0.1; `NotImplemented` is raised for deferred features.
- IVF was evaluated and rejected — not on the roadmap. RaBitQ (v0.4) and DiskANN (v0.5) address the same use cases without IVF's training-phase and tuning burdens.

---

## 2. Design documents

Each document covers one distinct concern. Read `design_vector_search.md` first,
then the sub-document for the area you are working on.

For the overall API surface and data pipeline (session model, operation taxonomy,
SST bulk load workflow, language bindings), see [`docs/api/design_api_overview.md`](../api/design_api_overview.md).

For **end-to-end call sequences** across all common use patterns (what to call and in what order),
see [`docs/api/design_session_workflows.md`](../api/design_session_workflows.md).

| File                                                                       | What it covers                                                                                                                                                                                                                                                     | Status   |
| -------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------- |
| [../api/design_session_workflows.md](../api/design_session_workflows.md)   | **Ground-truth call sequences** for all common use patterns: auto/strict schema + incremental writes, bulk load (auto and strict), add index to existing graph, model upgrade (add-new-index-then-drop-old). Read this first to understand what to call and in what order.            | Reference |
| [design_vector_search.md](design_vector_search.md)                         | Overall architecture, type system (`FloatVector`), index configuration, query API overview, `VectorIndex` trait, performance expectations                                                                                                                          | Proposal |
| [design_vector_wal.md](design_vector_wal.md)                               | Crash consistency: `CF_VECTOR_WAL` (separate WAL CF per index type), timestamp key generator, write path atomicity, recovery replay, snapshot format, WAL trimming                                                                                                 | Proposal |
| [design_vector_concurrency.md](design_vector_concurrency.md)               | Two race conditions (WAL key collision, index mutation), fixes (`AtomicU64` timestamp clock + `RwLock` per index), option comparison (RwLock vs lock-free vs background writer), RYOW isolation fix, known timestamp-vs-commit-order limitation                    | Proposal |
| [design_ann_algorithm_and_library.md](design_ann_algorithm_and_library.md) | HNSW vs IVF comparison (10 dimensions; IVF rejected); library comparison (faiss, hnswlib, usearch, instant-distance, hora); decision: usearch for v0.2 HNSW; `EntityKey` mapping via `vector_edge_labels` CF                                                      | Proposal |
| [design_vector_api.md](design_vector_api.md)                               | **Interface-stable** API for all scenarios: index lifecycle, online CRUD, query variants, bulk load, schema evolution, introspection; stability guarantee matrix; error types                                                                                      | Proposal |
| [design_vector_codec.md](design_vector_codec.md)                           | Binary wire protocol extension: `PRIM_FLOATVECTOR` (tag 10), opcodes 61–67 (`nearest`, `similarity`, `neighbors`, hint modulators), endianness rules, Python/TS codec changes, Rust decoder additions, wire format hex examples                           | Proposal |
| [design_hnsw_impl.md](design_hnsw_impl.md)                                 | `UsearchHnswIndex`: usearch crate integration, `CanonicalEdgeKey`→monotonic u64 label mapping via `vector_edge_labels` CF, all 6 `VectorIndex` trait methods, tombstone tracking, snapshot format v2, cold-start rebuild, WAL replay, per-query `ef_search` override | Proposal |
| [design_vector_quantization.md](design_vector_quantization.md)             | Memory optimisation: f32 (opt-in), f16 (default, usearch built-in), RaBitQ (v0.4, binary projection + async SVD + internal re-ranking); quantization lifecycle (WAL replay, rebuild, snapshot, change)                                                            | Proposal |

---

## 3. Key decisions already settled

| Decision                                                                                                                                                                                                                           | Where documented                                                      |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| Pattern B (dual checkpoint + seqno replay) chosen over tight WAL coupling                                                                                                                                                          | `design_vector_search.md` §2                                          |
| `GValue::FloatVector(Vec<f32>)` — not `List<Float>`                                                                                                                                                                                | `design_vector_search.md` §4a                                         |
| Explicit `Vector([...])` wrapper in Python/JS — not auto-detect `list[float]`                                                                                                                                                      | `design_vector_search.md` §4c–4d                                      |
| Named explicit indexes via `SchemaSession::add_vector_index()` — not implicit per-property                                                                                                                                         | `design_vector_search.md` §5b                                         |
| Both vertex and edge vector indexes supported via `VectorEntityType`                                                                                                                                                               | `design_vector_search.md` §5a                                         |
| `EntityKey` enum (`Vertex(i64)` / `Edge(EdgeKey)`) — not bare `i64`                                                                                                                                                                | `design_vector_search.md` §8a                                         |
| Brute-force exact KNN in v0.1; HNSW in v0.2                                                                                                                                                                                        | `design_vector_search.md` §7                                          |
| Post-filter ANN in v0.2; true pre-filter in v0.3                                                                                                                                                                                   | `design_vector_api.md` §6                                             |
| Score annotation via `project().by(__.similarity(...))` → plain dict; threshold via `where(__.similarity(...).is_(gt(t)))`; no `ScoredVertex`/`ScoredEdge` wrapper                                                     | `design_vector_api.md` §3a, §3c                                       |
| Process-local `AtomicU64` timestamp clock — 15-byte composite WAL key (`[prop_key_id][entity_type][ts][random]`); no mutex on write path                                                                                           | `design_vector_wal.md` §3–4; `design_vector_concurrency.md` §2        |
| `Arc<RwLock<Box<dyn VectorIndex>>>` per index — `RwLock` Option A                                                                                                                                                                  | `design_vector_concurrency.md` §3–4                                   |
| RYOW fix: `NearestStep` merges HNSW results with brute-force scan of `pending_vector_ops`                                                                                                                                        | `design_vector_concurrency.md` §5d                                    |
| Separate WAL CF per index type — `CF_VECTOR_WAL` for vector indexes; each future index type (e.g. `CF_TEXT_WAL` for FTS) gets its own CF for independent compaction tuning, recovery isolation, and WAL trimming                   | `design_vector_wal.md` §2                                             |
| WAL key carries `prop_key_id`, `entity_type`, timestamp, random suffix; value carries only `op_type` + entity key + vector                                                                                                         | `design_vector_wal.md` §3                                             |
| First-open rebuild from props CF (Strategy B) — not WAL replay                                                                                                                                                                     | `design_vector_wal.md` §7                                             |
| HNSW chosen over IVF — OLTP insert pattern, no training phase required; IVF evaluated and rejected                                                                                                                                  | `design_ann_algorithm_and_library.md` §2l                             |
| usearch chosen as HNSW crate — AVX2/NEON SIMD, safe Rust API, cross-platform, active maintenance                                                                                                                                   | `design_ann_algorithm_and_library.md` §3l                             |
| `EntityKey::Vertex(id)` → u64 label direct cast; `EntityKey::Edge(cek)` → monotonic u64 via `vector_edge_labels` CF (not in-memory HashMap)                                                                                        | `design_ann_algorithm_and_library.md` §4a; `design_hnsw_impl.md` §4  |
| `.nearest(property, query, k)` — three positional args, stable signature                                                                                                                                                        | `design_vector_api.md` §6a                                            |
| Hint modulators `withEfSearch` (v0.2), `withOverfetch` (v0.3) attach to `nearest` only (opcodes 64, 66); opcode 65 reserved; `withScore`/`withMinScore`/`withMaxDistance` removed in favour of `project()` / `where()` patterns | `design_vector_api.md` §3b; `design_vector_codec.md` §8e–8f           |
| `Graph.add/drop/rebuild/change_vector_index` — stable method names, impl ships v0.2–v0.3                                                                                                                                           | `design_vector_api.md` §3                                             |
| `BulkLoader` (`graph.open_bulk_loader(work_dir)`) — SST-based initial bulk load; stable interface, impl ships v0.3                                                                                                                  | `design_bulk_loader.md`                                               |
| `VectorError` variants — frozen in v0.1; `NotImplemented` raised for deferred features                                                                                                                                             | `design_vector_api.md` §8                                             |
| Multi-vector query: `nearest(property, [v1, v2], k, fusion=)` — stable, impl v0.5+                                                                                                                                              | `design_vector_api.md` §5; §10                                        |
| `PRIM_FLOATVECTOR = 10` — next primitive tag after `PRIM_BYTES = 9`; LE f32 bulk, BE dim prefix                                                                                                                                    | `design_vector_codec.md` §3                                           |
| Opcodes 61–67 — `OP_NEAREST` (61), `OP_SIMILARITY` (62), `OP_NEIGHBORS` (63), hint modulators (64–66), `OP_NEAREST_MULTI` (67); impls spread v0.1–v0.5+                                                                | `design_vector_codec.md` §8a                                          |
| Vector data in wire format uses LE f32; all structural fields remain BE — intentional asymmetry                                                                                                                                    | `design_vector_codec.md` §12                                          |
| `GValue::FloatVector` is a top-level variant, not `Scalar(Primitive)` — not predicatable                                                                                                                                           | `design_vector_codec.md` §4                                           |
| `UsearchHnswIndex` wraps usearch `Index` + `db: Arc<DB>` + `prop_key_id` + `next_edge_label` + `tombstone_count`; no in-memory label HashMaps                                                                                      | `design_hnsw_impl.md` §3                                              |
| Vertex labels: direct `i64 as u64` bit-cast (bijective, no storage); Edge labels: monotonic u64 counter persisted in `vector_edge_labels` CF                                                                                       | `design_hnsw_impl.md` §4                                              |
| Snapshot format v2: magic `RG_V` + version + `last_replayed_timestamp` + dim + metric + algorithm + tombstone_count + next_edge_label + CRC-32C (44-byte header, no bincode blobs). **Breaking change from v1**: v1 snapshots (bincode-encoded) are not forward-compatible; `load_vector_index` returns `VectorError::SnapshotCorrupt` on a v1 file and the index cold-starts from CF_VERTICES. | `design_hnsw_impl.md` §8a                                             |
| `VectorIndex` trait gains a 6th method `set_last_replayed_timestamp` for WAL replay                                                                                                                                                | `design_hnsw_impl.md` §10                                             |
| `vector/` module gated behind `vector` Cargo feature; usearch is the only optional dep (rustc-hash/bincode removed)                                                                                                                 | `design_hnsw_impl.md` §14                                             |
| Public API is strictly f32; quantization (f16, RaBitQ) is a transparent internal memory optimisation                                                                                                                               | `design_vector_quantization.md` §2                                    |
| f16 is the default quantization (v0.2); f32 is opt-in; RaBitQ (v0.4) is opt-in                                                                                                                                                     | `design_vector_quantization.md` §3                                    |
| RaBitQ training buffer eliminated — count tracked via `fallback_index.size()`; training data read from props CF; crash-safe                                                                                                         | `design_vector_quantization.md` §3c                                   |

---

## 4. Open questions / future documents

Add a new document here when a topic grows large enough to deserve its own file.

| Topic                                                                           | Suggested filename               | Depends on                        |
| ------------------------------------------------------------------------------- | -------------------------------- | --------------------------------- |
| RaBitQ compression layer (full impl design)                                     | `design_rabitq.md`               | `design_hnsw_impl.md`             |
| Pre-filter ANN (v0.3) — passing eligible entity key sets into the index         | `design_vector_prefilter.md`     | `design_vector_api.md` §6         |
| Python / JS `Vector` type, `VectorEntityType` binding details                   | `design_vector_bindings.md`      | `design_vector_search.md` §4c–4d  |
| Background HNSW rebuild thread (tombstone threshold, shutdown safety)           | `design_hnsw_rebuild.md`         | `design_hnsw_impl.md` §7          |
| Multi-property recovery optimisation (single WAL pass, dispatch to all indexes) | add to `design_vector_wal.md` §9 | —                                 |
