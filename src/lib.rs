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

//! RocksGraph — a Gremlin-compatible graph database engine.
//!
//! ## Quick start
//!
//! ```ignore
//! use rocksgraph::{Graph, TraversalBuilder, GValue, Primitive, StoreError, __};
//!
//! let graph = Graph::open("/path/to/db")?;
//!
//! // Read-only snapshot query — three ways to consume results
//! let mut snap = graph.read();
//! let count = snap.g().V([1]).out([KNOWS]).count().next()?.unwrap(); // first result
//! let names = snap.g().V([1]).out([KNOWS]).values(["name"]).to_list()?; // Vec<GValue>
//! for v in snap.g().V([]).out([KNOWS]).iter()? { println!("{:?}", v?); } // lazy
//!
//! // Read-write transaction
//! let mut tx = graph.begin();
//! tx.g().addV(PERSON).property("name", "alice").next()?;
//! tx.g().addE(KNOWS).from(1).to(2).property("weight", 0.9f64).next()?;
//! tx.commit()?;
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
pub mod api;
pub(crate) mod engine;
pub(crate) mod graph;
pub(crate) mod gremlin;
pub(crate) mod planner;
pub mod schema;
pub(crate) mod store;
pub mod types;

// ── User-facing re-exports ────────────────────────────────────────────────────
pub use api::{Graph, ReadSession, TxSession};
// GraphTraversal is doc-hidden but must be pub so users can pass `__()` values
// to where/coalesce/union without naming the type.
#[doc(hidden)]
pub use gremlin::traversal::GraphTraversal;
pub use gremlin::traversal::{BuiltTraversal, ReadTraversal, TraversalBuilder, WriteTraversal, __};
pub use types::{GValue, Primitive, StoreError};
