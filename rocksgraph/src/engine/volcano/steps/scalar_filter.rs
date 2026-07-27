// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::PIPELINE_PRODUCE_SIZE;
use std::rc::Rc;

use smallvec::SmallVec;

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue, PrimitivePredicate},
};

/// A physical step that filters traversers based on whether their scalar value matches a predicate.
#[derive(Debug)]
pub struct ScalarFilterStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The predicate to filter scalar values.
    pred: PrimitivePredicate,
}

/// Creates a new `ScalarFilterStep` with the predicate to filter by.
impl ScalarFilterStep {
    pub fn new(pred: PrimitivePredicate) -> Self {
        Self { upstream: None, pred }
    }
}

impl CoreStep for ScalarFilterStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        // Produces traversers whose `GValue::Scalar` matches the predicate.
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = SmallVec::with_capacity(PIPELINE_PRODUCE_SIZE);
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            let Some(t) = upstream.next(ctx)? else { break };
            if let GValue::Scalar(p) = &t.value {
                if self.pred.evaluate(p) {
                    batch.push(t);
                }
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
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
        let params = vec![("pred", format!("{:?}", self.pred))];
        ExplainNode::new("ScalarFilterStep").with_params(params)
    }
}
