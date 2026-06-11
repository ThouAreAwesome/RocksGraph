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
    types::{error::StoreError, Direction, EdgeKey, GValue, LabelId, VertexKey},
};

/// A physical step that retrieves specific edges based on labels, end vertices, and direction.
#[derive(Debug)]
pub struct GetEStep {
    upstream: Option<StepRef>,
    // label_ids should not be empty.
    label_ids: SmallVec<[LabelId; 4]>,
    // end_vertex_ids should not be empty.
    end_vertex_ids: SmallVec<[VertexKey; 4]>,
    direction: Direction,
}

impl GetEStep {
    /// Creates a new `GetEStep` to retrieve edges.
    pub fn new(
        label_ids: SmallVec<[LabelId; 4]>,
        end_vertex_ids: SmallVec<[VertexKey; 4]>,
        direction: Direction,
    ) -> Self {
        Self { upstream: None, label_ids, end_vertex_ids, direction }
    }
}

impl CoreStep for GetEStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this edge retrieval.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces traversers for edges that match the specified criteria.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };

            let src = match t.value {
                GValue::Vertex(src) => src,
                _ => return Err(StoreError::UnexpectedDataType("expected Vertex before outE".into())),
            };

            let mut results: SmallVec<[_; 4]> = smallvec![];

            for label_id in &self.label_ids {
                for dst in &self.end_vertex_ids {
                    let edge_key = match self.direction {
                        Direction::OUT => EdgeKey::out_e(src, *label_id, *dst, 0),
                        Direction::IN => EdgeKey::in_e(*dst, *label_id, src, 0),
                    };
                    if let Some(_e) = ctx.get_edge(&edge_key)? {
                        results.push(Traverser::new_rc_with_parent(GValue::Edge(edge_key), Rc::clone(&t)));
                    }
                }
            }
            if !results.is_empty() {
                return Ok(Some(results));
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
