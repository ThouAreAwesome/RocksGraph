# Codebase Assessment & Improvement Roadmap

> Generated: 2026-06-24. Covers the RocksGraph codebase at the point where
> `repeat()`/`until()`/`emit()`/`emit_if()` were implemented (345 tests passing).

---

## Table of Contents

1. [Scorecard](#scorecard)
2. [Architecture & Module Organization](#1-architecture--module-organization)
3. [User Interface](#2-user-interface)
4. [API Consistency](#3-api-consistency)
5. [Performance](#4-performance)
6. [Consolidated Improvement Roadmap](#5-consolidated-improvement-roadmap)

---

## Scorecard

| Dimension                 | Rating     | Headline issue                                                        |
|---------------------------|------------|-----------------------------------------------------------------------|
| Layered Architecture      | ★★★★★ | Dependency direction is unidirectional; visibility controls precise   |
| Module Cohesion           | ★★★☆☆ | 3 files > 800 lines (builder, traversal, tests)                       |
| Extensibility             | ★★★★☆ | New step = 4 touch points, but no compile-time exhaustiveness guard   |
| User Interface            | ★★★★☆ | Doc examples are all `ignore`d — zero compile-time verification       |
| API Consistency           | ★★★★☆ | `RuntimeError` is massively overloaded; error taxonomy needs splitting |
| Performance               | ★★★★☆ | `SmolStr` + `SmallVec` are right choices; repeat body reset is the low-hanging fruit |
| Documentation             | ★★★★☆ | Module-level diagrams are excellent; trait-level examples thin         |

---

## 1. Architecture & Module Organization

### Current layered architecture (well designed)

```
┌──────────────────────────────────────────┐
│  api.rs           Graph / Session        │  ◄── pub: user entry
├──────────────────────────────────────────┤
│  gremlin/traversal.rs   DSL Builder      │  ◄── pub(crate): fluent API
│  gremlin/value.rs       User-facing types│
├──────────────────────────────────────────┤
│  planner/logical_step/  LogicalPlan IR   │  ◄── pub(crate): intermediate repr
│  planner/optimizer/     Optimization rules│
├──────────────────────────────────────────┤
│  engine/volcano/steps/  Physical ops      │  ◄── pub(crate): execution engine
│  engine/volcano/builder.rs  IR→physical   │
├──────────────────────────────────────────┤
│  graph.rs              OCC overlay       │  ◄── pub(crate): query isolation
├──────────────────────────────────────────┤
│  store/rocks/          RocksDB storage    │  ◄── pub(crate): persistence
│  types/                Core domain types  │
│  schema/               Label/prop registry│
└──────────────────────────────────────────┘
```

### What's good

1. **Unidirectional dependency**: `api → gremlin → planner → engine → graph → store`.
   Upper layers never import from lower layers in reverse.

2. **Visibility discipline** (`lib.rs`):
   ```rust
   pub mod api;                    // user-facing
   pub(crate) mod gremlin;         // internal
   pub(crate) mod planner;         // internal
   pub(crate) mod engine;          // internal
   ```
   Only the `api` and `schema` modules are fully public; everything else is crate-private.

3. **Module-level documentation**: `lib.rs`, `engine/mod.rs`, `types/mod.rs`, and `graph.rs`
   all carry ASCII architecture diagrams and clear responsibility descriptions.

4. **Test co-location**: Tests live in `#[cfg(test)] mod` blocks inside the modules they test,
   granting direct access to `pub(crate)` internals.

### Issues found

| # | Severity | Issue | Location |
|---|----------|-------|----------|
| A1 | **P1** | `builder.rs` is ~870 lines and serves three roles: data structures, step construction, and tests | `src/engine/volcano/builder.rs` |
| A2 | **P1** | `traversal.rs` is ~820 lines; holds 6 types + 2 traits in a single file | `src/gremlin/traversal.rs` |
| A3 | **P1** | `tests.rs` is ~2900 lines; every added step extends it further | `src/engine/volcano/steps/tests.rs` |
| A4 | **P1** | `build_step` uses `_ => unreachable!()` — new `LogicalStep` variants silently panic at runtime instead of failing at compile time | `src/engine/volcano/builder.rs:597` |
| A5 | **P1** | `LogicalStep::optimize()` uses a `_ => {}` wildcard that silently skips 20+ variants — the same compile-time blind-spot | `src/planner/logical_step/mod.rs` |
| A6 | **P2** | Optimizer registration allocates a `Vec` on every `apply_rules` call | `src/planner/mod.rs:36` |
| A7 | **P2** | `store/distributed/mod.rs` is an empty placeholder module | `src/store/distributed/mod.rs` |
| A8 | **P2** | `gremlin/conversions.rs` mixes predicate validation, type bridging, and `push_has_step` — a grab-bag name | `src/gremlin/conversions.rs` |

---

## 2. User Interface

### What's good

1. **Move semantics for safety**: Every step method takes `self` by value and returns `Self`.
   No hidden `&mut` aliasing; the chain is a pure sequence of moves.

2. **Three terminal methods cover all use-cases**:

   | Method      | Returns                        | Equivalent       |
   |-------------|--------------------------------|------------------|
   | `.next()`   | `Result<Option<Value>>`       | `tryNext()`      |
   | `.to_list()`| `Result<Vec<Value>>`          | `toList()`       |
   | `.iter()`   | `Result<BuiltTraversal>`      | iterate Traversal|

3. **`Key` type unifies input and output**:
   ```rust
   snap.g().V([]).has(Key::Id, 42i64)          // filter by id
   snap.g().V([42]).values([Key::Id])          // extract id
   ```

4. **`Predicate` constructors are ergonomic free functions**:
   ```rust
   .has("age", eq(30))           // explicit
   .has("age", 30i32)            // shorthand (From → Eq)
   .has("age", between(20, 40))
   .has("age", within([20, 30]))
   ```

### Issues found

| #  | Severity | Issue | Recommendation |
|----|----------|-------|----------------|
| U1 | **P1**  | All doc examples use ` ```ignore ` — zero compile-time verification of API examples | Promote at least `api.rs` and `value.rs` doc-tests to `no_run` or runnable with `tempfile` |
| U2 | **P1**  | `Graph` has no `close()` / `Drop`; tests `std::mem::forget` temp-dirs | Add `Graph::close()` or internal ref-counting to let RocksDB be torn down cleanly |
| U3 | **P2**  | `__()` naming (TinkerPop compatible) is alien to Rust conventions | Consider `anon()` or `traversal()` as an alias |
| U4 | **P2**  | Missing `.explain()` / `.profile()` introspection — blocked on TODO.md P3 | Low priority, but valuable for debugging complex repeat queries |

---

## 3. API Consistency

### What's good

1. **Three-tier filter separation**: `has(key, pred)` (universal), `hasLabel(...)`, `hasId(...)` — clear.

2. **`emit()` / `emit_if(cond)`**: Explicit method-per-mode mirrors the existing
   `union(...)` / `coalesce(...)` / `where(...)` pattern of accepting `GraphTraversal`
   sub-traversals.

3. **Consistent `Into<SmolStr>` and `IntoIterator` bounds** on step methods — flexible input
   without boilerplate.

### Issues found

| #  | Severity | Issue | Recommendation |
|----|----------|-------|----------------|
| C1 | **P0**  | `StoreError::RuntimeError(String)` carries 3 completely separate error classes: DSL validation, builder checks, and engine failures — impossible to match programmatically | Add `StoreError::TraversalError(String)` or similar variant to split traversal semantics from I/O/storage errors |
| C2 | **P1**  | Error message text for the same constraint differs between layers: `"times(0) is invalid"` (DSL) vs `"must have at least one stop condition"` (builder) vs `"Incomplete repeat()"` (terminal) | Template error messages from a single canonical source |
| C3 | **P2**  | Method naming `inV`/`outV`/`otherV` follows TinkerPop, but `in_to_vertex()` would be more self-documenting to Rust newcomers | Add doc aliases; keep TinkerPop names for compatibility |
| C4 | **P2**  | `emit()` + `emit_if(cond)` use the `_if` suffix pattern, while `has(key, pred)` uses overload resolution — two different patterns in the same API surface | Document the rationale; revisit if more `_if` methods are added |

---

## 4. Performance

### What's good

1. **`SmolStr` for labels and property keys**: Strings ≤ 22 bytes are stack-allocated,
   covering ~95% of real-world property-key sizes.

2. **`SmallVec<[_; 4]>` inline capacity**: Most steps produce ≤ 4 outputs per `produce()`
   call; heap allocation is avoided in the common case.

3. **`BufferedStep` single RefCell borrow**: One `borrow_mut()` call covers buffer-check
   + produce + pop — deliberately designed to avoid 4 separate borrows per `next()`.

4. **`Rc<Traverser>` clone cost**: Only reference-count increments, no deep copy of
   `GValue` — since `GValue::Vertex` and `GValue::Edge` carry integer keys, not data.

5. **`InOutStep` batch pagination**: `get_adjacent_edges` with `AdjacentEdgeCursor` avoids
   loading all edges at once — critical for high-degree vertices.

### Issues found

| #  | Severity | Issue | Location |
|----|----------|-------|----------|
| P1 | **P0**  | `RepeatStep` resets entire body pipeline on every frontier pop — O(breadth × pipeline_depth) per iteration | `src/engine/volcano/steps/repeat.rs:172` |
| P2 | **P1**  | `VecSourceStep::produce` does `self.items.drain(..).collect()` — allocates a new SmallVec; `std::mem::take` would be zero-cost | `src/engine/volcano/steps/vec_source.rs:61` |
| P3 | **P1**  | `apply_rules()` allocates a `Vec<OptimizerRule>` on every call — should be a `const` slice | `src/planner/mod.rs:36` |
| P4 | **P1**  | `materialize()` acquires schema `RwLock` per-result — for `to_list()` on 1000 results, that's 1000 atomic read-lock ops | `src/gremlin/traversal.rs:78` |
| P5 | **P2**  | `PhysicalPlan::reset()` recursively resets the full pipeline chain — fine for one-shot queries, but RepeatStep calls it O(breadth) times per source traverser | `src/engine/volcano/builder.rs` |
| P6 | **P2**  | `Rc<Traverser>` heap-allocates every traverser; for deep repeat queries this creates long linked lists | `src/engine/traverser.rs` |
| P7 | **P2**  | `GraphCtx` uses `&mut dyn` — vtable dispatch on every storage call (fine for embedded, but worth noting) | `src/engine/context.rs` |

---

## 5. Consolidated Improvement Roadmap

### 🔴 P0 — do first (correctness / safety)

| #  | What | Where | Effort |
|----|------|-------|--------|
| P0-1 | Add `StoreError::TraversalError(String)` — split DSL/builder/engine errors from I/O errors | `types/error.rs`, call-sites in `traversal.rs`, `builder.rs` | ~2h |
| P0-2 | Add `PhysicalPlan::refresh()` or make `RepeatStep` avoid full body reset on every frontier pop | `builder.rs`, `repeat.rs` | ~3h |

### 🟡 P1 — high impact (maintainability / performance)

| #  | What | Where | Effort |
|----|------|-------|--------|
| P1-1 | Split `builder.rs` (~870 lines) into `builder/mod.rs` + `builder/build_step.rs` | `engine/volcano/builder/` | ~2h |
| P1-2 | Split `traversal.rs` (~820 lines) into `gremlin/traits.rs`, `gremlin/read.rs`, `gremlin/write.rs` | `gremlin/` | ~2h |
| P1-3 | Split `tests.rs` (~2900 lines) into `tests/{repeat,edge,vertex,filter}_tests.rs` | `engine/volcano/steps/tests/` | ~1h |
| P1-4 | Change `VecSourceStep::produce` to use `std::mem::take` instead of `drain().collect()` | `vec_source.rs` | 5 min |
| P1-5 | Change optimizer list from `Vec` to `const &[OptimizerRule]` | `planner/mod.rs` | 5 min |
| P1-6 | Hoist schema lock out of `materialize()` loop | `gremlin/traversal.rs` | 1h |
| P1-7 | Promote core doc examples from `ignore` to `no_run` or runnable | `api.rs`, `value.rs` | 1h |
| P1-8 | Add `Graph::close()` or `Drop` implementation | `api.rs` | 1h |

### 🟢 P2 — nice-to-have (polish / future-proofing)

| #  | What | Where | Effort |
|----|------|-------|--------|
| P2-1 | Unify error message templates between DSL/builder/engine layers | multiple | ~1h |
| P2-2 | Add `#[doc(alias)]` for `inV`/`outV`/`otherV` → `in_vertex`/`out_vertex`/`other_vertex` | `gremlin/traversal.rs` | 10 min |
| P2-3 | Remove or populate `store/distributed/mod.rs` empty module | `store/distributed/` | 1 min |
| P2-4 | Rename `gremlin/conversions.rs` → `gremlin/has_step.rs` or split into focused sub-files | `gremlin/` | 30 min |
| P2-5 | Consider arena allocator for `Rc<Traverser>` in hot repeat loops | `engine/traverser.rs` | future |

---

## Proposed Refactoring Order

```
Phase 1 (correctness):  P0-1 → P0-2
Phase 2 (maintainability): P1-1 → P1-2 → P1-3 → P1-4 → P1-5
Phase 3 (performance):  P1-6
Phase 4 (user-facing):  P1-7 → P1-8
Phase 5 (polish):       P2-1 → P2-2 → P2-3 → P2-4
```
