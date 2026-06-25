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

use crate::types::PIPELINE_BATCH_INLINE;
use crate::types::VERTEX_PROPS_INLINE;
use std::{collections::HashMap, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        gvalue::Primitive,
        keys::{CanonicalKey, Direction, EdgeKey, LabelId, Rank, VertexKey, DEFAULT_RANK},
        CanonicalEdgeKey, GValue, Property,
    },
};

/// A physical step that adds a new edge to the graph.
#[derive(Debug)]
pub struct AddEStep {
    // ── Static/Fixed configuration ──
    /// The label ID of the edge to be created.
    label_id: LabelId,
    /// The source vertex key of the edge.
    out_v_id: VertexKey,
    /// The destination vertex key of the edge.
    in_v_id: VertexKey,
    /// The property list to initialize the new edge with.
    properties: SmallVec<[Property; VERTEX_PROPS_INLINE]>,
    /// The rank of the edge to be created.
    rank: Rank,

    // ── Dynamic/Runtime execution state ──
    /// Whether the edge has been successfully created and emitted in this run.
    emitted: bool,
}

impl AddEStep {
    /// Creates a new `AddEStep` with the specified edge details.
    pub fn new(
        label_id: LabelId,
        out_v_id: VertexKey,
        in_v_id: VertexKey,
        properties: HashMap<u16, Primitive>,
        rank: Option<Rank>,
    ) -> Self {
        let final_rank = rank.unwrap_or(DEFAULT_RANK);
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property {
                owner: CanonicalKey::Edge(CanonicalEdgeKey {
                    src_id: out_v_id,
                    label_id,
                    dst_id: in_v_id,
                    rank: final_rank,
                }),
                key,
                value,
            })
            .collect::<SmallVec<[Property; VERTEX_PROPS_INLINE]>>();
        Self { label_id, out_v_id, in_v_id, properties, rank: final_rank, emitted: false }
    }
}

impl CoreStep for AddEStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("AddEStep is a source step and cannot have an upstream");
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        // Emits the newly created edge as a traverser.
        if self.emitted {
            return Ok(None);
        }
        let edge_key = EdgeKey {
            primary_id: self.out_v_id,
            direction: Direction::OUT,
            label_id: self.label_id,
            secondary_id: self.in_v_id,
            rank: self.rank,
        };
        let new_edge = ctx.add_edge(&edge_key)?;
        for property in &self.properties {
            ctx.set_property(property)?;
        }
        self.emitted = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::Edge(new_edge))]))
    }

    fn reset(&mut self) {
        // Resets the step's state, allowing it to be re-executed.
        self.emitted = false;
    }
}
