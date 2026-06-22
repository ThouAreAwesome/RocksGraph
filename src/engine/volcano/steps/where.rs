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

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
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
}
