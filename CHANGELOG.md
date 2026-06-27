# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-06

### Added
- Gremlin-inspired traversal API: `V()`, `out()`, `in()`, `both()`, `outE()`, `inE()`, `bothE()`, `outV()`, `inV()`, `otherV()`
- Predicate filtering: `has()`, `hasId()`, `hasLabel()`, `is()` with `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `between`, `within`, `without`
- Boolean filter composition: `where()`, `not()`, `and()`, `or()`, `choose()`
- Pagination and ordering: `limit()`, `range()`, `skip()`, `tail()`, `dedup()`, `order()`, `order().by(key)` / `order_by(key, dir)` (including multi-key tie-breaking)
- Variable-length traversals: `repeat()` / `until()` / `emit()`
- Path tracking and labelling: `as()`, `select()`, `path()`
- Extraction & aggregation: `values()`, `properties()`, `id()`, `label()`, `count()`, `sum()`, `mean()`, `max()`, `min()`, `fold()`, `unfold()`, `group()`, `groupCount()`
- Path filters: `simplePath()`, `cyclicPath()`
- Composition: `identity()`, `constant()`, `local()`, `union()`, `coalesce()`
- Mutation steps: `addV()`, `addE()`, `drop()`, property set/drop, `withProperties()`
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

[Unreleased]: https://github.com/austinhan1024/RocksGraph/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/austinhan1024/RocksGraph/releases/tag/v0.1.0
