// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0
//! HNSW vector index backed by the usearch crate.
//!
//! `UsearchHnswIndex` implements [`VectorIndex`] using the usearch C++ library's
//! HNSW (Hierarchical Navigable Small World) graph. Vertex keys are directly
//! bit-cast `i64 → u64`; edge indexes are not yet supported (v0.3).

use std::io::Write;
use std::path::Path;

use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use super::brute_force::EntityKey;
use super::error::VectorError;
use super::traits::{DistanceMetric, Quantization, VectorIndex, VectorIndexConfig};

// ── Snapshot constants ──────────────────────────────────────────────────────

/// Magic bytes: "RG_V" in ASCII, big-endian u32.
#[allow(dead_code)]
const SNAPSHOT_MAGIC: u32 = 0x52475F56;
/// Current snapshot format version.
#[allow(dead_code)]
const SNAPSHOT_FORMAT_VERSION: u16 = 2;

/// Header size in bytes: magic(4) + version(2) + timestamp(8) + dim(4) +
/// metric(1) + algorithm(1) + tombstone(8) + next_edge_label(8) + payload_len(8) = 44
#[allow(dead_code)]
const SNAPSHOT_HEADER_SIZE: usize = 44;

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
const DEFAULT_RESERVE_CAPACITY: usize = 1000;

/// HNSW vector index backed by the usearch crate.
///
/// Vertex keys map directly: `vertex_id as u64`. Edge keys are not yet
/// supported — `EntityKey::Edge` returns `VectorError::Unsupported`.
pub struct UsearchHnswIndex {
    inner: Index,
    config: VectorIndexConfig,
    tombstone_count: u64,
    last_replayed_timestamp: u64,
}

impl std::fmt::Debug for UsearchHnswIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsearchHnswIndex")
            .field("config", &self.config)
            .field("size", &self.inner.size())
            .field("capacity", &self.inner.capacity())
            .field("tombstones", &self.tombstone_count)
            .field("last_replayed_timestamp", &self.last_replayed_timestamp)
            .finish()
    }
}

impl UsearchHnswIndex {
    #[allow(dead_code)]
    pub fn dimension(&self) -> usize {
        self.config.dimension
    }

    #[allow(dead_code)]
    pub fn metric(&self) -> DistanceMetric {
        self.config.metric
    }

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

        Ok(Self { inner, config: config.clone(), tombstone_count: 0, last_replayed_timestamp: 0 })
    }

    fn key_to_label(key: &EntityKey) -> Result<u64, VectorError> {
        match key {
            EntityKey::Vertex(id) => Ok(*id as u64),
            EntityKey::Edge(_) => {
                Err(VectorError::Unsupported("edge vector indexes are not yet supported (v0.3)".into()))
            }
        }
    }

    /// Reverse vertex label mapping: direct bit-cast u64 → i64.
    fn label_to_key(label: u64) -> EntityKey {
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

        let label = Self::key_to_label(key)?;

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
        let label = Self::key_to_label(key)?;

        if self.inner.contains(label) {
            self.inner.remove(label).map_err(|e| VectorError::Internal(format!("usearch remove: {e}")))?;
            self.tombstone_count += 1;
        }
        // Idempotent: no-op if key not found.
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>, VectorError> {
        if query.len() != self.config.dimension {
            return Err(VectorError::DimensionMismatch { expected: self.config.dimension, actual: query.len() });
        }

        if k == 0 || self.inner.size() == 0 {
            return Ok(Vec::new());
        }

        let matches = self.inner.search(query, k).map_err(|e| VectorError::Internal(format!("usearch search: {e}")))?;

        let mut results = Vec::with_capacity(matches.keys.len());
        for (&label, &dist) in matches.keys.iter().zip(matches.distances.iter()) {
            results.push((Self::label_to_key(label), dist));
        }
        Ok(results)
    }

    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<(), VectorError> {
        // Serialize usearch index to buffer.
        let buf_len = self.inner.serialized_length();
        let mut usearch_buf = vec![0u8; buf_len];
        self.inner.save_to_buffer(&mut usearch_buf).map_err(|e| VectorError::Internal(format!("usearch save: {e}")))?;

        // Write composite snapshot: header + usearch payload + CRC-32C.
        let tmp_path = path.with_extension("snapshot.tmp");
        let mut file = std::fs::File::create(&tmp_path)?;

        // Header (44 bytes)
        file.write_all(&SNAPSHOT_MAGIC.to_be_bytes())?;
        file.write_all(&SNAPSHOT_FORMAT_VERSION.to_be_bytes())?;
        file.write_all(&last_replayed_timestamp.to_le_bytes())?;
        file.write_all(&(self.config.dimension as u32).to_le_bytes())?;
        file.write_all(&[self.config.metric as u8])?;
        // algorithm byte: 0=BruteForce, 1=HNSW
        file.write_all(&[1u8])?; // Always HNSW for this index type
        file.write_all(&self.tombstone_count.to_le_bytes())?;
        // next_edge_label: always 0 in v0.2 (no edge support)
        file.write_all(&0u64.to_le_bytes())?;
        file.write_all(&(usearch_buf.len() as u64).to_le_bytes())?;

        // usearch payload
        file.write_all(&usearch_buf)?;

        // CRC-32C of all bytes written so far
        // TODO: compute CRC by streaming through the file writer (or via a
        // multiplexing writer) rather than duplicating the header encoding
        // here.  Any header field added in save must also be added in the
        // hasher block below and in load_vector_index — three edit points for
        // one logical field.  A tee-writer that hashes on the fly would
        // collapse this to one.
        let crc = {
            let mut hasher = crc32fast::Hasher::new();
            // Recompute from what we've written — since File doesn't expose
            // already-written data easily, we compute offline.
            // Magic
            hasher.update(&SNAPSHOT_MAGIC.to_be_bytes());
            hasher.update(&SNAPSHOT_FORMAT_VERSION.to_be_bytes());
            hasher.update(&last_replayed_timestamp.to_le_bytes());
            hasher.update(&(self.config.dimension as u32).to_le_bytes());
            hasher.update(&[self.config.metric as u8]);
            hasher.update(&[1u8]); // algorithm
            hasher.update(&self.tombstone_count.to_le_bytes());
            hasher.update(&0u64.to_le_bytes()); // next_edge_label
            hasher.update(&(usearch_buf.len() as u64).to_le_bytes());
            hasher.update(&usearch_buf);
            hasher.finalize()
        };
        file.write_all(&crc.to_le_bytes())?;

        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    fn last_replayed_timestamp(&self) -> u64 {
        self.last_replayed_timestamp
    }

    fn set_last_replayed_timestamp(&mut self, seq: u64) {
        self.last_replayed_timestamp = seq;
    }
}

// ── Snapshot loading ────────────────────────────────────────────────────────

/// Load a vector index from a snapshot file.
///
/// This is a free function (not a trait method) to avoid `dyn` object-safety
/// issues with constructors returning `Self`.
#[allow(dead_code)]
pub fn load_vector_index(path: &Path, config: &VectorIndexConfig) -> Result<UsearchHnswIndex, VectorError> {
    let bytes = std::fs::read(path)?;

    if bytes.len() < SNAPSHOT_HEADER_SIZE + 4 {
        return Err(VectorError::Internal("snapshot file too short".into()));
    }

    // Read header fields
    let magic = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    if magic != SNAPSHOT_MAGIC {
        return Err(VectorError::Internal("snapshot magic mismatch".into()));
    }

    let format_version = u16::from_be_bytes(bytes[4..6].try_into().unwrap());
    if format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(VectorError::Unsupported(format!("unsupported snapshot format version {format_version}")));
    }

    let timestamp = u64::from_le_bytes(bytes[6..14].try_into().unwrap());
    let stored_dim = u32::from_le_bytes(bytes[14..18].try_into().unwrap()) as usize;
    let stored_metric_byte = bytes[18];
    // algorithm byte at offset 19 — read but ignored (we know it's HNSW)
    let stored_tombstone = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
    // next_edge_label at offset 28 — always 0 in v0.2
    let payload_len = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize;

    // Guard against truncated/corrupt files where payload_len extends past the
    // actual bytes (would panic on the CRC slice below).
    if bytes.len() < 44 + payload_len + 4 {
        return Err(VectorError::Internal("snapshot file too short".into()));
    }

    if stored_dim != config.dimension {
        return Err(VectorError::DimensionMismatch { expected: config.dimension, actual: stored_dim });
    }

    // Verify metric match
    let stored_metric = match stored_metric_byte {
        0 => DistanceMetric::Cosine,
        1 => DistanceMetric::Euclidean,
        2 => DistanceMetric::DotProduct,
        _ => return Err(VectorError::Internal("unknown metric in snapshot".into())),
    };
    if stored_metric != config.metric {
        return Err(VectorError::Unsupported(format!(
            "snapshot metric ({stored_metric:?}) does not match config ({:?})",
            config.metric
        )));
    }

    // Verify CRC-32C
    let expected_crc = u32::from_le_bytes(bytes[44 + payload_len..44 + payload_len + 4].try_into().unwrap());
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..44 + payload_len]);
    let actual_crc = hasher.finalize();
    if expected_crc != actual_crc {
        return Err(VectorError::Internal("snapshot CRC mismatch — file may be corrupt".into()));
    }

    // Load usearch from buffer
    let usearch_bytes = &bytes[44..44 + payload_len];
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
    inner.load_from_buffer(usearch_bytes).map_err(|e| VectorError::Internal(format!("usearch load: {e}")))?;

    Ok(UsearchHnswIndex {
        inner,
        config: config.clone(),
        tombstone_count: stored_tombstone,
        last_replayed_timestamp: timestamp,
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

        let results = idx.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
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
        let results = loaded.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results[0].0, EntityKey::Vertex(1));
    }

    #[test]
    fn test_dimension_mismatch() {
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let err = idx.insert(&EntityKey::Vertex(1), &[1.0, 0.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_edge_unsupported() {
        use crate::types::keys::CanonicalEdgeKey;
        let mut idx = UsearchHnswIndex::new(&test_config()).unwrap();
        let ek = EntityKey::Edge(CanonicalEdgeKey { src_id: 1, label_id: 1, dst_id: 2, rank: 0 });
        let err = idx.insert(&ek, &[1.0, 0.0, 0.0, 0.0]).unwrap_err();
        assert!(matches!(err, VectorError::Unsupported(_)));
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
            let hnsw_results = index.search(&query, k).unwrap();
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
        let res_zero = idx.search(&[1.0, 0.0, 0.0, 0.0], 0).unwrap();
        assert_eq!(res_zero.len(), 0);

        // k > size returns all available items without error or overflow
        let res_large = idx.search(&[1.0, 0.0, 0.0, 0.0], 100).unwrap();
        assert_eq!(res_large.len(), 2);
    }
}
