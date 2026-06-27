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

use std::{collections::VecDeque, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::types::{PIPELINE_PRODUCE_SIZE, SMALL_VECTOR_LENGTH};
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::{
            builder::PhysicalPlan,
            steps::traits::{CoreStep, ExplainNode, StepRef},
        },
    },
    types::error::StoreError,
};

/// Executes a sub-traversal locally on each incoming traverser and emits every
/// result produced by the sub-traversal.  For example:
/// `.local(__().out("knows").values("name"))` emits each neighbour's name
/// for every input vertex — a flat-map over the sub-plan.
#[derive(Debug)]
pub struct LocalStep {
    upstream: Option<StepRef>,
    sub_plan: PhysicalPlan,
    /// Accumulates results from the current sub-plan invocation.
    buffer: VecDeque<Rc<Traverser>>,
    /// True once the upstream has been fully consumed and the buffer is drained.
    exhausted: bool,
}

impl LocalStep {
    pub fn new(sub_plan: PhysicalPlan) -> Self {
        Self { upstream: None, sub_plan, buffer: VecDeque::new(), exhausted: false }
    }
}

impl CoreStep for LocalStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        loop {
            // Drain buffered results first.
            if !self.buffer.is_empty() {
                let mut out = SmallVec::new();
                while let Some(item) = self.buffer.pop_front() {
                    out.push(item);
                    if out.len() >= SMALL_VECTOR_LENGTH {
                        return Ok(Some(out));
                    }
                }
                return Ok(Some(out));
            }

            if self.exhausted {
                return Ok(None);
            }

            // Pull next input traverser.
            let Some(upstream) = self.upstream.as_ref() else {
                return Ok(None);
            };
            let Some(t) = upstream.next(ctx)? else {
                self.exhausted = true;
                continue;
            };

            // Run sub-plan to completion on this traverser.
            self.sub_plan.reset();
            self.sub_plan.inject(smallvec![Rc::clone(&t)]);
            while let Some(result) = self.sub_plan.next(ctx)? {
                self.buffer.push_back(result);
            }
            // Loop back to drain buffer.
        }
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.exhausted = false;
        self.sub_plan.reset();
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("LocalStep").with_children(vec![(String::new(), self.sub_plan.explain())])
    }
}
