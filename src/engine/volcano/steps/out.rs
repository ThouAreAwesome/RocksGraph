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

use std::{cell::RefCell, rc::Rc, sync::Arc};

use smallvec::SmallVec;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{BroadcastState, ConsumerIter, GremlinStep, HasBroadcast, Produce},
    },
    types::{GValue, LabelId},
};

struct Inner {
    upstream: Option<ConsumerIter>,
    label_ids: Vec<LabelId>,
    current_input: Option<Traverser>,
    current_label_idx: usize,
}

pub struct OutStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl OutStep {
    pub fn new(label_ids: Vec<LabelId>) -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { upstream: None, label_ids, current_input: None, current_label_idx: 0 }),
        })
    }
}

impl HasBroadcast for OutStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for OutStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let mut inner = self.inner.borrow_mut();
        loop {
            if inner.current_input.is_none() {
                let t = inner.upstream.as_ref()?.next(ctx)?;
                if matches!(t.value, GValue::Vertex(_)) {
                    inner.current_input = Some(t);
                    inner.current_label_idx = 0;
                } else {
                    continue;
                }
            }

            let t = inner.current_input.as_ref().unwrap().clone();
            if let GValue::Vertex(vk) = &t.value {
                let label =
                    if inner.label_ids.is_empty() { None } else { Some(inner.label_ids[inner.current_label_idx]) };

                let out_edges = ctx.get_out_edges(*vk, label).ok().unwrap_or_default();
                let mut results = SmallVec::new();
                for edge in out_edges {
                    let mut new_t = t.clone();
                    new_t.value = GValue::Vertex(edge.secondary_id);
                    new_t.parent = Some(Arc::new(t.clone()));
                    results.push(new_t);
                }

                inner.current_label_idx += 1;
                if inner.label_ids.is_empty() || inner.current_label_idx >= inner.label_ids.len() {
                    inner.current_input = None;
                }
                if !results.is_empty() {
                    return Some(results);
                }
            } else {
                inner.current_input = None;
            }
        }
    }
}

impl GremlinStep for OutStep {
    fn add_upper(&self, upstream: ConsumerIter) {
        self.inner.borrow_mut().upstream = Some(upstream);
    }
    fn reset(&self) {
        self.broadcast.borrow_mut().reset();
        let mut inner = self.inner.borrow_mut();
        if let Some(up) = &inner.upstream {
            up.reset();
        }
        inner.current_input = None;
        inner.current_label_idx = 0;
    }
}
