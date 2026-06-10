// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

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

#[derive(Debug)]
pub struct CoalesceStep {
    upstream: Option<StepRef>,
    physical_plans: SmallVec<[PhysicalPlan; 4]>,
    current_input: Option<Rc<Traverser>>,
    current_plan_idx: usize,
    winning_plan_idx: Option<usize>,
}

impl CoalesceStep {
    pub fn new(physical_plans: SmallVec<[PhysicalPlan; 4]>) -> Self {
        Self { upstream: None, physical_plans, current_input: None, current_plan_idx: 0, winning_plan_idx: None }
    }
}

impl CoreStep for CoalesceStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            // If we found a winning branch, keep draining it
            if let Some(winning_idx) = self.winning_plan_idx {
                if let Some(res) = self.physical_plans[winning_idx].next(ctx)? {
                    return Ok(Some(smallvec![res]));
                }
                self.current_input = None;
                self.winning_plan_idx = None;
            }

            // Fetch next input from upstream when current is exhausted
            if self.current_input.is_none() {
                let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
                let Some(t) = upstream.next(ctx)? else { return Ok(None) };
                self.current_input = Some(Rc::clone(&t));
                self.current_plan_idx = 0;
                if let Some(p) = self.physical_plans.first() {
                    p.reset();
                    p.inject(smallvec![Rc::clone(&t)]);
                }
            }

            // All branches exhausted for this input — move to next input traverser
            if self.current_plan_idx >= self.physical_plans.len() {
                self.current_input = None;
                continue;
            }

            // Try the current branch
            if let Some(res) = self.physical_plans[self.current_plan_idx].next(ctx)? {
                self.winning_plan_idx = Some(self.current_plan_idx);
                return Ok(Some(smallvec![res]));
            }

            // Branch yielded nothing — advance to next branch
            self.current_plan_idx += 1;
            if self.current_plan_idx < self.physical_plans.len() {
                let t = Rc::clone(self.current_input.as_ref().unwrap());
                self.physical_plans[self.current_plan_idx].reset();
                self.physical_plans[self.current_plan_idx].inject(smallvec![t]);
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        for p in &self.physical_plans {
            p.reset();
        }
        self.current_input = None;
        self.current_plan_idx = 0;
        self.winning_plan_idx = None;
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
