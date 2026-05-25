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
    types::{keys::CanonicalKey, prop_key::PropKey, GValue},
};

pub struct ValuesStep {
    upstream: Option<StepRef>,
    property_keys: Vec<PropKey>,
}

impl ValuesStep {
    pub fn new(property_keys: Vec<PropKey>) -> Self {
        Self { upstream: None, property_keys }
    }
}

impl CoreStep for ValuesStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        loop {
            let t = self.upstream.as_ref()?.next(ctx)?;
            let canonical_key = match &t.value {
                GValue::Vertex(v_arc) => CanonicalKey::Vertex(*v_arc),
                GValue::Edge(e_arc) => CanonicalKey::Edge(e_arc.canonical_edge_key()),
                // TODO: raise an error if it's not a vertex or edge
                _ => continue,
            };

            if self.property_keys.is_empty() {
                // TODO: implement fetching all properties if property_keys is empty
                continue;
            }

            let mut results = smallvec![];
            for key in &self.property_keys {
                if let Some(value) = ctx.get_property(canonical_key, key).ok()? {
                    results.push(Traverser::new_rc(GValue::Scalar(value)));
                }
            }
            if !results.is_empty() {
                return Some(results);
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
