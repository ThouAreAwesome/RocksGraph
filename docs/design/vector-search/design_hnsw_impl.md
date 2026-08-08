# Design: HNSW Implementation — `UsearchHnswIndex`

Status: proposal — implementation target v0.2.
Depends on: `design_ann_algorithm_and_library.md`, `design_vector_wal.md`.

---

## Table of Contents

- [Design: HNSW Implementation — `UsearchHnswIndex`](#design-hnsw-implementation--usearchhnswindex)
  - [Table of Contents](#table-of-contents)
  - [1. Scope](#1-scope)
  - [2. usearch crate integration](#2-usearch-crate-integration)
    - [2a. Cargo.toml entry](#2a-cargotoml-entry)
    - [2b. API surface used](#2b-api-surface-used)
    - [2c. Index construction parameters](#2c-index-construction-parameters)
  - [3. `UsearchHnswIndex` struct](#3-usearchhnswindex-struct)
  - [4. `EntityKey` → `u64` label mapping](#4-entitykey--u64-label-mapping)
    - [4a. Vertex labels — direct cast](#4a-vertex-labels--direct-cast)
    - [4b. Edge labels — canonical key → monotonic u64](#4b-edge-labels--canonical-key--monotonic-u64)
    - [4c. Reverse lookup during search](#4c-reverse-lookup-during-search)
  - [5. Implementing the `VectorIndex` trait](#5-implementing-the-vectorindex-trait)
    - [5a. `insert`](#5a-insert)
    - [5b. `remove`](#5b-remove)
    - [5d. `save`](#5d-save)
    - [5e. `last_replayed_timestamp`](#5e-last_replayed_timestamp)
  - [6. `load_vector_index` free function](#6-load_vector_index-free-function)
  - [7. Tombstone tracking](#7-tombstone-tracking)
  - [8. Snapshot file format](#8-snapshot-file-format)
    - [8a. File layout](#8a-file-layout)
    - [8b. Map serialization](#8b-map-serialization)
    - [8c. File naming and location](#8c-file-naming-and-location)
    - [8d. Atomic write](#8d-atomic-write)
  - [9. Cold-start rebuild (no snapshot)](#9-cold-start-rebuild-no-snapshot)
  - [10. WAL replay onto an existing snapshot](#10-wal-replay-onto-an-existing-snapshot)
  - [11. Per-query `ef_search` override](#11-per-query-ef_search-override)
  - [12. Parameters and defaults](#12-parameters-and-defaults)
  - [13. Error cases](#13-error-cases)
  - [14. Module layout](#14-module-layout)
  - [15. Implementation checklist](#15-implementation-checklist)
    - [Types and trait (`rocksgraph/src/vector/`)](#types-and-trait-rocksgraphsrcvector)
    - [`UsearchHnswIndex` (`rocksgraph/src/vector/hnsw.rs`)](#usearchhnswindex-rocksgraphsrcvectorhnswrs)
    - [Snapshot I/O (`rocksgraph/src/vector/snapshot.rs`)](#snapshot-io-rocksgraphsrcvectorsnapshotrs)
    - [Startup paths (`rocksgraph/src/vector/wal_replay.rs`)](#startup-paths-rocksgraphsrcvectorwal_replayrs)
    - [Integration with `Graph` struct](#integration-with-graph-struct)
    - [Tests](#tests)

---

## 1. Scope

This document specifies the concrete Rust implementation of the `VectorIndex` trait
(defined in `design_vector_search.md` §8b) using the `usearch` crate as the backing
HNSW library. It covers:

- Wrapping usearch's `Index` object inside `UsearchHnswIndex`
- Mapping `EntityKey` to usearch's `u64` label space
- Implementing all five trait methods with correct semantics
- The on-disk snapshot file format (byte-for-byte) that the WAL recovery path reads
- Cold-start rebuild from the props CF
- Per-query `ef_search` override threading

This document does not re-specify the WAL write path, counter mechanics, or
`RwLock` concurrency model — those are in `design_vector_wal.md` and
`design_vector_concurrency.md`.

---

## 2. usearch crate integration

### 2a. Cargo.toml entry

```toml
# rocksgraph/Cargo.toml
[dependencies]
usearch = "2"
```

usearch 2.x is the minimum required version. It exposes a safe Rust API via
the `usearch` crate on crates.io (wraps the C++ usearch core; the Rust crate
handles all unsafe internally). It requires a C++ compiler at build time but
has no system library dependencies beyond the C++ standard library.

### 2b. API surface used

```rust
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

// Construction
let index = Index::new(&IndexOptions {
    dimensions:       1536,
    metric:           MetricKind::Cos,
    quantization:     ScalarKind::F32,
    connectivity:     16,    // M parameter
    expansion_add:    200,   // ef_construction
    expansion_search: 50,    // ef_search default
    ..Default::default()
})?;
index.reserve(expected_capacity)?;

// Mutation
index.add(label: u64, vector: &[f32])?;
index.remove(label: u64)?;     // soft delete (tombstone)

// Query
let results = index.search(query: &[f32], count: usize)?;
// results.keys: &[u64], results.distances: &[f32]

// Per-query ef_search override (usearch 2.x)
// Note: exact method name must be verified against the locked crate version.
// usearch 2.x exposes expansion_search as a mutable property on the index;
// the approach shown below wraps a temporary override.
// If no per-query API is available, take the write lock and set
// index.expansion_search = ef before the search, restore after.
index.expansion_search = ef;   // set before search, restore after

// Serialization
index.save(path: &str)?;
index.load(path: &str)?;

// Metrics
index.size() -> usize          // count of live (non-tombstoned) vectors
index.capacity() -> usize      // allocated slots
```

### 2c. Index construction parameters

```rust
fn usearch_options(config: &VectorIndexConfig) -> IndexOptions {
    IndexOptions {
        dimensions: config.dimension,
        metric: match config.metric {
            DistanceMetric::Cosine       => MetricKind::Cos,
            DistanceMetric::Euclidean    => MetricKind::L2sq,
            DistanceMetric::InnerProduct => MetricKind::IP,
        },
        quantization:     ScalarKind::F32,
        connectivity:     match &config.algorithm {
            AnnAlgorithm::Hnsw { m, .. } => *m,
            _ => 16,
        },
        expansion_add: match &config.algorithm {
            AnnAlgorithm::Hnsw { ef_construction, .. } => *ef_construction,
            _ => 200,
        },
        expansion_search: DEFAULT_EF_SEARCH,
        ..Default::default()
    }
}

const DEFAULT_EF_SEARCH: usize = 50;
```

`ScalarKind::F32` stores raw 32-bit floats without quantization. Quantization
variants (SQ8, Binary) are reserved for a future `RaBitQ` integration.

---

## 3. `UsearchHnswIndex` struct

```rust
pub struct UsearchHnswIndex {
    /// The backing usearch HNSW index.
    inner: Index,

    /// RocksDB instance — used by edge indexes to read/write the
    /// `vector_edge_labels` CF for label→EntityKey and EntityKey→label lookups.
    /// Vertex indexes use a trivial i64↔u64 bit-cast and never touch the CF.
    db: Arc<DB>,

    /// Identifies which `vector_edge_labels` CF key prefix this index uses.
    /// Stored as u16 BE in the two-byte CF key prefix.
    prop_key_id: u16,

    /// Determines the label strategy: Vertex = bit-cast, Edge = CF lookup.
    entity_type: VectorEntityType,

    /// Monotonic counter for assigning u64 labels to edges.
    /// Incremented once per new edge insert (not on upsert). Persisted in the
    /// snapshot header so labels are stable across save/load cycles.
    /// Unused for vertex indexes (vertex_id is the label).
    next_edge_label: u64,

    /// WAL timestamp of the last entry applied to this index.
    /// Written into the snapshot header on `save`.
    last_replayed_timestamp: u64,

    /// Count of soft-deleted (tombstoned) entries.
    /// usearch's `size()` counts only live entries; tombstones are not exposed.
    /// We track this separately so `VectorIndexStats.tombstone_ratio` is accurate.
    tombstone_count: u64,

    /// Index configuration — stored for snapshot validation on reload.
    metric:    DistanceMetric,
    dimension: usize,
}
```

---

## 4. `EntityKey` → `u64` label mapping

usearch identifies vectors by a `u64` label. `EntityKey` is either
`Vertex(i64)` or `Edge(EdgeKey)`. The mapping must be:
- Deterministic (same key always maps to same label)
- Injective within each index (two different keys must not produce the same label)
- Efficiently reversible (search results are labels; we need the original EntityKey)

### 4a. Vertex labels — direct bit cast, no map required

```rust
fn vertex_to_label(id: i64) -> u64 {
    // Reinterpret bits — bijection on all i64 values, including negatives.
    // e.g. -1i64 → 18446744073709551615u64. The reverse cast recovers the ID exactly.
    id as u64
}

fn label_to_vertex(label: u64) -> EntityKey {
    EntityKey::Vertex(label as i64)
}
```

The bit cast is its own inverse. No map, no CF lookup, no allocation. Vertex-only
indexes have zero per-vector map overhead at any scale.

### 4b. Edge labels — CF-backed monotonic assignment

`CanonicalEdgeKey` (defined in `rocksgraph/src/types/keys.rs`) identifies an edge
uniquely as `(src_id: i64, label_id: i32, dst_id: i64, rank: u16)` = 22 bytes.

Instead of holding bidirectional HashMaps in RAM (which grow linearly — ~61 bytes
per edge, or ~6 GB at 100 M edges), labels are stored in a dedicated RocksDB Column
Family `vector_edge_labels`. This CF is always current and requires no startup
deserialization — RocksDB manages it like any other CF.

**CF key format** (all big-endian for lexicographic ordering):

```
Forward lookup (EntityKey → label), used during insert and remove:
  [prop_key_id: u16 BE][0x00][src_id: i64 LE][label_id: i32 LE][dst_id: i64 LE][rank: u16 LE]
  → value: [label: u64 LE]

Reverse lookup (label → EntityKey), used during search:
  [prop_key_id: u16 BE][0x01][label: u64 BE]
  → value: [src_id: i64 LE][label_id: i32 LE][dst_id: i64 LE][rank: u16 LE]
```

**Label assignment**:
```rust
fn assign_or_lookup_edge_label(
    db:         &DB,
    cf:         &ColumnFamily,
    prop_key_id: u16,
    cek:        &CanonicalEdgeKey,
    next_label: &mut u64,
) -> Result<(u64, bool /* is_new */)> {
    let fwd_key = make_fwd_cf_key(prop_key_id, cek);
    if let Some(v) = db.get_cf(cf, &fwd_key)? {
        let label = u64::from_le_bytes(v[..8].try_into().unwrap());
        return Ok((label, false));   // upsert: reuse existing label
    }
    let label = *next_label;
    *next_label += 1;
    // Both directions are written as a small WriteBatch so they are atomic.
    let mut batch = WriteBatch::default();
    batch.put_cf(cf, &fwd_key, &label.to_le_bytes());
    batch.put_cf(cf, &make_rev_cf_key(prop_key_id, label), &encode_cek(cek));
    db.write(batch)?;
    Ok((label, true))
}
```

**Properties**:
- **No in-memory map**: zero RAM overhead for label storage; block cache handles hot entries.
- **No collisions**: labels are sequential; each new edge gets a unique label.
- **Crash-safe**: CF writes are atomic per-batch. If the process crashes before the usearch
  insert, WAL replay calls `insert()` again, finds the existing CF entry, and proceeds.
- **No startup deserialization**: the CF is always up to date; no snapshot section to decode.

**Performance**: edge index search (k=10) requires 10 CF point lookups to convert labels back
to EntityKeys. At typical working set sizes, the block cache absorbs these reads. Cold access
costs one disk read per result (~10–50 μs each), which is within the acceptable range for
graph-layer entity resolution.

**usearch version pinning**: usearch currently accepts arbitrary u64 labels (non-contiguous,
non-sequential). Pin the crate to a minor version (e.g. `usearch = "~2.1"`) to prevent
silent breakage from upstream label-semantic changes.

### 4c. Reverse lookup during search

```rust
fn label_to_key(
    db:         &DB,
    cf:         &ColumnFamily,
    prop_key_id: u16,
    label:      u64,
    entity_type: VectorEntityType,
) -> Result<EntityKey> {
    match entity_type {
        VectorEntityType::Vertex => Ok(EntityKey::Vertex(label as i64)),
        VectorEntityType::Edge => {
            let key = make_rev_cf_key(prop_key_id, label);
            let bytes = db.get_cf(cf, &key)?
                .expect("usearch returned label not in vector_edge_labels CF — index corruption");
            Ok(EntityKey::Edge(decode_cek(&bytes)))
        }
    }
}
```

A missing CF entry for an edge label indicates index corruption and panics rather than
silently returning a wrong EntityKey — same safety invariant as the old HashMap version.

**Memory**: `vector_edge_labels` CF entries are stored on disk and cached by the block
cache. No in-memory allocation per vector. At 100 M edges, the CF holds ~6 GB of label
data on disk (manageable by RocksDB), not in heap.

---

## 5. Implementing the `VectorIndex` trait

```rust
impl VectorIndex for UsearchHnswIndex {
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<()>;
    fn remove(&mut self, key: &EntityKey) -> Result<()>;
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>>;
    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<()>;
    fn last_replayed_timestamp(&self) -> u64;
}
```

### 5a. `insert`

```rust
fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<()> {
    if vector.len() != self.dimension {
        return Err(VectorError::DimensionMismatch {
            expected: self.dimension,
            got: vector.len(),
        }.into());
    }

    let cf = self.db.cf_handle("vector_edge_labels").unwrap();
    let (label, is_new) = match key {
        EntityKey::Vertex(id) => (vertex_to_label(*id), !self.inner.contains(vertex_to_label(*id))),
        EntityKey::Edge(cek)  => {
            assign_or_lookup_edge_label(&self.db, &cf, self.prop_key_id, cek,
                                        &mut self.next_edge_label)?
        }
    };

    // If this label already exists in usearch (an update, not a fresh insert):
    // remove the old vector first so usearch doesn't accumulate duplicates.
    if !is_new {
        self.inner.remove(label)
            .map_err(|e| VectorError::BackendError(e.to_string()))?;
        // tombstone_count is NOT incremented — this is a replace, not a delete.
    }

    self.inner.add(label, vector)
        .map_err(|e| VectorError::BackendError(e.to_string()))?;
    Ok(())
}
```

**Update semantics**: a second `insert` for the same key is an upsert. The old
vector is soft-deleted and the new one is inserted at the same label. usearch
handles the `remove + add` sequence correctly and the graph does not accumulate
stale vectors.

**Vertex upsert detection**: `self.inner.contains(label)` detects whether the
vertex was already indexed. This is an O(1) usearch call. No reverse map is needed.

**Crash safety**: for edge inserts, `assign_or_lookup_edge_label` writes the CF
entries atomically before `usearch.add`. If the process crashes after the CF write
but before the usearch insert, WAL replay calls `insert()` again, finds the
existing CF entry, and completes the usearch insert — no duplicate label, no
corruption.

### 5b. `remove`

```rust
fn remove(&mut self, key: &EntityKey) -> Result<()> {
    let cf = self.db.cf_handle("vector_edge_labels").unwrap();
    let label_opt = match key {
        EntityKey::Vertex(id) => {
            let l = vertex_to_label(*id);
            if self.inner.contains(l) { Some(l) } else { None }
        }
        EntityKey::Edge(cek) => {
            let fwd_key = make_fwd_cf_key(self.prop_key_id, cek);
            if let Some(v) = self.db.get_cf(&cf, &fwd_key)? {
                let label = u64::from_le_bytes(v[..8].try_into().unwrap());
                // Delete both CF entries atomically.
                let mut batch = WriteBatch::default();
                batch.delete_cf(&cf, &fwd_key);
                batch.delete_cf(&cf, &make_rev_cf_key(self.prop_key_id, label));
                self.db.write(batch)?;
                Some(label)
            } else {
                None
            }
        }
    };

    if let Some(label) = label_opt {
        self.inner.remove(label)
            .map_err(|e| VectorError::BackendError(e.to_string()))?;
        self.tombstone_count += 1;
    }
    // If key not found, remove is a no-op (idempotent — safe for WAL replay)
    Ok(())
}

### 5c. `search`

```rust
fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>> {
    if query.len() != self.dimension {
        return Err(VectorError::DimensionMismatch {
            expected: self.dimension,
            got: query.len(),
        }.into());
    }

    let results = self.inner.search(query, k)
        .map_err(|e| VectorError::BackendError(e.to_string()))?;

    let cf = self.db.cf_handle("vector_edge_labels").unwrap();
    let mut out = Vec::with_capacity(results.keys.len());
    for (&label, &dist) in results.keys.iter().zip(results.distances.iter()) {
        let key = label_to_key(&self.db, &cf, self.prop_key_id, label, self.entity_type)?;
        out.push((key, dist));
    }
    // usearch returns results sorted by distance ascending (nearest first).
    // VectorIndex contract: search returns (EntityKey, f32) sorted by ascending distance.
    // This matches Cosine (1 - similarity, lower = more similar internally) and L2.
    Ok(out)
}
```

**Score semantics**: usearch returns raw distances, not similarities. For
`MetricKind::Cos`, usearch returns `1 - cosine_similarity`. The `NearestStep`
execution converts this to user-facing score:

```rust
// In NearestStep executor — not in UsearchHnswIndex:
let user_score = match config.metric {
    DistanceMetric::Cosine       => 1.0 - raw_distance,   // [0,1], higher = better
    DistanceMetric::Euclidean    => raw_distance,          // raw L2 distance
    DistanceMetric::InnerProduct => -raw_distance,         // usearch negates IP; restore
};
```

This conversion happens at the traversal layer, not inside `UsearchHnswIndex`,
so the trait method remains metric-agnostic.

### 5d. `save`

```rust
fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<()> {
    // Write usearch binary to a temp file, then read it back as bytes.
    // usearch's save API takes a path string, not a writer.
    let tmp_usearch = path.with_extension("usearch.tmp");
    self.inner.save(tmp_usearch.to_str().unwrap())
        .map_err(|e| VectorError::BackendError(e.to_string()))?;
    let usearch_bytes = std::fs::read(&tmp_usearch)?;
    std::fs::remove_file(&tmp_usearch)?;

    // No map serialization: label mappings live in the `vector_edge_labels` CF
    // and are always current. Only the usearch binary and header metadata are
    // written to the snapshot file.

    // Write final snapshot atomically (write to .tmp, then rename).
    let tmp_path = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp_path)?;
    write_snapshot_header(&mut file, last_replayed_timestamp, self.next_edge_label,
                          self.dimension, self.metric, self.tombstone_count,
                          &usearch_bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp_path, path)?;   // atomic on POSIX; best-effort on Windows

    Ok(())
}
```

The temp-then-rename pattern prevents a partially written snapshot from being
mistaken for a valid one on the next `Graph::open`. See §8d.

### 5e. `last_replayed_timestamp`

```rust
fn last_replayed_timestamp(&self) -> u64 {
    self.last_replayed_timestamp
}
```

Updated by the WAL replay and cold-start rebuild paths (via `set_last_replayed_timestamp`)
after applying all pending entries.

**Why `save()` takes `last_replayed_timestamp` as a parameter rather than reading
`self.last_replayed_timestamp`**: the snapshot flush runs on a background thread (v0.3)
or is triggered by `Graph::close`. By the time the flush begins, new WAL entries
may have already been committed and applied to the in-memory index — meaning
`self.last_replayed_timestamp` reflects those newer entries. The flush caller passes the
seq value from the moment the snapshot bytes were captured (before any subsequent
commits), ensuring the saved `last_replayed_timestamp` correctly marks the boundary up
to which the snapshot is authoritative. Passing it as a parameter makes this
snapshot-time capture explicit and avoids a TOCTOU race.

---

## 6. `load_vector_index` free function

`load` is not a trait method to avoid `dyn` object-safety issues (see
`design_vector_search.md` §8b). It is a free function:

```rust
pub fn load_vector_index(
    path:   &Path,
    config: &VectorIndexConfig,
    db:     &Arc<DB>,   // needed by UsearchHnswIndex for vector_edge_labels CF
) -> Result<Box<dyn VectorIndex>> {
    if !path.exists() {
        // No snapshot — return an empty index. The caller MUST run
        // cold_start_rebuild() instead of WAL replay: the props CF scan
        // covers all committed data and is faster than edge-by-edge WAL
        // replay. After the rebuild, last_replayed_timestamp is set to
        // the current WAL high watermark so future opens use WAL replay.
        return Ok(Box::new(UsearchHnswIndex::empty(config, db.clone())));
    }

    let bytes = std::fs::read(path)?;
    let header = read_snapshot_header(&bytes)?;

    // Sanity checks — catch snapshot/config mismatches before loading.
    // Dimension or metric mismatch: the snapshot was built with incompatible
    // parameters and cannot be safely loaded. Warn and trigger cold-start rebuild
    // rather than hard-erroring, so that a config change (e.g. switching from
    // Cosine to Euclidean, or resizing dimension) recovers automatically.
    if header.dimension != config.dimension || header.metric != config.metric {
        log::warn!(
            "vector index snapshot mismatch for '{}': stored (dim={}, metric={:?}) vs \
             configured (dim={}, metric={:?}) — discarding snapshot; caller MUST run cold_start_rebuild, not WAL replay",
            config.property, header.dimension, header.metric,
            config.dimension, config.metric,
        );
        return Ok(Box::new(UsearchHnswIndex::empty(config)));
    }
    // Algorithm mismatch: snapshot was built by a different implementation
    // (e.g. BruteForce snapshot loaded for an HNSW config). Warn; caller MUST run
    // cold_start_rebuild, not WAL replay.
    if header.algorithm != config.algorithm.kind_byte() {
        log::warn!(
            "vector index snapshot algorithm mismatch for '{}': stored kind={} vs \
             configured kind={} — discarding snapshot and rebuilding from props CF",
            config.property, header.algorithm, config.algorithm.kind_byte(),
        );
        return Ok(Box::new(UsearchHnswIndex::empty(config)));
    }

    // Write usearch bytes to temp file and load.
    let tmp = path.with_extension("load.tmp");
    std::fs::write(&tmp, &header.usearch_bytes)?;
    let inner = Index::new(&usearch_options(config))?;
    inner.load(tmp.to_str().unwrap())
        .map_err(|e| VectorError::SnapshotCorrupt(e.to_string()))?;
    std::fs::remove_file(&tmp).ok();   // best-effort cleanup

    // No map deserialization: label mappings live in the `vector_edge_labels` CF
    // and are always current. The index is ready to serve immediately after
    // loading the usearch binary — no O(N) HashMap rebuild on startup.

    Ok(Box::new(UsearchHnswIndex {
        inner,
        db:              db.clone(),   // Arc<DB> passed in by Graph::open
        prop_key_id:     config.prop_key_id,
        entity_type:     config.entity_type,
        next_edge_label: header.next_edge_label,
        last_replayed_timestamp: header.last_replayed_timestamp,
        tombstone_count: header.tombstone_count,
        metric:          config.metric,
        dimension:       config.dimension,
    }))
}
```

`UsearchHnswIndex::empty` constructs an index with `last_replayed_timestamp = 0`,
`next_edge_label = 0`, and `tombstone_count = 0`. No label maps are allocated — they
live in the `vector_edge_labels` CF.

**`last_replayed_timestamp = 0` is the sentinel** that `Graph::open` uses to decide
between WAL replay and cold-start rebuild:

- `last_replayed_timestamp = 0` → call `cold_start_rebuild` (props CF scan), which
  sets `last_replayed_timestamp = current_hwm` before returning. The subsequent WAL
  replay finds no entries with `ts > current_hwm` and is a no-op.
- `last_replayed_timestamp > 0` → the snapshot is valid; skip `cold_start_rebuild`
  and go straight to WAL replay from `last_replayed_timestamp`.

This means the snapshot mismatch path (returning `empty()`) correctly triggers a
full props CF scan — NOT a WAL replay from timestamp 0, which would replay the entire
WAL history (potentially years of individual insert/delete operations) instead of
performing the much faster sequential props CF scan.

---

## 7. Tombstone tracking

usearch's `size()` method returns the count of live (non-tombstoned) vectors.
Tombstoned vectors remain in the graph structure until a rebuild; usearch's
`remove` marks them dead without immediately freeing the graph slot.

usearch does not expose a tombstone count. We track it in `tombstone_count: u64`:

- **Incremented** by `remove` when the key is found (via bit-cast for vertices, via CF lookup for edges).
- **Never decremented** — tombstones are only reclaimed on rebuild.
- **Reset to 0** on cold-start rebuild or after `change_vector_index_algorithm`
  rebuilds a fresh index.
- **Serialized** in the snapshot header so it survives restarts.

**Tombstone ratio** (used by `VectorIndexStats` and the rebuild threshold):

```rust
fn tombstone_ratio(&self) -> f32 {
    let total = self.inner.size() as u64 + self.tombstone_count;
    if total == 0 { return 0.0; }
    self.tombstone_count as f32 / total as f32
}
```

**Design principle: tombstones are normal lifecycle state, not data corruption.
`Graph::open` must never block on a maintenance operation.**

**Rebuild trigger**: when `tombstone_ratio >= REBUILD_THRESHOLD` (default 0.30),
`Graph::open` logs a warning and sets `VectorIndexStats.rebuild_in_progress = true`
— **queries are accepted immediately; the existing index serves requests**.
The synchronous blocking-rebuild path is removed entirely from `Graph::open`.

```
Graph::open()
  ├─ Load snapshot + replay WAL
  ├─ Compute tombstone_ratio
  ├─ if ratio > 0.30:
  │     WARN "index 'embedding': 35% tombstones — queries may degrade. Call rebuild_vector_index()."
  │     set index.rebuild_in_progress = true
  └─ return Ok(graph)          ← immediately available
```

**Result correctness with tombstones**: usearch filters tombstoned entries from
result sets during search — they are traversed for graph navigation but never
appear in `nearest` output. Results remain correct; the cost is roughly
`tombstone_ratio` extra distance computations per query. At 30% tombstones,
expect ~30% slower queries, not wrong answers.

**Explicit rebuild is the only synchronous path**: `Graph.rebuild_vector_index(property)`
is a caller-controlled maintenance call. The caller chooses when to pay the
3–15 minute cost (see `design_vector_api.md` §6d).

**Alternative — `drop_vector_index`**: if the deletion was a bulk cleanup and the
index will not be re-used, `drop_vector_index("embedding")` removes the index and
its snapshot entirely. No rebuild needed; tombstone state is gone. Re-declaring the
index later (`add_vector_index`) starts with `tombstone_count = 0`.

**v0.3**: the background rebuild thread (`design_hnsw_rebuild.md`) triggers
automatically when the threshold is exceeded. It builds a fresh `UsearchHnswIndex`
from the props CF while queries are served from the existing index, then atomically
swaps the new index in, clears `rebuild_in_progress`, and resets `tombstone_count` to 0.

**Startup blocker — snapshot integrity failure**: if `load_vector_index` detects a
CRC-32C mismatch or magic/version error (§8a), it cannot serve queries from a
corrupt snapshot. In this case `Graph::open` discards the snapshot and triggers a
synchronous cold-start rebuild before accepting queries. This is the only
legitimate startup blocker: a correctness failure (corrupt data), not a performance
degradation (excess tombstones).

---

## 8. Snapshot file format

### 8a. File layout

```
Offset  Length  Field
──────  ──────  ──────────────────────────────────────────────────────────────
0       4       magic = 0x52475F56  ("RG_V" in ASCII, big-endian u32)
4       2       format_version = 2  (u16 BE; increment on incompatible changes)
6       8       last_replayed_timestamp   (u64 LE)
14      4       dimension           (u32 LE)
18      1       metric              (u8: 0=Cosine, 1=Euclidean, 2=InnerProduct)
19      1       algorithm           (u8: 0=BruteForce, 1=HNSW, 2=reserved)
20      8       tombstone_count     (u64 LE)
28      8       next_edge_label     (u64 LE; monotonic counter for edge label assignment)
36      8       usearch_payload_len (u64 LE; byte length of the usearch block)
44      N       usearch_payload     (N = usearch_payload_len bytes)
44+N    4       crc32c              (CRC-32C of all preceding bytes)
```

Total header overhead: 44 bytes + 4-byte trailer = 48 bytes per snapshot file.

**Why no map blocks**: label mappings live in the `vector_edge_labels` CF, which is always
current. There is nothing to serialize or deserialize. This eliminates the O(N) snapshot
encoding/decoding that was the source of the startup stall at 100M edges.

**format_version bumped to 2**: v1 snapshots included `reverse_map_len` and `forward_edge_len`
fields and bincode blocks after the usearch payload. `load_vector_index` must reject v1
snapshots with a `VectorError::SnapshotCorrupt` message instructing the operator to delete
the old snapshot and trigger a cold-start rebuild.

The `algorithm` byte enables `load_vector_index` to detect when a snapshot was
built by a different implementation than the one currently configured (e.g. an
HNSW snapshot loaded for an IVF config after the user calls
`change_vector_index_algorithm`). On mismatch, the snapshot is discarded and a
cold-start rebuild is triggered rather than returning a hard error — the stored
data in the props CF is always the ground truth.

**Magic and version** allow `load_vector_index` to detect stale files, files from
a different RocksGraph installation, and forward-compatibility issues (higher
`format_version` → return `VectorError::SnapshotCorrupt` with a clear message).

**CRC-32C** (Castagnoli, same as RocksDB's checksums): computed over all bytes
from offset 0 to 43+N+M, appended as little-endian u32. Detects bit-flips and
truncation. Checked before parsing any fields.

### 8b. Label storage in `vector_edge_labels` CF

Label mappings are NOT stored in the snapshot file. They live exclusively in the
`vector_edge_labels` RocksDB Column Family, which is opened as part of the normal
`DB::open_cf` call. There is no deserialization step at startup and no encoding
step during snapshot flush.

**CF key/value layout**:

```
Forward lookup — [prop_key_id: u16 BE][0x00][src_id: i64 LE][label_id: i32 LE][dst_id: i64 LE][rank: u16 LE]
  → value: [label: u64 LE]

Reverse lookup — [prop_key_id: u16 BE][0x01][label: u64 BE]
  → value: [src_id: i64 LE][label_id: i32 LE][dst_id: i64 LE][rank: u16 LE]
```

**Entry sizes on disk** (raw, before RocksDB compression):
- Forward entry: 25-byte key + 8-byte value = 33 bytes/edge
- Reverse entry: 11-byte key + 22-byte value = 33 bytes/edge
- Total on disk (before compression): ≈ 66 bytes/edge; at 100 M edges ≈ 6.6 GB
- After snappy compression (typical 2–3×): ≈ 2–3 GB — within normal RocksDB range

**Vertex indexes** write no CF entries — labels are recovered by bit-casting the
usearch-returned u64 back to i64. Zero CF overhead for vertex indexes at any scale.

**CF compaction**: the `vector_edge_labels` CF uses the same compaction settings as
other RocksDB CFs. Deleted entries (from `remove()`) are tombstoned and reclaimed on
the next compaction cycle. No separate maintenance is needed.

### 8c. File naming and location

```
{db_path}/vector_idx_{entity_type}_{prop_key}.snapshot
```

Where `entity_type` is `"v"` for `Vertex` and `"e"` for `Edge`, and `prop_key`
is the property name. Examples:

```
/data/mygraph/vector_idx_v_embedding.snapshot
/data/mygraph/vector_idx_e_embedding.snapshot
/data/mygraph/vector_idx_v_title_embedding.snapshot
```

The names are safe for all filesystems: only ASCII alphanumeric, underscores,
and dots. Property names with non-ASCII characters are percent-encoded (rare in
practice; property names are typically short ASCII strings).

### 8d. Atomic write

On POSIX (Linux, macOS), `rename` is atomic: a partial snapshot is never visible
as the current file. The sequence is:

```
1. Write to {path}.tmp
2. fsync {path}.tmp  (data on disk)
3. rename {path}.tmp → {path}
4. fsync parent directory  (directory entry updated)
```

On Windows, `rename` replaces the destination atomically in NTFS but requires
the destination to not exist. We first delete the old snapshot, then rename:

```rust
#[cfg(windows)]
fn atomic_rename(from: &Path, to: &Path) -> io::Result<()> {
    let _ = std::fs::remove_file(to);   // ignore error if not exists
    std::fs::rename(from, to)
}
```

This leaves a small non-atomic window on Windows; acceptable for a crash-recovery
system where the WAL can replay any missed entries.

---

## 9. Cold-start rebuild (no snapshot or discarded snapshot)

`Graph::open` calls `cold_start_rebuild` whenever `load_vector_index` returns an index
with `last_replayed_timestamp = 0`. This happens in three cases:

1. First-ever open — no snapshot file exists on disk.
2. Snapshot discarded due to dimension/metric/algorithm mismatch (§6) — the config
   changed since the snapshot was written, so the snapshot cannot be loaded.
3. After `drop_vector_index` + `add_vector_index` (explicit reindex) — the old snapshot
   was deleted, and the new index starts from scratch.

**WAL replay is a no-op after cold-start rebuild**: `cold_start_rebuild` reads the
current WAL clock high watermark from `__meta` CF and sets `last_replayed_timestamp =
current_hwm` before returning. The subsequent WAL replay step seeks from `current_hwm`
and finds no entries — all committed data was already incorporated by the props CF scan.
This is why the snapshot mismatch path must go through `cold_start_rebuild` rather than
WAL replay from timestamp 0: replaying the entire WAL history edge-by-edge is slower and
logically redundant when the props CF already contains the authoritative current state.

`Graph::open` then calls `cold_start_rebuild`:

```rust
fn cold_start_rebuild(
    db:     &DB,
    config: &VectorIndexConfig,
    index:  &mut UsearchHnswIndex,
) -> Result<u64> {
    // Choose CF based on entity type
    let cf = match config.entity_type {
        VectorEntityType::Vertex => db.cf_handle("vertex_props").unwrap(),
        VectorEntityType::Edge   => db.cf_handle("edge_props").unwrap(),
    };

    let prop_key = &config.property;
    let mut count = 0u64;

    // Sequential scan of the entire props CF.
    // Key prefix determines entity identity; value is encoded GValue.
    let iter = db.iterator_cf(&cf, IteratorMode::Start);
    for item in iter {
        let (raw_key, raw_value) = item?;
        let (entity_key, stored_prop_key) = decode_props_cf_key(&raw_key)?;
        if stored_prop_key.as_str() != prop_key.as_str() {
            continue;
        }
        let gvalue = decode_gvalue(&raw_value)?;
        if let GValue::FloatVector(vec) = gvalue {
            index.insert(&entity_key, &vec)?;
            count += 1;
        }
    }

    // Set last_replayed_timestamp to the current WAL clock high watermark so
    // subsequent recovery only replays entries written after this rebuild.
    let current_hwm = db.get_cf(&db.cf_handle("__meta").unwrap(), b"vector_wal_clock_hwm")?
        .map(|v| u64::from_le_bytes(v[..8].try_into().unwrap()))
        .unwrap_or(0);
    index.last_replayed_timestamp = current_hwm;

    log::info!("cold-start rebuild: {} vectors for property '{}' (hwm={})",
               count, prop_key, current_hwm);
    Ok(count)
}
```

**Performance**: at 1M × 1536-dim vectors, this scan reads ~6 GB of vector data
from RocksDB sequential storage and inserts into usearch at ~200 μs/insert:
total ~3–15 minutes (I/O bound, not CPU bound). This is the only slow open;
subsequent opens load the snapshot + replay WAL entries (typically <1 second).

**Full CF scan limitation**: the props CF is keyed by `(entity_key, prop_key_id)`.
There is no index on `prop_key_id` alone, so the scan cannot seek directly to
FloatVector entries — it must read every key in the CF and skip those whose
`prop_key` does not match. For a graph with 10M vertices and 100M+ property
entries, the scan itself (before HNSW insertion even begins) can take minutes,
dominated by CF read throughput, not insert throughput.

For v0.3, a `vector_metadata` secondary CF keyed by `(prop_key, entity_key)`
will allow cold-start rebuild to seek directly to the relevant entries without
scanning unrelated properties. Until then, cold-start time scales with the total
number of properties in the graph, not just the vector-carrying entities.

**This cold-start path is triggered** in two cases:
1. First-ever open of a graph with declared vector indexes
2. After `drop_vector_index` + `add_vector_index` (explicit reindex)

---

## 10. WAL replay onto an existing snapshot

Called after `load_vector_index` succeeds (snapshot loaded). Replays WAL
entries with `ts > last_replayed_timestamp` for each index independently via
prefix seek:

```rust
fn wal_replay(
    db:      &DB,
    indexes: &mut HashMap<(VectorEntityType, SmolStr), Box<dyn VectorIndex>>,
) -> Result<()> {
    let wal_cf = db.cf_handle(CF_VECTOR_WAL).unwrap();

    // Each index replays its own prefix independently — no global scan.
    for ((entity_type, prop_key), index) in indexes.iter_mut() {
        let prop_key_id  = schema_registry.intern(prop_key);
        let cutoff_ts    = index.last_replayed_timestamp();

        let mut prefix = [0u8; 3];
        prefix[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
        prefix[2] = *entity_type as u8;

        let iter = db.iterator_cf(
            &wal_cf,
            IteratorMode::From(&prefix, Direction::Forward),
        );

        for item in iter {
            let (raw_key, raw_value) = item?;
            if !raw_key.starts_with(&prefix) {
                break;  // left this index's key space
            }
            let entry_ts = u64::from_be_bytes(raw_key[3..11].try_into().unwrap());
            if entry_ts <= cutoff_ts {
                continue;   // already applied in a prior session
            }

            let op = decode_vector_op(&raw_value)?;
            match op.kind {
                VectorOpKind::Insert(vec) => index.insert(&op.entity_key, &vec)?,
                VectorOpKind::Delete      => index.remove(&op.entity_key)?,
            }
        }

        // Advance the index timestamp to the current WAL clock value.
        let current_hwm = db.get_cf(&db.cf_handle("__meta").unwrap(), b"vector_wal_clock_hwm")?
            .map(|v| u64::from_le_bytes(v[..8].try_into().unwrap()))
            .unwrap_or(0);
        index.set_last_replayed_timestamp(current_hwm);
    }

    Ok(())
}
```

**Per-index prefix seek**: entries for each `(prop_key_id, entity_type)` sort
together in the WAL CF. Recovery for each index is a targeted seek — no global
scan, no cross-index interleaving. Entries for undeclared indexes are never
visited.

**`set_last_replayed_timestamp`**: add a sixth method to the `VectorIndex` trait:

```rust
pub trait VectorIndex: Send + Sync {
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<()>;
    fn remove(&mut self, key: &EntityKey) -> Result<()>;
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>>;
    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<()>;
    fn last_replayed_timestamp(&self) -> u64;
    fn set_last_replayed_timestamp(&mut self, seq: u64);   // ← added
}
```

---

## 11. Per-query `ef_search` override

`design_vector_api.md` §6e specifies `.withEfSearch(ef)` as a per-query override.
The `NearestStep` executor resolves this as follows:

```rust
// In the Nearest step executor:
fn execute_nearest(
    step:     &NearestStep,
    ef_search: Option<usize>,          // from WithEfSearch modulator, if present
    index:    &Arc<RwLock<Box<dyn VectorIndex>>>,
) -> Result<Vec<(EntityKey, f32)>> {
    let guard = index.read().unwrap();
    guard.search_with_ef(&step.query, step.k, ef_search)
}
```

The `search_with_ef` method is an additional method on `UsearchHnswIndex` (not
on the trait — it's an optimization detail, not part of the stable contract):

```rust
impl UsearchHnswIndex {
    pub fn search_with_ef(
        &self,
        query: &[f32],
        k:     usize,
        ef:    Option<usize>,
    ) -> Result<Vec<(EntityKey, f32)>> {
        // Set ef_search on the index if an override is requested.
        // usearch 2.x exposes expansion_search as a mutable property.
        // This requires the read lock (in search) to serialize access.
        if let Some(ef) = ef {
            // SAFETY: expansion_search is only read during search; we hold
            // the RwLock read lock which prevents concurrent mutation of the
            // index. Setting it here before search and restoring after is safe
            // because no concurrent writer can observe the intermediate state.
            // (The RwLock prevents concurrent index mutations.)
            let old_ef = self.inner.expansion_search;
            // usearch 2.x: set via unsafe or through a provided setter.
            // Check crate API for the exact method; may be:
            //   self.inner.change_expansion_search(ef)
            // or:
            //   unsafe { /* direct field access */ }
            // This is the only point in the code that touches expansion_search.
            unsafe { /* set inner.expansion_search = ef */ };
            let result = self._search_inner(query, k);
            unsafe { /* restore inner.expansion_search = old_ef */ };
            result
        } else {
            self._search_inner(query, k)
        }
    }
}
```

**Implementation note**: the exact mechanism for per-query `ef_search` must be
verified against the locked usearch 2.x version. If usearch does not expose a
setter, the alternative is to store a secondary ef-search-override `AtomicUsize`
alongside the index and pass it as a `SearchContext` if that API is available.
The `RwLock` in `design_vector_concurrency.md` §3 serializes concurrent searches, so
resetting a shared field is safe in this context.

---

## 12. Parameters and defaults

| Parameter                    |         Default         | Config location                         | Notes                                         |
| ---------------------------- | :---------------------: | --------------------------------------- | --------------------------------------------- |
| `m` (HNSW connectivity)      |           16            | `VectorIndexConfig.algorithm`           | Higher → better recall, more memory           |
| `ef_construction`            |           200           | `VectorIndexConfig.algorithm`           | Higher → better recall, slower insert         |
| `ef_search` (default)        |           50            | `DEFAULT_EF_SEARCH` constant            | Overridable per-query via `withEfSearch`      |
| `rebuild_threshold`          |          0.30           | `REBUILD_THRESHOLD` constant            | Tombstone ratio at which background rebuild is scheduled (queries served immediately; no blocking) |
| Snapshot flush interval      | 10 min or 10K mutations | `GraphOptions.vector_snapshot_interval` | Whichever comes first                         |
| Bulk load batch size         |         10,000          | `BulkLoadOptions.batch_size`            | Vectors per RocksDB WriteBatch                |
| Cold-start rebuild log level |         `info`          | n/a                                     | Always logged; duration helps users plan      |

---

## 13. Error cases

| Error                                                        | Where raised                                                           | Handling                                                                                             |
| ------------------------------------------------------------ | ---------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| `VectorError::DimensionMismatch`                             | `insert`, `search`                                                     | Propagated to user; WAL entry not written                                                            |
| `VectorError::BackendError(msg)`                             | `insert`, `remove`, `search` when usearch returns an error             | Propagated; index state unchanged                                                                    |
| `VectorError::SnapshotCorrupt(msg)`                          | `load_vector_index` on magic mismatch, bad CRC, or field inconsistency | Fall back to cold-start rebuild                                                                      |
| Snapshot config mismatch (dim, metric, or algorithm differs) | `load_vector_index`                                                    | Log warning; discard snapshot; cold-start rebuild                                                    |
| Step-4 in-memory update fails after RocksDB commit           | `TxnSession::commit` step 4                                             | **See note below.**                                                                                  |
| `vector_edge_labels` CF missing edge label on `remove`       | `remove` (edge index) when CF entry absent                             | No-op (idempotent WAL replay); logs debug. Only possible if CF entry was never written — indicates prior crash between WAL write and CF write, healed by replay |
| `vector_edge_labels` CF missing edge label on `search`       | `search` returns a label with no CF reverse entry                      | Panics — indicates index corruption (CF and usearch disagree). Cannot produce a correct result silently |
| usearch capacity exceeded                                    | `insert` after `reserve` is exhausted                                  | usearch auto-grows; not an error in practice                                                         |
| WAL entry for undeclared index                               | `wal_replay`                                                           | Silently skipped                                                                                     |

**Step-4 failure after RocksDB commit**: if `index.insert` or `index.remove` in
`TxnSession::commit` step 4 returns an error (e.g. OOM in usearch), the commit
function propagates the error to the caller. However, the RocksDB `WriteBatch`
and the corresponding vector WAL entry are already durably written and cannot be
rolled back. The caller's `Err` result means the transaction data is committed
(the graph mutation is durable) but the in-memory index is temporarily stale.

The correct handling is:
1. Log the error at `error` level with the entity key and property
2. Return `Err` to the caller so they are aware
3. The in-memory index is self-healing on the next `Graph::open`: the WAL entry
   is present and will be replayed, restoring consistency

The inconsistency is bounded to the window between the failed step 4 and the
next restart (or explicit WAL replay). During this window, `nearest` queries
may omit the affected entity. This is identical to the crash-after-commit
inconsistency window that the WAL design already handles. An "unindexed queue"
with retry could shorten this window, but that complexity is deferred to v0.3.

---

## 14. Module layout

```
rocksgraph/src/
  vector/
    mod.rs            — pub use; VectorIndex trait; EntityKey; EdgeKey; VectorEntityType
    brute_force.rs    — BruteForceIndex (v0.1)
    hnsw.rs           — UsearchHnswIndex (v0.2) ← this document
    snapshot.rs       — read_snapshot_header, write_snapshot_header, atomic_rename
    wal_replay.rs     — wal_replay, cold_start_rebuild
    error.rs          — VectorError enum
```

The `vector/` module is a new top-level module alongside `gremlin/`, `planner/`,
`bytecode/`, and `types/`. It is gated behind a `vector` Cargo feature flag for
v0.1 to allow the library to build without vector support (useful for WASM targets
or minimal deployments):

```toml
[features]
default = ["vector"]
vector  = ["dep:usearch"]
```

---

## 15. Implementation checklist

### Types and trait (`rocksgraph/src/vector/`)

- [ ] Define `VectorIndex` trait with 6 methods (including `set_last_replayed_timestamp`)
- [ ] Define `EntityKey`, `EdgeKey` with `Hash + Eq + Clone`
- [ ] Define `VectorEntityType` enum
- [ ] Define `VectorError` enum with all variants from `design_vector_api.md` §8
- [ ] Implement `BruteForceIndex` (v0.1, ~120 lines)

### `UsearchHnswIndex` (`rocksgraph/src/vector/hnsw.rs`)

- [ ] Implement `UsearchHnswIndex::empty(config, db)` constructor
- [ ] Implement `vertex_to_label` / `label_to_vertex` (trivial bit cast)
- [ ] Implement `assign_or_lookup_edge_label` (CF read + atomic CF write of both directions)
- [ ] Implement `make_fwd_cf_key` / `make_rev_cf_key` / `encode_cek` / `decode_cek` helpers
- [ ] Implement `label_to_key` (vertex: bit cast; edge: CF point lookup with panic-on-missing)
- [ ] Implement `tombstone_ratio` helper
- [ ] Implement `VectorIndex::insert` with upsert semantics, dimension check, and CF writes
- [ ] Implement `VectorIndex::remove` with CF lookup for edge label, atomic CF delete batch
- [ ] Implement `VectorIndex::search` with CF-based label-to-key conversion
- [ ] Implement `VectorIndex::save` with temp-then-rename and CRC-32C (no map serialization)
- [ ] Implement `VectorIndex::last_replayed_timestamp` and `set_last_replayed_timestamp`
- [ ] Implement `search_with_ef` for per-query ef override
- [ ] Verify exact usearch 2.x API for `expansion_search` setter and `contains(label)` check
- [ ] Reject format_version=1 snapshots in `load_vector_index` with clear error

### Snapshot I/O (`rocksgraph/src/vector/snapshot.rs`)

- [ ] Implement `write_snapshot_header` writing all fields per §8a (format_version=2, no map blocks)
- [ ] Implement `read_snapshot_header` with CRC check, magic check, version check
- [ ] Implement `atomic_rename` (POSIX fsync + rename; Windows delete + rename)
- [ ] Implement `load_vector_index(path, config, db)` free function

### Startup paths (`rocksgraph/src/vector/wal_replay.rs`)

- [ ] Implement `cold_start_rebuild` (props CF scan, entity key decoding)
- [ ] Implement `wal_replay` (per-index prefix seek by `[prop_key_id][entity_type]`, timestamp cutoff, hwm update)
- [ ] Wire both into `Graph::open` after existing CF setup
- [ ] Open `vector_edge_labels` CF as part of `DB::open_cf` in `Graph::open`

### Integration with `Graph` struct

- [ ] Add `vector_indexes: HashMap<(VectorEntityType, SmolStr), Arc<RwLock<Box<dyn VectorIndex>>>>` to `Graph`
- [ ] Call `init_wal_clock(db)` on `Graph::open`; persist `WAL_CLOCK` to `vector_wal_clock_hwm` on snapshot flush
- [ ] Wire `TxnSession::commit` with pending vector ops (see `design_vector_wal.md` §5)
- [ ] Wire `NearestStep` executor to merge HNSW results with `pending_vector_ops` (see `design_vector_concurrency.md` §5d)
- [ ] Wire `OP_PROPERTY` handler to detect `GValue::FloatVector` and push pending op
- [ ] Wire `OP_DROP` (vertex) to push `VectorOpKind::Delete` for all indexed properties
- [ ] Wire `OP_DROP` (edge) to push `VectorOpKind::Delete` for all indexed edge properties

### Tests

- [ ] Unit test: `insert` + `search` on a small vertex index (3 vectors, dim=4)
- [ ] Unit test: `remove` reduces live count and tombstone_count increments
- [ ] Unit test: `save` + `load_vector_index` round-trip; verify `last_replayed_timestamp` preserved
- [ ] Unit test: CRC mismatch in snapshot → `SnapshotCorrupt` error
- [ ] Unit test: `cold_start_rebuild` on a graph with pre-existing FloatVector props
- [ ] Unit test: `wal_replay` replays 3 ops on top of a snapshot, verifies search results
- [ ] Unit test: edge upsert via CF (same CanonicalEdgeKey inserted twice → same u64 label in CF, tombstone_count unchanged)
- [ ] Unit test: `nearest` inside uncommitted transaction finds pending-insert vertex (RYOW fix §5d)
- [ ] Unit test: `nearest` inside uncommitted transaction does NOT return pending-delete vertex
- [ ] Integration test: full write → commit → close → reopen → nearest → correct results
- [ ] Integration test: crash simulation (write WAL, skip in-memory update) → reopen → correct results
