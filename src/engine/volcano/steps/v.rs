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
    types::{error::StoreError, keys::VertexKey, GValue},
};

#[derive(Debug)]
pub struct VStep {
    vertex_ids: Vec<VertexKey>,
    current_idx: usize,
}

impl VStep {
    pub fn new(vertex_ids: Vec<VertexKey>) -> Self {
        Self { vertex_ids, current_idx: 0 }
    }
}

impl CoreStep for VStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("VStep is a source step, it does not have an upstream.");
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            if self.current_idx >= self.vertex_ids.len() {
                return Ok(None);
            }
            let id = self.vertex_ids[self.current_idx];
            self.current_idx += 1;
            if let Some(vk) = ctx.get_vertex(id)? {
                return Ok(Some(smallvec![Traverser::new_rc(GValue::Vertex(vk))]));
            }
        }
    }

    fn reset(&mut self) {
        self.current_idx = 0;
    }
}
