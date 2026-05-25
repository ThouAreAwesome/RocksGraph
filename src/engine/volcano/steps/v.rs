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

use std::{collections::VecDeque, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{keys::VertexKey, GValue},
};

pub struct VStep {
    vertex_ids: VecDeque<VertexKey>,
    initial_ids: Vec<VertexKey>,
}

impl VStep {
    pub fn new(vertex_ids: Vec<VertexKey>) -> Self {
        Self { vertex_ids: VecDeque::from(vertex_ids.clone()), initial_ids: vertex_ids }
    }
}

impl CoreStep for VStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        panic!("VStep is a source step, it does not have an upstream.");
    }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Option<SmallVec<[Rc<Traverser>; 4]>> {
        let id = self.vertex_ids.pop_front()?;
        let vertex_arc = ctx.get_vertex(id).ok()??;
        Some(smallvec![Traverser::new_rc(GValue::Vertex(vertex_arc.id))])
    }

    fn reset(&mut self) {
        self.vertex_ids = VecDeque::from(self.initial_ids.clone());
    }
}
