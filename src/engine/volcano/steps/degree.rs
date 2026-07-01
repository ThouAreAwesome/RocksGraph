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

use crate::types::PIPELINE_PRODUCE_SIZE;
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
        gvalue::{GValue, Primitive},
        DegreeDirection,
    },
};

/// Physical streaming map that emits `GValue::Scalar(Int64(degree))` for each upstream vertex.
///
/// Produced only by the `degree_pushdown` optimizer — never by the traversal builder.
/// Every `produce()` call is O(1): a single overlay-HashMap lookup (or one CF point read
/// on a cold overlay).
#[derive(Debug)]
pub struct DegreeStep {
    upstream: Option<StepRef>,
    direction: DegreeDirection,
    /// Whether to thread the upstream traverser as parent so `path()` can
    /// reconstruct `[Vertex, Int64(degree)]`. Set by the builder when a
    /// `path()` step is present anywhere in the physical plan.
    track_path: bool,
}

impl DegreeStep {
    pub fn new(direction: DegreeDirection, track_path: bool) -> Self {
        Self { upstream: None, direction, track_path }
    }
}

impl CoreStep for DegreeStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let Some(t) = upstream.next(ctx)? else { return Ok(None) };

        match &t.value {
            GValue::Vertex(vk) => {
                let degree = ctx.get_degree(*vk, self.direction)?;
                Ok(Some(smallvec![Traverser::new_rc_conditional(
                    GValue::Scalar(Primitive::Int64(degree as i64)),
                    &t,
                    self.track_path,
                )]))
            }
            other => {
                Err(StoreError::UnexpectedDataType(format!("degree() expects a Vertex traverser, got {:?}", other)))
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("direction", format!("{:?}", self.direction))];
        ExplainNode::new("DegreeStep").with_params(params)
    }
}
