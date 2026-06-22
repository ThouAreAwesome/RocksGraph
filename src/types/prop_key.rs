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

//! [`PropKey`] — the property key type — and the built-in reserved keys.
//!
//! Property keys are plain strings (e.g. `"name"`, `"age"`).  They are represented
//! as [`SmolStr`], which stack-allocates strings up to 22 bytes and avoids heap
//! allocation for the vast majority of real-world keys.
//!
//! # Built-in keys
//!
//! Two keys are reserved and synthesized on-the-fly by [`Vertex`](crate::types::Vertex)
//! and [`Edge`](crate::types::Edge) rather than stored in `props`:
//!
//! - [`ID`] (`"id"`) — the element's numeric identifier ([`VertexKey`](crate::types::VertexKey)).
//! - [`LABEL`] (`"label"`) — the element's label as its numeric [`LabelId`](crate::types::LabelId).
//!
//! Querying these keys via `get_property` / `get_value` always succeeds without a
//! `props` scan.

use smol_str::SmolStr;

/// Name of a property key.
///
/// Stack-allocated for strings up to 22 bytes; heap-allocated only for
/// unusually long key names.  No interning or numeric mapping — the raw
/// string is the identity.
pub type PropKey = SmolStr;
pub const ID: PropKey = SmolStr::new_static("id");
pub const LABEL: PropKey = SmolStr::new_static("label");
pub const RANK: PropKey = SmolStr::new_static("rank");
