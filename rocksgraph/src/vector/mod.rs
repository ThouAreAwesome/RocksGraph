// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vector search module. v0.1 provides brute-force KNN via the volcano step;
//! v0.2 adds the `VectorIndex` trait and HNSW via usearch.

use parking_lot::RwLock;
use std::{collections::HashMap, sync::Arc};

use smol_str::SmolStr;

pub(crate) mod brute_force;
pub(crate) mod error;
pub(crate) mod hnsw;
pub(crate) mod persistence;
pub(crate) mod traits;
pub(crate) mod wal;

/// Pending vector mutation — tracked for RYOW isolation within a transaction.
#[derive(Debug, Clone)]
pub(crate) enum PendingVectorOp {
    /// An entity was inserted with this FloatVector value.
    Inserted { key: EntityKey, prop_name: SmolStr, vector: Vec<f32>, ts: u64 },
    /// An entity's FloatVector property was removed.
    Removed { key: EntityKey, prop_name: SmolStr, ts: u64 },
}

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

#[allow(unused_imports)]
pub(crate) use brute_force::{cosine_sim, BruteForceIndex, EntityKey};
#[allow(unused_imports)]
pub(crate) use error::{VectorEntityType, VectorError};
#[allow(unused_imports)]
pub(crate) use hnsw::load_vector_index;
#[allow(unused_imports)]
pub(crate) use traits::{
    AnnAlgorithm, DistanceMetric, HnswConfig, IndexOptions, PerIndexOptions, Quantization, VectorIndex,
    VectorIndexConfig, VectorIndexLimit,
};
