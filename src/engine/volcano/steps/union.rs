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

/// A physical step that implements the `union` logical step.
#[derive(Debug)]
pub struct UnionStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The physical plans representing the branches of the union.
    physical_plans: SmallVec<[PhysicalPlan; 4]>,

    // ── Dynamic/Runtime execution state ──
    /// The index of the current branch plan being evaluated for the active input.
    current_plan_idx: usize,
    /// The parent traverser currently being processed.
    current_input: Option<Rc<Traverser>>,
}

impl UnionStep {
    /// Creates a new `UnionStep` with the given physical sub-plans.
    pub fn new(physical_plans: SmallVec<[PhysicalPlan; 4]>) -> Self {
        Self { upstream: None, physical_plans, current_plan_idx: 0, current_input: None }
    }
}

impl CoreStep for UnionStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this union step.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Continuously attempts to produce traversers from its sub-plans.
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
        // Resets the state of this step and all its upstream and sub-plans.
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
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }
}
