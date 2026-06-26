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
    store::traits::{GraphSnapshot, GraphStore},
    types::{
        element::{Edge, Property, Vertex},
        keys::{
            AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId,
            VertexKey,
        },
        prop_key::PropKey,
        prop_key::{ID_KEY_ID, LABEL_KEY_ID},
        Primitive, StoreError,
    },
};
use std::collections::{hash_map::Entry, HashMap};

use super::ScanConfig;

// ── LogicalSnapshot ───────────────────────────────────────────────────────────

/// Read-only query context backed by a [`GraphSnapshot`].
///
/// Like `LogicalGraph` it maintains `vertices` and `edges` caches
/// (with the same vertex-label-cache side effects on edge reads)
/// so repeated reads within one traversal are O(1) map lookups.
/// Unlike `LogicalGraph` there is no dirty map and no write path — mutations
/// are rejected at the [`GraphCtx`] boundary with [`StoreError::ReadOnly`].
pub(crate) struct LogicalSnapshot<S: GraphStore> {
    store: S::Snapshot,
    vertices: HashMap<VertexKey, Vertex>,
    edges: HashMap<CanonicalEdgeKey, Edge>,
    pub(crate) scan_config: ScanConfig,
    pub(crate) schema: std::sync::Arc<std::sync::RwLock<crate::schema::Schema>>,
}

impl<S: GraphStore> LogicalSnapshot<S> {
    pub fn new(snapshot: S::Snapshot, schema: std::sync::Arc<std::sync::RwLock<crate::schema::Schema>>) -> Self {
        Self {
            store: snapshot,
            vertices: HashMap::new(),
            edges: HashMap::new(),
            scan_config: ScanConfig::default(),
            schema,
        }
    }

    /// Reset the in-memory vertex and edge caches.
    ///
    /// Called by `ReadSession::g()` before each traversal so that caches stay
    /// scoped to a single traversal rather than growing across all `g()` calls
    /// on the same session. The underlying RocksDB snapshot is not affected —
    /// all traversals on the same session still see the same consistent view.
    pub(crate) fn clear_caches(&mut self) {
        self.vertices.clear();
        self.edges.clear();
    }

    fn cache_vertex_label(&mut self, id: VertexKey, label_id: LabelId) {
        self.vertices.entry(id).or_insert_with(|| Vertex::label_only(id, label_id));
    }

    fn ensure_vertex_props_loaded(&mut self, id: VertexKey) -> Result<(), StoreError> {
        if self.vertices.get(&id).is_some_and(Vertex::is_label_only) {
            match self.store.get_vertex(id)? {
                Some(fresh) => {
                    self.vertices.insert(id, fresh);
                }
                None => return Err(StoreError::CorruptData("vertex missing for LabelOnly cache entry")),
            }
        }
        Ok(())
    }

    pub(crate) fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        if !self.vertices.contains_key(&key) {
            match self.store.get_vertex(key)? {
                None => return Ok(None),
                Some(vt) => {
                    self.vertices.insert(key, vt);
                }
            }
        }
        Ok(Some(key))
    }

    pub(crate) fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        let cek = key.canonical_edge_key();
        if !self.edges.contains_key(&cek) {
            match self.store.get_edge(key)? {
                None => return Ok(None),
                Some(eg) => {
                    if let Some(l) = eg.src_label {
                        self.cache_vertex_label(eg.src_id, l);
                    }
                    if let Some(l) = eg.dst_label {
                        self.cache_vertex_label(eg.dst_id, l);
                    }
                    self.edges.insert(cek, eg);
                }
            }
        }
        Ok(Some(*key))
    }

    pub(crate) fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        let mut missing_keys = Vec::new();
        for &k in keys {
            if !self.vertices.contains_key(&k) {
                missing_keys.push(k);
            }
        }
        if !missing_keys.is_empty() {
            let fetched = self.store.get_vertices(&missing_keys)?;
            for vt in fetched {
                self.vertices.insert(vt.id, vt);
            }
        }
        let mut result = Vec::new();
        for &k in keys {
            if self.vertices.contains_key(&k) {
                result.push(k);
            }
        }
        Ok(result)
    }

    pub(crate) fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
        let mut missing_keys = Vec::new();
        for k in keys {
            let cek = k.canonical_edge_key();
            if !self.edges.contains_key(&cek) {
                missing_keys.push(*k);
            }
        }
        if !missing_keys.is_empty() {
            let fetched = self.store.get_edges(&missing_keys)?;
            for eg in fetched {
                if let Some(l) = eg.src_label {
                    self.cache_vertex_label(eg.src_id, l);
                }
                if let Some(l) = eg.dst_label {
                    self.cache_vertex_label(eg.dst_id, l);
                }
                self.edges.insert(eg.canonical_key(), eg);
            }
        }
        let mut result = Vec::new();
        for k in keys {
            let cek = k.canonical_edge_key();
            if self.edges.contains_key(&cek) {
                result.push(*k);
            }
        }
        Ok(result)
    }

    pub(crate) fn get_adjacent_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<EdgeKey>, Option<AdjacentEdgeCursor>), StoreError> {
        let (committed, cursor) = self.store.get_adjacent_edges(vertex, direction, opts, limit)?;
        let mut result = Vec::with_capacity(committed.len());
        for edge in committed {
            if let Some(l) = edge.src_label {
                self.cache_vertex_label(edge.src_id, l);
            }
            if let Some(l) = edge.dst_label {
                self.cache_vertex_label(edge.dst_id, l);
            }
            let cek = edge.canonical_key();
            result.push(match direction {
                Direction::OUT => cek.out_key(),
                Direction::IN => cek.in_key(),
            });
            self.edges.entry(cek).or_insert(edge);
        }
        Ok((result, cursor))
    }

    pub(crate) fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        let (committed, cursor) = self.store.scan_vertices(label, start_from, limit)?;
        let mut result = Vec::with_capacity(committed.len());
        for vt in committed {
            result.push(vt.id);
            match self.vertices.entry(vt.id) {
                Entry::Vacant(e) => {
                    e.insert(vt);
                }
                Entry::Occupied(mut e) if e.get().is_label_only() => {
                    e.insert(vt);
                }
                Entry::Occupied(_) => {}
            }
        }
        Ok((result, cursor))
    }

    pub(crate) fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<CanonicalEdgeKey>), StoreError> {
        let (committed, cursor) = self.store.scan_edges(label, start_from, limit)?;
        let mut result = Vec::with_capacity(committed.len());
        for edge in committed {
            if let Some(l) = edge.src_label {
                self.cache_vertex_label(edge.src_id, l);
            }
            if let Some(l) = edge.dst_label {
                self.cache_vertex_label(edge.dst_id, l);
            }
            let cek = edge.canonical_key();
            result.push(cek.out_key());
            self.edges.entry(cek).or_insert(edge);
        }
        Ok((result, cursor))
    }

    pub(crate) fn get_property(
        &mut self,
        key: &CanonicalKey,
        prop_key_id: u16,
    ) -> Result<Option<Property>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk)?.is_some() {
                    if prop_key_id != ID_KEY_ID && prop_key_id != LABEL_KEY_ID {
                        self.ensure_vertex_props_loaded(vk)?;
                    }
                    Ok(self.vertices.get_mut(&vk).unwrap().get_property(prop_key_id))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => Ok(self.edges.get_mut(&ek).and_then(|eg| eg.get_property(prop_key_id))),
            CanonicalKey::Empty => Err(StoreError::TraversalError("Property owner cannot be empty".to_string())),
        }
    }

    pub(crate) fn get_value(&mut self, key: &CanonicalKey, prop_key_id: u16) -> Result<Option<Primitive>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk)?.is_some() {
                    if prop_key_id != ID_KEY_ID && prop_key_id != LABEL_KEY_ID {
                        self.ensure_vertex_props_loaded(vk)?;
                    }
                    Ok(self.vertices.get_mut(&vk).unwrap().get_value(prop_key_id))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => Ok(self.edges.get_mut(&ek).and_then(|eg| eg.get_value(prop_key_id))),
            CanonicalKey::Empty => {
                Err(StoreError::UnexpectedDataType("expected Vertex or Edge for get property value".to_string()))
            }
        }
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn get_all_props(
        &mut self,
        key: &CanonicalKey,
    ) -> Result<Option<(LabelId, Vec<(PropKey, Primitive)>)>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk)?.is_none() {
                    return Ok(None);
                }
                self.ensure_vertex_props_loaded(vk)?;
                let vt = self.vertices.get_mut(&vk).unwrap();
                let label_id = vt.label_id;
                let schema = self.schema.read().unwrap();
                let props = vt
                    .all_props()
                    .iter()
                    .map(|p| {
                        let name = schema
                            .prop_key_str(p.key)
                            .cloned()
                            .unwrap_or_else(|| smol_str::SmolStr::from(format!("__key_{}", p.key)));
                        (name, p.value.clone())
                    })
                    .collect();
                Ok(Some((label_id, props)))
            }
            CanonicalKey::Edge(ek) => {
                if self.get_edge(&ek.out_key())?.is_none() {
                    return Ok(None);
                }
                let eg = self.edges.get_mut(&ek).unwrap();
                let label_id = eg.label_id;
                let schema = self.schema.read().unwrap();
                let props = eg
                    .all_props()
                    .iter()
                    .map(|p| {
                        let name = schema
                            .prop_key_str(p.key)
                            .cloned()
                            .unwrap_or_else(|| smol_str::SmolStr::from(format!("__key_{}", p.key)));
                        (name, p.value.clone())
                    })
                    .collect();
                Ok(Some((label_id, props)))
            }
            CanonicalKey::Empty => Err(StoreError::TraversalError("Element key cannot be empty".to_string())),
        }
    }
}
