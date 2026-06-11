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
        volcano::steps::{CoreStep, StepRef},
    },
    types::{GValue, StoreError, VertexKey},
};

/// A physical step that filters edges based on their secondary (end) vertex ID.
#[derive(Default, Debug)]
pub struct EndVertexFilter {
    upstream: Option<StepRef>,
    ids: SmallVec<[VertexKey; 4]>,
}

/// Creates a new `EndVertexFilter` with a list of target vertex IDs.
impl EndVertexFilter {
    pub fn new(ids: SmallVec<[VertexKey; 4]>) -> Self {
        Self { upstream: None, ids }
    }
}

impl CoreStep for EndVertexFilter {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces traversers whose edge's secondary vertex ID is present in the `ids` list.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if let GValue::Edge(edge) = &t.value {
                if self.ids.contains(&edge.secondary_id) {
                    return Ok(Some(smallvec![Rc::clone(&t)]));
                }
            } else {
                return Err(StoreError::UnexpectedDataType("end vertex filter can only be applied on Edge".into()));
            }
        }
    }

    fn reset(&mut self) {
        // Resets the state of this step and its upstream.
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }
}
