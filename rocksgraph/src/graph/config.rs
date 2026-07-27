// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

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
