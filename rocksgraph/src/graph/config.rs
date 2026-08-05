// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::LabelId;
use std::collections::HashSet;

// ── LogicalGraph structs ───────────────────────────────────────────────────────

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
