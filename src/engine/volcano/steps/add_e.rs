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

use std::{collections::HashMap, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        error::StoreError,
        gvalue::Primitive,
        keys::{CanonicalKey, Direction, EdgeKey, LabelId, VertexKey},
        prop_key::PropKey,
        CanonicalEdgeKey, GValue, Property,
    },
};

#[derive(Debug)]
pub struct AddEStep {
    label_id: LabelId,
    out_v_id: VertexKey,
    in_v_id: VertexKey,
    properties: SmallVec<[Property; 8]>,
    emitted: bool,
}

impl AddEStep {
    pub fn new(
        label_id: LabelId,
        out_v_id: VertexKey,
        in_v_id: VertexKey,
        properties: HashMap<PropKey, Primitive>,
    ) -> Self {
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property {
                owner: CanonicalKey::Edge(CanonicalEdgeKey { src_id: out_v_id, label_id, dst_id: in_v_id, rank: 0 }),
                key,
                value,
            })
            .collect::<SmallVec<[Property; 8]>>();
        Self { label_id, out_v_id, in_v_id, properties, emitted: false }
    }
}

impl CoreStep for AddEStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("AddEStep is a source step and cannot have an upstream");
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; 4]>>, StoreError> {
        if self.emitted {
            return Ok(None);
        }
        let edge_key = EdgeKey {
            primary_id: self.out_v_id,
            direction: Direction::OUT,
            label_id: self.label_id,
            secondary_id: self.in_v_id,
            rank: 0,
        };
        let new_edge = ctx.add_edge(&edge_key)?;
        for property in &self.properties {
            ctx.set_property(property)?;
        }
        self.emitted = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::Edge(new_edge))]))
    }

    fn reset(&mut self) {
        self.emitted = false;
    }
}
