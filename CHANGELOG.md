# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-06

### Added
- Gremlin-inspired traversal API: `V()`, `out()`, `in()`, `both()`, `outE()`, `inE()`, `bothE()`, `outV()`, `inV()`, `otherV()`
- Predicate filtering: `has()`, `hasId()`, `hasLabel()`, `is()` with `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `between`, `within`, `without`
- Mutation steps: `addV()`, `addE()`, `drop()`, property set/drop
- Multi-property edge support with `Rank`
- Optimizer rules: `merge_v_id_filter`, `merge_end_vertex_filter`, `merge_addv_id`, `merge_adde_ids`, `reorder_filter`, `resolve_property_key`
- Physical plan `explain()` for query plan introspection
- `tracing` instrumentation behind feature gate
- Vertex-label fast-path via `LabelOnly` cache in overlay
- `LabelId` widened to `i32` (~2.1B labels)
- Auto schema mode: implicit label and property-key registration
- Read-after-write within a transaction

[Unreleased]: https://github.com/austinhan1024/RocksGraph/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/austinhan1024/RocksGraph/releases/tag/v0.1.0
