// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

use crate::types::PIPELINE_PRODUCE_INLINE;
use std::{collections::HashMap, rc::Rc};

use smallvec::{smallvec, SmallVec};

use crate::engine::volcano::steps::traits::ExplainNode;
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
        GValue, VERTEX_PROPS_INLINE,
    },
};

/// A physical step that adds a new vertex to the graph.
#[derive(Debug)]
pub struct AddVStep {
    // ── Static/Fixed configuration ──
    /// The label ID of the vertex to be created.
    label_id: LabelId,
    /// The designated vertex key.
    vertex_id: VertexKey,
    /// The property list to initialize the new vertex with.
    properties: SmallVec<[Property; VERTEX_PROPS_INLINE]>,

    // ── Dynamic/Runtime execution state ──
    /// Whether the vertex has been successfully created and emitted in this run.
    emitted: bool,
}

/// Creates a new `AddVStep` with the specified vertex details.
impl AddVStep {
    pub fn new(label_id: LabelId, vk: VertexKey, properties: HashMap<u16, Primitive>) -> Self {
        let properties = properties
            .into_iter()
            .map(|(key, value)| Property { owner: CanonicalKey::Vertex(vk), key, value })
            .collect();
        Self { label_id, vertex_id: vk, properties, emitted: false }
    }
}

impl CoreStep for AddVStep {
    fn add_upper(&mut self, _upstream: StepRef) {
        // `AddVStep` is a source step and does not have an upstream.
        panic!("AddVStep is a source step and cannot have an upstream");
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_INLINE]>>, StoreError> {
        // Emits the newly created vertex as a traverser.
        if self.emitted {
            return Ok(None);
        }
        let vk = ctx.add_vertex(self.vertex_id, self.label_id)?;
        for property in &self.properties {
            ctx.set_property(property)?;
        }
        self.emitted = true;
        Ok(Some(smallvec![Traverser::new_rc(GValue::Vertex(vk))]))
    }

    fn reset(&mut self) {
        // Resets the step's state, allowing it to be re-executed.
        self.emitted = false;
    }

    fn explain(&self) -> ExplainNode {
        let params = vec![("label", self.label_id.to_string()), ("id", format!("{:?}", self.vertex_id))];
        ExplainNode::new("AddVStep").with_params(params)
    }
}
