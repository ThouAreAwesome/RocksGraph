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

use std::sync::Arc;

use crate::{
    graph::LogicalGraph,
    store::traits::GraphStore,
    types::{
        element::{Edge, Vertex},
        gvalue::Primitive,
        keys::CanonicalKey,
        keys::LabelId,
        keys::VertexKey,
        prop_key::PropKey,
        EdgeKey, StoreError,
    },
};

/// Graph access interface passed through every `BasicStep::next` call.
///
/// Steps receive a `&mut dyn GraphCtx` rather than holding a stored reference,
/// which avoids `Rc<RefCell<…>>` and gives compile-time borrow guarantees.
/// Methods will be added here as step implementations are fleshed out
/// (e.g. `get_out_edges`, `get_property`).
pub trait GraphCtx {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Arc<Vertex>>, StoreError>;
    fn get_edge(&mut self, key: EdgeKey) -> Result<Option<Arc<Edge>>, StoreError>;
    fn get_outs(&mut self, vertex_key: VertexKey) -> Result<Vec<VertexKey>, StoreError>;
    fn get_out_edges(&mut self, vertex_key: VertexKey) -> Result<Vec<EdgeKey>, StoreError>;
    fn get_ins(&mut self, vertex_key: VertexKey) -> Result<Vec<VertexKey>, StoreError>;
    fn get_in_edges(&mut self, vertex_key: VertexKey) -> Result<Vec<EdgeKey>, StoreError>;
    fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError>;
    fn add_edge(&mut self, cek: EdgeKey) -> Result<EdgeKey, StoreError>;
    fn set_property(&mut self, key: CanonicalKey, prop: PropKey, value: Primitive) -> Result<(), StoreError>;
    fn get_property(&mut self, key: CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError>;
}

/// Zero-cost context used in unit tests where no real graph is needed.
pub struct NoopCtx;
impl GraphCtx for NoopCtx {
    fn get_vertex(&mut self, _key: VertexKey) -> Result<Option<Arc<Vertex>>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_vertex".to_string()))
    }
    fn get_edge(&mut self, _key: EdgeKey) -> Result<Option<Arc<Edge>>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_edge".to_string()))
    }
    fn get_outs(&mut self, _vertex_key: VertexKey) -> Result<Vec<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_outs".to_string()))
    }
    fn get_out_edges(&mut self, _vertex_key: VertexKey) -> Result<Vec<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_out_edges".to_string()))
    }
    fn get_ins(&mut self, _vertex_key: VertexKey) -> Result<Vec<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_ins".to_string()))
    }
    fn get_in_edges(&mut self, _vertex_key: VertexKey) -> Result<Vec<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_in_edges".to_string()))
    }
    fn add_vertex(&mut self, _id: VertexKey, _label_id: LabelId) -> Result<VertexKey, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support add_vertex".to_string()))
    }
    fn add_edge(&mut self, _cek: EdgeKey) -> Result<EdgeKey, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support add_edge".to_string()))
    }
    fn set_property(&mut self, _key: CanonicalKey, _prop: PropKey, _value: Primitive) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support set_property".to_string()))
    }
    fn get_property(&mut self, _key: CanonicalKey, _prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_property".to_string()))
    }
}

impl<S: GraphStore> GraphCtx for LogicalGraph<S> {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Arc<Vertex>>, StoreError> {
        LogicalGraph::get_vertex(self, key)
    }
    fn get_edge(&mut self, key: EdgeKey) -> Result<Option<Arc<Edge>>, StoreError> {
        LogicalGraph::get_edge(self, key.canonical_edge_key())
    }
    fn get_outs(&mut self, vertex_key: VertexKey) -> Result<Vec<VertexKey>, StoreError> {
        let edges = self.get_edges(vertex_key, crate::types::Direction::OUT, None, None)?;
        Ok(edges.into_iter().map(|(ek, _)| ek.secondary_id).collect())
    }
    fn get_out_edges(&mut self, vertex_key: VertexKey) -> Result<Vec<EdgeKey>, StoreError> {
        let edges = self.get_edges(vertex_key, crate::types::Direction::OUT, None, None)?;
        Ok(edges.into_iter().map(|(ek, _)| ek).collect())
    }
    fn get_ins(&mut self, vertex_key: VertexKey) -> Result<Vec<VertexKey>, StoreError> {
        let edges = self.get_edges(vertex_key, crate::types::Direction::IN, None, None)?;
        Ok(edges.into_iter().map(|(ek, _)| ek.secondary_id).collect())
    }
    fn get_in_edges(&mut self, vertex_key: VertexKey) -> Result<Vec<EdgeKey>, StoreError> {
        let edges = self.get_edges(vertex_key, crate::types::Direction::IN, None, None)?;
        Ok(edges.into_iter().map(|(ek, _)| ek).collect())
    }
    fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError> {
        let (vertex_key, _vertex_arc) = self.add_vertex(id, label_id)?;
        Ok(vertex_key)
    }
    fn add_edge(&mut self, cek: EdgeKey) -> Result<EdgeKey, StoreError> {
        self.add_edge(cek.canonical_edge_key())?;
        Ok(cek)
    }
    fn set_property(&mut self, key: CanonicalKey, prop: PropKey, value: Primitive) -> Result<(), StoreError> {
        self.set_property(key, prop, value)?;
        Ok(())
    }
    fn get_property(&mut self, key: CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        self.get_property(key, prop)
    }
}
