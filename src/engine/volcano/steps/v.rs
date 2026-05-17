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
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{BroadcastState, ConsumerIter, GremlinStep, HasBroadcast, Produce},
    },
    types::{keys::VertexKey, GValue},
};

struct Inner {
    vertex_ids: VecDeque<VertexKey>, // IDs to emit
    initial_ids: Vec<VertexKey>,     // To reset
}

pub struct VStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl VStep {
    pub fn new(vertex_ids: Vec<VertexKey>) -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { vertex_ids: VecDeque::from(vertex_ids.clone()), initial_ids: vertex_ids }),
        })
    }
}

impl HasBroadcast for VStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for VStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let mut inner = self.inner.borrow_mut();
        if let Some(id) = inner.vertex_ids.pop_front() {
            if let Some(vertex_arc) = ctx.get_vertex(id).ok()? {
                return Some(smallvec![Traverser::new(GValue::Vertex(vertex_arc.id))]);
            }
        }
        None
    }
}

impl GremlinStep for VStep {
    fn add_upper(&self, _upstream: ConsumerIter) {
        panic!("VStep is a source step, it does not have an upstream.");
    }
    fn reset(&self) {
        self.broadcast.borrow_mut().reset();
        let mut inner = self.inner.borrow_mut();
        inner.vertex_ids = VecDeque::from(inner.initial_ids.clone());
    }
}
