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

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        keys::{Rank, DEFAULT_RANK},
        Direction, EdgeKey, GValue, LabelId, VertexKey,
    },
};

/// A physical step that retrieves specific edges by exact key — a point lookup, used whenever
/// the planner already knows the label(s), end vertex/vertices, and (optionally) rank of the
/// edges being traversed, instead of scanning a vertex's adjacency list and filtering.
///
/// Every template key built in `new` always leaves `primary_id` as the runtime placeholder
/// (filled in with the upstream vertex in `produce`) regardless of direction — `EdgeKey::out_e`
/// and `EdgeKey::in_e` both happen to route their "currently unknown" argument into
/// `primary_id`, so a single unconditional assignment is correct for every key in the buffer,
/// including a mix of OUT and IN keys (the `direction: None` / "both" case).
#[derive(Debug)]
pub struct GetEStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// Whether to return the matched edges themselves (true) or the adjacent vertices (false).
    output_edges: bool,

    // ── Dynamic/Runtime execution state ──
    /// Pre-allocated template EdgeKeys populated with 0 as placeholder for the upstream vertex.
    /// Mutated in-place during execution to avoid allocations.
    keys_buffer: Vec<EdgeKey>,
}

impl GetEStep {
    /// Creates a new `GetEStep` to retrieve edges by exact key.
    ///
    /// `direction`: `Some(OUT)`/`Some(IN)` restricts the lookup to that direction (`outE`/`inE`,
    /// or their vertex-emitting `out`/`in` counterparts); `None` builds both an OUT and an IN
    /// template per `(label, end_vertex)` pair (`bothE`/`both`).
    ///
    /// `rank`: the edge rank to look up. `None` defaults to `DEFAULT_RANK` — correct whenever
    /// the label is known to be single-edge, and the same assumption this step already made
    /// before rank filters could be folded in.
    pub fn new(
        label_ids: SmallVec<[LabelId; 4]>,
        end_vertex_ids: SmallVec<[VertexKey; 4]>,
        direction: Option<Direction>,
        rank: Option<Rank>,
        output_edges: bool,
    ) -> Self {
        let directions: SmallVec<[Direction; 2]> = match direction {
            Some(d) => smallvec![d],
            None => smallvec![Direction::OUT, Direction::IN],
        };
        let rank = rank.unwrap_or(DEFAULT_RANK);
        let mut keys_buffer = Vec::with_capacity(label_ids.len() * end_vertex_ids.len() * directions.len());
        for label_id in &label_ids {
            for dst in &end_vertex_ids {
                for dir in &directions {
                    let edge_key = match dir {
                        Direction::OUT => EdgeKey::out_e(0, *label_id, *dst, rank),
                        Direction::IN => EdgeKey::in_e(*dst, *label_id, 0, rank),
                    };
                    keys_buffer.push(edge_key);
                }
            }
        }
        Self { upstream: None, output_edges, keys_buffer }
    }
}

impl CoreStep for GetEStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this edge retrieval.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces traversers for edges that match the specified criteria.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };

            let src = match t.value {
                GValue::Vertex(src) => src,
                _ => return Err(StoreError::UnexpectedDataType("expected Vertex before outE".into())),
            };

            let mut results: SmallVec<[_; 4]> = smallvec![];
            let output_edges = self.output_edges;
            let to_gvalue = move |edge_key: EdgeKey| {
                if output_edges {
                    GValue::Edge(edge_key)
                } else {
                    GValue::Vertex(edge_key.secondary_id)
                }
            };

            if self.keys_buffer.len() == 1 {
                let edge_key = &mut self.keys_buffer[0];
                edge_key.primary_id = src;
                if ctx.get_edge(edge_key)?.is_some() {
                    results.push(Traverser::new_rc_with_parent(to_gvalue(*edge_key), Rc::clone(&t)));
                }
            } else {
                for edge_key in &mut self.keys_buffer {
                    edge_key.primary_id = src;
                }
                let fetched = ctx.get_edges(&self.keys_buffer)?;
                for edge_key in fetched {
                    results.push(Traverser::new_rc_with_parent(to_gvalue(edge_key), Rc::clone(&t)));
                }
            }

            if !results.is_empty() {
                return Ok(Some(results));
            }
        }
    }

    fn reset(&mut self) {
        // Resets the state of this step and its upstream.
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }
}
