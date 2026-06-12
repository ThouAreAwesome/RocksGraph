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
//! ## Architecture
//!
//! ```text
//! gremlin API  ──►  planner  ──►  optimizer  ──►  engine/volcano
//!                      │                                │
//!                 logical IR                      LogicalGraph       ← query-scoped overlay (OCC)
//!                                                      │
//!                                            GraphStore / RocksDB
//! ```
//!
//! | Module | Role |
//! |--------|------|
//! [`gremlin`]           | Fluent query builder; converts Gremlin API calls into a `LogicalPlan`. |
//! [`planner`]           | Translates a Gremlin AST into engine-agnostic [`LogicalPlan`] IR. |
//! [`planner::optimizer`]| Rewrites a `LogicalPlan` into a more efficient equivalent (fixpoint iteration). |
//! [`engine`]            | Execution engine (`volcano`) and shared primitives (`GraphCtx`, `Traverser`). |
//! [`graph`]             | Query-scoped in-memory overlay over a `GraphStore` transaction with OCC support. |
//! [`store`]             | Pluggable storage backend abstraction; RocksDB implementation. |
//! [`schema`]            | Label-ID ↔ label-string bidirectional mapping. |
//! [`types`]             | Shared value types (`GValue`, `Primitive`, keys). |
//!
//! [`LogicalPlan`]: planner::logical_step::LogicalPlan
#[doc(hidden)]
pub mod engine;
pub(crate) mod graph;
pub mod gremlin;
pub(crate) mod planner;
pub mod schema;
pub mod store;
pub mod types;

// ── User-facing re-exports ────────────────────────────────────────────────────
pub use engine::GraphCtx;
pub use gremlin::traversal::{graphTraversalSource, open_rocks_store, BuiltTraversal, GraphTraversal, __};
pub use types::{GValue, Primitive, StoreError}; // now users write `rocksgraph::GraphCtx`

/// Begin a new graph transaction, returning an opaque context that implements [`engine::GraphCtx`].
pub fn begin_graph<S: store::traits::GraphStore>(txn: S::Txn) -> impl engine::GraphCtx {
    graph::LogicalGraph::<S>::new(txn)
}
