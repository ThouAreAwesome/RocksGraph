// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! [`VectorIndex`] trait and configuration types for vector search.
//!
//! Defines the trait that all vector index implementations must satisfy,
//! plus the structural and environmental configuration types.

use std::path::Path;

use smol_str::SmolStr;

use super::brute_force::EntityKey;
use super::error::{VectorEntityType, VectorError};

// ── Distance metric ──────────────────────────────────────────────────────────

/// The distance or similarity function used for vector comparison.
///
/// The choice of metric is baked into the embedding model's training objective —
/// using the wrong metric silently degrades retrieval quality without raising
/// an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceMetric {
    /// Cosine similarity: `dot(a, b) / (|a| * |b|)`.
    #[default]
    Cosine = 0,
    /// Euclidean (L2) distance: `sqrt(sum((a_i - b_i)^2))`.
    Euclidean = 1,
    /// Inner (dot) product: `sum(a_i * b_i)`.
    DotProduct = 2,
}

// ── Algorithm configuration ──────────────────────────────────────────────────

/// Configuration for the HNSW (Hierarchical Navigable Small World) algorithm.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HnswConfig {
    /// Max number of neighbours per node at layer 0 (2×M) and higher layers (M).
    /// Default: 16.
    pub m: usize,
    /// Number of candidates to evaluate during index construction.
    /// Higher → better recall, slower build. Default: 200.
    pub ef_construction: usize,
    /// Number of candidates to evaluate per query (search-time beam width).
    /// Higher → better recall, slower search. Schema-level; set at index creation. Default: 50.
    pub ef_search: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        Self { m: 16, ef_construction: 200, ef_search: 50 }
    }
}

impl HnswConfig {
    /// Set the max neighbours per node (M parameter).
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Set the number of candidates evaluated during index construction.
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }

    /// Set the default number of candidates evaluated per search query.
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }
}

/// The ANN algorithm backing a vector index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum AnnAlgorithm {
    /// Brute-force linear scan — exact, O(N) per query. v0.1.
    BruteForce = 0,
    /// HNSW via usearch — approximate, O(log N) per query. v0.2.
    Hnsw(HnswConfig) = 1,
}

// ── Quantization ─────────────────────────────────────────────────────────────

/// Scalar precision for vectors stored in the in-memory ANN index.
///
/// The public API and RocksDB storage always use f32. Quantization applies
/// only to the in-memory ANN index — it is a transparent memory optimisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quantization {
    /// Half-precision float (IEEE 754 binary16). Halves memory at <0.1%
    /// additional recall loss. Default for v0.2.
    #[default]
    F16 = 0,
    /// Full-precision float (IEEE 754 binary32). Opt-in for maximum recall.
    F32 = 1,
}

// ── Structural configuration (persisted to CF_SCHEMA) ────────────────────────

/// Structural configuration for a vector index.
///
/// Persisted to CF_SCHEMA on `add_vector_index()` and reloaded automatically
/// on every `Graph::open`. Contains dimension, metric, and algorithm — the
/// parameters that cannot be inferred from data alone.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct VectorIndexConfig {
    /// The property name this index accelerates.
    pub property: SmolStr,
    /// Whether this index covers vertices or edges.
    pub entity_type: VectorEntityType,
    /// The fixed dimension of all vectors in this index.
    pub dimension: usize,
    /// The distance metric used for similarity comparison.
    pub metric: DistanceMetric,
    /// The ANN algorithm and its parameters.
    pub algorithm: AnnAlgorithm,
    /// Scalar precision for in-memory storage. Default: F16.
    pub quantization: Quantization,
}

impl VectorIndexConfig {
    /// Create a new vector index configuration with default quantization (F16).
    pub fn new(
        property: impl Into<SmolStr>,
        entity_type: VectorEntityType,
        dimension: usize,
        metric: DistanceMetric,
        algorithm: AnnAlgorithm,
    ) -> Self {
        Self {
            property: property.into(),
            entity_type,
            dimension,
            metric,
            algorithm,
            quantization: Quantization::default(),
        }
    }

    /// Override the in-memory quantization precision.
    pub fn with_quantization(mut self, quantization: Quantization) -> Self {
        self.quantization = quantization;
        self
    }
}

// ── Environmental configuration (supplied per-open, never persisted) ─────────

/// Per-index memory limit. Applied as a hard boundary before any durable write.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct VectorIndexLimit {
    pub memory_limit_bytes: usize,
}

impl VectorIndexLimit {
    /// Create a memory limit of `bytes` bytes.
    pub fn new(bytes: usize) -> Self {
        Self { memory_limit_bytes: bytes }
    }
}

/// Runtime configuration for a specific vector index, matched by `(entity_type, property)`.
///
/// All fields are optional — only the settings you specify override the graph-wide defaults.
/// Use [`PerIndexOptions::new`] to create one, then chain builder methods for the options you need.
///
/// # Example
/// ```
/// use rocksgraph::schema::{PerIndexOptions, VectorEntityType, VectorIndexLimit};
///
/// let opts = PerIndexOptions::new(VectorEntityType::Vertex, "embedding")
///     .with_memory_limit(VectorIndexLimit::new(512 * 1024 * 1024)); // 512 MiB
/// ```
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PerIndexOptions {
    /// Which index this entry targets.
    pub entity_type: VectorEntityType,
    /// Which index this entry targets.
    pub property: SmolStr,
    /// Per-index memory cap. Overrides `IndexOptions::default_limit` when set.
    /// `None` = fall back to the graph-wide default.
    pub memory_limit: Option<VectorIndexLimit>,
}

impl PerIndexOptions {
    /// Create per-index options targeting `(entity_type, property)`.
    pub fn new(entity_type: VectorEntityType, property: impl Into<SmolStr>) -> Self {
        Self { entity_type, property: property.into(), memory_limit: None }
    }

    /// Set a memory cap for this specific index.
    pub fn with_memory_limit(mut self, limit: VectorIndexLimit) -> Self {
        self.memory_limit = Some(limit);
        self
    }
}

/// Runtime options for vector indexes, supplied at `Graph::open_with_options` time.
///
/// These are **environmental** — never persisted to disk, so a database file
/// created on a large server works correctly on a smaller machine.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    /// Default memory limit applied to every vector index.
    /// `None` = unlimited (expert escape hatch — can OOM if not sized to RAM).
    pub default_limit: Option<VectorIndexLimit>,
    /// Per-index settings matched by `(entity_type, property)`. Takes precedence
    /// over `default_limit` for whichever fields are set.
    pub per_index: Vec<PerIndexOptions>,
}

impl IndexOptions {
    /// Apply a default memory limit to all indexes that have no per-index override.
    pub fn with_default_limit(mut self, limit: VectorIndexLimit) -> Self {
        self.default_limit = Some(limit);
        self
    }

    /// Add per-index settings for a specific index (takes precedence over defaults).
    pub fn with_per_index(mut self, opts: PerIndexOptions) -> Self {
        self.per_index.push(opts);
        self
    }
}

// ── VectorIndex trait ────────────────────────────────────────────────────────

/// The trait that every vector index implementation must satisfy.
///
/// Implementors provide insert, remove, search, save/load, and WAL timestamp
/// tracking. The trait is object-safe (no `Self`-typed constructors) so that
/// indexes can be stored as `Box<dyn VectorIndex>` in the `Graph` struct.
#[allow(dead_code)]
pub(crate) trait VectorIndex: Send + Sync {
    /// Insert or update the vector for an entity key.
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<(), VectorError>;

    /// Remove an entity key from the index. Idempotent: no-op if not present.
    fn remove(&mut self, key: &EntityKey) -> Result<(), VectorError>;

    /// Search for the `k` nearest neighbours to `query`, returning
    /// `(entity_key, distance_or_similarity)` pairs. The returned ordering
    /// depends on the implementation (e.g. ascending distance for HNSW).
    ///
    /// `ef_search` overrides the schema-level beam width for this call only.
    /// `None` uses the value baked in at index creation. For HNSW, higher
    /// values improve recall at the cost of latency; concurrent calls with
    /// different overrides race for the shared setting (acceptable because
    /// it affects recall quality only, not correctness).
    fn search(&self, query: &[f32], k: usize, ef_search: Option<usize>) -> Result<Vec<(EntityKey, f32)>, VectorError>;

    /// Persist the index state to `path`, recording `last_replayed_timestamp`
    /// as the WAL timestamp boundary up to which this snapshot is authoritative.
    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<(), VectorError>;

    /// The WAL timestamp of the last entry applied to this index.
    fn last_replayed_timestamp(&self) -> u64;

    /// Advance the replayed timestamp after WAL catch-up or cold-start rebuild.
    fn set_last_replayed_timestamp(&mut self, seq: u64);

    /// Apply memory limit from `IndexOptions`. No-op for BruteForce.
    fn set_memory_limit(&mut self, _limit_bytes: usize) {}

    /// The distance metric this index was configured with.
    fn metric(&self) -> DistanceMetric;
}
