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
    types::{error::StoreError, keys::VertexKey, GValue},
};

/// A physical step that acts as a source, emitting traversers for specified vertex IDs.
#[derive(Debug)]
pub struct VStep {
    vertex_ids: SmallVec<[VertexKey; 4]>,
    current_idx: usize,
}

/// Creates a new `VStep` with a list of vertex IDs to emit.
impl VStep {
    pub fn new(vertex_ids: SmallVec<[VertexKey; 4]>) -> Self {
        Self { vertex_ids, current_idx: 0 }
    }
}

impl CoreStep for VStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        // `VStep` is a source step and does not have an upstream.
        panic!("VStep is a source step, it does not have an upstream.");
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces traversers for each vertex ID in its list, checking for existence in the graph context.
        loop {
            if self.current_idx >= self.vertex_ids.len() {
                return Ok(None);
            }
            let id = self.vertex_ids[self.current_idx];
            self.current_idx += 1;
            if let Some(vk) = ctx.get_vertex(id)? {
                return Ok(Some(smallvec![Traverser::new_rc(GValue::Vertex(vk))]));
            }
        }
    }

    fn reset(&mut self) {
        // Resets the step's internal index, allowing it to re-emit its vertex IDs.
        self.current_idx = 0;
    }
}
