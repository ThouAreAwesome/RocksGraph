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

pub(super) mod admin;
mod snapshot;
mod store;
mod transaction;

pub use store::{RocksOptions, RocksStorage};

pub(crate) const CF_VERTICES: &str = "vertices";
pub(crate) const CF_VERTEX_DEGREE: &str = "vertex_degree";
pub(crate) const CF_EDGES_OUT: &str = "edges_out";
pub(crate) const CF_EDGES_IN: &str = "edges_in";
pub(crate) const CF_SCHEMA: &str = "schema";
