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
use std::{cell::RefCell, collections::HashMap, rc::Rc};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{BroadcastState, ConsumerIter, GremlinStep, HasBroadcast, Produce},
    },
    types::{
        element::Property,
        gvalue::Primitive,
        keys::{CanonicalKey, LabelId, VertexKey},
        prop_key::PropKey,
        GValue,
    },
};

struct Inner {
    label_id: LabelId,
    vertex_id: VertexKey,                // Will be set when the vertex is created
    properties: SmallVec<[Property; 8]>, // Changed to PropKey, Primitive
    emitted: bool,                       // AddV is a source step that typically emits only once
}

pub struct AddVStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl AddVStep {
    pub fn new(label_id: LabelId, vk: VertexKey, properties: HashMap<PropKey, Primitive>) -> Rc<Self> {
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property { owner: CanonicalKey::Vertex(vk), key, value })
            .collect();
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { label_id, vertex_id: vk, properties, emitted: false }),
        })
    }
}

impl HasBroadcast for AddVStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for AddVStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        let mut inner = self.inner.borrow_mut();
        if inner.emitted {
            return None; // Only emit once
        }

        let added_vertex_key = ctx.add_vertex(inner.vertex_id, inner.label_id).ok()?;

        for property in inner.properties.drain(..) {
            ctx.set_property(&property).ok()?;
        }
        inner.emitted = true;
        Some(smallvec![Traverser::new_rc(GValue::Vertex(added_vertex_key))])
    }
}

impl GremlinStep for AddVStep {
    fn add_upper(&self, _upstream: ConsumerIter) {
        panic!("AddVStep is a source step and cannot have an upstream");
    }

    fn reset(&self) {
        self.broadcast.borrow_mut().reset();
        let mut inner = self.inner.borrow_mut();
        inner.emitted = false;
    }
}
