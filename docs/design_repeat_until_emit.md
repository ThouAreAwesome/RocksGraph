# Design: `repeat()` / `until()` / `emit()`

## Goals & non-goals

- **Goals:** Implement `repeat()` with `until()` termination and `emit()` for intermediate results; track path history within repeat loops for cycle detection; support `times(n)` shorthand.
- **Non-goals:** Nested `repeat()` inside `repeat()`; `emit()` with a sub-traversal condition in the initial implementation; full Gremlin `loops()` step.

## 1. Problem Statement

`docs/TODO.md` lists `repeat()`/`until()`/`emit()` as the top P0 gap in step coverage: without
variable-length looping, nothing beyond a fixed-depth chain of `.out()` calls is expressible —
no N-hop neighbors, reachability, or recursive-path queries.

This adds them as a single compound looping construct, following the patterns already
established for `where()`/`union()`/`coalesce()`: sub-traversals built via `__()`, one
`LogicalStep` variant, one physical Volcano step.

---

## 2. Departures from TinkerPop

RocksGraph is "Gremlin-inspired, not Gremlin-compatible" (`design_principles.md`). Two
deliberate departures here, chosen for safety and simplicity over full fidelity:

1. **Post-check only.** `.until()`/`.emit()` are only valid immediately after `.repeat()` in the
   chain — always do-while semantics (body runs at least once). TinkerPop's call-order-flips-
   semantics behavior (`until().repeat()` = pre-check vs `repeat().until()` = post-check) is a
   well-known footgun and is not reproduced.
2. **Mandatory bound.** Building a `repeat()` with neither `.times()` nor `.until()` configured
   is a build-time `StoreError`, not a silent infinite loop. This engine is synchronous,
   single-threaded, and embedded with no query timeout — an unbounded `repeat()` on a cyclic
   graph would hang the calling thread forever.

---

## 3. Semantics

- `repeat(body)` — loop `body` (a `__()` sub-traversal) over each incoming traverser.
- `.times(n)` — cap iterations at `n` (`n == 0` is a build error — a repeat that never runs its
  body isn't a meaningful case worth supporting).
- `.until(cond)` — boolean break test (same "does this sub-traversal yield ≥1 result" convention
  already used by `where()`), checked **after** each iteration. `times` and `until` combine
  with OR: the loop stops when either fires.
- `.emit()` — also emit every intermediate (non-final) traverser, not just the ones that
  satisfied the stop condition.
- `.emit_if(cond)` — like `.emit()` but only emits intermediates where `cond` matches. A
  separate method name rather than overloading `emit()`/`emit(cond)`, since Rust has no clean
  arity-based overload here — matches the existing style of small, explicit methods.
- Traversers that satisfy the stop condition are **always** emitted (that's how they exit the
  loop) — independent of `emit`/`emit_if`, which only governs *intermediate* output.
- Path tracking (`Traverser::parent`) is unconditional throughout the engine already (every
  value-changing step calls `new_rc_with_parent`), so `.repeat(...).path()` works for free — no
  special-casing needed.
- Iteration order: one input traverser's entire reachability tree is resolved breadth-first
  (FIFO frontier) before the next input traverser from upstream is pulled. No stronger ordering
  guarantee is promised (consistent with `union`/`coalesce`, which don't promise one either).

---

## 4. Detailed Design

### 4.1 Logical IR (`src/planner/logical_step/mod.rs`)

```rust
pub enum EmitSpec {
    Never,
    Always,
    If(LogicalPlan),
}

pub struct RepeatStep {
    pub body: LogicalPlan,
    pub until: Option<LogicalPlan>,
    pub times: Option<i64>,
    pub emit: EmitSpec,
}
```

`LogicalStep::Repeat(RepeatStep)` is added to the step enum. `impl Optimizer for RepeatStep`
recurses `optimizer_rule` into `body`, `until` (if `Some`), and the plan inside `EmitSpec::If`
(if present) — the same shape as `impl Optimizer for UnionStep`. No new optimizer *rule* is
needed: `apply_rules` already walks every step's sub-plans via `LogicalStep::optimize`, so
existing rules (e.g. index-seek folding) apply correctly inside the repeat body/until/emit
sub-plans automatically, same as they already do inside `where`/`union`/`coalesce`.

### 4.2 Physical step (`src/engine/volcano/steps/repeat.rs`)

```rust
pub enum PhysicalEmitMode {
    Never,
    Always,
    If(PhysicalPlan),
}

pub struct RepeatStep {
    upstream: Option<StepRef>,
    body: PhysicalPlan,
    until: Option<PhysicalPlan>,
    times: Option<i64>,
    emit: PhysicalEmitMode,

    frontier: VecDeque<(Rc<Traverser>, i64)>, // (traverser, iterations completed so far)
    ready: VecDeque<Rc<Traverser>>,           // outputs queued for this produce() call
    body_active: bool,
    current_iter_count: i64,
}
```

`produce()` reuses the reset/inject/next protocol already used by `where.rs`/`union.rs`/
`coalesce.rs`:

```
loop {
    if ready has an item -> return it

    if body_active:
        match body.next(ctx)? {
            Some(out) => {
                iter_count = current_iter_count + 1
                if is_done(iter_count, &out, ctx)?:       // times reached OR until matches
                    ready.push_back(out)
                else:
                    if should_emit(&out, ctx)?:           // emit policy: Never/Always/If
                        ready.push_back(clone of out)
                    frontier.push_back((out, iter_count))
                continue
            }
            None => body_active = false
        }

    if let Some((t, count)) = frontier.pop_front():
        current_iter_count = count; body.reset(); body.inject([t]); body_active = true; continue

    let t = upstream.next(ctx)?  (or return None if exhausted)
    current_iter_count = 0; body.reset(); body.inject([t]); body_active = true
}
```

`is_done` checks `times` first (`iter_count >= times`), then `until` via a small private helper
`sub_plan_matches(plan, t, ctx)` (reset + inject + `next().is_some()` — the same three lines
`where.rs` already uses, reused twice in this file: once for `until`, once for
`PhysicalEmitMode::If`).

`reset()` resets `upstream`, `body`, `until` (if any), the `If` plan inside `emit` (if any), and
clears `frontier`/`ready`/`body_active`/`current_iter_count`.

`build_step` in `engine/volcano/builder/build_step.rs` gets a new arm for `LogicalStep::Repeat`: a
defensive check that `until`/`times` isn't both-absent (the DSL layer already prevents this,
but `build_step` is also reachable from hand-built `LogicalPlan`s in tests, same as `WhereStep`'s
empty-sub-plan check), then recursively builds `body`/`until`/the `If` plan the same way the
`Union`/`Coalesce` arms do.

### 4.3 DSL layer (`src/gremlin/traversal/mod.rs`)

This is where "repeat + until + emit is really one compound construct" gets resolved, via a
**pending builder flushed on the next step** — the same shape `error: Option<StoreError>`
already uses across `GraphTraversal`/`ReadTraversal`/`WriteTraversal`.

```rust
struct RepeatBuilder {
    body: LogicalPlan,
    until: Option<LogicalPlan>,
    times: Option<i64>,
    emit: EmitSpec,
}
```

- `PlanAppender` gains `pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder>` and a
  default method `flush_pending_repeat(&mut self)`: takes the pending builder, validates
  `until.is_some() || times.is_some()` (else records an error and drops it — it never reaches
  the plan), and otherwise pushes `LogicalStep::Repeat(...)` directly onto `self.plan_mut().steps`.
- `push_step`'s default body calls `flush_pending_repeat()` first, then pushes as today — this
  is what makes "any other step after `.until()`/`.emit()`" correctly finalize the loop.
- `GraphTraversal`/`ReadTraversal`/`WriteTraversal` each add a `pending_repeat: Option<RepeatBuilder>`
  field (`None` initially); `Clone for GraphTraversal` resets it to `None`, mirroring `error`.
- New `TraversalBuilder` methods (available on all three types via the existing blanket impl):
  - `repeat(body)` — flushes any already-pending builder first (handles back-to-back
    `.repeat(a).repeat(b)`), merges `body`'s recorded error, installs a fresh `RepeatBuilder`.
  - `times(n)` — requires a pending builder; errors if `n == 0`.
  - `until(cond)` — requires a pending builder; merges `cond`'s recorded error.
  - `emit()` — requires a pending builder; sets `EmitSpec::Always`.
  - `emit_if(cond)` — requires a pending builder; sets `EmitSpec::If(cond)`.
- `GraphTraversal::into_plan()`/`build()` and `ReadTraversal::iter()`/`WriteTraversal::iter()`
  call `flush_pending_repeat()` before consuming `self.plan`, so a trailing
  `.repeat(...).until(...)` at the very end of a chain still gets finalized.

---

## Constraints / invariants

- `RepeatStep` must reset its body and until sub-plans on every loop iteration.
- Path tracking inside repeat loops uses the same `Rc<Traverser>` parent chain as `path()`/`simplePath()`.
- `emit()` with `If` must evaluate the emit sub-traversal independently from the until check.

## 5. Testing Strategy

- **Physical step** (`engine/volcano/steps/tests.rs`, hand-built `LogicalPlan`s against the
  TinkerPop "modern graph" fixture): N-hop neighbors via `.times(n)`; reachability short-circuit
  via `.until(...)`; `.emit()`/`.emit_if(...)` intermediate output; `repeat(...).path()` capturing
  every hop; a cycle in the fixture graph + `.times(n)` terminating instead of hanging (the core
  motivating safety property); a `debug_print` wiring test mirroring `test_print_union_and_coalesce`.
- **DSL / error paths** (`gremlin/tests.rs`, full `Graph`/`TxSession` round trip): `repeat()`
  alone with no bound → `Err`; `times(0)` → `Err`; `until`/`emit`/`emit_if` without a preceding
  `repeat()` → `Err`; back-to-back `repeat(a).repeat(b)`; an end-to-end N-hop query through the
  public fluent API.
