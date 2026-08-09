// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! RocksGraph — an embeddable, ACID-compliant property graph database with
//! Gremlin traversals and integrated HNSW vector search.
//!
//! ## Quick start
//!
//! ```
//! use rocksgraph::{Graph, Value};
//!
//! # let dir = tempfile::tempdir().unwrap();
//! # let graph = Graph::open(dir.path()).unwrap();
//!
//! // Write in an ACID transaction
//! let mut txn = graph.begin();
//! txn.g().addV("person").property("id", 1i64).property("name", "alice")
//!     .property("emb", Value::FloatVector(vec![0.9, 0.1, 0.0]))
//!     .next().unwrap();
//! txn.g().addV("person").property("id", 2i64).property("name", "bob")
//!     .property("emb", Value::FloatVector(vec![0.1, 0.9, 0.0]))
//!     .next().unwrap();
//! txn.g().addE("knows").from(1i64).to(2i64).property("weight", 0.9f64).next().unwrap();
//! txn.commit().unwrap();
//!
//! // Read from a point-in-time snapshot
//! let mut snap = graph.read();
//! let friends = snap.g().V([1i64]).out(["knows"]).values(["name"]).to_list().unwrap();
//! assert_eq!(friends, vec![Value::String("bob".into())]);
//!
//! // Vector search: find nearest vertex to a query embedding
//! let nearest = snap.g().V([]).nearest("emb", vec![1.0f32, 0.0, 0.0], 1)
//!     .values(["name"]).to_list().unwrap();
//! # graph.close().unwrap();
//! ```
//!
//! ## Guides
//!
//! - [Getting Started](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/guides/getting_started.md)
//! - [Vector Search](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/guides/vector_search.md)
//! - [Gremlin Step Reference](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/guides/step_reference.md)
//! - [Schema Management](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/guides/schema_management.md)
//! - [Transactions & Concurrency](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/guides/concurrency_and_tx.md)
//! - [Bulk Loading](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/guides/bulk_loading.md)
//!
//! ## Architecture
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`api`] | [`Graph`], [`ReadSession`], [`TxnSession`], [`IndexManager`] |
//! | `vector` | HNSW index, BruteForce fallback, WAL, traits |
//! | [`schema`] | Schema modes, property types, [`VectorIndexConfig`] |
//! | `gremlin` | Traversal builder, step types, [`Value`]/[`Vertex`]/[`Edge`] |
//! | `store` | RocksDB column families, transactions, snapshots |
//! | `engine` | Volcano physical operators, traverser, context |
//! | `planner` | Logical plan optimization, filter reordering |
//! | [`bulk`] | High-throughput [`BulkLoader`] for offline SST ingestion |
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod api;
pub mod bulk;
#[doc(hidden)]
pub(crate) mod bytecode;
pub(crate) mod engine;
pub(crate) mod graph;
pub(crate) mod gremlin;
pub(crate) mod planner;
pub mod schema;
pub(crate) mod store;
pub(crate) mod types;
/// Vector ANN search (v0.1: FloatVector type + brute-force KNN; v0.2: HNSW via usearch).
pub(crate) mod vector;

// ── User-facing re-exports ────────────────────────────────────────────────────
pub use api::{Graph, IndexManager, ReadSession, TxnSession};
pub use bulk::{BulkEdge, BulkLoadStats, BulkLoader, BulkSchema, BulkVertex, IntoBulkEdge, IntoBulkVertex};
pub use engine::ExecutionOptions;
pub use planner::logical_step::Order;
pub use schema::{
    AnnAlgorithm, DataType, DistanceMetric, EdgeMode, GraphOptions, HnswConfig, IndexOptions, Quantization, SchemaMode,
    VectorEntityType, VectorIndexConfig,
};
pub use smol_str::SmolStr;
pub use store::RocksOptions;
pub use types::{DegreeDirection, Direction, Primitive, StoreError};
// GraphTraversal is doc-hidden but must be pub so users can pass `__()` values
// to where/coalesce/union without naming the type.
#[doc(hidden)]
pub use gremlin::traversal::GraphTraversal;
pub use gremlin::{
    traversal::{BuiltTraversal, ByModulator, ByTarget, IntoBy, ReadTraversal, TraversalBuilder, WriteTraversal, __},
    value::{between, eq, gt, gte, lt, lte, ne, within, without, Edge, Map, Path, Predicate, Property, Value, Vertex},
};

#[cfg(test)]
mod concurrency_tests;
