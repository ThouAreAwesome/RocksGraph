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
pub struct UnionStep {
    upstream: Option<StepRef>,
    physical_plans: Vec<PhysicalPlan>,
    current_plan_idx: usize,
    current_input: Option<Rc<Traverser>>,
}

impl UnionStep {
    pub fn new(physical_plans: Vec<PhysicalPlan>) -> Self {
        Self { upstream: None, physical_plans, current_plan_idx: 0, current_input: None }
    }
}

impl CoreStep for UnionStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            if self.current_input.is_none() {
                let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
                let Some(t) = upstream.next(ctx)? else { return Ok(None) };
                self.current_input = Some(Rc::clone(&t));
                self.current_plan_idx = 0;
                if !self.physical_plans.is_empty() {
                    let p = &self.physical_plans[0];
                    p.reset();
                    p.inject(smallvec![Rc::clone(&t)]);
                }
            }

            if self.physical_plans.is_empty() {
                self.current_input = None;
                continue;
            }

            let p = &self.physical_plans[self.current_plan_idx];
            if let Some(res) = p.next(ctx)? {
                return Ok(Some(smallvec![res]));
            }

            self.current_plan_idx += 1;
            if self.current_plan_idx < self.physical_plans.len() {
                let next_p = &self.physical_plans[self.current_plan_idx];
                next_p.reset();
                next_p.inject(smallvec![Rc::clone(self.current_input.as_ref().unwrap())]);
            } else {
                self.current_input = None;
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
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
