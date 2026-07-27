// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

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
