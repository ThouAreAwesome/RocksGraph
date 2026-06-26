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

use crate::types::PIPELINE_BATCH_INLINE;
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, keys::VertexKey, BatchScenario, GValue},
};

/// A physical step that acts as a source, emitting traversers for specified vertex IDs or scanning all vertices.
#[derive(Debug)]
pub struct VStep {
    // ── Static/Fixed configuration ──
    /// Specific vertex keys to look up. If empty, scans all vertices in the database.
    vertex_ids: SmallVec<[VertexKey; PIPELINE_BATCH_INLINE]>,

    // ── Dynamic/Runtime execution state ──
    /// The index of the current key being processed in `vertex_ids` (only used when lookup IDs are specified).
    current_idx: usize,
    /// Internal buffer caching the fetched vertex keys in a batch.
    buffer: Vec<VertexKey>,
    /// Index of the next vertex key to yield from `buffer`.
    buffer_idx: usize,
    /// Cursor for database scan pagination.
    cursor: Option<VertexKey>,
    /// Tracks if database scan has started.
    scan_started: bool,
    /// Tracks if database scan has finished (no more vertices to retrieve).
    scan_finished: bool,
}

/// Creates a new `VStep` with a list of vertex IDs to emit or scan.
impl VStep {
    pub fn new(vertex_ids: SmallVec<[VertexKey; PIPELINE_BATCH_INLINE]>) -> Self {
        Self {
            vertex_ids,
            current_idx: 0,
            buffer: Vec::new(),
            buffer_idx: 0,
            cursor: None,
            scan_started: false,
            scan_finished: false,
        }
    }
}

impl CoreStep for VStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("VStep is a source step, it does not have an upstream.");
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        if !self.vertex_ids.is_empty() {
            if self.buffer.is_empty() && self.current_idx == 0 {
                let fetched = ctx.get_vertices(&self.vertex_ids)?;
                self.buffer = fetched;
            }
            if self.buffer_idx < self.buffer.len() {
                let vk = self.buffer[self.buffer_idx];
                self.buffer_idx += 1;
                return Ok(Some(smallvec![Traverser::new_rc(GValue::Vertex(vk))]));
            }
            Ok(None)
        } else {
            if self.scan_finished {
                return Ok(None);
            }
            if self.buffer_idx >= self.buffer.len() {
                if self.scan_started && self.cursor.is_none() {
                    self.scan_finished = true;
                    return Ok(None);
                }

                let limit = ctx.batch_size(BatchScenario::ScanVertices);
                let (vids, next_cursor) = ctx.scan_vertices(None, self.cursor, limit)?;
                self.scan_started = true;

                if vids.is_empty() {
                    self.scan_finished = true;
                    return Ok(None);
                }

                self.buffer = vids;
                self.buffer_idx = 0;
                self.cursor = next_cursor;
            }

            let vk = self.buffer[self.buffer_idx];
            self.buffer_idx += 1;
            Ok(Some(smallvec![Traverser::new_rc(GValue::Vertex(vk))]))
        }
    }

    fn reset(&mut self) {
        self.current_idx = 0;
        self.buffer.clear();
        self.buffer_idx = 0;
        self.cursor = None;
        self.scan_started = false;
        self.scan_finished = false;
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("ids", format!("{:?}", self.vertex_ids))];
        ExplainNode::new("VStep").with_params(params)
    }
}
