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

use std::collections::VecDeque;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, Direction, GValue},
};

pub struct InVOutVStep {
    upstream: Option<StepRef>,
    direction: Direction,
}

impl InVOutVStep {
    pub fn new(direction: Direction) -> Self {
        Self { upstream: None, direction }
    }
}

impl CoreStep for InVOutVStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx, buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(false) };
            let Some(t) = upstream.next(ctx)? else { return Ok(false) };
            if let GValue::Edge(ek) = &t.value {
                let cek = ek.canonical_edge_key();
                if self.direction == Direction::OUT {
                    buffer.push_back(Traverser::new_rc_with_parent(GValue::Vertex(cek.src_id), t.clone()));
                } else {
                    buffer.push_back(Traverser::new_rc_with_parent(GValue::Vertex(cek.dst_id), t.clone()));
                }
                return Ok(true);
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
