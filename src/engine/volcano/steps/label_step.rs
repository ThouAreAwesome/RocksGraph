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

use std::{rc::Rc, sync::Arc};

use smallvec::{smallvec, SmallVec};
use smol_str::SmolStr;

use crate::types::PIPELINE_BATCH_INLINE;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, ExplainNode, StepRef},
    },
    schema::Schema,
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive},
        keys::{CanonicalKey, LabelId},
        prop_key::LABEL_KEY_ID,
    },
};

/// Extracts the label string of the current element.  For vertices this reads
/// the label from the overlay via `get_value(LABEL_KEY_ID)` — which benefits
/// from the vertex-label cache and skips a full property load when the label is
/// already known (e.g. from an adjacent edge read).  For edges the label_id is
/// already in `EdgeKey`, so no RocksDB read is needed — only a schema lookup to
/// decode the id into a string name.
#[derive(Debug, Default)]
pub struct LabelStep {
    upstream: Option<StepRef>,
}

impl CoreStep for LabelStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };
        let Some(t) = upstream.next(ctx)? else {
            return Ok(None);
        };

        let label_str = match &t.value {
            GValue::Vertex(vk) => {
                let Some(Primitive::Int32(label_id)) = ctx.get_value(&CanonicalKey::Vertex(*vk), LABEL_KEY_ID)? else {
                    return Err(StoreError::NotFound);
                };
                decode_label(label_id, true, ctx.schema())
            }
            GValue::Edge(ek) => decode_label(ek.label_id, false, ctx.schema()),
            _ => return Ok(Some(smallvec![t])),
        };

        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(Primitive::String(label_str)))]))
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("LabelStep")
    }
}

/// Decode a numeric label_id into its string name using the schema registry.
/// `is_vertex` determines which namespace to look in — vertex and edge labels
/// are independent id spaces that both start at 1.
pub(super) fn decode_label(label_id: LabelId, is_vertex: bool, schema: Arc<std::sync::RwLock<Schema>>) -> SmolStr {
    let guard = schema.read().unwrap();
    let name = if is_vertex { guard.vertex_label_str(label_id) } else { guard.edge_label_str(label_id) };
    name.cloned().unwrap_or_else(|| SmolStr::from(format!("label_{}", label_id)))
}
