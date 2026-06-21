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

use crate::{
    graph::{LogicalGraph, LogicalSnapshot},
    store::traits::GraphStore,
    types::{
        element::Property,
        gvalue::Primitive,
        keys::{AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, LabelId, VertexKey},
        prop_key::PropKey,
        Direction, EdgeKey, StoreError,
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
/// than `get_edge` / `get_adjacent_edges`) are overlay-only — the caller must call
/// `get_edge` or `get_edges` first.
///
/// | Method                   | Store fallback (vertex) | Store fallback (edge) | Lock acquired  | Precondition                  |
/// |--------------------------|:-----------------------:|:---------------------:|:--------------:|-------------------------------|
/// | `get_vertex`             | ✅ on miss               | n/a                   | none          | none                          |
/// | `get_edge`               | n/a                     | ✅ on miss             | none          | none                          |
/// | `get_adjacent_xxx`       | n/a                     | ✅ merged              | none          | none                          |
/// | `get_property` (vertex)  | ✅ via `get_vertex`      | n/a                   | none          | none                          |
/// | `get_property` (edge)    | —                       | ✗ overlay-only        | none           | ⚠ **edge must be pre-loaded** |
/// | `get_value` (vertex)     | ✅ via `get_vertex`      | —                     | none          | none                          |
/// | `get_value` (edge)       | —                       | ✗ overlay-only        | none           | ⚠ **edge must be pre-loaded** |
/// | `add_vertex`             | ✅ via degree record     | n/a                   | none          | none                          |
/// | `add_edge`               | ✅ via degree record     | ✅ via OUT record     | none           | none                         |
/// | `set_property` (vertex)  | ✅ auto-load             | —                     | none          | none                          |
/// | `set_property` (edge)    | —                       | ✗ overlay-only        | none           | ⚠ **edge must be pre-loaded** |
/// | `drop_property` (vertex) | ✅ auto-load             | n/a                   | none          | none                          |
/// | `drop_property` (edge)   | —                       | ✗ overlay-only        | none           | ⚠ **edge must be pre-loaded** |
pub trait GraphCtx {
    /// Retrieves a vertex by its key.
    #[allow(dead_code)]
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError>;
    /// Retrieves multiple vertices in batch, caching them in overlay.
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError>;
    /// Retrieves an edge by its key.
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError>;
    /// Retrieves multiple edges in batch, caching them in overlay.
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError>;

    fn get_adjacent_vertices(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        direction: Direction,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError>;

    /// Retrieves adjacent edges for a given vertex, filtered by label, direction, and optional destination
    /// vertices with support for pagination.
    fn get_adjacent_edges(
        &mut self,
        vertex_key: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<EdgeKey>, Option<AdjacentEdgeCursor>), StoreError>;

    /// Scan all vertices in the database in batch mode.
    fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError>;

    /// Scan all unique canonical edges in the database in batch mode.
    fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<CanonicalEdgeKey>), StoreError>;

    /// Retrieves a property for a given canonical key (vertex or edge) and property key.
    fn get_property(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError>;
    /// Retrieves the primitive value of a property for a given canonical key and property key.
    fn get_value(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError>;
    /// Insert a vertex.
    fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError>;
    /// Insert an edge.
    fn add_edge(&mut self, cek: &EdgeKey) -> Result<EdgeKey, StoreError>;
    /// Sets a property on a vertex or an edge.
    fn set_property(&mut self, prop: &Property) -> Result<(), StoreError>;
    /// Drops a property from a vertex or an edge.
    fn drop_property(&mut self, prop: &Property) -> Result<(), StoreError>;
    /// Drops a vertex from the graph.
    fn drop_vertex(&mut self, vertex: VertexKey) -> Result<(), StoreError>;
    /// Drops an edge from the graph.
    fn drop_edge(&mut self, edge: &EdgeKey) -> Result<(), StoreError>;

    /// Returns the `label_id` and all stored properties of a vertex or edge as owned scalars.
    #[allow(clippy::type_complexity)]
    fn get_all_props(&mut self, key: &CanonicalKey)
        -> Result<Option<(LabelId, Vec<(PropKey, Primitive)>)>, StoreError>;
}

/// Zero-cost context used in unit tests where no real graph is needed.
#[cfg(test)]
pub struct NoopCtx;
#[cfg(test)]
impl GraphCtx for NoopCtx {
    fn get_vertex(&mut self, _key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_vertex".to_string()))
    }
    fn get_vertices(&mut self, _keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_vertices".to_string()))
    }
    fn get_edge(&mut self, _key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_edge".to_string()))
    }
    fn get_edges(&mut self, _keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_edges".to_string()))
    }
    fn get_adjacent_vertices(
        &mut self,
        _vertex_key: VertexKey,
        _label: Option<LabelId>,
        _direction: Direction,
        _end_vertex_ids: Option<&[VertexKey]>,
        _limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_adjacent_vertices".to_string()))
    }
    fn get_adjacent_edges(
        &mut self,
        _vertex_key: VertexKey,
        _direction: Direction,
        _opts: AdjacentEdgesOptions<'_>,
        _limit: Option<u32>,
    ) -> Result<(Vec<EdgeKey>, Option<AdjacentEdgeCursor>), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_adjacent_edges".to_string()))
    }
    fn scan_vertices(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<VertexKey>,
        _limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support scan_vertices".to_string()))
    }
    fn scan_edges(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<CanonicalEdgeKey>,
        _limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<CanonicalEdgeKey>), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support scan_edges".to_string()))
    }
    fn get_property(&mut self, _key: &CanonicalKey, _prop: &PropKey) -> Result<Option<Property>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_property".to_string()))
    }
    fn get_value(&mut self, _key: &CanonicalKey, _prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_value".to_string()))
    }
    fn add_vertex(&mut self, _id: VertexKey, _label_id: LabelId) -> Result<VertexKey, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support add_vertex".to_string()))
    }
    fn add_edge(&mut self, _cek: &EdgeKey) -> Result<EdgeKey, StoreError> {
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
    fn drop_edge(&mut self, _ek: &EdgeKey) -> Result<(), StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support drop_edge".to_string()))
    }
    #[allow(clippy::type_complexity)]
    fn get_all_props(
        &mut self,
        _key: &CanonicalKey,
    ) -> Result<Option<(LabelId, Vec<(PropKey, Primitive)>)>, StoreError> {
        Err(StoreError::UnsupportedOperation("NoopCtx does not support get_all_props".to_string()))
    }
}

impl<S: GraphStore> GraphCtx for LogicalGraph<S> {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        self.get_vertex(key)
    }
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        self.get_vertices(keys)
    }
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        self.get_edge(key)
    }
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
        self.get_edges(keys)
    }
    fn get_adjacent_vertices(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        direction: Direction,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        let opts = AdjacentEdgesOptions { label, dst: end_vertex_ids, rank: None, start_from: None };
        let (edges, _) = self.get_adjacent_edges(vertex_key, direction, opts, limit)?;
        Ok(edges.into_iter().map(|ek| ek.secondary_id).collect())
    }
    fn get_adjacent_edges(
        &mut self,
        vertex_key: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<EdgeKey>, Option<AdjacentEdgeCursor>), StoreError> {
        self.get_adjacent_edges(vertex_key, direction, opts, limit)
    }
    fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        self.scan_vertices(label, start_from, limit)
    }
    fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<CanonicalEdgeKey>), StoreError> {
        self.scan_edges(label, start_from, limit)
    }
    fn get_property(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError> {
        self.get_property(key, prop)
    }
    fn get_value(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        self.get_value(key, prop)
    }
    fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError> {
        self.add_vertex(id, label_id)
    }
    fn add_edge(&mut self, ek: &EdgeKey) -> Result<EdgeKey, StoreError> {
        self.add_edge(ek)
    }
    fn set_property(&mut self, prop: &Property) -> Result<(), StoreError> {
        self.set_property(prop)
    }
    fn drop_property(&mut self, prop: &Property) -> Result<(), StoreError> {
        self.drop_property(prop)
    }
    fn drop_vertex(&mut self, vertex: VertexKey) -> Result<(), StoreError> {
        self.drop_element(&CanonicalKey::Vertex(vertex))
    }
    fn drop_edge(&mut self, edge: &EdgeKey) -> Result<(), StoreError> {
        self.drop_element(&CanonicalKey::Edge(edge.canonical_edge_key()))
    }
    #[allow(clippy::type_complexity)]
    fn get_all_props(
        &mut self,
        key: &CanonicalKey,
    ) -> Result<Option<(LabelId, Vec<(PropKey, Primitive)>)>, StoreError> {
        self.get_all_props(key)
    }
}

impl<S: GraphStore> GraphCtx for LogicalSnapshot<S> {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        self.get_vertex(key)
    }
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        self.get_vertices(keys)
    }
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        self.get_edge(key)
    }
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
        self.get_edges(keys)
    }
    fn get_adjacent_vertices(
        &mut self,
        vertex_key: VertexKey,
        label: Option<LabelId>,
        direction: Direction,
        end_vertex_ids: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<VertexKey>, StoreError> {
        let opts = AdjacentEdgesOptions { label, dst: end_vertex_ids, rank: None, start_from: None };
        let (edges, _) = self.get_adjacent_edges(vertex_key, direction, opts, limit)?;
        Ok(edges.into_iter().map(|ek| ek.secondary_id).collect())
    }
    fn get_adjacent_edges(
        &mut self,
        vertex_key: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<EdgeKey>, Option<AdjacentEdgeCursor>), StoreError> {
        self.get_adjacent_edges(vertex_key, direction, opts, limit)
    }
    fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        self.scan_vertices(label, start_from, limit)
    }
    fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<CanonicalEdgeKey>), StoreError> {
        self.scan_edges(label, start_from, limit)
    }
    fn get_property(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError> {
        self.get_property(key, prop)
    }
    fn get_value(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        self.get_value(key, prop)
    }
    fn add_vertex(&mut self, _id: VertexKey, _label_id: LabelId) -> Result<VertexKey, StoreError> {
        Err(StoreError::ReadOnly)
    }
    fn add_edge(&mut self, _cek: &EdgeKey) -> Result<EdgeKey, StoreError> {
        Err(StoreError::ReadOnly)
    }
    fn set_property(&mut self, _prop: &Property) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }
    fn drop_property(&mut self, _prop: &Property) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }
    fn drop_vertex(&mut self, _vertex: VertexKey) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }
    fn drop_edge(&mut self, _edge: &EdgeKey) -> Result<(), StoreError> {
        Err(StoreError::ReadOnly)
    }
    #[allow(clippy::type_complexity)]
    fn get_all_props(
        &mut self,
        key: &CanonicalKey,
    ) -> Result<Option<(LabelId, Vec<(PropKey, Primitive)>)>, StoreError> {
        self.get_all_props(key)
    }
}
