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

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, Direction, GValue, LabelId, VertexKey},
};

pub struct BothStep {
    upstream: Option<StepRef>,
    label_ids: Vec<LabelId>,
    limit: Option<u32>,
    end_vertex_ids: Option<Vec<VertexKey>>,
    current_input: Option<Rc<Traverser>>,
    current_label_idx: usize,
    current_direction: Direction, // 0 = out, 1 = in
}

impl BothStep {
    pub fn new(label_ids: Vec<LabelId>, end_vertex_ids: Option<Vec<VertexKey>>) -> Self {
        Self {
            upstream: None,
            label_ids,
            limit: None,
            end_vertex_ids,
            current_input: None,
            current_label_idx: 0,
            current_direction: Direction::OUT,
        }
    }
}

impl CoreStep for BothStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            if self.current_input.is_none() {
                let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
                let Some(t) = upstream.next(ctx)? else { return Ok(None) };
                if matches!(&t.value, GValue::Vertex(_)) {
                    self.current_input = Some(t);
                    self.current_label_idx = 0;
                    self.current_direction = Direction::OUT;
                } else {
                    continue;
                }
            }

            let t = Rc::clone(self.current_input.as_ref().unwrap());
            if let GValue::Vertex(vk) = &t.value {
                let label = if self.label_ids.is_empty() { None } else { Some(self.label_ids[self.current_label_idx]) };

                let mut results = SmallVec::new();
                if self.current_direction == Direction::OUT {
                    let vertices = ctx.get_adjacent_vertices(
                        *vk,
                        label,
                        self.current_direction,
                        self.end_vertex_ids.as_deref(),
                        self.limit,
                    )?;
                    for vertex in vertices {
                        results.push(Traverser::new_rc_with_parent(GValue::Vertex(vertex), Rc::clone(&t)));
                    }
                    self.current_direction = Direction::IN;
                    if !results.is_empty() {
                        return Ok(Some(results));
                    }
                }

                if self.current_direction == Direction::IN {
                    let vertices = ctx.get_adjacent_vertices(
                        *vk,
                        label,
                        self.current_direction,
                        self.end_vertex_ids.as_deref(),
                        self.limit,
                    )?;
                    for vertex in vertices {
                        results.push(Traverser::new_rc_with_parent(GValue::Vertex(vertex), Rc::clone(&t)));
                    }
                    self.current_direction = Direction::OUT;
                    self.current_label_idx += 1;
                    if self.label_ids.is_empty() || self.current_label_idx >= self.label_ids.len() {
                        self.current_input = None;
                    }
                    if !results.is_empty() {
                        return Ok(Some(results));
                    }
                }
            } else {
                self.current_input = None;
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        self.current_input = None;
        self.current_label_idx = 0;
        self.current_direction = Direction::OUT;
    }
}
