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
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The direction of the edge traversal.
    direction: Direction,

    // ── Dynamic/Runtime execution state ──
    /// Pre-allocated template EdgeKeys populated with 0 as placeholder for src.
    /// Mutated in-place during execution to avoid allocations.
    keys_buffer: Vec<EdgeKey>,
}

impl GetEStep {
    /// Creates a new `GetEStep` to retrieve edges.
    pub fn new(
        label_ids: SmallVec<[LabelId; 4]>,
        end_vertex_ids: SmallVec<[VertexKey; 4]>,
        direction: Direction,
    ) -> Self {
        let mut keys_buffer = Vec::with_capacity(label_ids.len() * end_vertex_ids.len());
        for label_id in &label_ids {
            for dst in &end_vertex_ids {
                let edge_key = match direction {
                    Direction::OUT => EdgeKey::out_e(0, *label_id, *dst, 0),
                    Direction::IN => EdgeKey::in_e(*dst, *label_id, 0, 0),
                };
                keys_buffer.push(edge_key);
            }
        }
        Self { upstream: None, direction, keys_buffer }
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

            if self.keys_buffer.len() == 1 {
                let edge_key = &mut self.keys_buffer[0];
                match self.direction {
                    Direction::OUT => edge_key.primary_id = src,
                    Direction::IN => edge_key.secondary_id = src,
                }
                if ctx.get_edge(edge_key)?.is_some() {
                    results.push(Traverser::new_rc_with_parent(GValue::Edge(*edge_key), Rc::clone(&t)));
                }
            } else {
                for edge_key in &mut self.keys_buffer {
                    match self.direction {
                        Direction::OUT => edge_key.primary_id = src,
                        Direction::IN => edge_key.secondary_id = src,
                    }
                }
                let fetched = ctx.get_edges(&self.keys_buffer)?;
                for edge_key in fetched {
                    results.push(Traverser::new_rc_with_parent(GValue::Edge(edge_key), Rc::clone(&t)));
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
