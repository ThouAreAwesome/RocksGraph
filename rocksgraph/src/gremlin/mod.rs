// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! User-facing Gremlin-idiom types: traversal builder, value types, and the
//! property-graph element models (`Vertex`, `Edge`, `Property`).
//!
//! The traversal module provides a fluent builder that accumulates step
//! descriptors; built traversers are handed to the planner for compilation.
pub(crate) mod multi_edge_tests;
pub(crate) mod tests;
pub mod traversal;
pub(crate) mod type_bridge;
pub mod value;
