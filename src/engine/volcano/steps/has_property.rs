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

use crate::types::PIPELINE_PRODUCE_SIZE;
use std::rc::Rc;

use smallvec::SmallVec;

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    planner::optimizer::primitive_to_rank,
    types::{
        error::StoreError,
        gvalue::{Primitive, PrimitivePredicate},
        prop_key::{LABEL_KEY_ID, RANK_KEY_ID},
        CanonicalKey, GValue,
    },
};

/// A physical step that filters traversers based on a specific property key and its expected value.
#[derive(Debug)]
pub struct HasPropertyStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The property key ID to filter by.
    prop_key_id: u16,
    /// The predicate to filter property values.
    pred: PrimitivePredicate,
}

/// Creates a new `HasPropertyStep` with the property key ID and predicate to filter by.
impl HasPropertyStep {
    /// Normalizes predicate for reserved keys whose runtime representation doesn't
    /// match whatever literal type a caller wrote.
    pub fn new(prop_key_id: u16, pred: PrimitivePredicate) -> Self {
        let pred = if RANK_KEY_ID == prop_key_id {
            pred.map(|v| primitive_to_rank(&v).map(Primitive::UInt16).unwrap_or(v))
        } else {
            pred
        };
        Self { upstream: None, prop_key_id, pred }
    }

    /// `ctx.get_value`/`get_property` return the element's label as a raw
    /// `Primitive::Int32(label_id)` (see `Vertex`/`Edge::get_value`) — decode it to the
    /// label's string name so `.has("label", "person")` compares like-for-like with the
    /// `Primitive::String` an expected value would naturally take.
    fn decode_if_label(&self, ctx: &dyn GraphCtx, key: &CanonicalKey, value: Primitive) -> Primitive {
        if self.prop_key_id != LABEL_KEY_ID {
            return value;
        }
        ctx.schema().read().unwrap().decode_label_value(key, value)
    }
}

impl CoreStep for HasPropertyStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        // Produces traversers whose element has the specified property matching the predicate.
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = SmallVec::with_capacity(PIPELINE_PRODUCE_SIZE);
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            let Some(t) = upstream.next(ctx)? else { break };
            match &t.value {
                GValue::Vertex(vk) => {
                    let key = CanonicalKey::Vertex(*vk);
                    if let Some(vl) = ctx.get_value(&key, self.prop_key_id)? {
                        let vl = self.decode_if_label(ctx, &key, vl);
                        if self.pred.evaluate(&vl) {
                            batch.push(t);
                        }
                    }
                }
                GValue::Edge(ek) => {
                    let key = CanonicalKey::Edge(ek.canonical_edge_key());
                    if let Some(et) = ctx.get_value(&key, self.prop_key_id)? {
                        let et = self.decode_if_label(ctx, &key, et);
                        if self.pred.evaluate(&et) {
                            batch.push(t);
                        }
                    }
                }
                _ => {}
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
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

    fn explain(&self) -> ExplainNode {
        let params = vec![("key", self.prop_key_id.to_string()), ("pred", format!("{:?}", self.pred))];
        ExplainNode::new("HasPropertyStep").with_params(params)
    }
}
