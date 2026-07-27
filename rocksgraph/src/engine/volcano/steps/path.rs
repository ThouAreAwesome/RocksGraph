// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::types::PIPELINE_PRODUCE_SIZE;
use crate::types::STEP_LABEL_INLINE;
use std::rc::Rc;

use smallvec::SmallVec;
use smol_str::SmolStr;

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue},
};

/// A physical step that collects the full path of traversers.
#[derive(Debug)]
pub struct PathStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,
    // ── Dynamic/Runtime execution state ──
}

impl PathStep {
    pub fn new() -> Self {
        Self { upstream: None }
    }
}

impl CoreStep for PathStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };

        let mut batch = SmallVec::new();
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            let Some(t) = upstream.next(ctx)? else { break };
            let path_gvalues: Vec<(GValue, Option<SmallVec<[SmolStr; STEP_LABEL_INLINE]>>)> = t.collect_path();
            batch.push(Traverser::new_rc_conditional(GValue::Path(path_gvalues), &t, true));
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
        ExplainNode::new("PathStep")
    }
}
