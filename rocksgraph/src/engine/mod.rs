// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

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
pub mod options;
pub(crate) mod traverser;
pub(crate) mod volcano;

pub use options::ExecutionOptions;

// GraphCtx appears in GraphTraversal::build()'s impl-trait bound and must
// remain nameable outside the crate, but it is not part of the user-facing API.
pub(crate) use context::GraphCtx;
