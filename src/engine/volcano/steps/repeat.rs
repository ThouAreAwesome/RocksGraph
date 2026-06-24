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

use std::collections::VecDeque;
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

/// Controls when intermediate (non-final) traversers are emitted during looping.
#[derive(Debug)]
pub enum PhysicalEmitMode {
    Never,
    Always,
    If(PhysicalPlan),
}

/// A physical step that implements the `repeat` / `until` / `emit` looping construct.
///
/// Each incoming traverser has its body run repeatedly — breadth-first (FIFO frontier)
/// — until a stop condition (`times` or `until`) fires.  Intermediate results may be
/// emitted according to the `emit` policy.
#[derive(Debug)]
pub struct RepeatStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    body: PhysicalPlan,
    until: Option<PhysicalPlan>,
    times: Option<u32>,
    emit: PhysicalEmitMode,

    // ── Dynamic/Runtime execution state ──
    /// BFS frontier: (traverser, iterations_completed_so_far).
    frontier: VecDeque<(Rc<Traverser>, u32)>,
    /// Outputs queued for the next `produce()` call.
    ready: VecDeque<Rc<Traverser>>,
    /// Whether `body` is currently active (has been reset + injected and may yield more).
    body_active: bool,
    /// The iteration count of the traverser currently inside the body.
    current_iter_count: u32,
}

impl RepeatStep {
    pub fn new(
        body: PhysicalPlan,
        until: Option<PhysicalPlan>,
        times: Option<u32>,
        emit: PhysicalEmitMode,
    ) -> Self {
        Self {
            upstream: None,
            body,
            until,
            times,
            emit,
            frontier: VecDeque::new(),
            ready: VecDeque::new(),
            body_active: false,
            current_iter_count: 0,
        }
    }

    /// Returns true when a stop-condition is met for `out` at the given iteration count.
    fn is_done(&self, iter_count: u32, out: &Rc<Traverser>, ctx: &mut dyn GraphCtx) -> Result<bool, StoreError> {
        // 1. times bound reached.
        if let Some(times) = self.times {
            if iter_count >= times {
                return Ok(true);
            }
        }

        // 2. until sub-plan matches.
        if let Some(ref until_plan) = self.until {
            if sub_plan_matches(until_plan, out, ctx)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Returns true when the emit policy says this intermediate traverser should be output.
    fn should_emit(&self, out: &Rc<Traverser>, ctx: &mut dyn GraphCtx) -> Result<bool, StoreError> {
        match &self.emit {
            PhysicalEmitMode::Never => Ok(false),
            PhysicalEmitMode::Always => Ok(true),
            PhysicalEmitMode::If(plan) => sub_plan_matches(plan, out, ctx),
        }
    }
}

/// Helper: reset `plan`, inject `t`, then return whether `plan.next()` yields something.
fn sub_plan_matches(
    plan: &PhysicalPlan,
    t: &Rc<Traverser>,
    ctx: &mut dyn GraphCtx,
) -> Result<bool, StoreError> {
    plan.reset();
    plan.inject(smallvec![Rc::clone(t)]);
    Ok(plan.next(ctx)?.is_some())
}

impl CoreStep for RepeatStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            // ── Drain ready queue first ──
            if let Some(t) = self.ready.pop_front() {
                return Ok(Some(smallvec![t]));
            }

            // ── Body active: pull next result from the current iteration ──
            if self.body_active {
                match self.body.next(ctx)? {
                    Some(out) => {
                        let iter_count = self.current_iter_count + 1;
                        if self.is_done(iter_count, &out, ctx)? {
                            // Stop condition met — always emit (this is how the traverser exits).
                            self.ready.push_back(out);
                        } else {
                            if self.should_emit(&out, ctx)? {
                                // Intermediate emit — push a clone so the original can also be re-injected.
                                self.ready.push_back(Rc::clone(&out));
                            }
                            // Re-queue for the next iteration.
                            self.frontier.push_back((out, iter_count));
                        }
                        continue;
                    }
                    None => {
                        self.body_active = false;
                    }
                }
            }

            // ── Pull next traverser from the BFS frontier ──
            if let Some((t, count)) = self.frontier.pop_front() {
                self.current_iter_count = count;
                self.body.reset();
                self.body.inject(smallvec![t]);
                self.body_active = true;
                continue;
            }

            // ── Pull next traverser from upstream ──
            let Some(upstream) = self.upstream.as_ref() else {
                return Ok(None);
            };
            let Some(t) = upstream.next(ctx)? else {
                return Ok(None);
            };
            self.current_iter_count = 0;
            self.body.reset();
            self.body.inject(smallvec![t]);
            self.body_active = true;
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        self.body.reset();
        if let Some(ref until) = self.until {
            until.reset();
        }
        if let PhysicalEmitMode::If(ref plan) = self.emit {
            plan.reset();
        }
        self.frontier.clear();
        self.ready.clear();
        self.body_active = false;
        self.current_iter_count = 0;
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
