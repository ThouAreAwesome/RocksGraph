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
