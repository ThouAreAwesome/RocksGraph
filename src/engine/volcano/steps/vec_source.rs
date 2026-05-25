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

use std::{collections::VecDeque, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::engine::{
    context::GraphCtx,
    traverser::Traverser,
    volcano::steps::traits::{CoreStep, StepRef},
};

pub struct VecSourceStep {
    items: VecDeque<Rc<Traverser>>,
    items_backup: VecDeque<Rc<Traverser>>,
}

impl VecSourceStep {
    pub fn empty() -> Self {
        Self { items: VecDeque::new(), items_backup: VecDeque::new() }
    }

    pub fn inject(&mut self, items: VecDeque<Rc<Traverser>>) {
        self.items = items.clone();
        self.items_backup = items;
    }
}

impl CoreStep for VecSourceStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("VecSourceStep is a source step and cannot have an upstream");
    }

    fn produce(&mut self, _ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        let item = self.items.pop_front()?;
        Some(smallvec![item])
    }

    fn reset(&mut self) {
        self.items = self.items_backup.clone();
    }
}
