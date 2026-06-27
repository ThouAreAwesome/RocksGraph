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

use crate::types::PIPELINE_PRODUCE_INLINE;
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::error::StoreError,
};

/// A physical step that limits the number of traversers emitted by its upstream.
#[derive(Debug)]
pub struct LimitStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Static/Fixed configuration ──
    /// The maximum number of elements to yield.
    limit: i64,

    // ── Dynamic/Runtime execution state ──
    /// The number of elements yielded so far.
    current_idx: usize,
}

/// Creates a new `LimitStep` with the specified limit.
impl LimitStep {
    pub fn new(limit: i64) -> Self {
        Self { upstream: None, limit, current_idx: 0 }
    }
}

impl CoreStep for LimitStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this limit.
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError> {
        // Produces traversers from its upstream until the limit is reached.
        if self.current_idx >= self.limit as usize {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let Some(t) = upstream.next(ctx)? else { return Ok(None) };
        self.current_idx += 1;
        Ok(Some(smallvec![t]))
    }

    fn reset(&mut self) {
        // Resets the step's internal counter and its upstream.
        self.current_idx = 0;
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("limit", self.limit.to_string())];
        ExplainNode::new("LimitStep").with_params(params)
    }
}
