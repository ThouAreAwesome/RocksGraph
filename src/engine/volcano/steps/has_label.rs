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
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, prop_key::LABEL_KEY_ID, CanonicalKey, GValue, Primitive, PrimitivePredicate},
};

/// Sentinel `label_id` an unregistered label name resolves to (see `PhysicalPlanBuilder`'s
/// `LogicalStep::HasLabel` arm) — guaranteed to never equal a real one, since real ids are
/// non-negative (`LabelId` is `u16`, cast up to `i32` here to share `Primitive::Int32` with the
/// runtime value being compared against).
pub(crate) const UNRESOLVED_LABEL_ID: i32 = -1;

/// A physical step that filters traversers based on the label of the element they carry.
#[derive(Debug)]
pub struct HasLabelStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// Predicate over the element's label id in the *vertex* namespace — resolved from the
    /// user's label name(s) once at build time (see `PhysicalPlanBuilder`), so `produce()` never
    /// needs to touch the schema.
    vertex_pred: PrimitivePredicate,
    /// Predicate over the element's label id in the *edge* namespace. Separate from
    /// `vertex_pred` because vertex and edge labels are independent id spaces — the same name
    /// can resolve to different ids (or be registered in only one namespace).
    edge_pred: PrimitivePredicate,
}

/// Creates a new `HasLabelStep` with the vertex- and edge-namespace label-id predicates.
impl HasLabelStep {
    pub fn new(vertex_pred: PrimitivePredicate, edge_pred: PrimitivePredicate) -> Self {
        Self { upstream: None, vertex_pred, edge_pred }
    }
}

impl CoreStep for HasLabelStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        // Produces traversers whose element's label id matches the resolved predicate — a plain
        // integer comparison, no schema lookup needed.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            let matched = match &t.value {
                GValue::Vertex(vk) => {
                    let Some(Primitive::Int32(lb)) = ctx.get_value(&CanonicalKey::Vertex(*vk), LABEL_KEY_ID).unwrap()
                    else {
                        unreachable!("should alway find label id of a vertex")
                    };
                    self.vertex_pred.evaluate(&Primitive::Int32(lb))
                }
                GValue::Edge(ek) => self.edge_pred.evaluate(&Primitive::Int32(ek.label_id as i32)),
                _ => false,
            };
            if matched {
                return Ok(Some(smallvec![t]));
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
