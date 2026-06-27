# Design: filter reordering & edge-step folding — a single ranked pass instead of pairwise rules

Status: implemented. Problems 1-3 are done and verified (629/629 lib tests, clippy/fmt clean).
Problem 3's overwrite-on-fold bug turned out to also affect `rank` (Problem 3b) and
`label_pred` (Problem 3c) — both closed by a follow-up audit of every overwrite-style
assignment in the optimizer, prompted directly by the question "is the rank merge safe, and
are there other places like it?" Problem 3c's first fix attempt (bail out of extraction
entirely) was itself caught on review as the wrong shape and replaced — see Problem 3c.

## Problem

RocksGraph's optimizer turns `hasLabel()`/`hasId()`/`hasRank()`/`has()`/`where(otherV()…)`
into structural fields on a preceding `outE()`/`inE()`/`bothE()`/`out()`/`in()`/`both()`
step (`end_vertex_ids`, `rank`, `labels`) wherever it can, and the physical builder turns a
fully-pinned edge step into a `GetEStep` point lookup instead of an `InOutStep`/`BothStep`
adjacency scan. All three pieces already exist and are exercised by hundreds of tests:

| Rule (`src/planner/optimizer/`) | Job |
|---|---|
| `reorder_filter::reorder_filters` | Swaps adjacent filter-ish steps so fold-eligible ones end up touching their anchor |
| `extract_end_vertex_filter` | Turns `where(otherV().hasId(X))` / `where(otherV().has("id", X))` into a flat `EndVertexFilter` step |
| `merge_end_vertex_filter` | Folds `EndVertexFilter`/`HasId`/`HasProperty("id")`/`HasRank` into a preceding `OutE`/`InE`/`BothE`/`Out`/`In`/`Both` |
| `merge_haslabel_into_edge` | Folds `HasLabel`/`HasProperty("label")` into a preceding `OutE`/`InE`/`BothE` (only when its `labels` allowlist is still empty) |

`apply_rules` (`src/planner/mod.rs:39-60`) runs these to a fixed point:

```
reorder_filter::reorder_filters,
merge_v_id_filter::merge_v_id_filter,
merge_addv_id::merge_addv_id,
merge_adde_ids::merge_adde_from,
merge_adde_ids::reorder_rank_forward,
merge_adde_ids::merge_adde_rank,
extract_end_vertex_filter::extract_end_vertex_filter,
merge_end_vertex_filter::merge_end_vertex_filter,
merge_haslabel_into_edge::merge_haslabel_into_edge,
```

And the physical builder (`build_step.rs`'s `get_e_or_scan!` macro, ~line 177-198) emits
`GetEStep` instead of a scan once `end_vertex_ids` is `Some`, `label_ids` is non-empty, and
the rank is either known or the label is in `EdgeMode::Single` — this part is complete and
not the focus of this document.

### Problem 1 — the reorder rules are an O(k²) hand-written pairwise matrix

`reorder_filters` (`src/planner/optimizer/reorder_filter.rs`) is, by its own doc comment, a
single total order:

```
hasId = has("id"..) > hasLabel > EndVertexFilter > hasRank > has(not id..) > where()
```

But it's implemented as **18 separately hand-written adjacent-pair match arms** — one per
ordered pair across 6 kinds — because the function only ever compares `plan.steps[i]` against
`plan.steps[i+1]` and swaps if that specific pair is listed. Adding `HasRank` and
`EndVertexFilter` to the order (the change currently in flight, per the working-tree diff
against this file) took 10 new arms. A 7th kind would need up to 6 more. Nothing checks that
every one of the `C(k,2)` pairs is covered in the right direction — correctness rests on the
author enumerating all of them by hand, and the existing test suite only spot-checks specific
example pairs, not the full cross-product.

This is solvable in general: a total order over a fixed set of kinds is exactly what a
**priority ranking function** expresses in O(k) space, with a single generic comparator
doing the O(k²) work the compiler/runtime is good at, instead of a human enumerating it.

### Problem 2 — `extract_end_vertex_filter` only recognizes an exact 2-step sub-plan

`extract_end_vertex_filter` (`src/planner/optimizer/extract_end_vertex_filter.rs:29-46`) only
matches `wh.plan.steps` when it is *exactly* `[OtherV, HasId]` or `[OtherV, HasProperty(id)]`.
A sub-plan with anything else appended — e.g. `where(otherV().hasId(2).has("age", gt(30)))` —
matches neither arm and extracts nothing at all (`test_where_with_extra_steps_not_extracted`
asserts exactly this for the `hasLabel` case).

This contradicts `docs/design_explain_step.md`'s own example table (lines 279, 325), which
claims this exact shape partially extracts: the id part folds into `GetEStep`, and the
non-id part "stays as `EndVertexFilterStep`". That's not just unimplemented — it isn't
representable today: `EndVertexFilter` (`logical_step/mod.rs:516-518`) is `{ ids:
SmallVec<[VertexKey; 4]> }`, an id-allowlist only, with nowhere to put a leftover property
filter. The doc describes a capability that doesn't exist and a struct shape that couldn't
hold it; it needs correcting regardless of which way the design below goes.

### Problem 3 — confirmed bug: a second foldable end-vertex filter silently overwrites the first

Verified directly (scratch test against the current `merge_end_vertex_filter`, removed after
confirming): `OutE().EndVertexFilter([1,2]).EndVertexFilter([3])` folds to a *single* `OutE`
with `end_vertex_ids = Some([3])` — ids `1` and `2` are silently dropped, not intersected.
The merge arm is a plain assignment, `oute.end_vertex_ids = Some(idv)`, with no awareness
that a value might already be there from a prior fold in the same `i`/`j` walk.

This is reachable today, independent of anything else in this document — two separate
`where(otherV().hasId(…))` clauses on the same `outE()`/`bothE()`/`inE()` (chained filters are
AND semantics, same as two `.has()` calls in a row) silently keep only the *last* clause's
ids instead of intersecting them. `V([1]).outE("knows").where(otherV().hasId([1,2])).where(otherV().hasId([3]))`
should mean "the destination is in `{1,2}` *and* in `{3}`" — i.e. impossible, matches nothing
— but folds today into `GetEStep(end_vertex_ids=[3])`, which would actually return the edge to
vertex 3 if one exists. This needs fixing regardless of whether Problem 2 / the generalization
below ships, and the fix below (switching `ids` to `Option<…>` and intersecting on fold)
closes it as a side effect.

### Problem 3b — the same overwrite bug, found in `rank` after Problem 3 shipped

Once `ids` was fixed (intersect instead of overwrite), the identical bug was found — by
direct request, then confirmed by reproduction — in the *other* field this same function
merges: `oute.rank = Some(r)` / `ine.rank = Some(r)` / `bothe.rank = Some(r)` were still plain
overwrites, no guard. Reproduced directly: `OutE().HasRank(1).HasRank(2)` folded to
`rank=Some(2)`, silently losing the `rank=1` constraint — `outE().hasRank(1).hasRank(2)` means
"rank is 1 *and* rank is 2," impossible for a single edge, and should match nothing; instead
it silently became `rank==2`, which would wrongly match a real rank-2 edge.

`rank` can't reuse the `ids` fix's *shape* (`Option<SmallVec>` has a natural "empty"
representation for "matches nothing"; `rank: Option<Rank>` is a bare scalar with no such
representation). Fixed instead by reusing a *pattern* already established twice elsewhere in
this same codebase — `merge_haslabel_into_edge`'s `out_e.labels.is_empty()` guard and
`merge_v_id_filter`'s `v.ids.is_empty()` guard: add `rank.is_none()` to each match arm's
guard, so only the *first* `hasRank()` folds and a second is left unfolded rather than
overwriting:

```rust
(LogicalStep::OutE(oute), LogicalStep::HasRank(hr)) if oute.rank.is_none() => { ... }
(LogicalStep::InE(ine), LogicalStep::HasRank(hr)) if ine.rank.is_none() => { ... }
(LogicalStep::BothE(bothe), LogicalStep::HasRank(hr)) if bothe.rank.is_none() => { ... }
```

The unfolded second `HasRank` still gets evaluated downstream — and correctly produces "matches
nothing" for a genuine conflict, since the edge step now only ever emits rank-1 edges, and a
residual `HasRank(Eq(2))` check against an always-rank-1 stream is always false. A *redundant*
`hasRank(1).hasRank(1)` also still works (no correctness issue, just slightly less fused).

**Audit of every other overwrite-style assignment in the optimizer**, prompted by the same
question ("are there other places with this risk?"):

| Location | Field | Safe? | Why |
|---|---|---|---|
| `merge_v_id_filter.rs` | `VStep.ids` | ✅ | `v.ids.is_empty()` guard |
| `merge_haslabel_into_edge.rs` | `labels` | ✅ | `out_e.labels.is_empty()` guard |
| `merge_end_vertex_filter.rs` | `end_vertex_ids` | ✅ | Problem 3 fix — `intersect_ids` |
| `extract_end_vertex_filter.rs` | `ids` (accumulation) | ✅ | `intersect_option_ids`, same fix |
| `merge_adde_ids.rs` (write path) | `out_v_id`/`in_v_id`/`rank` on `AddE` | ✅ | Explicit `if ae.X.is_some() { return Err(...) }` — hard error on duplicate, pre-existing |
| `merge_end_vertex_filter.rs` | `rank` | ✅ (fixed here) | Was unguarded — Problem 3b |
| `extract_end_vertex_filter.rs` | `label_preds` (within one `where()` chain) | ✅ (fixed) | Was a single `Option`, guarded but silently dropped the second predicate; now a `Vec` (ANDed), same shape as `property_preds` — see Problem 3c |

### Problem 3c — `label_pred`'s silent drop — root cause was the field's shape, not the guard

The one remaining row from the audit above: a second `.hasLabel()` *inside the same*
`where(otherV()…)` chain (e.g. `where(otherV().hasLabel("a").hasLabel("b"))`) was guarded
against overwrite (`if label_pred.is_none()`), but the guard's `else` was simply absent — the
second predicate vanished with no error and no residual, unlike every other row in the audit
table (which either error or leave the extra occurrence unfolded for downstream evaluation).

**First attempt (superseded):** treat a second `HasLabel` like any unsupported step — flip
`all_filters = false` and bail out of extracting the *entire* `where()` clause, falling through
to the slow path. This worked (no data loss) but was the wrong fix, caught on review: it
pessimizes everything else in the same chain (a leading foldable `hasId()` stops folding too)
to work around a predicate type that never needed special-casing in the first place.

**Root cause, and the actual fix:** `label_pred` was `Option<PrimitivePredicate>` — a single
value — for no real reason. Compare it to the other two fields: `ids` *has* to be a concrete,
collapsible value because it feeds `GetEStep`'s key construction directly
(`end_vertex_ids: SmallVec<...>`); `rank` (Problem 3b) *has* to stay a single `Option<Rank>`
because `GetEStep` only ever builds keys for one rank value, not a list. `label_pred` has
**no such structural role** — its only consumer is `EndVertexFilterStep`'s own filter
evaluation, exactly like `property_preds`. There was never a reason it couldn't just be a
`Vec<PrimitivePredicate>` (ANDed) like `property_preds` already is. Changed it to
`label_preds: Vec<PrimitivePredicate>`; `extract_end_vertex_filter` now just `.push()`es each
one (no special-casing needed at all), and `EndVertexFilterStep::produce()` loops over all of
them the same way it already loops over `property_preds`. A second `hasLabel()` in a chain now
genuinely extracts and gets evaluated via the fused step — strictly better than the bail-out,
not just safe. `where(otherV().hasId(1).hasLabel("a").hasLabel("b"))` extracts the id *and*
both labels together (`test_where_id_and_two_haslabel_all_extracted`).

## Goals & non-goals

- **Goals (Problem 1, done):** replace the 18 hand-written pairwise rules with one small
  priority table and a generic "sort contiguous reorderable runs" pass — same observable
  behavior, same total order, same test results.
- **Goals (Problems 2/3, proposed):** generalize `EndVertexFilter` to carry label/property
  predicates on the other vertex, not just ids, so `where(otherV().has("age", gt(30)))`-shaped
  clauses extract out of the slow `WhereStep`/`OtherVStep` path; fix the confirmed
  ids-overwrite bug (Problem 3) as part of the same struct change; correct the two inaccurate
  `design_explain_step.md` rows regardless of sequencing.
- **Non-goals:** any new structural/key-level speedup for the other vertex's *label* —
  blocked on `docs/design_vertex_label.md`'s open storage-key question, unrelated to this
  document; touching the `GetEStep` vs. scan decision in `build_step.rs` (already correct);
  Cartesian-expanding `GetEStep` to support multiple `rank` values per lookup (Problem 3b's
  guard-and-leave-unfolded fix is the deliberately smaller alternative — see Problem 3b).
  (Multiple label predicates *are* in scope and accumulate via `Vec` — see Problem 3c; an
  earlier draft of this non-goals list excluded that too, which was the bug.)

## Design

### Insight: only two tiers actually matter for correctness

`merge_end_vertex_filter` and `merge_haslabel_into_edge` each independently scan for their
own subset and run in the same fixed-point pass; `merge_end_vertex_filter` runs first and
*removes* every step it folds, which closes the gap between the anchor and whatever came
next — so `merge_haslabel_into_edge` sees the anchor adjacent to `HasLabel` immediately
afterward even if `HasLabel` wasn't the closest one to begin with. The fold rules don't
actually need `HasId`/`HasLabel`/`EndVertexFilter`/`HasRank` in any particular order among
themselves — they only need to be **contiguous, with no `HasProperty(other)`/`Where` in the
middle of the run**. The 6-level total order is correctness-sufficient but not
correctness-necessary; a 2-tier split (fold-eligible vs. not) would also work.

This document still recommends keeping the existing 6-level order rather than collapsing to
2 tiers — it costs nothing extra (same table mechanism either way) and preserves today's
`explain()` output and every existing test assertion about exact step order, which a 2-tier
collapse would silently reshuffle (ties broken by original position) and likely break.

### Replace the pairwise matrix with a priority table

```rust
/// Lower sorts earlier. `None` = not a reorderable filter step at all — acts as a
/// barrier: a contiguous "run" of reorderable steps never crosses one of these.
fn filter_priority(step: &LogicalStep) -> Option<u8> {
    match step {
        LogicalStep::HasId(_) => Some(0),
        LogicalStep::HasProperty(hp) if hp.key.as_str() == ID => Some(0),
        LogicalStep::HasLabel(_) => Some(1),
        LogicalStep::EndVertexFilter(_) => Some(2),
        LogicalStep::HasRank(_) => Some(3),
        LogicalStep::HasProperty(_) => Some(4), // any other key
        LogicalStep::Where(_) => Some(5),
        _ => None,
    }
}

pub fn reorder_filters(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut i = 0;
    while i < plan.steps.len() {
        if filter_priority(&plan.steps[i]).is_none() {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < plan.steps.len() && filter_priority(&plan.steps[j]).is_some() {
            j += 1;
        }
        // [i, j) is a maximal contiguous run of reorderable steps — stable-sort it in place.
        let run = &mut plan.steps[i..j];
        let before: Vec<u8> = run.iter().map(|s| filter_priority(s).unwrap()).collect();
        run.sort_by_key(|s| filter_priority(s).unwrap()); // sort_by_key is stable
        if run.iter().map(|s| filter_priority(s).unwrap()).ne(before) {
            changed = true;
        }
        i = j;
    }
    Ok(changed)
}
```

This is a single pass, not a fixed point — a stable sort of each run is already fully
sorted in one call, unlike the pairwise-bubble version which relied on `apply_rules`' outer
`while plan_changed` loop to converge over multiple passes (still fine to leave wrapped in
that loop; it'll just report `changed = false` on the next call, same as today).

`changed` detection by comparing priorities before/after avoids a spurious `true` when a run
of equal-priority steps gets reordered among themselves by `sort_by_key`'s stability
guarantee (it won't — stable sort never reorders equal keys — but comparing makes that
explicit rather than relying on the reader to know `sort_by_key` is stable).

### Constraints / invariants

- **Stable sort.** Ties (multiple steps with the same priority, e.g. two `HasProperty`
  filters on different keys) must keep their original relative order — both for determinism
  (`explain()` output, existing test assertions) and because reordering same-priority filters
  relative to each other has no fold benefit and is pure risk for no reward.
- **Never cross a barrier.** Only a maximal contiguous run of `filter_priority(_).is_some()`
  steps is ever reordered; a run stops at the first non-filter step (`V`, `Out`, `OutE`,
  `Count`, …) in either direction. This matches today's behavior (the pairwise rules only
  ever match adjacent pairs, never reach across an unrelated step) and must keep matching it.
- **Same total order as today**, so this is a pure internal refactor: every existing
  `reorder_filter.rs` test should pass unchanged against the new implementation, character
  for character on the resulting step sequence — that's the acceptance bar, not just "tests
  pass," since a *different* valid total order could still pass a less specific test suite
  while silently changing `explain()` output for users relying on it.

## Phase 2 (separate, larger) — generalizing `EndVertexFilter` to arbitrary other-vertex predicates

`EndVertexFilter` today is an id-allowlist only (`{ ids: SmallVec<[VertexKey; 4]> }`). This
phase generalizes it to carry *every* kind of filter that can legally appear in
`where(otherV()…)` — id, label, and arbitrary properties — so the whole clause extracts out of
`WhereStep`/`OtherVStep` once, instead of only the id part today.

**Important framing up front:** unlike the id/rank fold (which enables `GetEStep`, an
asymptotic win — point lookup instead of a scan), folding a property/label predicate into
`EndVertexFilter` is a **pipeline-fusion** win, not an indexing win. The property read against
the other vertex still costs the same point-get either way; what's eliminated is `WhereStep`'s
clone-and-run-a-subplan-then-discard wrapper and `OtherVStep`'s intermediate `Vertex`
traverser that gets materialized solely to be thrown away one step later. Worth doing, but
it's a constant-factor improvement, not a complexity-class one — don't oversell it next to the
id/rank fold in the same breath.

### Generalized logical step

```rust
pub struct EndVertexFilter {
    /// `None` = unconstrained. `Some(empty)` = matches nothing (distinct from unconstrained —
    /// see Problem 3; this is the same `Option`-wrapped convention `OutEStep.end_vertex_ids`
    /// already uses, not the bare-`SmallVec` convention `VStep`/today's `EndVertexFilter` use).
    pub ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The other vertex's label predicates, ANDed — same accumulation shape as
    /// `property_preds`. (An earlier draft made this a single `Option<PrimitivePredicate>`,
    /// "first wins" — wrong call, see Problem 3c: label has no structural lookup-key role like
    /// `ids`/`rank` do, so there was never a reason it couldn't just be a list.)
    pub label_preds: Vec<PrimitivePredicate>,
    /// The other vertex's properties, ANDed — same semantics as chaining `.has()` calls.
    /// Naturally accumulative (just `extend()`), unlike `ids` there's no empty-vs-unconstrained
    /// ambiguity to worry about for either of the two `Vec` fields.
    pub property_preds: Vec<(SmolStr, PrimitivePredicate)>,
}
```

`ids` stays a dedicated field rather than folding into `property_preds` — consistent with the
reserved-key disjoint model (`docs/design_reserved_keys.md`, "Strict syntax, narrowly scoped
steps" in `design_principles.md`): id is structurally special, not a generic property, and the
dedicated-field shape is what lets it keep folding into `GetEStep` exactly as it does today.
`label_preds` doesn't need that protection — see Problem 3c for why it's a plain `Vec` instead.

### Fixing Problem 3 as part of this: accumulate, don't overwrite

When the optimizer folds a *second* `EndVertexFilter`-shaped clause onto the same anchor
(two separate `where(otherV()…)` calls, or any future case `reorder_filters` makes adjacent),
combine rather than replace:

- `ids`: intersect. `(None, x) | (x, None) => x`; `(Some(a), Some(b)) => Some(a ∩ b)`. An
  empty intersection becomes `Some(empty)` — already verified to mean "matches nothing"
  correctly all the way down: `OutEStep`/`InOutStep` pass `end_vertex_ids.as_deref()` into
  `AdjacentEdgesOptions.dst`, which `store/rocks/{transaction,snapshot}.rs` turn into a
  `HashSet` via `.map(...)` — `Some(&[])` becomes `Some(HashSet::new())`, and an empty set's
  `.contains()` is always `false`, so every candidate is correctly rejected. **No physical-layer
  change needed for this** — confirmed by reading the existing `dst_set` construction, the fix
  is entirely in the optimizer's accumulation logic.
- `label_preds`/`property_preds`: append (both `Vec`s, ANDed — see Problem 3c for why
  `label_preds` ended up this shape instead of the single-`Option` "first wins" this section
  originally proposed).

### Generalized `extract_end_vertex_filter`

Recognize `where()` sub-plans shaped as `[OtherV, F1, F2, …, Fn]` where every `Fi` is one of
`HasId` / `HasProperty(id)` / `HasLabel` / `HasProperty(other)` — a flat, linear, branch-free
chain of pure filters on the vertex `OtherV` just produced, of any length (not just the
current exact 2-step shapes) — and fold all of them into one `EndVertexFilter` via the
accumulation rules above. Anything else in the chain (a real traversal step, an `And`/`Or`/
`Not` combinator, …) means the whole `where()` is left untouched, same conservative boundary
as today, just widened.

Before attempting that match, **recurse the Problem-1 priority sort into the sub-plan**
(everything after the leading `OtherV`) so `where(otherV().has("age", gt(30)).hasId(2))`
(id-filter not first) gets reordered to `where(otherV().hasId(2).has("age", gt(30)))` first —
today's reorder pass only ever looks at the top-level `plan.steps`, never into a `WhereStep`'s
nested `plan`. Without this, the linear-chain match above would still work (order inside the
chain doesn't matter for *whether* it's all-filters), but doing it anyway keeps the *internal*
order of the extracted `EndVertexFilter`'s residual sub-plan (see next) consistent with the
top-level convention, and is needed regardless for the `ids`-first convention `extract_ids_from_predicate`
callers expect elsewhere.

### Refined `merge_end_vertex_filter`: partial fold

`ids` folds into the anchor's `end_vertex_ids` exactly as today (now via the accumulating
merge above). `label_preds`/`property_preds` have no field to fold into on `OutE`/`InE`/`BothE`
— they remain on a **residual `EndVertexFilter`** (same step, `ids` cleared/consumed) placed
immediately after the anchor. This is a deliberate, expected outcome — `EndVertexFilter`
becomes a real node that can survive optimization, not just an intermediate-only
representation, so it needs its own physical step.

### New physical `EndVertexFilterStep` (fused)

```rust
pub struct EndVertexFilterStep {
    upstream: Option<StepRef>,
    label_preds: Vec<PrimitivePredicate>,                 // schema-resolved at build time, like HasLabelStep
    property_preds: Vec<(PropKeyId, PrimitivePredicate)>, // schema-resolved at build time, like HasPropertyStep
}
```

`produce()`: pull the next upstream traverser (an edge — this step only ever sits right after
an `OutE`/`InE`/`BothE`-derived step), resolve the other vertex's id from the edge key
(same `secondary_id` field `GetEStep` already reads), point-read its label/properties as
needed, evaluate every entry in `label_preds`/`property_preds` (both loops, same shape), and
re-emit the **original edge traverser unchanged** if everything passes (filter semantics —
same as `WhereStep`, never replaces the traverser with the vertex it just checked).

### Constraint: `reject_reserved_key` must be re-applied here

`property_preds` entries are built directly into `EndVertexFilterStep`, **not** through the
generic `HasPropertyStep` build path in `build_step.rs` that currently owns the
`reject_reserved_key` call (`docs/design_reserved_keys.md`). Without an explicit check added
to `EndVertexFilterStep`'s own construction, `where(otherV().has("rank", 5))` — meaningless on
a vertex, but currently caught by the generic guard — would silently bypass it once extraction
starts pulling `HasProperty` steps out of `where()` sub-plans. The physical builder for
`EndVertexFilterStep` must call `reject_reserved_key` on every `property_preds` key, mirroring
the existing `HasProperty`/`Values`/`Properties` guard.

Either way (whether or not this phase ships), `docs/design_explain_step.md` lines 279 and 325
need correcting now: the claim that a non-id property filter "stays as `EndVertexFilterStep`"
was never representable by today's id-only struct.

A related, smaller gap noted but **not proposed for fixing here**: `hasLabel()` inside
`where(otherV()…)` folds fine under this design (the `label_preds` field above) — the
distinction worth keeping in mind is between *folding the filter out of `where()`* (this
document) and *getting a structural/key-level speedup from it* (blocked on
`docs/design_vertex_label.md`'s open question — vertex label lives in the value, not the key,
so `EndVertexFilterStep` still pays a point-get for it, same as `HasLabelStep` does today).

## Files changed

All done.

| File | Change |
|---|---|
| `src/planner/optimizer/reorder_filter.rs` | **Done.** `filter_priority` + single-pass sort, 22/22 tests passing. |
| `src/planner/logical_step/mod.rs` | Generalized `EndVertexFilter` (`ids: Option<…>`, `label_preds: Vec<…>`, `property_preds: Vec<…>` — `label_preds` corrected from an initial `Option` to a `Vec` on review, see Problem 3c) |
| `src/planner/optimizer/extract_end_vertex_filter.rs` | Recurses priority-sort into `where()` sub-plans; matches any linear all-filter chain, not just the exact 2-step shapes; accumulates `label_preds` (no special-casing needed once it's a `Vec`) |
| `src/planner/optimizer/merge_end_vertex_filter.rs` | Accumulates (intersects) `ids` across multiple folds instead of overwriting (Problem 3); guards `rank` the same way (Problem 3b); split-folds `label_preds`/`property_preds` into a residual `EndVertexFilter` |
| `src/engine/volcano/steps/end_vertex_filter.rs` (new) | New fused physical `EndVertexFilterStep`; loops over `label_preds` the same way it already loops over `property_preds` |
| `src/engine/volcano/builder/build_step.rs` | Builds `EndVertexFilterStep` for a residual `EndVertexFilter`; resolves every `label_preds` entry's string label to a `LabelId`; calls `reject_reserved_key` on every `property_preds` key |
| `docs/design_explain_step.md` | Wording on lines 279, 325 fixed (no longer claims a non-id filter "stays as `EndVertexFilterStep`" as if that were unrepresentable) |

## Implementation plan

All steps below are done; kept as a record of the original plan.

1. ~~Add `filter_priority` and the new `reorder_filters` body in `reorder_filter.rs`~~ — done.
2. ~~Change `EndVertexFilter.ids` to `Option<SmallVec<…>>`; accumulate-via-intersection instead
   of overwrite~~ — done (Problem 3), plus the same fix applied to `rank` (Problem 3b).
3. ~~Add `label_preds`/`property_preds` fields; generalize `extract_end_vertex_filter` to the
   linear-chain match~~ — done. (`label_preds` started as a single `Option`, "first wins" —
   corrected to a `Vec` on review, see Problem 3c; no chain-extraction special-casing needed
   once it matched `property_preds`'s shape.)
4. ~~Add the residual-fold split in `merge_end_vertex_filter` and the new `EndVertexFilterStep`
   physical step + its `build_step.rs` wiring, including `reject_reserved_key`~~ — done.
5. ~~Fix the two `design_explain_step.md` rows~~ — done.

## Test plan

Problem 1: the 22 existing `reorder_filter.rs` tests are the regression suite — already
passing. Problems 2/3:
- Regression test for the confirmed overwrite bug (step 2 above).
- `extract_end_vertex_filter`: a 3+-step linear chain (`where(otherV().hasId(2).has("age", gt(30)))`)
  now extracts into one `EndVertexFilter`; an out-of-order chain
  (`where(otherV().has("age", gt(30)).hasId(2))`) extracts identically after the internal
  reorder; a chain containing a real traversal step (`where(otherV().out("knows"))`) still
  does **not** extract.
- `merge_end_vertex_filter`: ids fold into `end_vertex_ids` and a residual `EndVertexFilter`
  (with only `property_preds`) survives immediately after; two folded `EndVertexFilter`s with
  overlapping ids intersect correctly, including the empty-intersection ("matches nothing")
  case producing `Some(empty)`, not `None`.
- `EndVertexFilterStep`: end-to-end test via `gremlin/tests.rs` against the TinkerPop "modern"
  graph — `where(otherV().has("age", gt(30)))` after `outE()` returns the same results as
  today's unfolded path, confirming the fusion is behavior-preserving.
- `reject_reserved_key`: `where(otherV().has("rank", 5))` is rejected at build time, not
  silently accepted via the new extraction path.

Problem 3b: `test_out_e_second_hasrank_not_merged` / `test_in_e_second_hasrank_not_merged` /
`test_both_e_second_hasrank_not_merged` (`merge_end_vertex_filter.rs`) — a second `hasRank()`
on the same anchor folds into a residual `HasRank` step, not an overwrite.
`test_out_e_duplicate_same_value_hasrank_not_merged_but_consistent` — same value twice still
only folds once (no correctness issue, just confirms the guard fires regardless of whether the
values match). `test_double_has_rank_conflicting_values_matches_nothing` (`gremlin/tests.rs`)
— end-to-end: `outE(["knows"]).hasRank(eq(0u16)).hasRank(eq(1u16))` against the "modern" graph
returns 0 results, not the 2 a silent overwrite-to-`rank==1`-then-ignore-the-rest would give.

Problem 3c: `test_where_second_haslabel_in_same_chain_both_accumulate` — a 2-`hasLabel()` chain
extracts into one `EndVertexFilter` with `label_preds.len() == 2`, not a bail-out and not a
silently-truncated single predicate. `test_where_id_and_two_haslabel_all_extracted` — same,
with a leading `hasId()` present, confirming the id *and* both labels all extract together
(not the bail-out-blocks-everything behavior the first fix attempt had).
`test_where_double_haslabel_same_chain_matches_nothing` (`gremlin/tests.rs`) — end-to-end:
`where(otherV().hasLabel(["person"]).hasLabel(["software"]))` against the "modern" graph
returns 0 results (no vertex is both) via the fused `EndVertexFilterStep` path now, same
correct result as the original bail-out-to-slow-path version gave.

## Out of scope

- Any new structural/key-level speedup for the other vertex's *label* — blocked on
  `docs/design_vertex_label.md`'s open storage-key question. (Multiple label predicates do
  AND-combine now via `label_preds: Vec<…>` — see Problem 3c — this item is only about there
  being no key-level shortcut to fold *into*, same as for any single label predicate.)
- Cartesian-expanding `GetEStep` to support multiple `rank` values per anchor — Problem 3b's
  guard-and-leave-unfolded fix is the deliberately smaller alternative.
- Changing the `GetEStep` vs. scan decision in `build_step.rs` — already correct, unrelated
  to how filters get adjacent to their anchor.
