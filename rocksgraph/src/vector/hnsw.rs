// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! HNSW vector index backed by the usearch crate.
//!
//! `UsearchHnswIndex` implements [`VectorIndex`] using the usearch C++ library's
//! HNSW (Hierarchical Navigable Small World) graph. Vertex keys are directly
//! bit-cast `i64 → u64`; edge indexes are not yet supported (v0.3).

use std::collections::HashMap;
use std::path::Path;

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::brute_force::EntityKey;
use super::error::{VectorEntityType, VectorError};
use super::persistence::{load_snapshot_file, save_snapshot_file, SnapshotHeader};
use super::traits::{DistanceMetric, Quantization, VectorIndex, VectorIndexConfig};
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
    }
}

// ── UsearchHnswIndex ────────────────────────────────────────────────────────

/// Initial capacity reserved at index construction — usearch requires
/// `reserve` before any `add`.  Will be driven by `IndexOptions`
/// once that is wired through to index construction.
pub(crate) const DEFAULT_RESERVE_CAPACITY: usize = 1000;

/// HNSW vector index backed by the usearch crate.
///
/// Vertex keys map directly: `vertex_id as u64`. Edge indexes use an internal
/// bidirectional label table (`edge_to_label` / `label_to_edge`) since usearch
/// only supports `u64` labels and `CanonicalEdgeKey` is 22 bytes. The maps are
/// initialized in `new()` when `config.entity_type == Edge`; they remain `None`
/// for Vertex-only indexes.
///
/// Edge support is gated by the schema layer (v0.3). When the gate is removed,
/// the TODOs inside `key_to_label`, `label_to_key`, `remove`, `save`, and
/// `load_vector_index` are the only remaining steps.
pub struct UsearchHnswIndex {
    inner: Index,
    config: VectorIndexConfig,
    tombstone_count: u64,
    last_replayed_timestamp: u64,
    memory_limit_bytes: Option<usize>,
    default_ef_search: usize,
    // Edge label table — None for Vertex-only indexes.
    #[allow(dead_code)] // TODO(v0.4): used when assigning labels for Edge keys
    next_edge_label: u64,
    label_to_edge: Option<HashMap<u64, CanonicalEdgeKey>>,
    edge_to_label: Option<HashMap<CanonicalEdgeKey, u64>>,
}

impl std::fmt::Debug for UsearchHnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsearchHnswIndex")
            .field("config", &self.config)
            .field("size", &self.inner.size())
            .field("capacity", &self.inner.capacity())
            .field("tombstones", &self.tombstone_count)
            .field("last_replayed_timestamp", &self.last_replayed_timestamp)
            .field("edge_label_count", &self.label_to_edge.as_ref().map(|m| m.len()))
            .finish()
    }
}

impl UsearchHnswIndex {
    pub fn new(config: &VectorIndexConfig) -> Result<Self, VectorError> {
        let options = IndexOptions {
            dimensions: config.dimension,
            metric: metric_to_usearch(config.metric),
            quantization: scalar_kind(config.quantization),
            connectivity: config.algorithm_connectivity(),
            expansion_add: config.algorithm_expansion_add(),
            expansion_search: config.algorithm_ef_search(),
            ..Default::default()
        };

        let inner = Index::new(&options).map_err(|e| VectorError::Internal(format!("usearch index creation: {e}")))?;

        inner.reserve(DEFAULT_RESERVE_CAPACITY).map_err(|e| VectorError::Internal(format!("usearch reserve: {e}")))?;

        let is_edge = config.entity_type == VectorEntityType::Edge;

        Ok(Self {
            inner,
            config: config.clone(),
            default_ef_search: config.algorithm_ef_search(),
            tombstone_count: 0,
            last_replayed_timestamp: 0,
            memory_limit_bytes: None,
            next_edge_label: 0,
            label_to_edge: if is_edge { Some(HashMap::new()) } else { None },
            edge_to_label: if is_edge { Some(HashMap::new()) } else { None },
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
    fn key_to_label(&mut self, key: &EntityKey) -> Result<u64, VectorError> {
        match key {
            EntityKey::Vertex(id) => {
                if *id < 0 {
                    return Err(VectorError::Internal(format!("invalid negative vertex id for vector index: {id}")));
                }
                Ok(*id as u64)
            }
            EntityKey::Edge(edge_key) => {
                let map = self
                    .edge_to_label
                    .as_mut()
                    .ok_or_else(|| VectorError::Internal("edge key used with vertex-only index".into()))?;
                if let Some(&label) = map.get(edge_key) {
                    return Ok(label);
                }
                // TODO(v0.4): assign label, store in edge_to_label and label_to_edge, return label.
                let _ = map; // suppress unused-mut warning until TODO is implemented
                Err(VectorError::Unsupported("edge vector indexes are not yet supported (v0.3)".into()))
            }
        }
    }

    /// Reverse label → `EntityKey` mapping.
    ///
    /// For Vertex indexes: direct `label as i64` cast.
    /// For Edge indexes: lookup in `label_to_edge` table.
    #[inline]
    fn label_to_key(&self, label: u64) -> EntityKey {
        if let Some(map) = &self.label_to_edge {
            if let Some(&edge_key) = map.get(&label) {
                return EntityKey::Edge(edge_key);
            }
        }
        EntityKey::Vertex(label as i64)
    }

    /// Returns the number of live (non-tombstoned) entries.
    #[allow(dead_code)]
    pub fn live_count(&self) -> usize {
        self.inner.size()
    }

    /// Returns the tombstone ratio: fraction of entries that are soft-deleted.
    #[allow(dead_code)]
    pub fn tombstone_ratio(&self) -> f32 {
        let total = self.live_count() as u64 + self.tombstone_count;
        if total == 0 {
            return 0.0;
        }
        self.tombstone_count as f32 / total as f32
    }
}

// ── VectorIndex impl ────────────────────────────────────────────────────────

impl VectorIndex for UsearchHnswIndex {
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<(), VectorError> {
        if vector.len() != self.config.dimension {
            return Err(VectorError::DimensionMismatch { expected: self.config.dimension, actual: vector.len() });
        }

        let label = self.key_to_label(key)?;

        // Upsert: remove old entry first, then add.
        if self.inner.contains(label) {
            self.inner
                .remove(label)
                .map_err(|e| VectorError::Internal(format!("usearch remove before upsert: {e}")))?;
            self.tombstone_count += 1; // remove creates a tombstone
        }

        // Expand capacity if needed — usearch does not auto-grow.
        let cur_cap = self.inner.capacity();
        if self.inner.size() >= cur_cap {
            let new_cap = (cur_cap * 2).max(DEFAULT_RESERVE_CAPACITY);

            self.inner.reserve(new_cap).map_err(|e| VectorError::Internal(format!("usearch reserve: {e}")))?;
        }

        self.inner.add(label, vector).map_err(|e| VectorError::Internal(format!("usearch add: {e}")))?;

        Ok(())
    }

    fn remove(&mut self, key: &EntityKey) -> Result<(), VectorError> {
        let label = self.key_to_label(key)?;

        if self.inner.contains(label) {
            self.inner.remove(label).map_err(|e| VectorError::Internal(format!("usearch remove: {e}")))?;
            self.tombstone_count += 1;
            // TODO(v0.4): for Edge keys, remove from edge_to_label and label_to_edge maps here.
        }
        // Idempotent: no-op if key not found.
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize, ef_search: Option<usize>) -> Result<Vec<(EntityKey, f32)>, VectorError> {
        if query.len() != self.config.dimension {
            return Err(VectorError::DimensionMismatch { expected: self.config.dimension, actual: query.len() });
        }

        if k == 0 || self.inner.size() == 0 {
            return Ok(Vec::new());
        }

        let prev_ef = if let Some(ef) = ef_search {
            self.inner.change_expansion_search(ef);
            Some(ef)
        } else {
            None
        };
        let matches = self.inner.search(query, k).map_err(|e| VectorError::Internal(format!("usearch search: {e}")))?;
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
            tombstone_count: self.tombstone_count,
            payload_len: usearch_buf.len(),
        };
        save_snapshot_file(path, &header, &usearch_buf)
    }

    fn last_replayed_timestamp(&self) -> u64 {
        self.last_replayed_timestamp
    }

    fn set_last_replayed_timestamp(&mut self, seq: u64) {
        self.last_replayed_timestamp = seq;
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

    let options = IndexOptions {
        dimensions: config.dimension,
        metric: metric_to_usearch(config.metric),
        quantization: scalar_kind(config.quantization),
        connectivity: config.algorithm_connectivity(),
        expansion_add: config.algorithm_expansion_add(),
        expansion_search: config.algorithm_ef_search(),
        ..Default::default()
    };

    let inner = Index::new(&options).map_err(|e| VectorError::Internal(format!("usearch create for load: {e}")))?;
    inner.load_from_buffer(&usearch_bytes).map_err(|e| VectorError::Internal(format!("usearch load: {e}")))?;

    let is_edge = config.entity_type == VectorEntityType::Edge;

    // TODO(v0.4): deserialize edge label maps from snapshot when edge index support is implemented.
    Ok(UsearchHnswIndex {
        inner,
        config: config.clone(),
        tombstone_count: header.tombstone_count,
        last_replayed_timestamp: header.last_replayed_timestamp,
        memory_limit_bytes: None,
        default_ef_search: config.algorithm_ef_search(),
        next_edge_label: 0,
        label_to_edge: if is_edge { Some(HashMap::new()) } else { None },
        edge_to_label: if is_edge { Some(HashMap::new()) } else { None },
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
    use crate::vector::traits::HnswConfig;
    use crate::vector::VectorEntityType;

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

    #[test]
    fn test_insert_search() {
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(3), &[0.7, 0.7, 0.0, 0.0]).unwrap();

        let results = idx.search(&[1.0, 0.0, 0.0, 0.0], 2, None).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, EntityKey::Vertex(1)); // exact match
    }

    #[test]
    fn test_remove() {
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(&EntityKey::Vertex(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(idx.live_count(), 2);
        idx.remove(&EntityKey::Vertex(1)).unwrap();
        assert_eq!(idx.live_count(), 1);
        assert_eq!(idx.tombstone_count, 1);
    }

    #[test]
    fn test_remove_idempotent() {
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
        idx.remove(&EntityKey::Vertex(999)).unwrap(); // no-op
        assert_eq!(idx.tombstone_count, 0);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
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
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let err = idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_edge_key_rejected_on_vertex_index() {
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
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
        let mut index = UsearchHnswIndex::new(&config).unwrap();

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
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
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
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
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
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
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
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
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

        let mut idx_f32 = UsearchHnswIndex::new(&cfg_f32).unwrap();
        let mut idx_f16 = UsearchHnswIndex::new(&cfg_f16).unwrap();

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
}
