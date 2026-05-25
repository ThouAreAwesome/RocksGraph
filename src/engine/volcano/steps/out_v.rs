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
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::GValue,
};

pub struct OutVStep {
    upstream: Option<StepRef>,
}

impl OutVStep {
    pub fn new() -> Self {
        Self { upstream: None }
    }
}

impl CoreStep for OutVStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        loop {
            let t = self.upstream.as_ref()?.next(ctx)?;
            if let GValue::Edge(ek) = &t.value {
                let vk = ek.canonical_edge_key().src_id;
                return Some(smallvec![Traverser::new_rc_with_parent(GValue::Vertex(vk), Rc::clone(&t))]);
            }
            // TODO: consider returning an error here instead of silently skipping non-edge traversers
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
