# Design: `group()` / `groupCount()` — value aggregation modulators

Status: deferred — compatibility risks identified below; current narrow behavior
(option 1) stays as-is until each risk is investigated. Revisit options 2/3 only
after that investigation, per the project's standing rule: a feature with an
identified compatibility risk does not get implemented until the risk is
understood, not worked around in the same pass it was found.

## Problem

TinkerPop's `group()` takes two independent `by()` modulators: one for the map key,
one for the map value. The *value* modulator's shape decides the result type per key —
verified against `GroupStep.java`: the step calls `determineBarrierStep(valueTraversal)`
once at construction, checking whether the value traversal's last step is a `Barrier`
(`count()`, `sum()`, `mean()`, `fold()`, …).

- No barrier (e.g. `.by('name')`) → each element is appended via an implicit
  `fold()`-like accumulation → the value is a **`List`**, one entry per element in
  that key's bucket (so a bucket with exactly one element is still a one-element
  `List`, not a scalar — it's the traversal *shape*, not the runtime count, that
  decides this).
- Barrier present (e.g. `.by(count())`, `.by(sum())`) → every element mapped to that
  key is pushed cumulatively through that *same* barrier step → the value is the
  barrier's own reduced output (a scalar for `count()`/`sum()`, a `List` for
  `fold()` — but a single fold over the whole bucket, not a nested list).

```groovy
g.V().group().by(label).by('name')     // {"person": ["marko", "vadas", ...], ...}  — List per key
g.V().group().by(label).by(count())    // {"person": 4, "software": 2}              — scalar per key
```

RocksGraph's `group()` / `groupCount()` (`src/engine/volcano/steps/group.rs`,
`src/gremlin/traversal/mod.rs:709-717`) have **no `by()` modulator at all**, on
either side:

- `GroupStep::produce` (`group.rs:37-57`) groups incoming traverser values by
  themselves (no key-`by()` — the grouping key is always the traverser's current
  `GValue`) and unconditionally wraps every bucket in `GValue::List`, regardless of
  bucket size or shape. There is no barrier detection.
- `groupCount()` is a wholly separate, hardcoded step (`group.rs:66-110`) that
  always returns `GValue::Scalar(Primitive::Int64(count))` per key. It's effectively
  the one fixed case of TinkerPop's `group().by(...).by(count())`, implemented as
  its own top-level step rather than a generalization of `group()`.
- Both `LogicalStep::Group`/`LogicalStep::GroupCount` carry a `key: Option<SmolStr>`
  field (`src/planner/logical_step/mod.rs:433-443`), presumably intended for a
  key-`by()` at some point — but it's dead: the only call sites always pass `None`
  (`src/gremlin/traversal/mod.rs:709-717`), and the builder discards it entirely
  (`LogicalStep::Group(_) => …`, `src/engine/volcano/builder/build_step.rs:667-674`).

This is not a regressed or half-finished feature — `by()` modulators for `group()`
were never built. `docs/TODO.md` marks `group()`/`groupCount()` as done in exactly
this narrow form; no prior design doc covers the gap.

## Goals & non-goals

- **Goals:** record the verified TinkerPop baseline; document RocksGraph's current
  behavior precisely, including the dead `key` field; enumerate the options for
  closing (or intentionally not closing) the gap.
- **Non-goals:** decide an implementation. This is a decision-support document, not
  yet an implementation plan (same role as `design_vertex_label.md`).

## Compatibility risks (must be resolved before options 2/3)

Verified empirically (`__().group().by("name")` built and inspected directly) before
writing this section:

1. **`.by()` already has a meaning, and it's wrong for `group()`.** `.by()`
   (`src/gremlin/traversal/mod.rs`) is hardcoded to `OrderStep`: if the last pushed
   step isn't `Order`, it silently *inserts a new `order()` step* and applies the key
   there instead. Confirmed: `__().group().by("name")` compiled, before the mitigation
   below, into `[Group, Order{keys:[Property("name")]}]` — two steps, unrelated to
   grouping, not an error. Giving `group()` a real `by()` means either special-casing
   `Group`/`GroupCount` inside the existing `.by()` (a silent behavior change for that
   call shape) or using distinct method names (e.g. mirroring `order_by()` vs `by()`)
   to avoid the collision outright.

   **v0.1.0 mitigation (in place):** `.by()`/`.order_by()` now reject outright —
   `StoreError::TraversalError` pointing at this doc — when the immediately preceding
   step is `Group`/`GroupCount`, instead of falling through to the auto-insert
   sugar (`follows_group_step`/`by_after_group_error` in `traversal/mod.rs`; tests
   `test_builder_by_after_group_rejected`, `test_builder_by_after_group_count_rejected`,
   `test_builder_order_by_after_group_rejected` in `order_tests.rs`). This closes the
   silent-garbage footgun without deciding how `group().by()` should eventually work —
   risks 2 and 3 are still open, and the auto-insert sugar itself is untouched for
   every other step (still covered by `test_builder_by_without_order_auto_creates_order_step`).
2. **Arity/shape mismatch.** `order().by().by()...` is homogeneous and unbounded —
   each call appends another sort key. TinkerPop's `group().by().by()` is positional
   and capped at two: 1st call sets the key extractor, 2nd sets the value extractor;
   a 3rd `.by()` doesn't target `group()` at all in real Gremlin. None of `OrderStep`'s
   accumulate-or-replace logic carries over — this is a different state machine.
3. **Key type generality.** The existing `.by(impl Into<SmolStr>)` only accepts a
   property name. TinkerPop's `group().by(label)` / `.by(values('age').mean())` takes
   an arbitrary sub-traversal. Real parity needs a new overload shaped like
   `where_()`/`local()` (which already accept a `GraphTraversal`), not a variant of
   the string-typed `by()`.

Not a compatibility risk, but a principle tension to track once 1-3 are resolved:
option 3's `group().by(key).by(count())` would duplicate what `groupCount()` already
computes — exactly the "two access paths to the same data" pattern the reserved-key
disjoint model (`design_reserved_keys.md`) was written to close.

## Options going forward

1. **Leave as-is — current choice.** Two fixed, narrow steps: `group()` always
   groups-by-identity into `List`s, `groupCount()` always counts. Consistent with the
   "Strict syntax, narrowly scoped steps" principle in `design_principles.md` — no
   modulator to misuse, no barrier-detection logic to maintain. Cost: callers who want
   "group by label, sum weight" or "group by label, take first name" have no way to
   express it; they'd need `group()` followed by manual post-processing, or a new
   dedicated step per aggregation (mirroring how `rank()`/`hasRank()` got their own
   steps instead of overloading `values()`).
2. **Add a key-`by()` only.** Let `group(by: impl Into<SmolStr>)` (or a sub-traversal)
   pick the grouping key from a property/step result instead of the raw traverser
   value; keep the value side always-a-`List`. Closes the more commonly-needed half
   (`group().by("label")` is far more common than custom value reduction) without
   reintroducing barrier-detection complexity.
3. **Full TinkerPop parity.** Add both key- and value-`by()` with barrier detection
   on the value side, deprecating `groupCount()` in favor of `group().by(...).by(count())`.
   Closest to TinkerPop, but reopens exactly the "two access paths to the same
   result" question the reserved-key disjoint model (`design_reserved_keys.md`) was
   written to close — `groupCount()` and `group().by(count())` would need to either
   coexist deliberately or one would need to be removed.

## Open questions

1. How should the `.by()` naming collision (risk 1) be resolved — special-case
   `Group`/`GroupCount` inside the existing `.by()`, or introduce distinct method
   names? Must be answered before option 2 or 3 is implemented, not discovered
   mid-implementation.
2. Is a key-`by()` (option 2) worth doing before v0.1.0, or deferred like the other
   `P3` items in `docs/TODO.md` — once risks 1-3 are resolved?
3. If pursued, does the value side ever get barrier detection, or does RocksGraph
   deliberately keep `groupCount()`/future `groupSum()`-style steps as the only way
   to get a reduced (non-`List`) value per key — trading TinkerPop's generality for
   the "narrowly scoped steps" principle?
