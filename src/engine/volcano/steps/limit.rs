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

use crate::engine::{
    context::GraphCtx,
    traverser::Traverser,
    volcano::steps::traits::{CoreStep, StepRef},
};

pub struct LimitStep {
    upstream: Option<StepRef>,
    limit: u32,
    current_idx: usize,
}

impl LimitStep {
    pub fn new(limit: u32) -> Self {
        Self { upstream: None, limit, current_idx: 0 }
    }
}

impl CoreStep for LimitStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        if self.current_idx >= self.limit as usize {
            return None;
        }
        let t = self.upstream.as_ref()?.next(ctx)?;
        self.current_idx += 1;
        Some(smallvec![Rc::clone(&t)])
    }

    fn reset(&mut self) {
        self.current_idx = 0;
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
