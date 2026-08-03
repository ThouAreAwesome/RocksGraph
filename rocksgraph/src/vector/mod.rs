// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vector search module. v0.1 provides brute-force KNN via the volcano step;
//! v0.2 adds the `VectorIndex` trait and HNSW via usearch.

pub mod brute_force;
pub mod error;
pub mod traits;

pub use brute_force::{cosine_sim, BruteForceIndex, EntityKey};
pub use error::{VectorEntityType, VectorError};
pub use traits::{
    AnnAlgorithm, DistanceMetric, HnswConfig, IndexLimitOverride, Quantization,
    VectorIndex, VectorIndexConfig, VectorIndexLimit, VectorRuntimeOptions,
};
