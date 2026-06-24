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

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
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
}
