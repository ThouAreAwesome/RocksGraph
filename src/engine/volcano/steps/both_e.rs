// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

use std::rc::Rc;

use smallvec::SmallVec;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        keys::{AdjacentEdgeCursor, AdjacentEdgesOptions},
        Direction, GValue, LabelId, VertexKey,
    },
};

/// A physical step that traverses both incoming and outgoing edges from a vertex, returning the edges themselves.
#[derive(Debug)]
pub struct BothEStep {
    upstream: Option<StepRef>,
    label_ids: SmallVec<[LabelId; 4]>,
    limit: Option<u32>,
    end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    current_input: Option<Rc<Traverser>>,
    current_label_idx: usize,
    current_direction: Direction, // 0 = out, 1 = in
    cursor: Option<AdjacentEdgeCursor>,
}

impl BothEStep {
    /// Creates a new `BothEStep` for traversing incident edges in both directions.
    pub fn new(label_ids: SmallVec<[LabelId; 4]>, end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>) -> Self {
        Self {
            upstream: None,
            label_ids,
            limit: None,
            end_vertex_ids,
            current_input: None,
            current_label_idx: 0,
            current_direction: Direction::OUT,
            cursor: None,
        }
    }
}

impl CoreStep for BothEStep {
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
                    self.cursor = None;
                } else {
                    continue;
                }
            }

            let t = Rc::clone(self.current_input.as_ref().unwrap());
            if let GValue::Vertex(vk) = &t.value {
                let label = if self.label_ids.is_empty() { None } else { Some(self.label_ids[self.current_label_idx]) };

                let mut results = SmallVec::new();
                if self.current_direction == Direction::OUT {
                    let opts = AdjacentEdgesOptions {
                        label,
                        dst: self.end_vertex_ids.as_deref(),
                        rank: None,
                        start_from: self.cursor,
                    };
                    let (edges, next_cursor) = ctx.get_adjacent_edges(*vk, self.current_direction, opts, self.limit)?;
                    self.cursor = next_cursor;
                    for edge in edges {
                        results.push(Traverser::new_rc_with_parent(GValue::Edge(edge), Rc::clone(&t)));
                    }
                    if self.cursor.is_none() {
                        self.current_direction = Direction::IN;
                    }
                    if !results.is_empty() {
                        return Ok(Some(results));
                    }
                }

                if self.current_direction == Direction::IN {
                    let opts = AdjacentEdgesOptions {
                        label,
                        dst: self.end_vertex_ids.as_deref(),
                        rank: None,
                        start_from: self.cursor,
                    };
                    let (edges, next_cursor) = ctx.get_adjacent_edges(*vk, self.current_direction, opts, self.limit)?;
                    self.cursor = next_cursor;
                    for edge in edges {
                        results.push(Traverser::new_rc_with_parent(GValue::Edge(edge), Rc::clone(&t)));
                    }
                    if self.cursor.is_none() {
                        self.current_direction = Direction::OUT;
                        self.current_label_idx += 1;
                        if self.label_ids.is_empty() || self.current_label_idx >= self.label_ids.len() {
                            self.current_input = None;
                        }
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
        self.cursor = None;
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
