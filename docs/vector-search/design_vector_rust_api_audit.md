# Design: Vector API — Rust interface audit and refactor plan

Status: proposal

## Problem

The vector search feature (v0.1–v0.2) was developed incrementally and bolted onto the
existing public API surface without a holistic review.  As a result the vector-related
Rust interface deviates from the established patterns of the rest of the crate in five
distinct ways, making it harder to use and harder to document.

This document catalogues each deviation and proposes concrete refactoring steps.

---

## Deviations, inconsistencies, and inconveniences

### 1. Vector schema config types live in the wrong module

**Current state**

Users who want to declare a vector index must import from two separate paths:

```rust
use rocksgraph::schema::{DataType, GraphOptions, SchemaMode};   // existing
use rocksgraph::vector::{                                        // new, different
    VectorEntityType, VectorIndexConfig, AnnAlgorithm,
    HnswConfig, DistanceMetric, Quantization,
};
```

**Why it's wrong**

`VectorEntityType`, `VectorIndexConfig`, `AnnAlgorithm`, `HnswConfig`,
`DistanceMetric`, and `Quantization` are all schema-declaration types — they configure
what the graph's schema looks like, exactly as `DataType`, `EdgeMode`, and `SchemaMode`
do.  Placing them in `vector` rather than `schema` splits what is conceptually one
import into two.

**Proposed fix**

Re-export all six types from `rocksgraph::schema` (types stay defined in
`vector/traits.rs` — only the re-export path moves):

```rust
// schema/mod.rs
pub use crate::vector::traits::{
    AnnAlgorithm, DistanceMetric, HnswConfig, Quantization,
    VectorEntityType, VectorIndexConfig,
};
```

After this change a complete schema setup needs only one import namespace:

```rust
use rocksgraph::schema::{DataType, VectorEntityType, VectorIndexConfig, AnnAlgorithm, HnswConfig};
```

---

### 2. `rebuild_vector_index` returns `VectorError`, not `StoreError`

**Current state**

```rust
// Every other Graph / SchemaSession method:
pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError>
pub fn commit(self) -> Result<(), StoreError>

// The new method:
pub fn rebuild_vector_index(&self, ..) -> Result<(), VectorError>
```

**Why it's wrong**

Users writing error-handling code that unifies graph errors have to handle two
unrelated error types.  `?` propagation is broken — a function returning
`Result<_, StoreError>` cannot use `?` on `rebuild_vector_index` without an explicit
`map_err`.

There is also a structural cycle: `VectorError::Store(StoreError)` already exists,
so naively adding `StoreError::Vector(VectorError)` would make the two types mutually
recursive.

**Proposed fix**

Note: under the new design where `rebuild_vector_index` returns `StoreError`, the
vertex-scan step produces `StoreError` and propagates it natively via `?`.  The
`VectorError::Store(StoreError)` variant becomes dead code — no conversion is ever
needed, so it is simply deleted rather than converted to `Internal`.

1. Delete `VectorError::Store(StoreError)` — no callers remain.

2. Add a new `StoreError` variant:
   ```rust
   /// A vector index operation failed (dimension mismatch, capacity, I/O, etc.).
   VectorIndex(String),
   ```

3. Add `impl From<VectorError> for StoreError` so that `read_vector_config` and
   `UsearchHnswIndex::new` (both return `VectorError`) compose with `?` inside a
   function returning `StoreError`:
   ```rust
   impl From<VectorError> for StoreError {
       fn from(e: VectorError) -> Self {
           Self::VectorIndex(e.to_string())
       }
   }
   ```

4. Change `rebuild_vector_index` to return `Result<(), StoreError>`.

5. Make `VectorError` `pub(crate)` — it becomes an implementation detail of the vector
   subsystem, not a type users ever need to name.

---

### 3. Option types are poorly named and the open API is inconsistent

**Current state**

```rust
// Two functions, leaking the storage backend name and taking 4 positional args:
pub fn open(path) -> Result<Self, StoreError>
pub fn open_with_options(path, GraphOptions) -> Result<Self, StoreError>
pub fn open_with_rocksdb_options(path, GraphOptions, RocksOptions, VectorRuntimeOptions)
    -> Result<Self, StoreError>
```

Three problems compound each other:

1. `open_with_rocksdb_options` leaks the storage backend (`rocksdb`) as a public
   API name.  Callers shouldn't need to know which engine is underneath.
2. `VectorRuntimeOptions` (a runtime-only tuning type, same category as `RocksOptions`)
   was added as a fourth positional argument rather than being grouped with the others.
3. The option taxonomy is absent: there is no clear separation between persisted schema
   config, engine tuning, and index runtime limits.

**Proposed fix — expand `GraphOptions` and rename `VectorRuntimeOptions`**

`GraphOptions` is already the natural home for "everything needed to open a Graph".
Expand it in-place with two categorised sub-fields.  `RocksOptions` keeps its name
(the fields — `block_cache_size`, `write_buffer_size`, etc. — are intrinsically
RocksDB-specific; the crate is named `rocksgraph`; honesty about what is being
configured is more valuable than hypothetical future backend-portability).
`VectorRuntimeOptions` is renamed `IndexOptions` for brevity and taxonomy fit.

```rust
// Unchanged name: RocksOptions  (fields are RocksDB-specific; honesty > portability fiction)
pub struct RocksOptions { /* block_cache_size, write_buffer_size, … */ }

// Renamed: VectorRuntimeOptions → IndexOptions  (shorter, fits taxonomy)
pub struct IndexOptions { /* default_limit, per_index_overrides */ }

// Expanded: two new runtime-only fields added alongside the existing two
pub struct GraphOptions {
    // ── persisted (written to CF_SCHEMA on first create) ─────────────
    pub mode:      SchemaMode,
    pub edge_mode: EdgeMode,

    // ── runtime-only (never persisted; applied every open) ───────────
    pub storage: RocksOptions,   // engine tuning
    pub index:   IndexOptions,   // vector index memory limits
    // future: pub execution: ExecutionOptions,
}
```

`GraphOptions::default()` gains defaults for both new fields.

The public API collapses to two functions:

```rust
pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
    Self::open_with_options(path, GraphOptions::default())
}

pub fn open_with_options(
    path: impl AsRef<Path>,
    opts: GraphOptions,
) -> Result<Self, StoreError> {
    let store = Arc::new(RocksStorage::open(path, &opts.storage)?);
    store.recover_bulk_load_crash()?;
    let schema = store.load_schema(opts.mode, opts.edge_mode)?;
    let vector_indexes = { /* load_vector_configs as before */ };
    Ok(Self { store, schema, vector_indexes, index_options: opts.index, … })
}
```

`open_with_options` now takes a single named struct, not 3-4 positional arguments.
`open_with_rocksdb_options` is deleted entirely.

**Usage — users specify only what they care about:**

```rust
// Nothing to configure:
let g = Graph::open("./db")?;

// Schema mode only:
let g = Graph::open_with_options("./db", GraphOptions {
    mode: SchemaMode::Strict,
    ..Default::default()
})?;

// Storage tuning only:
let g = Graph::open_with_options("./db", GraphOptions {
    storage: RocksOptions { block_cache_size: 8 * 1024 * 1024 * 1024, ..Default::default() },
    ..Default::default()
})?;

// Schema + index limits:
let g = Graph::open_with_options("./db", GraphOptions {
    mode:  SchemaMode::Strict,
    index: IndexOptions { default_limit: Some(VectorIndexLimit { bytes: 512 * 1024 * 1024 }), ..Default::default() },
    ..Default::default()
})?;
```

**Backward compatibility**

`GraphOptions` is a public struct with all-public fields.  Adding new fields is
technically a breaking change for callers using full struct literal syntax.  Audit
of the codebase found **8 internal call sites** that need `..Default::default()`
added — all in test files, mechanical one-line fixes each.

Callers already using `GraphOptions::default()` or `GraphOptions { mode: …, ..Default::default() }`
require no changes.

`VectorRuntimeOptions` is renamed to `IndexOptions`.  Its old name disappears
from the public API; callers update the type name.  It currently lives inside
`pub mod vector` which will become `pub(crate)` once §4 is applied, so the rename
is contained.  `RocksOptions` keeps its existing name; no callers need to change it.

`load_schema` is updated to accept only the two persisted fields (`mode`, `edge_mode`)
rather than the whole `GraphOptions` struct, keeping the schema layer unaware of
runtime-only config.

---

### 4. Implementation internals are publicly exported from `pub mod vector`

**Current state**

`rocksgraph::vector` re-exports:

| Symbol                                                             | Category                                                |
| ------------------------------------------------------------------ | ------------------------------------------------------- |
| `VectorEntityType`, `VectorIndexConfig`, `VectorRuntimeOptions`, … | User-facing config                                      |
| `VectorError`                                                      | Error type                                              |
| `BruteForceIndex`                                                  | Internal implementation                                 |
| `EntityKey`                                                        | Internal key type                                       |
| `cosine_sim`                                                       | Internal utility                                        |
| `load_vector_index`                                                | Internal persistence helper                             |
| `VectorIndex` (trait)                                              | Internal implementor contract                           |
| `VectorIndexLimit`, `IndexLimitOverride`                           | User-facing tuning, but only via `VectorRuntimeOptions` |

Users are presented with `BruteForceIndex`, `EntityKey`, `cosine_sim`, and
`VectorIndex` — types they cannot meaningfully use and should not need to know exist.

**Proposed fix**

After deviations 1–3 are resolved the only user-facing types that were in `vector`
have moved elsewhere:

| Type                                                                                  | New home                              |
| ------------------------------------------------------------------------------------- | ------------------------------------- |
| `VectorEntityType`, `VectorIndexConfig`, `AnnAlgorithm`, …                            | `rocksgraph::schema` (§1)             |
| `IndexOptions` (was `VectorRuntimeOptions`), `VectorIndexLimit`, `IndexLimitOverride` | `rocksgraph::GraphOptions.index` (§3) |
| `VectorError`                                                                         | `pub(crate)` (§2)                     |

At that point nothing in `vector` needs to be public:

- Change `pub mod vector` → `pub(crate) mod vector` in `lib.rs`.
- Replace all the `pub use` re-exports in `vector/mod.rs` with `pub(crate) use`
  entries for internal consumers.

No user ever needs to write `use rocksgraph::vector` again.

---

### 5. `nearest` and `similarity` are invisible in the docs

**Current state**

The two traversal steps are defined in `TraversalBuilder` (a `pub trait`), so they
compile and work, but there are zero doctest examples anywhere in the public API
surface (`lib.rs`, `api.rs`, `ReadSession`, `TraversalBuilder`).  A user reading
`cargo doc` output would not discover them.

Compare: every other step on `TraversalBuilder` (`out`, `has`, `values`, `count`, etc.)
appears in the `lib.rs` quick-start example or an explicit `/// # Example` block.

**Proposed fix**

Add a worked example to `TraversalBuilder::nearest`'s doc comment:

```rust
/// Search for the *k* vertices whose `FloatVector` property is closest to
/// the given query vector (cosine distance).
///
/// # Example
///
/// ```
/// # use rocksgraph::{Graph, TraversalBuilder};
/// # let dir = tempfile::tempdir().unwrap();
/// # let graph = Graph::open(dir.path()).unwrap();
/// // Declare the index once (SchemaMode::Auto registers the property key implicitly).
/// let mut mgmt = graph.open_schema();
/// // All vector config types live in rocksgraph::schema after §1 is applied.
/// use rocksgraph::schema::{VectorIndexConfig, VectorEntityType, DistanceMetric, AnnAlgorithm};
/// mgmt.add_vector_index(VectorIndexConfig {
///     property: "embedding".into(),
///     entity_type: VectorEntityType::Vertex,
///     dimension: 3,
///     metric: DistanceMetric::Cosine,
///     algorithm: AnnAlgorithm::Hnsw(Default::default()),
///     quantization: Default::default(),
/// });
/// mgmt.commit().unwrap();
///
/// // Rebuild to pick up any existing embedding data.
/// // Returns StoreError after §2 is applied — no VectorError import needed.
/// graph.rebuild_vector_index(VectorEntityType::Vertex, "embedding").unwrap();
///
/// // Query.
/// let mut snap = graph.read();
/// let nearest = snap
///     .g()
///     .V([])
///     .nearest("embedding", vec![0.1, 0.2, 0.3], 10)
///     .to_list()
///     .unwrap();
/// # graph.close().unwrap();
/// ```
fn nearest(mut self, prop_key: &str, query: Vec<f32>, k: usize) -> Self { … }
```

---

## Summary table

| #   | Deviation                                                                     | Severity                                             | Fix effort                                                                                              |
| --- | ----------------------------------------------------------------------------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| 1   | Schema config types in `vector`, not `schema`                                 | High — affects every user who declares an index      | Low — add re-exports to `schema/mod.rs`                                                                 |
| 2   | `rebuild_vector_index` returns `VectorError`                                  | High — breaks `?` propagation for all callers        | Medium — add `StoreError::VectorIndex`, delete `VectorError::Store`, add `From` impl                    |
| 3   | `open_with_rocksdb_options` has 4th positional arg; option types inconsistently placed | Medium — breaking API change, inconsistent placement | Low — expand `GraphOptions` with `storage: RocksOptions`/`index: IndexOptions` sub-fields, rename `VectorRuntimeOptions` → `IndexOptions`, delete old function |
| 4   | Internal types leaked through `pub mod vector`                                | Medium — pollutes API surface and docs               | Low once 1–3 are done; flip module to `pub(crate)`                                                      |
| 5   | No doctest examples for vector traversal steps                                | Low — discoverability gap, no compile breakage       | Low — one example block                                                                                 |

---

## Recommended refactor order

1. **§1** — move schema config types to `rocksgraph::schema`.  Zero behaviour change,
   purely additive re-exports.  Can ship immediately.

2. **§3** — rename `VectorRuntimeOptions` → `IndexOptions` (keep `RocksOptions` as-is);
   expand `GraphOptions` with `storage: RocksOptions` and `index: IndexOptions` fields;
   collapse open API to `open(path)` + `open_with_options(path, GraphOptions)`;
   update `load_schema` to take only the two persisted fields;
   delete `open_with_rocksdb_options`;
   fix the 8 internal test call sites (`..Default::default()`).
   Do before any external release.

3. **§2** — add `StoreError::VectorIndex(String)`, `impl From<VectorError> for
   StoreError`, change `rebuild_vector_index` return type, delete
   `VectorError::Store(StoreError)`.  Make `VectorError` `pub(crate)`.

4. **§4** — flip `pub mod vector` to `pub(crate)` once the above are done.  Everything
   internal; no user-facing change.

5. **§5** — add doctest to `TraversalBuilder::nearest`.  Can happen at any point
   alongside 1–4.
