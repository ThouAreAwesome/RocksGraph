# Design: Vertex label fast-path via edge value encoding

Status: implemented.

## Goals & non-goals

- **Goals:** Store destination/source vertex labels in edge value prefixes to eliminate vertex value reads during edge traversals; cache labels in per-transaction overlay; provide `LabelOnly` vertex state for zero-I/O label access.
- **Non-goals:** Promote vertex label to the storage key (see `design_vertex_label.md`); change the edge key encoding; eliminate vertex reads for property access (only label reads are optimized).

## Problem

`hasLabel()` on a vertex costs one RocksDB read per traverser —
[`has_label.rs`](../src/engine/volcano/steps/has_label.rs) calls
`ctx.get_value(CanonicalKey::Vertex(vk), LABEL_KEY_ID)`, which loads the vertex
record. `hasLabel()` on an edge is free — the label lives in `EdgeKey::label_id`,
carried in the traverser value itself, no store hit needed.

```
out(label).hasLabel(v_label):
  1. out() scans edges_out CF — efficient (label in key prefix)
  2. Each result is GValue::Vertex(vk) — no label information
  3. hasLabel() reads the vertices CF to get vk's label   ← 1 I/O per result
```

Root cause: `GValue::Edge(EdgeKey)` carries `label_id` as a key field — edge keys
are physically `(label_id, secondary_id, rank)`, so it was already there for free.
`GValue::Vertex(VertexKey)` carries only the id.

## Approach

The destination vertex's label is knowable at edge-write time (see "Write path"
below) and costs nothing extra to read back at edge-scan time once it's stored
alongside the edge — but the question is *where the label surfaces once read*.

Attaching it directly to the traverser value (`GValue::Vertex`) only helps the
specific step that just read the edge. It does nothing for a step that reaches
the same vertex by a different route later in the same query — `outE().inV()`,
`bothE().otherV()`, or even a completely separate `hasLabel()` call on a vertex id
that some earlier step in the same transaction already touched. None of those
have an edge value in hand at the point they need the label.

So instead the label is cached in `LogicalGraph`'s (and `LogicalSnapshot`'s)
existing per-transaction vertex overlay — the same `HashMap<VertexKey, Vertex>`
that already caches fully-loaded vertices to avoid redundant store reads within
one transaction. Every step that ever asks `ctx.get_value(vk, LABEL_KEY_ID)` —
regardless of how it reached `vk` — benefits if *any* earlier edge read in the
same transaction happened to mention that vertex, with **zero change** to the
`GraphCtx` trait, `GValue`, or any engine step. The caching is entirely internal
to `LogicalGraph`/`LogicalSnapshot`.

Three coordinated changes:

1. **Edge value** gets a 2-byte `end_vertex_label_id` prefix — same layout
   `VertexValue` already uses (`[label_id:u16 | property_blob]`). No backward
   compatibility is required (no existing database), so this is unconditional —
   no format detection, no migration.
2. **`vertex_degree` CF** gets a `vertex_label_id: u16` field, set once when the
   vertex is created. `add_edge` already loads both endpoints' `vertex_degree`
   records (existence check + degree counters) — both labels come along for free
   in the same records.
3. **`LogicalGraph.vertices` / `LogicalSnapshot.vertices`** can now hold a vertex
   entry in one of three states instead of two — see "The vertex cache" below.
   `get_adjacent_edges`/`get_edge`/`get_edges` populate a label-only entry for
   whichever vertex they learn about, as a side effect, before returning their
   normal (unchanged) result to the caller.

## Write path: getting both labels onto the edge for free

`add_edge` already calls `get_vertex_degree` for both endpoints to check
existence and bump degree counters
(`LogicalGraph::add_edge` in [`graph/logical.rs`](../src/graph/logical.rs)):

```rust
let (mut src_out, src_in) = self.get_vertex_degree(cek.src_id)?.ok_or(StoreError::NotFound)?;
let (dst_out, mut dst_in) = self.get_vertex_degree(cek.dst_id)?.ok_or(StoreError::NotFound)?;
```

Extending `get_vertex_degree`'s return shape to `(u32, u32, LabelId)` (set once at
`add_vertex` time, in `LogicalGraph::add_vertex` ([`graph/logical.rs`](../src/graph/logical.rs)),
and never updated again — no "change vertex label" operation exists) gives `add_edge`
both `src_label` and `dst_label` with no extra I/O.

Each *physical* edge record only needs one of them: a logical edge is written
twice at commit — once to `edges_out` and once to `edges_in`
(`LogicalGraph::add_edge`'s commit path, [`graph/logical.rs`](../src/graph/logical.rs)) — and for the `edges_out` row the
relevant "other vertex" is `dst`, while for the `edges_in` row it's `src`. So
`Edge` ([`types/element.rs:166`](../src/types/element.rs)) gains two fields:

```rust
pub struct Edge {
    pub src_id: VertexKey,
    pub label_id: LabelId,
    pub dst_id: VertexKey,
    pub rank: Rank,
    pub src_label: Option<LabelId>,
    pub dst_label: Option<LabelId>,
    // ...unchanged raw_props / props
}
```

At creation time (`add_edge`) both are `Some` — both labels are already in hand.

### The commit path is not only reached from `add_edge`

It's tempting to write the commit-time resolution as
`e.dst_label.expect("known at add_edge time")` — but the commit code below runs
for *every* dirty edge, not only ones created by `add_edge` in this
transaction. An edge loaded via `get_edge`/`get_edges`/`get_adjacent_edges` and
then mutated (e.g. `E([key]).property(...)`) reaches this same code with
`Existence::Modified` (or `CounterOnly`/`ModifiedWithCounter`) — and per the
Read path below, `build_lazy_edge` only ever sets *one* of `src_label`/
`dst_label` from a single physical-row read; the other is `None`. An `.expect`
would panic on that path; a silent default would persist a wrong label —
exactly the kind of gap the "Auditing every existing 'is it cached' check"
table below exists to catch for `Vertex`, and the same discipline applies here:

| `Existence` reaching this code | How the `Edge` was built | `src_label`/`dst_label` state |
|---|---|---|
| `New` | `add_edge`, this transaction | Both `Some` — already resolved via `get_vertex_degree` at creation |
| `Modified` / `CounterOnly` / `ModifiedWithCounter` | `get_edge`/`get_edges`/`get_adjacent_edges`, then mutated | Exactly one `Some` (whichever direction was read) — the other `None` |

For the `None` side, the missing label must come from a real, store-backed
lookup — `self.get_vertex_degree(id)` (the same read-through-cache method
`add_edge` already uses above), **not** a raw peek at the `self.vertex_degree`
overlay map. The overlay only has an entry for a vertex if something already
called `get_vertex_degree` on it *in this transaction* — an edge-only
load-then-mutate sequence never does that, so a raw peek finds nothing and any
fallback constant would be silently wrong, indistinguishable from a real label
id 0 or higher:

```rust
let (dst_id, src_id, dst_label, src_label) = {
    let e = self.edges.get(&ek).expect("dirty edge key not in edges");
    (e.dst_id, e.src_id, e.dst_label, e.src_label)
};
let dst_label = match dst_label {
    Some(l) => l,
    None => self.get_vertex_degree(dst_id)?.map(|(_, _, l)| l).ok_or(StoreError::NotFound)?,
};
let src_label = match src_label {
    Some(l) => l,
    None => self.get_vertex_degree(src_id)?.map(|(_, _, l)| l).ok_or(StoreError::NotFound)?,
};
let e = self.edges.get_mut(&ek).expect("dirty edge key not in edges");
self.store.put_edge(&ek.out_key(), dst_label, e.all_props())?;
self.store.put_edge(&ek.in_key(), src_label, e.all_props())?;
```

The first block ends the borrow on `self.edges` *before* calling
`self.get_vertex_degree(...)` (which needs `&mut self`), then re-borrows
`self.edges` afterward for `all_props()` — `e` and a `get_vertex_degree` call
can't be live at the same time.

`put_edge` ([`transaction.rs:439`](../src/store/rocks/transaction.rs)) takes the
extra `end_vertex_label: LabelId` parameter and writes it as the value's 2-byte
prefix.

## Read path: one label per physical row, attributed correctly

```rust
// Current (store/rocks/encoding.rs)
pub struct EdgeValue { pub property_blob: Vec<u8> }

// Proposed — same shape as VertexValue (encoding.rs:230-258)
pub struct EdgeValue {
    pub end_vertex_label: LabelId,
    pub property_blob: Vec<u8>,
}
impl EdgeValue {
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(2 + self.property_blob.len());
        buf.extend_from_slice(&self.end_vertex_label.to_be_bytes());
        buf.extend_from_slice(&self.property_blob);
        buf
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 2 { return None; }
        let end_vertex_label = u16::from_be_bytes(bytes[0..2].try_into().ok()?);
        Some(Self { end_vertex_label, property_blob: bytes[2..].to_vec() })
    }
}
```

`decode()` slices out the label without calling `decode_props` on the rest —
the same separation `VertexValue::decode` already does
([`encoding.rs:250`](../src/store/rocks/encoding.rs)); full property decode still
only happens on first accessor call via `Edge::ensure_decoded`. `encode()` moves
from a borrowed `&[u8]` to an owned `Vec<u8>` — every existing caller already
calls `.to_vec()` on the result immediately
([`transaction.rs:447`](../src/store/rocks/transaction.rs),
[`admin.rs:174`](../src/store/rocks/admin.rs)), so this removes a redundant copy
rather than adding one.

`build_lazy_edge`/`build_full_edge`
([`encoding.rs:391,410`](../src/store/rocks/encoding.rs)) take the *original*
(pre-canonicalization) `EdgeKey` — which still has direction — so they know
unambiguously which endpoint the decoded label belongs to:

```rust
let (src_label, dst_label) = match ek.direction {
    Direction::OUT => (None, Some(ev.end_vertex_label)),
    Direction::IN  => (Some(ev.end_vertex_label), None),
};
```

This is why the label can't simply live on `EdgeKey` itself instead of `Edge` —
see "Why not `EdgeKey`" below.

## The vertex cache: three states instead of two

`Vertex` ([`types/element.rs:67`](../src/types/element.rs)) currently has two
states bundled into `raw_props: Option<(Box<[u8]>, PropDecoder)>`: raw bytes
pending decode, or decoded (possibly via direct construction with `props`
already known). A vertex whose label was learned from an adjacent edge has
neither — no raw bytes were ever read from its own row. This needs a third,
weaker state, added as a sibling flag rather than restructuring the existing two
fields — `raw_props`/`props` and every method that touches them
(`ensure_decoded`, `get_property`, `get_value`, `all_props`, `props_mut`) stay
exactly as they are today:

```rust
pub struct Vertex {
    pub id: VertexKey,
    pub label_id: LabelId,
    /// `true` when only `label_id` is known — learned for free from an
    /// adjacent edge's value. `raw_props`/`props` are both empty placeholders
    /// in this state, not real data — any access beyond `id`/`label` needs a
    /// real fetch first; see "Auditing every existing 'is it cached' check".
    label_only: bool,
    raw_props: Option<(Box<[u8]>, PropDecoder)>,
    props: Vec<Property>,
}

impl Vertex {
    pub fn label_only(id: VertexKey, label_id: LabelId) -> Self {
        Vertex { id, label_id, label_only: true, raw_props: None, props: Vec::new() }
    }
    pub fn is_label_only(&self) -> bool {
        self.label_only
    }
    // with_props / from_raw unchanged except for the new `label_only: false` field.
}
```

`raw_props`/`props` are already private to `types/element.rs` (no `pub`), so the
only code that can ever construct a `Vertex` is the three constructors in that
one file — the invariant "`label_only` agrees with `raw_props`/`props`" only
needs to be kept consistent in one small, rarely-touched place, not enforced
crate-wide. (An enum with a `LabelOnly`/`Raw`/`Decoded` variant set would make
that invariant the compiler's problem instead, at the cost of restructuring the
two existing fields and every method touching them — not worth it here given
how contained the surface already is.)

`get_property`/`get_value` already special-case `ID_KEY_ID`/`LABEL_KEY_ID` and
answer directly from `self.id`/`self.label_id` *before* touching `props` at all
([`element.rs:117,131,144,158`](../src/types/element.rs)) — so those two paths
need no change and already work correctly against a `label_only` entry for
free. `ensure_decoded()` is never reached for those two keys; for any other key
it needs raw bytes that a `label_only` entry doesn't have. `Vertex` itself has
no store handle and can't fetch them — the caller must upgrade the entry first.

### Where the cache is populated

`get_adjacent_edges`/`get_edge`/`get_edges` on `LogicalGraph` and
`LogicalSnapshot` already obtain a fully-decoded `Edge` (with `src_label`/
`dst_label`) before narrowing it down to the `EdgeKey`(s) they return. Add, right
there, a side effect that costs nothing extra:

```rust
fn cache_vertex_label(&mut self, id: VertexKey, label_id: LabelId) {
    self.vertices.entry(id).or_insert_with(|| Vertex::label_only(id, label_id));
}
```

(mirrors the existing "don't clobber a richer entry" pattern already used for
the edges overlay in `LogicalGraph`/`LogicalSnapshot`
(`src/graph/logical.rs`, `src/graph/snapshot.rs`):
`self.edges.entry(cek).or_insert(edge)`.) Called as:

```rust
if let Some(l) = edge.src_label { self.cache_vertex_label(edge.src_id, l); }
if let Some(l) = edge.dst_label { self.cache_vertex_label(edge.dst_id, l); }
```

right after each edge is obtained in `get_adjacent_edges`, `get_edge`, and
`get_edges` — in both `LogicalGraph` ([`graph/logical.rs`](../src/graph/logical.rs))
and `LogicalSnapshot`'s mirrored methods ([`graph/snapshot.rs`](../src/graph/snapshot.rs)).
The methods' return types and the `GraphCtx` trait are untouched; this
is purely an internal side effect.

### Auditing every existing "is it cached" check

This is the part that must be done carefully: several existing call sites treat
"an entry exists in `self.vertices`" as "safe to use directly" or "safe to
mutate directly." A `LabelOnly` entry now also "exists" but is not safe for
either of those without an upgrade first. Each needs to be checked:

All call sites below are duplicated between `LogicalGraph` ([`graph/logical.rs`](../src/graph/logical.rs))
and `LogicalSnapshot` ([`graph/snapshot.rs`](../src/graph/snapshot.rs)); both copies need the fix.

| Call site | Currently checks | Needs to become |
|---|---|---|
| `get_vertex` — existence only | `!contains_key` → fetch | **No change.** A `LabelOnly` entry already correctly proves existence. |
| `get_vertices` — existence only | `!contains_key` → fetch | **No change**, same reasoning. |
| `get_property` / `get_value` | calls `get_vertex` then delegates straight to the cached `Vertex` | For any `prop_key_id` other than `ID_KEY_ID`/`LABEL_KEY_ID`, upgrade first if `is_label_only()` |
| `get_all_props` | calls `get_vertex` then delegates | Always upgrade first if `is_label_only()` — "all" can never be answered from a label-only entry |
| `set_property` (`LogicalGraph` only — write path) | `!contains_key` → fetch, else mutate in place | `!contains_key || is_label_only()` → fetch-and-replace, *then* mutate — otherwise a `props_mut()` call on a `LabelOnly` entry silently treats unread properties as nonexistent and the write would discard them |
| `drop_property` (`LogicalGraph` only — write path) | same pattern as `set_property` | same fix |
| `scan_vertices` batch entry insertion | `self.vertices.entry(vt.id).or_insert(vt)` | **Needs a fix.** `scan_vertices` always does a real fetch (it's a full scan, not conditional on cache state) and `vt` always arrives `Raw`/`Decoded`. But `or_insert` refuses to overwrite *any* existing entry — including a `LabelOnly` one — so a vertex already cached as `LabelOnly` from an earlier edge read stays `LabelOnly` even though the scan just fetched its full record and is about to throw that result away. Not a correctness bug (a later access still upgrades on demand per the rows above) but it silently wastes the read `scan_vertices` already paid for. Needs to upgrade-in-place instead of unconditionally refusing to overwrite: `match self.vertices.entry(vt.id) { Entry::Vacant(e) => { e.insert(vt); } Entry::Occupied(mut e) if e.get().is_label_only() => { e.insert(vt); } Entry::Occupied(_) => {} }` |

The upgrade itself reuses the existing full fetch — no new store primitive:

```rust
fn ensure_vertex_props_loaded(&mut self, id: VertexKey) -> Result<(), StoreError> {
    if self.vertices.get(&id).is_some_and(Vertex::is_label_only) {
        if let Some(fresh) = self.store.get_vertex(id)? {
            self.vertices.insert(id, fresh); // replaces the LabelOnly placeholder
        }
    }
    Ok(())
}
```

### Why not `EdgeKey`

Putting the label on `EdgeKey` instead of as a side-channel on `Edge` would let
`in_v_out_v.rs` (`inV()`/`outV()` pivoting off an edge already in a traverser)
and `other_v.rs` (`otherV()`) benefit too, by recovering it directly from the
`GValue::Edge` already in hand. This is deliberately not done: `EdgeKey` derives
`PartialEq, Eq, Hash` ([`types/keys.rs:166`](../src/types/keys.rs)) — *derived*,
not hand-written — and backs `GValue::Edge`'s equality, which `dedup()`
([`dedup.rs:59`](../src/engine/volcano/steps/dedup.rs)) relies on via
`HashSet<GValue>`. Adding a field to `EdgeKey` would silently join the derived
equality/hash — a vertex/edge reached with a label hint and without would compare
unequal, breaking `bothE().dedup()`, and nothing would flag it at review time
the way a hand-written impl's match arms would. The vertex-cache approach above
gets `in_v_out_v.rs`/`other_v.rs` the same benefit anyway, indirectly: those
steps' upstream `outE()`/`inE()`/`bothE()` call already populated the cache for
both endpoints when it read the edge, regardless of which `GValue` variant the
step ultimately emits.

## What's optimized

Because the cache is keyed by vertex id, not by traversal path, any of these
benefit as long as *some* edge incident to the vertex was read earlier in the
same transaction — not only the step immediately producing the vertex:

| Query pattern | Before | After |
|---------------|:---:|:---:|
| `out(label).hasLabel(v_label)` | 1 read/dst vertex | **0 reads** |
| `both(label).hasLabel(v_label)` | 1 read/dst vertex | **0 reads** |
| `in(label).hasLabel(v_label)` | 1 read/src vertex | **0 reads** |
| `out().out().hasLabel(v_label)` | 1 read/final vertex | **0 reads** |
| `outE().inV().hasLabel(v_label)` | 1 read/vertex | **0 reads** — `outE()`'s own edge read already cached `dst_label` |
| `bothE().otherV().hasLabel(v_label)` | 1 read/vertex | **0 reads**, same reasoning |
| `V([]).hasLabel(v_label)` with no prior edge touching those vertices in this transaction | 1 read/vertex | 1 read/vertex (unchanged — nothing cached it yet) |

## Files changed

| File | Change |
|------|--------|
| `types/element.rs` | `Vertex` gains a `label_only: bool` field alongside the existing (unchanged) `raw_props`/`props`; add `label_only()` constructor and `is_label_only()`. `Edge` gains `src_label: Option<LabelId>`, `dst_label: Option<LabelId>`; `with_props`/`from_raw` take them |
| `store/rocks/encoding.rs` | `EdgeValue` (label prefix), `VertexDegree` (10 bytes), `build_lazy_edge`/`build_full_edge` set `src_label`/`dst_label` from `ek.direction` |
| `store/traits.rs` | `GraphStore::get_vertex_degree`/`put_vertex_degree` signatures (+`LabelId`) and no-op impls |
| `store/rocks/transaction.rs` | `get_vertex_degree`/`put_vertex_degree` impls; `put_edge` takes `end_vertex_label`; commit path passes `dst_label`/`src_label` per direction |
| `store/rocks/admin.rs` | Edge-value write path (admin/test insertion) |
| `graph/logical.rs`, `graph/snapshot.rs` | `vertex_degree` overlay value type → `(u32,u32,LabelId)`; `add_vertex`, `add_edge` (logical.rs only — write path); `get_adjacent_edges`/`get_edge`/`get_edges` gain the `cache_vertex_label` side effect; `get_property`/`get_value`/`get_all_props`/`set_property`/`drop_property` gain the upgrade check per the audit table above — in both `LogicalGraph` and `LogicalSnapshot` |
| Everything engine-facing — `engine/context.rs` (`GraphCtx` trait), `types/gvalue.rs`, `engine/traverser.rs`, `has_label.rs`, `in_out.rs`, `both.rs`, `get_e.rs`, `in_v_out_v.rs`, `other_v.rs`, `v.rs` | **No changes.** The cache is invisible above `LogicalGraph`/`LogicalSnapshot`. |

## Implementation plan

**Phase 1 — `Edge` / `EdgeValue` / `VertexDegree`** (`element.rs`, `encoding.rs`)
1. `Edge` gains `src_label`/`dst_label: Option<LabelId>`; update both
   constructors and `build_lazy_edge`/`build_full_edge` (direction-aware
   assignment) and the `add_edge` call site.
2. `EdgeValue` gains the 2-byte prefix; `encode`/`decode` per the snippet above.
3. `VertexDegree` → 10 bytes, unconditionally.

**Phase 2 — write path** (`graph/logical.rs`, `store/traits.rs`, `transaction.rs`)
4. `add_vertex` stores the label in the `vertex_degree` overlay entry.
5. `get_vertex_degree`/`put_vertex_degree` (trait + impls + overlay map) carry
   `LabelId` alongside the two counters.
6. `add_edge` reads both endpoints' labels from their (already-loaded) degree
   records, sets both on the new `Edge`.
7. Commit path writes the correct per-direction label prefix — for *every*
   `Existence` state that reaches it, not only `New`. Resolve a missing label
   via `self.get_vertex_degree(id)`, never via a raw `self.vertex_degree.get(id)`
   peek with a fallback constant — see "The commit path is not only reached
   from `add_edge`" above. This is the single highest-risk line in the whole
   design: a raw-peek-with-fallback here compiles, passes a casual smoke test,
   and silently corrupts on-disk data only for edges that were loaded and
   mutated without their endpoints' `vertex_degree` ever being touched in the
   same transaction — exactly the kind of gap that doesn't show up until much
   later.

**Phase 3 — the vertex cache** (`types/element.rs`, `graph/logical.rs`, `graph/snapshot.rs`)
8. `Vertex`'s `label_only` field, `label_only()` constructor, `is_label_only()`.
9. `get_adjacent_edges`/`get_edge`/`get_edges` populate `cache_vertex_label` for
   both `LogicalGraph` and `LogicalSnapshot`.
10. Fix `get_property`/`get_value`/`get_all_props`/`set_property`/
    `drop_property` per the audit table — this is the step most likely to be
    half-done by accident; treat the audit table as a checklist, not a summary.

**Phase 4 — tests** — see "Test plan" below; treat it as a checklist, not a
summary, since most of these scenarios exist specifically because they were
*not* obvious from reading the implementation once.

## Constraints / invariants

- Vertex label must never be read through `get_all_props` on the store path — it must come from the edge value prefix or overlay cache.
- `ensure_vertex_props_loaded` must return `CorruptData` if a `LabelOnly`-cached vertex is unexpectedly `None` in the store.
- Edge value encodes `end_vertex_label` as 2-byte big-endian — must not grow with future `LabelId` width changes.

## Test plan

Grouped by what each group of tests is actually defending against, not in
implementation order — several of these were only found by tracing through the
audit table above, not by inspection.

### 1. Read-after-mutate must not lose data (the highest-risk category)

This is the category the upgrade-on-demand logic exists to get right, and the
easiest one to get subtly wrong by fixing only the call site that happens to be
tested first.

1. **Mutate a property reached through a `LabelOnly` entry, then read it back
   in the same transaction.** Traverse an edge to vertex X (caches `LabelOnly`),
   `property(X, "score", 5)`, then immediately `values(X, "score")` in the same
   transaction — must return `5`, not `None` and not trigger a second
   unnecessary fetch.
2. **Mutating one property must not lose a *different*, pre-existing one.**
   Same setup, but X already has a real `"name"` property on disk. After
   `property(X, "score", 5)`, read back `"name"` — must still return its
   original stored value. (This is the scenario `set_property`'s
   upgrade-before-mutate fix in the audit table exists for: skipping the
   upgrade and mutating a `LabelOnly` entry directly would silently treat
   `"name"` as never having existed.)
3. **`drop_property` through a `LabelOnly` entry removes the right property and
   nothing else.** X has real `"name"` and `"temp"` properties on disk, cached
   `LabelOnly` via an edge. `drop_property(X, "temp")`, then read back
   `"temp"` (→ `None`) and `"name"` (→ still present).
4. **The mutation survives commit, not just the in-memory overlay.** Repeat
   scenario 1 or 3, call `commit()`, then re-read X from a *fresh* transaction
   or snapshot — confirms the write actually reached the store and wasn't an
   overlay-only illusion that the next test would have missed.
5. **An edge loaded (not created) in this transaction, then mutated, commits
   with the correct label on *both* physical rows — not the previous one it
   already had, and not a `0` sentinel.** Create and commit an edge X→Y in one
   transaction. In a *fresh* transaction, load it one-directionally —
   `E([key])` or `out()` from X only, never touching Y's `vertex_degree` —
   then `property(...)` it and commit. Re-read both the `edges_out` and
   `edges_in` physical rows from yet another fresh transaction/snapshot: both
   must carry the real labels of their respective "other" vertex. This is the
   test that catches resolving a missing label from a raw cache peek instead
   of a real `get_vertex_degree` fetch (see "The commit path is not only
   reached from `add_edge`").

### 2. The fast path actually engages, including indirectly

5. `out(label).hasLabel(v_label)` answers without a `get_value(LABEL_KEY_ID)`
   store call — assert via a test double / call counter, not just correct
   output (correct output alone doesn't prove the slow path was skipped).
6. `outE().inV().hasLabel(v_label)` and `bothE().otherV().hasLabel(v_label)`
   also skip the store read, despite `inV()`/`otherV()` never touching the edge
   value themselves — this is the test that distinguishes this design from a
   narrower per-traverser-hint approach; it must be the *upstream* `outE()`/
   `bothE()` read that populated the cache.
7. Multi-hop: `out().out().hasLabel()` — the label is correct after the second
   hop, not stale from the first vertex visited.

### 3. The cache never serves stale or incorrect data

8. **A `LabelOnly` placeholder is never created where stronger data already
   exists.** Fully load X via `V([X]).values(...)` (→ `Decoded`), *then*
   traverse an edge to X — confirm the cached entry is still `Decoded`
   afterward, not downgraded. (`cache_vertex_label`'s `or_insert_with` should
   guarantee this; test it rather than trust it.)
9. **A tombstoned vertex is never served from a stale cache entry.** Traverse
   an edge to X (caches `LabelOnly`), then `drop_vertex(X)` later in the same
   transaction (only reachable if X has no remaining incident edges — set up
   accordingly), then `hasLabel()`/existence-check X — must report "not found,"
   not silently answer from the leftover `LabelOnly` entry. The dirty/tombstone
   check is tracked separately from the `vertices` map and should already
   cover this, but the interaction is new enough to deserve an explicit test
   rather than an assumption.

### 4. `scan_vertices` upgrades instead of wasting its own read

10. Traverse an edge to X (caches `LabelOnly`), then run a `V([])`-style full
    scan that passes over X in the same transaction, then access a non-label
    property on X — confirm it does *not* trigger a second store fetch (i.e.
    `scan_vertices` upgraded the entry in place rather than discarding the
    fetch it had just done, per the fix in the audit table).

### 5. No cross-transaction state leakage

This category exists because of the `TRACK_PATH` global-static bug found
earlier in this codebase's history — worth checking this design doesn't
introduce a quieter version of the same class of bug, given it also touches
per-transaction caching.

11. **Two concurrent transactions don't see each other's cache.** Transaction A
    traverses an edge to X, caching it `LabelOnly`. Transaction B (a separate
    `LogicalGraph`/`LogicalSnapshot` instance against the same underlying
    store, run concurrently or interleaved) reads X's label — must do its own
    correct store read (or its own correct cache population), never observing
    or being affected by A's in-memory state. This should be trivially true
    since the cache is a plain field on a per-transaction struct, not a
    global — but it's exactly the kind of invariant worth a test given the
    precedent.
12. **Cache state doesn't leak across a `Transaction` reuse.** `commit()`
    resets and reuses the same `Transaction`/`LogicalGraph` instance
    (`self.vertices.clear()` in [`graph/logical.rs`](../src/graph/logical.rs) and
    [`graph/snapshot.rs`](../src/graph/snapshot.rs)).
    Populate a `LabelOnly` entry, commit, then in the *same* reused instance
    immediately read a different vertex — confirm no leftover entry from the
    previous transaction is visible (`vertices.clear()` already does this; the
    test exists to catch a future refactor that accidentally skips it).

### 6. Defensive behavior under an unexpected store result

13. **`ensure_vertex_props_loaded`'s fetch unexpectedly returns `None` for a
    `LabelOnly`-cached vertex.** Should be unreachable in practice — a vertex
    referenced by a live edge can't be dropped while that edge exists, by the
    existing degree-counter invariant — but the behavior should be a
    deliberate decision (error out, e.g. `StoreError::CorruptData`) rather than
    silently falling through to "answer as if zero properties exist." Pick the
    behavior during implementation and write the test against that decision,
    don't leave it to whatever the code happens to do.

### 7. Regression

14. Full existing test suite.

## Net change to core structures

| Structure | Change |
|-----------|--------|
| `Vertex` (element.rs) | `+1` field (`label_only: bool`); `raw_props`/`props` unchanged |
| `Edge` (element.rs) | `+2` fields (`src_label`, `dst_label: Option<LabelId>`) |
| `VertexDegree` (encoding.rs) | `+2` bytes (8 → 10), unconditional |
| `EdgeValue` (encoding.rs) | `+2` byte prefix, unconditional; `encode()` returns owned `Vec<u8>` instead of `&[u8]` |
| `LogicalGraph` / `LogicalSnapshot` | `vertex_degree` overlay value: `(u32,u32)` → `(u32,u32,LabelId)`; new internal `cache_vertex_label`/`ensure_vertex_props_loaded` helpers |
| `GraphCtx` trait | **0 changes** |
| `GValue` | **0 changes** |
| `Traverser` | **0 changes** |
| `EdgeKey` | **0 changes** (deliberately — see "Why not `EdgeKey`") |
