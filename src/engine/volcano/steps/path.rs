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
    /// Whether the collected paths have already been emitted.
    emitted: bool,
}

impl PathStep {
    pub fn new() -> Self {
        Self { upstream: None, emitted: false }
    }
}

impl CoreStep for PathStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError> {
        if self.emitted {
            return Ok(None);
        }

        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };

        let mut paths = SmallVec::new();
        while let Some(t) = upstream.next(ctx)? {
            let path_gvalues: Vec<(GValue, Option<SmallVec<[SmolStr; STEP_LABEL_INLINE]>>)> = t.collect_path();
            paths.push(Traverser::new_rc(GValue::Path(path_gvalues)));
        }

        self.emitted = true;
        if paths.is_empty() {
            Ok(None)
        } else {
            Ok(Some(paths))
        }
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
        ExplainNode::new("PathStep")
    }
}
