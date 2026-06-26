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

use crate::types::PIPELINE_BATCH_INLINE;
use crate::types::STEP_LABEL_INLINE;
use std::rc::Rc;

use smallvec::{smallvec, SmallVec};
use smol_str::SmolStr;

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::error::StoreError,
};

/// Physical step for `as(label)`: attaches labels to the current traverser
/// without changing its value or parent chain.
#[derive(Debug)]
pub struct AsStep {
    upstream: Option<StepRef>,
    labels: SmallVec<[SmolStr; STEP_LABEL_INLINE]>,
}

impl AsStep {
    pub fn new(labels: SmallVec<[SmolStr; STEP_LABEL_INLINE]>) -> Self {
        Self { upstream: None, labels }
    }
}

impl CoreStep for AsStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        let Some(upstream) = self.upstream.as_ref() else {
            return Ok(None);
        };
        let Some(t) = upstream.next(ctx)? else {
            return Ok(None);
        };

        // Create a new traverser with the same value and parent, but with labels set.
        let labeled =
            Rc::new(Traverser { value: t.value.clone(), parent: t.parent.clone(), labels: Some(self.labels.clone()) });
        Ok(Some(smallvec![labeled]))
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
        let params = vec![("label", format!("{:?}", self.labels))];
        ExplainNode::new("AsStep").with_params(params)
    }
}

/// Physical step for `select(label)`: walks the parent chain to find a traverser
/// whose labels contain the target label, then emits that traverser.
#[derive(Debug)]
pub struct SelectStep {
    upstream: Option<StepRef>,
    labels: SmallVec<[SmolStr; STEP_LABEL_INLINE]>,
}

impl SelectStep {
    pub fn new(labels: SmallVec<[SmolStr; STEP_LABEL_INLINE]>) -> Self {
        Self { upstream: None, labels }
    }
}

impl CoreStep for SelectStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else {
                return Ok(None);
            };
            let Some(t) = upstream.next(ctx)? else {
                return Ok(None);
            };

            // Walk the parent chain to find a traverser whose labels match
            if let Some(found) = find_labeled(&t, &self.labels) {
                return Ok(Some(smallvec![found]));
            }
            // Not found in this traverser's chain — skip and try next
        }
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
        let params = vec![("label", format!("{:?}", self.labels))];
        ExplainNode::new("SelectStep").with_params(params)
    }
}

/// Walk the parent chain to find the first traverser whose `labels`
/// contains any of the target labels. Returns that traverser if found.
fn find_labeled(t: &Rc<Traverser>, targets: &SmallVec<[SmolStr; STEP_LABEL_INLINE]>) -> Option<Rc<Traverser>> {
    let mut cur = Some(Rc::clone(t));
    while let Some(node) = cur {
        if let Some(ref labels) = node.labels {
            for target in targets {
                if labels.contains(target) {
                    return Some(Rc::clone(&node));
                }
            }
        }
        cur = node.parent.as_ref().map(Rc::clone);
    }
    None
}
