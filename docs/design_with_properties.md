# Design: withProperties() Property Fetch Hint

## Problem

`materialize()` always calls `get_all_props()` for every `Vertex`/`Edge` in the terminal
result, fetching all properties via a RocksDB prefix scan. When the caller only needs id +
label, or a small subset of properties, this is wasteful — especially for traversals like
`outE().otherV().path()` that return many elements.

## Proposed API

```rust
// Return vertices with no properties — zero extra reads for edges, label-only read for vertices
g.V([1]).out([KNOWS]).withProperties([]).to_list()

// Return vertices with selected properties only
g.V([1]).out([KNOWS]).withProperties(["name", "age"]).to_list()

// Default: no withProperties() call = current behavior, all properties fetched
g.V([1]).out([KNOWS]).to_list()
```

`withProperties()` is a trailing step (not a per-preceding-step modulator). It applies
uniformly to all terminal `Vertex`/`Edge` values produced by the traversal.

## PropHint

```rust
enum PropHint {
    All,                          // default — existing get_all_props() behavior
    None,                         // id + label only, no property reads
    Keys(SmallVec<[SmolStr; 4]>), // fetch only named properties
}
```

## Design: PropHint embedded in GValue (per-element)

Preferred over a plan-level hint because path results mix vertices and edges independently.

```rust
// GValue variants gain an inline PropHint
GValue::Vertex(VertexKey, PropHint)
GValue::Edge(EdgeKey, PropHint)
```

`withProperties()` is a `LogicalStep::WithProperties { keys }`. The planner does NOT execute
it as a pipeline step; instead, when building the physical plan, it extracts the hint and
stamps every upstream `GValue::Vertex` / `GValue::Edge` produced in that plan with it.

Alternatively (simpler first cut): store `PropHint` on `BuiltTraversal` and pass it into
`materialize()` — avoids touching `GValue` but cannot support per-element hints inside
`path()` or `union()`. Upgrade to per-element later if needed.

## materialize() changes

```rust
fn materialize(gv: GValue, ctx: &mut dyn GraphCtx) -> Result<Value, StoreError> {
    match gv {
        GValue::Vertex(vk, hint) => match hint {
            PropHint::All  => { /* current get_all_props() path */ }
            PropHint::None => Ok(Value::Vertex(Vertex { id: vk, label: None, properties: HashMap::new() }))
            PropHint::Keys(keys) => { /* get_label() + per-key get_value() */ }
        }
        GValue::Edge(ek, hint) => match hint {
            PropHint::All  => { /* current get_all_props() path */ }
            PropHint::None => Ok(Value::Edge(Edge { out_v, in_v, label: Some(schema.edge_label_str(ek.label_id)...), rank: ek.rank, properties: HashMap::new() }))
            PropHint::Keys(keys) => { /* per-key get_value(); label_id still free from ek.label_id, but resolving it to a string now needs the in-memory schema lookup */ }
        }
        _ => { /* unchanged */ }
    }
}
```

Note: `PropHint::None` on edges costs zero extra *RocksDB* reads — `label_id` is already in
`EdgeKey`, and resolving it to the `label` string is an in-memory schema lookup, not a store
read. `PropHint::None` on vertices still needs a label read unless Option B from
`design_vertex_label.md` is implemented first.

## New GraphCtx methods needed

```rust
// For PropHint::Keys — fetch specific properties
fn get_selected_props(
    &mut self,
    key: &CanonicalKey,
    props: &[SmolStr],
) -> Result<Option<(LabelId, Vec<(PropKey, Primitive)>)>, StoreError>;

// For PropHint::None on vertices — label-only read (skip if vertex label is in key)
fn get_label(&mut self, key: &CanonicalKey) -> Result<Option<LabelId>, StoreError>;
```

Both can be omitted until `PropHint::Keys` is implemented; `PropHint::None` is the
valuable first milestone.

## Value::Vertex label change (prerequisite)

`PropHint::None` requires `label: Option<SmolStr>` in `Value::Vertex` (see
`design_vertex_label.md` Option A). Without this, we cannot return a vertex without
fetching the label.

## Implementation order

1. Change `Value::Vertex.label` to `Option<SmolStr>` (prerequisite, low effort)
2. Add `PropHint` enum
3. Add `withProperties()` to `TraversalBuilder` / `GraphTraversal` → `LogicalStep::WithProperties`
4. Thread hint into `BuiltTraversal` (plan-level, simpler first cut)
5. Update `materialize()` for `PropHint::None` — zero reads for edges, label-only for vertices
6. (Follow-on) Implement `PropHint::Keys` with `get_selected_props()`
7. (Follow-on) Move hint into `GValue::Vertex` / `GValue::Edge` for per-element control

## Affected Files

- `src/gremlin/value.rs` — `Vertex.label: Option<SmolStr>`
- `src/gremlin/traversal.rs` — `materialize()`, `BuiltTraversal`, `withProperties()` step
- `src/planner/logical_step.rs` — `LogicalStep::WithProperties`
- `src/engine/volcano/builder.rs` — extract hint during physical plan build
- `src/engine/context.rs` — `GraphCtx` new methods (step 6+)
- `src/graph.rs` — implement new `GraphCtx` methods
- `src/store/rocks/snapshot.rs`, `transaction.rs` — store-level selected prop fetch
