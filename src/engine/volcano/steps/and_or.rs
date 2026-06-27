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

use crate::types::{PIPELINE_PRODUCE_SIZE, SMALL_VECTOR_LENGTH};
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

/// Physical step for `and(subs...)`: passes if ALL sub-plans yield results.
#[derive(Debug)]
pub struct AndStep {
    upstream: Option<StepRef>,
    physical_plans: SmallVec<[PhysicalPlan; SMALL_VECTOR_LENGTH]>,
}

impl AndStep {
    pub fn new(physical_plans: SmallVec<[PhysicalPlan; SMALL_VECTOR_LENGTH]>) -> Self {
        Self { upstream: None, physical_plans }
    }
}

impl CoreStep for AndStep {
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

            let inner = Rc::clone(&t);
            let mut all_match = true;
            for plan in &self.physical_plans {
                plan.reset();
                plan.inject(smallvec![Rc::clone(&inner)]);
                if plan.next(ctx)?.is_none() {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                return Ok(Some(smallvec![t]));
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        for plan in &self.physical_plans {
            plan.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let children =
            self.physical_plans.iter().enumerate().map(|(i, plan)| (format!("sub {}", i), plan.explain())).collect();
        ExplainNode::new("AndStep").with_children(children)
    }
}

/// Physical step for `or(subs...)`: passes if ANY sub-plan yields results.
#[derive(Debug)]
pub struct OrStep {
    upstream: Option<StepRef>,
    physical_plans: SmallVec<[PhysicalPlan; SMALL_VECTOR_LENGTH]>,
}

impl OrStep {
    pub fn new(physical_plans: SmallVec<[PhysicalPlan; SMALL_VECTOR_LENGTH]>) -> Self {
        Self { upstream: None, physical_plans }
    }
}

impl CoreStep for OrStep {
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

            let inner = Rc::clone(&t);
            for plan in &self.physical_plans {
                plan.reset();
                plan.inject(smallvec![Rc::clone(&inner)]);
                if plan.next(ctx)?.is_some() {
                    return Ok(Some(smallvec![t]));
                }
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        for plan in &self.physical_plans {
            plan.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let children =
            self.physical_plans.iter().enumerate().map(|(i, plan)| (format!("sub {}", i), plan.explain())).collect();
        ExplainNode::new("OrStep").with_children(children)
    }
}
