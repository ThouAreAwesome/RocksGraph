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

//! Fundamental domain types and the core data model for RocksGraph.
//!
//! This module provides the essential building blocks used throughout the engine
//! and storage layers.  Understanding how these types relate to each other is the
//! key to reading most of the codebase.
//!
//! # Type hierarchy
//!
//! ```text
//! GValue                          ← universal value flowing through a traversal
//! ├── Vertex(VertexKey)           ← i64 handle; actual data fetched on demand
//! ├── Edge(EdgeKey)               ← directed handle (primary_id + direction + …)
//! ├── Property(Property)          ← owner + PropKey + Primitive
//! ├── Scalar(Primitive)           ← bool / i32 / i64 / f32 / f64 / String / Uuid / Null
//! ├── List(Vec<GValue>)
//! ├── Map(HashMap<GValue, GValue>)
//! └── Path(Vec<(GValue, labels)>)
//!
//! Keys
//! ├── VertexKey = i64
//! ├── CanonicalEdgeKey            ← direction-free (src, label, rank, dst)
//! └── EdgeKey                     ← directed (primary_id, direction, label, rank, secondary_id)
//!
//! Identifiers
//! ├── LabelId = u16               ← numeric label id (schema registry maps Label ↔ LabelId)
//! ├── Label(SmolStr)              ← human-readable label string ("person", "knows")
//! └── PropKey = SmolStr           ← property key string ("name", "age")
//! ```
//!
//! # Lazy element loading
//!
//! `GValue::Vertex` and `GValue::Edge` carry **keys only** — they are `Copy` and cheap
//! to clone (8 / 40 bytes).  The traversal engine calls `ctx.get_vertex(key)` or
//! `ctx.get_edges(…)` to materialize the full [`Vertex`] / [`Edge`] record (with
//! properties) only when needed.  This avoids unnecessary data fetches in filter-heavy
//! traversals.
//!
//! # Module layout
//!
//! | Sub-module | Contents |
//! |---|---|
//! | [`element`] | [`Vertex`], [`Edge`], [`Property`] — graph element records |
//! | [`gvalue`] | [`GValue`], [`Primitive`] — traversal value types |
//! | [`keys`] | [`VertexKey`], [`EdgeKey`], [`CanonicalEdgeKey`], [`Direction`], [`CanonicalKey`] |
//! | [`label`] | [`Label`] — human-readable label string |
//! | [`prop_key`] | [`PropKey`], [`ID`](prop_key::ID), [`LABEL`](prop_key::LABEL) — property key type and built-in keys |
//! | [`error`] | [`StoreError`] — storage and runtime errors |
//!
//! Most types are re-exported at the crate root for convenience.

pub mod element;
pub mod error;
pub mod gvalue;
pub mod keys;
pub mod label;
pub mod prop_key;

pub use element::{Edge, Property, Vertex};
pub use error::StoreError;
pub use gvalue::{GValue, Primitive};
pub use keys::{
    AdjacentEdgeCursor, AdjacentEdgesOptions, BatchScenario, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey,
    LabelId, Rank, VertexKey,
};
pub use prop_key::PropKey;
