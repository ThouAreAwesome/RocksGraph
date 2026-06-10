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
    types::{error::StoreError, keys::CanonicalKey, prop_key::PropKey, GValue},
};

#[derive(Debug)]
pub struct ValuesStep {
    upstream: Option<StepRef>,
    property_keys: SmallVec<[PropKey; 4]>,
    emit_property: bool,
}

impl ValuesStep {
    pub fn new(property_keys: SmallVec<[PropKey; 4]>, emit_property: bool) -> Self {
        Self { upstream: None, property_keys, emit_property }
    }
}

impl CoreStep for ValuesStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            let canonical_key = match &t.value {
                GValue::Vertex(vt) => CanonicalKey::Vertex(*vt),
                GValue::Edge(eg) => CanonicalKey::Edge(eg.canonical_edge_key()),
                _ => continue,
            };

            if self.property_keys.is_empty() {
                // TODO: implement fetching all properties if property_keys is empty
                continue;
            }

            let mut results = smallvec![];
            if self.emit_property {
                for key in &self.property_keys {
                    if let Some(value) = ctx.get_property(&canonical_key, key)? {
                        results.push(Traverser::new_rc_with_parent(GValue::Property(value), t.clone()));
                    }
                }
            } else {
                for key in &self.property_keys {
                    if let Some(value) = ctx.get_value(&canonical_key, key)? {
                        results.push(Traverser::new_rc_with_parent(GValue::Scalar(value), t.clone()));
                    }
                }
            }

            if !results.is_empty() {
                return Ok(Some(results));
            }
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
}
