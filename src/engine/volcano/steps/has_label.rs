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

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, keys::LabelId, prop_key::LABEL, CanonicalKey, GValue, Primitive},
};
use std::{collections::VecDeque, rc::Rc};

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

    fn produce(&mut self, ctx: &mut dyn GraphCtx, buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(false) };
            let Some(t) = upstream.next(ctx)? else { return Ok(false) };
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
                buffer.push_back(t);
                return Ok(true);
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
