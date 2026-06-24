# Design: In-Memory Storage Backend

## Problem

RocksDB is the only storage backend, and it requires compiling `librocksdb-sys` (a large C++
codebase). This is a significant barrier for:

- **First-time users** wanting to try the project — `cargo run --example basic_usage` takes minutes of C++ compilation
- **Rapid prototyping** — iterating on traversal logic shouldn't require a persistent store
- **Test iteration** — current tests spin up tempdirs and RocksDB instances even when only testing graph logic (not storage)

The `GraphStore` / `GraphTransaction` / `GraphSnapshot` traits already define a clean pluggable
abstraction. The only missing piece is the implementation.

## Design Goals

1. **Zero external dependencies.** Pure Rust, no C FFI, no new crates beyond what's already in `Cargo.toml`.
2. **Single-session enforcement.** Only one `TxSession` or `ReadSession` can be active at a time.
   Attempting a second concurrent session returns `StoreError::BackendBusy`.
3. **Identical single-session behavior to RocksDB.** Every graph invariant enforced by
   `LogicalGraph` — bidirectional edge writes, degree tracking, tombstone visibility, read-your-writes,
   duplicate detection — works identically because it all lives in `LogicalGraph`, not the store.
4. **Minimal implementation.** ~400-600 lines. No OCC, no MVCC, no compaction — just the
   `GraphStore` trait methods backed by plain Rust collections.

## Non-Goals

- **Multi-transaction isolation.** Single-session enforcement eliminates the need entirely.
  Tests that verify OCC conflict detection (`add_edge_vs_add_same_edge_handmade`, etc.) continue
  to use the RocksDB backend.
- **Persistence.** In-memory data is lost when the `Graph` handle is dropped. This is by design.

## Concurrency Model

The store wraps all internal state in `Arc<Mutex<InMemoryData>>`. Session creation uses
`Mutex::try_lock()`:

- **Success** → the session holds the `MutexGuard` for its lifetime; read-your-writes is
  automatic since no other session can interleave.
- **Failure** → returns `StoreError::BackendBusy`, a new error variant.

Because `MutexGuard` is `!Send`, sessions cannot be moved to other threads — this is an
additional safety property at the type level.

### New Error Variant

```rust
// types/error.rs
pub enum StoreError {
    // ... existing variants ...
    /// The backend cannot create a new session because another is still active.
    /// Retry after the current session is dropped.
    BackendBusy(String),
}
```

### Trait Change: `begin()` and `snapshot()` become fallible

`InMemoryStore` needs to signal session conflicts. The cleanest path is to make the trait
methods return `Result`:

```rust
// store/traits.rs
pub trait GraphStore {
    type Snapshot: GraphSnapshot;
    type Txn: GraphTransaction;

    fn snapshot(&self) -> Result<Self::Snapshot, StoreError>;
    fn begin(&self) -> Result<Self::Txn, StoreError>;
}
```

RocksDB's `begin()` and `snapshot()` never fail (they just allocate handles), so they return
`Ok(...)`. This is a minimal ripple — callers in `api.rs` and `graph.rs` tests add a `?`.
Distributed backends would also benefit from this (connection pool exhaustion, network errors).

## Internal Data Structures

```rust
/// The shared in-memory state, protected by a Mutex.
#[derive(Default)]
struct InMemoryData {
    // Maps use the same key encoding as RocksDB (big-endian bytes) to keep
    // ordering semantics identical for prefix scans.
    vertices:      BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_vertex_key → encoded_VertexValue
    edges_out:     BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_edge_key(OUT) → encoded_EdgeValue
    edges_in:      BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_edge_key(IN)  → encoded_EdgeValue
    vertex_degree: BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_vertex_key    → encoded_VertexDegree
    schema:        BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_schema_key    → encoded_schema_value
}

/// A write transaction: holds the global lock and a local staging buffer.
/// On commit(), staging is merged into the locked data. On abort()/drop, staging is discarded.
struct InMemoryTxn {
    guard: InMemoryGuard,            // MutexGuard<'static, InMemoryData>
    staging: InMemoryData,           // local writes, checked first on reads
}

/// A read-only snapshot: holds the global lock for its lifetime.
/// No staging buffer — reads go directly against `InMemoryData`.
struct InMemorySnapshot {
    guard: InMemoryGuard,            // MutexGuard<'static, InMemoryData>
}
```

Key design decisions:

- **BTreeMap for ordered iteration.** `get_adjacent_edges`, `scan_vertices`, and `scan_edges`
  rely on ordered key scans. `BTreeMap` gives `range()` for O(log n) prefix scans — same
  iteration pattern as RocksDB iterators.
- **Reuse RocksDB encoding layer.** `crate::store::rocks::encoding` already provides
  `encode_vertex_key`, `decode_vertex_key`, `encode_edge_key`, `decode_edge_key`,
  `VertexValue::encode/decode`, `EdgeValue::encode/decode`, `VertexDegree::encode/decode`,
  `encode_props`, `build_lazy_vertex`, `build_lazy_edge`, etc. The in-memory backend calls
  the same functions; it just reads/writes `BTreeMap<Vec<u8>, Vec<u8>>` instead of RocksDB
  column families.
- **Visibility: `pub(crate)`.** The store is not user-facing — users just call
  `Graph::open_in_memory()`.

## Transaction Read/Write Logic

```
Read path (get_vertex/get_edge/get_adjacent_edges/scan_*):
  1. Check staging map first (read-your-own-writes within the txn)
  2. If not found AND not marked as deleted in staging, fall back to locked global data

Write path (put_vertex/put_edge/delete_*/put_schema_entry):
  1. Write directly to staging map (local buffer)
  2. Mark as dirty for commit()

Commit:
  1. Merge staging maps into locked global data
     (drain staging maps, overwriting matching keys in global data)
  2. Clear staging
  3. Success — no conflict possible since we hold the only lock

Abort:
  1. Clear staging
  2. Lock releases on drop
```

Because the session owns the `MutexGuard`, there is zero contention between reads and writes —
the staging layer exists only to allow rollback (abort restores the pre-transaction state).

## Public API

```rust
// A new constructor on Graph, or an associated function
impl Graph {
    /// Create an empty in-memory graph. No RocksDB dependency, no disk I/O.
    /// Only one session can be active at a time; attempting a second session
    /// returns `StoreError::BackendBusy`.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let store = Arc::new(InMemoryStore::new());
        let schema = store.load_schema(GraphOptions::default())?;
        Ok(Graph { store: Arc::new(store), schema: Arc::new(RwLock::new(schema)) })
    }
}
```

Wait — `Graph` currently holds `store: Arc<RocksStorage>`. We have two options:

### Option A: Generic `Graph<S: GraphStore>`

```rust
pub struct Graph<S: GraphStore = RocksStorage> {
    store: Arc<S>,
    schema: Arc<RwLock<Schema>>,
}
```

Users who don't specify `S` get RocksDB (backward compatible). Users who want in-memory write
`Graph::<InMemoryStore>::open_in_memory()`. This is the cleanest long-term design.

**Impact:** `ReadSession` and `TxSession` become generic too:
```rust
pub struct ReadSession<S: GraphStore = RocksStorage> { ctx: LogicalSnapshot<S> }
pub struct TxSession<S: GraphStore = RocksStorage> { ctx: LogicalGraph<S> }
```

Since `RocksStorage` is the default type parameter, existing code compiles unchanged. Only code
that names `ReadSession` or `TxSession` explicitly in type annotations needs updating (and those
are rare — users mostly let type inference handle it).

### Option B: Arc-dyn Trait Object

```rust
pub struct Graph {
    store: Arc<dyn DynGraphStore>,
    schema: Arc<RwLock<Schema>>,
}
```

Where `DynGraphStore` is a helper trait that boxes `begin()` / `snapshot()` return types.

**Impact:** Requires boxing transaction and snapshot types, adding virtual dispatch overhead on
every store call. `LogicalGraph` currently uses static dispatch (generic over `S`). Worth
avoiding.

### Recommendation: Option A (generic with default)

The type parameter ripples through a few internal signatures but adds no runtime overhead.
Users never see the generics unless they opt in. A small `Store` type alias keeps the public
API ergonomic:

```rust
pub type Graph = Graph<RocksStorage>;
pub type InMemoryGraph = Graph<InMemoryStore>;
```

Wait, we can't alias a type with default parameters that way. A cleaner approach: keep `Graph`
as-is for backward compatibility and add a new top-level type:

```rust
/// RocksDB-backed graph (backward compatible, no change).
pub struct Graph { store: Arc<RocksStorage>, ... }

/// In-memory graph for development and testing.
pub struct InMemoryGraph { store: Arc<InMemoryStore>, ... }
```

Both implement:
```rust
pub trait OpenGraph {
    fn read(&self) -> ReadSession<???>;
    fn begin(&self) -> TxSession<???>;
    fn open_management(&self) -> SchemaManagement;
}
```

But this duplicates code. Simpler: make `Graph<S>` generic with a default, and provide a type
alias + constructor:

```rust
// The generic type, defaulting to RocksStorage
pub struct Graph<S = RocksStorage> {
    store: Arc<S>,
    schema: Arc<RwLock<Schema>>,
}

// Convenience constructors
impl Graph<RocksStorage> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> { ... }
    pub fn open_with_options(path: impl AsRef<Path>, opts: GraphOptions) -> Result<Self, StoreError> { ... }
}

impl Graph<InMemoryStore> {
    pub fn open_in_memory() -> Result<Self, StoreError> { ... }
}

// Both share:
impl<S: GraphStore> Graph<S> {
    pub fn read(&self) -> ReadSession<S> { ... }
    pub fn begin(&self) -> TxSession<S> { ... }
    pub fn open_management(&self) -> SchemaManagement { ... }
}
```

**This is the recommended approach.** No breaking change to `Graph::open(path)` callers. New
users write `Graph::open_in_memory()` and everything just works.

## Implementation Order

### Phase 1: Foundation

1. **Add `StoreError::BackendBusy` variant** (`types/error.rs`)
2. **Make `GraphStore::begin()` and `snapshot()` fallible** (`store/traits.rs`)
   - Update `RocksStorage` impl: wrap returns in `Ok(...)`
   - Update `MockStore` test impl
   - Update callers in `api.rs`, `graph.rs` tests — add `?` where needed
3. **Make `Graph<S>`, `ReadSession<S>`, `TxSession<S>` generic with `S = RocksStorage` default** (`api.rs`)
   - Existing code paths unchanged
   - `open()` and `open_with_options()` move to `impl Graph<RocksStorage>`

### Phase 2: In-Memory Store

4. **Add `InMemoryStore`, `InMemoryTxn`, `InMemorySnapshot`** (`store/in_memory/mod.rs`, new module)
   - `InMemoryData` with 5 `BTreeMap<Vec<u8>, Vec<u8>>` fields
   - `InMemoryStore` with `Arc<Mutex<InMemoryData>>`
   - `InMemoryTxn`: holds `MutexGuard` + staging `InMemoryData`
   - `InMemorySnapshot`: holds `MutexGuard`
   - Register the module in `store/mod.rs`
5. **Implement `GraphStore` for `InMemoryStore`**
6. **Implement `GraphTransaction` for `InMemoryTxn`** (~15 methods, reusing encoding layer)
7. **Implement `GraphSnapshot` for `InMemorySnapshot`**
8. **Add unit tests** for `InMemoryStore` (mirroring RocksDB transaction tests)

### Phase 3: Public API

9. **Add `Graph::open_in_memory()` constructor** (`impl Graph<InMemoryStore>`)
10. **Add example**: `examples/in_memory.rs` — demonstrate basic usage, upserts, filtering
11. **Wire into existing engine tests** — add a helper that creates `Graph<InMemoryStore>` so
    integration tests can optionally run against the in-memory backend

### Phase 4: Polish

12. **Update README** — mention `open_in_memory()` as a zero-dependency quick-start path
13. **Verify** `cargo run --example in_memory` compiles instantly (no C++ build)

## Affected Files

| File | Change |
|------|--------|
| `src/types/error.rs` | Add `BackendBusy(String)` variant |
| `src/store/traits.rs` | `begin()` and `snapshot()` return `Result`; update `MockStore` |
| `src/store/mod.rs` | Add `pub mod in_memory;` |
| `src/store/in_memory/mod.rs` | New: `InMemoryStore`, `InMemoryTxn`, `InMemorySnapshot`, `InMemoryData` |
| `src/api.rs` | Generic `Graph<S>`, `ReadSession<S>`, `TxSession<S>`, add `open_in_memory()` |
| `src/lib.rs` | Re-export `InMemoryStore` (pub use) if needed |
| `src/graph.rs` | Update test helpers for fallible `begin()` (add `?`) |
| `examples/in_memory.rs` | New example |
| `README.md` | Document `open_in_memory()` |

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| `MutexGuard<'static>` requires unsafe lifetime extension | Use `Arc<Mutex<>>` pattern: the `Arc` keeps the data alive. The guard borrows from the `Arc`'s pointee, which lives as long as the `Arc` is alive. No `transmute` needed — unlike RocksDB's transaction which transmutes the `'db` lifetime to `'static`. |
| BTreeMap iteration semantics differ from RocksDB prefix scans | Use the same `encode_*`/`decode_*` functions and the same `prefix_upper_bound` logic. BTreeMap's `range(prefix..=upper)` is equivalent to RocksDB's `set_iterate_upper_bound`. |
| Schema loading on `open_in_memory()` — empty store returns an empty schema | `InMemoryStore`'s `load_schema()` checks the `schema` BTreeMap. On first open it's empty → return `Schema::new()` with default options. Same flow as RocksDB's `load_schema` (no existing meta → save defaults). |
| `ScanConfig` (batch sizes) is meaningless in-memory | Not a problem. The config lives on `LogicalGraph`/`LogicalSnapshot`, not the store. In-memory sessions can use larger or unlimited batch sizes; the scan methods return all results in one call since there's no I/O cost. |
