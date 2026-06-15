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

/// A physical step that collects all upstream traversers into a single `GValue::List`.
///
/// This implements the Gremlin `fold()` step: it drains the upstream pipeline
/// completely, wraps every value into a `Vec<GValue>`, and emits it as one
/// `GValue::List` traverser downstream.  It emits exactly once and then signals
/// exhaustion.
#[derive(Debug, Default)]
pub struct FoldStep {
    upstream: Option<StepRef>,
    emitted: bool,
}

impl CoreStep for FoldStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
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
}
