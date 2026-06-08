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

use smallvec::SmallVec;
use std::collections::VecDeque;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{
        element::Property,
        error::StoreError,
        gvalue::Primitive,
        keys::{CanonicalKey, LabelId, VertexKey},
        prop_key::PropKey,
        GValue,
    },
};

pub struct AddVStep {
    label_id: LabelId,
    vertex_id: VertexKey,
    properties: SmallVec<[Property; 8]>,
    emitted: bool,
}

impl AddVStep {
    pub fn new(label_id: LabelId, vk: VertexKey, properties: HashMap<PropKey, Primitive>) -> Self {
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property { owner: CanonicalKey::Vertex(vk), key, value })
            .collect();
        Self { label_id, vertex_id: vk, properties, emitted: false }
    }
}

impl CoreStep for AddVStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("AddVStep is a source step and cannot have an upstream");
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx, buffer: &mut VecDeque<Rc<Traverser>>) -> Result<bool, StoreError> {
        if self.emitted {
            return Ok(false);
        }
        let vk = ctx.add_vertex(self.vertex_id, self.label_id)?;
        for property in &self.properties {
            ctx.set_property(property)?;
        }
        self.emitted = true;
        buffer.push_back(Traverser::new_rc(GValue::Vertex(vk)));
        Ok(true)
    }

    fn reset(&mut self) {
        self.emitted = false;
    }
}
