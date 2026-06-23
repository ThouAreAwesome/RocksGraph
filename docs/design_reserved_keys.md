# `id` / `label` / `rank`: Reserved-Key Syntax & Semantics

## Problem

RocksGraph treats three names — `"id"`, `"label"`, `"rank"` — as **reserved property
keys**: they are pre-registered in every [`Schema`](../src/schema/definition.rs) with fixed
numeric IDs, and `Vertex`/`Edge` synthesize their values on the fly instead of storing them
in the property blob. This is a deliberate simplification (one string namespace for
everything queryable, instead of a disjoint "intrinsic attribute" namespace like most graph
databases use) — but it means the three names carry *different* semantics depending on which
of several roughly-equivalent-looking access paths a caller uses, and that inconsistency has
already produced one real, silent bug (see "Recently-fixed inconsistency" below).

This document records what RocksGraph currently does, why, and how that compares with
TinkerPop and other property-graph systems, as a reference for whether the current design is
worth keeping as-is or tightening.

---

## 1. Current RocksGraph implementation

### 1.1 Reservation in the schema layer

[`src/types/prop_key.rs`](../src/types/prop_key.rs) defines the three names as constants with
fixed, never-reassigned numeric IDs, pre-registered by
[`Schema::default()`](../src/schema/definition.rs):

| Name | Constant | `prop_key_id` | Synthesized type | Carries structural meaning? |
|---|---|---|---|---|
| `"id"` | `ID` | `ID_KEY_ID = 0` | `Int64` | Yes — `VertexKey` |
| `"label"` | `LABEL` | `LABEL_KEY_ID = 1` | `Int32` | Yes — `LabelId` |
| `"rank"` | `RANK` | `RANK_KEY_ID = 2` | `Int32` | Yes — edge multiplicity discriminator (`design_multiple_edges.md`) |

`resolve_prop_key`/`declare_prop_key` lock a property key's `DataType` on first registration
(`src/schema/definition.rs`); since these three are pre-locked to `Int64`/`Int32`/`Int32`
respectively, any attempt to later use the same name with an incompatible type (e.g.
`.property("label", "x")`, a `String`) is rejected as a type mismatch — see §1.4.

### 1.2 Read path — synthesized, never stored

[`Vertex::get_value`/`get_property`](../src/types/element.rs) intercept `ID_KEY_ID` and
`LABEL_KEY_ID` *before* touching `self.props`, synthesizing the value from `self.id`/
`self.label_id` directly — a vertex's "id"/"label" never actually appear in its stored
property blob. [`Edge::get_value`/`get_property`](../src/types/element.rs) do the same for
`LABEL_KEY_ID` and `RANK_KEY_ID`, from `self.label_id`/`self.rank`. `Vertex` has no synthesized
case for `RANK_KEY_ID` — rank is an edge-only concept.

`id` and `rank` are returned as their raw numeric value directly — there is nothing to decode,
they're already meaningful integers. `label` is the one exception: the synthesized value is a
`LabelId` (`u16`), which has no meaning to a caller without a `Schema` lookup to turn it back
into the string the user wrote in `addV("person")`/`addE("knows")`. Decoding that id back to a
name is done by [`Schema::decode_label_value`](../src/schema/definition.rs), called from each
of the three places that can hand a label value to a caller: `HasPropertyStep`,
`ValuesStep`'s scalar branch (`.values(...)`), and `ValuesStep`'s property branch
(`.properties(...)`) — see [`src/engine/volcano/steps/has_property.rs`](../src/engine/volcano/steps/has_property.rs)
and [`src/engine/volcano/steps/values.rs`](../src/engine/volcano/steps/values.rs).
`HasLabelStep` ([`src/engine/volcano/steps/has_label.rs`](../src/engine/volcano/steps/has_label.rs))
deliberately keeps the *raw* `Int32` from `get_value` and compares numeric ids directly — it
already resolved the filter names to ids once at build time, so decoding would be pure
overhead.

### 1.3 Write path — optimizer folding, not a generic property

`"id"` and `"rank"` are not just reserved *read* values — when used as the property key in
`.property(...)` immediately after the step that creates the element, the optimizer folds them
into the structural field they actually represent, before a physical `PropertyStep` is ever
built:

- `merge_addv_id` ([`src/planner/optimizer/merge_addv_id.rs:34`](../src/planner/optimizer/merge_addv_id.rs#L34)) —
  folds a trailing `property("id", N)` into `AddVStep`'s vertex id.
- `merge_end_vertex_filter` ([`src/planner/optimizer/merge_end_vertex_filter.rs:61`](../src/planner/optimizer/merge_end_vertex_filter.rs#L61),
  [`:72`](../src/planner/optimizer/merge_end_vertex_filter.rs#L72)) — folds `has("id", ...)`/`has("rank", ...)`
  into the preceding edge-traversal step's end-vertex filter / rank.
- `merge_adde_rank` ([`src/planner/optimizer/merge_adde_ids.rs:80`](../src/planner/optimizer/merge_adde_ids.rs#L80)) —
  folds a trailing `property("rank", N)` into `AddEStep`'s rank field.

So `"id"` and `"rank"` are doubly special: a synthesized *read* value (§1.2) **and** optimizer
syntax for setting structural identity on write — they never reach a generic `PropertyStep` in
the paths the optimizer recognizes. `"label"` has no write-side equivalent: it is only ever set
via `addV(label)`/`addE(label)`'s dedicated argument, never via `.property("label", ...)`.

### 1.4 Closed Gap — Explicit Rejection of Reserved Key Writes

Writes to reserved property keys (`id`, `label`, and `rank`) are explicitly validated and rejected:
- **`label` writes** are checked early on the traversal builder (`WriteTraversal::property` and `GraphTraversal::property`) and rejected with `StoreError::SchemaViolation("Cannot manually set or update the reserved property 'label'...")`.
- **`id` and `rank` writes** that are not folded by the optimizer rules (e.g., when they are misplaced or do not immediately follow element creation) are validated during physical plan building and rejected with `StoreError::SchemaViolation("Unfolded or misplaced reserved property key...")`. This prevents silent data drops and ensures safety.

### 1.5 Gremlin-layer access paths

[`Key`](../src/gremlin/value.rs#L54) is RocksGraph's analogue of TinkerPop's `T` token —
`Key::Id`, `Key::Label`, `Key::Property(String)` — used by `.has(key, pred)`. Critically,
`key_to_prop_key` ([`src/gremlin/conversions.rs:74`](../src/gremlin/conversions.rs#L74)) maps
`Key::Id => ID` and `Key::Label => LABEL` — the exact same `PropKey` string that
`Key::Property("id")`/`Key::Property("label")` would produce. Because `&str`/`String` convert
to `Key` via `Key::Property` ([`src/gremlin/value.rs:63`](../src/gremlin/value.rs#L63)), a bare
string `"id"`/`"label"` and the explicit token end up **indistinguishable** by the time they
reach a `LogicalStep` — there is no `Key::Rank` at all, so `"rank"` is only ever reachable as a
plain string.

The one place this *does* still branch on which form was used is `.has()`'s routing
(`push_has_step`, [`src/gremlin/conversions.rs:89`](../src/gremlin/conversions.rs#L89)):
`Key::Id` routes to `HasIdStep`, `Key::Label` routes to `HasLabelStep` (label-name resolution,
§1.2's `HasLabelStep` exception), and `Key::Property(s)` — which a bare string always becomes —
routes to the generic `HasPropertyStep`. So `.has(Key::Label, "person")` and
`.has("label", "person")` take *different physical steps* even though they resolve to the same
`PropKey`, and (before the fix in §1.6) only one of the two decoded correctly.

### 1.6 Recently-fixed inconsistency

Before this review, only `ValuesStep`'s scalar branch (`.values(["label"])`) decoded
`LABEL_KEY_ID`'s raw `Int32` to a string. `HasPropertyStep` (reached by `.has("label", ...)`,
§1.5) and `ValuesStep`'s property branch (`.properties(["label"])`) did not — so
`.has("label", "person")` always returned zero results (comparing a raw `Int32` against the
caller's `Primitive::String`), and `.properties(["label"])` returned the meaningless raw id
instead of a name. Both are now fixed by routing all three through the same
`Schema::decode_label_value` helper (§1.2).

---

## 2. Design intent

[`design_auto_schema.md`](design_auto_schema.md)'s own problem statement frames the bug it set
out to fix using the bare string form — *"`values("label")` returns
`Primitive::Int32(label_id as i32)` — a raw number with no semantic meaning to the caller"* —
not `values(T.label)` or a dedicated `.label()` step. That choice of example confirms the
project's intent: `"label"` (and, by the same pattern already in place before that design doc,
`"id"`/`"rank"`) is meant to be usable as an ordinary string key across `.values()`/`.has()`/
`.properties()`, decoding to something meaningful, rather than being walled off into a separate
token namespace the way TinkerPop's `T.id`/`T.label` are. The design doc's §6/§8 describe fixing
the decode at specific points (`get_value`'s "label" case, `get_all_props`/`ValuesStep`'s
decode points) but did not enumerate `has()`/`properties()` explicitly — that gap is what
produced the inconsistency in §1.6.

---

## 3. Industrial practice comparison

### 3.1 Apache TinkerPop / Gremlin

`id()` and `label()` are core `Element` attributes, not properties — there is no `Property`
object for them at all. They are accessed exclusively through:

- Dedicated steps: `.id()`, `.label()`.
- The `T` token enum (`T.id`, `T.label`), used *inside* other steps: `hasLabel(...)` is sugar
  for `has(T.label, ...)`; `.by(T.label)`, `.select(T.label)` work the same way.

A bare string `"id"`/`"label"` passed to `.has()`/`.values()`/`.properties()` is **not** a
token — it is an ordinary property-key lookup. TinkerGraph (the reference implementation)
actively rejects defining a real property by either name
(`IllegalArgumentException: Property key can not be a reserved key: id`), so in practice
`g.V().has("label", "person")` and `g.V().values("label")` (bare string, not the token) match
**nothing**, for any vertex — not an error, just a vacuous query against a property that can
never exist. There is no TinkerPop equivalent of `"rank"`; parallel edges are distinguished by
edge `id()` alone.

### 3.2 JanusGraph

Same separation as TinkerPop (it implements the TinkerPop API): vertex/edge `id()`/`label()`
are intrinsic, never declared via `PropertyKey`, and reserved system names cannot be reused as
property keys. Edge multiplicity (`SIMPLE`/`MULTI`/`MANY2ONE`/...) is a *label-level*
configuration, not a queryable per-edge value the way RocksGraph's `rank` is — there is nothing
to read back, only a write-time constraint.

### 3.3 Neo4j / Cypher

Relationship/node ids are accessed via the `id()` function, and labels via the `labels(n)`
function (nodes can carry a *set* of labels) — both are syntactically functions over an
element, never property-map lookups, so there is no namespace collision to even consider:
nothing stops a user from also having a property literally called `id` or `label`, because the
two are unrelated names in unrelated grammars.

### 3.4 Summary

| System | id/label namespace vs. properties | Reserved-name write protection | Per-edge multiplicity tiebreaker exposed as a value? |
|---|---|---|---|
| TinkerPop / TinkerGraph | Disjoint (`T` tokens / `.id()`/`.label()`) | Yes — rejects `property(T.id\|T.label, ...)` | No — edge `id()` is unique already |
| JanusGraph | Disjoint (same TinkerPop API) | Yes | No — multiplicity is a write-time label config, not a per-edge value |
| Neo4j / Cypher | Disjoint (functions, not property lookups) | N/A — different grammar entirely, no collision possible | N/A |
| **RocksGraph** | **Shared** — `id`/`label`/`rank` are reserved entries in the same `PropKey` namespace as user properties | **Partial / accidental** — only blocked when the value's type happens to mismatch the locked-in type (§1.4) | **Yes** — `rank` is a real, queryable `Int32` property-like value |

---

## 4. Trade-offs of RocksGraph's approach

**In favor of the current design:**
- One resolution path (`Schema::prop_key_id`/`resolve_prop_key`) and one wire format
  (`prop_key_id: u16`) for everything queryable — `id`/`label`/`rank`/user properties all flow
  through `.values([...])` uniformly, with no second token type to thread through the builder,
  the optimizer, and the physical steps.
- `rank` genuinely needs to be a readable, filterable value (multi-edge disambiguation), which
  TinkerPop's stricter model has no real equivalent for — RocksGraph already can't fully copy
  TinkerPop here even if it wanted to.

**Costs already visible:**
- The three names are special in *two* different, easy-to-conflate ways depending on context —
  read-only synthesized value (`label`) vs. write-time optimizer syntax (`id`, `rank`) — which
  is itself one more thing every new physical step or optimizer rule has to remember to handle
  (and, per §1.6, one of them initially didn't).
- Reservation is enforced only as a side effect of type-locking, not a deliberate check — a
  same-typed `.property("label", 99i32)` silently no-ops instead of erroring (§1.4), unlike
  TinkerGraph's explicit rejection.
- `Key`/string-literal access to `"id"`/`"label"` already collide by construction (§1.5); a
  future `Key::Rank` would need the same care to behave consistently with the bare string
  `"rank"` it would alias.

---

## 5. Open questions

These are design decisions, not bugs — recorded here for a deliberate choice rather than a
silent drift:

1. **Reject reserved-key writes outright? (Resolved)** Yes. Misplaced writes to `"id"`, `"label"`, and `"rank"` are now explicitly checked and rejected with a build-time `StoreError::SchemaViolation`.
2. **Add `Key::Rank`?** For symmetry with `Key::Id`/`Key::Label`, should there be an explicit
   token for rank, even though it would alias the same `PropKey` as the bare string `"rank"`
   either way?
3. **Keep the shared-namespace model, or move toward TinkerPop's disjoint one?** The current
   fix (§1.6) doubled down on "bare string keys alias the reserved value, consistently, across
   every access path." The alternative — stop treating bare `"id"/"label"/"rank"` as special
   anywhere, and require `Key::Id`/`Key::Label`/dedicated steps the way TinkerPop does — would
   be a larger, deliberate behavior change (and would need `Key::Rank` to exist at all, since
   `"rank"` has no step-based equivalent today).
