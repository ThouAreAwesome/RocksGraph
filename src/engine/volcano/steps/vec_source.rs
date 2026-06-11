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
    types::error::StoreError,
};

/// A physical step that acts as a source, emitting a predefined vector of `Traverser` items.
#[derive(Debug)]
pub struct VecSourceStep {
    items: SmallVec<[Rc<Traverser>; 4]>,
}

impl VecSourceStep {
    /// Creates an empty `VecSourceStep`.
    pub fn empty() -> Self {
        Self { items: SmallVec::new() }
    }

    /// Injects a collection of `Traverser` items into this source step.
    /// These items will be emitted when `produce` is called.
    pub fn inject(&mut self, items: SmallVec<[Rc<Traverser>; 4]>) {
        self.items.extend(items);
    }
}

impl CoreStep for VecSourceStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        // `VecSourceStep` is a source step and does not have an upstream.
        panic!("VecSourceStep is a source step and cannot have an upstream");
    }

    fn produce(&mut self, _ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Emits all currently held `Traverser` items and then clears its internal buffer.
        if !self.items.is_empty() {
            Ok(Some(self.items.drain(..).collect()))
        } else {
            Ok(None)
        }
    }

    fn reset(&mut self) {
        // Resets the step by clearing its internal buffer of items.
        self.items.clear();
    }
}
