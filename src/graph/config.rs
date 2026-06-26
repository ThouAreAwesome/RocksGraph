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

use crate::types::LabelId;
use std::collections::HashSet;

// ── LogicalGraph structs ───────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanConfig {
    pub(crate) scan_vertices_batch_size: u32,
    pub(crate) scan_edges_batch_size: u32,
    pub(crate) get_adjacent_edges_batch_size: u32,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self { scan_vertices_batch_size: 1024, scan_edges_batch_size: 1024, get_adjacent_edges_batch_size: 64 }
    }
}

#[derive(Debug, Default)]
pub(crate) struct StagedSchema {
    pub(crate) staged_vertex_labels: HashSet<LabelId>,
    pub(crate) staged_edge_labels: HashSet<LabelId>,
    pub(crate) staged_prop_keys: HashSet<u16>,
}

impl StagedSchema {
    pub(crate) fn clear(&mut self) {
        self.staged_vertex_labels.clear();
        self.staged_edge_labels.clear();
        self.staged_prop_keys.clear();
    }
}
