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
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive},
    },
};

/// A physical step that counts the number of traversers received from its upstream.
#[derive(Default, Debug)]
pub struct CountStep {
    upstream: Option<StepRef>,
    done: bool,
}

/// Implements the `CoreStep` trait for `CountStep`.
impl CoreStep for CountStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        if self.done {
            // Only produces a single count result.
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut count: u64 = 0;
        while upstream.next(ctx)?.is_some() {
            count += 1;
        }
        self.done = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::Scalar(Primitive::Int64(count as i64)))]))
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
}
