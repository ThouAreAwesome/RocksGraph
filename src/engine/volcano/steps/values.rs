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
    types::{keys::CanonicalKey, prop_key::PropKey, GValue},
};

struct Inner {
    upstream: Option<ConsumerIter>,
    property_keys: Vec<PropKey>,
}

pub struct ValuesStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl ValuesStep {
    pub fn new(property_keys: Vec<PropKey>) -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { upstream: None, property_keys }),
        })
    }
}

impl HasBroadcast for ValuesStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for ValuesStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        let inner = self.inner.borrow();
        loop {
            let t = inner.upstream.as_ref()?.next(ctx)?;
            let canonical_key = match &t.value {
                GValue::Vertex(v_arc) => CanonicalKey::Vertex(*v_arc),
                GValue::Edge(e_arc) => CanonicalKey::Edge(e_arc.canonical_edge_key()),
                // todo: better to raise an error if it's not a vertex or edge, as values() should only be called on
                // elements. For now, we'll just skip non-element traversers.
                _ => continue, // Only process vertices and edges
            };

            let mut results = smallvec![];
            if inner.property_keys.is_empty() {
                // If no specific keys are given, return all values (this is more complex, requires iterating properties
                // on the element itself) For now, let's skip this case or return an error if no keys
                // are specified, as GraphCtx::get_property requires a specific key.
                // A proper implementation would need to fetch the element and iterate its properties.
                // For simplicity, let's assume property_keys is never empty for now, or handle it as a no-op.
                // todo: implement fetching all properties if property_keys is empty.
                continue;
            } else {
                for key in &inner.property_keys {
                    if let Some(value) = ctx.get_property(canonical_key, key).ok()? {
                        results.push(Traverser::new_rc(GValue::Scalar(value)));
                    }
                }
            }
            if !results.is_empty() {
                return Some(results);
            }
        }
    }
}

impl GremlinStep for ValuesStep {
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
