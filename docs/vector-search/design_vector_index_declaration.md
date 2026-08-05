# Design: Vector Index Declaration — the Auto-Schema Tension

Status: proposal.

---

## 1. The core constraint

A regular property like `"name": "Alice"` has **no structural parameters**.
It is stored as a key-value pair, and the storage engine needs nothing more than
"this is a String." Even in auto-schema mode, `schema.resolve_prop_key("name", DataType::String)`
interns the key on first write, and everything proceeds lazily.

A vector index is different. HNSW and usearch allocate internal data structures
at construction time based on **dimension** and **metric**:

```rust
let mut index = usearch::Index::new(&usearch::IndexOptions {
    dimensions: 1536,  // must be known at construction
    metric: usearch::Metric::Cos,
    quantization: usearch::ScalarKind::F32,
    ..Default::default()
})?;
```

These parameters are **not inferrable from data**. Given a `Vec<f32>` of length
1536, you don't know whether the user intended cosine or euclidean distance.
You also don't know whether the user wants an index at all — storing vectors
without ANN acceleration is a valid use case (batch export, manual comparison,
future index creation).

This is fundamentally different from a regular property. The question is not
"strict vs auto schema mode" but **"where does the vector index declaration live?"**

---

## 2. How the three modes handle it

### 2a. Strict-schema mode

`GraphOptions` only carries `mode` and `edge_mode` — it contains no schema content.
All schema elements (vertex labels, edge labels, property keys, and vector indexes)
are declared via `SchemaSession` in both strict and auto modes. The difference is
enforcement: in strict mode the engine rejects any label or property key that was
not pre-declared; in auto mode unknown names are registered on first use.

```rust
// GraphOptions sets mode only — no schema content here
let g = Graph::open_with_options(path, GraphOptions {
    mode: SchemaMode::Strict,
    ..Default::default()
})?;

// All schema — labels, keys, and vector index — declared together via SchemaSession
let mut sess = g.open_schema();
sess.add_vertex_label("doc")
    .add_property_key("title",     DataType::String)
    .add_property_key("embedding", DataType::FloatVector)  // optional — add_vector_index implies this
    .add_vector_index(VectorIndexConfig {
        entity_type: VectorEntityType::Vertex,
        property:    "embedding",
        dimension:   1536,
        metric:      DistanceMetric::Cosine,
        algorithm:   AnnAlgorithm::Hnsw { m: 16, ef_construction: 200 },
    });
sess.commit()?;
```

The key invariant: `prop_key_id` is interned at `sess.commit()` time, WAL replay
uses the persisted CF_SCHEMA config, and subsequent opens auto-load all declared
schema and vector index config without any re-declaration.

### 2b. Auto-schema mode — explicit `add_vector_index()` (recommended)

The rest of the graph stays fully lazy. Only vector indexes require an
explicit registration call before the first vector write:

```python
# Everything up to here works exactly like today:
g = Graph("./data")
tx = g.tx()
tx.g().addV("doc").property("id", 1).property("title", "hello").next()
tx.commit()

# Before any vector writes, declare the index via SchemaSession:
with g.open_schema() as mgmt:
    # add_property_key is optional — add_vector_index implicitly registers
    # "embedding" as DataType.FLOAT_VECTOR if not already declared.
    mgmt.add_property_key("embedding", DataType.FLOAT_VECTOR)
    mgmt.add_vector_index(VectorIndexConfig(
        entity_type=VectorEntityType.VERTEX,
        property="embedding",
        dimension=1536,
        metric=DistanceMetric.COSINE,
        algorithm=HnswConfig(m=16, ef_construction=200),
    ))
    mgmt.commit()

# Now vector writes work normally:
tx = g.tx()
tx.g().addV("doc").property("id", 2) \
    .property("embedding", Vector(embedding)).next()
tx.commit()
```

The declaration call:
1. Interns the `prop_key_id` for `"embedding"` if not already interned
2. Creates the `UsearchHnswIndex` with the declared dimension and metric
3. Inserts it into `graph.vector_indexes`
4. From this point forward, behavior is identical to strict mode

### 2c. Auto-schema — silently auto-create on first write (rejected)

Infer dimension from the first vector seen. Default to cosine. This is too
fragile:

- Wrong-dimension writes would fail confusingly (not "you forgot to declare an
  index" but "dimension mismatch: expected 1536, got 768")
- Metric choice is semantically significant — cosine and euclidean produce
  different rankings. Guessing wrong silently breaks search quality
- Silent ongoing memory growth: each insert adds vector data and HNSW adjacency
  entries without the user realizing an index is being built. No upfront 6 GB
  spike, but continuous accumulation with no visibility
- Race condition: two concurrent first-writes to the same property create
  competing indexes

---

## 3. The split: regular graph data vs vector indexes

|                            | Regular properties                         | Vector indexes                                                                     |
| -------------------------- | ------------------------------------------ | ---------------------------------------------------------------------------------- |
| **Can be lazily created?** | Yes — key is just a key                    | **No** — requires `(dimension, metric)` upfront                                    |
| **Declaration location**   | First `.property("key", value)` call       | `SchemaSession::add_vector_index()` — same in both auto and strict modes           |
| **Inferrable from data?**  | Yes — `"Alice"` → `String`, `42` → `Int64` | **No** — 768 floats could be cosine or euclidean                                   |
| **Stored without index?**  | Not applicable                             | Yes — `GValue::FloatVector` is stored as a regular property value. Valid use case. |

---

## 4. Design decision: declaration required, schema mode optional

|                                     | `SchemaSession::add_vector_index()` (chosen) | Silent auto-create on first write                                      |
| ----------------------------------- | -------------------------------------------- | ---------------------------------------------------------------------- |
| Declaration location                | Explicit call before first write (both modes)| Implicit on first write                                                |
| `prop_key_id` stability             | Guaranteed (interned at `sess.commit()`)     | Race risk if declared concurrently                                     |
| Metric/dimension source             | Call arguments — explicit user intent        | Inferred from data (ambiguous)                                         |
| Surprise index creation             | No                                           | Yes — memory spike on first insert                                     |
| Portability                         | ✅ structural params in CF_SCHEMA only        | ✅ same                                                                 |
| Inconsistency with schema mode      | Minor: one required upfront call per index   | None                                                                   |
| WAL replay correctness              | Clean                                        | Fragile — depends on replay seeing the index config at the right point |

**Decision:** Require `SchemaSession::add_vector_index()` before the first vector
write, regardless of schema mode (auto or strict). Regular graph data stays fully lazy. Vector
indexes are explicit because they are structurally different: query accelerators
with fixed structural parameters, not opaque data fields.

The minor inconsistency ("auto-schema except for vector index declaration")
is documented honestly: "Vector indexes always require an explicit declaration
step because dimension and metric are structural parameters."

---

## 5. What happens without a declaration

If a user writes a `FloatVector` to a property that has no declared vector index,
the engine stores it silently as a regular property value — no ANN acceleration.

The user can later call `add_vector_index()` via `open_schema()`, then
`rebuild_vector_index()` directly on `Graph` to add ANN search to existing data:

```python
with g.open_schema() as mgmt:
    mgmt.add_property_key("embedding", DataType.FLOAT_VECTOR)  # optional
    mgmt.add_vector_index(VectorIndexConfig(
        entity_type=VectorEntityType.VERTEX,
        property="embedding",
        dimension=1536,
        metric=DistanceMetric.COSINE,
        algorithm=HnswConfig(m=16, ef_construction=200),
    ))
    mgmt.commit()
# commit() triggers the HNSW build from CF_VERTICES in one scan;
# explicit rebuild_vector_index() is only needed after subsequent SST bulk loads
# or tombstone accumulation (see docs/api/design_api_overview.md §5d).
```

This is friendlier than rejecting the write. It matches the "SQLite philosophy":
accept data first, optimize later.

---

## 6. Declaration persistence: `__meta` CF storage

The declaration must survive restarts, or every `Graph::open` loses all vector
index knowledge and WAL replay silently skips all prior vector mutations.

**Decision:** `add_vector_index()` persists the config to `__meta` CF under
key `vector_index_config/{entity_type}/{prop_key}`. `Graph::open` loads all
stored configs before WAL replay:

```
1. Graph::open()
2. Scan __meta CF for keys matching "vector_index_config/*"
3. For each stored config:
   - Intern prop_key_id (registering the key if auto-schema)
   - Construct usearch::Index with stored (dimension, metric)
   - Insert into self.vector_indexes
4. Run WAL replay → now all declared indexes exist, no entries skipped
5. Graph is ready
```

The user still calls `add_vector_index()` explicitly on first use — the method
stores the config and creates the in-memory index. On subsequent opens, the graph
auto-reloads all stored configs without user intervention.

**Semantics:** `add_vector_index()` means "register this index permanently
until explicitly dropped." It is NOT a per-session configuration. This is the
same model as SQLite's `CREATE INDEX` — the index outlives the connection.

`drop_vector_index()` removes both the in-memory index and the `__meta` CF entry.
On the next open, the index is gone and its WAL entries will be skipped during
replay.

**What is NOT persisted — environmental config:** `memory_limit_bytes` is
**not** part of `VectorIndexConfig` and is never written to CF_SCHEMA. It is
supplied per-open via `GraphOptions::index` as a `IndexOptions`
entry. This preserves portability: a database file created on a 64 GB server
works correctly on a 8 GB laptop because no memory constraint is baked into the
file. The caller applies whatever limit is appropriate for the current machine.

```rust
// Environmental config — supplied at open, never persisted
#[derive(Debug, Clone)]
pub struct VectorIndexLimit {
    pub memory_limit_bytes: usize,  // must be > 0; use default_limit: None for unlimited
}

pub struct IndexLimitOverride {
    pub entity_type: VectorEntityType,
    pub property:    SmolStr,
    pub limit:       VectorIndexLimit,
}

pub struct IndexOptions {
    /// Default limit applied to every vector index.
    /// None = unlimited (expert escape hatch).
    pub default_limit: Option<VectorIndexLimit>,

    /// Per-index overrides matched by (entity_type, property).
    /// Takes precedence over default_limit. Indexes with no matching
    /// override fall back to default_limit; if that is also None, unlimited.
    pub per_index_overrides: Vec<IndexLimitOverride>,
}

/// Storage-level hardware tunables — supplied per-open, never persisted.
/// Leave as Default unless you have profiling data justifying a change.
#[derive(Debug, Clone)]
pub struct RocksOptions {
    pub max_open_files: i32,
    pub block_cache_mb: usize,
    pub write_buffer_mb: usize,
    pub max_background_jobs: i32,
    pub custom_rocks_modifier: Option<std::sync::Arc<dyn Fn(&mut rocksdb::Options) + Send + Sync>>,
}

// GraphOptions carries runtime opts and storage tunables, not structural config
pub struct GraphOptions {
    pub mode:           SchemaMode,
    pub edge_mode:      EdgeMode,
    pub storage:        RocksOptions,
    pub index: IndexOptions,
}
```

## 7. Edge case: index config changes

**Dimension change** (e.g., model upgrade from 384 → 768). An index's dimension and
metric cannot be changed in-place. Replacement is achieved by declaring a new index
under a different property name (§6 add-new-then-drop-old pattern). In both auto and
strict mode:

```python
with g.open_schema() as mgmt:
    mgmt.drop_vector_index(entity_type=VectorEntityType.VERTEX, property="embedding")
    mgmt.commit()

with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(
        entity_type=VectorEntityType.VERTEX, property="embedding",
        dimension=768, metric=DistanceMetric.COSINE,
        algorithm=HnswConfig(m=16, ef_construction=200),
    ))
    mgmt.commit()  # triggers rebuild from CF_VERTICES
```

Step 3 (the rebuild triggered by `commit()`) does a cold-start scan of existing FloatVectors. Vectors with mismatched dimensions are counted and returned in a `SkippedCount` field on the result, and a warning is logged with each skipped entity key. They are NOT silently dropped.

**Metric change** (cosine → euclidean):
Same flow. Index can't be reconfigured in-place because the internal ANN graph
is parameterized by the metric used during construction.

---

## 8. Implementation notes

### `add_vector_index` signature

`add_vector_index` is a method on `SchemaSession` (returned by `graph.open_schema()`).
The schema CAS commit in `session.commit()` triggers the two-phase build:

```rust
impl SchemaSession {
    pub fn add_vector_index(&mut self, config: VectorIndexConfig) -> &mut Self { ... }
    pub fn commit(self) -> Result<(), StoreError> { ... }
    // Phase 1: fast CAS writes index entry with state=Building to CF_SCHEMA
    // Phase 2: bulk scan + HNSW build + WAL catch-up + state=Ready
}
```

### Locking

- Phase 2 calls `graph.vector_indexes.write()` to insert the new index after build
- Interns `prop_key_id` via `schema.resolve_prop_key(property, DataType::FloatVector)`
  (in auto mode, this registers the key if not already present; `add_property_key` call
  in the session performs the same registration and is optional but makes the type
  declaration explicit)
- Index construction happens in Phase 2 using the (dimension, metric) from the config

### Concurrent declaration calls

`SchemaSession::commit()` performs a CAS on the schema version — concurrent sessions
conflict and the loser receives `StoreError::Conflict`. If two sessions race to declare
the same index:

1. Session A wins the CAS: Phase 1 writes the index entry to CF_SCHEMA; Phase 2
   builds the HNSW index and updates state to `Ready`.
2. Session B loses the CAS: `commit()` returns `StoreError::Conflict`. The caller
   may retry with a fresh `open_schema()` call — on retry, Phase 1 discovers the
   index already exists and returns `VectorError::IndexAlreadyExists`.

The declaration is never silently overwritten. To change dimension/metric, call
`drop_vector_index()` via a new `SchemaSession` first.

### Rebuild path

`rebuild_vector_index(property)` scans the props CF for all `FloatVector` values
matching `(entity_type, prop_key_id)`, inserts each into the HNSW index, and
writes the snapshot. This is the same code path as the cold-start scan in
`design_vector_search.md` §7b.

### WAL replay compatibility

**Before declaration:** The commit path writes a WAL entry only for properties
that have a declared index. If no index is declared, no WAL entry is written.
So "WAL entries recorded before declaration" cannot exist in the normal flow.

**After `drop_vector_index()`:** WAL entries from the period when the index existed
are still in `CF_VECTOR_WAL`. During `Graph::open` replay, entries for a
dropped index are silently skipped — the config is gone from `__meta` CF, so
no matching `VectorIndex` exists. This is correct: the user explicitly chose
to remove the index and its history.

**After re-declaration with different dimension:** Old WAL entries have the
wrong vector length. The replay engine catches `DimensionMismatch` and skips
those entries, logging a warning with the WAL seqno.
