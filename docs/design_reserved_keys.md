# Design: `id` / `label` / `rank` — reserved-key syntax & semantics

Status: implemented.

## Problem

RocksGraph treats three names — `"id"`, `"label"`, `"rank"` — as **reserved property keys**:
pre-registered in every `Schema` with fixed numeric IDs, and `Vertex`/`Edge` synthesize their
values on the fly instead of storing them in the property blob.  Earlier, these three names
were reachable through *two* overlapping mechanisms: dedicated steps (`id()`/`label()`/
`hasId()`/`hasLabel()`) and the generic property machinery (`values()`/`properties()`/`has()`
with a bare string, or `Key::Id`/`Key::Label` tokens) — and that overlap had already produced
one real, silent bug (label decoding was inconsistent across the two paths).

This document originally recorded that shared-namespace design as a reference, with an open
question: keep the shared namespace, or move to TinkerPop's disjoint model (intrinsic
attributes accessed only through dedicated steps, never through the generic property
namespace)? **Resolved: moved to the disjoint model.** `"id"`/`"label"`/`"rank"` are now
rejected by `values()`/`properties()`/`has()` (unfolded) — dedicated steps are the only
sanctioned access path. This document now records that resolved design.

## Goals & non-goals

- **Goals:** `"id"`/`"label"`/`"rank"` are accessible **only** through dedicated steps —
  `id()`/`label()`/`rank()` for extraction, `hasId()`/`hasLabel()`/`hasRank()` for filtering.
  `values()`/`properties()`/`has()` reject all three, with an error pointing at the dedicated
  replacement. The optimizer-fold shortcuts (`.outE(...).has("rank", N)` immediately adjacent
  to an edge-traversal step) are preserved, since folding happens before the rejection check
  ever runs.
- **Non-goals:** Changing what's stored on disk (still synthesized, never in the property
  blob); adding a `Key`-style token type back (the disjoint model doesn't need one — see
  "Gremlin-layer access paths" below).

## Design

### Reservation in the schema layer

`src/types/prop_key.rs` defines the three names as constants with fixed, never-reassigned
numeric IDs, pre-registered by `Schema::default()`:

| Name | Constant | `prop_key_id` | Synthesized type | Carries structural meaning? |
|---|---|---|---|---|
| `"id"` | `ID` | `ID_KEY_ID = 1` | `Int64` (vertex only — see below) | Yes — `VertexKey` |
| `"label"` | `LABEL` | `LABEL_KEY_ID = 2` | `Int32` | Yes — `LabelId` |
| `"rank"` | `RANK` | `RANK_KEY_ID = 3` | `UInt16` (edge only — vertices have no rank) | Yes — edge multiplicity discriminator |

`Vertex::get_value`/`get_property` synthesize `ID_KEY_ID`/`LABEL_KEY_ID`. `Edge::get_value`/
`get_property` synthesize `LABEL_KEY_ID`/`RANK_KEY_ID` — **not** `ID_KEY_ID`: an edge's id is
the 30-character Base64 canonical-key string (`design_edge_id_string.md`), produced
exclusively by `IdStep` calling `EdgeKey::to_id_string()` directly. There never was a generic
synthesis path for edge id — `id()` is the only way to get it, for either element type.

`resolve_prop_key`/`declare_prop_key` lock a property key's `DataType` on first
registration; since these three are pre-locked to `Int64`/`Int32`/`UInt16` respectively,
any attempt to later use the same name with an incompatible type is rejected as a type
mismatch.

### Read path — synthesized, never stored, gated to dedicated steps

`Vertex::get_value`/`get_property` intercept `ID_KEY_ID` and `LABEL_KEY_ID` *before*
touching `self.props`, synthesizing the value from `self.id`/`self.label_id` directly.
`Edge::get_value`/`get_property` do the same for `LABEL_KEY_ID` and `RANK_KEY_ID` (and only
those two — see above for edge id). `label` is decoded from its raw `LabelId` to a string
by `Schema::decode_label_value`.

This synthesis is still used by the dedicated steps (`IdStep`, `LabelStep`, `RankStep`,
`HasIdStep`, `HasLabelStep`, `HasRankStep`) — but `reject_reserved_key`
(`engine/volcano/builder/build_step.rs`) stops `"id"`/`"label"`/`"rank"` from reaching the
*generic* steps (`HasPropertyStep` via `.has()`, `ValuesStep` via `.values()`/`.properties()`)
in the first place, at physical-build time. The dedicated steps never go through this check
— they're built from their own `LogicalStep::Id`/`Label`/`Rank`/`HasId`/`HasLabel`/`HasRank`
variants, which `reject_reserved_key` doesn't apply to.

### Write path — optimizer folding, not a generic property

`"id"` and `"rank"` are not just reserved *read* values — when used as the property key
in `.property(...)` immediately after the element-creating step, the optimizer folds them
into the structural field they represent, before a physical `PropertyStep` is ever built:

- `merge_addv_id` — folds trailing `property("id", N)` into `AddVStep`'s vertex id.
- `merge_end_vertex_filter` — folds `has("id", ...)`/`has("rank", ...)` into the
  preceding edge-traversal step's end-vertex filter / rank.
- `merge_adde_rank` — folds trailing `property("rank", N)` into `AddEStep`'s rank field.

Writes to reserved keys that are NOT folded are explicitly rejected with
`StoreError::SchemaViolation`. `"label"` has no write-side equivalent — it is only ever
set via `addV(label)`/`addE(label)`'s dedicated argument.

**The same fold/reject split now applies symmetrically on the read side.** Three existing
fold rules all run before `reject_reserved_key` ever sees the plan, so each of their patterns
keeps working unchanged:
- `merge_v_id_filter` — folds `V([]).has("id", N)` into `V([N])` (a direct index seek).
- `merge_end_vertex_filter` — folds `.outE(...).has("rank", N)` (and `has("id", N)` on the
  end vertex) into that step's structural filter.
- `merge_haslabel_into_edge` — folds `.outE([]).has("label", N)` into `.outE([N])`, **only**
  when the edge step has no label restriction yet (`out_e.labels.is_empty()`) — verified
  directly: `.outE([]).has("label", "knows")` folds and succeeds, but
  `.outE(["knows"]).has("label", "knows")` (labels already non-empty) does not match the
  fold's precondition and is correctly rejected by `reject_reserved_key` instead.

Any reserved-key access that *isn't* foldable (not adjacent to the right step, e.g. hidden
behind a `union()`) now gets rejected instead of silently falling through to the generic
property step.

### Gremlin-layer access paths — `Key` removed, dedicated steps only

There is no `Key` type anymore. `.has(key, pred)` takes a plain property name
(`impl Into<SmolStr>`); `.values()`/`.properties()` take plain `&str` keys. `"id"`/`"label"`/
`"rank"` passed this way are rejected by `reject_reserved_key`, with an error pointing at the
dedicated replacement (`id()`/`label()`/`rank()`/`hasId()`/`hasLabel()`/`hasRank()`).

`hasId()`/`hasLabel()`/`hasRank()` take `impl Into<Predicate>`, not a plain list — a fixed-size
array of values still collapses to `Eq` (one element) or `Within` (more than one) via
`impl<T: Into<Value>, const N: usize> From<[T; N]> for Predicate` (`gremlin/value.rs`), so
`hasId([1, 2, 3])` keeps working unchanged, while `hasId(gt(2i64))`/`hasId(within([...]))`/
`hasId(without([...]))` are also valid. `hasLabel()` additionally rejects range predicates
(`gt`/`gte`/`lt`/`lte`/`between`) via `validate_label_predicate` — label names aren't
meaningfully ordered. Negation beyond `Without` (i.e. for `hasId([])`-shaped queries with no
explicit predicate) goes through the existing `not()` combinator: `.not(__().hasId([1, 2]))`.

## Constraints / invariants

- Reserved-key writes (`id`, `label`, `rank`) are rejected by the builder unless folded
  by the optimizer into the structural field. Reserved-key *reads* (via `HasProperty`/
  `Values`/`Properties`) follow the identical rule — rejected unless folded.
- `label` decoding must go through `Schema::decode_label_value` at every access point
  that hands a label value to a caller.
- `rank()`/`hasRank()` are edge-only: `RankStep` errors on a vertex traverser (extraction
  of a value that doesn't exist should be loud, not silent); `HasRankStep` treats a vertex
  traverser as a non-match (consistent with `HasLabelStep`'s type-mismatch handling — a
  filter narrows a stream, it doesn't assert about the stream's shape).
- `reject_reserved_key` must run *after* `apply_rules` (the optimizer pass) and *before*
  any generic property physical step is constructed — running it earlier would reject
  the fold-eligible cases too; running it later would let the leak back in.

## Trade-offs

**In favor:**
- The intrinsic three (id/label/rank) can never again drift between access paths the way
  label decoding once did — there's exactly one path per name now (its dedicated step),
  not several that have to independently stay consistent.
- `rank` genuinely needs to be a readable, filterable value (multi-edge disambiguation),
  which TinkerPop's model has no equivalent for — dedicated `rank()`/`hasRank()` give it
  that without reopening the shared-namespace problem.

**Costs:**
- `hasId()`/`hasLabel()`/`hasRank()` taking `impl Into<Predicate>` instead of a plain
  `IntoIterator` means `hasId([])` (truly empty) doesn't infer without a type annotation —
  same `[]`-inference limitation the collection-taking methods had before *their* fix,
  now also true here, documented as a known latent gap.
- The fold/reject split (rather than rejecting unfolded reserved-key access unconditionally)
  means the same `.has("rank", N)` call's validity depends on what step precedes it in the
  traversal — easy to forget when adding a new optimizer rule that could plausibly fold
  a reserved key, or when removing one.

## Open questions (all resolved)

1. **Reject reserved-key writes outright? (Resolved)** Misplaced writes to `"id"`,
   `"label"`, and `"rank"` are explicitly checked and rejected.
2. **Add `Key::Rank`? (Resolved — moot.)** `Key` itself was removed rather than extended.
   `.has()`/`.values()`/`.properties()` take plain strings now; there's no token type left
   to add a third variant to.
3. **Keep the shared-namespace model? (Resolved — no.)** Moved to TinkerPop's disjoint
   model: `"id"`/`"label"`/`"rank"` are rejected by the generic property steps
   (`reject_reserved_key`, unless optimizer-folded) and reachable only through
   `id()`/`label()`/`rank()`/`hasId()`/`hasLabel()`/`hasRank()`.
