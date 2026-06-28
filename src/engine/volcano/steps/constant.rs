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

use crate::types::PIPELINE_PRODUCE_SIZE;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, ExplainNode, StepRef},
    },
    types::{error::StoreError, gvalue::Primitive},
};

/// Replaces each traverser with a constant value, discarding the original.
#[derive(Debug)]
pub struct ConstantStep {
    upstream: Option<StepRef>,
    value: Primitive,
    track_path: bool,
}

impl ConstantStep {
    pub fn new(value: Primitive, track_path: bool) -> Self {
        Self { upstream: None, value, track_path }
    }
}

impl CoreStep for ConstantStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };
        let Some(t) = upstream.next(ctx)? else {
            return Ok(None);
        };
        Ok(Some(smallvec![Traverser::new_rc_conditional(
            crate::types::GValue::Scalar(self.value.clone()),
            &t,
            self.track_path
        )]))
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("ConstantStep").with_params(vec![("value", format!("{:?}", self.value))])
    }
}
