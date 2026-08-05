# Step Coverage TODO

RocksGraph provides a Gremlin-style traversal API (see [design_principles.md](architecture/design_principles.md)).
This list is not a TinkerPop compliance checklist. It tracks which Gremlin-vocabulary steps are
still missing from the traversal API
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
  functional one — not yet implemented.
- **`branch()`** — multi-way conditional branching (sibling to `choose()`). Workaround:
  nested/chained `.choose()` calls. Not yet implemented.
- **`aggregate()` / `sideEffect()` / `store()` / `map()` / `flatMap()`** — depend on lambda
  support, which is not yet available (see the main README roadmap).
- **`inject()`** — minor utility step, low value on its own.
- **`explain()` / `profile()`** — `explain()` is implemented; `profile()` (runtime timing
  breakdown per step) is not yet implemented.
- **OLAP-style steps** (`pageRank`, `connectedComponent`, `program()`, `subgraph()`/`tree()`) —
  not yet implemented for the embedded OLTP engine.
- **Batch size configuration via `open_with_options()`** — `set_batch_size()` was removed from
  `ReadSession`/`TxSession` (v0.2). The hardcoded defaults (1024/1024/64) cover all current
  workloads. Expose `scan_vertices_batch_size` / `scan_edges_batch_size` /
  `get_adjacent_edges_batch_size` in a future `QueryOptions` or `RuntimeOptions` struct passed
  to `Graph::open_with_options()`.  Also re-enable the single-element pagination test in
  `gremlin/tests.rs` (search for the TODO comments).

---

## Code Quality & Refactoring Backlog

### Medium — repeated lock acquisition in vector WAL helpers

**Location**: `graph/logical.rs` — `maybe_record_wal_insert` / `maybe_record_wal_remove`

Each call acquires `self.vector_indexes.read()` independently. When a write
transaction modifies many properties in a loop (e.g. bulk `property()` calls),
this results in N lock acquisitions for the same unchanged map.

**Fix**: Pre-build a `HashSet<u16>` of vector-indexed property key IDs at
transaction open time (e.g. in `LogicalGraph::new`), updated only when the
schema changes. The helpers then do a plain set lookup with no locking.

---

### Low — `pending_repeat_mut` returns `&mut Option<T>` (internal API)

**Location**: `gremlin/traversal/mod.rs` — `PlanAppender` trait, line ~111

The trait method `fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder>`
leaks interior mutability: callers can do arbitrary mutations to the `Option`
(replace, take, mutate fields). Since this is `pub(crate)`, the blast radius is
bounded, but it is an anti-pattern.

**Fix**: Replace with dedicated methods:
```rust
fn take_pending_repeat(&mut self) -> Option<RepeatBuilder>;
fn set_pending_repeat(&mut self, rb: RepeatBuilder);
fn with_pending_repeat_mut<F: FnOnce(&mut RepeatBuilder)>(&mut self, f: F);
```

---

## Vector Search & Extensions

For the Vector Search feature production-readiness roadmap and tasks, see [docs/vector-search/TODO.md](file:///Users/austinhan/Workplace/RocksGraph/docs/vector-search/TODO.md).

