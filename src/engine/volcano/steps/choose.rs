// Physical step: choose()

use crate::types::PIPELINE_BATCH_INLINE;
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
use smallvec::{smallvec, SmallVec};
use std::rc::Rc;

/// Conditional branching: if predicate matches, inject into true_choice;
/// otherwise inject into false_choice (or pass through).
#[derive(Debug)]
pub struct ChooseStep {
    upstream: Option<StepRef>,
    predicate: PhysicalPlan,
    true_choice: PhysicalPlan,
    false_choice: Option<PhysicalPlan>,
    active_plan: Option<PhysicalPlan>, // currently running branch
}

impl ChooseStep {
    pub fn new(predicate: PhysicalPlan, true_choice: PhysicalPlan, false_choice: Option<PhysicalPlan>) -> Self {
        Self { upstream: None, predicate, true_choice, false_choice, active_plan: None }
    }
}

impl CoreStep for ChooseStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        if let Some(u) = &self.upstream {
            u.reset();
        }
        self.predicate.reset();
        self.true_choice.reset();
        if let Some(ref fc) = self.false_choice {
            fc.reset();
        }
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        loop {
            // Drain active sub-plan first
            if let Some(ref plan) = self.active_plan {
                match plan.next(ctx)? {
                    Some(t) => return Ok(Some(smallvec![t])),
                    None => self.active_plan = None,
                }
            }

            // Pull next traverser from upstream
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };

            // Evaluate predicate
            self.predicate.reset();
            self.predicate.inject(smallvec![Rc::clone(&t)]);
            let matches = self.predicate.next(ctx)?.is_some();

            let branch = if matches {
                &self.true_choice
            } else if let Some(ref fc) = self.false_choice {
                fc
            } else {
                // No false_choice — pass through the original traverser
                return Ok(Some(smallvec![t]));
            };

            branch.reset();
            branch.inject(smallvec![Rc::clone(&t)]);
            self.active_plan = Some(branch.clone());
            // Continue loop to drain from active_plan
        }
    }
}
