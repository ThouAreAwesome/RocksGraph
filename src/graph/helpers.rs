// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::{
    element::Edge,
    keys::{Direction, LabelId, VertexKey},
};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Evaluates whether an edge matches the specified traversal filters.
///
/// This function verifies that the edge's primary endpoint matches `vertex` in the given `direction`,
/// and optionally applies filters for `label` and the secondary endpoint (`dst`).
pub(crate) fn edge_matches(
    view: &Edge,
    vertex: VertexKey,
    direction: Direction,
    label: Option<LabelId>,
    dst: Option<&[VertexKey]>,
) -> bool {
    let primary = match direction {
        Direction::OUT => view.src_id,
        Direction::IN => view.dst_id,
    };
    if primary != vertex {
        return false;
    }
    if let Some(lbl) = label {
        if view.label_id != lbl {
            return false;
        }
    }
    if let Some(slice) = dst {
        let remote = match direction {
            Direction::OUT => view.dst_id,
            Direction::IN => view.src_id,
        };
        if !slice.contains(&remote) {
            return false;
        }
    }
    true
}
