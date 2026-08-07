// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Brute-force exact KNN index. v0.2 reference implementation for the
//! [`VectorIndex`](super::traits::VectorIndex) trait (v0.2).

use std::path::Path;

use smol_str::SmolStr;

use crate::types::keys::CanonicalEdgeKey;

/// Identifies a vertex or edge that owns a vector property.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityKey {
    Vertex(i64),
    Edge(CanonicalEdgeKey),
}

/// Cosine similarity of two equal-length f32 slices.
/// Uses f64 accumulation for precision; returns 0.0 for zero-magnitude vectors.
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vector dimension mismatch");
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for i in 0..a.len() {
        let av = a[i] as f64;
        let bv = b[i] as f64;
        dot += av * bv;
        na += av * av;
        nb += bv * bv;
    }
    let d = (na * nb).sqrt();
    if d == 0.0 {
        0.0
    } else {
        (dot / d) as f32
    }
}

/// Dot (inner) product of two equal-length f32 slices.
pub(crate) fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vector dimension mismatch");
    a.iter().zip(b.iter()).map(|(&x, &y)| x as f64 * y as f64).sum::<f64>() as f32
}

/// Squared L2 distance of two equal-length f32 slices.
pub(crate) fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vector dimension mismatch");
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum::<f64>() as f32
}

/// Compute a similarity score consistent with `1.0 − usearch_dist` for `metric`.
///
/// - Cosine    → `cosine_sim(a, b)` ∈ [−1, 1]
/// - DotProduct → raw dot product (higher = more similar)
/// - Euclidean → `1.0 − l2_sq(a, b)` (higher = closer)
pub(crate) fn metric_sim(metric: super::traits::DistanceMetric, a: &[f32], b: &[f32]) -> f32 {
    match metric {
        super::traits::DistanceMetric::Cosine => cosine_sim(a, b),
        super::traits::DistanceMetric::DotProduct => dot_product(a, b),
        super::traits::DistanceMetric::Euclidean => 1.0 - l2_sq(a, b),
    }
}

/// Convert a usearch distance value to a similarity score for `metric`.
///
/// Usearch minimizes distance, so the semantics differ per metric:
/// - Cosine    → usearch returns `1.0 − cos`, so `sim = 1.0 − dist`
/// - Euclidean → usearch returns `L2²`,        so `sim = 1.0 − dist`
/// - DotProduct → usearch returns `−dot`,       so `sim = −dist`
///
/// Use this when converting HNSW search results to similarity scores so that
/// HNSW-path and RYOW-path scores (computed via `metric_sim`) are consistent.
pub(crate) fn dist_to_sim(metric: super::traits::DistanceMetric, dist: f32) -> f32 {
    match metric {
        super::traits::DistanceMetric::DotProduct => -dist,
        _ => 1.0 - dist,
    }
}

/// Ephemeral brute-force vector index. Stores (entity_key, vector) pairs
/// and performs exact linear-scan KNN searches.
///
/// In v0.1 this is not directly wired into the traversal engine — the
/// [`NearestStep`](crate::engine::volcano::steps::vector::NearestStep)
/// does inline brute-force. v0.2 will route searches through the
/// [`VectorIndex`](super::traits::VectorIndex) trait, with this struct
/// serving as the fallback / reference implementation.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct BruteForceIndex {
    entries: Vec<(EntityKey, Vec<f32>)>,
    last_replayed_timestamp: u64,
    property: SmolStr,
    memory_limit_bytes: Option<usize>,
    metric: super::traits::DistanceMetric,
}

#[allow(dead_code)]
impl BruteForceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a BruteForceIndex seeded with the index's property name (used in error messages).
    pub fn with_config(config: &super::traits::VectorIndexConfig) -> Self {
        Self { property: config.property.clone(), metric: config.metric, ..Self::default() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert or update the vector for an entity key.
    pub fn insert(&mut self, key: EntityKey, vector: Vec<f32>) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == key) {
            self.entries[pos].1 = vector;
        } else {
            self.entries.push((key, vector));
        }
    }

    /// Remove an entity key from the index.
    pub fn remove(&mut self, key: &EntityKey) {
        self.entries.retain(|(k, _)| k != key);
    }

    /// Exact KNN search: computes cosine similarity against every entry
    /// and returns the top k results sorted by descending similarity.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(EntityKey, f32)> {
        if k == 0 || self.entries.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(EntityKey, f32)> =
            self.entries.iter().map(|(key, vec)| (key.clone(), metric_sim(self.metric, vec, query))).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

// ── VectorIndex trait impl ──────────────────────────────────────────────────

use super::error::VectorError;
use super::traits::VectorIndex;

impl VectorIndex for BruteForceIndex {
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<(), VectorError> {
        let is_update = self.entries.iter().any(|(k, _)| k == key);
        if !is_update {
            if let Some(limit) = self.memory_limit_bytes {
                // Each entry occupies `dim * 4` bytes of vector data.
                let projected =
                    (self.entries.len() + 1).checked_mul(vector.len()).and_then(|x| x.checked_mul(4)).ok_or_else(
                        || VectorError::MemoryLimitExceeded { index: self.property.clone(), used: usize::MAX, limit },
                    )?;
                if projected > limit {
                    return Err(VectorError::MemoryLimitExceeded {
                        index: self.property.clone(),
                        used: projected,
                        limit,
                    });
                }
            }
        }
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            self.entries[pos].1 = vector.to_vec();
        } else {
            self.entries.push((key.clone(), vector.to_vec()));
        }
        Ok(())
    }

    fn set_memory_limit(&mut self, limit_bytes: usize) {
        self.memory_limit_bytes = Some(limit_bytes);
    }

    fn memory_limit_bytes(&self) -> Option<usize> {
        self.memory_limit_bytes
    }

    fn size(&self) -> usize {
        self.entries.len()
    }

    fn remove(&mut self, key: &EntityKey) -> Result<(), VectorError> {
        self.entries.retain(|(k, _)| k != key);
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize, _ef_search: Option<usize>) -> Result<Vec<(EntityKey, f32)>, VectorError> {
        if k == 0 || self.entries.is_empty() {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(EntityKey, f32)> =
            self.entries.iter().map(|(key, vec)| (key.clone(), metric_sim(self.metric, vec, query))).collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    fn save(&self, _path: &Path, _last_replayed_timestamp: u64) -> Result<(), VectorError> {
        // BruteForce is ephemeral — save is a no-op.
        Ok(())
    }

    fn last_replayed_timestamp(&self) -> u64 {
        self.last_replayed_timestamp
    }

    fn set_last_replayed_timestamp(&mut self, seq: u64) {
        self.last_replayed_timestamp = seq;
    }

    fn metric(&self) -> super::traits::DistanceMetric {
        self.metric
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical() {
        let a = vec![1.0f32, 2.0, 3.0];
        assert!((cosine_sim(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_orthogonal() {
        assert!((cosine_sim(&[1.0, 0.0], &[0.0, 1.0]) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_opposite() {
        assert!((cosine_sim(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_index_insert_search() {
        let mut idx = BruteForceIndex::new();
        idx.insert(EntityKey::Vertex(1), vec![1.0, 0.0]);
        idx.insert(EntityKey::Vertex(2), vec![0.0, 1.0]);
        idx.insert(EntityKey::Vertex(3), vec![0.7, 0.7]);

        let results = idx.search(&[1.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, EntityKey::Vertex(1)); // exact match
        assert!((results[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_index_remove() {
        let mut idx = BruteForceIndex::new();
        idx.insert(EntityKey::Vertex(1), vec![1.0, 0.0]);
        idx.insert(EntityKey::Vertex(2), vec![0.0, 1.0]);
        assert_eq!(idx.len(), 2);
        idx.remove(&EntityKey::Vertex(1));
        assert_eq!(idx.len(), 1);
        let results = idx.search(&[1.0, 0.0], 2);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, EntityKey::Vertex(2));
    }

    #[test]
    fn test_index_clear() {
        let mut idx = BruteForceIndex::new();
        idx.insert(EntityKey::Vertex(1), vec![1.0, 0.0]);
        idx.clear();
        assert!(idx.is_empty());
        assert!(idx.search(&[1.0, 0.0], 1).is_empty());
    }

    #[test]
    fn test_search_empty_index() {
        let idx = BruteForceIndex::new();
        assert!(idx.search(&[1.0, 0.0], 5).is_empty());
    }
    #[test]
    fn test_dist_to_sim_all_metrics() {
        use crate::vector::brute_force::dist_to_sim;
        use crate::vector::DistanceMetric;

        assert!((dist_to_sim(DistanceMetric::Cosine, 0.0) - 1.0).abs() < 1e-6);
        assert!((dist_to_sim(DistanceMetric::Cosine, 1.0) - 0.0).abs() < 1e-6);
        assert!((dist_to_sim(DistanceMetric::DotProduct, -5.0) - 5.0).abs() < 1e-6);
        assert!((dist_to_sim(DistanceMetric::DotProduct, 0.0) - 0.0).abs() < 1e-6);
        assert!((dist_to_sim(DistanceMetric::Euclidean, 0.0) - 1.0).abs() < 1e-6);
        assert!((dist_to_sim(DistanceMetric::Euclidean, 1.0) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_metric_sim_consistency() {
        use super::metric_sim;
        use crate::vector::DistanceMetric;

        let a = [1.0, 0.0];
        let b = [0.0, 1.0];
        let c = [1.0, 0.0];

        assert!((metric_sim(DistanceMetric::Cosine, &a, &b)).abs() < 1e-6, "orthogonal: cos=0");
        assert!((metric_sim(DistanceMetric::Cosine, &a, &c) - 1.0).abs() < 1e-6, "identical: cos=1");
        assert!((metric_sim(DistanceMetric::DotProduct, &[2.0, 3.0], &[2.0, 3.0]) - 13.0).abs() < 1e-4);
        assert!((metric_sim(DistanceMetric::Euclidean, &a, &c) - 1.0).abs() < 1e-6, "zero dist: sim=1");
        assert!((metric_sim(DistanceMetric::Euclidean, &[0.0, 0.0], &[1.0, 0.0])).abs() < 1e-6, "L2sq=1: sim=0");
    }

    #[test]
    fn test_dist_to_sim_metric_roundtrip() {
        use super::{cosine_sim, dist_to_sim, dot_product, l2_sq, metric_sim};
        use crate::vector::DistanceMetric;

        let a = [0.6, 0.8];
        let b = [0.8, 0.6];
        let cos = cosine_sim(&a, &b);
        let dot = dot_product(&a, &b);
        let l2 = l2_sq(&a, &b);

        // Cosine: dist = 1-cos  →  dist_to_sim = cos ≈ metric_sim
        assert!((dist_to_sim(DistanceMetric::Cosine, 1.0 - cos) - cos).abs() < 1e-5);
        assert!(
            (dist_to_sim(DistanceMetric::Cosine, 1.0 - cos) - metric_sim(DistanceMetric::Cosine, &a, &b)).abs() < 1e-5
        );

        // DotProduct: dist = -dot  →  dist_to_sim = dot
        assert!((dist_to_sim(DistanceMetric::DotProduct, -dot) - dot).abs() < 1e-5);
        assert!(
            (dist_to_sim(DistanceMetric::DotProduct, -dot) - metric_sim(DistanceMetric::DotProduct, &a, &b)).abs()
                < 1e-5
        );

        // Euclidean: dist = l2  →  dist_to_sim = 1-l2
        assert!((dist_to_sim(DistanceMetric::Euclidean, l2) - (1.0 - l2)).abs() < 1e-5);
        assert!(
            (dist_to_sim(DistanceMetric::Euclidean, l2) - metric_sim(DistanceMetric::Euclidean, &a, &b)).abs() < 1e-5
        );
    }
}
