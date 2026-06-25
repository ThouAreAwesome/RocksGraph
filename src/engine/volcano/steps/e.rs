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

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        keys::{CanonicalEdgeKey, EdgeKey},
        BatchScenario, GValue,
    },
};

/// A physical step that acts as a source, emitting traversers for specified edge keys or scanning all edges.
#[derive(Debug)]
pub struct EStep {
    // ── Static/Fixed configuration ──
    /// Specific edge keys to look up. If empty, scans all edges in the database.
    keys: SmallVec<[EdgeKey; PIPELINE_BATCH_INLINE]>,

    // ── Dynamic/Runtime execution state ──
    /// The index of the current key being processed in `keys` (only used when lookup keys are specified).
    current_idx: usize,
    /// Internal buffer caching the fetched edge keys in a batch.
    buffer: Vec<EdgeKey>,
    /// Index of the next edge key to yield from `buffer`.
    buffer_idx: usize,
    /// Cursor for database scan pagination.
    cursor: Option<CanonicalEdgeKey>,
    /// Tracks if database scan has started.
    scan_started: bool,
    /// Tracks if database scan has finished (no more edges to retrieve).
    scan_finished: bool,
}

/// Creates a new `EStep` with a list of edge keys to emit or scan.
impl EStep {
    pub fn new(keys: SmallVec<[EdgeKey; PIPELINE_BATCH_INLINE]>) -> Self {
        Self {
            keys,
            current_idx: 0,
            buffer: Vec::new(),
            buffer_idx: 0,
            cursor: None,
            scan_started: false,
            scan_finished: false,
        }
    }
}

impl CoreStep for EStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("EStep is a source step, it does not have an upstream.");
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        if !self.keys.is_empty() {
            if self.buffer.is_empty() && self.current_idx == 0 {
                let fetched = ctx.get_edges(&self.keys)?;
                self.buffer = fetched;
            }
            if self.buffer_idx < self.buffer.len() {
                let ek = self.buffer[self.buffer_idx];
                self.buffer_idx += 1;
                return Ok(Some(smallvec![Traverser::new_rc(GValue::Edge(ek))]));
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

                let limit = ctx.batch_size(BatchScenario::ScanEdges);
                let (ekeys, next_cursor) = ctx.scan_edges(None, self.cursor, limit)?;
                self.scan_started = true;

                if ekeys.is_empty() {
                    self.scan_finished = true;
                    return Ok(None);
                }

                self.buffer = ekeys;
                self.buffer_idx = 0;
                self.cursor = next_cursor;
            }

            let ek = self.buffer[self.buffer_idx];
            self.buffer_idx += 1;
            Ok(Some(smallvec![Traverser::new_rc(GValue::Edge(ek))]))
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
}
