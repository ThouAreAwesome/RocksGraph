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
    types::{error::StoreError, gvalue::Primitive, prop_key::PropKey, CanonicalKey, GValue},
};

pub struct HasPropertyStep {
    upstream: Option<StepRef>,
    prop_key: PropKey,
    expected_value: Primitive,
}

impl HasPropertyStep {
    pub fn new(prop_key: PropKey, expected_value: Primitive) -> Self {
        Self { upstream: None, prop_key, expected_value }
    }
}

impl CoreStep for HasPropertyStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        loop {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let Some(t) = upstream.next(ctx)? else { return Ok(None) };
            match &t.value {
                GValue::Vertex(vk) => {
                    if let Some(vl) = ctx.get_value(CanonicalKey::Vertex(*vk), &self.prop_key)? {
                        if vl == self.expected_value {
                            return Ok(Some(smallvec![Rc::clone(&t)]));
                        }
                    }
                }
                GValue::Edge(ek) => {
                    if let Some(et) = ctx.get_value(CanonicalKey::Edge(ek.canonical_edge_key()), &self.prop_key)? {
                        if et == self.expected_value {
                            return Ok(Some(smallvec![Rc::clone(&t)]));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn reset(&mut self) {
        if let Some(up) = &self.upstream {
            up.reset();
        }
    }
}
