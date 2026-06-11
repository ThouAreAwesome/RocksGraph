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
    types::{error::StoreError, keys::LabelId, prop_key::LABEL, CanonicalKey, GValue, Primitive},
};

/// A physical step that filters traversers based on the label of the element they carry.
#[derive(Debug)]
pub struct HasLabelStep {
    upstream: Option<StepRef>,
    label_ids: SmallVec<[LabelId; 4]>,
}

/// Creates a new `HasLabelStep` with a list of target label IDs.
impl HasLabelStep {
    pub fn new(label_ids: SmallVec<[LabelId; 4]>) -> Self {
        Self { upstream: None, label_ids }
    }
}

impl CoreStep for HasLabelStep {
    fn add_upper(&mut self, upstream: StepRef) {
        // Sets the upstream step for this filter.
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        // Produces traversers whose element's label ID is present in the `label_ids` list.
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            let matched = match &t.value {
                GValue::Vertex(vk) => {
                    let Some(Primitive::Int32(lb)) = ctx.get_value(&CanonicalKey::Vertex(*vk), &LABEL).unwrap() else {
                        unreachable!("")
                    };
                    self.label_ids.contains(&(lb as u16))
                }
                GValue::Edge(ek) => self.label_ids.contains(&ek.label_id),
                _ => false,
            };
            if matched {
                return Ok(Some(smallvec![Rc::clone(&t)]));
            }
        }
    }

    fn reset(&mut self) {
        // Resets the state of this step and its upstream.
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }

    fn upper(&self) -> Option<StepRef> {
        // Returns a clone of the upstream step reference.
        self.upstream.clone()
    }
}
