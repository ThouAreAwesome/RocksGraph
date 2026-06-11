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
    types::{error::StoreError, Direction, GValue},
};

/// A physical step that extracts either the incoming (`inV`) or outgoing (`outV`) vertex from an edge traverser.
#[derive(Debug)]
pub struct InVOutVStep {
    upstream: Option<StepRef>,
    direction: Direction,
}

/// Creates a new `InVOutVStep` for extracting a vertex from an edge based on the specified direction.
impl InVOutVStep {
    pub fn new(direction: Direction) -> Self {
        Self { upstream: None, direction }
    }
}

impl CoreStep for InVOutVStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this vertex extraction.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces a traverser carrying the extracted vertex (either source or destination) from an edge.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if let GValue::Edge(ek) = &t.value {
                let cek = ek.canonical_edge_key();
                if self.direction == Direction::OUT {
                    return Ok(Some(smallvec![Traverser::new_rc_with_parent(
                        GValue::Vertex(cek.src_id),
                        Rc::clone(&t)
                    )]));
                } else {
                    return Ok(Some(smallvec![Traverser::new_rc_with_parent(
                        GValue::Vertex(cek.dst_id),
                        Rc::clone(&t)
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
}
