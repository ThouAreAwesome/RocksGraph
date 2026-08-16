// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HNSW vector index backed by the usearch crate.
//!
//! `UsearchHnswIndex` implements [`VectorIndex`] using the usearch C++ library's
//! HNSW (Hierarchical Navigable Small World) graph. Vertex keys are directly
//! bit-cast `i64 → u64`; edge indexes are not yet supported (v0.3).

use std::{collections::HashMap, path::Path};

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::{
    brute_force::EntityKey,
    error::{VectorEntityType, VectorError},
    persistence::{load_snapshot_file, save_snapshot_file, SnapshotHeader},
    traits::{DistanceMetric, Quantization, VectorIndex, VectorIndexConfig},
};
use crate::types::keys::CanonicalEdgeKey;

fn metric_to_usearch(m: DistanceMetric) -> MetricKind {
    match m {
        DistanceMetric::Cosine => MetricKind::Cos,
        DistanceMetric::Euclidean => MetricKind::L2sq,
        DistanceMetric::DotProduct => MetricKind::IP,
    }
}

fn scalar_kind(q: Quantization) -> ScalarKind {
    match q {
        Quantization::F16 => ScalarKind::F16,
        Quantization::F32 => ScalarKind::F32,
        Quantization::RaBitQ { .. } => ScalarKind::B1,
    }
}

// ── UsearchHnswIndex ────────────────────────────────────────────────────────

/// Initial capacity reserved at index construction — usearch requires
/// `reserve` before any `add`.  Will be driven by `IndexOptions`
/// once that is wired through to index construction.
pub(crate) const DEFAULT_RESERVE_CAPACITY: usize = 1000;

/// usearch pre-allocates a fixed pool of per-thread scratch buffers sized by
/// the `threads` argument to `reserve_capacity_and_threads` (default: just
/// `hardware_concurrency()` if unspecified via the bare `reserve(capacity)`).
/// Any operation from a thread beyond that pool size fails with "Reserve
/// capacity ahead of insertions/searches!" — a distinct failure mode from
/// vector-count capacity, confirmed empirically: a workload combining rayon's
/// internal pool with a handful of dedicated searcher threads exceeded the
/// hardware-concurrency default. Reserve generously so this is unreachable
/// under realistic concurrency rather than merely "usually enough".
fn reserved_thread_count() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) * 4
}

/// Maps CanonicalEdgeKey to usearch's u64 labels, as usearch only supports u64.
#[derive(Debug)]
pub(crate) struct EdgeLabelMap {
    #[allow(dead_code)] // TODO(v0.4): used when assigning labels for Edge keys
    next_edge_label: std::sync::atomic::AtomicU64,
    label_to_edge: std::sync::RwLock<HashMap<u64, CanonicalEdgeKey>>,
    edge_to_label: std::sync::RwLock<HashMap<CanonicalEdgeKey, u64>>,
}

impl EdgeLabelMap {
    fn new() -> Self {
        Self {
            next_edge_label: std::sync::atomic::AtomicU64::new(0),
            label_to_edge: std::sync::RwLock::new(HashMap::new()),
            edge_to_label: std::sync::RwLock::new(HashMap::new()),
        }
    }

    fn key_to_label(&self, edge_key: &CanonicalEdgeKey) -> Result<u64, VectorError> {
        if let Some(&label) = self.edge_to_label.read().unwrap().get(edge_key) {
            return Ok(label);
        }
        // TODO(v0.4): assign label, store in edge_to_label and label_to_edge, return label.
        Err(VectorError::Unsupported("edge vector indexes are not yet supported (v0.3)".into()))
    }

    fn label_to_key(&self, label: u64) -> Option<CanonicalEdgeKey> {
        self.label_to_edge.read().unwrap().get(&label).copied()
    }

    fn count(&self) -> usize {
        self.label_to_edge.read().unwrap().len()
    }
}

/// Sharded mutexes for serializing concurrent upsert/remove operations on the
/// same key. usearch's `add` has no atomic upsert (checked: no `update`
/// method exists, and `add` on an already-existing label is rejected
/// outright with "Duplicate keys not allowed") — an upsert is remove-then-add,
/// two separate calls. Without per-key serialization, two threads upserting
/// the same key can both pass `contains()`, both remove, and the second's
/// `add` then collides with the first's fresh entry — confirmed empirically.
/// Sharding (rather than one global lock) keeps operations on *different*
/// keys fully concurrent, which is what `resize_lock`'s read side is for in
/// the first place.
pub(crate) struct ShardedUpsertLocks {
    locks: Vec<parking_lot::Mutex<()>>,
}

impl ShardedUpsertLocks {
    fn new(shards: usize) -> Self {
        Self { locks: (0..shards).map(|_| parking_lot::Mutex::new(())).collect() }
    }

    fn lock(&self, label: u64) -> parking_lot::MutexGuard<'_, ()> {
        self.locks[(label as usize) % self.locks.len()].lock()
    }
}

const UPSERT_LOCK_SHARDS: usize = 64;

/// HNSW vector index backed by the usearch crate.
///
/// Vertex keys map directly: `vertex_id as u64`. Edge indexes use an internal
/// bidirectional label table since usearch only supports `u64` labels.
///
/// Edge support is gated by the schema layer (v0.3). When the gate is removed,
/// the TODOs in `EdgeLabelMap`, `remove`, `save`, and `load_vector_index` are
/// the only remaining steps.
pub struct UsearchHnswIndex {
    inner: Index,
    config: VectorIndexConfig,
    tombstone_count: std::sync::atomic::AtomicU64,
    last_replayed_timestamp: std::sync::atomic::AtomicU64,
    memory_limit_bytes: Option<usize>,
    default_ef_search: usize,

    /// Edge label table — None for Vertex-only indexes.
    edge_map: Option<EdgeLabelMap>,

    /// Guards `inner`'s capacity against concurrent `insert`/`remove`/`search`.
    /// usearch's `reserve` is NOT safe to call concurrently with any other operation.
    resize_lock: parking_lot::RwLock<()>,

    /// Per-key serialization for upserts/removes to avoid conflicts on the same key.
    upsert_locks: ShardedUpsertLocks,

    pub(crate) rabitq_transform: Option<crate::vector::rabitq::RaBitQTransform>,
}

impl std::fmt::Debug for UsearchHnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsearchHnswIndex")
            .field("config", &self.config)
            .field("size", &self.inner.size())
            .field("capacity", &self.inner.capacity())
            .field("tombstones", &self.tombstone_count)
            .field("last_replayed_timestamp", &self.last_replayed_timestamp.load(std::sync::atomic::Ordering::Relaxed))
            .field("edge_label_count", &self.edge_map.as_ref().map(|m| m.count()))
            .finish()
    }
}

enum OldVector {
    F32(Vec<f32>),
    Packed(Vec<u8>),
}

impl UsearchHnswIndex {
    pub fn new(config: &VectorIndexConfig) -> Result<Self, VectorError> {
        let mut dimensions = config.dimension;
        if let Quantization::RaBitQ { seed } = config.quantization {
            let actual_seed = seed.unwrap_or(42);
            let transform = crate::vector::rabitq::RaBitQTransform::new(config.dimension, actual_seed, config.metric);
            dimensions = transform.pad_dim() + 64; // pad_dim bits + 8 bytes (64 bits) trailer
        }
        let mut rabitq_transform = None;

        let options = IndexOptions {
            dimensions,
            metric: metric_to_usearch(config.metric),
            quantization: scalar_kind(config.quantization),
            connectivity: config.algorithm_connectivity(),
            expansion_add: config.algorithm_expansion_add(),
            expansion_search: config.algorithm_ef_search(),
            ..Default::default()
        };

        let mut inner =
            Index::new(&options).map_err(|e| VectorError::Internal(format!("usearch index creation: {e}")))?;

        if let Quantization::RaBitQ { seed } = config.quantization {
            let actual_seed = seed.unwrap_or(42);
            let transform = crate::vector::rabitq::RaBitQTransform::new(config.dimension, actual_seed, config.metric);
            let metric_fn = crate::vector::rabitq::create_rabitq_metric(transform.pad_dim(), config.metric);
            inner.change_metric::<usearch::b1x8>(metric_fn);
            rabitq_transform = Some(transform);
        }

        inner
            .reserve_capacity_and_threads(DEFAULT_RESERVE_CAPACITY, reserved_thread_count())
            .map_err(|e| VectorError::Internal(format!("usearch reserve: {e}")))?;

        let is_edge = config.entity_type == VectorEntityType::Edge;

        Ok(Self {
            inner,
            config: config.clone(),
            default_ef_search: config.algorithm_ef_search(),
            tombstone_count: std::sync::atomic::AtomicU64::new(0),
            last_replayed_timestamp: std::sync::atomic::AtomicU64::new(0),
            memory_limit_bytes: None,
            edge_map: if is_edge { Some(EdgeLabelMap::new()) } else { None },
            resize_lock: parking_lot::RwLock::new(()),
            upsert_locks: ShardedUpsertLocks::new(UPSERT_LOCK_SHARDS),
            rabitq_transform,
        })
    }

    /// Maps an `EntityKey` to a usearch `u64` label.
    ///
    /// Vertex keys use a direct `vertex_id as u64` cast. Edge keys use an
    /// internal incrementing label table (populated in `edge_to_label`).
    ///
    /// Returns `Internal` if an edge key is presented to a vertex-only index
    /// (wrong entity type). Returns `Unsupported` for edge keys on an edge
    /// index until edge support is fully implemented (TODO v0.4).
    #[inline]
    fn key_to_label(&self, key: &EntityKey) -> Result<u64, VectorError> {
        match key {
            EntityKey::Vertex(id) => {
                if *id < 0 {
                    return Err(VectorError::Internal(format!("invalid negative vertex id for vector index: {id}")));
                }
                Ok(*id as u64)
            }
            EntityKey::Edge(edge_key) => {
                let map = self
                    .edge_map
                    .as_ref()
                    .ok_or_else(|| VectorError::Internal("edge key used with vertex-only index".into()))?;
                map.key_to_label(edge_key)
            }
        }
    }

    /// Reverse label → `EntityKey` mapping.
    ///
    /// For Vertex indexes: direct `label as i64` cast.
    /// For Edge indexes: lookup in `label_to_edge` table.
    #[inline]
    fn label_to_key(&self, label: u64) -> EntityKey {
        if let Some(map) = &self.edge_map {
            if let Some(edge_key) = map.label_to_key(label) {
                return EntityKey::Edge(edge_key);
            }
        }
        EntityKey::Vertex(label as i64)
    }

    /// Fetches the old vector from the index if it exists.
    fn fetch_old_vector(&self, label: u64) -> Option<OldVector> {
        if !self.inner.contains(label) {
            return None;
        }
        if self.rabitq_transform.is_some() {
            let dimensions = self.inner.dimensions();
            let mut buf = vec![0u8; dimensions];
            let b1x8_slice = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut usearch::b1x8, buf.len()) };
            match self.inner.get(label, b1x8_slice) {
                Ok(n) if n > 0 => {
                    buf.truncate(dimensions.div_ceil(8));
                    Some(OldVector::Packed(buf))
                }
                _ => None,
            }
        } else {
            let mut buf = vec![0.0f32; self.config.dimension];
            match self.inner.get(label, &mut buf) {
                Ok(n) if n > 0 => Some(OldVector::F32(buf)),
                _ => None,
            }
        }
    }

    /// Removes the old vector (if any), creating a tombstone.
    /// This is step 1 of an upsert. Note: during the window between remove and add,
    /// a concurrent search may observe the key as absent.
    fn upsert_remove_old(&self, label: u64, has_old_vector: bool) -> Result<(), VectorError> {
        if has_old_vector {
            self.inner
                .remove(label)
                .map_err(|e| VectorError::Internal(format!("usearch remove before upsert: {e}")))?;
            self.tombstone_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// Attempt to restore the old vector after a failed add, to avoid permanently dropping it.
    fn restore_on_failure(&self, label: u64, old_vector: Option<OldVector>, add_err: String) -> VectorError {
        if let Some(old) = old_vector {
            let res = match &old {
                OldVector::F32(vec) => self.inner.add(label, vec.as_slice()),
                OldVector::Packed(buf) => {
                    let b1x8_slice = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const usearch::b1x8, buf.len()) };
                    self.inner.add(label, b1x8_slice)
                }
            };
            if res.is_err() {
                return VectorError::Internal(format!(
                    "usearch add failed ({add_err}) and restoring the previous vector also failed — \
                     '{label}' may be missing from the index until the next rebuild"
                ));
            }
        }
        VectorError::Internal(format!("usearch add: {add_err}"))
    }

    /// The slow path for `insert`: acquires exclusive access, grows capacity, and adds the vector.
    fn grow_capacity_and_add(
        &self,
        label: u64,
        vector: &[f32],
        old_vector: Option<OldVector>,
        b1x8_slice: Option<&[usearch::b1x8]>,
    ) -> Result<(), VectorError> {
        let _guard = self.resize_lock.write();
        let cur_cap = self.inner.capacity();

        // Re-reserve capacity and threads. Even if `size < cur_cap`, the fast path might have
        // failed due to thread-slot exhaustion in usearch, so we unconditionally re-reserve.
        let new_cap = if self.inner.size() >= cur_cap { (cur_cap * 2).max(DEFAULT_RESERVE_CAPACITY) } else { cur_cap };
        self.inner
            .reserve_capacity_and_threads(new_cap, reserved_thread_count())
            .map_err(|e| VectorError::Internal(format!("usearch reserve: {e}")))?;

        let res =
            if let Some(packed) = b1x8_slice { self.inner.add(label, packed) } else { self.inner.add(label, vector) };
        res.map_err(|e| self.restore_on_failure(label, old_vector, e.to_string()))?;
        Ok(())
    }

    /// Returns the number of live (non-tombstoned) entries.
    #[allow(dead_code)]
    pub fn live_count(&self) -> usize {
        self.inner.size()
    }

    /// Returns the tombstone ratio: fraction of entries that are soft-deleted.
    #[allow(dead_code)]
    pub fn tombstone_ratio(&self) -> f32 {
        let total = self.live_count() as u64 + self.tombstone_count.load(std::sync::atomic::Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.tombstone_count.load(std::sync::atomic::Ordering::Relaxed) as f32 / total as f32
    }
}

// ── VectorIndex impl ────────────────────────────────────────────────────────

impl VectorIndex for UsearchHnswIndex {
    fn insert(&self, key: &EntityKey, vector: &[f32]) -> Result<(), VectorError> {
        if vector.len() != self.config.dimension {
            return Err(VectorError::DimensionMismatch { expected: self.config.dimension, actual: vector.len() });
        }

        let label = self.key_to_label(key)?;

        // Serializes concurrent operations on THIS key.
        let _key_guard = self.upsert_locks.lock(label);

        // Fetch the old vector (if any) before removing it, to restore on failure.
        let old_vector = self.fetch_old_vector(label);

        // Fast path: usearch's `add` and `remove` are safe to call concurrently with
        // each other across distinct keys.
        let packed_vector =
            if let Some(ref transform) = self.rabitq_transform { transform.transform_and_pack(vector) } else { vec![] };

        let b1x8_slice = if !packed_vector.is_empty() {
            // SAFETY: usearch::b1x8 is a transparent u8 wrapper, and packed_vector has the exact byte length expected.
            let ptr = packed_vector.as_ptr() as *const usearch::b1x8;
            Some(unsafe { std::slice::from_raw_parts(ptr, packed_vector.len()) })
        } else {
            None
        };

        {
            let _guard = self.resize_lock.read();
            self.upsert_remove_old(label, old_vector.is_some())?;
            let res = if let Some(packed) = b1x8_slice {
                self.inner.add(label, packed)
            } else {
                self.inner.add(label, vector)
            };
            if res.is_ok() {
                return Ok(());
            }
        }

        // Slow path: out of capacity or thread slots.
        self.grow_capacity_and_add(label, vector, old_vector, b1x8_slice)
    }

    fn remove(&self, key: &EntityKey) -> Result<(), VectorError> {
        let label = self.key_to_label(key)?;

        let _key_guard = self.upsert_locks.lock(label);
        let _guard = self.resize_lock.read();
        if self.inner.contains(label) {
            self.inner.remove(label).map_err(|e| VectorError::Internal(format!("usearch remove: {e}")))?;
            self.tombstone_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // TODO(v0.4): for Edge keys, remove from edge_map here.
        }
        Ok(())
    }

    fn reserve(&self, capacity: usize) -> Result<(), VectorError> {
        let _guard = self.resize_lock.write();
        self.inner
            .reserve_capacity_and_threads(capacity, reserved_thread_count())
            .map_err(|e| VectorError::Internal(format!("usearch reserve: {e}")))?;
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize, ef_search: Option<usize>) -> Result<Vec<(EntityKey, f32)>, VectorError> {
        if query.len() != self.config.dimension {
            return Err(VectorError::DimensionMismatch { expected: self.config.dimension, actual: query.len() });
        }

        // Read side of `resize_lock`: must not run concurrently with `reserve`.
        let _guard = self.resize_lock.read();

        if k == 0 || self.inner.size() == 0 {
            return Ok(Vec::new());
        }

        let prev_ef = if let Some(ef) = ef_search {
            self.inner.change_expansion_search(ef);
            Some(ef)
        } else {
            None
        };

        let matches = if let Some(ref transform) = self.rabitq_transform {
            let mut rotated = transform.rotate(query);
            if self.metric() == DistanceMetric::Cosine {
                let norm = rotated.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    rotated.iter_mut().for_each(|x| *x /= norm);
                }
            }
            
            crate::vector::rabitq::CURRENT_QUERY.with(|q| {
                *q.borrow_mut() = Some(rotated);
            });
            let bits_len = transform.pad_dim().div_ceil(8);
            let dummy_bytes = vec![0u8; bits_len + 8];
            // SAFETY: Dummy bytes buffer corresponds directly to b1x8 which is a wrapper over u8.
            let dummy_b1x8_slice =
                unsafe { std::slice::from_raw_parts(dummy_bytes.as_ptr() as *const usearch::b1x8, dummy_bytes.len()) };

            let res = self
                .inner
                .search(dummy_b1x8_slice, k)
                .map_err(|e| VectorError::Internal(format!("usearch search: {e}")));
            crate::vector::rabitq::CURRENT_QUERY.with(|q| *q.borrow_mut() = None);
            res?
        } else {
            self.inner.search(query, k).map_err(|e| VectorError::Internal(format!("usearch search: {e}")))?
        };

        if prev_ef.is_some() {
            self.inner.change_expansion_search(self.default_ef_search);
        }

        let mut results = Vec::with_capacity(matches.keys.len());
        for (&label, &dist) in matches.keys.iter().zip(matches.distances.iter()) {
            results.push((self.label_to_key(label), dist));
        }
        Ok(results)
    }

    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<(), VectorError> {
        // TODO(v0.4): serialize edge label maps (next_edge_label, label_to_edge) alongside the
        // usearch buffer when edge index support is implemented.

        // Serialize usearch index to buffer.
        let buf_len = self.inner.serialized_length();
        let mut usearch_buf = vec![0u8; buf_len];
        self.inner.save_to_buffer(&mut usearch_buf).map_err(|e| VectorError::Internal(format!("usearch save: {e}")))?;

        let header = SnapshotHeader {
            last_replayed_timestamp,
            dimension: self.config.dimension,
            metric: self.config.metric,
            tombstone_count: self.tombstone_count.load(std::sync::atomic::Ordering::Relaxed),
            payload_len: usearch_buf.len(),
        };
        save_snapshot_file(path, &header, &usearch_buf)
    }

    fn last_replayed_timestamp(&self) -> u64 {
        self.last_replayed_timestamp.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set_last_replayed_timestamp(&self, seq: u64) {
        self.last_replayed_timestamp.store(seq, std::sync::atomic::Ordering::Relaxed);
    }

    fn set_memory_limit(&mut self, limit_bytes: usize) {
        self.memory_limit_bytes = Some(limit_bytes);
    }

    fn metric(&self) -> DistanceMetric {
        self.config.metric
    }

    fn size(&self) -> usize {
        self.inner.size()
    }

    fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    fn dimension(&self) -> usize {
        self.config.dimension
    }

    fn memory_limit_bytes(&self) -> Option<usize> {
        self.memory_limit_bytes
    }

    fn bytes_per_scalar(&self) -> usize {
        match self.config.quantization {
            crate::vector::Quantization::F16 => 2,
            crate::vector::Quantization::F32 => 4,
            crate::vector::Quantization::RaBitQ { .. } => 1,
        }
    }
}

// ── Snapshot loading ────────────────────────────────────────────────────────

/// Load a vector index from a snapshot file.
///
/// This is a free function (not a trait method) to avoid `dyn` object-safety
/// issues with constructors returning `Self`.
pub fn load_vector_index(path: &Path, config: &VectorIndexConfig) -> Result<UsearchHnswIndex, VectorError> {
    let (header, usearch_bytes) = load_snapshot_file(path, config.dimension, config.metric)?;

    let mut dimensions = config.dimension;
    if let Quantization::RaBitQ { seed } = config.quantization {
        let actual_seed = seed.unwrap_or(42);
        let transform = crate::vector::rabitq::RaBitQTransform::new(config.dimension, actual_seed, config.metric);
        dimensions = transform.pad_dim() + 64;
    }

    let options = IndexOptions {
        dimensions,
        metric: metric_to_usearch(config.metric),
        quantization: scalar_kind(config.quantization),
        connectivity: config.algorithm_connectivity(),
        expansion_add: config.algorithm_expansion_add(),
        expansion_search: config.algorithm_ef_search(),
        ..Default::default()
    };

    let mut inner = Index::new(&options).map_err(|e| VectorError::Internal(format!("usearch create for load: {e}")))?;

    inner.load_from_buffer(&usearch_bytes).map_err(|e| VectorError::Internal(format!("usearch load: {e}")))?;

    let mut rabitq_transform = None;
    if let Quantization::RaBitQ { seed } = config.quantization {
        let actual_seed = seed.unwrap_or(42);
        let transform = crate::vector::rabitq::RaBitQTransform::new(config.dimension, actual_seed, config.metric);
        let metric_fn = crate::vector::rabitq::create_rabitq_metric(transform.pad_dim(), config.metric);
        inner.change_metric::<usearch::b1x8>(metric_fn);
        rabitq_transform = Some(transform);
    }

    let is_edge = config.entity_type == VectorEntityType::Edge;

    // TODO(v0.4): deserialize edge label maps from snapshot when edge index support is implemented.
    Ok(UsearchHnswIndex {
        inner,
        config: config.clone(),
        tombstone_count: std::sync::atomic::AtomicU64::new(header.tombstone_count),
        last_replayed_timestamp: std::sync::atomic::AtomicU64::new(header.last_replayed_timestamp),
        memory_limit_bytes: None,
        default_ef_search: config.algorithm_ef_search(),
        edge_map: if is_edge { Some(EdgeLabelMap::new()) } else { None },
        resize_lock: parking_lot::RwLock::new(()),
        upsert_locks: ShardedUpsertLocks::new(UPSERT_LOCK_SHARDS),
        rabitq_transform,
    })
}

// ── Helpers for extracting HNSW config ──────────────────────────────────────

impl VectorIndexConfig {
    fn algorithm_connectivity(&self) -> usize {
        match &self.algorithm {
            super::traits::AnnAlgorithm::Hnsw(c) => c.m,
            super::traits::AnnAlgorithm::BruteForce => 0,
        }
    }

    fn algorithm_expansion_add(&self) -> usize {
        match &self.algorithm {
            super::traits::AnnAlgorithm::Hnsw(c) => c.ef_construction,
            super::traits::AnnAlgorithm::BruteForce => 0,
        }
    }

    fn algorithm_ef_search(&self) -> usize {
        match &self.algorithm {
            super::traits::AnnAlgorithm::Hnsw(c) => c.ef_search,
            super::traits::AnnAlgorithm::BruteForce => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{traits::HnswConfig, VectorEntityType};

    fn test_config() -> VectorIndexConfig {
        VectorIndexConfig {
            property: "embedding".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: crate::vector::traits::AnnAlgorithm::Hnsw(HnswConfig::default()),
            quantization: Quantization::F32,
        }
    }

    /// Well-separated test vector, via a small local LCG (splitmix64-style).
    ///
    /// Two earlier generation schemes were tried and rejected, both
    /// empirically: (1) `[i, 0, 0, 0]`-style colinear vectors are degenerate
    /// under `DistanceMetric::Cosine` — any two positive scalar multiples of
    /// the same direction have similarity 1 ("identical" to the metric),
    /// making self-recall assertions meaningless. (2) `sin(i*13 + d*37)` in
    /// only 4 dimensions produces near-duplicate vectors often enough at
    /// 20,000+ samples to flake self-recall assertions — confirmed by
    /// computing cosine similarity 0.9999997 between two "different"
    /// vectors from that scheme (sin()'s bounded, periodic range loses
    /// precision for large arguments, and 4 dimensions isn't enough space to
    /// avoid collisions at this sample count). An LCG has neither problem:
    /// well-dispersed pseudo-random output, no periodicity at this scale.
    fn test_vector(i: i64) -> Vec<f32> {
        let mut state = (i as u64).wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(1);
        (0..4)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn test_concurrent_insert_no_upfront_reserve() {
        // Regression test for a reliably-reproducible SIGSEGV: concurrent
        // insert() calls (e.g. from IndexManager::rebuild()'s rayon-parallel
        // insert loop) used to call usearch's reserve() reactively whenever
        // capacity ran out, with no synchronization against other threads'
        // concurrent add()/reserve() calls. usearch's `add`/`remove` are safe
        // to call concurrently with each other, but `reserve` is not safe to
        // call concurrently with either — confirmed by reproducing the crash
        // 5/5 runs before this fix, and by confirming it disappears once
        // `reserve` is moved behind `resize_lock`'s exclusive side.
        //
        // Deliberately don't pre-reserve capacity here — starts at
        // DEFAULT_RESERVE_CAPACITY (1000) and must grow reactively many
        // times over the course of this test, exactly the scenario that
        // used to crash.
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let n = 20_000i64;
        use rayon::prelude::*;
        let result: Result<(), VectorError> =
            (0..n).into_par_iter().try_for_each(|i| idx.insert(&EntityKey::Vertex(i), &test_vector(i)));
        result.unwrap();
        assert_eq!(idx.live_count(), n as usize);

        // Content check, not just count: a race that silently dropped or
        // overwrote an entry (rather than crashing) wouldn't show up in
        // live_count() alone if it happened to swap one entry for another.
        // Spot-check a sample spread across the id range, including past
        // each capacity-growth boundary (1000 -> 2000 -> 4000 -> 8000 -> ...).
        //
        // Aggregate threshold, not a per-point exact match: HNSW is
        // approximate, so even with well-separated vectors, self-recall
        // isn't mathematically guaranteed 100% of the time for every single
        // sampled point — confirmed empirically (two "different" vectors at
        // n=20,000 in 4 dimensions can land at cosine similarity 0.998
        // purely by chance, occasionally letting ef_search's beam settle on
        // the near neighbor instead of the exact self-match). A real
        // corruption bug would fail far more than an occasional sample, not
        // borderline-miss one; matches the `RECALL_THRESHOLD`-based
        // aggregate assertion pattern used by the e2e rebuild tests in
        // `graph/tests/vector.rs` for the same reason.
        let sample: Vec<i64> = (0..n).step_by(731).collect();
        let mut hits = 0usize;
        for &i in &sample {
            let results = idx.search(&test_vector(i), 1, None).unwrap();
            if results.first().map(|(k, _)| k) == Some(&EntityKey::Vertex(i)) {
                hits += 1;
            }
        }
        let recall = hits as f32 / sample.len() as f32;
        assert!(recall >= 0.95, "self-recall after concurrent insert too low: {hits}/{} ({recall:.3})", sample.len());
    }

    #[test]
    fn test_concurrent_insert_and_search_no_upfront_reserve() {
        // Mixed workload: one thread inserting (forcing capacity growth via
        // the write side of resize_lock) while others concurrently search
        // (read side) — the scenario the read/write split is actually for.
        // The insert-only regression test above doesn't exercise search()'s
        // guard at all, since nothing else was running concurrently with it.
        let idx = std::sync::Arc::new(UsearchHnswIndex::new(&test_config()).unwrap());
        let n = 15_000i64;

        // Seed one findable entry before the concurrent phase so searchers
        // always have at least one result to retrieve.
        idx.insert(&EntityKey::Vertex(0), &test_vector(0)).unwrap();

        let inserter = {
            let idx = std::sync::Arc::clone(&idx);
            std::thread::spawn(move || {
                use rayon::prelude::*;
                (1..n).into_par_iter().try_for_each(|i| idx.insert(&EntityKey::Vertex(i), &test_vector(i)))
            })
        };

        let searchers: Vec<_> = (0..4)
            .map(|_| {
                let idx = std::sync::Arc::clone(&idx);
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        // Must never panic/crash/deadlock while capacity is
                        // concurrently growing; result content isn't checked
                        // here since the inserter is still in flight.
                        idx.search(&test_vector(0), 5, None).unwrap();
                    }
                })
            })
            .collect();

        inserter.join().unwrap().unwrap();
        for s in searchers {
            s.join().unwrap();
        }

        assert_eq!(idx.live_count(), n as usize);
        // Search must still find vertex 0 once the concurrent phase is over.
        // k=5 rather than an exact top-1 match: HNSW is approximate, and a
        // near-duplicate vector can occasionally rank first by chance (see
        // the aggregate-recall comment in the insert-only regression test
        // above) — this only needs to confirm the entry wasn't lost or
        // corrupted, not exercise search ranking precision.
        let results = idx.search(&test_vector(0), 5, None).unwrap();
        assert!(
            results.iter().any(|(k, _)| *k == EntityKey::Vertex(0)),
            "vertex 0 must still be findable after the concurrent insert+search phase"
        );
    }

    #[test]
    fn test_concurrent_upsert_same_key() {
        // Regression test for a confirmed race: without per-key
        // serialization, two threads upserting the SAME key could both pass
        // `contains(label)`, both remove, and the second thread's `add` then
        // collide with the first thread's fresh entry (usearch has no
        // atomic upsert — rejects `add` on an already-existing label with
        // "Duplicate keys not allowed", confirmed empirically). Every one of
        // these concurrent upserts must succeed, and the index must end up
        // with exactly one live entry for the key, holding one of the
        // racing values (whichever happened to run last).
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &test_vector(0)).unwrap();

        use rayon::prelude::*;
        let result: Result<(), VectorError> =
            (1..=200i64).into_par_iter().try_for_each(|i| idx.insert(&EntityKey::Vertex(1), &test_vector(i)));
        result.unwrap();

        assert_eq!(idx.live_count(), 1, "concurrent upserts of the same key must leave exactly one live entry");
        // Whichever racing value ended up stored, a broad search must find
        // exactly one result — the key itself, not the specific value (that
        // outcome is inherently non-deterministic under the race).
        let results = idx.search(&test_vector(1), 200, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, EntityKey::Vertex(1));
    }

    #[test]
    fn test_insert_search() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(3), &[0.7, 0.7, 0.0, 0.0]).unwrap();

        let results = idx.search(&[1.0, 0.0, 0.0, 0.0], 2, None).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, EntityKey::Vertex(1)); // exact match
    }

    #[test]
    fn test_remove() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(idx.live_count(), 2);
        idx.remove(&EntityKey::Vertex(1)).unwrap();
        assert_eq!(idx.live_count(), 1);
        assert_eq!(idx.tombstone_count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn test_remove_idempotent() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.remove(&EntityKey::Vertex(999)).unwrap(); // no-op
        assert_eq!(idx.tombstone_count.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.snapshot");
        idx.save(&path, 42).unwrap();

        let loaded = load_vector_index(&path, &test_config()).unwrap();
        assert_eq!(loaded.last_replayed_timestamp(), 42);
        assert_eq!(loaded.live_count(), 2);
        let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 2, None).unwrap();
        assert_eq!(results[0].0, EntityKey::Vertex(1));
    }

    #[test]
    fn test_dimension_mismatch() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let err = idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_edge_key_rejected_on_vertex_index() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let ek = EntityKey::Edge(CanonicalEdgeKey { src_id: 1, label_id: 1, dst_id: 2, rank: 0 });
        let err = idx.insert(&ek, &[1.0, 0.0, 0.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::Internal(ref msg) if msg.contains("edge key used with vertex-only index")));
        // remove() takes the same key_to_label path — verify parity
        let err = idx.remove(&ek).unwrap_err();
        assert!(matches!(err, VectorError::Internal(ref msg) if msg.contains("edge key used with vertex-only index")));
    }

    #[test]
    fn test_hnsw_recall_vs_brute_force_large() {
        use crate::vector::{cosine_sim, AnnAlgorithm, DistanceMetric, HnswConfig, Quantization};
        use std::collections::HashSet;

        // Simple deterministic LCG random generator (reproducible, zero dependencies)
        let mut seed: u64 = 42;
        let mut next_f32 = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f32) / ((1u32 << 31) as f32) - 0.5
        };

        let dim = 16;
        let num_vectors = 2000;
        let k = 10;
        let num_queries = 50;

        let config = VectorIndexConfig {
            property: "emb".into(),
            entity_type: crate::vector::VectorEntityType::Vertex,
            dimension: dim,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(HnswConfig { m: 16, ef_construction: 200, ef_search: 64 }),
            quantization: Quantization::F32,
        };
        let index = UsearchHnswIndex::new(&config).unwrap();

        // 1. Generate and insert dataset (2,000 vectors triggers dynamic capacity growth past 1,000 default)
        let mut dataset: Vec<Vec<f32>> = Vec::with_capacity(num_vectors);
        for id in 0..num_vectors {
            let vec: Vec<f32> = (0..dim).map(|_| next_f32()).collect();
            index.insert(&EntityKey::Vertex(id as i64), &vec).unwrap();
            dataset.push(vec);
        }

        // 2. Evaluate recall across queries
        let mut total_hits = 0;
        for _ in 0..num_queries {
            let query: Vec<f32> = (0..dim).map(|_| next_f32()).collect();

            // Exact brute-force top-k ground truth
            let mut exact: Vec<(i64, f32)> =
                dataset.iter().enumerate().map(|(id, vec)| (id as i64, cosine_sim(vec, &query))).collect();
            exact.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let ground_truth: HashSet<i64> = exact.iter().take(k).map(|(id, _)| *id).collect();

            // HNSW top-k
            let hnsw_results = index.search(&query, k, None).unwrap();
            let hnsw_ids: HashSet<i64> = hnsw_results
                .iter()
                .map(|(ek, _)| match ek {
                    EntityKey::Vertex(id) => *id,
                    _ => -1,
                })
                .collect();

            total_hits += ground_truth.intersection(&hnsw_ids).count();
        }

        let avg_recall = total_hits as f64 / (num_queries * k) as f64;
        assert!(avg_recall >= 0.95, "Recall was {:.2}%, expected >= 95%", avg_recall * 100.0);
    }

    #[test]
    fn test_corrupt_snapshot_crc() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt_crc.snapshot");
        idx.save(&path, 10).unwrap();

        // Corrupt a byte in the usearch payload section
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 48);
        bytes[45] ^= 0xFF; // flip bits in payload
        std::fs::write(&path, bytes).unwrap();

        let err = load_vector_index(&path, &test_config()).unwrap_err();
        assert!(matches!(err, VectorError::Internal(msg) if msg.contains("CRC mismatch")));
    }

    #[test]
    fn test_corrupt_snapshot_magic() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt_magic.snapshot");
        idx.save(&path, 10).unwrap();

        // Corrupt magic bytes (first 4 bytes)
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = 0x00;
        std::fs::write(&path, bytes).unwrap();

        let err = load_vector_index(&path, &test_config()).unwrap_err();
        assert!(matches!(err, VectorError::Internal(msg) if msg.contains("magic mismatch")));
    }

    #[test]
    fn test_search_boundary_k() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        // k = 0 returns empty results without error
        let res_zero = idx.search(&[1.0, 0.0, 0.0, 0.0], 0, None).unwrap();
        assert_eq!(res_zero.len(), 0);

        // k > size returns all available items without error or overflow
        let res_large = idx.search(&[1.0, 0.0, 0.0, 0.0], 100, None).unwrap();
        assert_eq!(res_large.len(), 2);
    }

    #[test]
    fn test_reject_negative_vertex_id() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let res = idx.insert(&EntityKey::Vertex(-5), &[1.0, 0.0, 0.0, 0.0]);
        assert!(res.is_err());
        assert!(matches!(res.unwrap_err(), VectorError::Internal(msg) if msg.contains("invalid negative vertex id")));
    }

    #[test]
    fn test_rebuild_changes_quantization() {
        let dim = 32;
        let mut cfg_f32 = test_config();
        cfg_f32.dimension = dim;
        cfg_f32.quantization = Quantization::F32;

        let mut cfg_f16 = test_config();
        cfg_f16.dimension = dim;
        cfg_f16.quantization = Quantization::F16;

        let idx_f32 = UsearchHnswIndex::new(&cfg_f32).unwrap();
        let idx_f16 = UsearchHnswIndex::new(&cfg_f16).unwrap();

        let num_entries = 100;
        for i in 0..num_entries {
            let vec: Vec<f32> = (0..dim).map(|d| ((i * 17 + d * 31) as f32).sin()).collect();
            idx_f32.insert(&EntityKey::Vertex(i as i64), &vec).unwrap();
            idx_f16.insert(&EntityKey::Vertex(i as i64), &vec).unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let path_f32 = dir.path().join("f32.snapshot");
        let path_f16 = dir.path().join("f16.snapshot");

        idx_f32.save(&path_f32, 10).unwrap();
        idx_f16.save(&path_f16, 10).unwrap();

        let f32_size = std::fs::metadata(&path_f32).unwrap().len();
        let f16_size = std::fs::metadata(&path_f16).unwrap().len();

        // F16 quantization should produce a significantly smaller snapshot payload
        assert!(f16_size < f32_size, "F16 snapshot ({f16_size} bytes) should be smaller than F32 ({f32_size} bytes)");

        // Loaded indexes should both produce accurate search results
        let loaded_f32 = load_vector_index(&path_f32, &cfg_f32).unwrap();
        let loaded_f16 = load_vector_index(&path_f16, &cfg_f16).unwrap();

        let query: Vec<f32> = (0..dim).map(|d| ((999 + d * 31) as f32).sin()).collect();
        let res_f32 = loaded_f32.search(&query, 5, None).unwrap();
        let res_f16 = loaded_f16.search(&query, 5, None).unwrap();

        assert_eq!(res_f32.len(), 5);
        assert_eq!(res_f16.len(), 5);
        // Top match should agree
        assert_eq!(res_f32[0].0, res_f16[0].0);
    }
    #[test]
    fn test_capacity_bug_with_tombstones() {
        let idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.reserve(2).unwrap();
        let cap = idx.inner.capacity();
        println!("Capacity after reserve(2): {}", cap);
        for i in 1..=cap {
            idx.insert(&EntityKey::Vertex(i as i64), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        }
        println!(
            "Size: {}, Tombs: {}",
            idx.inner.size(),
            idx.tombstone_count.load(std::sync::atomic::Ordering::Relaxed)
        );
        idx.remove(&EntityKey::Vertex(1)).unwrap();

        let res = idx.insert(&EntityKey::Vertex(99999), &[0.0, 0.0, 1.0, 0.0]);
        assert!(res.is_ok(), "Insert failed: {:?}", res.err());
    }
}

#[cfg(test)]
mod rabitq_tests {
    use super::*;
    use crate::vector::{cosine_sim, AnnAlgorithm, DistanceMetric, HnswConfig, Quantization, VectorEntityType};
    use std::collections::HashSet;

    #[test]
    fn test_rabitq_recall_vs_brute_force_large() {
        let mut seed: u64 = 42;
        let mut next_f32 = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f32) / ((1u32 << 31) as f32) - 0.5
        };

        let dim = 768; // RaBitQ usually needs larger dimensions to show value
        let num_vectors = 2000;
        let k = 10;
        let num_queries = 50;

        let config = VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: dim,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(HnswConfig { m: 16, ef_construction: 200, ef_search: 64 }),
            quantization: Quantization::RaBitQ { seed: Some(42) },
        };
        let index = UsearchHnswIndex::new(&config).unwrap();

        let mut dataset: Vec<Vec<f32>> = Vec::with_capacity(num_vectors);
        for id in 0..num_vectors {
            let vec: Vec<f32> = (0..dim).map(|_| next_f32()).collect();
            index.insert(&EntityKey::Vertex(id as i64), &vec).unwrap();
            dataset.push(vec);
        }

        let mut total_hits = 0;
        for _ in 0..num_queries {
            let query: Vec<f32> = (0..dim).map(|_| next_f32()).collect();
            let mut exact: Vec<(i64, f32)> =
                dataset.iter().enumerate().map(|(id, vec)| (id as i64, cosine_sim(vec, &query))).collect();
            exact.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let ground_truth: HashSet<i64> = exact.iter().take(k).map(|(id, _)| *id).collect();

            let results = index.search(&query, k, Some(200)).unwrap();
            for (key, _) in results {
                if let EntityKey::Vertex(id) = key {
                    if ground_truth.contains(&id) {
                        total_hits += 1;
                    }
                }
            }
        }

        let avg_recall = total_hits as f32 / (num_queries * k) as f32;
        println!("RaBitQ Avg Recall: {:.2}%", avg_recall * 100.0);
        // Expecting around 20-30% without re-ranking
        assert!(avg_recall >= 0.15, "RaBitQ Recall was {:.2}%, expected >= 15%", avg_recall * 100.0);
    }
}
