# Design: Codebase Modularization, Concurrency & Quality Hardening

Status: approved / ready for execution
Created: 2026-08-04
Updated: 2026-08-04

---

## 1. Problem Statement

With the rapid expansion of RocksGraph (streaming bulk loading, ACID transactions, Volcano query engine, and native HNSW vector search with durable WAL in v0.2), five distinct categories of technical debt and risk have accumulated:

1. **Test Congestion (`graph/tests.rs` - 3,124 lines)**: The single largest file in the codebase, mixing CRUD, schema mode validation, transaction isolation, crash simulation, and vector WAL recovery.
2. **Facade Bloat (`api.rs` - 794 lines)**: Combines user-facing API sessions (`Graph`, `ReadSession`, `TxnSession`) with vector persistence I/O (`load_vector_configs`, `save_vector_indexes`, `vector_snapshot_path`), WAL seek replay, and GC routines.
3. **Bulk Loader Monolith (`bulk/loader.rs` - 2,053 lines)**: Bundles the streaming external sort-merge join pipeline, degree table construction, edge label annotation, and ~1,000 lines of tests.
4. **Concurrency & Lock Contention**:
   - `std::sync::RwLock` panics on poisoning if any worker panics while holding a lock.
   - `graph/logical.rs` re-acquires `schema.read()` on every single property get/set/drop, causing high lock contention during write transactions.
5. **Correctness & Edge-Case Vulnerabilities**:
   - Potential multiplication overflow in `hnsw.rs` memory limit checks (`new_cap * dimension * 4`).
   - Ambiguity in `RepeatBuilder` if both `.times(n)` and `.until(pred)` are specified.
   - Negative vertex IDs in vector indexing need clear validation.

---

## 2. Goals & Non-Goals

### Goals
- **Pragmatic, Idiomatic Refactoring**: Favor clean module separation and pure free functions over artificial wrapper structs.
- **Developer Productivity First**: Split `graph/tests.rs` immediately into modular test files to eliminate merge conflicts.
- **Subsystem Decoupling**: Extract pure vector snapshot I/O to `vector::persistence` and WAL operations to `vector::wal`. Keep graph scanning in `api.rs` / `graph/`.
- **Streamlined Bulk Loading**: Extract `edge_annotator.rs` and `degree.rs` from `bulk/loader.rs`, reducing `loader.rs` to a focused ~400-line pipeline orchestrator.
- **Concurrency Hardening**: Switch shared locks to `parking_lot::RwLock` (poison-free) and introduce `TxnSchemaCache` to avoid repeated schema lock acquisitions.
- **Zero Public API Breakage**: Public structs and method signatures remain 100% backward-compatible.

### Non-Goals
- Introducing stateful intermediate wrapper objects (like a `VectorManager` struct or `TxnVectorBuffer`) that add indirection without protecting invariants.
- Changing RocksDB on-disk storage layout, column family naming, or binary key/value formats.
- Moving deprecated code (`SstBulkLoader`) to separate files where it will bit-rot (keep in `loader.rs` with `#[deprecated]`).

---

## 3. Execution Sequence & Roadmap

$$\text{Phase 0 (Hardening)} \longrightarrow \text{Phase 1 (Tests, Buffer, Locks \& Cache)} \longrightarrow \text{Phase 2 (Vector Persistence \& WAL)} \longrightarrow \text{Phase 3 (Bulk Modularization)} \longrightarrow \text{Phase 4 (Docs)}$$

---

## 4. Phase Details & Technical Specifications

### Phase 0: Immediate Correctness & Safety Fixes

| Item | Target File | Problem | Solution |
|---|---|---|---|
| **0.1 Memory Limit Overflow Check** | `vector/hnsw.rs` | `new_cap * dimension * 4` can overflow `usize` on 32-bit platforms or large dimension vectors. | Use `new_cap.checked_mul(dimension).and_then(|x| x.checked_mul(4)).ok_or(...)`. |
| **0.2 Non-Negative Vertex ID Guard** | `vector/hnsw.rs` | Negative `i64` IDs cast directly to `u64` can cause subtle inconsistencies. | Explicitly validate `*id >= 0` for vertex vector keys. |
| **0.3 RepeatBuilder Modulator Exclusivity** | `gremlin/traversal/mod.rs` | Calling both `.times(n)` and `.until(pred)` produces ambiguous plan termination. | Use error accumulation pattern on builder: if termination condition already exists, record `StoreError::TraversalError("repeat() cannot specify both times() and until()".into())`. |
| **0.4 Zero-Allocation Label Names** | `api.rs` | `edge_label_names()` allocates `Vec<String>` from `SmolStr`s. | Change return type to `Vec<SmolStr>`. |

---

### Phase 1: Test Suite Decomposition, Concurrency Hardening & Transaction Staging

#### 1.1 Split `graph/tests.rs` (3,124 lines)
Decompose `rocksgraph/src/graph/tests.rs` into focused modules under `rocksgraph/src/graph/tests/`:

```
rocksgraph/src/graph/tests/
├── mod.rs              # Test harness, test helpers, and submodule declarations
├── crud.rs             # Vertex & edge insertion, deletion, property mutations, label filtering
├── schema.rs           # SchemaMode::Strict vs Auto, dynamic schema evolution, property type checks
├── isolation.rs        # OCC conflict checking, dirty read prevention, rollbacks
├── persistence_wal.rs  # Crash recovery simulation, storage snapshot reload
└── vector.rs           # Vector-specific commit sync, RYOW, WAL seek replay, and bulk rebuild
```

#### 1.2 `parking_lot::RwLock` Migration
Switch from `std::sync::RwLock` to `parking_lot::RwLock` across `api.rs`, `graph/logical.rs`, `graph/context.rs`, and `vector/mod.rs` to guarantee poison-free lock operations and reduce lock acquisition overhead.

#### 1.3 `TxnSchemaCache` in `graph/schema_cache.rs`
Define an eager snapshot cache of label and property ID mappings captured at `Graph::begin()`:

```rust
// rocksgraph/src/graph/schema_cache.rs

use ahash::AHashMap;
use smol_str::SmolStr;
use crate::types::{LabelId, error::StoreError};
use crate::schema::{Schema, SchemaMode};

#[derive(Clone, Debug, Default)]
pub(crate) struct TxnSchemaCache {
    vertex_label_ids: AHashMap<SmolStr, LabelId>,
    edge_label_ids: AHashMap<SmolStr, LabelId>,
    prop_key_ids: AHashMap<SmolStr, u16>,
}

impl TxnSchemaCache {
    /// Eagerly populate cache from schema read guard at transaction start.
    pub fn from_schema(schema: &Schema) -> Self {
        Self {
            vertex_label_ids: schema.vertex_labels.iter().map(|(id, name)| (name.clone(), *id)).collect(),
            edge_label_ids: schema.edge_labels.iter().map(|(id, name)| (name.clone(), *id)).collect(),
            prop_key_ids: schema.prop_keys.iter().map(|(id, name)| (name.clone(), *id)).collect(),
        }
    }

    #[inline]
    pub fn vertex_label_id(&self, name: &str) -> Option<LabelId> {
        self.vertex_label_ids.get(name).copied()
    }

    #[inline]
    pub fn edge_label_id(&self, name: &str) -> Option<LabelId> {
        self.edge_label_ids.get(name).copied()
    }

    #[inline]
    pub fn prop_key_id(&self, name: &str) -> Option<u16> {
        self.prop_key_ids.get(name).copied()
    }

    pub fn insert_vertex_label(&mut self, name: SmolStr, id: LabelId) {
        self.vertex_label_ids.insert(name, id);
    }

    pub fn insert_edge_label(&mut self, name: SmolStr, id: LabelId) {
        self.edge_label_ids.insert(name, id);
    }

    pub fn insert_prop_key(&mut self, name: SmolStr, id: u16) {
        self.prop_key_ids.insert(name, id);
    }
}
```

In `LogicalGraph`:
- Reads use `self.schema_cache` directly without taking locks.
- In `SchemaMode::Auto`, if a label or property is missing, acquire `self.schema.write()`, register the new entry, and update `self.schema_cache`.

#### 1.4 Extract `flush_vector_wal` in `vector/wal.rs`
Extract WAL encoding logic during `commit()` into a standalone function:

```rust
// rocksgraph/src/vector/wal.rs

pub(crate) fn flush_vector_wal(
    txn: &mut Transaction<RocksStore>,
    schema: &Schema,
    pending_ops: &[PendingVectorOp],
) -> Result<(), StoreError>;
```

`LogicalGraph::commit()` coordinates:
```rust
if !self.vector_pending_ops.is_empty() {
    flush_vector_wal(&mut self.txn, &schema, &self.vector_pending_ops)?;
    apply_vector_mutations(&self.vector_indexes, &self.vector_pending_ops)?;
    self.vector_pending_ops.clear();
}
```

---

### Phase 2: Vector Subsystem Encapsulation (`persistence.rs` & `wal.rs`)

#### 2.1 Pure I/O in `vector/persistence.rs`
Extract all stateless snapshot file I/O, headers, CRC checks, and path helpers:

```rust
// rocksgraph/src/vector/persistence.rs

pub const SNAPSHOT_MAGIC: &[u8; 8] = b"RGVECSNP";
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;
pub const SNAPSHOT_HEADER_SIZE: usize = 44;

pub fn vector_snapshot_path(db_path: &Path, entity: VectorEntityType, prop: &str) -> PathBuf;
pub fn read_vector_config(store: &RocksStorage, entity: VectorEntityType, prop: &str) -> Result<VectorIndexConfig, StoreError>;
pub fn load_vector_configs(store: &RocksStorage, map: &mut VectorIndexMap);
pub fn save_vector_indexes(store: &RocksStorage, map: &VectorIndexMap) -> Result<(), StoreError>;
```

#### 2.2 WAL Operations in `vector/wal.rs` (Consistent Signatures)
Both `replay_vector_wal` and `gc_vector_wal` take `&VectorIndexMap` (with callers holding read/write guards), enabling clean unit-testing without `Arc<RwLock>` overhead:

```rust
// rocksgraph/src/vector/wal.rs

pub fn replay_vector_wal(
    store: &RocksStorage,
    vector_indexes: &VectorIndexMap,
    schema: &Schema,
) -> Result<(), StoreError>;

pub fn gc_vector_wal(
    store: &RocksStorage,
    vector_indexes: &VectorIndexMap,
    schema: &Schema,
) -> Result<(), StoreError>;
```

#### 2.3 `rebuild_vector_index` in `api.rs`
`rebuild_vector_index` remains on `Graph` in `api.rs` where it naturally coordinates `LogicalSnapshot::scan_vertices()` (graph concern) and `persistence::save_snapshot()` (vector concern) without circular module dependencies.

---

### Phase 3: Bulk Loading Modularization (`bulk/`)

Decompose `bulk/loader.rs` into focused components:

```
rocksgraph/src/bulk/
├── mod.rs              # pub(crate) mod declarations & public re-exports
├── loader.rs           # BulkLoader streaming session (~400 lines orchestrator + generic SST writers)
├── sort.rs             # ExternalSorter (already extracted)
├── edge_annotator.rs   # annotate_edges() sort-merge join
├── degree.rs           # SortedLabelFile, DegreeCounter, write_degree_sst()
└── tests.rs            # Bulk loader integration test suite
```

- **`SstBulkLoader`**: Retained in `loader.rs` marked `#[deprecated]` to prevent bit-rot in a separate file.
- **`bulk/mod.rs`**: Cleanly re-exports public items and internal module boundaries.

---

### Phase 4: Documentation Categorization (`docs/`)

Acknowledge existing subdirectories (`docs/vector-search/`, `docs/api/`) and categorize the remaining ~16 flat design files:

```
docs/
├── architecture/          # design_principles.md, design_storage_agnostic_api.md, design_concurrent_pressure.md
├── schema/                # design_auto_schema.md, design_reserved_keys.md, design_widen_label_id.md
├── query-engine/          # design_filter_reordering.md, design_explain_step.md, design_degree_step.md, etc.
├── ingestion-bindings/    # design_bulkload_sst_ingest.md, design_python_bindings.md, design_nodejs_bindings.md
├── vector-search/         # (existing, 13 files)
├── api/                   # (existing, 3 files)
└── TODO.md                # Unified backlog referencing domain TODOs
```

#### Link Integrity Tooling (Blocking Pre-Check)
Before moving files:
1. Scan for relative Markdown links: `grep -rn '\[.*\](.*\.md)' docs/`.
2. Apply automated path rewrite on renamed/moved files.
3. Validate zero broken links.

---

## 5. Invariants & Guardrails

1. **No Public API Breakage**: Public structs and method signatures (`Graph`, `TxnSession`, `ReadSession`, `BulkLoader`, `VectorIndexConfig`) remain 100% backward-compatible.
2. **Zero Dead Code & Linter Warnings**: `cargo clippy --all-targets -- -D warnings` must report 0 warnings after each phase.
3. **Documentation Accuracy**: `cargo doc --no-deps` must build cleanly without warnings or altered public docs.
4. **Preserve Complete Test Coverage**: All 813+ unit, integration, and doc-tests must pass without regression across every milestone.
