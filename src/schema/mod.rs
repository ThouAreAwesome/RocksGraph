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

pub(crate) mod definition;
pub(crate) mod management;

#[cfg(test)]
pub mod tests;

// Public surface: only what callers need to configure a `Graph` (`GraphOptions` and friends) and
// to declare schema via `SchemaManagement`. `Schema` itself (the live registry) and
// `PropKeyConfig` (one of its internal fields) are crate-internal — see `Graph::schema()`.
// `Cardinality` is also crate-internal: it has a single variant (`Single`) today, so there's
// nothing for `PropertyKeyMaker::cardinality()` to publicly expose yet — see docs/TODO.md.
pub use definition::{DataType, EdgeMode, GraphOptions, SchemaMode};
pub use management::{EdgeLabelMaker, PropertyKeyMaker, SchemaManagement, VertexLabelMaker};

pub(crate) use definition::Schema;
