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
    types::{keys::LabelId, GValue},
};

struct Inner {
    upstream: Option<ConsumerIter>,
    label_id: LabelId,
}

pub struct HasLabelStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl HasLabelStep {
    pub fn new(label_id: LabelId) -> Rc<Self> {
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { upstream: None, label_id }),
        })
    }
}

impl HasBroadcast for HasLabelStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for HasLabelStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let inner = self.inner.borrow();
        loop {
            let t = inner.upstream.as_ref()?.next(ctx)?;
            let matched = match &t.value {
                GValue::Vertex(v_arc) => {
                    let vertex = ctx.get_vertex(*v_arc).ok()??;
                    vertex.label_id == inner.label_id
                }
                GValue::Edge(e_arc) => e_arc.label_id == inner.label_id,
                _ => false,
            };
            if matched {
                return Some(smallvec![t]);
            }
        }
    }
}

impl GremlinStep for HasLabelStep {
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
