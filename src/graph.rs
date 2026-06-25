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

//! Query-scoped logical graph — the ground truth for a single traversal.
//! # Role:
//! `LogicalGraph<S>` sits between the Gremlin traversal engine and the
//! persistent `GraphStore`.  The engine never touches the store directly;
//! it only ever calls methods on `LogicalGraph`.
//!
//!
//! ```text
//! Traversal Engine
//!   │  ctx.get_vertex(key)     → Result<Option<VertexKey>, StoreError>
//!   │  ctx.add_vertex(id, lbl) → Result<VertexKey, StoreError>
//!   │  ctx.get_edges(…)        → Result<Vec<EdgeKey>, StoreError>
//!   │  ctx.set_property(…)     → Result<(), StoreError>
//!   │  ctx.commit()
//!   ▼
//! LogicalGraph<S: GraphStore>
//!   vertices:      HashMap<VertexKey, Vertex>          ← query-scoped overlay
//!   edges:         HashMap<CanonicalEdgeKey, Edge>
//!   vertex_degree: HashMap<VertexKey, (u32, u32)>      ← degree tracking (out, in)
//!   dirty:         HashMap<CanonicalKey, Existence>
//!   schema:        Arc<RwLock<Schema>>                 ← shared label/prop-key dictionary
//!   staged_schema: StagedSchema                        ← labels/keys newly registered this tx
//!   store:         S::Txn                              ← flush-on-commit
//!   ▼
//! S::Txn: GraphTransaction         ← RocksDB / Distributed / Mock
//! ```
//!
//! # Read path
//!
//! On first access, `get_vertex` checks the local map.  If absent it calls
//! `store.get_vertex`, inserts the result into the overlay, and returns the
//! `VertexKey`.  Subsequent accesses in the same query are O(1) map lookups.
//!
//! # Write path
//!
//! Mutations update the in-memory overlay and mark the element `dirty`.  The
//! store is never written until `commit()`.  This means the engine sees its
//! own writes immediately (read-your-writes), regardless of store backend.
//!
//! # Commit
//!
//! `commit()` iterates `dirty` and calls `store.put_*` / `store.delete_*` for
//! each element, flushes any newly-registered `staged_schema` entries via
//! `store.put_schema_entry`, then calls `store.commit()`. The overlay (including
//! `staged_schema`) is cleared so the `LogicalGraph` can be reused for a retry on
//! OCC conflict, regardless of whether the commit succeeded.
//!
//! # Graph Consistency
//!
//! `LogicalGraph` is solely responsible for graph-level integrity, while the
//! store layer acts as a dumb physical backend. It enforces invariants such as:
//! - Bidirectional edges: Committing an edge always emits writes for both `OUT` and `IN` indices.
//! - Dangling prevention: Creating an edge strictly verifies the existence of both vertices.
//! - Degree validation: A vertex cannot be dropped if its incident edge counts are non-zero.
//!
//! # In-place mutation
//!
//! Elements in the overlay are owned values (`Vertex` / `Edge`). Mutations
//! acquire a write lock on the `RwLock` wrapping the element's properties and
//! modify them in place.

use crate::{
    schema::Schema,
    store::{
        rocks::encoding,
        traits::{GraphSnapshot, GraphStore, GraphTransaction},
    },
    types::{
        element::{Edge, Property, Vertex},
        keys::{
            AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId,
            VertexKey, DEFAULT_RANK,
        },
        prop_key::PropKey,
        Primitive, Rank, StoreError,
    },
};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    sync::{Arc, RwLock},
};

// ── Existence ────────────────────────────────────────────────────────────────
//
/// Mutation kind for a dirty graph element within a `LogicalGraph`.
///
/// Only dirty elements appear in the `dirty` map; absence means `Clean`.
///
/// **Note**: How to handle delete -> add on the same element within a single query?
/// This is currently treated as `New`, but it might be beneficial to distinguish it
///     from a pure create for better conflict detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Existence {
    /// Props were mutated on an existing element.
    Modified,
    /// Only the vertex edge counts changed.
    CounterOnly,
    /// Props and vertex edge counts both changed.
    ModifiedWithCounter,
    /// Created in this query; not yet persisted.
    New,
    /// Deleted in this query.
    Tombstone,
}

impl Existence {
    /// Merges two dirty states for the same element within a single transaction.
    ///
    /// This defines the state machine for consecutive operations. For example:
    /// - Any operation followed by a deletion (`Tombstone`) results in a `Tombstone`.
    /// - Modifying properties (`Modified`) and changing edge counts (`CounterOnly`) combines into
    ///   `ModifiedWithCounter`.
    fn merge(self, other: Existence) -> Existence {
        use Existence::*;
        match (self, other) {
            (Tombstone, _) | (_, Tombstone) => Tombstone,
            (New, _) | (_, New) => New,
            (ModifiedWithCounter, _) | (_, ModifiedWithCounter) => ModifiedWithCounter,
            (Modified, CounterOnly) | (CounterOnly, Modified) => ModifiedWithCounter,
            (Modified, Modified) => Modified,
            (CounterOnly, CounterOnly) => CounterOnly,
        }
    }
}

// ── LogicalGraph structs ───────────────────────────────────────────────────────
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanConfig {
    pub(crate) scan_vertices_batch_size: u32,
    pub(crate) scan_edges_batch_size: u32,
    pub(crate) get_adjacent_edges_batch_size: u32,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self { scan_vertices_batch_size: 1024, scan_edges_batch_size: 1024, get_adjacent_edges_batch_size: 64 }
    }
}

#[derive(Debug, Default)]
pub(crate) struct StagedSchema {
    pub(crate) staged_vertex_labels: HashSet<LabelId>,
    pub(crate) staged_edge_labels: HashSet<LabelId>,
    pub(crate) staged_prop_keys: HashSet<u16>,
}

impl StagedSchema {
    pub(crate) fn clear(&mut self) {
        self.staged_vertex_labels.clear();
        self.staged_edge_labels.clear();
        self.staged_prop_keys.clear();
    }
}
// ── LogicalGraph ──────────────────────────────────────────────────────────────
/// Query-scoped logical graph wrapping a store transaction.
///
/// Obtained by calling `LogicalGraph::new(store.begin())`. The engine uses this
/// as its sole interface to the graph.
pub(crate) struct LogicalGraph<S: GraphStore> {
    store: S::Txn, // The underlying transaction from the GraphStore.
    vertices: HashMap<VertexKey, Vertex>,
    edges: HashMap<CanonicalEdgeKey, Edge>,
    vertex_degree: HashMap<VertexKey, (u32, u32, LabelId)>,
    dirty: HashMap<CanonicalKey, Existence>,
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
            self.vertices.entry(vt.id).or_insert(vt);
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
    //   get_property (vertex)   none  (delegates to get_vertex)
    //   get_property (edge)     ⚠ edge must be in overlay
    //   add_vertex              none  (get_vertex_degree checks overlay+store)
    //   add_edge                none  (overlay+store for edge; get_vertex_degree
    //                                  for endpoints)
    //   set_property (vertex)   none  (auto-load from store)
    //   set_property (edge)     ⚠ edge must be in overlay
    //   drop_property (vertex)  none  (auto-load from store)
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

        // Verify both endpoints exist (overlay-first via get_vertex_degree, then store).
        let (mut src_out, src_in, src_label_id) = self.get_vertex_degree(cek.src_id)?.ok_or(StoreError::NotFound)?;
        let (dst_out, mut dst_in, dst_label_id) = self.get_vertex_degree(cek.dst_id)?.ok_or(StoreError::NotFound)?;

        src_out += 1;
        dst_in += 1;

        self.vertex_degree.insert(cek.src_id, (src_out, src_in, src_label_id));
        self.mark_dirty(CanonicalKey::Vertex(cek.src_id), Existence::CounterOnly);

        self.vertex_degree.insert(cek.dst_id, (dst_out, dst_in, dst_label_id));
        self.mark_dirty(CanonicalKey::Vertex(cek.dst_id), Existence::CounterOnly);

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
                // Auto-load from store if not yet in overlay.
                if !self.vertices.contains_key(&id) {
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
                // Auto-load from store if not yet in overlay.
                if !self.vertices.contains_key(&id) {
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
                        let val = encoding::encode_schema_prop_value(
                            prop_key_id,
                            cfg.data_type.to_u8(),
                            cfg.cardinality.to_u8(),
                        );
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

// ── LogicalSnapshot ───────────────────────────────────────────────────────────

/// Read-only query context backed by a [`GraphSnapshot`].
///
/// Like `LogicalGraph` it maintains `vertices`, `edges`, and `vertex_degree`
/// caches so repeated reads within one traversal are O(1) map lookups.
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
            self.vertices.entry(vt.id).or_insert(vt);
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Updates a property in-place if its key exists, otherwise appends it to the list.
#[inline]
fn upsert_prop(props: &mut Vec<Property>, prop: &Property) {
    if let Some(p) = props.iter_mut().find(|p| p.key == prop.key) {
        p.value = prop.value.clone();
    } else {
        props.push(prop.clone());
    }
}

/// Evaluates whether an edge matches the specified traversal filters.
///
/// This function verifies that the edge's primary endpoint matches `vertex` in the given `direction`,
/// and optionally applies filters for `label` and the secondary endpoint (`dst`).
fn edge_matches(
    view: &Edge,
    vertex: VertexKey,
    direction: Direction,
    label: Option<LabelId>,
    dst: Option<&[VertexKey]>,
) -> bool {
    let primary = match direction {
        Direction::OUT => view.src_id,
        Direction::IN => view.dst_id,
    };
    if primary != vertex {
        return false;
    }
    if let Some(lbl) = label {
        if view.label_id != lbl {
            return false;
        }
    }
    if let Some(slice) = dst {
        let remote = match direction {
            Direction::OUT => view.dst_id,
            Direction::IN => view.src_id,
        };
        if !slice.contains(&remote) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {

    use smol_str::SmolStr;

    use super::LogicalGraph;
    use crate::store::traits::GraphStore;

    use crate::{
        store::RocksStorage,
        types::{
            element::Property,
            gvalue::Primitive,
            keys::{AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId, VertexKey},
            StoreError,
        },
    };

    fn open() -> (RocksStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        {
            let loaded = store.load_schema(crate::schema::GraphOptions::default()).unwrap();
            let schema = std::sync::Arc::new(std::sync::RwLock::new(loaded));
            let mut c = LogicalGraph::<RocksStorage>::new(store.begin(), schema.clone());
            {
                let mut s = schema.write().unwrap();
                s.resolve_prop_key("age", crate::schema::DataType::Int32).unwrap();
                s.resolve_prop_key("name", crate::schema::DataType::String).unwrap();
                s.resolve_prop_key("x", crate::schema::DataType::Int32).unwrap();
                s.resolve_prop_key("y", crate::schema::DataType::Int32).unwrap();
                s.resolve_prop_key("w", crate::schema::DataType::Float64).unwrap();
                s.resolve_prop_key("a", crate::schema::DataType::Int32).unwrap();
                s.resolve_prop_key("b", crate::schema::DataType::Int32).unwrap();
                s.resolve_prop_key("since", crate::schema::DataType::Int32).unwrap();
                s.resolve_prop_key("nonexistent", crate::schema::DataType::Int32).unwrap();

                s.resolve_vertex_label("person").unwrap();
                s.resolve_vertex_label("software").unwrap();
                s.resolve_edge_label("knows").unwrap();
                s.resolve_edge_label("created").unwrap();
            }
            for label_id in 0..10 {
                c.staged_schema.staged_vertex_labels.insert(label_id);
                c.staged_schema.staged_edge_labels.insert(label_id);
            }
            for prop_key_id in 0..20 {
                c.staged_schema.staged_prop_keys.insert(prop_key_id);
            }
            c.commit().unwrap();
        }
        (store, dir)
    }

    fn ctx(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
        let loaded = store.load_schema(crate::schema::GraphOptions::default()).unwrap();
        let schema = std::sync::Arc::new(std::sync::RwLock::new(loaded));
        LogicalGraph::new(store.begin(), schema)
    }

    fn cek(src: i64, label: u16, dst: i64) -> CanonicalEdgeKey {
        CanonicalEdgeKey { src_id: src, label_id: label, rank: 0, dst_id: dst }
    }

    fn get_adjacent_edges_test(
        c: &mut LogicalGraph<RocksStorage>,
        vertex: VertexKey,
        direction: Direction,
        label: Option<LabelId>,
        dst: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Vec<EdgeKey> {
        c.get_adjacent_edges(
            vertex,
            direction,
            AdjacentEdgesOptions { label, dst, rank: None, start_from: None },
            limit,
        )
        .unwrap()
        .0
    }

    // ── add_vertex / get_vertex ───────────────────────────────────────────────

    #[test]
    fn add_vertex_visible_via_get_vertex() {
        let (store, _dir) = open();
        let mut c = ctx(&store);

        let key = c.add_vertex(100, 1).unwrap();
        let result = c.get_vertex(key).unwrap();
        assert_eq!(result, Some(key));
    }

    #[test]
    fn get_vertex_absent_returns_none() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        assert!(c.get_vertex(9999).unwrap().is_none());
    }

    #[test]
    fn get_vertex_returns_same_idx_on_repeated_calls() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 2).unwrap();
        assert_eq!(c.get_vertex(key).unwrap(), Some(key));
    }

    // ── add_edge / get_edge ───────────────────────────────────────────────────

    #[test]
    fn add_edge_visible_via_get_edge() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        let key = c.add_edge(&k.out_key()).unwrap();
        let result = c.get_edge(&k.out_key()).unwrap().unwrap();
        assert_eq!(k.out_key(), key);
        assert_eq!(result, key);
        assert_eq!((result.primary_id, result.label_id, result.secondary_id), (v1, 5, v2));
    }

    #[test]
    fn add_duplicated_edge_should_fail() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c.add_edge(&k.out_key()).unwrap();

        c.commit().unwrap();

        let mut c = ctx(&store);
        let result = c.add_edge(&k.out_key());
        assert!(result.is_err());
    }

    #[test]
    fn add_duplicated_edge_in_mem_should_fail() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c.add_edge(&k.out_key()).unwrap();

        let result = c.add_edge(&k.out_key());
        assert!(result.is_err());
    }

    #[test]
    fn add_edge_vs_add_same_edge_handmade() {
        let (store, _dir) = open();
        let mut c0 = ctx(&store);
        let v1 = c0.add_vertex(1, 1).unwrap();
        let v2 = c0.add_vertex(2, 1).unwrap();
        c0.commit().unwrap();

        let mut c1 = ctx(&store);
        let mut c2 = ctx(&store);
        let k = cek(v1, 5, v2);

        c1.add_edge(&k.out_key()).unwrap();
        c2.add_edge(&k.out_key()).unwrap();

        c1.commit().unwrap();
        let result = c2.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn commit_resets_overlay_even_on_conflict() {
        let (store, _dir) = open();
        let mut c0 = ctx(&store);
        let v1 = c0.add_vertex(1, 1).unwrap();
        let v2 = c0.add_vertex(2, 1).unwrap();
        c0.commit().unwrap();

        let mut c1 = ctx(&store);
        let mut c2 = ctx(&store);
        let k = cek(v1, 5, v2);

        c1.add_edge(&k.out_key()).unwrap();
        c2.add_edge(&k.out_key()).unwrap();

        c1.commit().unwrap();
        let result = c2.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));

        // The failed commit must still clear the overlay -- see the doc comment on
        // `commit`: callers are allowed to reuse the same context for a fresh attempt
        // rather than discarding it for a brand-new one.
        assert!(c2.dirty.is_empty(), "overlay must be cleared even when the underlying commit conflicts");

        // And the context must genuinely be usable afterward, not just empty.
        let v3 = c2.add_vertex(3, 1).unwrap();
        c2.commit().unwrap();
        assert!(store.get_vertex(v3).unwrap().is_some());
    }
    // ── set_property ─────────────────────────────────────────────────────────

    #[test]
    fn set_property_on_new_vertex_read_your_writes() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();

        let prop = Property { owner: CanonicalKey::Vertex(key), key: 4, value: Primitive::Int32(42) };
        c.set_property(&prop).unwrap();

        let v = c.get_vertex(key).unwrap();
        assert_eq!(v, Some(key));
        let val = c.get_value(&CanonicalKey::Vertex(key), 4).unwrap();
        assert_eq!(val, Some(Primitive::Int32(42)));
    }

    #[test]
    fn set_property_upserts_existing_key() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();

        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
        c.set_property(&prop1).unwrap();
        c.set_property(&prop2).unwrap();

        let _ = c.get_vertex(key).unwrap().unwrap();
        let val = c.get_value(&CanonicalKey::Vertex(key), 6).unwrap();
        assert_eq!(val, Some(Primitive::Int32(2)));
    }

    #[test]
    fn set_property_on_edge_read_your_writes() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c.add_edge(&k.out_key()).unwrap();

        let prop = Property { owner: CanonicalKey::Edge(k), key: 8, value: Primitive::Float64(1.5) };
        c.set_property(&prop).unwrap();

        let _ = c.get_edge(&k.out_key()).unwrap().unwrap();
        let val = c.get_value(&CanonicalKey::Edge(k), 8).unwrap();
        assert_eq!(val, Some(Primitive::Float64(1.5)));
    }

    #[test]
    fn set_vertex_property_vs_set_vertex_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let key = c1.add_vertex(100, 1).unwrap();
        c1.commit().unwrap();

        // Two contexts concurrently update the same property key with different values.
        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
        c2.set_property(&prop1).unwrap();
        c3.set_property(&prop2).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
        let mut c4 = ctx(&store);
        let _ = c4.get_vertex(key).unwrap().unwrap();
        let val = c4.get_value(&CanonicalKey::Vertex(key), 6).unwrap();
        assert_eq!(val, Some(Primitive::Int32(1)));
    }

    #[test]
    fn set_edge_property_vs_set_edge_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c1.add_edge(&k.out_key()).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        c2.get_edge(&k.out_key()).unwrap();
        c3.get_edge(&k.out_key()).unwrap();
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(2) };
        c2.set_property(&prop1).unwrap();
        c3.set_property(&prop2).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    // ── drop_property ─────────────────────────────────────────────────────────

    #[test]
    fn drop_property_removes_key() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();

        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 9, value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 10, value: Primitive::Int32(2) };
        c.set_property(&prop1).unwrap();
        c.set_property(&prop2).unwrap();
        c.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 9, value: Primitive::Null }).unwrap();

        let _ = c.get_vertex(key).unwrap().unwrap();
        let val_a = c.get_value(&CanonicalKey::Vertex(key), 9).unwrap();
        let val_b = c.get_value(&CanonicalKey::Vertex(key), 10).unwrap();
        assert_eq!(val_a, None);
        assert_eq!(val_b, Some(Primitive::Int32(2)));
    }

    #[test]
    fn drop_property_on_missing_key_is_noop() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 12, value: Primitive::Null }).unwrap();
        let _ = c.get_vertex(key).unwrap().unwrap();
        let val = c.get_value(&CanonicalKey::Vertex(key), 12).unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn drop_vertex_property_vs_set_vertex_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let key = c1.add_vertex(100, 1).unwrap();
        let prop = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
        c1.set_property(&prop).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        c2.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Null }).unwrap();
        let prop = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
        c3.set_property(&prop).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn set_vertex_property_vs_drop_vertex_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let key = c1.add_vertex(100, 1).unwrap();
        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
        c1.set_property(&prop1).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
        c2.set_property(&prop2).unwrap();
        c3.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Null }).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn drop_edge_property_vs_set_edge_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c1.add_edge(&k.out_key()).unwrap();
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
        c1.set_property(&prop1).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let _ = c2.get_edge(&k.out_key()).unwrap();
        let _ = c3.get_edge(&k.out_key()).unwrap();
        c2.drop_property(&Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Null }).unwrap();
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(2) };
        c3.set_property(&prop2).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn set_edge_property_vs_drop_edge_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c1.add_edge(&k.out_key()).unwrap();
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
        c1.set_property(&prop1).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let _ = c2.get_edge(&k.out_key()).unwrap();
        let _ = c3.get_edge(&k.out_key()).unwrap();
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(2) };
        c2.set_property(&prop2).unwrap();
        let prop3 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Null };
        c3.drop_property(&prop3).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    // ── drop_element ──────────────────────────────────────────────────────────

    #[test]
    fn tombstoned_vertex_invisible_to_get_vertex() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        let v = c.get_vertex(key).unwrap().unwrap();
        assert_eq!(v, key);
        c.drop_element(&CanonicalKey::Vertex(key)).unwrap();
        assert!(c.get_vertex(key).unwrap().is_none());
    }

    #[test]
    fn tombstoned_edge_invisible_to_get_edge() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c.add_edge(&k.out_key()).unwrap();
        let e = c.get_edge(&k.out_key()).unwrap().unwrap();
        assert_eq!(e.canonical_edge_key(), k);
        c.drop_element(&CanonicalKey::Edge(k)).unwrap();
        assert!(c.get_edge(&k.out_key()).unwrap().is_none());
    }

    #[test]
    fn drop_vertex_with_edges_errors() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c.add_edge(&k.out_key()).unwrap();

        let err = c.drop_element(&CanonicalKey::Vertex(v1));
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().to_string(), "cannot drop vertex with incident edges");

        c.commit().unwrap();

        let mut c2 = ctx(&store);
        let err = c2.drop_element(&CanonicalKey::Vertex(v1));
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().to_string(), "cannot drop vertex with incident edges");
    }

    #[test]
    fn set_property_on_tombstoned_vertex_errors() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.drop_element(&CanonicalKey::Vertex(key)).unwrap();
        let prop = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
        let err = c.set_property(&prop);
        assert!(err.is_err());
        assert_eq!(err.unwrap_err().to_string(), "element is tombstoned");
    }

    #[test]
    fn add_edge_vs_drop_edge_handmade() {
        let (store, _dir) = open();
        let mut c0 = ctx(&store);
        let v1 = c0.add_vertex(1, 1).unwrap();
        let v2 = c0.add_vertex(2, 1).unwrap();
        c0.commit().unwrap();

        let mut c1 = ctx(&store);
        let mut c2 = ctx(&store);
        let k = cek(v1, 5, v2);

        c1.add_edge(&k.out_key()).unwrap();
        c2.add_edge(&k.out_key()).unwrap();

        c1.commit().unwrap();
        c2.drop_element(&CanonicalKey::Edge(k)).unwrap();
        let result = c2.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn drop_vertex_vs_add_edge_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 2).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);

        let k = cek(v1, 5, v2);
        c2.add_edge(&k.out_key()).unwrap();
        c3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();

        assert!(c3.commit().is_ok(), "c3 should commit successfully");

        let result = c2.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn add_edge_vs_drop_vertex_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 2).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);

        let k = cek(v1, 5, v2);
        c2.add_edge(&k.out_key()).unwrap();
        c3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();

        assert!(c2.commit().is_ok(), "c2 should commit successfully");

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn drop_dst_vertex_vs_add_edge_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 2).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);

        let k = cek(v1, 5, v2);
        c2.add_edge(&k.out_key()).unwrap();
        c3.drop_element(&CanonicalKey::Vertex(v2)).unwrap();

        assert!(c3.commit().is_ok(), "c3 should commit successfully");

        let result = c2.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn add_edge_vs_drop_dst_vertex_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 2).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);

        let k = cek(v1, 5, v2);
        c2.add_edge(&k.out_key()).unwrap();
        c3.drop_element(&CanonicalKey::Vertex(v2)).unwrap();

        assert!(c2.commit().is_ok(), "c2 should commit successfully");

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn set_edge_property_vs_drop_edge_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c1.add_edge(&k.out_key()).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let _ = c2.get_edge(&k.out_key()).unwrap();
        let _ = c3.get_edge(&k.out_key()).unwrap();
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
        c2.set_property(&prop1).unwrap();
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Null };
        c3.drop_property(&prop2).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    #[test]
    fn drop_edge_vs_set_edge_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let v1 = c1.add_vertex(1, 1).unwrap();
        let v2 = c1.add_vertex(2, 1).unwrap();
        let k = cek(v1, 5, v2);
        c1.add_edge(&k.out_key()).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let _ = c2.get_edge(&k.out_key()).unwrap();
        let _ = c3.get_edge(&k.out_key()).unwrap();
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
        c2.drop_element(&CanonicalKey::Edge(k)).unwrap();
        c3.set_property(&prop1).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
    }

    // ── commit ────────────────────────────────────────────────────────────────

    #[test]
    fn commit_persists_vertex_to_store() {
        let (store, _dir) = open();
        let id = {
            let mut c = ctx(&store);
            let key = c.add_vertex(77, 7).unwrap();
            let prop =
                Property { owner: CanonicalKey::Vertex(key), key: 5, value: Primitive::String(SmolStr::new("Alice")) };
            c.set_property(&prop).unwrap();
            c.commit().unwrap();
            key
        };

        let mut fv = store.get_vertex(id).unwrap().unwrap();
        assert_eq!(fv.label_id, 7);
        assert_eq!(fv.all_props().len(), 1);
        assert_eq!(fv.all_props()[0].value, Primitive::String(SmolStr::new("Alice")));
    }

    #[test]
    fn commit_persists_edge_to_store() {
        let (store, _dir) = open();
        let (v1, v2) = {
            let mut c0 = ctx(&store);
            let v_1 = c0.add_vertex(1, 1).unwrap();
            let v_2 = c0.add_vertex(2, 1).unwrap();
            c0.commit().unwrap();
            (v_1, v_2)
        };
        let k = cek(v1, 3, v2);
        {
            let mut c = ctx(&store);
            c.add_edge(&k.out_key()).unwrap();
            let prop = Property { owner: CanonicalKey::Edge(k), key: 11, value: Primitive::Int32(99) };
            c.set_property(&prop).unwrap();
            c.commit().unwrap();
        }

        let mut edges = store.get_edges(v1, Direction::OUT, None, None, None).unwrap();
        assert_eq!(edges.len(), 1);
        let e = &mut edges[0];
        assert_eq!(e.all_props().len(), 1);
        assert_eq!(e.all_props()[0].value, Primitive::Int32(99));
    }

    #[test]
    fn commit_persists_vertex_deletion() {
        let (store, _dir) = open();
        let id = {
            let mut c = ctx(&store);
            let key = c.add_vertex(100, 1).unwrap();
            c.commit().unwrap();
            key
        };
        assert!(store.get_vertex(id).unwrap().is_some());

        {
            let mut c = ctx(&store);
            let _ = c.get_vertex(id).unwrap();
            c.drop_element(&CanonicalKey::Vertex(id)).unwrap();
            c.commit().unwrap();
        }
        assert!(store.get_vertex(id).unwrap().is_none());
    }

    #[test]
    fn commit_resets_overlay_for_reuse() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.commit().unwrap();
        // Overlay is cleared — the same key must now load from store, not the old overlay.
        let vertex = c.get_vertex(key).unwrap().unwrap();
        assert_eq!(vertex, key);
    }

    // ── abort ─────────────────────────────────────────────────────────────────

    #[test]
    fn abort_discards_pending_writes() {
        let (store, _dir) = open();
        let id = {
            let mut c = ctx(&store);
            let key = c.add_vertex(100, 1).unwrap();
            c.abort();
            key
        };
        assert!(store.get_vertex(id).unwrap().is_none());
    }

    // ── get_edges ─────────────────────────────────────────────────────────────

    #[test]
    fn get_edges_returns_new_dirty_edges_before_commit() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v10 = c.add_vertex(10, 1).unwrap();
        let v20 = c.add_vertex(20, 1).unwrap();
        c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();

        let edges = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn get_edges_filters_tombstoned_edges() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v10 = c.add_vertex(10, 1).unwrap();
        let v20 = c.add_vertex(20, 1).unwrap();
        c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
        c.drop_element(&CanonicalKey::Edge(cek(v1, 1, v10))).unwrap();

        let edges = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn get_edges_direction_in_vs_out() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        c.add_edge(&cek(v1, 1, v2).out_key()).unwrap();

        let out = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
        let in_ = get_adjacent_edges_test(&mut c, v2, Direction::IN, None, None, None);
        assert_eq!(out.len(), 1);
        assert_eq!(in_.len(), 1);
        // Vertex v1 has no incoming edges; vertex v2 has no outgoing.
        assert!(get_adjacent_edges_test(&mut c, v1, Direction::IN, None, None, None).is_empty());
        assert!(get_adjacent_edges_test(&mut c, v2, Direction::OUT, None, None, None).is_empty());
    }

    #[test]
    fn get_edges_label_filter() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v10 = c.add_vertex(10, 1).unwrap();
        let v20 = c.add_vertex(20, 1).unwrap();
        let v30 = c.add_vertex(30, 1).unwrap();
        c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
        c.add_edge(&cek(v1, 2, v20).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v30).out_key()).unwrap();

        let label1 = get_adjacent_edges_test(&mut c, v1, Direction::OUT, Some(1), None, None);
        assert_eq!(label1.len(), 2);
        assert!(label1.iter().all(|ek| ek.label_id == 1));

        let label2 = get_adjacent_edges_test(&mut c, v1, Direction::OUT, Some(2), None, None);
        assert_eq!(label2.len(), 1);
    }

    #[test]
    fn get_edges_dst_filter() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v10 = c.add_vertex(10, 1).unwrap();
        let v20 = c.add_vertex(20, 1).unwrap();
        let v30 = c.add_vertex(30, 1).unwrap();
        c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v30).out_key()).unwrap();

        let result = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, Some(&[v10, v30]), None);
        assert_eq!(result.len(), 2);
        let mut secondaries: Vec<i64> = result.iter().map(|ek| ek.secondary_id).collect();
        secondaries.sort_unstable();
        let mut expected = vec![v10, v30];
        expected.sort_unstable();
        assert_eq!(secondaries, expected);
    }

    #[test]
    fn get_edges_limit_filter() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v10 = c.add_vertex(10, 1).unwrap();
        let v20 = c.add_vertex(20, 1).unwrap();
        let v30 = c.add_vertex(30, 1).unwrap();
        c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
        c.add_edge(&cek(v1, 1, v30).out_key()).unwrap();

        let result = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, Some(2));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn get_edges_merges_committed_and_dirty() {
        let (store, _dir) = open();

        // Commit one edge, then add another in a new context.
        let (v1, v10, v20) = {
            let mut c0 = ctx(&store);
            let v_1 = c0.add_vertex(1, 1).unwrap();
            let v_10 = c0.add_vertex(10, 1).unwrap();
            let v_20 = c0.add_vertex(20, 1).unwrap();
            c0.commit().unwrap();
            (v_1, v_10, v_20)
        };

        let k1 = cek(v1, 1, v10);
        {
            let mut c = ctx(&store);
            c.add_edge(&k1.out_key()).unwrap();
            c.commit().unwrap();
        }

        let mut c = ctx(&store);
        c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
        let edges = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
        assert_eq!(edges.len(), 2);
    }

    // ── Concurrency & Conflict Test Matrix ────────────────────────────────────
    //
    // This matrix documents the test coverage for Optimistic Concurrency Control
    // (OCC) conflicts. It shows which concurrent operations on the same or
    // related elements are tested to guarantee a `StoreError::Conflict` on `commit()`.
    // Both commit orders (Txn1 -> Txn2, and Txn2 -> Txn1) are tested for every cell
    // in `conflict_matrix`, alongside specific handmade tests.
    //
    // | Txn 1 \ Txn 2   | Add Edge       | Drop Edge      | Set Prop(E)    | Drop Prop(E)   | Set Prop(V)    | Drop Prop(V)   | Drop Vertex    |
    // |-----------------|----------------|----------------|----------------|----------------|----------------|----------------|----------------|
    // | Add Edge        | [1], [20]      | [2], [21]      | N/A            | N/A            | [3]            | [4]            | [5], [22..25]  |
    // | Drop Edge       | [2], [21]      | [6]            | [7], [26,27]   | [8]            | [9]            | [10]           | N/A            |
    // | Set Prop(E)     | N/A            | [7], [26,27]   | [11], [28]     | [12], [29,30]  | N/A            | N/A            | N/A            |
    // | Drop Prop(E)    | N/A            | [8]            | [12], [29,30]  | [13]           | N/A            | N/A            | N/A            |
    // | Set Prop(V)     | [3]            | [9]            | N/A            | N/A            | [14], [31]     | [15], [32,33]  | [16]           |
    // | Drop Prop(V)    | [4]            | [10]           | N/A            | N/A            | [15], [32,33]  | [17]           | [18]           |
    // | Drop Vertex     | [5], [22..25]  | N/A            | N/A            | N/A            | [16]           | [18]           | [19]           |
    //
    // ── Automated conflict_matrix tests:
    // [1]  add_edge_vs_add_edge
    // [2]  add_edge_vs_drop_edge
    // [3]  add_edge_vs_set_vertex_property
    // [4]  add_edge_vs_drop_vertex_property
    // [5]  add_edge_vs_drop_vertex
    // [6]  drop_edge_vs_drop_edge
    // [7]  drop_edge_vs_set_edge_property
    // [8]  drop_edge_vs_drop_edge_property
    // [9]  drop_edge_vs_set_vertex_property
    // [10] drop_edge_vs_drop_vertex_property
    // [11] set_edge_property_vs_set_edge_property
    // [12] set_edge_property_vs_drop_edge_property
    // [13] drop_edge_property_vs_drop_edge_property
    // [14] set_vertex_property_vs_set_vertex_property
    // [15] set_vertex_property_vs_drop_vertex_property
    // [16] set_vertex_property_vs_drop_vertex
    // [17] drop_vertex_property_vs_drop_vertex_property
    // [18] drop_vertex_property_vs_drop_vertex
    // [19] drop_vertex_vs_drop_vertex
    //
    // ── Handmade concurrent tests:
    // [20] add_edge_vs_add_edge_handmade
    // [21] add_edge_vs_drop_edge_handmade
    // [22] drop_vertex_vs_add_edge_handmade
    // [23] add_edge_vs_drop_vertex_handmade
    // [24] drop_dst_vertex_vs_add_edge_handmade
    // [25] add_edge_vs_drop_dst_vertex_handmade
    // [26] set_edge_property_vs_drop_edge_handmade
    // [27] drop_edge_vs_set_edge_property_handmade
    // [28] set_edge_property_vs_set_edge_property_handmade
    // [29] drop_edge_property_vs_set_edge_property_handmade
    // [30] set_edge_property_vs_drop_edge_property_handmade
    // [31] set_vertex_property_vs_set_vertex_property_handmade
    // [32] drop_vertex_property_vs_set_vertex_property_handmade
    // [33] set_vertex_property_vs_drop_vertex_property_handmade
    //
    // N/A: Combinations that don't conflict (mutate distinct elements without read dependencies)
    // or are impossible (e.g. dropping a vertex with an existing edge fails validation early).
    // ──────────────────────────────────────────────────────────────────────────

    mod conflict_matrix {
        use super::*;

        fn run_non_conflict<State: Copy, Setup, Op1, Op2>(setup: Setup, op1: Op1, op2: Op2)
        where
            Setup: Fn(&mut LogicalGraph<RocksStorage>) -> State,
            Op1: Fn(&mut LogicalGraph<RocksStorage>, State),
            Op2: Fn(&mut LogicalGraph<RocksStorage>, State),
        {
            // Order 1: Txn1 commits, Txn2 conflicts
            {
                let (store, _dir) = open();
                let mut c0 = ctx(&store);
                let state = setup(&mut c0);
                c0.commit().unwrap();

                let mut c1 = ctx(&store);
                let mut c2 = ctx(&store);

                op1(&mut c1, state);
                op2(&mut c2, state);

                c1.commit().unwrap();
                let res = c2.commit();
                assert!(res.is_ok(), "unexpected conflict in non-conflicting operations. Order 1 (Txn1 commits, Txn2 should succeed) failed with error: {:?}", res.err());
            }

            // Order 2: Txn2 commits, Txn1 conflicts
            {
                let (store, _dir) = open();
                let mut c0 = ctx(&store);
                let state = setup(&mut c0);
                c0.commit().unwrap();

                let mut c1 = ctx(&store);
                let mut c2 = ctx(&store);

                op1(&mut c1, state);
                op2(&mut c2, state);

                c2.commit().unwrap();
                let res = c1.commit();
                assert!(res.is_ok(), "unexpected conflict in non-conflicting operations. Order 2 (Txn2 commits, Txn1 should succeed) failed with error: {:?}", res.err());
            }
        }

        fn run_conflict<State: Copy, Setup, Op1, Op2>(setup: Setup, op1: Op1, op2: Op2)
        where
            Setup: Fn(&mut LogicalGraph<RocksStorage>) -> State,
            Op1: Fn(&mut LogicalGraph<RocksStorage>, State),
            Op2: Fn(&mut LogicalGraph<RocksStorage>, State),
        {
            // Order 1: Txn1 commits, Txn2 conflicts
            {
                let (store, _dir) = open();
                let mut c0 = ctx(&store);
                let state = setup(&mut c0);
                c0.commit().unwrap();

                let mut c1 = ctx(&store);
                let mut c2 = ctx(&store);

                op1(&mut c1, state);
                op2(&mut c2, state);

                c1.commit().unwrap();
                let res = c2.commit();
                assert!(
                    matches!(res, Err(StoreError::Conflict)),
                    "Order 1 (Txn1 commits, Txn2 conflicts) failed. Expected Conflict, got {:?}",
                    res
                );
            }

            // Order 2: Txn2 commits, Txn1 conflicts
            {
                let (store, _dir) = open();
                let mut c0 = ctx(&store);
                let state = setup(&mut c0);
                c0.commit().unwrap();

                let mut c1 = ctx(&store);
                let mut c2 = ctx(&store);

                op1(&mut c1, state);
                op2(&mut c2, state);

                c2.commit().unwrap();
                let res = c1.commit();
                assert!(
                    matches!(res, Err(StoreError::Conflict)),
                    "Order 2 (Txn2 commits, Txn1 conflicts) failed. Expected Conflict, got {:?}",
                    res
                );
            }
        }

        #[test]
        fn add_edge_vs_add_edge() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    (v1, v2)
                },
                |c, (v1, v2)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
                |c, (v1, v2)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_add_edge_with_same_vertex() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    (v1, v2, v3)
                },
                |c, (v1, v2, _v3)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
                |c, (v1, _v2, v3)| {
                    c.add_edge(&cek(v1, 5, v3).out_key()).unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_drop_edge() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    let e1 = cek(v1, 5, v2);
                    c.add_edge(&e1.out_key()).unwrap();
                    (v1, e1, v3)
                },
                |c, (v1, _, v3)| {
                    c.add_edge(&cek(v1, 6, v3).out_key()).unwrap();
                },
                |c, (_, e1, _)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e1)).unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_drop_edge_with_same_vertex() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    let e1 = cek(v1, 5, v2);
                    c.add_edge(&e1.out_key()).unwrap();
                    (v1, e1, v3)
                },
                |c, (v1, _, v3)| {
                    c.add_edge(&cek(v1, 6, v3).out_key()).unwrap();
                },
                |c, (_, e1, _)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e1)).unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_set_vertex_property() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    (v1, v2)
                },
                |c, (v1, v2)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
                |c, (v1, _)| {
                    c.get_vertex(v1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_drop_vertex_property() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    (v1, v2)
                },
                |c, (v1, v2)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
                |c, (v1, _)| {
                    c.get_vertex(v1).unwrap();
                    c.drop_property(&Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Null })
                        .unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_drop_vertex() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    (v1, v2)
                },
                |c, (v1, v2)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
                |c, (_, v2)| {
                    c.get_vertex(v2).unwrap();
                    c.drop_element(&CanonicalKey::Vertex(v2)).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_drop_edge() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_drop_edge_with_same_vertex() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    let e2 = cek(v1, 6, v3);
                    c.add_edge(&e.out_key()).unwrap();
                    c.add_edge(&e2.out_key()).unwrap();
                    (e, e2)
                },
                |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e1)).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e2)).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_set_edge_property() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_drop_edge_property() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_property(&Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null })
                        .unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_set_vertex_property() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    (v1, e)
                },
                |c, (_, e)| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
                |c, (v1, _)| {
                    c.get_vertex(v1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_drop_vertex_property() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    (v1, e)
                },
                |c, (_, e)| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
                |c, (v1, _)| {
                    c.get_vertex(v1).unwrap();
                    c.drop_property(&Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Null })
                        .unwrap();
                },
            );
        }

        #[test]
        fn set_edge_property_vs_set_edge_property() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(2) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_edge_property_vs_set_edge_property_with_same_vertex() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    let e2 = cek(v1, 6, v3);
                    c.add_edge(&e.out_key()).unwrap();
                    c.add_edge(&e2.out_key()).unwrap();
                    (e, e2)
                },
                |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e1), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Int32(2) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_edge_property_vs_drop_edge_property() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_edge_property_vs_drop_edge_property_with_same_vertex() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    let e2 = cek(v1, 6, v3);
                    c.add_edge(&e.out_key()).unwrap();
                    c.add_edge(&e2.out_key()).unwrap();
                    let prop1 = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    let prop2 = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Int32(2) };
                    c.set_property(&prop1).unwrap();
                    c.set_property(&prop2).unwrap();
                    (e, e2)
                },
                |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e1), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_property_vs_drop_edge_property() {
            run_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    c.add_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_property_vs_drop_edge_property_with_same_vertex() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    let v3 = c.add_vertex(3, 1).unwrap();
                    let e = cek(v1, 5, v2);
                    let e2 = cek(v1, 6, v3);
                    c.add_edge(&e.out_key()).unwrap();
                    c.add_edge(&e2.out_key()).unwrap();
                    let prop1 = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                    let prop2 = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Int32(2) };
                    c.set_property(&prop1).unwrap();
                    c.set_property(&prop2).unwrap();
                    (e, e2)
                },
                |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    let val = c.get_value(&CanonicalKey::Edge(e1), 6).unwrap();
                    assert_eq!(val, Some(Primitive::Int32(1)));
                    let prop = Property { owner: CanonicalKey::Edge(e1), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    let prop = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_vertex_property_vs_set_vertex_property() {
            run_conflict(
                |c| c.add_vertex(100, 1).unwrap(),
                |c, v| {
                    c.get_vertex(v).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(2) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_vertex_property_vs_drop_vertex_property() {
            run_conflict(
                |c| {
                    let v = c.add_vertex(100, 1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    v
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(2) };
                    c.set_property(&prop).unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap().unwrap();
                    let val = c.get_value(&CanonicalKey::Vertex(v), 6).unwrap();
                    assert_eq!(val, Some(Primitive::Int32(1)));
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_vertex_property_vs_drop_vertex() {
            run_conflict(
                |c| c.add_vertex(100, 1).unwrap(),
                |c, v| {
                    c.get_vertex(v).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
                },
            );
        }

        #[test]
        fn drop_vertex_property_vs_drop_vertex_property() {
            run_conflict(
                |c| {
                    let v = c.add_vertex(100, 1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    v
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_property(&Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null })
                        .unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_property(&Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null })
                        .unwrap();
                },
            );
        }

        #[test]
        fn drop_vertex_property_vs_drop_vertex() {
            run_conflict(
                |c| {
                    let v = c.add_vertex(100, 1).unwrap();
                    let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    v
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_property(&Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null })
                        .unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
                },
            );
        }

        #[test]
        fn drop_vertex_vs_drop_vertex() {
            run_conflict(
                |c| c.add_vertex(100, 1).unwrap(),
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
                },
            );
        }
    }

    // ── Integration tests ─────────────────────────────────────────────────────

    #[test]
    fn sequential_contexts_accumulate_edges() {
        let (store, _dir) = open();

        // Build edges in separate contexts; each must see all previously committed edges.
        let hub = {
            let mut c = ctx(&store);
            let key = c.add_vertex(100, 1).unwrap();
            c.commit().unwrap();
            key
        };

        let spokes: Vec<i64> = (0..4)
            .map(|i| {
                let mut c = ctx(&store);
                let key = c.add_vertex(i, 1).unwrap();
                c.add_edge(&cek(hub, 1, key).out_key()).unwrap();
                c.commit().unwrap();
                key
            })
            .collect();

        // A final context must see all 4 outgoing edges from hub.
        let mut c = ctx(&store);
        let out = get_adjacent_edges_test(&mut c, hub, Direction::OUT, Some(1), None, None);
        assert_eq!(out.len(), 4);

        // check vertex counter is correct after multiple contexts
        let (out_e, in_e, _label) = c.vertex_degree_for_test(hub).unwrap().unwrap();
        assert_eq!(out_e, 4);
        assert_eq!(in_e, 0);

        // The 4 edges must land at the 4 spoke vertices.
        let mut dst_ids: Vec<i64> = out.iter().map(|ek| ek.secondary_id).collect();
        dst_ids.sort_unstable();
        let mut expected = spokes.clone();
        expected.sort_unstable();
        assert_eq!(dst_ids, expected);

        // Each spoke has exactly one incoming edge from hub.
        for &spoke in &spokes {
            let in_edges = get_adjacent_edges_test(&mut c, spoke, Direction::IN, Some(1), None, None);
            assert_eq!(in_edges.len(), 1);
            assert_eq!(in_edges[0].secondary_id, hub);
        }
    }

    #[test]
    fn two_concurrent_contexts_build_graph_fourth_reads_all() {
        let (store, _dir) = open();

        // ctx1 — person: Alice
        let mut c1 = ctx(&store);
        let alice = {
            let key = c1.add_vertex(101, 1).unwrap();
            let name_prop =
                Property { owner: CanonicalKey::Vertex(key), key: 5, value: Primitive::String(SmolStr::new("Alice")) };
            c1.set_property(&name_prop).unwrap();
            let age_prop = Property { owner: CanonicalKey::Vertex(key), key: 4, value: Primitive::Int32(30) };
            c1.set_property(&age_prop).unwrap();
            key
        };

        // ctx2 — person: Bob
        let mut c2 = ctx(&store);
        let bob = {
            let key = c2.add_vertex(102, 1).unwrap();
            let name_prop =
                Property { owner: CanonicalKey::Vertex(key), key: 5, value: Primitive::String(SmolStr::new("Bob")) };
            c2.set_property(&name_prop).unwrap();
            let age_prop = Property { owner: CanonicalKey::Vertex(key), key: 4, value: Primitive::Int32(25) };
            c2.set_property(&age_prop).unwrap();
            key
        };

        c2.commit().unwrap();
        c1.commit().unwrap(); // commit after c2 to test concurrent visibility of both contexts

        // ctx3 — city: London + two "lives_in" edges (label=2) from each person
        let london = {
            let mut c = ctx(&store);
            let city_key = c.add_vertex(201, 2).unwrap();
            let name_prop = Property {
                owner: CanonicalKey::Vertex(city_key),
                key: 5,
                value: Primitive::String(SmolStr::new("London")),
            };
            c.set_property(&name_prop).unwrap();
            // Alice -> London
            let e1 = cek(alice, 2, city_key);
            c.add_edge(&e1.out_key()).unwrap();
            let since_prop = Property { owner: CanonicalKey::Edge(e1), key: 11, value: Primitive::Int32(2015) };
            c.set_property(&since_prop).unwrap();
            // Bob -> London
            let e2 = cek(bob, 2, city_key);
            c.add_edge(&e2.out_key()).unwrap();
            let since_prop2 = Property { owner: CanonicalKey::Edge(e2), key: 11, value: Primitive::Int32(2019) };
            c.set_property(&since_prop2).unwrap();
            c.commit().unwrap();
            city_key
        };

        // ctx4 — read-only verification
        let mut c = ctx(&store);

        // Vertices survive across contexts.
        let _ = c.get_vertex(alice).unwrap().unwrap();
        assert_eq!(
            c.get_value(&CanonicalKey::Vertex(alice), 5).unwrap(),
            Some(Primitive::String(SmolStr::new("Alice")))
        );
        assert_eq!(c.get_value(&CanonicalKey::Vertex(alice), 4).unwrap(), Some(Primitive::Int32(30)));
        let (alice_out_e, alice_in_e, _label) = c.vertex_degree_for_test(alice).unwrap().unwrap();
        assert_eq!(alice_out_e, 1);
        assert_eq!(alice_in_e, 0);

        let _ = c.get_vertex(bob).unwrap().unwrap();
        assert_eq!(c.get_value(&CanonicalKey::Vertex(bob), 5).unwrap(), Some(Primitive::String(SmolStr::new("Bob"))));
        let (bob_out_e, bob_in_e, _label) = c.vertex_degree_for_test(bob).unwrap().unwrap();
        assert_eq!(bob_out_e, 1);
        assert_eq!(bob_in_e, 0);

        let _ = c.get_vertex(london).unwrap().unwrap();
        assert_eq!(
            c.get_value(&CanonicalKey::Vertex(london), 5).unwrap(),
            Some(Primitive::String(SmolStr::new("London")))
        );
        let (london_out_e, london_in_e, _label) = c.vertex_degree_for_test(london).unwrap().unwrap();
        assert_eq!(london_out_e, 0);
        assert_eq!(london_in_e, 2);

        // Both outgoing "lives_in" edges from Alice land at London.
        let alice_out = get_adjacent_edges_test(&mut c, alice, Direction::OUT, Some(2), None, None);
        assert_eq!(alice_out.len(), 1);
        let e_ek = alice_out[0];
        assert_eq!(e_ek.secondary_id, london);
        let since_val = c.get_value(&CanonicalKey::Edge(e_ek.canonical_edge_key()), 11).unwrap();
        assert_eq!(since_val, Some(Primitive::Int32(2015)));

        // London has two incoming edges: one from Alice, one from Bob.
        let london_in = get_adjacent_edges_test(&mut c, london, Direction::IN, Some(2), None, None);
        assert_eq!(london_in.len(), 2);
        let mut src_ids: Vec<i64> = london_in.iter().map(|ek| ek.secondary_id).collect();
        src_ids.sort_unstable();
        assert_eq!(src_ids, vec![alice.min(bob), alice.max(bob)]);
    }

    // Tests that operations depending on vertex counters (like adding an edge or dropping the vertex)
    // succeed during the transaction due to snapshot isolation but fail with a Conflict at commit time
    // when the vertex is deleted concurrently by another transaction.
    #[test]
    fn concurrent_vertex_deletion_fails_dependent_operations() {
        let (store, _dir) = open();

        // step 1, insert a vertex and set properties, commit the transaction txn1
        let mut txn1 = ctx(&store);
        let v1 = txn1.add_vertex(1, 1).unwrap();
        txn1.add_vertex(2, 1).unwrap();
        let v3 = txn1.add_vertex(3, 1).unwrap();
        let name_prop =
            Property { owner: CanonicalKey::Vertex(v1), key: 5, value: Primitive::String(SmolStr::new("Alice")) };
        txn1.set_property(&name_prop).unwrap();
        txn1.commit().unwrap();

        // step 2, in a new Transaction txn2, get_vertex
        let mut txn2 = ctx(&store);
        assert!(txn2.get_vertex(v1).unwrap().is_some());
        assert!(txn2.get_vertex(v3).unwrap().is_some());

        // step 3, the vertices were deleted in another transaction, commit the deleting transaction which should
        // succeed
        let mut txn3 = ctx(&store);
        txn3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();
        txn3.drop_element(&CanonicalKey::Vertex(v3)).unwrap();
        txn3.commit().unwrap();

        // Under Repeatable Reads, adding an edge in txn2 using the vertex (which is still visible in txn2's snapshot)
        // should succeed
        assert!(txn2.add_edge(&cek(v1, 5, 2).out_key()).is_ok());

        // Similarly, dropping v3 in txn2 (still visible, degree 0) should succeed
        assert!(txn2.drop_element(&CanonicalKey::Vertex(v3)).is_ok());

        // But when txn2 tries to commit, it should fail with Conflict due to the concurrent deletion committed by txn3
        let commit_err = txn2.commit();
        assert!(matches!(commit_err, Err(StoreError::Conflict)));
    }

    #[test]
    fn test_logical_scan_vertices_overlays() {
        let (store, _dir) = open();

        // 1. Add some committed vertices: 1, 2, 3
        let mut txn = ctx(&store);
        txn.add_vertex(1, 1).unwrap();
        txn.add_vertex(2, 1).unwrap();
        txn.add_vertex(3, 1).unwrap();
        txn.commit().unwrap();

        // 2. Start a new transaction. Add 4 (dirty new), delete 2 (tombstone)
        let mut txn = ctx(&store);
        txn.add_vertex(4, 1).unwrap();
        txn.drop_element(&CanonicalKey::Vertex(2)).unwrap();

        // 3. Scan vertices with limit 2
        let (batch1, cursor1) = txn.scan_vertices(None, None, 2).unwrap();
        assert_eq!(batch1, vec![1]);
        assert_eq!(cursor1, Some(2));

        // 4. Scan next batch using cursor1
        let (batch2, cursor2) = txn.scan_vertices(None, cursor1, 2).unwrap();
        assert_eq!(batch2, vec![3, 4]);
        assert_eq!(cursor2, None);
    }

    #[test]
    fn test_logical_scan_edges_overlays() {
        let (store, _dir) = open();

        // 1. Add some committed vertices and edges
        let mut txn = ctx(&store);
        txn.add_vertex(1, 1).unwrap();
        txn.add_vertex(2, 1).unwrap();
        txn.add_vertex(3, 1).unwrap();

        let ek1 = cek(1, 10, 2).out_key();
        let ek2 = cek(2, 10, 3).out_key();
        let ek3 = cek(1, 10, 3).out_key();

        txn.add_edge(&ek1).unwrap();
        txn.add_edge(&ek2).unwrap();
        txn.add_edge(&ek3).unwrap();
        txn.commit().unwrap();

        // 2. Start a new transaction. Add ek4 (dirty), delete ek2 (tombstone)
        let mut txn = ctx(&store);
        let ek4 = cek(2, 10, 1).out_key();
        txn.add_edge(&ek4).unwrap();

        // Edge must be loaded into memory before drop
        txn.get_edge(&ek2).unwrap().unwrap();
        txn.drop_element(&CanonicalKey::Edge(ek2.canonical_edge_key())).unwrap();

        // 3. Scan edges with limit 2
        let (batch1, cursor1) = txn.scan_edges(None, None, 2).unwrap();
        assert_eq!(batch1.len(), 2);
        assert_eq!(batch1[0], ek1);
        assert_eq!(batch1[1], ek3);
        assert_eq!(cursor1, Some(ek3.canonical_edge_key()));

        // 4. Scan next batch using cursor1
        let (batch2, cursor2) = txn.scan_edges(None, cursor1, 2).unwrap();
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0], ek4);
        assert_eq!(cursor2, None);
    }

    #[test]
    fn test_logical_get_adjacent_edges_overlays() {
        let (store, _dir) = open();

        // 1. Add some committed vertices and edges from vertex 1
        let mut txn = ctx(&store);
        txn.add_vertex(1, 1).unwrap();
        txn.add_vertex(2, 1).unwrap();
        txn.add_vertex(3, 1).unwrap();
        txn.add_vertex(4, 1).unwrap();

        let ek1 = cek(1, 10, 2).out_key();
        let ek2 = cek(1, 10, 3).out_key();

        txn.add_edge(&ek1).unwrap();
        txn.add_edge(&ek2).unwrap();
        txn.commit().unwrap();

        // 2. Start a new transaction. Add ek3 (dirty), delete ek2 (tombstone)
        let mut txn = ctx(&store);
        let ek3 = cek(1, 10, 4).out_key();
        txn.add_edge(&ek3).unwrap();

        // Edge must be loaded into memory before drop
        txn.get_edge(&ek2).unwrap().unwrap();
        txn.drop_element(&CanonicalKey::Edge(ek2.canonical_edge_key())).unwrap();

        // 3. Scan adjacent edges with limit 1
        let opts = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: None };
        let (batch1, cursor1) = txn.get_adjacent_edges(1, Direction::OUT, opts, Some(1)).unwrap();
        assert_eq!(batch1.len(), 1);
        assert_eq!(batch1[0], ek1);
        assert!(cursor1.is_some());

        // 4. Scan next batch using cursor1
        let opts2 = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: cursor1 };
        let (batch2, cursor2) = txn.get_adjacent_edges(1, Direction::OUT, opts2, Some(1)).unwrap();
        // Since ek2 is tombstoned and the DB scan hit limit 1, ek3 is excluded as it is > ek2.
        // So batch2 is empty, but cursor2 is Some(ek2).
        assert_eq!(batch2.len(), 0);
        assert!(cursor2.is_some());

        // 5. Scan third batch using cursor2
        let opts3 = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: cursor2 };
        let (batch3, cursor3) = txn.get_adjacent_edges(1, Direction::OUT, opts3, Some(1)).unwrap();
        // Now database scan reaches the end (cursor is None), so ek3 is included and returned.
        assert_eq!(batch3.len(), 1);
        assert_eq!(batch3[0], ek3);
        assert_eq!(cursor3, None);
    }

    #[test]
    fn test_concurrent_scan_isolation() {
        let (store, _dir) = open();

        // 1. Add some initial committed vertices and edges
        let mut txn = ctx(&store);
        txn.add_vertex(1, 1).unwrap();
        txn.add_vertex(2, 1).unwrap();
        let ek1 = cek(1, 10, 2).out_key();
        txn.add_edge(&ek1).unwrap();
        txn.commit().unwrap();

        // 2. Start Transaction 1. This captures a snapshot.
        let mut txn1 = ctx(&store);

        // Perform first paginated scans (limit 1)
        let (v_batch1, v_cursor1) = txn1.scan_vertices(None, None, 1).unwrap();
        assert_eq!(v_batch1, vec![1]);
        assert!(v_cursor1.is_some());

        let opts = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: None };
        let (e_batch1, e_cursor1) = txn1.get_adjacent_edges(1, Direction::OUT, opts, Some(1)).unwrap();
        assert_eq!(e_batch1.len(), 1);
        assert_eq!(e_batch1[0], ek1);

        // 3. Start Transaction 2 concurrently. Add vertex 3 and edge 1 -> 10 -> 3, then commit it.
        let mut txn2 = ctx(&store);
        txn2.add_vertex(3, 1).unwrap();
        let ek2 = cek(1, 10, 3).out_key();
        txn2.add_edge(&ek2).unwrap();
        txn2.commit().unwrap();

        // 4. Continue pagination in Transaction 1.
        // Under Snapshot Isolation, subsequent pagination requests do NOT see
        // concurrently committed inserts that occurred after Transaction 1 started.
        let (v_batch2, v_cursor2) = txn1.scan_vertices(None, v_cursor1, 1).unwrap();
        assert_eq!(v_batch2, vec![2]);
        assert_eq!(v_cursor2, Some(2));

        // A third scan reaches the end of the snapshot (vertex 3 is isolated/invisible)
        let (v_batch2_next, v_cursor2_next) = txn1.scan_vertices(None, v_cursor2, 1).unwrap();
        assert_eq!(v_batch2_next.len(), 0);
        assert_eq!(v_cursor2_next, None);

        let opts2 = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: e_cursor1 };
        let (e_batch2, e_cursor2) = txn1.get_adjacent_edges(1, Direction::OUT, opts2, Some(1)).unwrap();
        // The concurrently committed edge ek2 is not visible (isolated)
        assert_eq!(e_batch2.len(), 0);
        assert_eq!(e_cursor2, None);

        // 5. Start a new Transaction 3. It should see vertex 3 and edge ek2.
        let mut txn3 = ctx(&store);
        let (v_batch3, _) = txn3.scan_vertices(None, None, 10).unwrap();
        assert!(v_batch3.contains(&3));

        let (e_batch3, _) = txn3.get_adjacent_edges(1, Direction::OUT, opts, Some(10)).unwrap();
        assert!(e_batch3.contains(&ek2));
    }

    #[test]
    fn test_snapshot_scan_isolation() {
        let (store, _dir) = open();

        // 1. Add some initial committed vertices
        let mut txn = ctx(&store);
        txn.add_vertex(1, 1).unwrap();
        txn.add_vertex(2, 1).unwrap();
        txn.commit().unwrap();

        // 2. Open a read snapshot (LogicalSnapshot)
        // S::Snapshot represents the snapshot type. For RocksStorage, it's Snapshot.
        let mut snap = crate::graph::LogicalSnapshot::<RocksStorage>::new(
            store.snapshot(),
            std::sync::Arc::new(std::sync::RwLock::new(crate::schema::Schema::new())),
        );

        // Perform first paginated scan (limit 1)
        let (v_batch1, v_cursor1) = snap.scan_vertices(None, None, 1).unwrap();
        assert_eq!(v_batch1, vec![1]);

        // 3. Start a transaction concurrently to insert vertex 3 and commit it
        let mut txn2 = ctx(&store);
        txn2.add_vertex(3, 1).unwrap();
        txn2.commit().unwrap();

        // 4. Continue pagination in the snapshot
        // Unlike LogicalGraph transactions, the LogicalSnapshot MUST isolate us from concurrent inserts.
        // So it should NOT see vertex 3!
        let (v_batch2, v_cursor2) = snap.scan_vertices(None, v_cursor1, 1).unwrap();
        assert_eq!(v_batch2, vec![2]);
        assert_eq!(v_cursor2, Some(2)); // Hit limit 1, so cursor is Some(2)

        // A third scan reaches the end of the snapshot (vertex 3 is isolated)
        let (v_batch3, v_cursor3) = snap.scan_vertices(None, v_cursor2, 1).unwrap();
        assert_eq!(v_batch3.len(), 0);
        assert_eq!(v_cursor3, None);
    }
}
