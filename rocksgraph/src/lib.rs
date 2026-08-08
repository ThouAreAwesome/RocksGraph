// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! RocksGraph — a Gremlin-inspired property graph database engine backed by RocksDB.
//!
//! ## Quick start
//!
//! ```
//! use rocksgraph::{Graph, TraversalBuilder, Value};
//!
//! # let dir = tempfile::tempdir().unwrap();
//! # let graph = Graph::open(dir.path()).unwrap();
//!
//! // Read-write transaction
//! let mut txn = graph.begin();
//! txn.g().addV("person").property("id", 1).property("name", "alice").next().unwrap();
//! txn.g().addV("person").property("id", 2).property("name", "bob").next().unwrap();
//! txn.g().addE("knows").from(1).to(2).property("weight", 0.9f64).next().unwrap();
//! txn.commit().unwrap();
//!
//! // Read-only snapshot query
//! let mut snap = graph.read();
//! let count = snap.g().V([1]).out(["knows"]).count().next().unwrap().unwrap();
//! assert_eq!(count, Value::Int64(1));
//! let names = snap.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
//! assert_eq!(names, vec![Value::String("bob".into())]);
//! for v in snap.g().V([]).out(["knows"]).iter().unwrap() { println!("{:?}", v.unwrap()); }
//! # graph.close().unwrap();
//! ```
//!
//! ## Architecture
//!
//! ```text
//! Graph::open / graph.read() / graph.begin()          ← api (pub)
//!   │  session.g() → ReadTraversal / WriteTraversal
//!   │               step methods: self → Self (move semantics)
//!   │               terminals: .next()? / .to_list()? / .iter()?
//!   ▼
//! gremlin::traversal   fluent builder → LogicalPlan AST
//!   ▼
//! planner              AST → LogicalPlan IR + optimizer
//!   ▼
//! engine::volcano      pull-based Volcano iterator pipeline
//!   ▼
//! graph                query-scoped overlay (OCC dirty tracking)
//!   ▼
//! store / RocksDB      OptimisticTransactionDB
//! ```
//!
//! All modules below `api` are `pub(crate)` — users only interact through
//! [`Graph`], [`ReadSession`], [`TxnSession`], and the traversal types re-exported
//! at the crate root.
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
    traversal::{BuiltTraversal, ReadTraversal, TraversalBuilder, WriteTraversal, __},
    value::{between, eq, gt, gte, lt, lte, ne, within, without, Edge, Map, Path, Predicate, Property, Value, Vertex},
};

#[cfg(test)]
mod concurrency_tests;
