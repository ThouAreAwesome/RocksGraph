# Design: Schema Management & Schema Modes

## Problem

The engine has a `Schema` struct ([`src/schema/definition.rs`](../src/schema/definition.rs)) that
maps `LabelId ↔ String` and `PropKey ↔ u16`, but it is **completely disconnected** from the
rest of the system:

- The traversal API takes raw `u16` label IDs: `g.addV(42u16)` — the caller must manage
  the mapping themselves with no engine support.
- `values("label")` returns `Primitive::Int32(label_id as i32)` — a raw number with no
  semantic meaning to the caller.
- `Schema` is never written to or read from RocksDB; it evaporates on restart.
- `schema/index.rs` and `schema/validator.rs` exist as empty stub files.
- There is **no user-facing way to declare schema explicitly**. The only way to populate
  a `Schema` today is to call `register_vertex_label` / `register_prop_key` directly on the
  struct (currently exercised only from a test — see
  [`src/gremlin/tests.rs:583`](../src/gremlin/tests.rs#L583)). There is no equivalent of
  JanusGraph's `graph.openManagement()` for an application to declare its schema up front,
  and no way to say "reject anything not declared."

The result: the schema layer is a stub, and the only schema-population strategy on offer
is implicit, best-effort registration on first write. Production deployments that want a
locked-down schema — the way JanusGraph, Neo4j (constraints), and most SQL databases do —
have no path to that today.

---

## Goal

Two first-class **schema modes**, selected per `Graph`:

- **`SchemaMode::Auto`** (default) — labels and property keys are registered automatically
  on the first write that introduces them. The user never calls anything explicitly; they
  just write `g.addV("person")` and the engine handles the rest. Good for prototyping and
  for workloads where the schema is genuinely data-driven.
- **`SchemaMode::Strict`** — every vertex label, edge label, and property key **must be
  declared before it is used**. A write that references an undeclared name is rejected
  with an error rather than silently expanding the schema. Good for production deployments
  that want schema drift to be a build-time/deploy-time decision, not a runtime accident.

Both modes are served by the same explicit, **JanusGraph-style management interface**:
a `SchemaManagement` opened from the `Graph` handle, with builder methods to define vertex
labels, edge labels, and property keys, committed as one atomic, version-checked batch.
This interface is *mandatory* in `Strict` mode and *optional but available* in `Auto` mode
(e.g. to declare a property key's data type up front).

The schema is durable in both modes — it survives restarts. Schema changes are themselves
transactional, with the same optimistic-concurrency discipline (`StoreError::Conflict` on a
stale read) already used for ordinary data writes — see §4.

---

## Architecture overview

Two independent paths reach `Schema`, and they cross very different amounts of the
pipeline. The traversal path (`g.addV(...)`, `g.out(...)`, ...) is **string-typed only
above `PhysicalPlanBuilder`** — the Gremlin builder, `LogicalPlan`, and the optimizer never
need a resolved id. `build_step` is where every label *and* property-key name gets resolved
to its numeric form — `label_id` or `prop_key_id` — **once per `LogicalStep`**, before any
physical step exists. Below `build_step` — every volcano physical step, `GraphCtx`, and the
store — deals exclusively in numeric ids (`label_id`, `prop_key_id`); both kinds of name are
the same shape this far down, exactly mirroring each other rather than one staying string
and the other numeric. `Schema` itself is touched only by `GraphCtx` implementations (called
from `build_step`, and from `Schema`'s own decode paths — `get_value`'s "label" case and
`materialize`/`get_all_props`'s property-key case, §6/§8) and by `SchemaManagement`, which
talks to `Schema` directly but never enters this pipeline at
all — it's a second, independent way to reach the same `Schema`, not a continuation of it.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ Gremlin layer — STRINGS                                                  │
│   WriteTraversal::addV("person") / addE("knows") / property(..)          │
│   ReadTraversal::out(["knows"]) / hasLabel(["person"])                   │
└─────────────────────────────────┬────────────────────────────────────────┘
                                  │ builds
                                  ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ LogicalPlan / LogicalStep — STRINGS                                      │
│   AddVStep{label: SmolStr}   HasLabelStep{labels: Vec<SmolStr>}          │
│   Optimizer rules (merge_adde_from, …) rewrite structure only —          │
│   none of them need a resolved id                                        │
└─────────────────────────────────┬────────────────────────────────────────┘
                                  │ terminal call (.next() / .to_list() / .iter())
                                  ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ PhysicalPlanBuilder::build / build_step — STRINGS IN, IDS OUT            │
│   Schema's own methods (§5), via the handle from ctx.schema() (§3):      │
│   resolve_vertex_label/resolve_edge_label/resolve_prop_key(name)         │
│     -> label_id / prop_key_id  (write steps — mode-gated, +version)      │
│   vertex_label_id/edge_label_id/prop_key_id(name) -> Option<id>          │
│     (read-side filters/projections — lookup only, never mutates)         │
│   — once per LogicalStep, before any element is scanned/written          │
└─────────────────────────────────┬────────────────────────────────────────┘
                                  │ resolved label_id/prop_key_id baked into the
                                  │ physical step — unchanged below
══════════════════════════════════╪════════ §6 CONVERSION BOUNDARY ═════════
                                  │ everything from here down deals only in
                                  │ label_id/prop_key_id, never label/prop_key
                                  ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ Volcano physical steps — NUMERIC IDS, mirrors today's label_id           │
│   AddVStep{label_id} / HasLabelStep{label_ids} / InOutStep{label_ids}    │
│   HasPropertyStep{prop_key_id} / PropertyStep{prop_key_id}               │
│   ValuesStep{property_keys: [(label, prop_key_id), ...]}                 │
└─────────────────────────────────┬────────────────────────────────────────┘
                                  │ ctx.add_vertex(id,label_id) / ctx.get_value(key,
                                  │ prop_key_id) — unchanged shape, numeric either way
                                  ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ GraphCtx — LogicalGraph / LogicalSnapshot                                │
│   receives BOTH the resolution calls above (from the builder, once)      │
│   and the ordinary numeric data calls here (from physical steps,         │
│   once per element) — schema: Arc<RwLock<Schema>>, private               │
└─────────────────────────────────┬────────────────────────────────────────┘
                                  │ numeric label_id/prop_key_id only — same
                                  │ WriteBatch as the data
                                  ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ GraphStore — S::Txn / S::Snapshot — NEVER references Schema              │
│   numeric VertexKey/EdgeKey/label_id/prop_key_id — the property blob     │
│   is opaque bytes to the store; only encode_props/decode_props (§6)      │
│   know it's [prop_key_id, value] pairs                                   │
└──────────────────────────────────────────────────────────────────────────┘

                                  │
                                  ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ RocksDB — CF_SCHEMA, CF_VERTICES, CF_EDGES_OUT/IN, CF_VERTEX_DEGREE      │
└──────────────────────────────────────────────────────────────────────────┘

                   ── separately: the schema-only path ──

┌──────────────────────────────────────────────────────────────────────────┐
│ SchemaManagement — STRINGS, never enters the pipeline above              │
│   open_management() → make_vertex_label()/make_edge_label()/             │
│   make_property_key()/set_edge_mode()/set_schema_mode() → commit()       │
└─────────────────────────────────┬────────────────────────────────────────┘
                                  │ declare_*() + version CAS, own WriteBatch
                                  ▼
                                    Schema (Arc<RwLock<Schema>>)  ──▶  RocksDB: CF_SCHEMA
```

Five things to take away from this:

- **`label_id`/`prop_key_id` dominate the engine; `label`/`prop_key` are the exception, not
  the rule.** Past `build_step`, the only thing that exists is the numeric form of both
  kinds of name. The strings live in exactly two places: the Gremlin builder/`LogicalPlan`
  (above `build_step`, where the user's literal name is the only thing available yet) and
  the decode points inside `get_value`'s "label" handling and `materialize`/`get_all_props`'s
  property-key handling (§8), used when a step like `values(["label"])`, `.properties(...)`,
  or final materialization needs to hand the user an actual string. Naming follows this
  split throughout: `label`/`labels`/`prop_key`/`prop_keys` always mean a string;
  `label_id`/`label_ids`/`prop_key_id`/`prop_key_ids` always mean the resolved numeric form.
  Never the same field, never ambiguous which one a parameter is from its name alone.
- **Property keys get a numeric form for a different reason than labels.** Labels need
  `label_id` because the engine reuses it as a compact, prefix-scannable storage *key*
  (`EdgeKey`/`VertexKey`) — there's no equivalent for property keys, which only ever live in
  the property *value* blob (`encode_props`/`decode_props`,
  [`src/store/rocks/encoding.rs:224`](../src/store/rocks/encoding.rs#L224)/
  [`:317`](../src/store/rocks/encoding.rs#L317)); RocksGraph has no property indexes, so
  there's no scan that a numeric `prop_key_id` makes algorithmically faster. The
  justification here is CPU, not storage layout: today, `decode_props` constructs (and
  UTF-8-validates) a `SmolStr` for *every* property key on an element the first time
  anything touches it, even when a step only cares about one of them
  (`HasPropertyStep`/`ValuesStep`'s known-key case) — see §6. Switching the wire format and
  `Property.key` to `prop_key_id: u16` makes that a plain integer read, and lets
  known-key lookups compare `u16 == u16` instead of `SmolStr == SmolStr`. The one case that
  does *not* clearly benefit is full enumeration (`g.V().next()`'s property map, `get_all_props`)
  — see "Why property keys also get a numeric form" in §6 for the honest accounting of that
  trade-off.
- **Encoding is hoisted as early as possible; decoding is deferred as late as possible —
  for both labels and property keys.** These aren't symmetric for a reason: the string in
  `.addV("person")`/`.property("name", v)` is a fixed input that never changes for that
  step's lifetime, so there's nothing gained by resolving it lazily — `build_step` does it
  once, immediately, for both. A scanned element's `label_id`/`prop_key_id`, by contrast,
  isn't known until the store actually returns it, so decoding it to a string can only
  happen at execution time, and "as late as possible" means: only for the specific
  element/property a step asks about, never speculatively for every element that happens to
  pass through.
- **Two different things stay numeric for two different reasons.** A `Traverser`'s
  `EdgeKey`/`VertexKey` payload stays numeric because it's *structural identity*, reused
  zero-cost as the literal storage key — nobody computes on it as a string, so there's
  nothing to convert, ever (§6). The `Store`/`GraphStore` trait stays numeric because it
  must never import `Schema` at all (confirmed already true of `src/store/traits.rs` today).
  Both of these are unaffected by where resolution happens — they were never the boundary.
- **`SchemaManagement` is a separate, shorter path.** It skips `LogicalPlan`, the optimizer,
  and the volcano engine entirely — it's a thin wrapper that stages calls into a batch and
  applies them to `Schema` directly at `commit()`. This is why it can use plain
  CAS-on-`version` (§4) instead of needing any of the traversal machinery.

---

## Design

### 0. `SchemaMode`, `EdgeMode`, and `version` — what lives on `Schema`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchemaMode {
    /// Register unknown labels/property keys automatically on first write. (Default)
    #[default]
    Auto,
    /// Reject writes that reference an undeclared label or property key.
    /// Schema must be populated via `SchemaManagement` first.
    Strict,
}

pub struct Schema {
    pub mode: SchemaMode,        // graph-wide write policy — persisted, versioned, CAS-changed
    pub edge_mode: EdgeMode,     // graph-wide multiplicity setting — persisted, versioned, CAS-changed
    pub version: u64,            // bumped on every successful resolve_*/declare_*  (§5)
    pub vertex_labels: BiHashMap<LabelId, SmolStr>,
    pub edge_labels: BiHashMap<LabelId, SmolStr>,
    pub prop_keys: BiHashMap<u16, PropKey>,
    pub prop_key_types: HashMap<u16, PropKeyConfig>, // §4
    /// Set of vertex label IDs successfully persisted on disk.
    pub persisted_vertex_labels: std::collections::HashSet<LabelId>,
    /// Set of edge label IDs successfully persisted on disk.
    pub persisted_edge_labels: std::collections::HashSet<LabelId>,
    /// Set of property key IDs successfully persisted on disk.
    pub persisted_prop_keys: std::collections::HashSet<u16>,
}
```

`mode` and `edge_mode` are treated identically: both are graph-wide schema *content*, not
a per-process runtime preference. Both are persisted in the schema CF, both participate in
`version`/CAS, and both are changed exactly one way — through `SchemaManagement` (§4),
never through a direct setter on `Graph`. This is what makes `SchemaMode` consistent: every
process that opens a given on-disk graph sees the same `mode`, because it's read from disk,
not chosen at `open()` time.

There is deliberately **no per-label `EdgeConfig`/`vertex_configs` map**. Edge multiplicity
is the single graph-wide `edge_mode`, not a per-label setting — see `design_multiple_edges.md`
§2 for the rationale. Nothing today needs per-label configuration on either vertex or edge
labels, so no map exists for either; if a genuine per-label need shows up later (e.g. the
property-constraints idea under "Out of scope"), add both maps together at that point.

`GraphOptions` only seeds a **brand-new** database — it has no effect on one that already
has a schema:

```rust
pub struct GraphOptions {
    pub schema_mode: SchemaMode,
    pub edge_mode: EdgeMode,
}

Graph::open(path)?;                                       // bootstraps SchemaMode::Auto, EdgeMode::Single if `path` is empty
Graph::open_with_options(path, GraphOptions { schema_mode: SchemaMode::Strict, edge_mode: EdgeMode::Single })?;
```

If the schema CF already has a metadata entry (kind `3`, §1) — i.e. this database has been
opened before — `Graph::open`/`open_with_options` load `mode`/`edge_mode`/`version` from
it and ignore whatever `GraphOptions` was passed. If the metadata entry is absent (a truly
fresh database), the supplied `GraphOptions` (or its defaults) are written as that entry
before `open` returns, so every subsequent open — from any process, with or without
`GraphOptions` — sees the same values. After that first write, the only way to change
`mode` or `edge_mode` is `SchemaManagement` (§4), where they go through the same
version/CAS check as any declaration.

### 1. Schema CF on disk

Add a new `schema` column family alongside `vertices`, `edges_out`, `edges_in`, and
`vertex_degree`. Most entries are a single label or property-key declaration:

```
Key:   [ kind:u8 | name:UTF-8 bytes ]
Value: [ id:u16 | config bytes (kind-dependent, possibly empty) ]
```

`kind` discriminant values:

| Value | Meaning |
|-------|---------|
| `0`   | vertex label |
| `1`   | edge label |
| `2`   | property key (value carries `PropKeyConfig`, §4: data type tag + cardinality tag) |
| `3`   | graph metadata — one fixed-key entry, **not** a `LabelId`/name. Value: `[version:u64 BE \| edge_mode:u8 \| schema_mode:u8]` |

This format is append-only and crash-safe: a schema entry is either fully written or
not present; there is no partial-write risk for small values. A full schema scan at
startup costs one sequential read over a typically tiny CF. The metadata entry (kind `3`)
is what makes `SchemaMode`/`EdgeMode` graph-wide and process-independent (§0) — they are
read from this entry on every `open()`, not from `GraphOptions`.

On-disk encoding helpers live in `src/store/rocks/encoding.rs`:

```rust
pub const CF_SCHEMA: &str = "schema";

pub const SCHEMA_KIND_VERTEX_LABEL: u8 = 0;
pub const SCHEMA_KIND_EDGE_LABEL:   u8 = 1;
pub const SCHEMA_KIND_PROP_KEY:     u8 = 2;
pub const SCHEMA_KIND_META:         u8 = 3;
pub const SCHEMA_META_KEY: [u8; 1] = [SCHEMA_KIND_META];

pub fn encode_schema_key(kind: u8, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + name.len());
    key.push(kind);
    key.extend_from_slice(name.as_bytes());
    key
}
```

### 2. Schema load on open

`RocksStorage::open()` scans the schema CF after opening all column families. If it finds
a metadata entry (kind `3`), that entry is authoritative and `defaults` is ignored; if not
(a brand-new database), it writes one from `defaults` before returning:

```rust
impl RocksStorage {
    fn load_schema(&self, defaults: GraphOptions) -> Result<Schema, StoreError> {
        let cf = self.db.cf_handle(CF_SCHEMA)...;
        let mut schema = Schema::new(); // mode: Auto, edge_mode: Single, version: 0 — placeholders, overwritten below
        let mut saw_meta = false;
        for (k, v) in self.db.iterator_cf(&cf, IteratorMode::Start) {
            match k[0] {
                SCHEMA_KIND_VERTEX_LABEL => {
                    let id = id_from(&v);
                    schema.vertex_labels.insert(id, name_from(&k));
                    schema.persisted_vertex_labels.insert(id);
                }
                SCHEMA_KIND_EDGE_LABEL   => {
                    let id = id_from(&v);
                    schema.edge_labels.insert(id, name_from(&k));
                    schema.persisted_edge_labels.insert(id);
                }
                SCHEMA_KIND_PROP_KEY     => {
                    let id = id_from(&v);
                    /* load config and insert mapping */
                    schema.persisted_prop_keys.insert(id);
                }
                SCHEMA_KIND_META         => { schema.version = ..; schema.edge_mode = ..; schema.mode = ..; saw_meta = true; }
                _ => {} // forward-compatible: ignore unknown kinds
            }
        }
        if !saw_meta {
            schema.mode = defaults.schema_mode;
            schema.edge_mode = defaults.edge_mode;
            self.write_schema_meta(&schema)?; // version stays 0 — this is the bootstrap write, not a "change"
        }
        Ok(schema)
    }
}
```

The caller wraps the result in `Arc<RwLock<Schema>>` and passes it into `LogicalGraph`
(see §3).

### 3. Threading Schema through the stack

`Schema` is shared across concurrent traversals and across the management interface. The
owner is the top-level graph handle (`Graph`), which already exposes it publicly as
`Graph::schema() -> Arc<RwLock<Schema>>` ([`src/api.rs:89`](../src/api.rs#L89)). `Schema` is
touched directly by exactly three kinds of code, and nothing else:

- **`GraphCtx` implementations** (`LogicalGraph`/`LogicalSnapshot`), which own the field.
- **`PhysicalPlanBuilder`**, which resolves names at build time (§6) — it receives a handle
  to `Schema` the same way `Graph::schema()`'s callers always have: a plain `Arc` clone, not
  a bespoke trait method per operation.
- **`SchemaManagement`**, on the schema-declaration path (§4).

Everything else — the Gremlin builder, `LogicalPlan`, the optimizer, every volcano physical
step, and the `Store`/`GraphStore` trait (`S::Txn`/`S::Snapshot`) — never imports `Schema`
and never holds a reference to it. This is not a new constraint for the `Store` side:
`src/store/traits.rs` already has zero references to `Schema` today; this design just makes
that independence an explicit, permanent invariant rather than an accident of what hasn't
been built yet (point 4 from review — see also §6).

A first draft of this design put five separate methods on `GraphCtx`
(`resolve_vertex_label`, `resolve_edge_label`, `resolve_prop_key`, `vertex_label_id`,
`edge_label_id`) — each one just a thin forward to the method of the same name already
defined on `Schema` itself (§5). That's unnecessary duplication for a capability exactly one
caller (`PhysicalPlanBuilder`) uses: every `GraphCtx` implementor, including the minimal
`NoopCtx`, would have had to grow five boilerplate stubs for methods volcano steps never
call. The fix is to expose the *handle*, not a copy of every operation you can perform on
it — exactly how `Graph::schema()` already works one layer up:

```
Graph
  schema: Arc<RwLock<Schema>>
  ├── .schema() -> Arc<RwLock<Schema>>                (already implemented, src/api.rs:89)
  ├── .open_management() -> SchemaManagement          (§4 — direct access, bypasses GraphCtx)
  ├── .read()  -> ReadSession   ─┐
  └── .begin() -> TxSession     ─┴─ both pass Arc::clone(&self.schema) into
                                     LogicalSnapshot / LogicalGraph (already implemented)
LogicalGraph<S> / LogicalSnapshot<S>
  schema: Arc<RwLock<Schema>>
  store: S::Txn / S::Snapshot   — never sees `schema`, only ever gets numeric ids/keys
GraphCtx trait
  fn schema(&self) -> Arc<RwLock<Schema>>;   // the ONE new method — same shape as Graph::schema()
  // data access — unchanged signatures, called from volcano steps' produce(), as today
  fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError>;
  fn add_edge(&mut self, cek: &EdgeKey) -> Result<EdgeKey, StoreError>;
```

`GraphCtx::schema()` is the **only** new method on the trait. `GraphTraversal::build`
([`src/gremlin/traversal.rs:173`](../src/gremlin/traversal.rs#L173)) calls it once, up
front, and passes the `Arc<RwLock<Schema>>` into `PhysicalPlanBuilder::build`/`build_step` as
a plain parameter — not `&mut dyn GraphCtx` at all, since the builder needs nothing else
from `GraphCtx` (§6). Inside `build_step`, resolution is just calling `Schema`'s *existing*
methods directly: `schema.write().unwrap().resolve_vertex_label(name)` for write steps,
`schema.read().unwrap().vertex_label_id(name)` for read-side filters — the exact methods
already specified in §5, with no GraphCtx-side wrapper duplicating them. `add_vertex`/
`add_edge`/`set_property` keep their exact current signatures and are called from volcano
steps' `produce()` exactly as today, with an already-resolved `LabelId` in hand.

The `RwLock` is acquired for **writing** only when the schema actually changes: a new label
or property key introduced via `resolve_*` at build time (Auto mode), or a `commit()` on a
`SchemaManagement` session. Every other access — `vertex_label_id`/`edge_label_id`/`prop_key_id`
lookups, `values("label")`'s decode, and `materialize`/`get_all_props`'s property-key decode
(§8) — takes the read lock (shared, non-blocking under concurrent reads).

### 4. JanusGraph-style management interface

JanusGraph separates *using* the graph (`g.addV(...)`) from *defining* its schema
(`graph.openManagement()` → `mgmt.makeVertexLabel(...)`, `mgmt.makePropertyKey(...)`,
`mgmt.makeEdgeLabel(...)`, then `mgmt.commit()`). RocksGraph adopts the same split, minus
the per-label multiplicity knob (`design_multiple_edges.md` §2):

```rust
let mgmt = graph.open_management();

mgmt.make_property_key("name", DataType::String).make();
mgmt.make_property_key("since", DataType::Int64).make();

mgmt.make_vertex_label("person").make();
mgmt.make_edge_label("knows").make();     // no per-label multiplicity — see set_edge_mode below
mgmt.set_edge_mode(EdgeMode::Multi);       // graph-wide, one-way: Single -> Multi only (see below)
mgmt.set_schema_mode(SchemaMode::Strict);  // graph-wide, either direction

mgmt.commit()?;   // CAS-validates + applies the whole batch atomically, persists to schema CF
```

**Staging, not immediate effect.** Each `make_*`/`set_edge_mode` call accumulates into a
`pending_*` vector owned by the `SchemaManagement` session; it does **not** touch the shared
`Schema` until `commit()`. This mirrors JanusGraph's transactional management system and
means a batch of related declarations either all land or none do.

```rust
pub struct SchemaManagement {
    store: Arc<RocksStorage>,
    schema: Arc<std::sync::RwLock<Schema>>,
    base_version: u64,
    pending_vertex_labels: Vec<String>,
    pending_edge_labels: Vec<String>,
    pending_prop_keys: Vec<(String, DataType, Cardinality)>,
    pending_edge_mode: Option<EdgeMode>,
    pending_schema_mode: Option<SchemaMode>,
}

impl Graph {
    /// Open a schema-management session, mirroring JanusGraph's `graph.openManagement()`.
    pub fn open_management(&self) -> SchemaManagement { .. }
}

pub struct PropertyKeyMaker<'a> { mgmt: &'a mut SchemaManagement, name: String, data_type: DataType, cardinality: Cardinality }
pub struct VertexLabelMaker<'a> { mgmt: &'a mut SchemaManagement, name: String }
pub struct EdgeLabelMaker<'a>   { mgmt: &'a mut SchemaManagement, name: String }

impl SchemaManagement {
    pub fn make_property_key(&mut self, name: impl Into<String>, data_type: DataType) -> PropertyKeyMaker<'_> { .. }
    pub fn make_vertex_label(&mut self, name: impl Into<String>) -> VertexLabelMaker<'_> { .. }
    pub fn make_edge_label(&mut self, name: impl Into<String>) -> EdgeLabelMaker<'_> { .. }

    /// Stage a graph-wide multiplicity change. Applied atomically with everything
    /// else in this batch at `commit()`. `commit()` rejects `EdgeMode::Multi -> EdgeMode::Single`
    /// with `StoreError::SchemaConflict` — see "One-way ratchet" below.
    pub fn set_edge_mode(&mut self, mode: EdgeMode) -> &mut Self { .. }

    /// Stage a graph-wide schema-mode change (either direction is allowed).
    pub fn set_schema_mode(&mut self, mode: SchemaMode) -> &mut Self { .. }

    pub fn commit(self) -> Result<(), StoreError> { .. } // see "Versioning and CAS commit" below
}
```

`PropertyKeyMaker::data_type` is metadata only in this phase — it is recorded
(`PropKeyConfig`, below) but **not yet enforced** against the `Primitive` written by
`property()`. `VertexLabelMaker` has no configuration knobs yet (vertex labels carry no
behavior beyond identity today, unlike JanusGraph's `partition()`/`setStatic()`); it exists
as its own builder type purely for symmetry with the other two makers, so options can be
added later without breaking call sites that don't use them.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType { Bool, Int32, Int64, Float32, Float64, String, Uuid }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality { Single } // Set/List reserved — properties are single-valued today

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropKeyConfig { pub data_type: DataType, pub cardinality: Cardinality }
```

**Index building** (JanusGraph's `mgmt.buildIndex(...)`) is intentionally **out of scope**
— RocksGraph has no secondary-index infrastructure yet. The shape leaves room for a
`build_index(...)` method later without disturbing this design.

#### Versioning and CAS commit

Schema changes get the same optimistic-concurrency treatment as ordinary data writes,
instead of simply holding a lock for the session's whole lifetime:

1. `open_management()` captures `base_version = schema.read().version` and returns
   immediately — no lock held while the caller builds up the batch with `make_*` calls.
2. `commit()` takes the `Schema` write lock **once**, for the duration of applying the
   batch, and first checks `schema.version == self.base_version`.
   - **Mismatch** → return `StoreError::Conflict` and discard the whole batch, *without
     touching `Schema`*. This is the **same variant and the same retry contract** already
     documented for data transactions (`src/types/error.rs`: "the only variant that callers
     are expected to retry... retry the transaction from scratch") — a stale
     `SchemaManagement` is conceptually no different from a stale `TxSession`.
   - **Match** → apply every staged `declare_*`/`set_edge_mode`/`set_schema_mode` call,
     increment `version` by 1, and write the schema CF batch (declarations + the new
     `[version, edge_mode, schema_mode]` metadata entry) in one `WriteBatch`.
3. Redeclaring an existing name with an *identical* configuration is a no-op (idempotent —
   re-running a schema-setup script is safe) and does **not** bump `version`, since nothing
   actually changed. Redeclaring with a **different** configuration is a
   `StoreError::SchemaConflict` — a different failure from `StoreError::Conflict`
   (concurrent-modification race), because it's not a race, it's a real incompatible
   request, and retrying won't fix it.

#### One-way ratchet: `edge_mode` can only go `Single` → `Multi`

`commit()` rejects a staged `set_edge_mode(EdgeMode::Single)` when the current persisted
`edge_mode` is already `EdgeMode::Multi`, with `StoreError::SchemaConflict`. Once any
`(src, label, dst)` triple is allowed to carry more than one edge, there is no way to make
"at most one edge" true again without inspecting and rewriting existing data — and this
design doesn't attempt that retroactive validation (the previous draft's caveat about
"existing data is left as-is" described exactly the inconsistent state this ratchet now
prevents from being reachable at all). Demoting is therefore not offered as an option;
going from `Multi` back to `Single` requires an explicit data migration outside this
design, not a schema call. `schema_mode` has no such restriction — `Auto ↔ Strict` is
allowed in both directions, since neither direction can make previously-written data
invalid (every label/key on disk was, by construction, resolved or declared at the time
it was written, regardless of what `schema_mode` is set to afterwards).

`resolve_*` (§6) also increments `version` on every new auto-registration, under the same
write lock it already takes — so a `SchemaManagement` staged concurrently with an Auto-mode
write that registers a brand-new name will correctly see its `base_version` go stale and
get `StoreError::Conflict` at `commit()`, even though the racing write was a regular
traversal, not another management session.

### 5. `resolve_*` vs `declare_*`, and the StoreError variants

```rust
impl Schema {
    /// Always allowed, in either `SchemaMode`. Used only by `SchemaManagement::commit()`.
    /// Does not touch `version` itself — the caller (`commit()`) bumps it once per batch.
    pub fn declare_vertex_label(&mut self, name: &str) -> Result<LabelId, StoreError> {
        self.register_vertex_label(name).ok_or(StoreError::SchemaExhausted)
    }
    pub fn declare_prop_key(&mut self, name: &str, cfg: PropKeyConfig) -> Result<u16, StoreError> {
        if let Some(id) = self.prop_key_id(name) {
            if self.prop_key_types.get(&id) != Some(&cfg) {
                return Err(StoreError::SchemaConflict(name.into()));
            }
            return Ok(id);
        }
        let id = self.register_prop_key(name).ok_or(StoreError::SchemaExhausted)?;
        self.prop_key_types.insert(id, cfg);
        Ok(id)
    }

    /// One-way ratchet — see "Versioning and CAS commit" §4.
    pub fn declare_edge_mode(&mut self, mode: EdgeMode) -> Result<(), StoreError> {
        if self.edge_mode == EdgeMode::Multi && mode == EdgeMode::Single {
            return Err(StoreError::SchemaConflict("edge_mode: Multi -> Single is not allowed".into()));
        }
        self.edge_mode = mode;
        Ok(())
    }

    /// Either direction is always allowed.
    pub fn declare_schema_mode(&mut self, mode: SchemaMode) -> Result<(), StoreError> {
        self.mode = mode;
        Ok(())
    }

    /// Called from `PhysicalPlanBuilder::build_step` (§6), once per `LogicalStep`,
    /// before any physical step is constructed — never from a volcano step.
    pub fn resolve_vertex_label(&mut self, name: &str) -> Result<LabelId, StoreError> {
        if let Some(id) = self.vertex_label_id(name) {
            return Ok(id);
        }
        match self.mode {
            SchemaMode::Strict => Err(StoreError::SchemaViolation(name.into())),
            SchemaMode::Auto => {
                let id = self.register_vertex_label(name).ok_or(StoreError::SchemaExhausted)?;
                self.version += 1;
                Ok(id)
            }
        }
    }
    // mirror resolve_edge_label

    /// Same shape as `resolve_vertex_label`, for the property-key namespace.
    /// Infers and locks down the `DataType` of the property on its very first write.
    pub fn resolve_prop_key(&mut self, name: &str, data_type: DataType) -> Result<u16, StoreError> {
        if let Some(id) = self.prop_key_id(name) {
            if let Some(cfg) = self.prop_key_types.get(&id) {
                if cfg.data_type != data_type {
                    return Err(StoreError::SchemaViolation(
                        format!("Type mismatch for property key '{}': expected {:?}", name, cfg.data_type).into()
                    ));
                }
            }
            return Ok(id);
        }
        match self.mode {
            SchemaMode::Strict => Err(StoreError::SchemaViolation(name.into())),
            SchemaMode::Auto => {
                let id = self.register_prop_key(name).ok_or(StoreError::SchemaExhausted)?;
                self.prop_key_types.insert(id, PropKeyConfig { data_type, cardinality: Cardinality::Single });
                self.version += 1;
                Ok(id)
            }
        }
    }
}
```

Three new `StoreError` variants:

```rust
pub enum StoreError {
    // ...
    /// `SchemaMode::Strict` and the name was never declared via `SchemaManagement`,
    /// or a write-time type mismatch occurred against a declared DataType.
    SchemaViolation(SmolStr),
    /// `SchemaManagement::commit()` redeclared an existing name with an incompatible config.
    SchemaConflict(SmolStr),
    /// The 12-bit (labels) or 16-bit (prop keys) id space is exhausted.
    SchemaExhausted,
}
```

Behavior table:

| Call | Mode | Pre-declared? | Result |
|---|---|---|---|
| `g.addV("person")` | Auto | no | registers `"person"` at build time, proceeds, `version += 1` |
| `g.addV("person")` | Strict | yes (via `mgmt`) | resolves to existing id at build time, proceeds, `version` unchanged |
| `g.addV("ghost")` | Strict | no | `Err(StoreError::SchemaViolation("ghost"))` at build time — before execution |
| `g.addV("person").property("nickname", v)` | Strict | `"nickname"` undeclared | `Err(StoreError::SchemaViolation("nickname"))` at build time |
| `g.V([]).out(["unknown"])` | Auto | no | `Ok([])` — read filter short-circuits to statically zero-results step |
| `g.V([]).out(["unknown"])` | Strict | no | `Err(StoreError::SchemaViolation("unknown"))` at build time |

### 5a. Staged Schema Persistence & Validation Invariants

To guarantee absolute consistency and prevent uncommitted schema pollution on transaction aborts/rollback, RocksGraph employs a staged in-memory persistence flag mechanism and write-time validators.

#### 1. In-Memory Persisted Flags (Challenge A)

When new labels or property keys are registered dynamically in `SchemaMode::Auto` during `build_step`'s execution prep:
- They are resolved to new numeric IDs and inserted into `Schema` (in-memory maps), but their IDs are **not** immediately added to `persisted_vertex_labels`, `persisted_edge_labels`, or `persisted_prop_keys`.
- This ensures concurrent threads compiling transactions get a globally consistent ID assignment.
- During execution, the transaction (`LogicalGraph`) tracks all referenced schema elements. For any element where `persisted == false`, `LogicalGraph` stages key-value pairs representing the registration to be written to the RocksDB `CF_SCHEMA` column family inside the transaction's single atomic `WriteBatch`.
- **Upon commit**: Under the `Schema` write lock, `LogicalGraph::commit()` marks all successfully committed IDs as `persisted = true` in the global `Schema` registry (by inserting them into the persisted sets).
- **Upon abort**: Staged writes are discarded. The shared `Schema` entries remain unpersisted in memory. Any future write transaction referencing these unpersisted labels/keys will detect `persisted == false` and stage the schema CF writes again, guaranteeing eventual durability.
- **Upon restart**: `RocksStorage::load_schema()` only populates `persisted` sets with declarations successfully read from the database, dropping any uncommitted dangling entries.

#### 2. Write-Time Type Safety Enforcement (Challenge B)

To prevent type coercion bugs and schema corruption:
- Each property key has a designated `DataType` stored in `PropKeyConfig`.
- In `SchemaMode::Strict`, keys and their types are pre-declared via `SchemaManagement`.
- In `SchemaMode::Auto`, a key's `DataType` is inferred and registered on its very first write.
- At write time (`LogicalGraph::set_property`), the engine retrieves the `PropKeyConfig` for the resolved `prop_key_id`. It validates that the type of the incoming `Primitive` matches the registered `DataType`.
- Mismatched writes are rejected immediately with `StoreError::SchemaViolation`.

#### 3. Strict Mode Read-Path Validation (Challenge C)

Under `SchemaMode::Strict`, read-only queries must not silently ignore typos or reference non-existent elements:
- When resolving names for read-side steps (`Out`, `In`, `Both`, `HasLabel`, `HasProperty`, `Values`), `build_step` calls the read-only lookup methods (`vertex_label_id`, `edge_label_id`, `prop_key_id`).
- If any lookup returns `None`:
  - **In `SchemaMode::Strict`**: The query is rejected immediately at compile time with `Err(StoreError::SchemaViolation)`.
  - **In `SchemaMode::Auto`**: The query compiles successfully, short-circuiting to a zero-results physical step.

Strict mode applies the same gate to property keys as to labels — declaring a vertex label
does **not** implicitly whitelist which property keys can be set on it; *every* property
key used anywhere must itself be declared via `mgmt.make_property_key(...)`. Per-label
property constraints (JanusGraph's `mgmt.addProperties(vertexLabel, ...)`) are a stricter,
optional layer on top of this and remain out of scope (below); this design only requires
that the *key itself* exist somewhere in the schema.

### 6. Conversion boundary: encode early, decode late

Today, **no conversion exists anywhere in the code**: `addV`/`addE`/`out`/`hasLabel`
already take a raw `LabelId`/`u16` directly at the Gremlin layer
([`src/gremlin/traversal.rs:194-273`](../src/gremlin/traversal.rs#L194)), and
`LogicalStep::AddV`/`AddE` already store `label_id: LabelId`
([`src/planner/logical_step/mod.rs:314-332`](../src/planner/logical_step/mod.rs#L314)).
There is no string anywhere in the pipeline to convert — yet. Two earlier drafts of this
section disagreed about exactly where the conversion should happen (`TraversalBuilder` call
time, then inside `GraphCtx` at execution time) because they treated *encoding* and
*decoding* as the same problem with one answer. They aren't — each has a different correct
answer, for a structural reason:

- **Encoding** (`label`/`prop_key` name → `label_id`/numeric id) has a **fixed input**. The
  string in `.addV("person")` is known the instant the `LogicalStep` exists and never
  changes for that step's lifetime. There is nothing to gain by deferring it — resolve it
  once, as early as the string is available, the same way a compiler constant-folds a
  literal.
- **Decoding** (`label_id` → `label` string) has a **data-dependent input**. Which numeric
  id you'll see isn't known until the store actually returns a scanned element — so it
  can't be hoisted early, and "as late as possible" means: decode only for the specific
  element/property a step is actually asked to emit as a string, never speculatively for
  every element that happens to pass through.

#### Naming convention

Because of this split, the field/parameter name should always say which one you're holding:
**`label`/`labels` always means a string name; `label_id`/`label_ids` always means the
resolved numeric form.** Property keys follow the exact same convention now:
`prop_key`/`prop_keys` is the string name, `prop_key_id`/`prop_key_ids` is the resolved
numeric form — see "Why property keys also get a numeric form" below for why this is no
longer a special case.

#### Where encoding happens: `PhysicalPlanBuilder::build_step`, once per `LogicalStep`

`build_step` runs when a terminal method (`.next()`/`.to_list()`/`.iter()`) is called —
already the correct point per the documented lazy-execution model (`src/api.rs`: nothing
happens until a terminal call) — but it runs **before any physical step is constructed**,
i.e. before any element is scanned or written. It gains a `schema: Arc<RwLock<Schema>>`
parameter — not `&mut dyn GraphCtx`, since resolution is all it needs (§3) — obtained once
by its caller, [`GraphTraversal::build`](../src/gremlin/traversal.rs#L173), via
`ctx.schema()`. For each `LogicalStep` that names a label or property key, `build_step`
calls `Schema`'s own methods (§5) directly before building the matching physical step:

- `AddV`/`AddE` → `schema.write().unwrap().resolve_vertex_label(name)`/`resolve_edge_label(name)`
  — mutating, `SchemaMode`-gated. In `Strict` mode with an undeclared name, `build_step`
  returns `Err(StoreError::SchemaViolation(..))` immediately — **nothing in the plan
  executes**, not even earlier steps that would have succeeded, because the whole plan is
  built (and thus fully resolved) before execution begins. This is the "encode ASAP" payoff:
  a Strict-mode schema violation is a build-time error, not a partway-through-a-write error.
- `Out`/`In`/`Both`/`HasLabel` → `schema.read().unwrap().vertex_label_id(name)`/`edge_label_id(name)`
  — read-only, never mutates, `None` on an unknown name.
- `Property`/`HasProperty`/`Values` → `schema.write().unwrap().resolve_prop_key(name)` for
  the write side (`Property`, mode-gated, version bump — same shape as `resolve_vertex_label`)
  or `schema.read().unwrap().prop_key_id(name)` for read-only steps (`HasProperty`, `Values`).
  Unlike the discarded result in an earlier draft of this section, the returned `prop_key_id`
  **is** substituted into the physical step, exactly like `label_id` — see "Why property
  keys also get a numeric form" below for why this changed.

The structural physical steps are **exactly the structs that exist today, unchanged**:
`AddVStep{label_id}`, `HasLabelStep{label_ids}`, `InOutStep{label_ids}`. The property-bearing
ones change their key field the same way labels already work: `HasPropertyStep{prop_key_id}`,
`PropertyStep{prop_key_id}`, and `ValuesStep`'s `property_keys` becomes a list of
`(name, prop_key_id)` pairs — the step keeps the original string alongside the id it
resolved at build time, so it never needs to ask `Schema` again at `produce()` time even when
emitting a `GValue::Property` that must carry the key's name (see "Where decoding happens"
below). In both cases the conversion is fully absorbed by `build_step`, before any element is
scanned; nothing downstream re-derives a name from an id except the two decode points in §8.

One correctness trap to guard against in `build_step`'s resolution logic: `label_ids.is_empty()`
currently means *"no filter — match all labels"* (see the doc comment on
`InOutStep`/`BothStep`). When a **non-empty** string list resolves to zero known ids (every
name is unknown), the result must **not** collapse to an empty `label_ids` — that would
silently flip "match nothing" into "match everything." `build_step` must special-case this
(e.g. emit a step that's statically known to produce zero results) rather than relying on
emptiness to mean the same thing it means for an unfiltered step.

`HasPropertyStep` has the same trap in miniature, simplified by holding a single key rather
than a list: if `schema.read().unwrap().prop_key_id(name)` returns `None` (no vertex or edge
has ever used this key, under either mode), `.has("ghost_key", v)` can never match anything —
`build_step` must emit a statically-zero-results step, not a `HasPropertyStep` constructed
with a dangling/sentinel id. `ValuesStep` does **not** have this trap: its `property_keys` is
a projection list, not an AND-filter, so an unresolved name is simply omitted from the
resolved `(name, prop_key_id)` pairs — the same way an absent key on a particular element
already contributes nothing today. No special-casing needed there; omission is already the
steady-state behavior.

#### Why property keys also get a numeric form

An earlier draft of this section argued property keys should stay string-only, because
labels get `LabelId` for a reason — compact, prefix-scannable storage *keys*
(`EdgeKey`/`VertexKey`) — that property keys don't share, since they only ever live in the
property *value* blob and RocksGraph has no property indexes to make scannable. That part is
still true. What that draft missed: the engine doesn't only pay a storage-size cost for
string property keys — it pays a **CPU cost on every read**. `decode_props`
([`src/store/rocks/encoding.rs:317`](../src/store/rocks/encoding.rs#L317)) parses and
UTF-8-validates a `SmolStr` for *every* property key on an element the first time anything
touches it (`Vertex`/`Edge::ensure_decoded`, [`src/types/element.rs:96`](../src/types/element.rs#L96)) —
including keys a step never asked about. `HasPropertyStep` filtering on one key still pays
that cost for every *other* key on the element it's looking at.

Switching `Property.key` (`src/types/element.rs`) and the `encode_props`/`decode_props` wire
format from `[u16 keylen | key bytes]` to a plain `prop_key_id: u16` removes that cost for
the common case: decoding becomes an integer read with no UTF-8 validation, and equality
checks in `HasPropertyStep`/`ValuesStep`/`Vertex::get_value` become `u16 == u16` instead of
`SmolStr == SmolStr`. Combined with the build-time resolution above, a step that already
knows which key(s) it wants (`.has("age", 30)`, `.values("name")`) never touches `Schema` at
`produce()` time at all — it resolved the id once, at build time, the same way label filters
already do.

**This is not a free win across the board, and the doc should be honest about the one case
that doesn't clearly benefit:** full property enumeration (`g.V().next()`'s property map,
backed by `get_all_props` — see below) still has to turn every `prop_key_id` on the element
back into a string, because the public `Value::Vertex`/`Value::Edge` property map is
`HashMap<String, _>` with no numeric form exposed to callers at all
([`src/gremlin/value.rs:245`](../src/gremlin/value.rs#L245)). That trades "parse the string
out of the blob" for "look up the string in `Schema`'s `BiHashMap` and clone it" — a lateral
move at best, plus a new `Schema` read-lock acquisition per key that didn't exist before. The
win is real for filter/known-key access; for full-enumeration access it's roughly a wash, and
if that ever shows up as real contention, a transaction-local snapshot of the id→name map
(taken once instead of locked per key) would be the natural fix — not attempted here.

#### Where decoding happens: two places, both already behind `GraphCtx`

§8 describes the label decode point: `LogicalGraph::get_value` intercepts the reserved
`"label"` key and decodes `vertex.label_id`/`ek.label_id` to a string via
`schema.read().vertex_label_str(id)`/`edge_label_str(id)`. Property keys get the same
treatment at two points, both already inside a `GraphCtx` implementation (so no new trait
method is needed beyond `schema()`, §3):

- **`get_all_props`** ([`src/graph.rs:546`](../src/graph.rs#L546)) — backs full
  `Vertex`/`Edge` materialization (`g.V().next()`'s property map, and `materialize`'s
  `GValue::Vertex`/`GValue::Edge` arms in [`src/gremlin/traversal.rs:79`](../src/gremlin/traversal.rs#L79)).
  Since `LogicalGraph`/`LogicalSnapshot` already own `schema: Arc<RwLock<Schema>>` directly,
  `get_all_props` decodes every `prop_key_id` to a string *internally* before returning —
  its signature (`Vec<(PropKey, Primitive)>`) stays exactly as it is today; only the
  implementation changes.
  ~~`materialize` itself needs no edit for these two arms.~~ Correction: this missed that
  `get_all_props` returns `(LabelId, Vec<(PropKey, Primitive)>)` — the label id itself, not
  just the property keys, was still left numeric on the public `Value::Vertex`/`Value::Edge`
  (`label_id: u16`) until a follow-up fix decoded it to `label: String` in `materialize`'s
  `GValue::Vertex`/`GValue::Edge` arms, matching `Key::Label`'s decode behavior.
- **`materialize`'s `GValue::Property` arm** ([`src/gremlin/traversal.rs:100`](../src/gremlin/traversal.rs#L100))
  — backs `.properties(...)`-style results, where the physical step already carries the
  original string next to the id it resolved (see "Where encoding happens" above) for the
  *known-key* case, so no decode is needed there at all. The one place a real decode is still
  needed is `ValuesStep`'s not-yet-implemented empty-list ("all properties") branch
  ([`src/engine/volcano/steps/values.rs:69`](../src/engine/volcano/steps/values.rs#L69)) — like
  `get_all_props`, it doesn't know its keys ahead of time, so it must call
  `ctx.schema().read().unwrap().prop_key_str(id)` per discovered id, mirroring the label
  decode pattern exactly.

No other step decodes anything. A pass-through chain like `out().out().out()` never touches
either decode point at all — every intermediate `GValue::Vertex`/`GValue::Edge` stays exactly
as numeric as it always was.

### 7. Read steps stay mode-independent

**Read steps** (`out`, `in`, `hasLabel`, `has`, `values`, etc.) never call `resolve_*` and
are unaffected by `SchemaMode`. An unknown label or property key on a read step produces
**zero results** in both modes — semantically correct, because if the name has never been
declared or written, no elements with that label/property can exist:

```rust
g.V([]).out(["unknown_label"]).to_list()      // returns [] without error, Auto or Strict
g.V([]).has("unknown_key", 1).to_list()       // same — see the HasPropertyStep trap, §6
```

Implementation: `schema.read().unwrap().vertex_label_id("unknown_label")` /
`prop_key_id("unknown_key")` (§3, §6 — the read-only `Schema` lookup, never `resolve_*`)
returns `None`. For `HasLabelStep`/`InOutStep`/`HasPropertyStep`, `build_step` must turn that
into "definitely zero matches," not an empty `label_ids` vec or a dangling `prop_key_id` —
see the empty-list/single-key traps called out in §6 — since an empty vec on the label-list
steps means "no filter, match everything," the opposite of what an all-unknown filter list
should mean. The physical step itself never sees a string at all — by the time it exists,
`build_step` has already resolved (or short-circuited) the filter. `ValuesStep` is the one
exception: an unresolved name among several is simply omitted, not short-circuited (§6).

### 8. Fix `values("label")`, and decode property-key names

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
It degrades gracefully instead of returning `None`. This is independent of `SchemaMode` —
by construction, a vertex on disk always has a label id that was, at some point, resolved
successfully (Auto) or declared (Strict).

Property keys get the same fallback treatment at the two decode points from §6 —
`get_all_props` and `ValuesStep`'s all-properties branch:

```rust
// Inside LogicalGraph::get_all_props, after collecting (prop_key_id, Primitive) pairs:
let schema = self.schema.read();
let props = raw_props.into_iter()
    .map(|(id, v)| {
        let name = schema.prop_key_str(id).cloned().unwrap_or_else(|| PropKey::new(id.to_string()));
        (name, v)
    })
    .collect();
```

Same reasoning as the label fallback: a `prop_key_id` on disk was, by construction, resolved
or declared at write time, so the fallback only ever fires for the same kind of edge case
(a bug, or data written before the in-memory `Schema` was populated) — never in normal
operation.

---

## Consistency guarantees

| Scenario | Behaviour |
|----------|-----------|
| Crash after schema CF write, before vertex CF write | The label/key exists in schema but no element uses it yet. Harmless; ids are never reused. |
| Crash after vertex CF write, before schema CF write | **Impossible** — both writes are in the same `WriteBatch`; RocksDB guarantees atomicity within a batch. |
| Two concurrent transactions resolving the same new label (Auto) | `RwLock` write lock is held for the duration of `resolve_*`. The second writer sees the already-registered id and skips the schema write; `version` only advances once. |
| `SchemaManagement::commit()` races a concurrent `resolve_*` or another `commit()` | Whichever takes the `Schema` write lock first wins and advances `version`. Any other in-flight `SchemaManagement` whose `base_version` no longer matches gets `StoreError::Conflict` from its own `commit()` — same retry contract as a data-transaction conflict: discard the session, reopen `open_management()`, restage, retry. |
| `commit()` redeclares an existing name with an incompatible config (e.g. a different `data_type`) | `StoreError::SchemaConflict` — not retryable; the caller's batch is wrong, not stale. |
| `commit()` stages `set_edge_mode(EdgeMode::Single)` while the persisted `edge_mode` is already `Multi` | `StoreError::SchemaConflict` — the one-way ratchet (§4) rejects the whole batch; not retryable. |
| `Strict` mode, write references an undeclared name | `StoreError::SchemaViolation` — detected before any batch is built; no partial write occurs. |
| `set_edge_mode(EdgeMode::Multi)` commits after `Single`-mode data already exists | Existing on-disk edges are left as-is (all at `DEFAULT_RANK`, which remains valid); only new writes are checked against the new `edge_mode` (`design_multiple_edges.md` §1). The reverse direction is unreachable — see the one-way ratchet. |
| A process opens an existing graph with `GraphOptions` that differ from the persisted metadata entry | `GraphOptions` is ignored; the persisted `mode`/`edge_mode` win, so every process sees the same values (§0). |

The schema CF has at most `2 × MAX_LABELS + u16::MAX + 1` entries (vertex labels + edge
labels + prop keys + the one metadata entry). At 4096 labels each and 65535 prop keys this
is ~73k entries, well within RocksDB's efficient range.

---

## Implementation order

**Phase 1 — core schema registry + persistence (mode-independent)**

1. Add `CF_SCHEMA` constant, kind discriminants (incl. `SCHEMA_KIND_META`), and encoding
   helpers to `src/store/rocks/encoding.rs`
2. Add `CF_SCHEMA` to the column family list in `RocksStorage::open()`; add
   `load_schema(defaults: GraphOptions)`
3. Add `mode: SchemaMode`, `edge_mode: EdgeMode`, `version: u64` fields to `Schema`; remove
   the per-label `EdgeConfig`/`edge_configs`/`register_edge_label_with_config` machinery
   already in `src/schema/definition.rs`, replaced by the single `edge_mode` field
   (`design_multiple_edges.md`). `load_schema` takes a `GraphOptions` used only to bootstrap
   a brand-new database's metadata entry (§0, §2) — on an existing database the persisted
   entry wins
4. Update `LogicalGraph::add_edge` ([`src/graph.rs:672`](../src/graph.rs#L672)) to read
   `schema.edge_mode` instead of `schema.edge_config(label_id)`
5. Thread schema-CF write bytes (including the metadata entry) through
   `LogicalGraph::add_vertex`/`add_edge`/`set_property` so registrations land in the same
   `WriteBatch` as the data they accompany

**Phase 2 — conversion boundary at `build_step` + `SchemaMode` enforcement (§6)**

6. Change `LogicalStep::AddV`/`AddE`/`HasLabel`/`Out`/`In`/`Both` label fields from
   `LabelId`/`SmallVec<[LabelId; 4]>` to `SmolStr`/`SmallVec<[SmolStr; 4]>`
   (`src/planner/logical_step/mod.rs`). `LogicalStep::Property`/`HasProperty`/`Properties`
   are unchanged — `PropKey` is already `SmolStr`, and logical steps stay string-typed
   regardless of what changes below `build_step` (§6)
7. Change `addV`/`addE`/`out`/`in`/`hasLabel` traversal-builder methods
   (`src/gremlin/traversal.rs`) to accept `impl Into<SmolStr>`; `property()`/`has()`/
   `values()` are unchanged
8. Add `resolve_vertex_label`/`resolve_edge_label`/`resolve_prop_key` (mutating,
   `SchemaMode`-gated, bumps `version`) and `vertex_label_id`/`edge_label_id`/`prop_key_id`
   (read-only) to `Schema` itself (`src/schema/definition.rs`, §5) — these are plain inherent
   methods on `Schema`, not `GraphCtx` methods
9. Add **one** new method, `fn schema(&self) -> Arc<RwLock<Schema>>`, to the `GraphCtx`
   trait (`src/engine/context.rs`), mirroring `Graph::schema()`; implement in
   `LogicalGraph`/`LogicalSnapshot`/`NoopCtx` as a plain `Arc::clone`/stub. No other new
   `GraphCtx` methods — this replaces an earlier draft's five separate forwarding methods
   (`resolve_*`, `vertex_label_id`, `edge_label_id` directly on the trait), which were pure
   boilerplate for a capability only `PhysicalPlanBuilder` uses
10. Add a `schema: Arc<RwLock<Schema>>` parameter to `PhysicalPlanBuilder::build`/
    `build_step` (`src/engine/volcano/builder.rs`) — not `&mut dyn GraphCtx`, since
    resolution via step 8's methods is all the builder needs. Update its one call site
    ([`GraphTraversal::build`](../src/gremlin/traversal.rs#L173)) to call `ctx.schema()`
    once and pass the result through. For each `LogicalStep` naming a label *or property
    key*, call the matching method from step 8 directly on `schema` *before* constructing
    the physical step, and pass the resolved id into the **existing, unchanged** constructor
    for structural steps (`AddVStep::new`, `HasLabelStep::new`, etc.) or the **updated**
    constructor for property-bearing steps (step 14, below)
11. Handle the "all names unresolved" case in `build_step` per the traps in §6/§7: a read
    step whose entire (non-empty) label filter list fails to resolve, or a `HasPropertyStep`
    whose single key fails to resolve, must become a step that's statically zero-results —
    never one constructed with an empty `label_ids` or a dangling `prop_key_id`. `ValuesStep`
    is the exception: unresolved names are simply omitted from its resolved list (§6, §7)
12. Change `Property`'s key field (`src/types/element.rs`) from `key: PropKey` to
    `key: u16` (`prop_key_id`), and `GraphCtx::get_property`/`get_value`'s `prop: &PropKey`
    parameter (`src/engine/context.rs`, plus the `LogicalGraph`/`LogicalSnapshot`/`NoopCtx`
    implementations) to `prop_key_id: u16` — callers now always pass an already-resolved id
    (from step 10), mirroring `add_vertex(id, label_id)`
13. Change `encode_props`/`decode_props`'s wire format
    ([`src/store/rocks/encoding.rs:224`](../src/store/rocks/encoding.rs#L224)/
    [`:317`](../src/store/rocks/encoding.rs#L317)) from `[u16 keylen | key bytes]` per
    property to `[u16 prop_key_id]` — a breaking on-disk format change, acceptable because
    no schema/data has shipped yet (§6, "Why property keys also get a numeric form")
14. Change `HasPropertyStep`, `PropertyStep`, `ValuesStep`
    (`src/engine/volcano/steps/{has_property,property,values}.rs`) to hold `prop_key_id: u16`
    instead of `PropKey`, mirroring `label_id` on `HasLabelStep`/`AddVStep`. `ValuesStep`
    additionally keeps the original string next to each resolved id (a list of
    `(SmolStr, u16)` instead of `SmallVec<[PropKey; 4]>`), so it can re-attach the name to a
    `GValue::Property` without a second `Schema` lookup at `produce()` time (§6)
15. Update `LogicalGraph::get_all_props`/`LogicalSnapshot::get_all_props`
    ([`src/graph.rs:546`](../src/graph.rs#L546)) to decode each `prop_key_id` to a string via
    `self.schema` before returning — the signature (`Vec<(PropKey, Primitive)>`) is
    unchanged, only the implementation. This is the only change needed for full
    `Vertex`/`Edge` materialization (§6, §8); `materialize`
    ([`src/gremlin/traversal.rs:76`](../src/gremlin/traversal.rs#L76)) needs no edit for its
    `GValue::Vertex`/`GValue::Edge` arms — only `ValuesStep`'s not-yet-implemented
    all-properties branch, when it's eventually built, needs the equivalent decode
16. Add `StoreError::SchemaViolation`, `SchemaExhausted`
17. Fix `GraphCtx::get_value("label")` in `LogicalGraph` to return `Primitive::String`
    (§8) — paired with step 15's property-key decode, these are the only two decode points
    in this phase

**Phase 3 — `SchemaManagement` (JanusGraph-style explicit declaration + CAS)**

18. New module `src/schema/management.rs`: `SchemaManagement`,
    `PropertyKeyMaker`, `VertexLabelMaker`, `EdgeLabelMaker`, `set_edge_mode`,
    `set_schema_mode`
19. Add `declare_vertex_label`/`declare_edge_label`/`declare_prop_key`/`declare_edge_mode`/
    `declare_schema_mode` to `Schema`; `declare_edge_mode` enforces the one-way ratchet,
    the rest are mode-independent and conflict-checked against the existing config (if any)
20. Add `StoreError::SchemaConflict`; implement `commit()`'s version-CAS check
    (capture `base_version` at `open_management()`, compare-and-apply at `commit()`,
    reuse `StoreError::Conflict` on mismatch)
21. Add `GraphOptions`, `Graph::open_with_options()`, `Graph::open_management()`. No direct
    `Graph::set_schema_mode()`/`set_edge_mode()` mutators — both go through
    `SchemaManagement` only, so they're always persisted, versioned, and CAS-checked (§0)
22. Remove the empty `schema/index.rs`/`schema/validator.rs` stubs (or fill them in, once
    there's an actual index/validation design to put there); update
    `src/gremlin/tests.rs:583` off the removed `register_edge_label_with_config`

---

## Out of scope (future work)

- **Per-label property constraints** (JanusGraph `mgmt.addProperties(label, keys...)`):
  restricting *which* declared property keys may be set on a given vertex/edge label.
  This design only requires that a key be declared *somewhere*, not tied to a label. If
  this lands, it's the natural point to introduce symmetric `vertex_configs`/`edge_configs`
  per-label maps — deliberately not added now (§0).
- **Data-type enforcement**: `PropertyKeyMaker::data_type` is recorded but not yet checked
  against the `Primitive` value passed to `property()`.
- **Multi-valued properties** (`Cardinality::Set`/`Cardinality::List`): the storage layer
  is single-valued per key today (`set_property` upserts); `Cardinality` is defined now so
  the management API doesn't need a breaking change later.
- **Secondary indexes** (`mgmt.buildIndex(...)`): no index infrastructure exists yet.
- **Per-label edge multiplicity** (JanusGraph's `MULTI`/`SIMPLE`/`ONE2MANY`/`MANY2ONE`/
  `ONE2ONE`): a deliberate non-goal, not just unimplemented — RocksGraph's `EdgeMode` is a
  single graph-wide setting; see `design_multiple_edges.md` §2 for why.

---

## Affected files

| File | Change |
|------|--------|
| `src/store/rocks/encoding.rs` | Add `CF_SCHEMA`, kind discriminants incl. `SCHEMA_KIND_META`, key/value encode helpers. Change `encode_props`/`decode_props`'s per-property wire format from `[u16 keylen \| key bytes]` to `[u16 prop_key_id]` (§6) — a breaking on-disk format change |
| `src/store/rocks/store.rs` | Add schema CF to `open()`; add `load_schema(defaults: GraphOptions)` (bootstraps a fresh DB, otherwise ignored — §0) |
| `src/store/rocks/transaction.rs` | Include schema bytes (incl. metadata entry) in `WriteBatch` on schema change |
| `src/schema/definition.rs` | Add `mode`, `edge_mode`, `version`; remove `EdgeConfig`/`edge_configs`; add `resolve_*`, `declare_*`, `PropKeyConfig`, `DataType`, `Cardinality` |
| `src/schema/management.rs` | **New.** `SchemaManagement`, the three `*Maker` builders, `set_edge_mode` |
| `src/schema/mod.rs` | Add `pub mod management`; keep/replace `index`/`validator` stubs |
| `src/types/error.rs` | Add `StoreError::SchemaViolation`, `SchemaConflict`, `SchemaExhausted` |
| `src/types/element.rs` | `Property.key: PropKey` → `key: u16` (`prop_key_id`); `Vertex`/`Edge::get_property`/`get_value` take `prop_key_id: u16` instead of `&PropKey` (§6) |
| `src/api.rs` | Add `GraphOptions`, `Graph::open_with_options()`, `Graph::open_management()` (no direct mode setters — §0) |
| `src/engine/context.rs` | `GraphCtx` trait: add **one** new method, `fn schema(&self) -> Arc<RwLock<Schema>>` (mirrors `Graph::schema()`). `get_property`/`get_value`'s `prop: &PropKey` param becomes `prop_key_id: u16` (§6); `add_vertex`/`add_edge`/`set_property`/`get_all_props` keep their exact current signatures |
| `src/graph.rs` | `LogicalGraph`/`LogicalSnapshot` implement `schema()` as `Arc::clone(&self.schema)`; `get_property`/`get_value` take `prop_key_id: u16`; `get_all_props` decodes `prop_key_id → PropKey` internally via `self.schema` before returning (§6, §8) — its own signature is unchanged; `add_vertex`/`add_edge`/`set_property` otherwise **unchanged**; `add_edge` reads `schema.edge_mode` (not per-label `edge_config`); fix `get_value("label")` |
| `src/planner/logical_step/mod.rs` | `AddVStep`/`AddEStep`/`HasLabelStep`/`InOutStep`/`BothStep` label fields: `LabelId`/`SmallVec<[LabelId;4]>` → `SmolStr`/`SmallVec<[SmolStr;4]>`. `HasPropertyStep`/`PropertyStep`/`PropertiesStep` **unchanged** — logical steps stay string-typed regardless of what changes below `build_step` |
| `src/engine/volcano/builder.rs` | `build`/`build_step` take `schema: Arc<RwLock<Schema>>` (not `&mut dyn GraphCtx`); resolve every label *and property-key* name via `Schema`'s own methods (§5), once per `LogicalStep`, before constructing the physical step — this **is** the conversion boundary (§6) |
| `src/engine/volcano/steps/*.rs` | Structural steps (`AddVStep`, `HasLabelStep`, `InOutStep`, `BothStep`, ...) **unchanged**. `HasPropertyStep`/`PropertyStep` change `prop_key: PropKey` → `prop_key_id: u16`; `ValuesStep.property_keys` changes from `SmallVec<[PropKey;4]>` to a list of `(SmolStr, u16)` pairs so it can re-attach a name to `GValue::Property` without a second `Schema` lookup (§6) |
| `src/gremlin/traversal.rs` | `addV`/`addE`/`out`/`in`/`hasLabel` accept strings; `property()`/`has()`/`values()` unchanged; `GraphTraversal::build` passes `ctx` one call deeper into `PhysicalPlanBuilder::build`; `materialize`'s `GValue::Vertex`/`GValue::Edge`/`GValue::Property` arms are **unchanged** — the property-key decode they rely on moved into `get_all_props`/the physical steps themselves (§6, §8) |
| `src/gremlin/tests.rs` | Update the test at line 583 off the removed `register_edge_label_with_config` |
| `src/store/traits.rs` | **No change** — confirms the invariant that `GraphStore`/`S::Txn`/`S::Snapshot` never reference `Schema` (§3) |
| `docs/design_multiple_edges.md` | `EdgeConfig` per-label → `EdgeMode` graph-wide (companion update, already applied) |
| `src/types/label.rs` | `Label` struct can be removed — superseded by plain `SmolStr` at the API boundary |
