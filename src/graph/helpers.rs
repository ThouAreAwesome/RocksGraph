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
