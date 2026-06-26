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

//! Query-scoped logical graph — the ground truth for a single traversal.
//! See [`LogicalGraph`] and [`LogicalSnapshot`] for details.

mod config;
mod existence;
mod helpers;
mod logical;
mod snapshot;
#[cfg(test)]
mod tests;

pub(crate) use config::{ScanConfig, StagedSchema};
pub(crate) use existence::Existence;
pub(crate) use logical::LogicalGraph;
pub(crate) use snapshot::LogicalSnapshot;
