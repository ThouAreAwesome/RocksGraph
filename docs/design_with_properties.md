# Design: withProperties() Property Fetch Hint

## Problem

`materialize()` always calls `get_all_props()` for every `Vertex`/`Edge` in the terminal
result, fetching all properties from the store. When the caller only needs id + label — or
when no element reaches the terminal at all (count, dedup, fold) — this is pure waste.
Traversals like `outE().otherV().path()` amplify it: every position in the path triggers a
full property fetch.

## Design

Change the default: do NOT fetch properties unless explicitly asked. The API mirrors the
existing `[] = all` convention used by `V([])`, `outE([])`, `values([])`, `properties([])`.

```rust
// Default: no withProperties() → return id + label only, zero property reads
snap.g().V([1]).out(["knows"]).to_list()?;

// Explicit: return all properties ([] = all convention)
snap.g().withProperties([]).V([1]).out(["knows"]).to_list()?;

// Explicit: return named properties only
snap.g().withProperties(["name", "age"]).V([1]).out(["knows"]).to_list()?;
```

`withProperties()` is a **source-level configuration** placed after `g()` (matching
TinkerPop's `with()` convention).

## Why the default change is safe

Property reads during traversal are **read-through cached** by the `LogicalGraph` /
`LogicalSnapshot` overlay: the first access to any vertex or edge loads it from the store
and caches it; all subsequent accesses within the same traversal are O(1) HashMap lookups.

Whether that first access happens at `has("age", gt(18))` or at `materialize()` time makes
no difference to I/O cost — it's the same read against the same cache. The overlay
guarantees no double-reads regardless of fetch timing.

| Scenario | Store reads per terminal element |
|---|---|
| Default (no hint), element not filtered | 0 (label from in-memory schema lookup) |
| Default, element already loaded by `has(…)` / `values(…)` | 0 (overlay cache hit) |
| `withProperties([])` | 1 if not yet in overlay; 0 if already cached by a prior step |
| `withProperties(["name"])` | 1 (first `get_property` loads full record; subsequent calls cache-hit) |

## Implementation

One optional field on `BuiltTraversal`. No new types, no `GValue` change, no plan step.

```rust
// gremlin/traversal/built.rs
pub struct BuiltTraversal<'g> {
    graph:     &'g mut dyn GraphCtx,
    plan:      PhysicalPlan,
    prop_keys: Option<Vec<SmolStr>>,  // None = default (no properties)
}
```

```rust
// Same file — materialize() gains one parameter
fn materialize(
    gv: GValue,
    ctx: &mut dyn GraphCtx,
    prop_keys: Option<&[SmolStr]>,
) -> Result<Value, StoreError> {
    match gv {
        GValue::Vertex(vk) => {
            match prop_keys {
                None => {
                    // label only, no property read
                    let label = /* schema vertex_label_str from overlay label_id */;
                    Ok(Value::Vertex(Vertex { id: vk, label, properties: HashMap::new() }))
                }
                Some(keys) if keys.is_empty() => {
                    // all properties (existing get_all_props path)
                    let (label_id, props) = ctx.get_all_props(&CanonicalKey::Vertex(vk))?;
                    // ... materialize label + all props ...
                }
                Some(keys) => {
                    // named properties via existing get_property()
                    for key in keys { ctx.get_property(...); }
                    // ... materialize label + selected props ...
                }
            }
        }
        GValue::Edge(ek) => {
            // Same three-way match. Edge label_id is already in ek — in-memory schema
            // lookup for label string, zero I/O. Only property reads vary by hint.
            match prop_keys { ... }
        }
        _ => { /* Scalars, Lists, Maps, Paths — unchanged */ }
    }
}
```

`ReadTraversal`/`WriteTraversal` store the hint and forward it:

```rust
// gremlin/traversal/mod.rs
impl ReadTraversal<'_> {
    pub fn withProperties(mut self, keys: impl Into<Vec<SmolStr>>) -> Self {
        self.pending_hint = Some(keys.into());
        self
    }
}
```

The hint transfers to `BuiltTraversal.prop_keys` when any terminal method is called.

## Write path consistency

```rust
// A vertex created in this transaction; no property auto-materialized
tx.g().addV("person").property("id", 1i64).property("name", "alice").next()?;

// Explicitly ask for properties after creation
tx.g().withProperties([]).addV("person").property("id", 1i64).property("name", "alice").next()?;

// Read-your-writes still works — addV flushes to LogicalGraph overlay;
// subsequent steps in the same tx see properties regardless of hint
tx.g().addV("person").property("id", 1i64).property("name", "alice")
      .withProperties([]).next()?;
// Wait — withProperties is source-level, after g(), so it can't appear mid-chain.
// The terminal call determines the hint.
```

The write path uses the same convention: default returns no properties, `withProperties([])`
returns all. This is consistent and minimal — the caller always controls what they pay for.

## Implementation order

1. Add `prop_keys: Option<Vec<SmolStr>>` to `BuiltTraversal`
2. Add `withProperties()` to `ReadTraversal`/`WriteTraversal`
3. Thread `prop_keys` into `materialize()` calls from `BuiltTraversal::next()`
4. Implement three-way `match` in `materialize()` — `None` / `Some([])` / `Some(keys)`
5. Update existing callers that relied on eager property materialization (add
   `.withProperties([])` where tests/examples need full properties back)

## Affected files

- `src/gremlin/traversal/mod.rs` — `withProperties()` on `ReadTraversal`/`WriteTraversal`
- `src/gremlin/traversal/built.rs` — `BuiltTraversal.prop_keys`, `materialize()` signature + logic
