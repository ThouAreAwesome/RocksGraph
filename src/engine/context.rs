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

use crate::{
    graph::LogicalGraph,
    store::traits::GraphStore,
    types::{
        element::Property,
        gvalue::Primitive,
        keys::{CanonicalKey, LabelId, VertexKey},
        prop_key::PropKey,
        EdgeKey, StoreError,
    },
};

/// Graph access interface passed through every `BasicStep::next` call.
///
/// Steps receive a `&mut dyn GraphCtx` rather than holding a stored reference,
/// which avoids `Rc<RefCell<…>>` and gives compile-time borrow guarantees.
///
/// # Existence checks and locking — contract summary
///
/// The trait itself is a thin facade; the real invariants live in
/// `LogicalGraph`.  See `graph.rs` for the authoritative details.  The table
/// below gives a quick reference for callers of this trait.
///
/// **Not every method is precondition-free.**  All vertex-targeting operations
/// auto-load from the store when the overlay is cold.  Edge operations (other
/// than `get_edge` / `get_edges`) are overlay-only — the caller must call
/// `get_edge` or `get_edges` first.
///
/// | Method                   | Store fallback (vertex) | Store fallback (edge) | Lock acquired            | Precondition                  |
/// |--------------------------|:-----------------------:|:---------------------:|:------------------------:|-------------------------------|
/// | `get_vertex`             | ✅ on miss               | n/a                   | none                     | none                          |
/// | `get_edge`               | n/a                     | ✅ on miss             | none                     | none                          |
/// | `get_outs` / `get_ins`   | n/a                     | ✅ merged              | none                     | none                          |
/// | `get_property` (vertex)  | ✅ via `get_vertex`      | —                     | `RwLock::read` on props  | none                          |
/// | `get_property` (edge)    | —                       | ✗ overlay-only        | `RwLock::read` on props  | ⚠ **edge must be pre-loaded** |
/// | `get_value` (vertex)     | ✅ via `get_vertex`      | —                     | `RwLock::read` on props  | none                          |
/// | `get_value` (edge)       | —                       | ✗ overlay-only        | `RwLock::read` on props  | ⚠ **edge must be pre-loaded** |
/// | `add_vertex`             | ✅ via degree record     | n/a                   | none                     | none                          |
/// | `add_edge`               | ✅ via degree record     | ✅ via OUT record      | none                     | none                          |
/// | `set_property` (vertex)  | ✅ auto-load             | —                     | `RwLock::write` on props | none                          |
/// | `set_property` (edge)    | —                       | ✗ overlay-only        | `RwLock::write` on props | ⚠ **edge must be pre-loaded** |
/// | `drop_property` (vertex) | ✅ auto-load             | —                     | `RwLock::write` on props | none                          |
/// | `drop_property` (edge)   | —                       | ✗ overlay-only        | `RwLock::write` on props | ⚠ **edge must be pre-loaded** |
pub trait GraphCtx {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError>;
    fn get_edge(&mut self, key: EdgeKey) -> Result<Option<EdgeKey>, StoreError>;
    fn get_outs(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError>;
    fn get_out_edges(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError>;
    fn get_ins(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError>;
    fn get_in_edges(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError>;
    fn get_property(&mut self, key: CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError>;
    fn get_value(&mut self, key: CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError>;
    /// Insert a vertex.  See `LogicalGraph::add_vertex` for existence-check and
    /// locking details.
    fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError>;
    /// Insert an edge.  Both endpoint vertices must already exist (overlay or
    /// store).  See `LogicalGraph::add_edge` for existence-check and locking
    /// details.
    fn add_edge(&mut self, cek: EdgeKey) -> Result<EdgeKey, StoreError>;
    /// Upsert a property.  For vertices the element is auto-loaded from the store
    /// if absent from the overlay (no precondition).  For edges the edge must
    /// already be in the overlay — call `get_edge` first.  Acquires
    /// `RwLock::write` on `props` briefly.
    fn set_property(&mut self, prop: &Property) -> Result<(), StoreError>;
    fn drop_property(&mut self, prop: &Property) -> Result<(), StoreError>;
    fn drop_vertex(&mut self, vertex: VertexKey) -> Result<(), StoreError>;
    fn drop_edge(&mut self, edge: EdgeKey) -> Result<(), StoreError>;
}

/// Zero-cost context used in unit tests where no real graph is needed.
pub struct NoopCtx;
impl GraphCtx for NoopCtx {
    fn get_vertex(&mut self, _key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_vertex".to_string()))
    }
    fn get_edge(&mut self, _key: EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_edge".to_string()))
    }
    fn get_outs(
        &mut self,
        _vertex_key: VertexKey,
        _label: Option<LabelId>,
        _limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_outs".to_string()))
    }
    fn get_out_edges(
        &mut self,
        _vertex_key: VertexKey,
        _label: Option<LabelId>,
        _end_vertex_ids: Option<&[VertexKey]>,
        _limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_out_edges".to_string()))
    }
    fn get_ins(
        &mut self,
        _vertex_key: VertexKey,
        _label: Option<LabelId>,
        _limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_ins".to_string()))
    }
    fn get_in_edges(
        &mut self,
        _vertex_key: VertexKey,
        _label: Option<LabelId>,
        _end_vertex_ids: Option<&[VertexKey]>,
        _limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_in_edges".to_string()))
    }
    fn get_property(&mut self, _key: CanonicalKey, _prop: &PropKey) -> Result<Option<Property>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_property".to_string()))
    }
    fn get_value(&mut self, _key: CanonicalKey, _prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_value".to_string()))
    }
    fn add_vertex(&mut self, _id: VertexKey, _label_id: LabelId) -> Result<VertexKey, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support add_vertex".to_string()))
    }
    fn add_edge(&mut self, _cek: EdgeKey) -> Result<EdgeKey, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support add_edge".to_string()))
    }
    fn set_property(&mut self, _prop: &Property) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support set_property".to_string()))
    }
    fn drop_property(&mut self, _prop: &Property) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support drop_property".to_string()))
    }
    fn drop_vertex(&mut self, _vk: VertexKey) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support drop_vertex".to_string()))
    }
    fn drop_edge(&mut self, _ek: EdgeKey) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support drop_edge".to_string()))
    }
}

impl<S: GraphStore> GraphCtx for LogicalGraph<S> {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        LogicalGraph::get_vertex(self, key)
    }
    fn get_edge(&mut self, key: EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        LogicalGraph::get_edge(self, key.canonical_edge_key())
    }
    fn get_outs(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        let edges = self.get_edges(vertex_key, crate::types::Direction::OUT, label, None, limit)?;
        Ok(edges.into_iter().map(|ek| ek.secondary_id).collect())
    }
    fn get_out_edges(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError> {
        self.get_edges(vertex_key, crate::types::Direction::OUT, label, end_vertex_ids, limit)
    }
    fn get_ins(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        let edges = self.get_edges(vertex_key, crate::types::Direction::IN, label, None, limit)?;
        Ok(edges.into_iter().map(|ek| ek.secondary_id).collect())
    }
    fn get_in_edges(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError> {
        self.get_edges(vertex_key, crate::types::Direction::IN, label, end_vertex_ids, limit)
    }
    fn get_property(&mut self, key: CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError> {
        self.get_property(key, prop)
    }
    fn get_value(&mut self, key: CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        self.get_value(key, prop)
    }
    fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError> {
        self.add_vertex(id, label_id)
    }
    fn add_edge(&mut self, cek: EdgeKey) -> Result<EdgeKey, StoreError> {
        self.add_edge(cek.canonical_edge_key())?;
        Ok(cek)
    }
    fn set_property(&mut self, prop: &Property) -> Result<(), StoreError> {
        self.set_property(prop)?;
        Ok(())
    }
    fn drop_property(&mut self, prop: &Property) -> Result<(), StoreError> {
        self.drop_property(prop)
    }

    fn drop_vertex(&mut self, vertex: VertexKey) -> Result<(), StoreError> {
        self.drop_element(CanonicalKey::Vertex(vertex))
    }
    fn drop_edge(&mut self, edge: EdgeKey) -> Result<(), StoreError> {
        self.drop_element(CanonicalKey::Edge(edge.canonical_edge_key()))
    }
}
