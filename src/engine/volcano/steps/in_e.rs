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

use std::{cell::RefCell, rc::Rc};

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

pub struct InEStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl InEStep {
    pub fn new(label_ids: Vec<LabelId>) -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { upstream: None, label_ids, current_input: None, current_label_idx: 0 }),
        })
    }
}

impl HasBroadcast for InEStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for InEStep {
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

                let in_edges = ctx.get_in_edges(*vk, label).ok().unwrap_or_default();
                let results: SmallVec<[_; 4]> = in_edges.into_iter().map(|e| t.clone_with_edge(e)).collect();

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

impl GremlinStep for InEStep {
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
