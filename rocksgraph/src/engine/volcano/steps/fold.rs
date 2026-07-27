// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::PIPELINE_PRODUCE_SIZE;
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue},
};

/// A physical step that collects all upstream traversers into a single `GValue::List`.
///
/// This implements the Gremlin `fold()` step: it drains the upstream pipeline
/// completely, wraps every value into a `Vec<GValue>`, and emits it as one
/// `GValue::List` traverser downstream.  It emits exactly once and then signals
/// exhaustion.
#[derive(Debug, Default)]
pub struct FoldStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Dynamic/Runtime execution state ──
    /// Whether the folded list has already been emitted.
    emitted: bool,
}

impl CoreStep for FoldStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.emitted {
            return Ok(None);
        }

        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut list = Vec::new();
        while let Some(t) = upstream.next(ctx)? {
            list.push(t.value.clone());
        }

        self.emitted = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::List(list))]))
    }

    fn reset(&mut self) {
        self.emitted = false;
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("FoldStep")
    }
}
