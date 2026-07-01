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
    schema::Schema,
    store::{
        rocks::encoding,
        traits::{GraphStore, GraphTransaction},
    },
    types::{
        element::{Edge, Property, Vertex},
        keys::{
            AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId,
            VertexKey, DEFAULT_RANK,
        },
        prop_key::PropKey,
        prop_key::{ID_KEY_ID, LABEL_KEY_ID},
        Primitive, Rank, StoreError,
    },
};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, RwLock},
};

use super::helpers::{edge_matches, upsert_prop};
use super::{Existence, ScanConfig, StagedSchema};

// ── LogicalGraph ──────────────────────────────────────────────────────────────
/// Query-scoped logical graph wrapping a store transaction.
///
/// Obtained by calling `LogicalGraph::new(store.begin())`. The engine uses this
/// as its sole interface to the graph.
pub(crate) struct LogicalGraph<S: GraphStore> {
    store: S::Txn, // The underlying transaction from the GraphStore.
    pub(crate) vertices: HashMap<VertexKey, Vertex>,
    edges: HashMap<CanonicalEdgeKey, Edge>,
    vertex_degree: HashMap<VertexKey, (u32, u32, LabelId)>,
    pub(crate) dirty: HashMap<CanonicalKey, Existence>,
    pub(crate) scan_config: ScanConfig,
    pub(crate) schema: Arc<RwLock<Schema>>,
    pub(crate) staged_schema: StagedSchema,
}

impl<S: GraphStore> LogicalGraph<S> {
    /// Create a new logical graph context wrapping the given transaction.
    pub fn new(store: S::Txn, schema: Arc<RwLock<Schema>>) -> Self {
        // Creates a new `LogicalGraph` instance, initializing its in-memory caches
        // and associating it with a store transaction.
        Self {
            store,
            vertices: HashMap::new(),
            edges: HashMap::new(),
            vertex_degree: HashMap::new(),
            // Tracks the mutation state of elements within this transaction.
            dirty: HashMap::new(),
            scan_config: ScanConfig::default(),
            schema,
            staged_schema: StagedSchema::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn vertex_degree_for_test(&mut self, key: VertexKey) -> Result<Option<(u32, u32, LabelId)>, StoreError> {
        self.get_vertex_degree(key)
    }

    /// Retrieves the degree (out-edge count, in-edge count) and label of a vertex.
    /// This method acts as a transparent read-through cache:
    ///     it first checks the in-memory `vertex_degree` overlay,
    ///     and falls back to the underlying `GraphStore` on a miss, caching the result.
    ///     It is central to existence checks for vertex existence.
    ///     Returns `None` if the vertex does not exist.
    fn get_vertex_degree(&mut self, key: VertexKey) -> Result<Option<(u32, u32, LabelId)>, StoreError> {
        if let Some(counts) = self.vertex_degree.get(&key) {
            return Ok(Some(*counts));
        }
        self.store.get_vertex_degree(key)?.map_or(Ok(None), |counts| {
            self.vertex_degree.insert(key, counts);
            Ok(Some(counts))
        })
    }

    /// O(1) read of the per-vertex degree counters via the in-memory overlay.
    /// Also caches the vertex label learned for free from the degree record.
    /// Returns `Ok(0)` for a missing vertex — consistent with `out([]).count()` returning 0.
    pub(crate) fn get_degree(
        &mut self,
        key: VertexKey,
        direction: crate::types::DegreeDirection,
    ) -> Result<u64, StoreError> {
        let (out_cnt, in_cnt, label_id) = match self.get_vertex_degree(key)? {
            Some(t) => t,
            None => return Ok(0),
        };
        self.cache_vertex_label(key, label_id);
        Ok(match direction {
            crate::types::DegreeDirection::Out => out_cnt as u64,
            crate::types::DegreeDirection::In => in_cnt as u64,
            crate::types::DegreeDirection::Both => out_cnt as u64 + in_cnt as u64,
        })
    }

    /// Records an element's mutation state in the query-scoped overlay.
    ///
    /// If the element was already modified in this transaction, its state is
    /// combined with the new state via `Existence::merge`.
    fn mark_dirty(&mut self, key: CanonicalKey, state: Existence) {
        // Marks an element as dirty with a specific existence state.
        match self.dirty.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(state);
            }
            Entry::Occupied(mut entry) => {
                let combined = entry.get().merge(state);
                *entry.get_mut() = combined;
            }
        }
    }

    /// Cache a vertex label learned for free from an adjacent edge's value prefix.
    /// Uses `or_insert_with` — never clobbers a richer entry (same pattern as the
    /// edges overlay: `self.edges.entry(cek).or_insert(edge)`).
    #[inline]
    fn cache_vertex_label(&mut self, id: VertexKey, label_id: LabelId) {
        self.vertices.entry(id).or_insert_with(|| Vertex::label_only(id, label_id));
    }

    /// Upgrade a `LabelOnly` vertex entry to a fully-loaded one from the store.
    /// No-op if the entry is already richer or absent.  Returns
    /// `StoreError::CorruptData` if a `LabelOnly`-cached vertex is no longer
    /// present in the store — this is unreachable under the degree-counter
    /// invariant (an edge can't exist without its endpoint), but it becomes a
    /// surfaced error rather than a silent fallthrough to "zero properties."
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

    // ── Reads ─────────────────────────────────────────────────────────────────

    /// Look up a vertex by key, loading from the store on first access.
    ///
    /// Returns `None` for absent or tombstoned vertices.
    ///
    /// **Note**: Consider adding a batch `get_vertices` method for bulk property retrieval.
    /// Currently, `get_vertex` serves dual purposes: fetching property data and checking
    /// for existence. A batch API would improve data fetching performance, but would require careful
    //      design to comfortably handle partial results where some keys might be missing.
    pub(crate) fn get_vertex(&mut self, key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        if !self.vertices.contains_key(&key) {
            match self.store.get_vertex(key)? {
                None => return Ok(None),
                Some(vt) => {
                    self.vertices.insert(key, vt);
                }
            }
        }
        if self.dirty.get(&CanonicalKey::Vertex(key)) == Some(&Existence::Tombstone) {
            return Ok(None);
        }
        Ok(Some(key))
    }

    /// Look up an edge by canonical key, loading from the store on first access.
    /// This method returns `None` for absent or tombstoned edges.
    /// Returns `None` for absent or tombstoned edges.
    pub(crate) fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        let cek = key.canonical_edge_key();
        if !self.edges.contains_key(&cek) {
            // Load the primary physical record (OUT) to populate the canonical edge.
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
        if self.dirty.get(&CanonicalKey::Edge(cek)) == Some(&Existence::Tombstone) {
            return Ok(None);
        }
        Ok(Some(*key))
    }

    pub(crate) fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        let mut missing_keys = Vec::new();
        for &k in keys {
            if !self.vertices.contains_key(&k)
                && self.dirty.get(&CanonicalKey::Vertex(k)) != Some(&Existence::Tombstone)
            {
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
            if self.vertices.contains_key(&k) && self.dirty.get(&CanonicalKey::Vertex(k)) != Some(&Existence::Tombstone)
            {
                result.push(k);
            }
        }
        Ok(result)
    }

    pub(crate) fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
        let mut missing_keys = Vec::new();
        for k in keys {
            let cek = k.canonical_edge_key();
            if !self.edges.contains_key(&cek) && self.dirty.get(&CanonicalKey::Edge(cek)) != Some(&Existence::Tombstone)
            {
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
            if self.edges.contains_key(&cek) && self.dirty.get(&CanonicalKey::Edge(cek)) != Some(&Existence::Tombstone)
            {
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
        for edge in committed {
            // Cache vertex labels learned for free from this physical edge read
            // — populates the vertex overlay so later hasLabel() calls skip the store.
            if let Some(l) = edge.src_label {
                self.cache_vertex_label(edge.src_id, l);
            }
            if let Some(l) = edge.dst_label {
                self.cache_vertex_label(edge.dst_id, l);
            }
            let cek = edge.canonical_key();
            self.edges.entry(cek).or_insert(edge);
        }

        let rank_set: Option<HashSet<Rank>> = opts.rank.map(|r| r.iter().copied().collect());
        let mut matching = Vec::new();
        let dirty = &self.dirty;
        for (&cek, edge) in &self.edges {
            if dirty.get(&CanonicalKey::Edge(cek)) == Some(&Existence::Tombstone) {
                continue;
            }
            if !edge_matches(edge, vertex, direction, opts.label, opts.dst) {
                continue;
            }
            if let Some(ref set) = rank_set {
                if !set.contains(&edge.rank) {
                    continue;
                }
            }
            if let Some(last_committed) = cursor {
                let edge_cursor = AdjacentEdgeCursor::from_edge(edge, direction);
                if edge_cursor > last_committed {
                    continue;
                }
            }
            matching.push(edge);
        }

        matching.sort_by(|a, b| {
            let cursor_a = AdjacentEdgeCursor::from_edge(a, direction);
            let cursor_b = AdjacentEdgeCursor::from_edge(b, direction);
            cursor_a.cmp(&cursor_b)
        });

        let mut start_idx = 0;
        if let Some(start_cursor) = opts.start_from {
            if let Some(pos) = matching.iter().position(|e| AdjacentEdgeCursor::from_edge(e, direction) == start_cursor)
            {
                start_idx = pos + 1;
            } else {
                start_idx = matching
                    .iter()
                    .position(|e| AdjacentEdgeCursor::from_edge(e, direction) > start_cursor)
                    .unwrap_or(matching.len());
            }
        }

        let limit_val = limit.unwrap_or(u32::MAX) as usize;
        let end_idx = std::cmp::min(start_idx + limit_val, matching.len());
        let mut result = Vec::new();
        for edge in &matching[start_idx..end_idx] {
            let cek = edge.canonical_key();
            let physical_key = match direction {
                Direction::OUT => cek.out_key(),
                Direction::IN => cek.in_key(),
            };
            result.push(physical_key);
        }

        let next_cursor = if end_idx < matching.len() {
            Some(AdjacentEdgeCursor::from_edge(matching[end_idx - 1], direction))
        } else {
            cursor
        };

        Ok((result, next_cursor))
    }

    pub(crate) fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        let (committed, cursor) = self.store.scan_vertices(label, start_from, limit)?;
        for vt in committed {
            // Upgrade a LabelOnly placeholder if a full record just arrived from
            // the scan — don't waste the read scan_vertices already paid for.
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

        let mut matching = Vec::new();
        let dirty = &self.dirty;
        for (&vk, vt) in &self.vertices {
            if dirty.get(&CanonicalKey::Vertex(vk)) == Some(&Existence::Tombstone) {
                continue;
            }
            if let Some(lbl) = label {
                if vt.label_id != lbl {
                    continue;
                }
            }
            if let Some(last_committed) = cursor {
                if vk > last_committed {
                    continue;
                }
            }
            matching.push(vk);
        }

        matching.sort();

        let mut start_idx = 0;
        if let Some(start_vk) = start_from {
            if let Some(pos) = matching.iter().position(|&vk| vk == start_vk) {
                start_idx = pos + 1;
            } else {
                start_idx = matching.iter().position(|&vk| vk > start_vk).unwrap_or(matching.len());
            }
        }

        let limit_val = limit as usize;
        let end_idx = std::cmp::min(start_idx + limit_val, matching.len());
        let result = matching[start_idx..end_idx].to_vec();

        let next_cursor = if end_idx < matching.len() { Some(matching[end_idx - 1]) } else { cursor };

        Ok((result, next_cursor))
    }

    pub(crate) fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<CanonicalEdgeKey>), StoreError> {
        let (committed, cursor) = self.store.scan_edges(label, start_from, limit)?;
        for edge in committed {
            if let Some(l) = edge.src_label {
                self.cache_vertex_label(edge.src_id, l);
            }
            if let Some(l) = edge.dst_label {
                self.cache_vertex_label(edge.dst_id, l);
            }
            let cek = edge.canonical_key();
            self.edges.entry(cek).or_insert(edge);
        }

        let mut matching = Vec::new();
        let dirty = &self.dirty;
        for (&cek, edge) in &self.edges {
            if dirty.get(&CanonicalKey::Edge(cek)) == Some(&Existence::Tombstone) {
                continue;
            }
            if let Some(lbl) = label {
                if edge.label_id != lbl {
                    continue;
                }
            }
            if let Some(last_committed) = cursor {
                if cek > last_committed {
                    continue;
                }
            }
            matching.push(cek);
        }

        matching.sort();

        let mut start_idx = 0;
        if let Some(start_cek) = start_from {
            if let Some(pos) = matching.iter().position(|&cek| cek == start_cek) {
                start_idx = pos + 1;
            } else {
                start_idx = matching.iter().position(|&cek| cek > start_cek).unwrap_or(matching.len());
            }
        }

        let limit_val = limit as usize;
        let end_idx = std::cmp::min(start_idx + limit_val, matching.len());
        let result = matching[start_idx..end_idx].iter().map(|cek| cek.out_key()).collect();

        let next_cursor = if end_idx < matching.len() { Some(matching[end_idx - 1]) } else { cursor };

        Ok((result, next_cursor))
    }

    /// Read a single property from a vertex or edge.
    ///
    /// # Vertex — no precondition
    /// Delegates to `get_vertex`, which loads from the store on a cache miss and
    /// returns `None` for absent or tombstoned vertices.
    ///
    /// # Edge — overlay-only (precondition: edge must be in overlay)
    /// The edge must already be in the overlay (populated via a prior `get_edge`
    /// / `get_edges` call); returns `None` if absent. Consistent with the
    /// overlay-only policy for all other edge operations.
    ///
    /// # Locking
    /// No lock is acquired. The method operates via an exclusive borrow (`&mut self`).
    pub(crate) fn get_property(
        &mut self,
        key: &CanonicalKey,
        prop_key_id: u16,
    ) -> Result<Option<Property>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk).unwrap().is_some() {
                    // LabelOnly entries carry the label and id but no properties —
                    // for any key other than id/label, upgrade to a real fetch first.
                    if prop_key_id != ID_KEY_ID && prop_key_id != LABEL_KEY_ID {
                        self.ensure_vertex_props_loaded(vk)?;
                    }
                    Ok(self.vertices.get_mut(&vk).unwrap().get_property(prop_key_id))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => {
                if self.dirty.get(key) == Some(&Existence::Tombstone) {
                    return Ok(None);
                }
                Ok(self.edges.get_mut(&ek).unwrap().get_property(prop_key_id))
            }
            CanonicalKey::Empty => Err(StoreError::TraversalError("Property owner cannot be empty".to_string())),
        }
    }

    /// Read a single primitive value from a vertex or edge property.
    ///
    /// # Vertex — no precondition
    /// Delegates to `get_vertex`, which loads from the store on a cache miss and then
    /// returns `None` for absent or tombstoned vertices.
    ///
    /// # Edge — overlay-only (precondition: edge must be in overlay)
    /// The edge must already be in the overlay; returns `None` if absent.
    ///
    /// # Locking
    /// No lock is acquired. The method operates via an exclusive borrow (`&mut self`).
    pub(crate) fn get_value(&mut self, key: &CanonicalKey, prop_key_id: u16) -> Result<Option<Primitive>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk).unwrap().is_some() {
                    if prop_key_id != ID_KEY_ID && prop_key_id != LABEL_KEY_ID {
                        self.ensure_vertex_props_loaded(vk)?;
                    }
                    Ok(self.vertices.get_mut(&vk).unwrap().get_value(prop_key_id))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => {
                if self.dirty.get(key) == Some(&Existence::Tombstone) {
                    return Ok(None);
                }
                Ok(self.edges.get_mut(&ek).unwrap().get_value(prop_key_id))
            }
            CanonicalKey::Empty => {
                Err(StoreError::UnexpectedDataType("expected Vertex or Edge for get property value".to_string()))
            }
        }
    }
    #[allow(clippy::type_complexity)]
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
                // "all" can never be answered from a label-only entry — upgrade first.
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

    // ── Mutations ─────────────────────────────────────────────────────────────
    //
    // Not every method is precondition-free.  Vertex operations are fully
    // self-sufficient — they reach the store automatically when the overlay is
    // cold.  Edge operations (except get_edge / get_edges) are overlay-only:
    // the caller must ensure the edge is in the overlay via a prior get_edge /
    // get_edges call before invoking set_property, drop_property, get_property,
    // or drop_element on an edge key.
    //
    //   Method                  Precondition
    //   ──────────────────────  ────────────────────────────────────────────
    //   get_vertex              none  (store fallback on miss)
    //   get_edge                none  (store fallback on miss) // Corrected comment: get_edge has store fallback.
    //   get_edges               none  (store merged with overlay)
    //   get_property (vertex)   none  (delegates to get_vertex; upgrades LabelOnly
    //                                  entries automatically for non-id/label keys)
    //   get_property (edge)     ⚠ edge must be in overlay
    //   add_vertex              none  (get_vertex_degree checks overlay+store)
    //   add_edge                none  (overlay+store for edge; get_vertex_degree
    //                                  for endpoints)
    //   set_property (vertex)   none  (auto-load from store; also upgrades LabelOnly)
    //   set_property (edge)     ⚠ edge must be in overlay
    //   drop_property (vertex)  none  (auto-load from store; also upgrades LabelOnly)
    //   drop_property (edge)    ⚠ edge must be in overlay
    //   drop_element (vertex)   none  (get_vertex_degree checks overlay+store)
    //   drop_element (edge)     ⚠ edge must be in overlay

    /// Add a new vertex with explicit `id` and `label_id` to the overlay.
    ///
    /// Returns the `VertexKey` on success.
    /// This method performs an existence check and updates the in-memory overlay.
    /// # Existence check
    /// Duplicate detection is a single call to `get_vertex_degree(id)`, which
    /// first checks the in-memory `vertex_degree` overlay and then falls back to
    /// the store.  Using the lightweight degree record avoids loading the full
    /// vertex body.  Both newly-created and pre-existing (persisted) vertices are
    /// covered by this single path, so **no precondition is required of the
    /// caller**.
    ///
    /// **Gap — TOCTOU between check and commit:** another concurrent transaction
    /// could insert the same `id` between this check and `commit()`.  That race
    /// is not caught here; it is detected by the store's Optimistic Concurrency Control (OCC) conflict check at
    /// commit time, which returns `StoreError::Conflict`.
    ///
    /// **Gap — delete-then-add in the same transaction:** a tombstoned vertex
    /// still has a degree record in the overlay, so `get_vertex_degree` returns
    /// `Some` and this method returns `StoreError::DuplicateVertex`.
    /// Re-inserting a deleted vertex within one transaction is not supported.
    ///
    /// # Locking
    /// No lock is acquired. The new vertex's `props` field is an empty
    /// `Vec<Property>`.
    pub(crate) fn add_vertex(&mut self, id: VertexKey, label_id: LabelId) -> Result<VertexKey, StoreError> {
        // Single-call check: covers both overlay (vertex_degree map) and store.
        if self.get_vertex_degree(id)?.is_some() {
            return Err(StoreError::DuplicateVertex(id));
        }

        {
            let schema = self.schema.read().unwrap();
            if !schema.persisted_vertex_labels.contains(&label_id) {
                self.staged_schema.staged_vertex_labels.insert(label_id);
            }
        }

        let vertex = Vertex::with_props(id, label_id, Vec::new());
        self.vertices.insert(id, vertex);
        self.vertex_degree.insert(id, (0, 0, label_id));
        self.mark_dirty(CanonicalKey::Vertex(id), Existence::New);
        Ok(id)
    }

    /// Register a new directed edge identified by `cek`.
    /// This method returns an `EdgeKey` in Out orientation on success.
    /// Returns an `EdgeKey` in Out orientation on success.
    ///
    /// # Existence check — no precondition required
    /// Duplicate detection is two-phase:
    /// 1. Overlay check — `self.edges.contains_key(&cek)`. Detects edges already inserted (or tombstoned but still in
    ///    the overlay) within this transaction.
    /// 2. Store check — `store.get_edge(cek, OUT)`. Detects persisted edges not yet loaded into the overlay.
    ///
    /// **Gap — TOCTOU:** same as `add_vertex`; concurrent inserts of the same edge
    /// are caught at commit time via OCC, not here.
    /// This method does not support re-inserting a tombstoned edge within the same transaction.
    /// **Gap — re-adding a tombstoned edge:** a tombstoned edge remains in
    /// `self.edges`, so the overlay check fires and returns
    /// `StoreError::DuplicateEdge`.  Re-inserting a deleted edge within one
    /// transaction is not supported.
    ///
    /// # Endpoint existence check — no precondition required
    /// Both endpoint vertices must exist, but the caller does not need to
    /// pre-load them.  `get_vertex_degree` is called for both `src_id` and
    /// `dst_id`; it checks the in-memory overlay first and falls back to the
    /// store automatically. A missing endpoint returns `StoreError::NotFound`
    /// before any state is mutated.
    ///
    /// # Degree counter update
    /// After the existence checks, `src.out_degree` and `dst.in_degree` are
    /// incremented atomically within the overlay and marked `CounterOnly` dirty.
    /// This prevents `drop_element` from deleting either endpoint while this edge
    /// is live, because the degree check in `drop_element` will see a non-zero
    /// counter.
    ///
    /// # Locking
    /// No lock is acquired.  The new edge's `props` field starts empty.
    pub(crate) fn add_edge(&mut self, ek: &EdgeKey) -> Result<EdgeKey, StoreError> {
        let edge_mode = self.schema.read().unwrap().edge_mode;
        if edge_mode == crate::schema::EdgeMode::Single && ek.rank != DEFAULT_RANK {
            return Err(StoreError::UnsupportedOperation(format!(
                "Non-zero rank {} is not allowed for single-edge relationship of label id {}",
                ek.rank, ek.label_id
            )));
        }
        {
            let schema = self.schema.read().unwrap();
            if !schema.persisted_edge_labels.contains(&ek.label_id) {
                self.staged_schema.staged_edge_labels.insert(ek.label_id);
            }
        }
        let cek = ek.canonical_edge_key();
        if self.edges.contains_key(&cek) {
            return Err(StoreError::DuplicateEdge(cek));
        }
        // Check store for a persisted edge not yet in the overlay.
        if self.store.get_edge(ek)?.is_some() {
            return Err(StoreError::DuplicateEdge(cek));
        }

        // Verify both endpoints exist and update degree counters.
        //
        // Self-loop guard: when src_id == dst_id, two independent reads before
        // either insert both return the pre-increment value, so the second insert
        // silently overwrites the first and the out_e_cnt increment is lost.
        // Fix: read once, increment both counters atomically, write once.
        let (src_label_id, dst_label_id) = if cek.src_id == cek.dst_id {
            let (mut out_cnt, mut in_cnt, label_id) =
                self.get_vertex_degree(cek.src_id)?.ok_or(StoreError::NotFound)?;
            out_cnt += 1;
            in_cnt += 1;
            self.vertex_degree.insert(cek.src_id, (out_cnt, in_cnt, label_id));
            self.mark_dirty(CanonicalKey::Vertex(cek.src_id), Existence::CounterOnly);
            (label_id, label_id)
        } else {
            let (mut src_out, src_in, src_label_id) =
                self.get_vertex_degree(cek.src_id)?.ok_or(StoreError::NotFound)?;
            let (dst_out, mut dst_in, dst_label_id) =
                self.get_vertex_degree(cek.dst_id)?.ok_or(StoreError::NotFound)?;
            src_out += 1;
            dst_in += 1;
            self.vertex_degree.insert(cek.src_id, (src_out, src_in, src_label_id));
            self.mark_dirty(CanonicalKey::Vertex(cek.src_id), Existence::CounterOnly);
            self.vertex_degree.insert(cek.dst_id, (dst_out, dst_in, dst_label_id));
            self.mark_dirty(CanonicalKey::Vertex(cek.dst_id), Existence::CounterOnly);
            (src_label_id, dst_label_id)
        };

        // 2. insert new edge into overlay and mark dirty.  The store is not touched until commit.
        self.edges.insert(
            cek,
            Edge::with_props(
                cek.src_id,
                cek.label_id,
                cek.dst_id,
                cek.rank,
                Vec::new(),
                Some(src_label_id),
                Some(dst_label_id),
            ),
        );
        self.mark_dirty(CanonicalKey::Edge(cek), Existence::New);
        Ok(cek.out_key())
    }

    /// Upsert a property on a vertex or edge.
    /// This method updates the in-memory overlay and marks the element as modified.
    /// # Existence check
    /// 1. Tombstone guard — if the element's dirty state is `Tombstone`, returns `StoreError::Tombstoned` immediately.
    /// 2. For **vertices**: if the vertex is not yet in the overlay it is loaded from the store automatically.
    ///    `StoreError::NotFound` is returned only if the store also has no record.  **No precondition required.**
    /// 3. For **edges**: overlay-only.  The edge must already be in the overlay (populated via a prior `get_edge`
    ///    call); if absent `StoreError::NotFound` is returned.  **Caller must pre-load the edge.**
    ///
    /// # Locking
    /// No lock is acquired. Mutates the properties in place via an exclusive borrow.
    pub(crate) fn set_property(&mut self, prop: &Property) -> Result<(), StoreError> {
        {
            let schema = self.schema.read().unwrap();
            if !schema.persisted_prop_keys.contains(&prop.key) {
                self.staged_schema.staged_prop_keys.insert(prop.key);
            }
            if let Some(cfg) = schema.prop_key_types.get(&prop.key) {
                let incoming_type = crate::schema::DataType::from_primitive(&prop.value);
                if cfg.data_type != incoming_type {
                    return Err(StoreError::SchemaViolation(format!(
                        "Type mismatch for property key id {}: expected {:?}",
                        prop.key, cfg.data_type
                    )));
                }
            }
        }
        let key = prop.owner;
        match key {
            CanonicalKey::Vertex(id) => {
                if self.dirty.get(&key) == Some(&Existence::Tombstone) {
                    return Err(StoreError::Tombstoned);
                }
                // Auto-load from store if not yet in overlay, or if the existing entry
                // is LabelOnly — mutating a LabelOnly entry without an upgrade first
                // would treat unread properties as nonexistent and discard them.
                if !self.vertices.contains_key(&id) || self.vertices.get(&id).is_some_and(Vertex::is_label_only) {
                    match self.store.get_vertex(id)? {
                        None => return Err(StoreError::NotFound),
                        Some(vt) => {
                            self.vertices.insert(id, vt);
                        }
                    }
                }
                {
                    let vt = self.vertices.get_mut(&id).expect("just loaded");
                    upsert_prop(vt.props_mut(), prop);
                }
                self.mark_dirty(key, Existence::Modified);
            }
            CanonicalKey::Edge(ek) => {
                if self.dirty.get(&key) == Some(&Existence::Tombstone) {
                    return Err(StoreError::Tombstoned);
                }
                match self.edges.get_mut(&ek) {
                    None => return Err(StoreError::NotFound),
                    Some(eg) => {
                        upsert_prop(eg.props_mut(), prop);
                    }
                }
                self.mark_dirty(key, Existence::Modified);
            }
            CanonicalKey::Empty => {
                return Err(StoreError::TraversalError("Property owner cannot be empty".to_string()));
            }
        }
        Ok(())
    }

    /// Remove a property from a vertex or edge.
    ///
    /// # Existence check and locking
    /// Same semantics as `set_property`: tombstone guard first, then,
    /// auto-load-from-store for vertices (no precondition), overlay-only check
    /// for edges (caller must pre-load). Mutates the properties in place via an
    /// exclusive borrow.
    pub(crate) fn drop_property(&mut self, prop: &Property) -> Result<(), StoreError> {
        let key = prop.owner;
        match key {
            CanonicalKey::Vertex(id) => {
                if self.dirty.get(&key) == Some(&Existence::Tombstone) {
                    return Err(StoreError::Tombstoned);
                }
                // Auto-load from store if not yet in overlay, or if the existing entry
                // is LabelOnly — same rationale as set_property.
                if !self.vertices.contains_key(&id) || self.vertices.get(&id).is_some_and(Vertex::is_label_only) {
                    match self.store.get_vertex(id)? {
                        None => return Err(StoreError::NotFound),
                        Some(vt) => {
                            self.vertices.insert(id, vt);
                        }
                    }
                }
                {
                    let vt = self.vertices.get_mut(&id).expect("just loaded");
                    vt.props_mut().retain(|p| p.key != prop.key);
                }
                self.mark_dirty(key, Existence::Modified);
            }
            CanonicalKey::Edge(ek) => {
                if self.dirty.get(&key) == Some(&Existence::Tombstone) {
                    return Err(StoreError::Tombstoned);
                }
                match self.edges.get_mut(&ek) {
                    None => return Err(StoreError::NotFound),
                    Some(eg) => {
                        eg.props_mut().retain(|p| p.key != prop.key);
                    }
                }
                self.mark_dirty(key, Existence::Modified);
            }
            CanonicalKey::Empty => {
                return Err(StoreError::TraversalError("Property owner cannot be empty".to_string()));
            }
        }
        Ok(())
    }

    /// Mark a vertex or edge as deleted in the overlay (tombstoned).
    ///
    /// The physical delete is deferred to `commit()`.
    ///
    /// # Vertex — existence check
    /// Existence is verified via `get_vertex_degree`, which checks the overlay,
    /// first and then falls back to the store.  Returns `StoreError::NotFound`
    /// if neither source has a record.
    ///
    /// **Gap — already-tombstoned vertex:** if the vertex was tombstoned earlier
    /// in this transaction its degree record is still present (in `vertex_degree`
    /// or the store), so the existence check succeeds and the tombstone is applied
    /// a second time — a no-op thanks to `Existence::merge`, but no error is
    /// returned.  Callers that need idempotency-free semantics must check the
    /// dirty map themselves.
    ///
    /// # Vertex — incident-edge guard
    /// Before tombstoning, the method reads the current degree `(out_e, in_e)`.
    /// If either is non-zero, `StoreError::IncidentEdges` is returned, and the
    /// vertex is left unchanged.  All incident edges (including those added in
    /// this transaction) must be tombstoned first.
    ///
    /// **Gap — race with concurrent edge inserts:** another transaction could
    /// insert an edge incident to this vertex after the degree check passes here
    /// but before `commit()`.  The OCC check at commit time does not
    /// automatically detect this graph-level invariant violation; it would
    /// require a store-level constraint or a re-check inside the commit path.
    ///
    /// # Edge — existence check (precondition: edge must be in overlay)
    /// The check is **overlay-only**: `self.edges.contains_key(&cek)`.  If the
    /// edge exists in the store but has not been loaded into the overlay yet,
    /// this method returns `StoreError::NotFound`.  Unlike vertex operations,
    /// there is no auto-load fallback here; callers must call `get_edge` first
    /// to populate the overlay before dropping an edge.  This asymmetry is
    /// intentional: dropping an edge that was never read in this transaction is
    /// unusual and likely a caller bug.
    ///
    /// # Edge — degree counter update
    /// When an edge is tombstoned `src.out_degree` and `dst.in_degree` are
    /// decremented in the overlay (using `saturating_sub` to avoid underflow).
    /// This update is idempotent — if the edge is already tombstoned the degree
    /// adjustment is skipped.
    ///
    /// # Locking
    /// No lock is acquired.  Property data is not read or modified during a drop;
    /// the element is only marked in the dirty map.
    pub(crate) fn drop_element(&mut self, key: &CanonicalKey) -> Result<(), StoreError> {
        match *key {
            CanonicalKey::Vertex(id) => {
                let (out_e, in_e, _) = self.get_vertex_degree(id)?.ok_or(StoreError::NotFound)?;
                if out_e > 0 || in_e > 0 {
                    return Err(StoreError::IncidentEdges);
                }
                self.mark_dirty(*key, Existence::Tombstone);
            }
            CanonicalKey::Edge(ek) => {
                if !self.edges.contains_key(&ek) {
                    return Err(StoreError::NotFound);
                }
                if self.dirty.get(key) != Some(&Existence::Tombstone) {
                    self.mark_dirty(*key, Existence::Tombstone);
                    if let Some((mut out_e, in_e, label_id)) = self.get_vertex_degree(ek.src_id)? {
                        out_e = out_e.saturating_sub(1);
                        self.vertex_degree.insert(ek.src_id, (out_e, in_e, label_id));
                        self.mark_dirty(CanonicalKey::Vertex(ek.src_id), Existence::CounterOnly);
                    }
                    if let Some((out_e, mut in_e, label_id)) = self.get_vertex_degree(ek.dst_id)? {
                        in_e = in_e.saturating_sub(1);
                        self.vertex_degree.insert(ek.dst_id, (out_e, in_e, label_id));
                        self.mark_dirty(CanonicalKey::Vertex(ek.dst_id), Existence::CounterOnly);
                    }
                }
            }
            CanonicalKey::Empty => {
                return Err(StoreError::TraversalError("Element key cannot be empty".to_string()));
            }
        }
        Ok(())
    }

    // ── Transaction control ───────────────────────────────────────────────────

    /// Flush all dirty mutations to the store and commit atomically.
    ///
    /// On `StoreError::Conflict` the overlay is cleared so the context can be
    /// reused; the caller must rebuild traversal state from scratch for a retry.
    pub fn commit(&mut self) -> Result<(), StoreError> {
        // Collect first so the loop body can borrow self.vertices / self.edges
        // and self.store simultaneously without a conflict on self.dirty.
        let dirty: Vec<(CanonicalKey, Existence)> = self.dirty.iter().map(|(&k, &v)| (k, v)).collect();
        for (key, existence) in dirty {
            match (key, existence) {
                (CanonicalKey::Vertex(id), Existence::New) => {
                    let v = self.vertices.get_mut(&id).expect("dirty vertex key not in vertices");
                    self.store.put_vertex(id, v.label_id, v.all_props())?;
                    let (out_e, in_e, label_id) = self.vertex_degree[&id];
                    self.store.put_vertex_degree(id, out_e, in_e, label_id)?;
                }
                (CanonicalKey::Vertex(id), Existence::Modified) => {
                    let v = self.vertices.get_mut(&id).expect("dirty vertex key not in vertices");
                    self.store.put_vertex(id, v.label_id, v.all_props())?;
                }
                (CanonicalKey::Vertex(id), Existence::CounterOnly) => {
                    let (out_e, in_e, label_id) = self.vertex_degree[&id];
                    self.store.put_vertex_degree(id, out_e, in_e, label_id)?;
                }
                (CanonicalKey::Vertex(id), Existence::ModifiedWithCounter) => {
                    let v = self.vertices.get_mut(&id).expect("dirty vertex key not in vertices");
                    self.store.put_vertex(id, v.label_id, v.all_props())?;
                    let (out_e, in_e, label_id) = self.vertex_degree[&id];
                    self.store.put_vertex_degree(id, out_e, in_e, label_id)?;
                }
                (CanonicalKey::Vertex(id), Existence::Tombstone) => {
                    self.store.delete_vertex(id)?;
                    self.store.delete_vertex_degree(id)?;
                }
                (
                    CanonicalKey::Edge(ek),
                    Existence::New | Existence::Modified | Existence::CounterOnly | Existence::ModifiedWithCounter,
                ) => {
                    // Extract labels and endpoint ids first — they're Copy, so the
                    // immutable borrow on self.edges is released before we call
                    // self.get_vertex_degree (which borrows self mutably).
                    let (dst_id, src_id, dst_label, src_label) = {
                        let e = self.edges.get(&ek).expect("dirty edge key not in edges");
                        (e.dst_id, e.src_id, e.dst_label, e.src_label)
                    };

                    // Resolve any missing label via the read-through vertex_degree cache.
                    // For New edges both labels are Some (set by add_edge) — zero extra I/O.
                    // For Modified edges one label is None (build_lazy_edge only populates
                    // the label for the direction it read).  A missing vertex at this point
                    // is a data-integrity violation — the edge exists but its endpoint
                    // doesn't — and surfaces as StoreError::NotFound.
                    let dst_label = match dst_label {
                        Some(l) => l,
                        None => self.get_vertex_degree(dst_id)?.map(|(_, _, l)| l).ok_or(StoreError::NotFound)?,
                    };
                    let src_label = match src_label {
                        Some(l) => l,
                        None => self.get_vertex_degree(src_id)?.map(|(_, _, l)| l).ok_or(StoreError::NotFound)?,
                    };

                    let e = self.edges.get_mut(&ek).expect("dirty edge key not in edges");
                    self.store.put_edge(&ek.out_key(), dst_label, e.all_props())?;
                    self.store.put_edge(&ek.in_key(), src_label, e.all_props())?;
                }
                (CanonicalKey::Edge(cek), Existence::Tombstone) => {
                    self.store.delete_edge(&cek.out_key())?;
                    self.store.delete_edge(&cek.in_key())?;
                }
                (CanonicalKey::Empty, _) => {
                    return Err(StoreError::TraversalError("Element key cannot be empty".to_string()));
                }
            }
        }

        let mut schema_changed = false;
        {
            let schema = self.schema.read().unwrap();
            for &label_id in &self.staged_schema.staged_vertex_labels {
                if let Some(name) = schema.vertex_label_str(label_id) {
                    let val = encoding::encode_schema_label_value(label_id);
                    self.store.put_schema_entry(encoding::SCHEMA_KIND_VERTEX_LABEL, name, &val)?;
                    schema_changed = true;
                }
            }
            for &label_id in &self.staged_schema.staged_edge_labels {
                if let Some(name) = schema.edge_label_str(label_id) {
                    let val = encoding::encode_schema_label_value(label_id);
                    self.store.put_schema_entry(encoding::SCHEMA_KIND_EDGE_LABEL, name, &val)?;
                    schema_changed = true;
                }
            }
            for &prop_key_id in &self.staged_schema.staged_prop_keys {
                if let Some(name) = schema.prop_key_str(prop_key_id) {
                    if let Some(cfg) = schema.prop_key_types.get(&prop_key_id) {
                        let val = encoding::encode_schema_prop_value(prop_key_id, cfg.data_type.to_u8());
                        self.store.put_schema_entry(encoding::SCHEMA_KIND_PROP_KEY, name, &val)?;
                        schema_changed = true;
                    }
                }
            }
            if schema_changed {
                // `resolve_vertex_label`/`resolve_edge_label`/`resolve_prop_key` already bumped
                // `schema.version` once, in-memory, at build time when each of these entries was
                // newly registered (under the same write lock as the registration itself). This
                // step only flushes that already-current version to disk together with the
                // entries it covers — it must not bump it a second time.
                let meta_val =
                    encoding::encode_schema_meta(schema.version, schema.edge_mode.to_u8(), schema.mode.to_u8());
                self.store.put_schema_entry(encoding::SCHEMA_KIND_META, encoding::SCHEMA_META_NAME, &meta_val)?;
            }
        }

        let commit_result = self.store.commit();

        // Mark these persisted only once the underlying store commit actually succeeded;
        // must run before `self.reset()` below clears `self.staged_schema`.
        if schema_changed && commit_result.is_ok() {
            let mut schema = self.schema.write().unwrap();
            for &label_id in &self.staged_schema.staged_vertex_labels {
                schema.persisted_vertex_labels.insert(label_id);
            }
            for &label_id in &self.staged_schema.staged_edge_labels {
                schema.persisted_edge_labels.insert(label_id);
            }
            for &prop_key_id in &self.staged_schema.staged_prop_keys {
                schema.persisted_prop_keys.insert(prop_key_id);
            }
        }

        // Always reset, success or failure — see the doc comment on `commit`.
        self.reset();
        commit_result
    }

    /// Discard all pending mutations and reset the context.
    pub fn abort(&mut self) {
        // Discards all pending mutations and resets the logical graph's state.
        self.store.abort();
        self.reset();
    }

    fn reset(&mut self) {
        self.dirty.clear();
        self.vertices.clear();
        self.edges.clear();
        self.vertex_degree.clear();
        self.staged_schema.clear();
    }
}
