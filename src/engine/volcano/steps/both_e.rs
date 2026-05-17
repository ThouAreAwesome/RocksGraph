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

use smallvec::SmallVec;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use crate::engine::{
    context::GraphCtx,
    traverser::Traverser, // Assuming these are defined here
    volcano::steps::traits::{BroadcastState, ConsumerIter, GremlinStep, HasBroadcast, Produce},
};
use crate::types::GValue;

struct Inner {
    upstream: Option<ConsumerIter>,
}

pub struct BothEStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl BothEStep {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { broadcast: RefCell::new(BroadcastState::new()), inner: RefCell::new(Inner { upstream: None }) })
    }
}

impl HasBroadcast for BothEStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for BothEStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let inner = self.inner.borrow();

        loop {
            let traverser = inner.upstream.as_ref()?.next(ctx)?;
            if let GValue::Vertex(id) = traverser.value {
                let out_edges = ctx.get_out_edges(id).ok()?;
                let in_edges = ctx.get_in_edges(id).ok()?;

                let mut results = SmallVec::new();
                for edge in out_edges {
                    let mut t = Traverser::new(GValue::Edge(edge));
                    t.parent = Some(Arc::new(traverser.clone()));
                    results.push(t);
                }
                for edge in in_edges {
                    let mut t = Traverser::new(GValue::Edge(edge));
                    t.parent = Some(Arc::new(traverser.clone()));
                    results.push(t);
                }
                if !results.is_empty() {
                    return Some(results);
                }
            } else {
                // If not a vertex, pass it through or filter it out based on Gremlin semantics
            }
        }
    }
}

impl GremlinStep for BothEStep {
    fn add_upper(&self, upstream: ConsumerIter) {
        self.inner.borrow_mut().upstream = Some(upstream);
    }
    fn reset(&self) {
        self.broadcast.borrow_mut().reset();
        let inner = self.inner.borrow();
        if let Some(up) = &inner.upstream {
            up.reset();
        }
    }
}
