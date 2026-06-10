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
    types::{error::StoreError, Direction, EdgeKey, GValue, LabelId, VertexKey},
};

#[derive(Debug)]
pub struct GetEStep {
    upstream: Option<StepRef>,
    // label_ids should not be empty.
    label_ids: SmallVec<[LabelId; 4]>,
    // end_vertex_ids should not be empty.
    end_vertex_ids: SmallVec<[VertexKey; 4]>,
    direction: Direction,
}

impl GetEStep {
    pub fn new(
        label_ids: SmallVec<[LabelId; 4]>,
        end_vertex_ids: SmallVec<[VertexKey; 4]>,
        direction: Direction,
    ) -> Self {
        Self { upstream: None, label_ids, end_vertex_ids, direction }
    }
}

impl CoreStep for GetEStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };

            let src = match t.value {
                GValue::Vertex(src) => src,
                _ => return Err(StoreError::UnexpectedDataType("expected Vertex before outE".into())),
            };

            let mut results: SmallVec<[_; 4]> = smallvec![];

            for label_id in &self.label_ids {
                for dst in &self.end_vertex_ids {
                    let edge_key = match self.direction {
                        Direction::OUT => EdgeKey::out_e(src, *label_id, *dst, 0),
                        Direction::IN => EdgeKey::in_e(*dst, *label_id, src, 0),
                    };
                    if let Some(_e) = ctx.get_edge(&edge_key)? {
                        results.push(Traverser::new_rc_with_parent(GValue::Edge(edge_key), Rc::clone(&t)));
                    }
                }
            }
            if !results.is_empty() {
                return Ok(Some(results));
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
