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
        gvalue::Primitive,
        keys::{CanonicalKey, Direction, EdgeKey, LabelId, VertexKey},
        prop_key::PropKey,
        CanonicalEdgeKey, GValue, Property,
    },
};

struct Inner {
    label_id: LabelId,
    out_v_id: VertexKey,
    in_v_id: VertexKey,
    properties: SmallVec<[Property; 8]>,
    emitted: bool, // AddE is a source step that typically emits only once
}

pub struct AddEStep {
    broadcast: RefCell<BroadcastState>,
    inner: RefCell<Inner>,
}

impl AddEStep {
    pub fn new(
        label_id: LabelId,
        out_v_id: VertexKey,
        in_v_id: VertexKey,
        properties: HashMap<PropKey, Primitive>,
    ) -> Rc<Self> {
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property {
                owner: CanonicalKey::Edge(CanonicalEdgeKey {
                    src_id: out_v_id,
                    label_id,
                    dst_id: in_v_id,
                    rank: 0, // Assuming rank 0 for now; can be enhanced to support user-specified ranks
                }),
                key,
                value,
            })
            .collect();
        Rc::new(Self {
            broadcast: RefCell::new(BroadcastState::new()),
            inner: RefCell::new(Inner { label_id, out_v_id, in_v_id, properties, emitted: false }),
        })
    }
}

impl HasBroadcast for AddEStep {
    fn broadcast(&self) -> &RefCell<BroadcastState> {
        &self.broadcast
    }
}

impl Produce for AddEStep {
    fn produce(&self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Traverser; 4]>> {
        let mut inner = self.inner.borrow_mut();
        if inner.emitted {
            return None; // Only emit once
        }

        let edge_key = EdgeKey {
            primary_id: inner.out_v_id,
            direction: Direction::OUT,
            label_id: inner.label_id,
            secondary_id: inner.in_v_id,
            rank: 0,
        };
        let new_edge_arc = ctx.add_edge(edge_key).ok()?;

        for property in inner.properties.drain(..) {
            ctx.set_property(&property).ok()?;
        }

        inner.emitted = true;
        Some(smallvec![Traverser::new(GValue::Edge(new_edge_arc))])
    }
}

impl GremlinStep for AddEStep {
    fn add_upper(&self, _upstream: ConsumerIter) {
        panic!("AddEStep is a source step and cannot have an upstream");
    }

    fn reset(&self) {
        self.broadcast.borrow_mut().reset();
        let mut inner = self.inner.borrow_mut();
        inner.emitted = false;
    }
}
