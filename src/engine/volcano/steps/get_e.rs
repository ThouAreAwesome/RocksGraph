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
    types::{error::StoreError, Direction, EdgeKey, GValue, LabelId, VertexKey},
};

pub struct GetEStep {
    upstream: Option<StepRef>,
    // label_ids should not be empty.
    label_ids: Vec<LabelId>,
    direction: Direction,
    // end_vertex_ids should not be empty.
    end_vertex_ids: Vec<VertexKey>,
}

impl GetEStep {
    pub fn new(label_ids: Vec<LabelId>, direction: Direction, end_vertex_ids: Vec<VertexKey>) -> Self {
        Self { upstream: None, label_ids, direction, end_vertex_ids }
    }
}

impl CoreStep for GetEStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx, buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(false) };
            let Some(t) = upstream.next(ctx)? else { return Ok(false) };

            let GValue::Vertex(src) = t.value else {
                return Err(StoreError::UnexpectedDataType("expected Vertex before outE".into()));
            };

            for label_id in &self.label_ids {
                for dst in &self.end_vertex_ids {
                    let edge_key = match self.direction {
                        Direction::OUT => EdgeKey::out_e(src, *label_id, *dst, 0),
                        Direction::IN => EdgeKey::in_e(src, *label_id, *dst, 0),
                    };

                    if let Some(_e) = ctx.get_edge(&edge_key)? {
                        buffer.push_back(Traverser::new_rc_with_parent(GValue::Edge(edge_key), t.clone()));
                    }
                }
            }
            if !buffer.is_empty() {
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
