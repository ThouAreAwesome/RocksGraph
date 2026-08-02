// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vector search module. v0.1 provides brute-force KNN via the volcano step;
//! v0.2 will add the `VectorIndex` trait and HNSW via usearch.

pub mod brute_force;

pub use brute_force::{cosine_sim, BruteForceIndex, EntityKey};
