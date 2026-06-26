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

//! Volcano-style pull-based query engine.
//!
//! The engine translates a logical plan into a directed acyclic graph of
//! physical steps. Each step pulls traversers from its upstream via a uniform
//! `next()` interface; results are buffered one at a time by `BufferedStep`.
//! Execution is single-threaded per query; multiple queries can run concurrently
//! against independent sessions.
//! Execution engine and shared runtime primitives.
//!
//! ## Submodules
//!
//! | Submodule    | Role |
//! |--------------|------|
//! | [`context`]  | [`GraphCtx`] trait — the graph-access interface passed to every step at runtime. Shared by both engines. |
//! | [`traverser`]| [`Traverser`] — the unit of work flowing between steps. |
//! | [`volcano`]  | Pull-based iterator execution engine. Logical steps are compiled to a chain of physical operators by [`volcano::builder::PhysicalPlanBuilder`]. |
//!
//! [`GraphCtx`]: context::GraphCtx
//! [`Traverser`]: traverser::Traverser
//! [`PhysicalPlanBuilder`]: volcano::builder::PhysicalPlanBuilder

pub(crate) mod context;
pub(crate) mod traverser;
pub(crate) mod volcano;

// GraphCtx appears in GraphTraversal::build()'s impl-trait bound and must
// remain nameable outside the crate, but it is not part of the user-facing API.
#[doc(hidden)]
pub use context::GraphCtx;
