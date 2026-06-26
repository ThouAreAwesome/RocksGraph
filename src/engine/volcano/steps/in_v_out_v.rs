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
    types::{error::StoreError, Direction, GValue},
};

/// A physical step that extracts either the incoming (`inV`) or outgoing (`outV`) vertex from an edge traverser.
#[derive(Debug)]
pub struct InVOutVStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The direction of the vertex to extract (incoming or outgoing).
    direction: Direction,
    /// Whether to link the parent chain on emitted traversers (`false` skips the `Rc::clone`
    /// when the plan has no `as()`/`select()`/`path()` anywhere in it).
    track_path: bool,
}

/// Creates a new `InVOutVStep` for extracting a vertex from an edge based on the specified direction.
impl InVOutVStep {
    pub fn new(direction: Direction, track_path: bool) -> Self {
        Self { upstream: None, direction, track_path }
    }
}

impl CoreStep for InVOutVStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this vertex extraction.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        // Produces a traverser carrying the extracted vertex (either source or destination) from an edge.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if let GValue::Edge(ek) = &t.value {
                let cek = ek.canonical_edge_key();
                if self.direction == Direction::OUT {
                    return Ok(Some(smallvec![Traverser::new_rc_conditional(
                        GValue::Vertex(cek.src_id),
                        &t,
                        self.track_path
                    )]));
                } else {
                    return Ok(Some(smallvec![Traverser::new_rc_conditional(
                        GValue::Vertex(cek.dst_id),
                        &t,
                        self.track_path
                    )]));
                }
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

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("InVOutVStep").with_params(vec![("direction", format!("{:?}", self.direction))])
    }
}
