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

/// A physical step that collects the full path of traversers.
#[derive(Debug)]
pub struct PathStep {
    upstream: Option<StepRef>,
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

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        if self.emitted {
            return Ok(None);
        }

        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };

        let mut paths = SmallVec::new();
        while let Some(t) = upstream.next(ctx)? {
            let path_gvalues: Vec<GValue> = t.collect_path().into_iter().map(|(gv, _)| gv).collect();
            paths.push(Traverser::new_rc(GValue::List(Rc::new(path_gvalues))));
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
}
