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
        GValue,
    },
};

/// A physical step that acts as a source, emitting traversers for specified edge keys or scanning all edges.
#[derive(Debug)]
pub struct EStep {
    keys: SmallVec<[EdgeKey; 4]>,
    current_idx: usize,
    buffer: Vec<EdgeKey>,
    buffer_idx: usize,
    cursor: Option<CanonicalEdgeKey>,
    scan_started: bool,
    scan_finished: bool,
}

/// Creates a new `EStep` with a list of edge keys to emit or scan.
impl EStep {
    pub fn new(keys: SmallVec<[EdgeKey; 4]>) -> Self {
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

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
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

                let limit = 1000;
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
