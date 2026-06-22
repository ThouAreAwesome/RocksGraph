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

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue},
};

/// A physical step that extracts the "other" vertex from an edge traverser.
#[derive(Default, Debug)]
pub struct OtherVStep {
    // ── Upstream link ──
    upstream: Option<StepRef>,
}

/// Implements the `CoreStep` trait for `OtherVStep`.
impl CoreStep for OtherVStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            // Pull a traverser from the upstream.
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            // If the traverser carries an edge, extract its secondary ID (the "other" vertex) and emit it as a new
            // traverser.
            if let GValue::Edge(ek) = &t.value {
                return Ok(Some(smallvec![Traverser::new_rc_with_parent(GValue::Vertex(ek.secondary_id), t.clone())]));
            }
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
}
