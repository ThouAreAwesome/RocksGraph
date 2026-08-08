# Design: edge `id()` → canonical key string + `g.E([string])`

## Problem

Edge `id()` currently returns `primary_id` (the source vertex), which is **not globally
unique** — two edges from vertex 1 with different labels or destinations both have
`id = 1`.  This makes `id()` meaningless for edge disambiguation and prevents
`g.E([edge_id_string])` from being implemented correctly.

`CanonicalEdgeKey { src_id, label_id, dst_id, rank }` already defines a direction-free,
globally-unique edge identity.  It just needs a stable string representation and a
public API for round-tripping.

## Goals & non-goals

- **Goals:** Edge `id()` returns a compact, round-trippable string of the
  `CanonicalEdgeKey`; `g.E([string, ...])` parses those strings back to look up edges;
  `hasId()` works with edge-id strings; `Value::Edge` carries the canonical id string.
- **Non-goals:** Change vertex `id()` (stays `i64`); add a separate `EdgeId` type
  (the string IS the id); support legacy `primary_id`-based lookups (forcing
  backward-compat with old edge id values).

## Design

### 1. String encoding: Base64 (URL-safe, no padding) of the raw 22 bytes

`CanonicalEdgeKey` is packed as `[src_id:8B][label_id:4B][dst_id:8B][rank:2B]`
big-endian (22 bytes total), then encoded with the URL-safe Base64 alphabet and no
`=` padding. Fixed **30-character** length regardless of field values — printable,
copy-pasteable, and trivially round-trippable — but deliberately opaque: it does not
read as "this edge connects src and dst" the way a decimal-composite format would,
so it doesn't invite callers to string-split it or treat it as anything other than
an opaque token.

```
"AAAAAAAAAAEAAAADAAAAAAAAAAIAAA"  →  src=1, label=3 (knows), dst=2, rank=0
"AAAAAAAAAAEAAAAEAAAAAAAAAAMAAA"  →  src=1, label=4 (created), dst=3, rank=0
```

**Alternatives considered:**
- Colon-separated decimal (`"{src_id}:{label_id}:{dst_id}:{rank}"`) — short for
  small test-scale ids (e.g. `"1:3:2:0"`), but unbounded: up to ~56 characters for
  large or negative `i64` vertex ids, with no fixed upper bound. Also invites
  callers to string-split on `:`, treating the id as a decomposable composite of
  its endpoints rather than an opaque identity — undermining the whole point of
  giving edges a "real," independent id. Rejected.
- Hex-encoded big-endian bytes — same information as Base64, strictly longer (44
  chars vs. 30) for no benefit, and still visually suggests "these are 4 packed
  numbers." Base64 dominates hex on every axis here. Rejected.
- Human-readable `"({src} -{label}-> {dst})[rank={rank}]"` (current `Display`) —
  not round-trippable (no `FromStr`), and `label` is a raw `LabelId`, not a
  resolved name, so it isn't actually more legible than any other option either.
  Rejected.
- Binary blob: `[u8; 22]` — tightest, but can't pass as a string through the
  public API at all. Rejected.

**Chosen: Base64 (URL-safe alphabet, no padding) of the raw 22-byte
`CanonicalEdgeKey`** — fixed 30-character length regardless of id magnitude,
printable, copy-pasteable, round-trippable, and deliberately opaque.

#### Heap allocation: unavoidable in the string itself, avoidable in the filter path

`SmolStr` (pinned at `0.2.2`) inlines up to **23 bytes**; the 30-character Base64
string exceeds that, so every `to_id_string()` call heap-allocates. No denser
printable encoding fixes this: fitting 22 bytes (176 bits) into ≤23 characters needs
≥7.65 bits/char, but even a maximal 94-symbol printable alphabet only reaches ~6.55
bits/char — 27 characters minimum, still over the inline cap. Shrinking the 22-byte
payload isn't free either: `src_id`/`dst_id` are `VertexKey = i64`, `label_id` was
deliberately widened to `i32` (`design_widen_label_id.md`), and `rank` is
deliberately `u16` (`design_multiple_edges.md`) — truncating any of them to claw
back bytes risks silently reintroducing id collisions, the exact bug this design
exists to fix. **One heap allocation per produced edge-id string is accepted as
unavoidable.**

What *is* avoidable is paying that allocation in `HasIdStep`'s filter loop, which
runs once per **upstream traverser**, not once per **matched result** — stringifying
every edge just to do a string comparison would allocate even for traversers that
don't match. §6 below fixes this by resolving the predicate's string operand(s) into
`CanonicalEdgeKey` once, at construction, and comparing structurally per traverser
instead, with zero allocation in the hot loop. `IdStep` (§3) and `materialize_edge()`
(§5) keep the allocation — they run once per edge actually returned/materialized to
the caller, the same cost class as any other string-valued output, not a hot filter
loop.

### 2. `EdgeKey::to_id_string()` + `CanonicalEdgeKey::FromStr`

Add `base64` as a **direct** dependency in `Cargo.toml` (the `base64` crate is
already pulled in by `hdrhistogram` and resolves to `0.21.7`; pin to that same
major line — but you still must list it as a direct dependency to `use` it).

`EdgeKey` gets the `to_id_string()` method (callers hold `GValue::Edge(EdgeKey)`,
so this is the natural entry point).  `CanonicalEdgeKey` gets `FromStr` for the
reverse direction (parsing id strings back to keys inside `g.E([...])`):

```rust
// src/types/keys.rs
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

impl EdgeKey {
    /// Stable, globally-unique string id: Base64 (URL-safe, no padding) of
    /// the packed `CanonicalEdgeKey`.  Fixed 30-character length.
    #[inline]
    pub fn to_id_string(&self) -> String {
        self.canonical_edge_key().to_id_string()
    }
}

impl CanonicalEdgeKey {
    /// Encode `self` as a Base64 string of `[src_id:8B][label_id:4B][dst_id:8B][rank:2B]`
    /// big-endian (22 bytes → 30 chars).
    pub fn to_id_string(&self) -> String {
        let mut buf = [0u8; 22];
        buf[0..8].copy_from_slice(&self.src_id.to_be_bytes());
        buf[8..12].copy_from_slice(&self.label_id.to_be_bytes());
        buf[12..20].copy_from_slice(&self.dst_id.to_be_bytes());
        buf[20..22].copy_from_slice(&self.rank.to_be_bytes());
        URL_SAFE_NO_PAD.encode(buf)
    }
}

impl FromStr for CanonicalEdgeKey {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = URL_SAFE_NO_PAD.decode(s).map_err(|_| {
            StoreError::UnexpectedDataType(format!("invalid edge id '{}': not valid base64", s))
        })?;
        if bytes.len() != 22 {
            return Err(StoreError::UnexpectedDataType(format!(
                "invalid edge id '{}': expected 22 decoded bytes, got {}",
                s, bytes.len()
            )));
        }
        Ok(CanonicalEdgeKey {
            src_id: i64::from_be_bytes(bytes[0..8].try_into().unwrap()),
            label_id: i32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            dst_id: i64::from_be_bytes(bytes[12..20].try_into().unwrap()),
            rank: u16::from_be_bytes(bytes[20..22].try_into().unwrap()),
        })
    }
}
```

### 3. `IdStep` returns the canonical string for edges

```rust
// src/engine/volcano/steps/id_step.rs — produce()

let id_value = match &t.value {
    GValue::Vertex(vk) => GValue::Scalar(Primitive::Int64(*vk)),
    GValue::Edge(ek) => GValue::Scalar(Primitive::String(ek.to_id_string().into())),
    _ => return Ok(Some(smallvec![t])),
};
```

### 4. `g.E([...])` accepts string ids — `String`, not `SmolStr`

`Item = String` (concrete), not `impl Into<SmolStr>` (generic). Two reasons:

- **`E([])` must keep working** as "scan all edges," matching `V([])`/`values([])`'s
  established `[] = all` convention. A generic `impl Into<SmolStr>` bound makes the
  array's element type unconstrained when called with a literal `[]` — confirmed via
  `rustc`: `E0283: cannot infer type for type parameter`. A *concrete* `Item` type
  (exactly what makes `V([])` infer fine today, since `VertexKey = i64` is concrete)
  resolves the same way for `E`.
- **`SmolStr` buys nothing here anyway.** Edge ids are always exactly 30 Base64
  characters — past `SmolStr`'s 23-byte inline cap (see "Heap allocation" above) —
  so the string is heap-allocated regardless of which type carries it. `.id()`
  already returns `Value::String(String)` to callers, so `String` is also what a
  caller naturally already has in hand after capturing an id, with no conversion.

The cost: a bare `&str` literal no longer converts implicitly — `E(["abc"])` needs
`E(["abc".to_string()])`. Acceptable, since the realistic calling pattern is passing
back a `String` already captured from `.id()`, not a hand-typed literal.

```rust
// src/gremlin/traversal/mod.rs

/// g.E(["AAAAAAAAAAEAAAADAAAAAAAAAAIAAA".to_string()]) — look up by canonical id
/// string. g.E([]) — empty = scan all edges (same convention as V([])).
fn E(mut self, keys: impl IntoIterator<Item = String>) -> Self {
    self.push_step(LogicalStep::E(EStep {
        keys: keys.into_iter().collect(),
    }));
    self
}
```

The physical `EStep` resolves each string to a `CanonicalEdgeKey`, then to an `EdgeKey`
(`OUT` direction, since canonical is always OUT-oriented) for store lookup.
Strings that fail to parse are **silently skipped** — `g.E(["valid_id", "garbage", "valid_id2"])`
returns only the valid edge(s):

```rust
// src/engine/volcano/steps/e.rs — produce(), keys branch

while self.buffer_idx < self.keys.len() {
    let key_str = &self.keys[self.buffer_idx];
    self.buffer_idx += 1;
    if let Ok(cek) = key_str.parse::<CanonicalEdgeKey>() {
        let ek = EdgeKey {
            primary_id: cek.src_id,
            direction: Direction::OUT,
            label_id: cek.label_id,
            secondary_id: cek.dst_id,
            rank: cek.rank,
        };
        let edges = ctx.get_edges(&[ek])?;
        if !edges.is_empty() {
            return Ok(Some(smallvec![edges.into_iter()
                .map(|e| Traverser::new_rc(GValue::Edge(e)))
                .collect::<SmallVec<_>>()]));
        }
    }
    // malformed string → skip, try next key
}
return Ok(None);
```

### 5. `materialize_edge()` uses the canonical id string

```rust
// src/gremlin/traversal/built.rs

fn materialize_edge(...) -> Result<Value, StoreError> {
    let cek = ek.canonical_edge_key();
    match prop_keys {
        None => Ok(Value::Edge(UserEdge {
            id: ek.to_id_string(),          // NEW — added alongside existing fields
            out_v: cek.src_id,              // unchanged
            in_v: cek.dst_id,               // unchanged
            label: cache.edge_label(ek.label_id).clone(),
            // ... other fields unchanged ...
        })),
    }
}
```

### 6. `HasIdStep` supports edge-id matching — without allocating per traverser

Naively, this would extract the canonical key string from every edge traverser and
evaluate the predicate against it — but that allocates on *every* `produce()` call
(see "Heap allocation" in §1), including for traversers that never match. Instead,
`pred`'s string operand(s) are parsed into `CanonicalEdgeKey` once, at construction,
and the per-traverser comparison is a plain struct comparison — no string, no
allocation:

```rust
// HasIdStep — resolve once at construction, compare structurally per traverser
pub struct HasIdStep {
    upstream: Option<StepRef>,
    pred: PrimitivePredicate,
    /// `pred`'s `Primitive::String` operand(s) pre-parsed into `CanonicalEdgeKey`,
    /// mirroring `pred`'s shape (`Eq`/`Ne`/`Within`/`Without`). A target string that
    /// fails to parse becomes "never matches" rather than a build-time error — same
    /// semantics as `hasId()` on a vertex id that doesn't exist. `None` when `pred`
    /// has no string operands (i.e. this filter only ever sees vertices).
    edge_pred: Option<CanonicalKeyPredicate>,
}

impl HasIdStep {
    pub fn new(pred: PrimitivePredicate) -> Self {
        let edge_pred = CanonicalKeyPredicate::try_from_primitive(&pred); // parses once
        Self { upstream: None, pred, edge_pred }
    }
}

// HasIdStep::produce()
match &t.value {
    GValue::Vertex(vk) => {
        if self.pred.evaluate(&Primitive::Int64(*vk)) {
            return Ok(Some(smallvec![t]));
        }
    }
    GValue::Edge(ek) => {
        if let Some(edge_pred) = &self.edge_pred {
            if edge_pred.matches(&ek.canonical_edge_key()) {
                return Ok(Some(smallvec![t]));
            }
        }
    }
    _ => {}
}
```

### 7. `Value::Edge` gains an `id: String` field

The public `Edge` struct keeps `out_v`, `in_v`, `label`, `rank`, and `properties`
unchanged, and adds `id: String` — the new canonical Base64 edge id string.

## Constraints / invariants

- Edge id string format — Base64 (URL-safe alphabet, no padding) of
  `[src_id:8B][label_id:4B][dst_id:8B][rank:2B]` big-endian — is **stable**: alphabet,
  padding, byte order, and field widths must never change once shipped, since
  changing any of them changes the string for every existing edge id.  (The `Display`
  impl for debug output is free to change independently.)
- `label_id` packed into the id is a numeric `LabelId`, NOT the label name — label names
  can be renamed or resolved differently across schemas; numeric ids are immutable.
- The id string is deliberately opaque: nothing in the public API should encourage
  callers to decode or string-split it themselves — `from_str`/`to_id_string` are the
  only sanctioned round-trip path.
- `CanonicalEdgeKey::from_str` must reject malformed input, not return wrong results.
  The `EStep` call site catches parse failures and silently skips the bad key — no
  error propagates to the caller.
- Edge id lookup always uses `Direction::OUT` — the canonical form is OUT-oriented
  by definition; the same `CanonicalEdgeKey` can also produce an `in_key()` if needed
  for reverse-direction lookups.
- `HasIdStep` must not call `to_id_string()` (or otherwise allocate) inside
  `produce()`'s per-traverser loop — the predicate's string operand(s) are parsed
  into `CanonicalEdgeKey` once, at construction, and matched structurally per
  traverser. `CanonicalEdgeKey` already derives `Copy, PartialEq, Eq`, so this
  comparison is free.

## Files changed

| File | Change |
|------|--------|
| `Cargo.toml` | Add `base64 = "0.21"` as a direct dependency |
| `src/types/keys.rs` | `EdgeKey::to_id_string()` (inline wrapper), `CanonicalEdgeKey::to_id_string()` (Base64 encode), `FromStr` impl (Base64 decode) |
| `src/engine/volcano/steps/id_step.rs` | Edge branch calls `ek.to_id_string()` |
| `src/engine/volcano/steps/e.rs` | Accept string keys, parse to `CanonicalEdgeKey` → `EdgeKey` |
| `src/engine/volcano/steps/has_id.rs` | Pre-parse `pred`'s string operand(s) into `CanonicalEdgeKey` once in `new()`; match edge traversers structurally in `produce()` (no per-traverser allocation) |
| `src/planner/logical_step/mod.rs` | `EStep.keys` type change: `SmallVec<[EdgeKey; 4]>` → `SmallVec<[String; 4]>` |
| `src/engine/volcano/builder/build_step.rs` | `EStep` wiring: resolve string keys |
| `src/gremlin/traversal/built.rs` | `materialize_edge()` adds `id` field via `ek.to_id_string()` |
| `src/gremlin/value.rs` | `Edge` struct: add `id: String` field alongside `out_v`, `in_v`, etc. |
| `src/gremlin/traversal/mod.rs` | `E()` signature: accept string keys |
| `src/gremlin/tests.rs` | e2e tests |
| `src/graph/tests.rs` | Update any tests referencing `Edge.id` |

## Implementation plan

1. Add `base64` as a direct dependency (`Cargo.toml`); add `EdgeKey::to_id_string()`,
   `CanonicalEdgeKey::to_id_string()`, and `CanonicalEdgeKey::FromStr` in `keys.rs`.
2. Add `id: String` field to `Edge` struct (`value.rs`); update `materialize_edge` to
   populate it via `ek.to_id_string()`, keeping `out_v`/`in_v` unchanged.
3. Change `EStep.keys` to `SmallVec<[String; 4]>` (concrete `Item = String` on the
   `E()` builder, not `impl Into<SmolStr>` — required so `E([])` still infers
   without an explicit type annotation, same as `V([])`); resolve in build_step;
   parse in EStep.
4. Update `IdStep` edge branch to emit the canonical string.
5. Update `HasIdStep` to pre-parse `pred`'s string operand(s) into `CanonicalEdgeKey`
   once at construction, and match edge traversers structurally — no per-traverser
   allocation.
6. Update `E()` builder signature.
7. Fix all compilation errors in tests and benchmarks.
8. Add e2e tests: `g.E([edge_id_string])`, `g.V().outE().id()`, `g.E([...]).hasId(...)`.

## Test plan

1. **Round-trip `to_id_string()` → `from_str()`**. Serialize and deserialize a range
   of `CanonicalEdgeKey` values; assert equality.
2. **`g.E([id_string])` returns the correct edge**. Create an edge, capture its id
   via `.id()`, then look it up with `g.E([captured_id])`; assert success.
3. **`g.V().outE().id()` returns unique strings**. All edge ids from a vertex are
   distinct and parseable back to `CanonicalEdgeKey`.
4. **`g.E([bogus_string])` returns empty**. Badly formatted strings are silently
   skipped — no error, no crash, just an empty result batch.
5. **`hasId` on edge traversers**. `g.V().outE().hasId(edge_id_string)` matches
   the correct edge.
6. **Regression: existing `g.E()` tests still pass** with updated fixture types.
