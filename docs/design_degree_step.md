# Design: `get_degree()` on `GraphCtx` + `Degree` map step + cascading `degree_pushdown` optimizer

Status: implemented — all optimizer rules, physical step, and GraphCtx interface landed
and verified (692/692 lib tests, clippy/fmt clean).  Several additional bugs were caught
and fixed during implementation review; see **Post-implementation fixes** below.

## Problem

`g.V([id]).out().count()` performs a **full adjacency scan** only to count the results.
RocksGraph already maintains exact per-vertex degree counters in the `vertex_degree` CF
(`{ out_e_cnt: u32, in_e_cnt: u32 }`), kept current on every `add_edge`/`drop_edge`.
These counters are never used for query answering — only for drop-vertex integrity checks.

## Goals & non-goals

**Goals (Phase 1 — fully optimizable patterns only):**
- Add `GraphCtx::get_degree(key, direction)` — O(1) read from the `vertex_degree` CF overlay.
- Add `LogicalStep::Degree(DegreeStep)` — an **internal-only streaming map step**:
  one `GValue::Vertex` in → one `GValue::Scalar(Int64(degree))` out; error on other types.
- Three cascading optimizer rules:
  - **Rule A**: Convert `[edge-scan, Count]` to `[Degree, Sum]` when the edge step has
    **empty labels, no `end_vertex_ids`, no `rank`**, and `Count` is the immediately
    adjacent next step (no filter steps in between).
  - **Rule B**: In sub-plans that receive one traverser at a time (`local`, `where`), remove
    a `Sum` that immediately follows a `Degree`.
  - **Rule C**: `local([Degree])` — exactly one step — lifts `Degree` to the outer plan.

**Non-goals:**
- Label-filtered degree: `out(["knows"]).count()` — deferred to Phase 2.
- Property-filtered degree: `out([]).has("age", gt(25)).count()` — deferred to Phase 2.
- A user-visible `.degree()` traversal step — the rewrite is fully internal.

## What qualifies for Phase 1

An edge-step + Count pair qualifies **only** when all of the following hold:

| Condition | Reason |
|---|---|
| `labels.is_empty()` | `vertex_degree` CF has no per-label breakdown |
| `end_vertex_ids == None` | CF has no per-destination breakdown |
| `rank == None` (E-steps) | CF has no per-rank breakdown |
| `Count` is the immediately next step | Any intervening step (e.g. `has()`) changes the count |

Because filter-folding rules run before `degree_pushdown`, folded-in predicates are already
visible in the edge step's fields. For example, `out().hasId([1]).count()` becomes
`Out(end_vertex_ids=[1]), Count` before degree_pushdown runs — the guard `end_vertex_ids ==
None` correctly rejects it. Same for `out().hasLabel(["p"]).count()` → `Out(labels=["p"]),
Count` → rejected by `labels.is_empty()`.

## Design

### New enum: `DegreeDirection`

```rust
// src/types/keys.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DegreeDirection { Out, In, Both }
```

The existing `Direction` (`OUT` / `IN`) has no `Both` variant; `DegreeDirection` adds it for
the `both().count()` case.

### `GraphCtx::get_degree`

```rust
// src/engine/context.rs — new method in the GraphCtx trait
/// O(1) read from the vertex_degree overlay.
/// Returns 0 for a missing vertex — consistent with out([]).count() returning 0
/// for a non-existent source vertex.
fn get_degree(&mut self, key: VertexKey, direction: DegreeDirection) -> Result<u64, StoreError>;
```

`LogicalGraph<S>` implementation:

```rust
fn get_degree(&mut self, key: VertexKey, direction: DegreeDirection) -> Result<u64, StoreError> {
    // get_vertex_degree checks the in-memory overlay before the CF, so in-progress
    // writes within the same transaction are reflected — consistent with what a full
    // adjacency scan would count.
    let (out_cnt, in_cnt, _) = match self.get_vertex_degree(key)? {
        Some(t) => t,
        None    => return Ok(0),
    };
    Ok(match direction {
        DegreeDirection::Out  => out_cnt as u64,
        DegreeDirection::In   => in_cnt  as u64,
        DegreeDirection::Both => out_cnt as u64 + in_cnt as u64,
    })
}
```

`LogicalSnapshot<S>` is identical using its own `get_vertex_degree`.

### Logical step

```rust
// src/planner/logical_step/mod.rs
#[derive(Debug, Clone, PartialEq)]
pub struct DegreeStep {
    pub direction: DegreeDirection,
}

pub enum LogicalStep {
    // ... existing ...
    Degree(DegreeStep),   // ← new; between Count and HasLabel alphabetically
    // ...
}
```

`DegreeStep` is only produced by the optimizer rule — never by the traversal builder.
No `labels` field: Phase 1 only reaches this step when labels are empty.

### Physical step (`src/engine/volcano/steps/degree.rs`)

Streaming map. Always O(1) — reads from the `vertex_degree` CF overlay.

```rust
pub struct DegreeStep {
    upstream: Option<StepRef>,
    direction: DegreeDirection,
}

impl CoreStep for DegreeStep {
    fn produce(&mut self, ctx: &mut dyn GraphCtx)
        -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError>
    {
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let Some(t) = upstream.next(ctx)? else { return Ok(None) };

        let GValue::Vertex(vk) = &t.value else {
            return Err(StoreError::UnexpectedDataType(format!(
                "degree() expects a Vertex traverser, got {:?}", t.value
            )));
        };

        let degree = ctx.get_degree(*vk, self.direction)?;
        Ok(Some(smallvec![Traverser::new_rc(
            GValue::Scalar(Primitive::Int64(degree as i64))
        )]))
    }
    // reset / upper / add_upper / explain — standard boilerplate
}
```

No branching, no labels, no adjacency scan. Every `produce()` call is one overlay-HashMap
lookup (or one CF point read on cold overlay).

---

## Optimizer rules

All three rules live in `src/planner/optimizer/degree_pushdown.rs` and are registered in
`apply_rules` **after** all filter-folding rules.

The existing `Optimizer for LogicalStep` implementation already recurses into sub-plans for
`Local`, `Where`, and seven other wrapper steps, so the rules apply at every nesting depth
without extra wiring.

### Rule A — convert `[edge-scan, Count]` to `[Degree, Sum]`

**Scope:** top-level plan and all sub-plans (via existing recursion).

**Pattern:**

```
[Out | In | Both | OutE | InE | BothE] → [Count]
  where labels.is_empty()
    AND end_vertex_ids == None
    AND rank == None   (E-steps only)
```

**Replacement:**

```
[Degree(direction)] → [Sum]
```

`Sum` already exists (`LogicalStep::Sum`, physical `SumStep`). The rewrite produces two
steps: a streaming `Degree` map followed by a `Sum` barrier that aggregates all per-vertex
degree values.

**Why Sum, not Count?** `DegreeStep` emits one `Int64` value per vertex. `Sum` adds those
values (total degree across all input vertices). `Count` would count the number of values —
one per vertex regardless of degree, which is wrong.

**Direction mapping:**

| Edge step | DegreeDirection |
|---|---|
| `Out`, `OutE` | `Out` |
| `In`, `InE` | `In` |
| `Both`, `BothE` | `Both` |

### Rule B — remove `Sum` immediately after `Degree` in single-element contexts

**Scope:** sub-plans inside `local()`, `where()`, and any other container that feeds exactly
**one** traverser at a time to its sub-plan.

**Pattern:** any consecutive `[Degree, Sum]` pair anywhere inside such a sub-plan.

**Replacement:** `[Degree]` — remove `Sum`.

**Why correct:** In a single-element context, `Degree` emits exactly one `Int64`.
`sum([x]) = x`, so `Sum` is the identity and safe to remove. The rule applies regardless of
what follows `Sum`:

```
[Degree, Sum, ScalarFilter(gt(2))]  →  [Degree, ScalarFilter(gt(2))]
[Degree, Sum]                       →  [Degree]
```

**Containers that qualify for Rule B:**

| Container | Reason |
|---|---|
| `LocalStep` | feeds one element per outer traverser |
| `WhereStep` | feeds one element per outer traverser |

### Rule C — lift `Degree` out of `local()`

**Scope:** top-level plan, after Rule B has simplified sub-plans.

**Pattern:**
```
Local(LocalStep { plan: [Degree(d)] })    ← exactly one step
```

**Replacement:**
```
Degree(d)    ← directly in the outer plan
```

`local(f)` where `f` is a stateless one-in-one-out map equals `f` applied directly.
`DegreeStep` is stateless and does not affect path tracking, so the lift is safe.

### Cascade

The three rules compose via the fixed-point loop in `apply_rules`. A single pass may apply
multiple rules in sequence:

**`local(out([]).count())`** — the common per-vertex degree pattern:

```
initial:    Local([Out(labels=[]), Count])
Rule A:     Local([Degree(Out), Sum])        ← A on sub-plan
Rule B:     Local([Degree(Out)])             ← B: Sum after Degree in single-element context
Rule C:     Degree(Out)                      ← C: lift lone Degree out of local
result:     Degree(Out)                      ← O(1) per vertex, no Sum needed
```

**`g.V([1,2,3]).out([]).count()`** — aggregate across multiple vertices:

```
initial:    [Out(labels=[]), Count]
Rule A:     [Degree(Out), Sum]               ← A at top level
            (Rule B does NOT apply here — top-level plan is not a single-element context)
result:     [Degree(Out), Sum]               ← O(1) per vertex, Sum aggregates across three
```

**`where(__.out([]).count().is(gt(2)))`** — filter using degree:

```
initial:    Where([Out([]), Count, ScalarFilter(gt(2))])
Rule A:     Where([Degree(Out), Sum, ScalarFilter(gt(2))])
Rule B:     Where([Degree(Out), ScalarFilter(gt(2))])    ← Sum removed, ScalarFilter stays
result:     Where([Degree(Out), ScalarFilter(gt(2))])    ← O(1) per vertex inside where
```

**`local(out(["knows"]).count())`** — NOT rewritten (Phase 1 scope):

```
initial:    Local([Out(labels=["knows"]), Count])
Rule A:     no match — labels non-empty → guard fails
result:     Local([Out(labels=["knows"]), Count])    ← unchanged, full adjacency scan
```

### Implementation sketch

```rust
// src/planner/optimizer/degree_pushdown.rs

pub(crate) fn degree_pushdown(plan: &mut LogicalPlan) -> bool {
    let mut changed = false;

    // ── Rule A: [edge-scan, Count] → [Degree, Sum] in this plan ─────────────
    changed |= apply_rule_a(plan);

    // ── Rules B + C applied to wrapper sub-plans ──────────────────────────────
    for step in &mut plan.steps {
        match step {
            LogicalStep::Local(local) => {
                changed |= degree_pushdown(&mut local.plan); // recurse first
                changed |= apply_rule_b(&mut local.plan);   // then B
                // Rule C: if sub-plan is exactly [Degree], lift it out.
                if let [LogicalStep::Degree(ds)] = local.plan.steps.as_slice() {
                    let ds = ds.clone();
                    *step = LogicalStep::Degree(ds);
                    changed = true;
                }
            }
            LogicalStep::Where(wh) => {
                changed |= degree_pushdown(&mut wh.plan);
                changed |= apply_rule_b(&mut wh.plan);
                // where() keeps its wrapper; only Sum inside is removed.
            }
            // Other wrappers (Union, Repeat, …) recurse via the existing
            // Optimizer trait implementation — degree_pushdown is registered
            // as an OptimizerRule, so it gets called for all sub-plans automatically.
            _ => {}
        }
    }
    changed
}

fn apply_rule_a(plan: &mut LogicalPlan) -> bool {
    let mut i = 0;
    let mut changed = false;
    while i + 1 < plan.steps.len() {
        if let Some(dir) = unfiltered_edge_scan_dir(&plan.steps[i]) {
            if matches!(plan.steps[i + 1], LogicalStep::Count(_)) {
                plan.steps.splice(
                    i..=i + 1,
                    [
                        LogicalStep::Degree(DegreeStep { direction: dir }),
                        LogicalStep::Sum(SumStep {}),
                    ],
                );
                changed = true;
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    changed
}

fn apply_rule_b(plan: &mut LogicalPlan) -> bool {
    let mut i = 0;
    let mut changed = false;
    while i + 1 < plan.steps.len() {
        if matches!(plan.steps[i],     LogicalStep::Degree(_))
        && matches!(plan.steps[i + 1], LogicalStep::Sum(_))
        {
            plan.steps.remove(i + 1); // remove Sum
            changed = true;
            // stay at i: re-examine steps[i] and new steps[i+1]
        } else {
            i += 1;
        }
    }
    changed
}

/// Returns Some(direction) iff the step is a fully-unfiltered edge scan.
/// Phase 1: labels must be empty, end_vertex_ids must be None, rank must be None.
fn unfiltered_edge_scan_dir(step: &LogicalStep) -> Option<DegreeDirection> {
    match step {
        LogicalStep::Out(s)
            if s.labels.is_empty() && s.end_vertex_ids.is_none()
            => Some(DegreeDirection::Out),
        LogicalStep::OutE(s)
            if s.labels.is_empty() && s.end_vertex_ids.is_none() && s.rank.is_none()
            => Some(DegreeDirection::Out),
        LogicalStep::In(s)
            if s.labels.is_empty() && s.end_vertex_ids.is_none()
            => Some(DegreeDirection::In),
        LogicalStep::InE(s)
            if s.labels.is_empty() && s.end_vertex_ids.is_none() && s.rank.is_none()
            => Some(DegreeDirection::In),
        LogicalStep::Both(s)
            if s.labels.is_empty() && s.end_vertex_ids.is_none()
            => Some(DegreeDirection::Both),
        LogicalStep::BothE(s)
            if s.labels.is_empty() && s.end_vertex_ids.is_none() && s.rank.is_none()
            => Some(DegreeDirection::Both),
        _ => None,
    }
}
```

### Position in `apply_rules`

```rust
// src/planner/mod.rs — after all filter-folding rules
apply_rule(plan, degree_pushdown::degree_pushdown);
```

---

## Files changed

| File | Change |
|---|---|
| `src/types/keys.rs` | Add `DegreeDirection` enum |
| `src/engine/context.rs` | Add `get_degree` to trait; `NoopCtx` returns `Err` |
| `src/graph/logical.rs` | Implement `get_degree` via `get_vertex_degree` |
| `src/graph/snapshot.rs` | Implement `get_degree` via snapshot `get_vertex_degree` |
| `src/planner/logical_step/mod.rs` | `DegreeStep { direction }` struct + `LogicalStep::Degree` |
| `src/engine/volcano/steps/degree.rs` | New streaming map physical step |
| `src/engine/volcano/steps/mod.rs` | Register `degree` module |
| `src/engine/volcano/builder/build_step.rs` | Build arm for `LogicalStep::Degree` |
| `src/planner/optimizer/degree_pushdown.rs` | Rules A, B, C + recursion |
| `src/planner/optimizer/mod.rs` | Register module |
| `src/planner/mod.rs` | Add to `apply_rules` after filter-folding rules |
| `src/engine/volcano/steps/numeric_reducers.rs` | Verify `SumStep` handles `GValue::Scalar(Int64)` |

## Implementation plan

- [ ] **Step 1** — `DegreeDirection` + `GraphCtx::get_degree`: implement and unit-test.
- [ ] **Step 2** — Logical `DegreeStep { direction }`: add struct and enum variant.
- [ ] **Step 3** — Physical `DegreeStep`: implement; unit-test both Vertex and error cases.
- [ ] **Step 4** — Verify `SumStep` handles `GValue::Scalar(Int64)` traversers; extend if needed.
- [ ] **Step 5** — Rule A only: unit-test the rewrite in isolation.
- [ ] **Step 6** — Rules B + C: unit-test the cascade.
- [ ] **Step 7** — E2E tests + `just full-check`.

## Test plan

### Optimizer unit tests

| Input plan | Expected output | Rule(s) |
|---|---|---|
| `[Out([]), Count]` | `[Degree(Out), Sum]` | A |
| `[OutE(labels=[], rank=None), Count]` | `[Degree(Out), Sum]` | A |
| `[In([]), Count]` | `[Degree(In), Sum]` | A |
| `[Both([]), Count]` | `[Degree(Both), Sum]` | A |
| `[Out(labels=["knows"]), Count]` | unchanged | A guard: labels non-empty |
| `[Out(dst=Some([1])), Count]` | unchanged | A guard: dst set |
| `[OutE(rank=Some(0)), Count]` | unchanged | A guard: rank set |
| `[Out([]), Has(…), Count]` | unchanged | A guard: Count not adjacent |
| `Local([Out([]), Count])` | `Local([Degree(Out), Sum])` | A on sub-plan |
| `Local([Degree(Out), Sum])` | `Local([Degree(Out)])` | B |
| `Local([Degree(Out), Sum, ScalarFilter(gt(2))])` | `Local([Degree(Out), ScalarFilter(gt(2))])` | B mid-plan |
| `Local([Degree(Out)])` | `Degree(Out)` | C |
| `Local([Out([]), Count])` (full cascade) | `Degree(Out)` | A + B + C |
| `Where([Out([]), Count])` | `Where([Degree(Out)])` | A + B (no C for where) |
| `Where([Out([]), Count, ScalarFilter(gt(2))])` | `Where([Degree(Out), ScalarFilter(gt(2))])` | A + B |
| `[Out([]), Count, Out([]), Count]` | `[Degree(Out), Sum, Degree(Out), Sum]` | A twice |
| `Local([Out(["knows"]), Count])` | unchanged | A guard inside sub-plan |

### Physical `DegreeStep` unit tests

| Scenario | Expected |
|---|---|
| `GValue::Vertex(vk)`, out-degree 3 | emits `Scalar(Int64(3))` |
| `GValue::Vertex(vk)`, missing vertex | emits `Scalar(Int64(0))` |
| `GValue::Edge(…)` upstream | `UnexpectedDataType` error |
| Upstream exhausted | `Ok(None)` |
| `reset()` then second pull | works correctly |

### End-to-end (`src/gremlin/tests.rs`, modern graph)

| Query | Expected | Notes |
|---|---|---|
| `g.V([1]).out([]).count()` | 3 | Rule A → Degree(Out) + Sum |
| `g.V([1]).in_([]).count()` | 0 | Rule A → Degree(In) + Sum |
| `g.V([1]).both([]).count()` | 3 | Rule A → Degree(Both) + Sum |
| `g.V([1]).outE([]).count()` | 3 | OutE variant |
| `g.V([1,2,3]).out([]).count()` | sum of three out-degrees | Sum aggregates |
| `g.V([]).out([]).count()` | sum of all out-degrees | full-scan vertex set |
| `g.V([1,2]).local(__.out([]).count())` | `[3, 0]` two separate Int64 values | A+B+C → Degree map |
| `g.V([]).where(__.out([]).count().is(gt(0)))` | vertices with any out-edges | A+B in where sub-plan |
| `g.V([1]).out(["knows"]).count()` | 2 | NOT rewritten — full adjacency scan |
| `g.V([1]).out([]).has("age", gt(25)).count()` | N | NOT rewritten — has() between out and count |

Verify via `explain()` that `DegreeStep` appears in optimized plans and `InOutStep`/
`BothStep` in the non-rewrite cases.

## Constraints / invariants

1. **DegreeStep is a pure O(1) map** — always reads from `vertex_degree` CF via overlay.
   No adjacency scan, no property lookup, no edge materialization.
2. **Rule A guard is strict** — all three filter fields must be absent (`labels` empty,
   `end_vertex_ids` None, `rank` None) AND `Count` must be the immediately next step.
3. **Rule B fires only in single-element contexts** — `apply_rule_b` must only be called
   on sub-plans inside `local`/`where` (or other per-element containers), never on the
   top-level plan.
4. **Rule C requires exactly one step** — `local([Degree])` with plan length 1 only.
5. **SumStep must handle `GValue::Scalar(Int64)` traversers** — confirm before Step 4.
6. **Run after filter-folding rules** — ensures folded predicates are visible in the edge
   step's fields when Rule A's guard runs.

## Out of scope (Phase 2)

- `out(["knows"]).count()` → label-filtered degree. Would require per-label degree counters
  in the `vertex_degree` CF, or a separate DegreeStep slow-path that does a label-prefix
  adjacency scan without emitting traversers.
- `out([]).has("prop", val).count()` → property-filtered degree. Fundamentally requires a
  property lookup per edge; no CF shortcut exists.
- User-visible `.degree()` traversal step.

## Post-implementation fixes

Four bugs were found and fixed during the implementation review, none of which were
anticipated in the original design:

### 1. `DegreeStep` missing `track_path` and parent threading
`DegreeStep::produce()` called `Traverser::new_rc(...)` (no parent chain) instead of
`Traverser::new_rc_conditional(value, &t, track_path)`.  Without the upstream vertex
traverser threaded as parent, `path()` after `local(out([]).count())` would produce
`[Int64(degree)]` instead of `[Vertex, Int64(degree)]`, silently breaking any query that
combined degree counting with path reconstruction.  Fix: added `track_path: bool` to
`DegreeStep`; build arm now passes the outer `track_path` flag.

### 2. `LocalStep` missing parent re-threading for barrier sub-plans
`LocalStep::produce()` pushed sub-plan results to the buffer without re-threading the
outer traverser `t` as parent.  For **streaming** sub-plans (`outE().otherV()`), this was
correct — those steps already thread parents correctly, so passing results through preserves
the full chain `[outer → edge → vertex]`.  For **barrier** sub-plans (`out().count()`),
`CountStep` uses `new_rc` (no parent), so `path()` would see only `[Int64(count)]` and
lose the outer vertex.  Fix: when `track_path` is true AND `result.parent.is_none()` (the
barrier discriminator), re-wrap the result with `t` as parent.  The `result.parent.is_none()`
check is the correct discriminator because the sub-plan is built with the same `track_path`
as the outer plan — streaming steps produce `parent = Some(...)`, barrier steps produce
`parent = None`, regardless of track_path.

### 3. `LogicalGraph::get_degree` missing `cache_vertex_label`
The `vertex_degree` CF record already carries `vertex_label_id`.  The initial implementation
discarded it with `let (out_cnt, in_cnt, _)`.  Fixed to call `cache_vertex_label(key, label_id)`
for free — matching the same optimization already present in `get_adjacent_edges`.

### 4. Self-loop degree undercount in `LogicalGraph::add_edge`
When `src_id == dst_id`, the original code performed two independent reads of
`get_vertex_degree(id)` before either insert.  Both reads returned the same pre-increment
value; the second insert silently overwrote the first's `out_e_cnt` increment.  Every vertex
with a self-loop had `out_e_cnt` undercounted by exactly 1, producing the diagnostic pattern
`stored(CF)=N scanned=N+1`.  Fix: when `src_id == dst_id`, read once, increment `out_cnt`
and `in_cnt` atomically, write once.  Regression test: `graph::tests::test_self_loop_degree_correct`.
