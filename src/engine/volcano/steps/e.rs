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

use crate::types::{PIPELINE_BATCH_INLINE, PIPELINE_PRODUCE_INLINE};
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        keys::{CanonicalEdgeKey, Direction, EdgeKey},
        BatchScenario, GValue,
    },
};

/// A physical source step: emits edges for specified id strings or scans all edges.
#[derive(Debug)]
pub struct EStep {
    /// Canonical id strings to look up.  Empty = scan all edges.
    keys: SmallVec<[String; PIPELINE_BATCH_INLINE]>,
    /// Index into `keys` for the next string to resolve.
    current_idx: usize,
    /// Buffer of fetched edge keys.
    buffer: Vec<EdgeKey>,
    /// Index of the next edge key to yield from `buffer`.
    buffer_idx: usize,
    /// Cursor for scan pagination.
    cursor: Option<CanonicalEdgeKey>,
    scan_started: bool,
    scan_finished: bool,
}

impl EStep {
    pub fn new(keys: SmallVec<[String; PIPELINE_BATCH_INLINE]>) -> Self {
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
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError> {
        if !self.keys.is_empty() {
            // Look up by id string — resolve one at a time, skip malformed.
            while self.current_idx < self.keys.len() {
                let key_str = &self.keys[self.current_idx];
                self.current_idx += 1;
                if let Ok(cek) = key_str.parse::<CanonicalEdgeKey>() {
                    let ek = EdgeKey {
                        primary_id: cek.src_id,
                        direction: Direction::OUT,
                        label_id: cek.label_id,
                        secondary_id: cek.dst_id,
                        rank: cek.rank,
                    };
                    let edges = ctx.get_edges(&[ek])?;
                    if !edges.is_empty() {
                        return Ok(Some(edges.into_iter().map(|e| Traverser::new_rc(GValue::Edge(e))).collect()));
                    }
                }
                // malformed → skip
            }
            Ok(None)
        } else {
            // Scan all edges.
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

    fn explain(&self) -> ExplainNode {
        let params = vec![("keys", format!("{:?}", self.keys))];
        ExplainNode::new("EStep").with_params(params)
    }
}
