// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

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
//! let mut tx = graph.begin();
//! tx.g().addV("person").property("id", 1).property("name", "alice").next().unwrap();
//! tx.g().addV("person").property("id", 2).property("name", "bob").next().unwrap();
//! tx.g().addE("knows").from(1).to(2).property("weight", 0.9f64).next().unwrap();
//! tx.commit().unwrap();
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
//! [`Graph`], [`ReadSession`], [`TxSession`], and the traversal types re-exported
//! at the crate root.
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod api;
pub(crate) mod engine;
pub(crate) mod graph;
pub(crate) mod gremlin;
pub(crate) mod planner;
pub mod schema;
pub(crate) mod store;
pub(crate) mod types;

// ── User-facing re-exports ────────────────────────────────────────────────────
pub use api::{Graph, ReadSession, TxSession};
pub use store::RocksOptions;
// GraphTraversal is doc-hidden but must be pub so users can pass `__()` values
// to where/coalesce/union without naming the type.
#[doc(hidden)]
pub use gremlin::traversal::GraphTraversal;
pub use gremlin::{
    traversal::{BuiltTraversal, ReadTraversal, TraversalBuilder, WriteTraversal, __},
    value::{between, eq, gt, gte, lt, lte, ne, within, without, Edge, Map, Path, Predicate, Property, Value, Vertex},
};
pub use types::StoreError;

#[cfg(test)]
mod concurrency_tests;
