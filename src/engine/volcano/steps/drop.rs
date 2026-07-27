// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

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
    types::{error::StoreError, GValue},
};

/// A physical step that drops the element (vertex, edge, or property) carried by the incoming traverser.
#[derive(Default, Debug)]
pub struct DropStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,
}

/// Implements the `CoreStep` trait for `DropStep`.
impl CoreStep for DropStep {
    /// Wire an upstream step. Called once per upstream during plan construction.
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        // Consumes all incoming traversers and drops the elements they carry from the graph context.
        let Some(up) = self.upstream.as_deref() else {
            return Ok(None);
        };
        while let Some(el) = up.next(ctx)? {
            match &el.value {
                GValue::Property(pp) => ctx.drop_property(pp)?,
                GValue::Vertex(vt) => ctx.drop_vertex(*vt)?,
                GValue::Edge(eg) => ctx.drop_edge(eg)?,
                _ => {
                    return Err(StoreError::UnexpectedDataType("unexpected data type for drop step".into()));
                }
            }
        }
        Ok(None)
    }

    /// Reset all mutable state and propagate to upstreams.
    /// Resets the state of this step and its upstream.
    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("DropStep")
    }
}
