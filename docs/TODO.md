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
- [ ] **`as()`** — step labeling (sibling to `select()`). `GValue::Path` already
      carries a `step_labels: Option<SmallVec<[SmolStr; 2]>>` slot per position
      (`types/gvalue.rs`), and `path()`'s rendering code in `traversal.rs` already reads it —
      but nothing ever writes to it, since there's no `as()` step.
- [x] **`select()`** — result extraction from labelled path history.
- [x] **`not()`** — boolean filter negation.
- [ ] **`and()` / `or()`** — boolean filter composition. `where()` only takes one
      positive sub-traversal today; there's no way to compose multiple conditions.

## P1 — commonly used, moderate effort

- [x] **`order()`** (with `by()`) — sorting traversal results.
- [x] **`range()` / `skip()` / `tail()`** — pagination beyond `limit()`.
- [ ] **`group()`** — arbitrary keyed aggregation (sibling to `groupCount()`).
- [x] **`groupCount()`** — keyed count aggregation.
- [ ] **`valueMap()` / `elementMap()`** — bulk property extraction as a map (today this is
      `properties()` + `values()` as two separate steps).

## P2 — useful, narrower audience

- [x] **`sum()` / `mean()` / `max()` / `min()`** — numeric reducers alongside the existing
      `count()` / `fold()`.
- [x] **`unfold()`** — inverse of `fold()`.
- [ ] **`simplePath()` / `cyclicPath()`** — path filters (physical step implemented;
      needs traversal API exposure).
- [x] **`choose()`** — conditional traversal branching.

## P3 — deferred or likely out of scope

- **`aggregate()` / `sideEffect()` / `store()` / `map()` / `flatMap()` / `local()`** — all
  depend on lambda support, which isn't planned (see the main README roadmap).
- **`inject()` / `constant()` / `identity()`** — minor utility steps, low value on their own.
- **`explain()` / `profile()`** — introspection terminals; useful eventually, not blocking.
- **OLAP-style steps** (`pageRank`, `connectedComponent`, `program()`, `subgraph()`/`tree()`) —
  likely a permanent non-goal for a single-threaded, embedded OLTP engine rather than a
  "not yet."
