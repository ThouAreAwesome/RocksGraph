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
        volcano::{
            builder::PhysicalPlan,
            steps::traits::{CoreStep, StepRef},
        },
    },
    types::error::StoreError,
};

/// Physical step for `not(sub)`: passes the traverser if the sub-plan yields nothing.
#[derive(Debug)]
pub struct NotStep {
    upstream: Option<StepRef>,
    physical_plan: PhysicalPlan,
}

impl NotStep {
    pub fn new(physical_sub_plan: PhysicalPlan) -> Self {
        Self { upstream: None, physical_plan: physical_sub_plan }
    }
}

impl CoreStep for NotStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else {
                return Ok(None);
            };
            let Some(t) = upstream.next(ctx)? else {
                return Ok(None);
            };

            self.physical_plan.reset();
            let inner = Rc::clone(&t);
            self.physical_plan.inject(smallvec![inner]);

            // Pass if sub-plan yields NOTHING (negation of where)
            if self.physical_plan.next(ctx)?.is_none() {
                return Ok(Some(smallvec![t]));
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        self.physical_plan.reset();
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let children = vec![(String::new(), self.physical_plan.explain())];
        ExplainNode::new("NotStep").with_children(children)
    }
}
