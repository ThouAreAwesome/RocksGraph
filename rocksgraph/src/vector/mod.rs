// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vector search module. v0.1 provides brute-force KNN via the volcano step;
//! v0.2 adds the `VectorIndex` trait and HNSW via usearch.

use std::{collections::HashMap, sync::Arc, sync::RwLock};

use smol_str::SmolStr;

pub mod brute_force;
pub mod error;
pub mod hnsw;
pub mod traits;

/// Shared, lockable map of declared vector indexes, keyed by
/// `(entity_type, property_name)`.
pub(crate) type VectorIndexMap =
    HashMap<(self::error::VectorEntityType, SmolStr), Arc<RwLock<Box<dyn self::traits::VectorIndex>>>>;

/// Create an empty `VectorIndexMap`.  Useful as a default when no indexes
/// are declared — callers that don't participate in vector search (e.g.
/// unit tests) pass this to satisfy the constructor signature.
#[cfg(test)]
pub(crate) fn empty_vector_index_map() -> Arc<RwLock<VectorIndexMap>> {
    Arc::new(RwLock::new(HashMap::new()))
}

pub use brute_force::{cosine_sim, BruteForceIndex, EntityKey};
pub use error::{VectorEntityType, VectorError};
pub use hnsw::load_vector_index;
pub use traits::{
    AnnAlgorithm, DistanceMetric, HnswConfig, IndexLimitOverride, Quantization, VectorIndex, VectorIndexConfig,
    VectorIndexLimit, VectorRuntimeOptions,
};
