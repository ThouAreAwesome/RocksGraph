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
        volcano::steps::{CoreStep, StepRef},
    },
    types::{GValue, StoreError, VertexKey},
};

#[derive(Default)]
pub struct EndVertexFilter {
    upstream: Option<StepRef>,
    ids: Vec<VertexKey>,
}

impl EndVertexFilter {
    pub fn new(ids: Vec<VertexKey>) -> Self {
        Self { upstream: None, ids }
    }
}

impl CoreStep for EndVertexFilter {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx, buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(false) };
            let Some(t) = upstream.next(ctx)? else { return Ok(false) };
            if let GValue::Edge(edge) = &t.value {
                if self.ids.contains(&edge.secondary_id) {
                    buffer.push_back(t);
                    return Ok(true);
                }
            } else {
                return Err(StoreError::UnexpectedDataType("end vertex filter can only be applied on Edge".into()));
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
