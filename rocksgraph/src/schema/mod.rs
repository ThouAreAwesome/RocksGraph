// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Schema types, configurations, and DDL.
//!
//! [`SchemaSession`] manages atomic DDL: adding vertex/edge labels, declaring
//! property keys and types, and registering/dropping vector indexes via
//! [`add_vector_index`](SchemaSession::add_vector_index) /
//! [`drop_vector_index`](SchemaSession::drop_vector_index).
//!
//! ## Key types
//!
//! | Type | Purpose |
//! |------|---------|
//! | [`DataType`] | Property value types (Int32, String, FloatVector, etc.) |
//! | [`SchemaMode`] | Auto (infer on write) vs Strict (reject undeclared) |
//! | [`EdgeMode`] | Single (one edge per pair) vs Multi (parallel edges with rank) |
//! | [`VectorIndexConfig`] | HNSW index declaration (dimension, metric, quantization) |
//! | [`DistanceMetric`] | Cosine, DotProduct, or Euclidean |
//! | [`Quantization`] | F16 (half precision, default) or F32 (full precision) |
//! | [`AnnAlgorithm`] | HNSW (backed by usearch) or BruteForce (linear scan) |
//! | [`GraphOptions`] | Persisted schema + runtime [`IndexOptions`] |

pub(crate) mod definition;
pub(crate) mod management;

#[cfg(test)]
#[cfg(test)]
mod tests;

// Public surface: only what callers need to configure a `Graph` (`GraphOptions` and friends) and
// to declare schema via `SchemaSession`. `Schema` itself (the live registry) and
// `PropKeyConfig` (one of its internal fields) are crate-internal — see `Graph::schema()`.
pub use crate::engine::ExecutionOptions;
pub use crate::vector::error::VectorEntityType;
pub use crate::vector::traits::{
    AnnAlgorithm, DistanceMetric, HnswConfig, IndexOptions, PerIndexOptions, Quantization, VectorIndexConfig,
    VectorIndexLimit,
};
pub use definition::{DataType, EdgeMode, GraphOptions, SchemaMode};
pub use management::SchemaSession;

pub(crate) use definition::Schema;
