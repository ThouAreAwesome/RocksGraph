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
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive},
    },
};

/// A physical step that counts the number of traversers received from its upstream.
#[derive(Default, Debug)]
pub struct CountStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Dynamic/Runtime execution state ──
    /// Whether the count has already been produced.
    done: bool,
}

/// Implements the `CoreStep` trait for `CountStep`.
impl CoreStep for CountStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.done {
            // Only produces a single count result.
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut count: i64 = 0;
        while upstream.next(ctx)?.is_some() {
            count += 1;
        }
        self.done = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(Primitive::Int64(count)))]))
    }

    fn reset(&mut self) {
        // Resets the step's internal state, allowing it to recount.
        self.done = false;
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    /// Returns a clone of the upstream step reference.
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("CountStep")
    }
}
