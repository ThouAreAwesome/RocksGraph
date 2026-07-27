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

/// A physical step that extracts the "other" vertex from an edge traverser.
#[derive(Debug)]
pub struct OtherVStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// Whether to link the parent chain on emitted traversers (`false` skips the `Rc::clone`
    /// when the plan has no `as()`/`select()`/`path()` anywhere in it).
    track_path: bool,
}

impl OtherVStep {
    pub fn new(track_path: bool) -> Self {
        Self { upstream: None, track_path }
    }
}

/// Implements the `CoreStep` trait for `OtherVStep`.
impl CoreStep for OtherVStep {
    fn add_upper(&mut self, upstream: StepRef) {
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
            match &t.value {
                GValue::Edge(ek) => {
                    batch.push(Traverser::new_rc_conditional(GValue::Vertex(ek.secondary_id), &t, self.track_path))
                }
                other => {
                    return Err(StoreError::UnexpectedDataType(format!("otherV() expects an Edge, got {:?}", other)))
                }
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }

    /// Resets the state of this step and its upstream.
    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("OtherVStep")
    }
}
