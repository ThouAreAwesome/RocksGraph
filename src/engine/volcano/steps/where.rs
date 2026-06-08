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

use smallvec::smallvec;
use std::collections::VecDeque;

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

pub struct WhereStep {
    upstream: Option<StepRef>,
    physical_plans: PhysicalPlan,
}

impl WhereStep {
    pub fn new(physical_sub_plan: PhysicalPlan) -> Self {
        Self { upstream: None, physical_plans: physical_sub_plan }
    }
}

impl CoreStep for WhereStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx, buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(false) };
            let Some(t) = upstream.next(ctx)? else { return Ok(false) };

            let physical_sub_plan = &self.physical_plans;

            physical_sub_plan.reset();
            physical_sub_plan.inject(smallvec![Rc::clone(&t)]);

            // Sub pipeline evaluates properly — if sub-traversal yields at least one item, original goes through
            if physical_sub_plan.next(ctx)?.is_some() {
                buffer.push_back(t);
                return Ok(true);
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        self.physical_plans.reset();
    }
}
