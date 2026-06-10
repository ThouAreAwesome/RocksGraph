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
    types::{error::StoreError, Direction, GValue},
};

#[derive(Debug)]
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

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            if let GValue::Edge(ek) = &t.value {
                let cek = ek.canonical_edge_key();
                if self.direction == Direction::OUT {
                    return Ok(Some(smallvec![Traverser::new_rc_with_parent(
                        GValue::Vertex(cek.src_id),
                        Rc::clone(&t)
                    )]));
                } else {
                    return Ok(Some(smallvec![Traverser::new_rc_with_parent(
                        GValue::Vertex(cek.dst_id),
                        Rc::clone(&t)
                    )]));
                }
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
