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

use std::{collections::VecDeque, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, GValue},
};

/// Physical step for `unfold()`: emits each element of a `GValue::List` individually.
/// Non-list values pass through unchanged.
#[derive(Debug, Default)]
pub struct UnfoldStep {
    upstream: Option<StepRef>,
    buffer: VecDeque<Rc<Traverser>>,
}

impl CoreStep for UnfoldStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            if let Some(t) = self.buffer.pop_front() {
                return Ok(Some(smallvec![t]));
            }

            let Some(upstream) = self.upstream.as_ref() else {
                return Ok(None);
            };
            let Some(t) = upstream.next(ctx)? else {
                return Ok(None);
            };

            if let GValue::List(items) = &t.value {
                for item in items.iter().rev() {
                    self.buffer.push_front(Traverser::new_rc_with_parent(item.clone(), Rc::clone(&t)));
                }
            } else {
                return Ok(Some(smallvec![t]));
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
        self.buffer.clear();
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
}
