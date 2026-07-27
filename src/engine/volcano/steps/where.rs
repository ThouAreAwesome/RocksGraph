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

/// A physical step that filters incoming traversers based on the results of a sub-plan.
#[derive(Debug)]
pub struct WhereStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The physical sub-plan representing the filter traversal condition.
    physical_plans: PhysicalPlan,
}

/// Creates a new `WhereStep` with the given physical sub-plan.
impl WhereStep {
    pub fn new(physical_sub_plan: PhysicalPlan) -> Self {
        Self { upstream: None, physical_plans: physical_sub_plan }
    }
}

impl CoreStep for WhereStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        // Produces traversers from its upstream if the sub-plan yields any results for that traverser.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };

            let physical_sub_plan = &self.physical_plans;

            physical_sub_plan.reset();
            physical_sub_plan.inject(smallvec![Rc::clone(&t)]);

            // Sub pipeline evaluates properly — if sub-traversal yields at least one item, original goes through
            if physical_sub_plan.next(ctx)?.is_some() {
                return Ok(Some(smallvec![t]));
            }
        }
    }

    fn reset(&mut self) {
        // Resets the state of this step, its upstream, and its sub-plan.
        if let Some(up) = &self.upstream {
            up.reset();
        }
        self.physical_plans.reset();
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let children = vec![(String::new(), self.physical_plans.explain())];
        ExplainNode::new("WhereStep").with_children(children)
    }
}
