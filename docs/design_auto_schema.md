# Design: Auto-Schema

## Problem

The engine has a `Schema` struct ([`src/schema/definition.rs`](../src/schema/definition.rs)) that
maps `LabelId ↔ String` and `PropKeyId ↔ String`, but it is **completely disconnected** from the
rest of the system:

- The traversal API takes raw `u16` label IDs: `g.addV(42u16)` — the caller must manage
  the mapping themselves with no engine support.
- `values("label")` returns `Primitive::Int32(label_id as i32)` — a raw number with no
  semantic meaning to the caller.
- `Schema` is never written to or read from RocksDB; it evaporates on restart.
- `schema/mod.rs` declares `mod index` and `mod validator` which do not exist yet.

The result: the schema layer is a stub. Users must do their own string↔ID bookkeeping,
which defeats the purpose of having a schema registry at all.

---

## Goal

**Auto-schema**: labels and property keys are registered automatically on the first write
that introduces them. The user never calls `register_vertex_label` manually; they just write
`g.addV("person")` and the engine handles the rest. The schema is durable — it survives
restarts. An optional strict mode (Phase 2) allows production deployments to reject unknown
labels at write time.

---

## Design

### 1. Schema CF on disk

Add a new `schema` column family alongside `vertices`, `edges_out`, `edges_in`, and
`vertex_degree`. Each entry is a single label or property-key registration:

```
Key:   [ kind:u8 | name:UTF-8 bytes ]
Value: [ id:u16 ]
```

`kind` discriminant values:

| Value | Meaning |
|-------|---------|
| `0`   | vertex label |
| `1`   | edge label   |
| `2`   | property key |

This format is append-only and crash-safe: a schema entry is either fully written or
not present; there is no partial-write risk for small values.  A full schema scan at
startup costs one sequential read over a typically tiny CF.

On-disk encoding helpers live in `src/store/rocks/encoding.rs`:

```rust
pub const CF_SCHEMA: &str = "schema";

pub const SCHEMA_KIND_VERTEX_LABEL: u8 = 0;
pub const SCHEMA_KIND_EDGE_LABEL:   u8 = 1;
pub const SCHEMA_KIND_PROP_KEY:     u8 = 2;

pub fn encode_schema_key(kind: u8, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + name.len());
    key.push(kind);
    key.extend_from_slice(name.as_bytes());
    key
}

pub fn encode_schema_value(id: u16) -> [u8; 2] {
    id.to_be_bytes()
}
```

### 2. Schema load on open

`RocksStorage::open()` scans the schema CF after opening all column families and returns
the populated `Schema` to the caller:

```rust
impl RocksStorage {
    fn load_schema(&self) -> Result<Schema, StoreError> {
        let cf = self.db.cf_handle(CF_SCHEMA)...;
        let mut schema = Schema::new();
        for (k, v) in self.db.iterator_cf(&cf, IteratorMode::Start) {
            let kind = k[0];
            let name = std::str::from_utf8(&k[1..])?;
            let id   = u16::from_be_bytes(v[0..2].try_into()?);
            match kind {
                SCHEMA_KIND_VERTEX_LABEL => { schema.vertex_labels.insert(id, name.into()); }
                SCHEMA_KIND_EDGE_LABEL   => { schema.edge_labels.insert(id, name.into()); }
                SCHEMA_KIND_PROP_KEY     => { schema.prop_keys.insert(id, name.into()); }
                _ => {} // forward-compatible: ignore unknown kinds
            }
        }
        Ok(schema)
    }
}
```

The caller wraps the result in `Arc<RwLock<Schema>>` and passes it into
`LogicalGraph` (see §4).

### 3. Schema persistence on register

When a new label is first seen at write time, it is persisted to the schema CF in the
**same `WriteBatch` as the data write** — the schema entry and the vertex/edge record land
atomically. There is no window where a label ID exists in memory but not on disk.

`Schema` gains a new method that returns the key/value bytes to be included in a batch:

```rust
impl Schema {
    /// Registers a new vertex label and returns the bytes to write to the schema CF.
    /// Returns `(id, Some((key_bytes, value_bytes)))` for a new label,
    /// or `(existing_id, None)` if already registered (no write needed).
    pub fn register_vertex_label_bytes(
        &mut self,
        name: &str,
    ) -> Option<(LabelId, Option<(Vec<u8>, [u8; 2])>)> {
        if let Some(&id) = self.vertex_labels.get_by_right(name) {
            return Some((id, None));
        }
        if self.vertex_labels.len() >= MAX_LABELS { return None; }
        let id = self.vertex_labels.len() as u16;
        self.vertex_labels.insert(id, name.into());
        let key = encode_schema_key(SCHEMA_KIND_VERTEX_LABEL, name);
        let val = encode_schema_value(id);
        Some((id, Some((key, val))))
    }
    // mirror for edge labels and prop keys
}
```

`LogicalGraph::add_vertex` calls this under a write lock, then adds the schema bytes to
the same batch it uses for the vertex record.

### 4. Threading Schema through the stack

`Schema` is shared across concurrent traversals. The owner is the top-level graph handle
(`RocksGraph` or equivalent). Everything below it receives an `Arc<RwLock<Schema>>`.

```
RocksGraph
  schema: Arc<RwLock<Schema>>
  ↓ passed into each LogicalGraph / LogicalSnapshot at construction
LogicalGraph<S>
  schema: Arc<RwLock<Schema>>
  ↓ accessed via GraphCtx
GraphCtx trait
  fn schema(&self) -> &Arc<RwLock<Schema>>;
```

The `RwLock` is acquired for **writing** only when a new label is introduced at mutation
time (rare). All reads (label resolution, `values("label")`) take the read lock (shared,
non-blocking under concurrent reads).

### 5. Traversal API: string labels

The traversal builder changes from accepting `u16` to accepting `&str` / `impl Into<SmolStr>`.

**Write steps** (`addV`, `addE`) auto-register:

```rust
// Before
g.addV(42u16)

// After
g.addV("person")   // registers "person" → LabelId on first call
g.addE("knows")
```

The `TraversalBuilder` holds a reference to `Arc<RwLock<Schema>>`. When `addV("person")`
is called, it calls `schema.write().register_vertex_label_bytes("person")` to resolve the
string to a `LabelId` and builds the `LogicalStep::AddV { label_id }` plan node. The
schema write bytes are queued and flushed when the traversal commits.

**Read steps** (`out`, `in`, `hasLabel`, etc.) do **not** auto-register. An unknown label
on a read step produces **zero results** — semantically correct, because if the label has
never been written, no elements with that label exist:

```rust
g.V([]).out(["unknown_label"]).to_list()  // returns [] without error
```

Implementation: `schema.read().vertex_label_id("unknown_label")` returns `None`, so the
step emits an empty `label_ids` vec, and the scan finds nothing.

### 6. Fix `values("label")`

Currently `Vertex::get_value("label")` returns `Primitive::Int32(label_id as i32)`. With
schema available, `GraphCtx` intercepts the "label" key and resolves it:

```rust
// In LogicalGraph's get_value implementation:
if key == LABEL {
    let label_id = vertex.label_id;
    let schema = self.schema.read();
    let name = schema.vertex_label_str(label_id)
        .cloned()
        .unwrap_or_else(|| SmolStr::new(label_id.to_string()));
    return Ok(Some(Primitive::String(name)));
}
```

The fallback (`label_id.to_string()`) handles the edge case of a vertex whose label was
written before the current schema instance was populated (e.g. a bug or data migration).
It degrades gracefully instead of returning `None`.

### 7. Strict mode (Phase 2)

An optional enum on the graph handle controls behaviour when an unknown label is presented
at write time:

```rust
pub enum SchemaMode {
    /// Register unknown labels automatically on first write. (Default)
    Auto,
    /// Reject writes that introduce unknown labels. Schema must be pre-populated.
    Strict,
}
```

In `Strict` mode, `register_vertex_label_bytes` returns an error instead of assigning a
new ID. This lets production deployments treat unexpected labels as bugs rather than
silently expanding the schema.

Schema pre-population under `Strict` mode is done with explicit calls outside any
traversal:

```rust
graph.schema_mut().register_vertex_label("person")?;
graph.schema_mut().register_edge_label("knows")?;
```

---

## Consistency guarantees

| Scenario | Behaviour |
|----------|-----------|
| Crash after schema CF write, before vertex CF write | The label ID exists in schema but no vertex uses it. Harmless; IDs are never reused. |
| Crash after vertex CF write, before schema CF write | **Impossible** — both writes are in the same `WriteBatch`; RocksDB guarantees atomicity within a batch. |
| Two concurrent transactions registering the same label | `RwLock` write lock is held for the duration of `register_*_bytes`. The second writer sees the already-registered ID and skips the schema write. |
| Two concurrent transactions registering different labels | Each gets its own ID. The in-memory `Schema` serialises all registrations through the write lock. |

The schema CF has at most `2 × MAX_LABELS + u16::MAX` entries (vertex labels + edge labels
+ prop keys). At 4096 labels each and 65535 prop keys this is ~73k entries, well within
RocksDB's efficient range.

---

## Implementation order

**Phase 1 — core (required for any string-label API)**

1. Add `CF_SCHEMA` constant and encoding helpers to `src/store/rocks/encoding.rs`
2. Add `CF_SCHEMA` to the column family list in `RocksStorage::open()`; add `load_schema()`
3. Add `register_*_bytes()` variants to `Schema` in `src/schema/definition.rs`
4. Thread `Arc<RwLock<Schema>>` through `LogicalGraph` and `GraphCtx`
5. Change `addV` / `addE` traversal steps to accept `impl Into<SmolStr>` and resolve at plan-build time
6. Change `out` / `in` / `hasLabel` steps to accept `impl Into<SmolStr>` and resolve to `LabelId`; `None` → empty scan
7. Fix `GraphCtx::get_value("label")` in `LogicalGraph` to return `Primitive::String`
8. Remove the dead `pub mod index` and `pub mod validator` declarations from `schema/mod.rs` until those modules exist

**Phase 2 — prop key auto-registration**

9. Call `register_prop_key_bytes()` inside `LogicalGraph::set_property` for each new key
10. `values("propKey")` can then return the resolved `PropKey` string (currently the raw
    string is stored on disk, so this is already correct — but interning enables future
    optimisation)

**Phase 3 — strict mode**

11. Add `SchemaMode` enum to the graph handle
12. Thread `SchemaMode` into `register_*_bytes()`; return `SchemaError::UnknownLabel` on
    `Strict` + unknown name

---

## Affected files

| File | Change |
|------|--------|
| `src/store/rocks/encoding.rs` | Add `CF_SCHEMA`, schema key/value encode helpers |
| `src/store/rocks/store.rs` | Add schema CF to `open()`; add `load_schema()` |
| `src/store/rocks/transaction.rs` | Include schema bytes in `WriteBatch` on new label |
| `src/schema/definition.rs` | Add `register_*_bytes()` methods; remove phantom `mod` declarations |
| `src/schema/mod.rs` | Remove `mod index`, `mod validator` until implemented |
| `src/graph.rs` | Add `schema: Arc<RwLock<Schema>>` to `LogicalGraph`; fix `get_value("label")` |
| `src/engine/context.rs` | Add `fn schema()` to `GraphCtx` trait |
| `src/gremlin/traversal.rs` | Change `addV`/`addE`/`out`/`in`/`hasLabel` to accept strings |
| `src/types/label.rs` | `Label` struct can be removed — superseded by plain `SmolStr` at the API boundary |
