// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Brute-force exact KNN index. v0.1 reference implementation for the
//! [`VectorIndex`](super::traits::VectorIndex) trait (v0.2).

use std::path::Path;

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
}

#[allow(dead_code)]
impl BruteForceIndex {
    pub fn new() -> Self {
        Self::default()
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
            self.entries.iter().map(|(key, vec)| (key.clone(), cosine_sim(vec, query))).collect();
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
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            self.entries[pos].1 = vector.to_vec();
        } else {
            self.entries.push((key.clone(), vector.to_vec()));
        }
        Ok(())
    }

    fn remove(&mut self, key: &EntityKey) -> Result<(), VectorError> {
        self.entries.retain(|(k, _)| k != key);
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>, VectorError> {
        if k == 0 || self.entries.is_empty() {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(EntityKey, f32)> =
            self.entries.iter().map(|(key, vec)| (key.clone(), cosine_sim(vec, query))).collect();
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
}
