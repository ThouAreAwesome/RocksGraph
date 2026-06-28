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

use crate::types::{PIPELINE_PRODUCE_SIZE, SMALL_VECTOR_LENGTH};
use std::rc::Rc;

use smallvec::SmallVec;

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::types::prop_key::LABEL_KEY_ID;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::{CoreStep, StepRef},
    },
    types::keys::CanonicalKey,
    types::{GValue, StoreError, VertexKey},
};

/// Fused physical step that filters edges by predicates on the other (end) vertex.
#[derive(Default, Debug)]
pub struct EndVertexFilterStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// Target vertex IDs (None = unconstrained, Some([]) = matches nothing).
    ids: Option<SmallVec<[VertexKey; SMALL_VECTOR_LENGTH]>>,
    /// Label predicates on the other vertex, ANDed — same accumulation shape as
    /// `property_preds` (label has no structural lookup-key role here, unlike `ids`).
    label_preds: Vec<crate::types::PrimitivePredicate>,
    /// Property predicates on the other vertex, ANDed.
    property_preds: Vec<(u16, crate::types::PrimitivePredicate)>,
}

impl EndVertexFilterStep {
    pub fn new(
        ids: Option<SmallVec<[VertexKey; SMALL_VECTOR_LENGTH]>>,
        label_preds: Vec<crate::types::PrimitivePredicate>,
        property_preds: Vec<(u16, crate::types::PrimitivePredicate)>,
    ) -> Self {
        Self { upstream: None, ids, label_preds, property_preds }
    }
}

impl CoreStep for EndVertexFilterStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = SmallVec::with_capacity(PIPELINE_PRODUCE_SIZE);
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            let Some(t) = upstream.next(ctx)? else { break };
            if let GValue::Edge(edge) = &t.value {
                let dst_id = edge.secondary_id;
                // Id filter — empty ids means match nothing.
                if let Some(ref ids) = self.ids {
                    if !ids.contains(&dst_id) {
                        continue;
                    }
                }
                // Label and property filters on the other vertex.
                if !self.label_preds.is_empty() || !self.property_preds.is_empty() {
                    let ck = CanonicalKey::Vertex(dst_id);
                    let mut skip = false;
                    for lp in &self.label_preds {
                        if let Some(v) = ctx.get_value(&ck, LABEL_KEY_ID)? {
                            if !lp.evaluate(&v) {
                                skip = true;
                                break;
                            }
                        } else {
                            skip = true;
                            break;
                        }
                    }
                    if !skip {
                        for (key_id, pp) in &self.property_preds {
                            if let Some(v) = ctx.get_value(&ck, *key_id)? {
                                if !pp.evaluate(&v) {
                                    skip = true;
                                    break;
                                }
                            } else {
                                skip = true;
                                break;
                            }
                        }
                    }
                    if skip {
                        continue;
                    }
                }
                batch.push(t);
            } else {
                return Err(StoreError::UnexpectedDataType("end vertex filter can only be applied on Edge".into()));
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
        let params = vec![("ids", format!("{:?}", self.ids))];
        ExplainNode::new("EndVertexFilterStep").with_params(params)
    }
}
