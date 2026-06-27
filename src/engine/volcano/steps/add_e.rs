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

use crate::types::PIPELINE_PRODUCE_INLINE;
use crate::types::VERTEX_PROPS_INLINE;
use std::{collections::HashMap, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
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
///
/// When both endpoints are `Some` the step acts as a source and emits one edge.
/// When one endpoint is `None` the step accepts an upstream; each traverser
/// provides the missing vertex via `GValue::Vertex`.
#[derive(Debug)]
pub struct AddEStep {
    label_id: LabelId,
    out_v_id: Option<VertexKey>,
    in_v_id: Option<VertexKey>,
    properties: SmallVec<[Property; VERTEX_PROPS_INLINE]>,
    rank: Rank,
    upstream: Option<StepRef>,
    emitted: bool,
}

impl AddEStep {
    pub fn new(
        label_id: LabelId,
        out_v_id: Option<VertexKey>,
        in_v_id: Option<VertexKey>,
        properties: HashMap<u16, Primitive>,
        rank: Option<Rank>,
    ) -> Self {
        let final_rank = rank.unwrap_or(DEFAULT_RANK);
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property {
                owner: CanonicalKey::Edge(CanonicalEdgeKey {
                    src_id: out_v_id.unwrap_or(0),
                    label_id,
                    dst_id: in_v_id.unwrap_or(0),
                    rank: final_rank,
                }),
                key,
                value,
            })
            .collect::<SmallVec<[Property; VERTEX_PROPS_INLINE]>>();
        Self { label_id, out_v_id, in_v_id, properties, rank: final_rank, upstream: None, emitted: false }
    }
}

impl CoreStep for AddEStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError> {
        if self.emitted && self.upstream.is_none() {
            return Ok(None);
        }
        let (out_v_id, in_v_id) = if let Some(ref upstream) = self.upstream {
            let Some(t) = upstream.next(ctx)? else {
                self.emitted = true;
                return Ok(None);
            };
            let vk = match &t.value {
                GValue::Vertex(v) => *v,
                other => {
                    return Err(StoreError::UnexpectedDataType(format!(
                        "addE expects a vertex traverser, got {:?}",
                        other
                    )));
                }
            };
            (self.out_v_id.unwrap_or(vk), self.in_v_id.unwrap_or(vk))
        } else {
            self.emitted = true;
            (
                self.out_v_id.expect("out_v_id required for source AddEStep"),
                self.in_v_id.expect("in_v_id required for source AddEStep"),
            )
        };

        let edge_key = EdgeKey {
            primary_id: out_v_id,
            direction: Direction::OUT,
            label_id: self.label_id,
            secondary_id: in_v_id,
            rank: self.rank,
        };
        let new_edge = ctx.add_edge(&edge_key)?;
        for property in &self.properties {
            ctx.set_property(property)?;
        }
        Ok(Some(smallvec![Traverser::new_rc(GValue::Edge(new_edge))]))
    }

    fn reset(&mut self) {
        self.emitted = false;
        if let Some(u) = &self.upstream {
            u.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![
            ("label", self.label_id.to_string()),
            ("from", format!("{:?}", self.out_v_id)),
            ("to", format!("{:?}", self.in_v_id)),
            ("rank", format!("{:?}", self.rank)),
        ];
        ExplainNode::new("AddEStep").with_params(params)
    }
}
