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
    types::GValue,
};

struct Inner {
    upstream: Option<ConsumerIter>,
    // No specific state needed for otherV, it just transforms edges
}

pub struct OtherVStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl OtherVStep {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { broadcast: RefCell::new(BroadcastState::new()), inner: RefCell::new(Inner { upstream: None }) })
    }
}

impl HasBroadcast for OtherVStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for OtherVStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let inner = self.inner.borrow();
        loop {
            let t = inner.upstream.as_ref()?.next(ctx)?;
            if let GValue::Edge(ek) = &t.value {
                return Some(smallvec![Traverser::new(GValue::Vertex(ek.secondary_id))]);
            } else {
                // If it's not an edge, we can't apply otherV, so we skip it. In a more complete implementation,
                // todo: we might raise an error here, as otherV should only be called on edges. For now, we'll just
                // skip non-edge traversers.
                continue;
            }
        }
    }
}

impl GremlinStep for OtherVStep {
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
