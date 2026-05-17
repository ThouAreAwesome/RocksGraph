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
use std::{cell::RefCell, collections::VecDeque, rc::Rc, sync::Arc};

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
    // Buffer for results from a single upstream traverser, as 'both' can produce multiple
    buffer: VecDeque<Traverser>,
}

pub struct BothStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl BothStep {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { upstream: None, buffer: VecDeque::new() }),
        })
    }
}

impl HasBroadcast for BothStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for BothStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let mut inner = self.inner.borrow_mut();

        // First, try to drain the buffer
        if let Some(t) = inner.buffer.pop_front() {
            return Some(smallvec![t]);
        }

        // If buffer is empty, get more from upstream
        loop {
            let t = inner.upstream.as_ref()?.next(ctx)?;
            if let GValue::Vertex(vt) = &t.value {
                let out_edges = ctx.get_out_edges(*vt).ok()?;
                let in_edges = ctx.get_in_edges(*vt).ok()?;

                let mut results = SmallVec::new();
                for edge in out_edges {
                    let mut t = Traverser::new(GValue::Vertex(edge.secondary_id));
                    t.parent = Some(Arc::new(t.clone()));
                    results.push(t);
                }
                for edge in in_edges {
                    let mut t = Traverser::new(GValue::Vertex(edge.secondary_id));
                    t.parent = Some(Arc::new(t.clone()));
                    results.push(t);
                }
                if !results.is_empty() {
                    return Some(results);
                }
            } else {
                // todo if it's not a vertex, just pass it through (or should we error?)
                continue;
            }
        }
    }
}

impl GremlinStep for BothStep {
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
