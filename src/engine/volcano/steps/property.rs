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
    types::{element::Property, error::StoreError, gvalue::Primitive, keys::CanonicalKey, GValue},
};

/// A physical step that sets a property on the element carried by the incoming traverser.
#[derive(Debug)]
pub struct PropertyStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The template property containing the key and value to set.
    /// The owner is updated dynamically to point to the current element.
    prop: Property,
}

/// Creates a new `PropertyStep` with the property key and value to set.
impl PropertyStep {
    pub fn new(prop_key_id: u16, prop_value: Primitive) -> Self {
        Self { upstream: None, prop: Property { owner: CanonicalKey::Empty, key: prop_key_id, value: prop_value } }
    }
}

impl CoreStep for PropertyStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this property setter.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        // Sets the property on the element carried by the upstream traverser and then re-emits the traverser.
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = SmallVec::with_capacity(PIPELINE_PRODUCE_SIZE);
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            let Some(t) = upstream.next(ctx)? else { break };
            let canonical_key = match &t.value {
                GValue::Vertex(vt) => CanonicalKey::Vertex(*vt),
                GValue::Edge(eg) => CanonicalKey::Edge(eg.canonical_edge_key()),
                other => {
                    return Err(StoreError::UnexpectedDataType(format!(
                        "property() expects a Vertex or Edge, got {:?}",
                        other
                    )))
                }
            };
            let mut prop = self.prop.clone();
            prop.owner = canonical_key;
            ctx.set_property(&prop)?;
            batch.push(t);
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
        let params = vec![("key", self.prop.key.to_string())];
        ExplainNode::new("PropertiesStep").with_params(params)
    }
}
