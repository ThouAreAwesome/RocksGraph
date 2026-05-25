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

use smallvec::SmallVec;

use crate::engine::{
    context::GraphCtx,
    traverser::Traverser,
    volcano::steps::traits::{CoreStep, StepRef},
};

pub struct VecSourceStep {
    items: SmallVec<[Rc<Traverser>; 4]>,
}

impl VecSourceStep {
    pub fn empty() -> Self {
        Self { items: SmallVec::new() }
    }

    pub fn inject(&mut self, items: SmallVec<[Rc<Traverser>; 4]>) {
        self.items.extend(items);
    }
}

impl CoreStep for VecSourceStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("VecSourceStep is a source step and cannot have an upstream");
    }

    fn produce(&mut self, _ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        if !self.items.is_empty() {
            Some(self.items.drain(..).collect())
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.items.clear();
    }
}
