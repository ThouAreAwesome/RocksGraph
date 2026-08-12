# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2] — 2026-08-12

### Added
- **BulkLoader property-type enforcement**: `load_vertices`/`load_edges` validate incoming property values against the schema's declared (Strict mode) or first-seen (Auto mode) `DataType`, rejecting mismatches with `StoreError::SchemaViolation` instead of silently accepting inconsistent types
- **Cross-validation harness**: new `bulk_load_ldbc_typed`, `oltp_load_ldbc`, and `cross_validate_load` binaries plus `scripts/run_cross_validate.sh`, verifying `BulkLoader` (Rust and Python) and `TxnSession` produce byte-identical graphs — including `FloatVector` properties — from the same diversified synthetic dataset (`scripts/generate_synthetic_ldbc.py`, covering every `DataType`)
- New Rust and Python bulk-load examples for SNAP and LDBC-shaped datasets (`bulkload_snap`, `bulkload_ldbc`)
- Property-based (`proptest`) round-trip tests: `BulkLoader` (arbitrary vertices/edges retrievable with identical properties) and bytecode `encode`/`decode` (every serializable `LogicalStep` variant, including recursive `repeat()`/`choose()`/`order()` sub-plans)
- `cargo-fuzz` target (`fuzz/decode_bytecode`) fuzzing `bytecode::decode` against arbitrary bytes
- Python: `by()` now accepts an anonymous sub-traversal as a sort key (`.by(__.out("knows").count())`, optionally with an `Order`), matching the Rust modulator below — sorts vertices/edges by a computed value without replacing the traverser with that value, unlike computing it as a preceding step
- Python: `ExecutionOptions` (scan/traversal batch size tuning) is now exposed, matching the existing Rust API — set globally via `GraphOptions(execution=ExecutionOptions(...))` or per session via `ReadSession.with_execution_options(...)` / `TxnSession.with_execution_options(...)`
- New OCC write benchmark (`bench_write_occ.rs`) and updated benchmark docs
- Python: enabled PyO3 `abi3-py39` — wheels are now ABI3-universal (one wheel covers Python 3.9+) instead of one build per minor version

### Changed
- `order_by(key, order)` removed from `TraversalBuilder`; `by()` is now a generic Gremlin-standard modulator accepting a property key (`.by("age")`), a key/direction tuple (`.by(("age", Order::Desc))`), a bare `Order` to sort the traverser's own value (`.by(Order::Desc)`), or an anonymous sub-traversal for a computed sort key (`.by(__().out(["knows"]).count())`), optionally paired with an `Order` (`.by((sub_traversal, Order::Desc))`)
- `bytecode` module is now `pub` (still `#[doc(hidden)]`), so `encode`/`decode`/`encode_response`/`decode_response` are reachable outside the crate — needed by the new fuzz target

### Performance
- Paginated `scan_vertices`/`scan_edges` no longer scan the overlay cache in O(N) per page; dropped to O(batch size) — no interface change
- `delete_vertex` no longer performs synchronous I/O for vector WAL tombstones — queued in-memory and flushed optimistically — no interface change
- Added `merge_v_into_nearest` optimizer rule, preventing unbounded `VStep` materialization ahead of a `nearest()` vector-index search — internal query-plan rewrite, no interface change

### Fixed
- Property blob encoding no longer panics or silently corrupts data on oversized values: `Bytes` over 65,535 bytes previously hit a hard `assert!` panic, `String` over the same limit had no check at all and silently truncated its length prefix, and several properties whose combined encoded size overflowed the directory's `u16` offset field silently corrupted a later property's offset — all three now return `StoreError::PropertyValueTooLarge`
- Python: fixed `py_to_primitive` type-name resolution for PyO3 0.21 — typed wrapper classes (`Int32`, `Uuid`, `Vector`, etc.) could resolve against the wrong Rust type, since PyO3 0.21 returns a fully-qualified type name where earlier versions returned the bare class name

## [0.2.0] — 2026-08

### Added
- **HNSW vector index** (via usearch): `add_vector_index()` schema declaration, `.nearest()` / `.similarity()` accelerated with approximate KNN search
- **Vector WAL**: crash-consistent durability for vector index mutations; auto-replay on open; GC on close
- **RYOW (Read-Your-Own-Writes)**: pending vector operations merged into `.nearest()` results within a transaction
- **IndexManager session**: `graph.index_manager()` for `rebuild()`, `save()`, `save_all()` operations — index maintenance decoupled from Graph
- **ExecutionOptions**: batch size configuration (`scan_vertices_batch_size`, `scan_edges_batch_size`, `get_adjacent_edges_batch_size`) via `GraphOptions.execution` or per-session `with_execution_options()`
- **Python API parity**: `.g()` replaces `.traversal()`, `.begin()` replaces `.tx()`, `Graph.open_with_options()` with `GraphOptions`/`RocksOptions`/`IndexOptions`, `SchemaSession`, `BulkLoader`, `IndexManager` fully bound
- **VectorIndexConfig** and **IndexOptions**: per-index and global memory limit enforcement with `PerIndexOptions` overrides
- Python type stubs (`__init__.pyi`) for IDE autocompletion

### Changed
- `edge_label_names()` removed; use `.E([]).label().dedup().to_list()` instead
- `set_batch_size()` removed from `ReadSession`/`TxSession`; use `ExecutionOptions` via `with_execution_options()`
- `VectorEntityType::Edge` returns explicit `Unsupported` error for unsupported edge rebuild

### Fixed
- `SstBulkLoader` deprecated in favor of `Graph::open_bulk_loader()`; still present and usable, but emits a deprecation warning

## [0.1.0] — 2026-07

### Added
- Gremlin-inspired traversal API: `V()`, `out()`, `in()`, `both()`, `outE()`, `inE()`, `bothE()`, `outV()`, `inV()`, `otherV()`
- Predicate filtering: `has()`, `hasId()`, `hasLabel()`, `is()` with `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `between`, `within`, `without`
- Boolean filter composition: `where()`, `not()`, `and()`, `or()`, `choose()`
- Pagination and ordering: `limit()`, `range()`, `skip()`, `tail()`, `dedup()`, `order()`, `order().by(key)` / `order_by(key, dir)` (including multi-key tie-breaking)
- Variable-length traversals: `repeat()` / `until()` / `emit()`
- Path tracking and labelling: `as()`, `select()`, `path()`
- Extraction & aggregation: `values()`, `properties()`, `id()`, `label()`, `count()`, `sum()`, `mean()`, `max()`, `min()`, `fold()`, `unfold()`, `group()`, `groupCount()`, `withProperties()`
- Path filters: `simplePath()`, `cyclicPath()`
- Composition: `identity()`, `constant()`, `local()`, `union()`, `coalesce()`
- Mutation steps: `addV()`, `addE()`, `drop()`, property set/drop
- **Vector search**: `FloatVector` property type; `nearest(prop, query, k)` (brute-force exact KNN, cosine similarity); `similarity(prop, query)` (per-traverser score); Python `Vector([f32, ...])` input type with auto-coercion from `list[float]`
- `addE()` upstream vertex support — `.from()` / `.to()` may be omitted to use the upstream
  traverser as that edge endpoint (e.g. `V([v1]).out("knows").addE("friends").to(v1)`),
  creating one edge per upstream traverser
- Multi-property edge support with `Rank`
- Optimizer rules: `merge_v_id_filter`, `merge_end_vertex_filter`, `merge_addv_id`, `merge_adde_ids`, `merge_haslabel_into_edge`, `reorder_filter`, `resolve_property_key`
- Physical plan `explain()` for query plan introspection
- `tracing` instrumentation behind feature gate
- Vertex-label fast-path via `LabelOnly` cache in overlay
- `LabelId` widened to `i32` (~2.1B labels)
- Auto schema mode: implicit label and property-key registration
- Read-after-write within a transaction
- `cargo-deny` dependency audit (`deny.toml`, `just audit`, CI job)
- `just release` recipe for release automation

### Fixed
- Resolved `RUSTSEC-2026-0002` (unsound `IterMut` in `lru`) by removing the `lru` and
  `parking_lot` dependencies, which were unused dead weight left over from a
  never-implemented `SharedStoreCache`

[Unreleased]: https://github.com/ThouAreAwesome/RocksGraph/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/ThouAreAwesome/RocksGraph/compare/v0.2.0...v0.2.2
[0.2.0]: https://github.com/ThouAreAwesome/RocksGraph/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ThouAreAwesome/RocksGraph/releases/tag/v0.1.0
