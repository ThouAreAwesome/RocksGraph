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

use smallvec::SmallVec;

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
    upstream: Option<StepRef>,
}

/// Implements the `CoreStep` trait for `DropStep`.
impl CoreStep for DropStep {
    /// Wire an upstream step. Called once per upstream during plan construction.
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
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
}
