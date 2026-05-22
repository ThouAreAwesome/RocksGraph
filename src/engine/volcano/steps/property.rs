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

use smallvec::{smallvec, SmallVec};
use std::{cell::RefCell, rc::Rc};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{BroadcastState, ConsumerIter, GremlinStep, HasBroadcast, Produce},
    },
    types::{element::Property, gvalue::Primitive, keys::CanonicalKey, prop_key::PropKey, GValue},
};

struct Inner {
    upstream: Option<ConsumerIter>,
    prop: Property,
}

pub struct PropertyStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl PropertyStep {
    pub fn new(prop_key: PropKey, prop_value: Primitive) -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner {
                upstream: None,
                prop: Property { owner: CanonicalKey::Empty, key: prop_key, value: prop_value },
            }),
        })
    }
}

impl HasBroadcast for PropertyStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for PropertyStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let inner = self.inner.borrow();
        loop {
            let t = inner.upstream.as_ref()?.next(ctx)?;
            let canonical_key = match &t.value {
                GValue::Vertex(v_arc) => CanonicalKey::Vertex(*v_arc),
                GValue::Edge(e_arc) => CanonicalKey::Edge(e_arc.canonical_edge_key()),
                _ => {
                    // If the traverser is not a vertex or edge, we should raise an error. For now, we'll just skip it.
                    continue;
                }
            };
            let mut prop = inner.prop.clone();
            prop.owner = canonical_key;
            ctx.set_property(&prop).ok()?;
            return Some(smallvec![t]);
        }
    }
}

impl GremlinStep for PropertyStep {
    fn add_upper(&self, upstream: ConsumerIter) {
        self.inner.borrow_mut().upstream = Some(upstream);
    }

    fn reset(&self) {
        self.broadcast.borrow_mut().reset();
        if let Some(up) = &self.inner.borrow().upstream {
            up.reset();
        }
    }
}
