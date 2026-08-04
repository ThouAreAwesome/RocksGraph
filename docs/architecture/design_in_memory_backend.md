# Design: in-memory storage backend

## Problem

RocksDB is the only storage backend, requiring `librocksdb-sys` (a large C++ codebase).
This is a barrier for first-time users (`cargo run --example basic_usage` takes minutes
of C++ compilation), rapid prototyping, and fast test iteration.

The `GraphStore` / `GraphTransaction` / `GraphSnapshot` traits already define a clean
pluggable abstraction.  Only the implementation is missing.

## Goals & non-goals

- **Goals:** Zero external dependencies (pure Rust); single-session enforcement;
  identical behaviour to RocksDB for all `LogicalGraph` invariants; ~400-600 lines.
- **Non-goals:** Multi-transaction isolation (single-session enforcement eliminates the
  need); persistence (data lost on drop, by design).

## Design

### Concurrency model

The store wraps all internal state in `Arc<Mutex<InMemoryData>>`. Session creation uses
`Mutex::try_lock()`:

- **Success** → session holds the `MutexGuard` for its lifetime; read-your-writes is
  automatic since no other session can interleave.
- **Failure** → returns `StoreError::BackendBusy`, a new error variant.

Because `MutexGuard` is `!Send`, sessions cannot be moved to other threads — this is
an additional type-level safety property.

### New error variant and fallible trait methods

```rust
pub enum StoreError {
    // ... existing variants ...
    BackendBusy(String),
}

pub trait GraphStore {
    type Snapshot: GraphSnapshot;
    type Txn: GraphTransaction;
    fn snapshot(&self) -> Result<Self::Snapshot, StoreError>;
    fn begin(&self) -> Result<Self::Txn, StoreError>;
}
```

RocksDB's `begin()`/`snapshot()` never fail; they return `Ok(...)`.  Callers in `api.rs`
and test code add a `?`.  Distributed backends would also benefit.

### Internal data structures

```rust
struct InMemoryData {
    // BTreeMap for ordered iteration (same prefix-scan semantics as RocksDB iterators)
    vertices:      BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_vertex_key → encoded_VertexValue
    edges_out:     BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_edge_key(OUT) → encoded_EdgeValue
    edges_in:      BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_edge_key(IN)  → encoded_EdgeValue
    vertex_degree: BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_vertex_key    → encoded_VertexDegree
    schema:        BTreeMap<Vec<u8>, Vec<u8>>,  // encoded_schema_key    → encoded_schema_value
}

struct InMemoryTxn { guard: InMemoryGuard, staging: InMemoryData }
struct InMemorySnapshot { guard: InMemoryGuard }
```

Key decisions:
- **BTreeMap** — gives `range()` for O(log n) prefix scans, equivalent to RocksDB iterators.
- **Reuse encoding layer** — calls the same `encode_*`/`decode_*` functions from
  `store/rocks/encoding.rs`; just writes to BTreeMap instead of RocksDB CFs.

### Transaction logic

```
Read path:   1. check staging map first (read-your-own-writes)
             2. if not found AND not deleted in staging, fall back to locked global data
Write path:  1. write to staging map
Commit:      1. drain staging into locked global data
             2. clear staging
Abort:       1. clear staging (lock releases on drop)
```

### Public API — generic `Graph<S>` with default `RocksStorage`

```rust
pub struct Graph<S = RocksStorage> {
    store: Arc<S>,
    schema: Arc<RwLock<Schema>>,
}

impl Graph<RocksStorage> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> { ... }
}

impl Graph<InMemoryStore> {
    pub fn open_in_memory() -> Result<Self, StoreError> { ... }
}

impl<S: GraphStore> Graph<S> {
    pub fn read(&self) -> ReadSession<S> { ... }
    pub fn begin(&self) -> TxSession<S> { ... }
}
```

No breaking change to `Graph::open(path)` callers.  New users write
`Graph::open_in_memory()` and everything works.

## Constraints / invariants

- Only one session active at a time (enforced by `Mutex::try_lock()`).
- BTreeMap iteration semantics must match RocksDB prefix scans — uses same `encode_*`
  functions and `prefix_upper_bound` logic.
- Schema loading on empty store returns `Schema::new()` with default options.
- `ScanConfig` (batch sizes) is meaningless in-memory — not a problem; it lives on
  `LogicalGraph`/`LogicalSnapshot`, not the store.

## Implementation plan

### Phase 1: Foundation
1. Add `StoreError::BackendBusy`
2. Make `GraphStore::begin()` / `snapshot()` fallible
3. Make `Graph<S>`, `ReadSession<S>`, `TxSession<S>` generic with `S = RocksStorage` default

### Phase 2: In-Memory Store
4. Add `InMemoryStore`, `InMemoryTxn`, `InMemorySnapshot` (new `store/in_memory/` module)
5. Implement `GraphStore` for `InMemoryStore`
6. Implement `GraphTransaction` for `InMemoryTxn` (~15 methods)
7. Implement `GraphSnapshot` for `InMemorySnapshot`
8. Add unit tests

### Phase 3: Public API
9. Add `Graph::open_in_memory()` constructor
10. Add `examples/in_memory.rs`
11. Wire into existing engine tests

### Phase 4: Polish
12. Update README
13. Verify `cargo run --example in_memory` compiles instantly (no C++ build)

## Files changed

| File | Change |
|------|--------|
| `src/types/error.rs` | Add `BackendBusy(String)` |
| `src/store/traits.rs` | `begin()` / `snapshot()` → `Result` |
| `src/store/in_memory/mod.rs` | New module |
| `src/api.rs` | Generic `Graph<S>` + `open_in_memory()` |
| `src/lib.rs` | Re-export if needed |
| `examples/in_memory.rs` | New example |
| `README.md` | Document `open_in_memory()` |

## Risks & mitigations

| Risk | Mitigation |
|------|-----------|
| `MutexGuard<'static>` lifetime | `Arc<Mutex<T>>` keeps data alive; guard borrows from `Arc`'s pointee — no `transmute` |
| BTreeMap vs RocksDB iteration | Same `encode_*`/`decode_*` + `prefix_upper_bound` logic; BTreeMap `range()` = RocksDB prefix scan |
| Empty schema on first open | `load_schema()` returns `Schema::new()` with defaults, same flow as RocksDB |
