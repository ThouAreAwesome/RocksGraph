# Step Coverage TODO

RocksGraph is Gremlin-inspired, not Gremlin-compatible (see
[design_principles.md](design_principles.md)) — this list is not a TinkerPop compliance
checklist. It tracks which Gremlin-vocabulary steps are still missing from the traversal API
(`gremlin/traversal.rs`) and the physical engine (`engine/volcano/steps/`), prioritized by how
much they'd unblock real use cases versus how niche they are for a single-threaded, embedded,
lambda-free engine.

Checked against both layers directly — nothing below is "implemented but not exposed"; each
item needs both a `GraphTraversal` method and a new physical step.

## P0 — foundational gaps

These block entire classes of queries, not just convenience.

- [x] **`repeat()` / `until()` / `emit()`** — variable-length traversals (N-hop neighbors,
      reachability, recursive paths).
- [x] **`as()`** — step labeling (sibling to `select()`), exposed as `.as_(label)`
      (`as` is a Rust keyword).
- [x] **`select()`** — result extraction from labelled path history.
- [x] **`not()`** — boolean filter negation.
- [x] **`and()` / `or()`** — boolean filter composition, exposed as `.and([...])` /
      `.or([...])` taking a list of sub-traversals.
- [x] **`order()` `by()`** — `.by(key)` / `.order_by(key, dir)` do real,
      schema-resolved, property-based sorting. Chaining `.by(k1).by(k2)` correctly
      appends for multi-key tie-breaking (only replaces the default `Value` placeholder
      on the first call) — verified empirically and covered by
      `test_builder_order_by_two_keys_tie_break` in `order_tests.rs`.

## P1 — commonly used, moderate effort

- [x] **`range()` / `skip()` / `tail()`** — pagination beyond `limit()`.
- [x] **`group()`** — arbitrary keyed aggregation (sibling to `groupCount()`), exposed
      as `.group()`.
- [x] **`groupCount()`** — keyed count aggregation.

## P2 — useful, narrower audience

- [x] **`sum()` / `mean()` / `max()` / `min()`** — numeric reducers alongside the existing
      `count()` / `fold()`.
- [x] **`unfold()`** — inverse of `fold()`.
- [x] **`simplePath()` / `cyclicPath()`** — path filters, exposed as `.simple_path()` /
      `.cyclic_path()`.
- [x] **`choose()`** — conditional traversal branching.
- [x] **`identity()` / `constant()` / `local()`** — these don't actually require lambda
      support (they take a fixed value or a sub-traversal, not a closure) and are
      implemented: `.identity()`, `.constant(value)`, `.local(__().xxx())`.

## P3 — deferred past the first publish (not blocking, workarounds exist)

- **`valueMap()` / `elementMap()`** — bulk property extraction as a map. Workaround:
  `.properties([...])` + `.values([...])` as two separate steps. Ergonomic gap, not a
  functional one — deliberately deferred past v0.1.0.
- **`branch()`** — multi-way conditional branching (sibling to `choose()`). Workaround:
  nested/chained `.choose()` calls. Deliberately deferred past v0.1.0.
- **`aggregate()` / `sideEffect()` / `store()` / `map()` / `flatMap()`** — depend on lambda
  support, which isn't planned (see the main README roadmap).
- **`inject()`** — minor utility step, low value on its own.
- **`explain()` / `profile()`** — `explain()` is implemented; `profile()` (runtime timing
  breakdown per step) is not.
- **OLAP-style steps** (`pageRank`, `connectedComponent`, `program()`, `subgraph()`/`tree()`) —
  likely a permanent non-goal for a single-threaded, embedded OLTP engine rather than a
  "not yet."
