// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

use std::rc::Rc;

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{keys::LabelId, GValue},
};

pub struct HasLabelStep {
    upstream: Option<StepRef>,
    label_ids: Vec<LabelId>,
}

impl HasLabelStep {
    pub fn new(label_ids: Vec<LabelId>) -> Self {
        Self { upstream: None, label_ids }
    }
}

impl CoreStep for HasLabelStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        loop {
            let t = self.upstream.as_ref()?.next(ctx)?;
            let matched = match &t.value {
                GValue::Vertex(v_arc) => {
                    let vertex = ctx.get_vertex(*v_arc).ok()??;
                    self.label_ids.contains(&vertex.label_id)
                }
                GValue::Edge(e_arc) => self.label_ids.contains(&e_arc.label_id),
                _ => false,
            };
            if matched {
                return Some(smallvec![Rc::clone(&t)]);
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
