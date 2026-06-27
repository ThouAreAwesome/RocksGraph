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

use crate::types::PIPELINE_PRODUCE_INLINE;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, ExplainNode, StepRef},
    },
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive},
    },
};

/// Extracts the id of the current element.  For vertices returns `Int64(vk)`,
/// for edges returns `Int64(primary_id)`.  All other traverser values pass
/// through unchanged.
#[derive(Debug, Default)]
pub struct IdStep {
    upstream: Option<StepRef>,
}

impl CoreStep for IdStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };
        let mut batch = SmallVec::with_capacity(PIPELINE_PRODUCE_INLINE);
        while batch.len() < PIPELINE_PRODUCE_INLINE {
            let Some(t) = upstream.next(ctx)? else { break };
            let id_value = match &t.value {
                GValue::Vertex(vk) => GValue::Scalar(Primitive::Int64(*vk)),
                GValue::Edge(ek) => GValue::Scalar(Primitive::String(ek.to_id_string().into())),
                _ => {
                    batch.push(Rc::clone(&t));
                    continue;
                }
            };
            batch.push(Traverser::new_rc(id_value));
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
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
        ExplainNode::new("IdStep")
    }
}
