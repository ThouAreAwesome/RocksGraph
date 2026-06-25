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
        keys::{AdjacentEdgeCursor, AdjacentEdgesOptions, Rank},
        BatchScenario, Direction, GValue, LabelId, VertexKey,
    },
};

/// A physical step that traverses either incoming or outgoing edges (or both) from a vertex, returning adjacent
/// vertices or edges.
#[derive(Debug)]
pub struct InOutStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The edge labels to filter by during traversal (empty means all labels).
    label_ids: SmallVec<[LabelId; 4]>,
    /// The direction of the edge traversal (incoming or outgoing).
    direction: Direction,
    /// Maximum number of results to produce per input vertex.
    limit: Option<u32>,
    /// Optional target vertex IDs to filter the destination vertices of the traversed edges.
    end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    /// Optional edge rank to filter by, folded in from a `.has("rank", N)` filter.
    rank: Option<Rank>,
    /// Whether to return the traversed edges themselves (true) or the adjacent vertices (false).
    output_edges: bool,
    /// Whether to link the parent chain on emitted traversers (`false` skips the `Rc::clone`
    /// when the plan has no `as()`/`select()`/`path()` anywhere in it).
    track_path: bool,

    // ── Dynamic/Runtime execution state ──
    /// The parent traverser currently being expanded.
    current_input: Option<Rc<Traverser>>,
    /// The index of the label in `label_ids` currently being processed for the active input.
    current_label_idx: usize,
    /// Suffix cursor for paginating results of the current label/direction scan.
    cursor: Option<AdjacentEdgeCursor>,
}

impl InOutStep {
    /// Creates a new `InOutStep` for traversing adjacent vertices or edges.
    pub fn new(
        label_ids: SmallVec<[LabelId; 4]>,
        direction: Direction,
        end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
        rank: Option<Rank>,
        output_edges: bool,
        track_path: bool,
    ) -> Self {
        Self {
            upstream: None,
            label_ids,
            direction,
            limit: None,
            end_vertex_ids,
            rank,
            current_input: None,
            current_label_idx: 0,
            cursor: None,
            output_edges,
            track_path,
        }
    }
}

impl CoreStep for InOutStep {
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
                    self.cursor = None;
                } else {
                    continue;
                }
            }

            let t = Rc::clone(self.current_input.as_ref().unwrap());
            if let GValue::Vertex(vk) = &t.value {
                let label = if self.label_ids.is_empty() { None } else { Some(self.label_ids[self.current_label_idx]) };

                let opts = AdjacentEdgesOptions {
                    label,
                    dst: self.end_vertex_ids.as_deref(),
                    rank: self.rank.as_ref().map(std::slice::from_ref),
                    start_from: self.cursor,
                };
                let batch_size = ctx.batch_size(BatchScenario::GetAdjacentEdges);
                let fetch_limit = match self.limit {
                    Some(l) => std::cmp::min(l, batch_size),
                    None => batch_size,
                };
                let (edges, next_cursor) = ctx.get_adjacent_edges(*vk, self.direction, opts, Some(fetch_limit))?;

                self.cursor = next_cursor;
                if self.cursor.is_none() {
                    self.current_label_idx += 1;
                    if self.label_ids.is_empty() || self.current_label_idx >= self.label_ids.len() {
                        self.current_input = None;
                    }
                }

                if !edges.is_empty() {
                    let results: SmallVec<[_; 4]> = edges
                        .into_iter()
                        .map(|e| {
                            let value =
                                if self.output_edges { GValue::Edge(e) } else { GValue::Vertex(e.secondary_id) };
                            Traverser::new_rc_conditional(value, &t, self.track_path)
                        })
                        .collect();
                    return Ok(Some(results));
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
        self.cursor = None;
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
