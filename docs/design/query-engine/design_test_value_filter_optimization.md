# Design: `test_value` — zero-clone property predicate evaluation

Status: proposal

## Problem

`HasPropertyStep` calls `ctx.get_value(key, prop_key_id)` to retrieve a property
value and then evaluates a predicate against it:

```rust
// has_property.rs — current hot path
if let Some(vl) = ctx.get_value(&key, self.prop_key_id)? {
    if self.pred.evaluate(&vl) {
        batch.push(t);
    }
}
```

`ctx.get_value` is backed by `PropertyMap::get_value`:

```rust
pub(crate) fn get_value(&self, key: u16) -> Option<Primitive> {
    match self {
        PropertyMap::Blob(bytes) => prop_codec::decode_prop_by_key(bytes, key),
        PropertyMap::Map(map)   => map.get(&key).cloned(),   // ← unnecessary clone
        PropertyMap::LabelOnly  => None,
    }
}
```

For `PropertyMap::Map` (elements that have been mutated in the current transaction),
`map.get(&key)` returns `Option<&Primitive>`. The `.cloned()` call exists solely
because the return type `Option<Primitive>` requires an owned value. The clone is
unnecessary: `HasPropertyStep` only evaluates a predicate (`&Primitive → bool`) and
immediately drops the value. Nothing is stored.

For `PropertyMap::Blob` (the common case for read-only traversals) the clone is
unavoidable — the `Primitive` is decoded fresh from raw bytes on every call.

`String` and `Vec<u8>` properties in Map state allocate on the heap for each
`has()` filter evaluation. For Int32/Int64/Bool the clone is free (Copy types),
but for any long string or binary property it is a real cost.

## Goals & non-goals

- **Goal:** eliminate the Map-state clone in `HasPropertyStep` without changing the
  existing `get_value` API or introducing lifetime complexity.
- **Goal:** zero allocation for Map-state predicate checks on any `Primitive` type.
- **Non-goal:** eliminate the Blob-state decode cost (unavoidable — there is no
  pre-existing `Primitive` to reference).
- **Non-goal:** change `ValuesStep` or `materialize` — those paths need an owned
  `Primitive` to emit into the traversal pipeline; the existing `get_value` is correct.
- **Non-goal:** replace `get_value` with `Cow<'_, Primitive>` — the lifetime
  complexity and wide API change are not justified given the narrow benefit.

## Existing code to touch

| File | Change |
|---|---|
| `src/types/element.rs` | Add `test_value` to `PropertyMap`, `Vertex`, `Edge` |
| `src/graph/logical.rs` | Add `test_value` to `LogicalGraph` |
| `src/graph/snapshot.rs` | Add `test_value` to `LogicalSnapshot` |
| `src/engine/context.rs` | Add `test_value` to `GraphCtx` trait + `NoopCtx` impl |
| `src/engine/volcano/steps/has_property.rs` | Use `ctx.test_value` instead of `ctx.get_value` |

## Design

### `PropertyMap::test_value`

Add a closure-based method alongside the existing `get_value`:

```rust
// types/element.rs — PropertyMap
pub(crate) fn test_value<F>(&self, key: u16, f: F) -> bool
where
    F: FnOnce(&Primitive) -> bool,
{
    match self {
        PropertyMap::LabelOnly => false,

        // Blob: decode into a temporary, pass &ref to closure, drop it.
        // No clone escapes — the Primitive lives only for the duration of f.
        PropertyMap::Blob(bytes) => {
            prop_codec::decode_prop_by_key(bytes, key).map_or(false, |v| f(&v))
        }

        // Map: borrow the stored value, pass &ref directly — no clone.
        PropertyMap::Map(map) => {
            map.get(&key).map_or(false, f)
        }
    }
}
```

The key property: for `Map`, `map.get(&key)` returns `Option<&Primitive>`. The
reference is passed directly into `f` — the `Primitive` is never cloned.

### `Vertex::test_value` and `Edge::test_value`

Thin wrappers that intercept reserved keys (id / label / rank), exactly mirroring
the existing `get_value` pattern:

```rust
// Vertex
pub fn test_value<F>(&self, prop_key_id: u16, f: F) -> bool
where
    F: FnOnce(&Primitive) -> bool,
{
    use crate::types::prop_key::{ID_KEY_ID, LABEL_KEY_ID};
    if prop_key_id == ID_KEY_ID {
        return f(&Primitive::Int64(self.id));
    }
    if prop_key_id == LABEL_KEY_ID {
        return f(&Primitive::Int32(self.label_id));
    }
    self.props.test_value(prop_key_id, f)
}
```

### `GraphCtx::test_value`

New method on the trait, with a default implementation that falls back to
`get_value` for backwards compatibility with any external implementors:

```rust
// engine/context.rs
fn test_value(
    &mut self,
    key: &CanonicalKey,
    prop_key_id: u16,
    f: &dyn Fn(&Primitive) -> bool,
) -> Result<bool, StoreError> {
    Ok(self.get_value(key, prop_key_id)?.map_or(false, |v| f(&v)))
}
```

`LogicalGraph` and `LogicalSnapshot` override this with efficient implementations
that call `vertex.test_value(prop_key_id, f)` directly without materialising an
owned `Primitive`.

Note: the closure is `&dyn Fn` (not `impl FnOnce`) because `dyn GraphCtx` requires
object-safe methods. A `&dyn Fn` is object-safe; a generic `F: FnOnce` is not.

### `HasPropertyStep` call site

```rust
// Before
if let Some(vl) = ctx.get_value(&key, self.prop_key_id)? {
    let vl = self.decode_if_label(ctx, &key, vl);
    if self.pred.evaluate(&vl) {
        batch.push(t);
    }
}

// After
let pred = &self.pred;
let key_ctx = &key;
let prop_key_id = self.prop_key_id;
if ctx.test_value(&key, prop_key_id, &|v| {
    let decoded = self.decode_if_label_ref(ctx_schema, key_ctx, v);
    pred.evaluate(decoded.as_ref())
})? {
    batch.push(t);
}
```

`decode_if_label` currently takes ownership of the `Primitive` to perform label
decoding (Int32 → String). It needs a companion `decode_if_label_ref` that borrows
the `Primitive` and returns a `Cow<'_, Primitive>` to avoid allocating when no
decoding is needed (non-label keys, which is the common case).

## Constraints / invariants

- `get_value` is **not removed or changed** — `ValuesStep`, `materialize_vertex`,
  and `materialize_edge` continue using it.
- `test_value` must not be used where the value needs to be owned after the call.
- The `decode_if_label` path (converting raw `label_id: Int32` to `String`) still
  allocates a `SmolStr` — that allocation is inherent to the label-decode step and
  is outside the scope of this optimisation.

## Performance impact

| Path | Before | After |
|---|---|---|
| `has()` filter, Map state, scalar property | `clone()` (free, Copy) | `&ref` (same or better) |
| `has()` filter, Map state, String property > 22B | heap allocation | zero allocation |
| `has()` filter, Map state, `Bytes` property | heap allocation | zero allocation |
| `has()` filter, Blob state | decode (same) | decode (same) |
| `values()`, any state | unchanged — still uses `get_value` | unchanged |

The optimisation is most visible on elements in Map state (mutated in the current
transaction) with large string or binary property values. For the typical read-only
traversal path, elements are in Blob state and the improvement is structural only
(no clone was happening in the Map branch anyway, since the Blob path always decodes).

## Test plan

### `HasPropertyStep` — zero-clone path

- Unit test: construct a `Vertex` with `PropertyMap::Map` containing a long string
  property, call `test_value` with a predicate, assert the predicate ran with the
  correct value without the map being mutated.

### `HasPropertyStep` — Blob path unchanged

- Existing `has()` integration tests continue to pass; no behaviour change.

### Predicate correctness

- All existing `has(key, pred)` integration tests in `graph/tests.rs` pass without
  modification — the semantics are identical, only the clone is eliminated.

### Label-key handling

- `has("label", "person")` continues to work correctly: the label decode path is
  exercised via `decode_if_label_ref`.
