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

use std::{collections::HashSet, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue},
};

/// A physical step that removes duplicate traversers.
#[derive(Debug, Default)]
pub struct DedupStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,

    // ── Dynamic/Runtime execution state ──
    /// The set of unique values seen so far.
    seen: HashSet<GValue>,
}

impl CoreStep for DedupStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };

        while let Some(t) = upstream.next(ctx)? {
            match t.value {
                GValue::Edge(ek) => {
                    if self.seen.insert(GValue::Edge(ek.canonical())) {
                        return Ok(Some(smallvec![t]));
                    }
                }
                _ => {
                    if self.seen.insert(t.value.clone()) {
                        return Ok(Some(smallvec![t]));
                    }
                }
            }
        }

        Ok(None)
    }

    fn reset(&mut self) {
        self.seen.clear();
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
