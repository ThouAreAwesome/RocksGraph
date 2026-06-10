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
    types::{element::Property, error::StoreError, gvalue::Primitive, keys::CanonicalKey, prop_key::PropKey, GValue},
};

#[derive(Debug)]
pub struct PropertyStep {
    upstream: Option<StepRef>,
    prop: Property,
}

impl PropertyStep {
    pub fn new(prop_key: PropKey, prop_value: Primitive) -> Self {
        Self { upstream: None, prop: Property { owner: CanonicalKey::Empty, key: prop_key, value: prop_value } }
    }
}

impl CoreStep for PropertyStep {
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
            let mut prop = self.prop.clone();
            prop.owner = canonical_key;
            ctx.set_property(&prop)?;
            return Ok(Some(smallvec![Rc::clone(&t)]));
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
