# Implementation Plan: Vector Search v0.2 — HNSW via usearch

Status: proposal. Depends on v0.1 (`FloatVector` type + `BruteForceIndex`).

---

## Scope

| In scope | Out of scope |
|----------|-------------|
| `usearch` crate dependency | IVF index |
| `VectorIndex` trait + `VectorError` type | RaBitQ quantization |
| `VectorIndex` impl for `BruteForceIndex` + `UsearchHnswIndex` | `neighbors` step |
| `VectorIndexConfig` + schema integration | Pre-filter ANN |
| `vector_wal` CF + crash consistency | Background index rebuild |
| `AtomicU64` clock + `RwLock` concurrency | `add_vector_index_async` |
| `VectorRuntimeOptions` + memory limit | Edge vector indexes (vertex-only) |
| f16 quantization (default) | `withScore()` modulator |
| Snapshot format v2 | `change_vector_index_algorithm` (v0.3) |
| RYOW: merge pending vector ops into search results | |

---

## Phase 1: usearch dependency (~20 lines)

### Step 1.1 — Cargo.toml

```toml
[dependencies]
usearch = "~2.1"       # HNSW ANN index, pure Rust, MIT
rustc-hash = "2"       # FxHash for EntityKey → label mapping
crc32fast = "1"        # CRC-32C checksum for snapshot headers
```

Vector search is always-on (not feature-gated) — the v0.1 `pub mod vector` in
`lib.rs` is already unconditional, and v0.2 continues that. Users who don't use
vectors pay only the compile cost, no runtime overhead.

### Step 1.2 — `rocksgraph/src/vector/mod.rs`

```rust
pub mod brute_force;     // v0.1, already present
pub mod error;           // v0.2, new
pub mod hnsw;            // v0.2, new
pub mod snapshot;        // v0.2, new
pub mod traits;          // v0.2, shared VectorIndex trait
pub mod wal;             // v0.2, new
```

---

## Phase 2: VectorError + VectorIndex trait (~100 lines)

### Step 2.1 — VectorError (new)

`rocksgraph/src/vector/error.rs`

```rust
#[derive(Debug)]
pub enum VectorError {
    DimensionMismatch { expected: usize, actual: usize },
    IndexNotFound { entity_type: VectorEntityType, property: SmolStr },
    MemoryLimitExceeded { index: SmolStr, used: usize, limit: usize },
    Io(std::io::Error),
    Store(StoreError),
    Unsupported(String),
}
```

This is the concrete error type for all `VectorIndex` trait methods. Wraps
`StoreError` and `std::io::Error` for save/load paths.

### Step 2.2 — Trait definition

`rocksgraph/src/vector/traits.rs`

```rust
pub trait VectorIndex: Send + Sync {
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<(), VectorError>;
    fn remove(&mut self, key: &EntityKey) -> Result<(), VectorError>;
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>, VectorError>;
    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<(), VectorError>;
    fn last_replayed_timestamp(&self) -> u64;
    fn set_last_replayed_timestamp(&mut self, seq: u64);
}
```

`EntityKey` is already defined in `brute_force.rs` (v0.1) as `Vertex(i64)` | `Edge(CanonicalEdgeKey)`. The trait reuses this type.

### Step 2.3 — Wrap BruteForceIndex behind the trait

`impl VectorIndex for BruteForceIndex` — mostly passthrough. `save` and timestamps
are no-ops because brute-force is ephemeral. Returns `0` for `last_replayed_timestamp`.

---

## Phase 3: UsearchHnswIndex (~300 lines)

> **Spike**: Before writing Phase 3, evaluate `usearch::Index::serialize()` / `view()`
> in the usearch 2.x crate. If either returns a byte slice, the snapshot `save()`
> path can use it directly instead of the temp-file workaround described in
> Step 3.4. This materially simplifies the snapshot code.

### Step 3.1 — Struct

`rocksgraph/src/vector/hnsw.rs`

```rust
pub struct UsearchHnswIndex {
    inner: usearch::Index,
    dimension: usize,
    metric: DistanceMetric,
    config: VectorIndexConfig,
    tombstone_count: u64,
    last_replayed_timestamp: u64,
}
```

### Step 3.2 — Vertex label mapping

Vertex IDs are `i64`, usearch labels are `u64`. Direct cast:

```rust
fn vertex_to_label(id: i64) -> u64 { id as u64 }
fn label_to_vertex(label: u64) -> i64 { label as i64 }
```

Simple, deterministic. Negative vertex IDs become large u64 values — functionally
correct since labels are opaque to usearch.

### Step 3.3 — Edge label mapping (deferred to v0.3)

Edge labels require the `vector_edge_labels` CF. In v0.2, only vertex indexes
are supported. `EntityKey::Edge` → `UnsupportedOperation`.

### Step 3.4 — `impl VectorIndex for UsearchHnswIndex`

```
insert(): dimension check → usearch::Index::add(label, vector)
remove(): usearch::Index::remove(label) → tombstone_count++
search(): usearch::Index::search(query, k) → map labels back to EntityKey::Vertex
save(): usearch::Index::save(path) + write snapshot header (magic, version, timestamp, CRC-32C)

> **Implementation note**: usearch 2.x `save()` writes directly to a file path,
> not to a byte buffer. To embed the usearch payload inside the custom snapshot
> header, save to a temporary file, read it back as bytes, then write the
> composite snapshot. The temporary file is deleted after the composite write.
> The usearch `view()` + `serialize()` API (if available in 2.1+) should be
> evaluated first — if it returns a `&[u8]` or `Vec<u8>`, no temp file is needed.
last_replayed_timestamp(): return stored value
set_last_replayed_timestamp(): update stored value
```

---

## Phase 4: Index configuration + schema (~100 lines)

### Step 4.1 — VectorIndexConfig

`rocksgraph/src/vector/traits.rs`

```rust
pub struct VectorIndexConfig {
    pub property: SmolStr,
    pub entity_type: VectorEntityType,  // Vertex | Edge (Edge → UnsupportedOperation in v0.2)
    pub dimension: usize,
    pub metric: DistanceMetric,
    pub algorithm: AnnAlgorithm,        // BruteForce | Hnsw(HnswConfig)
    pub quantization: Quantization,     // F16 (default) | F32
}

pub struct VectorIndexLimit {
    pub memory_limit_bytes: usize,
}

pub struct IndexLimitOverride {
    pub entity_type: VectorEntityType,
    pub property: SmolStr,
    pub limit: VectorIndexLimit,
}

pub struct VectorRuntimeOptions {
    pub default_limit: Option<VectorIndexLimit>,
    pub per_index_overrides: Vec<IndexLimitOverride>,
}
```


`DistanceMetric` variants: `Cosine` (0), `Euclidean` (1), `DotProduct` (2).
Encoded as `u8` in the snapshot header.

`AnnAlgorithm` variants: `BruteForce` (0), `Hnsw(HnswConfig)` (1).
`HnswConfig` fields and defaults:

```rust
pub struct HnswConfig {
    pub m: usize,               // default: 16
    pub ef_construction: usize, // default: 200
    pub ef_search: usize,       // default: 50
}
```

`Quantization` variants: `F16` (0, default), `F32` (1).
Controls `usearch::ScalarKind` at index construction.


`VectorEntityType` variants: `Vertex` (0), `Edge` (1). Edge support is deferred
to v0.3; any operation on `EntityKey::Edge` returns `VectorError::Unsupported`.

### Step 4.2 — SchemaSession integration

Add to `SchemaSession`:

```rust
pub fn add_vector_index(&mut self, config: VectorIndexConfig) -> &mut Self;
pub fn drop_vector_index(&mut self, entity_type: VectorEntityType, property: &str) -> &mut Self;
```

`add_vector_index` persists config to `CF_SCHEMA` under `vector_index_config/{entity_type}/{prop_key}`.

`drop_vector_index` removes the config key from `CF_SCHEMA`. Existing WAL entries for the
dropped index are not deleted; they are silently filtered out during WAL replay in
`Graph::open` by checking whether the index still exists in `CF_SCHEMA`. No compaction
step is required in v0.2.

### Step 4.3 — Graph integration

`Graph` struct gains `vector_indexes: HashMap<(VectorEntityType, SmolStr), Arc<RwLock<Box<dyn VectorIndex>>>>`.

`Graph::open` loads configs from `CF_SCHEMA`, constructs indexes, runs WAL replay.

### Step 4.4 — VectorRuntimeOptions injection

`VectorRuntimeOptions` is passed into `Graph::open` alongside `GraphOptions`:

```rust
pub fn open_with_options(
    path: impl AsRef<Path>,
    opts: GraphOptions,
    rocks_opts: Option<RocksOptions>,
    vector_opts: Option<VectorRuntimeOptions>,
) -> Result<Self, StoreError>
```

`Graph::open()` (convenience) and `Graph::open_with_rocks_options()` use
`VectorRuntimeOptions::default()` (no per-index overrides, no global limit).
The vector options are stored on `Graph.vector_options` and consulted at
index construction time.

---

## Phase 4b: Index maintenance (~30 lines)

### `rebuild_vector_index` on `Graph`

```rust
impl Graph {
    /// Rebuild a named vector index from scratch by scanning all vertices.
    /// Used after schema changes or manual recovery. Clears the existing
    /// index, scans CF_VERTICES + CF_VERTEX_PROPS for FloatVector values
    /// matching `(entity_type, prop_key_id)`, and re-inserts them.
    ///
    /// This is a manual maintenance operation, not triggered automatically
    /// on schema changes (v0.3 will add auto-rebuild triggers).
    pub fn rebuild_vector_index(
        &self,
        entity_type: VectorEntityType,
        property: &str,
    ) -> Result<(), VectorError>
}
```

---

## Phase 5: WAL + crash consistency (~250 lines)

### Step 5.1 — Column family

`rocksgraph/src/store/rocks/mod.rs` — add `CF_VECTOR_WAL = "vector_wal"`.

### Step 5.2 — WAL key/value format

```
Key:   [prop_key_id: u16 BE][entity_type: u8][ts: u64 BE][random: u32 BE]  (15 bytes)
Value: [op_type: u8][entity_key: ...][vector_len: u32 LE][vector_data: f32 LE ...]
    Vertex entity keys are encoded as `[0x00: u8][vertex_id: i64 LE]` (9 bytes).
    Edge encoding (discriminant `0x01`) is reserved for v0.3.
```

### Step 5.3 — Timestamp clock

```rust
static WAL_CLOCK: AtomicU64 = AtomicU64::new(0);  // seeded from SystemTime + stored HWM on open
```

`TxSession::commit()` performs a pre-flight memory check on all indexes affected
by the transaction. If any index would exceed its limit, the commit is rejected
*before* any durable write. After the check passes, vector mutations are written
to `vector_wal` CF in the same `WriteBatch` as graph mutations — one `fsync`
covers both. The memory check and WAL write are separate steps; the check gates
the write.

### Step 5.4 — Snapshot format v2

```
Offset  Length  Field
0       4       magic = 0x52475F56  ("RG_V")
4       2       format_version = 2
6       8       last_replayed_timestamp   (u64 LE)
14      4       dimension           (u32 LE)
18      1       metric              (u8)
19      1       algorithm           (u8)
20      8       tombstone_count     (u64 LE)
28      8       next_edge_label     (u64 LE)    // always 0 in v0.2; reserved for v0.3+ edge labels
36      8       usearch_payload_len (u64 LE)
44      N       usearch_payload
44+N    4       crc32c
```

### Step 5.5 — Recovery

`Graph::open`:
1. Load configs from `CF_SCHEMA`
2. For each index: load snapshot → `last_replayed_timestamp`
3. For each index, construct a seek key as `[prop_key_id BE][entity_type BE][0x0000000000000000 BE]` and iterate from there until `prop_key_id` or `entity_type` changes. Replay each entry where `ts > last_replayed_timestamp`.
4. Skip WAL entries whose index config is absent from `CF_SCHEMA` (dropped indexes)
5. Replay remaining entries into index

---

## Phase 6: Concurrency (~60 lines)

### Step 6.1 — AtomicU64 clock (already in Phase 5)

Replaces the removed `Mutex<u64>` counter. No global lock.

### Step 6.2 — RwLock per index

Each `Box<dyn VectorIndex>` is wrapped in `Arc<RwLock<>>`. Readers share,
writers serialize. `TxSession::commit` acquires write lock after `db.write()`.

### Step 6.3 — Memory limit enforcement

`VectorRuntimeOptions.default_limit` passed to `UsearchHnswIndex` at construction.
Memory enforcement is done via the pre-flight check in `TxSession::commit()`
(see Phase 5.3). No runtime check in `insert()` — the pre-flight model ensures
the WAL entry is always replayable and avoids an OOM death loop on recovery.

---

## Phase 7: RYOW — Read-Your-Own-Writes (~80 lines)

### Step 7.1 — `vector_pending_ops` on `TxSession`

For RYOW isolation, uncommitted vector mutations in the current transaction must
be visible to vector searches within the same session. This is analogous to the
existing RYOW support for graph traversals.

```rust
pub struct TxSession {
    // ... existing fields ...
    vector_pending_ops: Vec<PendingVectorOp>,
}
enum PendingVectorOp {
    Inserted { key: EntityKey, vector: Vec<f32> },
    Removed { key: EntityKey },
}
```

### Step 7.2 — Merge in `NearestStep::produce`

After collecting HNSW search results, merge with a brute-force scan of
`vector_pending_ops` from the current `TxSession`:

```
1. HNSW search → top_k candidates
2. Filter out entries removed in vector_pending_ops
3. Add entries inserted in vector_pending_ops (brute-force scored)
4. Re-sort and re-truncate to k
```

The merge is O(|pending| × D) — acceptable since pending ops are small
(< 1000 per transaction).

### Step 7.3 — `GraphCtx` extension

`GraphCtx` gains a `vector_pending_ops()` method:

```rust
pub trait GraphCtx {
    // ... existing methods ...
    fn vector_pending_ops(&self) -> &[PendingVectorOp];
}
```

`ReadSession` returns an empty slice; `TxSession` returns its pending ops.
This avoids downcasting and keeps the step signature `&mut dyn GraphCtx`.

### Step 7.4 — Step dispatch on `Graph`

`NearestStep::produce` checks `graph.vector_indexes` for a matching index:

- Index exists → `index.read().search(query, k)` (HNSW) +
  merge `vector_pending_ops` from the current session
- No index → fallback to inline brute-force scan (v0.1 behavior)

`SimilarityStep::produce` follows the same dispatch pattern.

## Files changed

| File | Change |
|------|--------|
| `rocksgraph/Cargo.toml` | Add `usearch`, `rustc-hash`, `crc32fast` |
| `rocksgraph/src/vector/mod.rs` | Module wiring (incremental across MRs: error+traits in MR1, hnsw in MR2, wal+snapshot in MR3) |
| `rocksgraph/src/vector/traits.rs` | New: `VectorIndex`, `VectorIndexConfig`, `VectorEntityType`, `DistanceMetric`, `AnnAlgorithm` |
| `rocksgraph/src/vector/hnsw.rs` | New: `UsearchHnswIndex` |
| `rocksgraph/src/vector/brute_force.rs` | Update: implement `VectorIndex` trait |
| `rocksgraph/src/vector/wal.rs` | New: WAL key/value encode/decode, timestamp clock |
| `rocksgraph/src/vector/snapshot.rs` | New: snapshot header read/write with CRC-32C |
| `rocksgraph/src/store/rocks/mod.rs` | Add `CF_VECTOR_WAL` column family |
| `rocksgraph/src/api.rs` | Add `vector_indexes`, `vector_options` to `Graph`; WAL replay on open; `rebuild_vector_index`; `TxSession.vector_pending_ops` |
| `rocksgraph/src/schema/management.rs` | Add `add_vector_index`, `drop_vector_index` |
| `rocksgraph/src/vector/error.rs` | New: `VectorError` enum |
| `rocksgraph/src/engine/volcano/steps/vector.rs` | Update: dispatch through `graph.vector_indexes`; merge `pending_vector_ops` for RYOW |
| `rocksgraph/src/lib.rs` | `pub mod vector` |
| `rocksgraph/src/engine/context.rs` | `GraphCtx` gains `vector_pending_ops()` method for RYOW pass-through |



---

## MR breakdown

The plan decomposes naturally into three independent merge requests, ordered by
dependency. Each is reviewable at < 450 lines and has zero behavioral overlap
with the others.

### MR 1 — Foundation (Phases 1–2, ~140 lines)

| File | Change |
|------|--------|
| `rocksgraph/Cargo.toml` | Add `usearch`, `rustc-hash`, `crc32fast` |
| `rocksgraph/src/vector/error.rs` | New: `VectorError` enum |
| `rocksgraph/src/vector/traits.rs` | New: `VectorIndex` trait, `VectorIndexConfig`, `VectorEntityType`, `DistanceMetric`, `AnnAlgorithm`, `Quantization`, `VectorIndexLimit`, `IndexLimitOverride`, `VectorRuntimeOptions`, `HnswConfig` |
| `rocksgraph/src/vector/brute_force.rs` | Update: `impl VectorIndex for BruteForceIndex` (save/timestamp are no-ops for BruteForce) |
| `rocksgraph/src/vector/mod.rs` | Add `pub mod traits`, `pub mod error`; re-export `VectorError` |

**Verification**: `cargo check --lib` compiles; existing 784 tests pass. No
runtime behaviour change — traits are defined but no call site uses them yet.

---

### MR 2 — HNSW + Schema (Phases 3–4, ~450 lines)

| File | Change |
|------|--------|
| `rocksgraph/src/vector/mod.rs` | Add `pub mod hnsw` |
| `rocksgraph/src/vector/hnsw.rs` | New: `UsearchHnswIndex` struct + `impl VectorIndex` (insert, remove, search, save, timestamps). Includes f16 quantization via `usearch::ScalarKind::F16` and the vertex label i64-u64 direct cast |
| `rocksgraph/src/schema/management.rs` | Add `SchemaSession::add_vector_index`, `drop_vector_index`. Persists config to CF_SCHEMA under key prefix `vector_index_config/{entity_type}/{prop_key}` |
| `rocksgraph/src/api.rs` | `Graph.vector_indexes: HashMap<...>` field. `Graph::open` loads configs from CF_SCHEMA, constructs `UsearchHnswIndex` instances, stores as `Arc<RwLock<Box<dyn VectorIndex>>>`. `open_with_options` takes `Option<VectorRuntimeOptions>`. `rebuild_vector_index(entity_type, property)` scans CF_VERTICES for FloatVector values and rebuilds the named index |
| `rocksgraph/src/engine/volcano/steps/vector.rs` | **Core dispatch change**: `NearestStep::produce` checks `graph.vector_indexes` for a matching index registry. Index exists: call `index.read().search(query, k)`. No index: fallback to v0.1 inline brute-force scan. `SimilarityStep` follows the same pattern |

**Verification**: existing Python integration tests (test_nearest_exact_knn
et al.) continue to pass via the no-index fallback path. New tests: declare an
HNSW index via `SchemaSession`, insert 10K random vectors, search for top-10,
verify recall at least 95% vs brute-force ground truth.

**Dependency**: MR 1 must be merged first (trait definition + error type).

---

### MR 3 — Persistence + Correctness (Phases 5–7, ~420 lines)

| File | Change |
|------|--------|
| `rocksgraph/src/vector/mod.rs` | Add `pub mod wal`, `pub mod snapshot` |
| `rocksgraph/src/vector/wal.rs` | New: WAL key/value encode/decode, `AtomicU64` timestamp clock (seeded from `SystemTime` + stored HWM) |
| `rocksgraph/src/vector/snapshot.rs` | New: snapshot header with CRC-32C, format v2 write/read, usearch payload embedding (temp-file approach or `serialize()` depending on spike result) |
| `rocksgraph/src/store/rocks/mod.rs` | Add `CF_VECTOR_WAL` column family |
| `rocksgraph/src/api.rs` | `TxSession.vector_pending_ops` field + RYOW merge in commit. WAL replay loop in `Graph::open`: load config then load snapshots then replay `vector_wal` entries after `last_replayed_timestamp` |
| `rocksgraph/src/engine/context.rs` | `GraphCtx::vector_pending_ops()` method. `ReadSession` returns empty slice; `TxSession` returns its pending ops |
| `rocksgraph/src/engine/volcano/steps/vector.rs` | RYOW merge: after index search, filter removed entries from `ctx.vector_pending_ops()`, add inserted entries with brute-force scoring, re-sort and truncate to k |

**Verification**: crash-recovery integration test: write 1000 vectors, kill the
process during WAL write, reopen, verify exact match between recovered index and
expected state. RYOW test: insert a vector in a transaction and search for it
in the same session before commit; verify the write is visible.

**Dependency**: MR 2 must be merged first (index dispatch path + `Graph.vector_indexes`).

---

### Timeline

```
MR 1 (traits + types)    two days (review: one hour)
MR 2 (HNSW + schema)     four days (review: two hours)  
MR 3 (persist + safety)  four days (review: three hours)
```

MR 2 can start after MR 1 is approved (not necessarily merged) since the trait
interface is the coupling point. MR 3 must wait for MR 2 to land.


Total estimate: ~1080 lines Rust. No Python changes (brute-force API is identical).
