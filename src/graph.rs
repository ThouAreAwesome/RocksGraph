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
//! each element, then calls `store.commit()`. The overlay is cleared so the
//! `LogicalGraph` can be reused for a retry on OCC conflict.
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

use std::collections::{hash_map::Entry, HashMap};

use crate::{
    store::traits::{GraphSnapshot, GraphStore, GraphTransaction},
    types::{
        element::{Edge, Property, Vertex},
        keys::{CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId, VertexKey},
        prop_key::PropKey,
        Primitive, StoreError,
    },
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

// ── LogicalGraph ──────────────────────────────────────────────────────────────
/// Query-scoped logical graph wrapping a store transaction.
///
/// Obtained by calling `LogicalGraph::new(store.begin())`. The engine uses this
/// as its sole interface to the graph.
pub(crate) struct LogicalGraph<S: GraphStore> {
    store: S::Txn, // The underlying transaction from the GraphStore.
    vertices: HashMap<VertexKey, Vertex>,
    edges: HashMap<CanonicalEdgeKey, Edge>,
    vertex_degree: HashMap<VertexKey, (u32, u32)>,
    dirty: HashMap<CanonicalKey, Existence>,
}

impl<S: GraphStore> LogicalGraph<S> {
    /// Create a new logical graph context wrapping the given transaction.
    pub fn new(store: S::Txn) -> Self {
        // Creates a new `LogicalGraph` instance, initializing its in-memory caches
        // and associating it with a store transaction.
        Self {
            store,
            vertices: HashMap::new(),
            edges: HashMap::new(),
            vertex_degree: HashMap::new(),
            // Tracks the mutation state of elements within this transaction.
            dirty: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn vertex_degree_for_test(&mut self, key: VertexKey) -> Result<Option<(u32, u32)>, StoreError> {
        self.get_vertex_degree(key)
    }

    /// Retrieves the degree (out-edge count, in-edge count) of a vertex.
    /// This method acts as a transparent read-through cache:
    ///     it first checks the in-memory `vertex_degree` overlay,
    ///     and falls back to the underlying `GraphStore` on a miss, caching the result.
    ///     It is central to existence checks for vertex existence.
    ///     Returns `None` if the vertex does not exist.
    fn get_vertex_degree(&mut self, key: VertexKey) -> Result<Option<(u32, u32)>, StoreError> {
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

    /// Scan edges incident to `vertex` in `direction`, merging committed data
    /// with the in-memory dirty overlay. Tombstoned edges are filtered out.
    ///
    /// Returns `EdgeKey` values in the requested direction.
    pub(crate) fn get_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        label: Option<LabelId>,
        dst: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError> {
        // Phase 1: populate overlay from store (mutable).
        let committed = self.store.get_edges(vertex, direction, label, dst, limit)?;
        for edge in committed {
            let cek = edge.canonical_key();
            self.edges.entry(cek).or_insert(edge);
        }
        // Phase 2: collect from overlay (immutable, returns refs into self.edges).
        let dirty = &self.dirty;
        let mut result = Vec::new();
        for (&cek, edge) in &self.edges {
            if let Some(l) = limit {
                if result.len() >= l as usize {
                    break;
                }
            }
            if dirty.get(&CanonicalKey::Edge(cek)) == Some(&Existence::Tombstone) {
                continue;
            }
            if !edge_matches(edge, vertex, direction, label, dst) {
                continue;
            }
            let physical_key = match direction {
                Direction::OUT => cek.out_key(),
                Direction::IN => cek.in_key(),
            };
            result.push(physical_key);
        }
        Ok(result)
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
    pub(crate) fn get_property(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk).unwrap().is_some() {
                    let fv = self.vertices.get(&vk).unwrap();
                    Ok(fv.get_property(prop))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => {
                if self.dirty.get(key) == Some(&Existence::Tombstone) {
                    return Ok(None);
                }
                let eg = self.edges.get(&ek).unwrap();
                Ok(eg.get_property(prop))
            }
            CanonicalKey::Empty => Err(StoreError::RuntimeError("Property owner cannot be empty".to_string())),
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
    pub(crate) fn get_value(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk).unwrap().is_some() {
                    let fv = self.vertices.get(&vk).unwrap();
                    Ok(fv.get_value(prop))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => {
                if self.dirty.get(key) == Some(&Existence::Tombstone) {
                    return Ok(None);
                }
                let eg = self.edges.get(&ek).unwrap();
                Ok(eg.get_value(prop))
            }
            CanonicalKey::Empty => {
                Err(StoreError::UnexpectedDataType("expected Vertex or Edge for get property value".to_string()))
            }
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

        let vertex = Vertex { id, label_id, props: Vec::new() };
        self.vertices.insert(id, vertex);
        self.vertex_degree.insert(id, (0, 0));
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
        let cek = ek.canonical_edge_key();
        if self.edges.contains_key(&cek) {
            return Err(StoreError::DuplicateEdge(cek));
        }
        // Check store for a persisted edge not yet in the overlay.
        if self.store.get_edge(ek)?.is_some() {
            return Err(StoreError::DuplicateEdge(cek));
        }

        // Verify both endpoints exist (overlay-first via get_vertex_degree, then store).
        let (mut src_out, src_in) = self.get_vertex_degree(cek.src_id)?.ok_or(StoreError::NotFound)?;
        let (dst_out, mut dst_in) = self.get_vertex_degree(cek.dst_id)?.ok_or(StoreError::NotFound)?;

        src_out += 1;
        dst_in += 1;

        self.vertex_degree.insert(cek.src_id, (src_out, src_in));
        self.mark_dirty(CanonicalKey::Vertex(cek.src_id), Existence::CounterOnly);

        self.vertex_degree.insert(cek.dst_id, (dst_out, dst_in));
        self.mark_dirty(CanonicalKey::Vertex(cek.dst_id), Existence::CounterOnly);

        // 2. insert new edge into overlay and mark dirty.  The store is not touched until commit.
        self.edges.insert(
            cek,
            Edge { src_id: cek.src_id, label_id: cek.label_id, rank: cek.rank, dst_id: cek.dst_id, props: Vec::new() },
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
                    upsert_prop(&mut vt.props, prop);
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
                        upsert_prop(&mut eg.props, prop);
                    }
                }
                self.mark_dirty(key, Existence::Modified);
            }
            CanonicalKey::Empty => {
                return Err(StoreError::RuntimeError("Property owner cannot be empty".to_string()));
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
                    vt.props.retain(|p| p.key != prop.key);
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
                        eg.props.retain(|p| p.key != prop.key);
                    }
                }
                self.mark_dirty(key, Existence::Modified);
            }
            CanonicalKey::Empty => {
                return Err(StoreError::RuntimeError("Property owner cannot be empty".to_string()));
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
                let (out_e, in_e) = self.get_vertex_degree(id)?.ok_or(StoreError::NotFound)?;
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
                    if let Some((mut out_e, in_e)) = self.get_vertex_degree(ek.src_id)? {
                        out_e = out_e.saturating_sub(1);
                        self.vertex_degree.insert(ek.src_id, (out_e, in_e));
                        self.mark_dirty(CanonicalKey::Vertex(ek.src_id), Existence::CounterOnly);
                    }
                    if let Some((out_e, mut in_e)) = self.get_vertex_degree(ek.dst_id)? {
                        in_e = in_e.saturating_sub(1);
                        self.vertex_degree.insert(ek.dst_id, (out_e, in_e));
                        self.mark_dirty(CanonicalKey::Vertex(ek.dst_id), Existence::CounterOnly);
                    }
                }
            }
            CanonicalKey::Empty => {
                return Err(StoreError::RuntimeError("Element key cannot be empty".to_string()));
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
                    let v = self.vertices.get(&id).expect("dirty vertex key not in vertices");
                    self.store.put_vertex(id, v.label_id, &v.props)?;
                    let (out_e, in_e) = self.vertex_degree[&id];
                    self.store.put_vertex_degree(id, out_e, in_e)?;
                }
                (CanonicalKey::Vertex(id), Existence::Modified) => {
                    let v = self.vertices.get(&id).expect("dirty vertex key not in vertices");
                    self.store.put_vertex(id, v.label_id, &v.props)?;
                }
                (CanonicalKey::Vertex(id), Existence::CounterOnly) => {
                    let (out_e, in_e) = self.vertex_degree[&id];
                    self.store.put_vertex_degree(id, out_e, in_e)?;
                }
                (CanonicalKey::Vertex(id), Existence::ModifiedWithCounter) => {
                    let v = self.vertices.get(&id).expect("dirty vertex key not in vertices");
                    self.store.put_vertex(id, v.label_id, &v.props)?;
                    let (out_e, in_e) = self.vertex_degree[&id];
                    self.store.put_vertex_degree(id, out_e, in_e)?;
                }
                (CanonicalKey::Vertex(id), Existence::Tombstone) => {
                    self.store.delete_vertex(id)?;
                    self.store.delete_vertex_degree(id)?;
                }
                (
                    CanonicalKey::Edge(ek),
                    Existence::New | Existence::Modified | Existence::CounterOnly | Existence::ModifiedWithCounter,
                ) => {
                    let e = self.edges.get(&ek).expect("dirty edge key not in edges");
                    self.store.put_edge(&ek.out_key(), &e.props)?;
                    self.store.put_edge(&ek.in_key(), &e.props)?;
                }
                (CanonicalKey::Edge(cek), Existence::Tombstone) => {
                    self.store.delete_edge(&cek.out_key())?;
                    self.store.delete_edge(&cek.in_key())?;
                }
                (CanonicalKey::Empty, _) => {
                    return Err(StoreError::RuntimeError("Element key cannot be empty".to_string()));
                }
            }
        }
        self.reset();
        self.store.commit()
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
}

impl<S: GraphStore> LogicalSnapshot<S> {
    pub fn new(snapshot: S::Snapshot) -> Self {
        Self { store: snapshot, vertices: HashMap::new(), edges: HashMap::new() }
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

    pub(crate) fn get_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        label: Option<LabelId>,
        dst: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<EdgeKey>, StoreError> {
        // The store's RocksDB snapshot is the authoritative source — no dirty
        // overlay exists on a read-only session. Return store results directly.
        // We still populate the edge cache as a side effect so that subsequent
        // ValuesStep / PropertiesStep calls can look up edge properties in O(1)
        // by canonical key without issuing a second store read.
        let committed = self.store.get_edges(vertex, direction, label, dst, limit)?;
        let mut result = Vec::with_capacity(committed.len());
        for edge in committed {
            let cek = edge.canonical_key();
            result.push(match direction {
                Direction::OUT => cek.out_key(),
                Direction::IN => cek.in_key(),
            });
            self.edges.entry(cek).or_insert(edge);
        }
        Ok(result)
    }

    pub(crate) fn get_property(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Property>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk)?.is_some() {
                    Ok(self.vertices.get(&vk).unwrap().get_property(prop))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => Ok(self.edges.get(&ek).and_then(|e| e.get_property(prop))),
            CanonicalKey::Empty => Err(StoreError::RuntimeError("Property owner cannot be empty".to_string())),
        }
    }

    pub(crate) fn get_value(&mut self, key: &CanonicalKey, prop: &PropKey) -> Result<Option<Primitive>, StoreError> {
        match *key {
            CanonicalKey::Vertex(vk) => {
                if self.get_vertex(vk)?.is_some() {
                    Ok(self.vertices.get(&vk).unwrap().get_value(prop))
                } else {
                    Ok(None)
                }
            }
            CanonicalKey::Edge(ek) => Ok(self.edges.get(&ek).and_then(|e| e.get_value(prop))),
            CanonicalKey::Empty => {
                Err(StoreError::UnexpectedDataType("expected Vertex or Edge for get property value".to_string()))
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Updates a property in-place if its key exists, otherwise appends it to the list.
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
            keys::{CanonicalEdgeKey, CanonicalKey, Direction},
            StoreError,
        },
    };

    fn open() -> (RocksStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        (store, dir)
    }

    fn ctx(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
        LogicalGraph::new(store.begin())
    }

    fn cek(src: i64, label: u16, dst: i64) -> CanonicalEdgeKey {
        CanonicalEdgeKey { src_id: src, label_id: label, rank: 0, dst_id: dst }
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
    // ── set_property ─────────────────────────────────────────────────────────

    #[test]
    fn set_property_on_new_vertex_read_your_writes() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();

        let prop = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("age"), value: Primitive::Int32(42) };
        c.set_property(&prop).unwrap();

        let v = c.get_vertex(key).unwrap();
        assert_eq!(v, Some(key));
        let val = c.get_value(&CanonicalKey::Vertex(key), &SmolStr::new("age")).unwrap();
        assert_eq!(val, Some(Primitive::Int32(42)));
    }

    #[test]
    fn set_property_upserts_existing_key() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();

        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(2) };
        c.set_property(&prop1).unwrap();
        c.set_property(&prop2).unwrap();

        let _ = c.get_vertex(key).unwrap().unwrap();
        let val = c.get_value(&CanonicalKey::Vertex(key), &SmolStr::new("x")).unwrap();
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

        let prop = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("w"), value: Primitive::Float64(1.5) };
        c.set_property(&prop).unwrap();

        let _ = c.get_edge(&k.out_key()).unwrap().unwrap();
        let val = c.get_value(&CanonicalKey::Edge(k), &SmolStr::new("w")).unwrap();
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
        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(2) };
        c2.set_property(&prop1).unwrap();
        c3.set_property(&prop2).unwrap();

        c2.commit().unwrap();

        let result = c3.commit();
        assert!(matches!(result, Err(StoreError::Conflict)));
        let mut c4 = ctx(&store);
        let _ = c4.get_vertex(key).unwrap().unwrap();
        let val = c4.get_value(&CanonicalKey::Vertex(key), &SmolStr::new("x")).unwrap();
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
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(2) };
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

        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("a"), value: Primitive::Int32(1) };
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("b"), value: Primitive::Int32(2) };
        c.set_property(&prop1).unwrap();
        c.set_property(&prop2).unwrap();
        c.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("a"), value: Primitive::Null })
            .unwrap();

        let _ = c.get_vertex(key).unwrap().unwrap();
        let val_a = c.get_value(&CanonicalKey::Vertex(key), &SmolStr::new("a")).unwrap();
        let val_b = c.get_value(&CanonicalKey::Vertex(key), &SmolStr::new("b")).unwrap();
        assert_eq!(val_a, None);
        assert_eq!(val_b, Some(Primitive::Int32(2)));
    }

    #[test]
    fn drop_property_on_missing_key_is_noop() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.drop_property(&Property {
            owner: CanonicalKey::Vertex(key),
            key: SmolStr::new("nonexistent"),
            value: Primitive::Null,
        })
        .unwrap();
        let _ = c.get_vertex(key).unwrap().unwrap();
        let val = c.get_value(&CanonicalKey::Vertex(key), &SmolStr::new("nonexistent")).unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn drop_vertex_property_vs_set_vertex_property_handmade() {
        let (store, _dir) = open();
        let mut c1 = ctx(&store);
        let key = c1.add_vertex(100, 1).unwrap();
        let prop = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        c1.set_property(&prop).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        c2.drop_property(&Property {
            owner: CanonicalKey::Vertex(key),
            key: SmolStr::new("x"),
            value: Primitive::Null,
        })
        .unwrap();
        let prop = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(2) };
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
        let prop1 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        c1.set_property(&prop1).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let prop2 = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(2) };
        c2.set_property(&prop2).unwrap();
        c3.drop_property(&Property {
            owner: CanonicalKey::Vertex(key),
            key: SmolStr::new("x"),
            value: Primitive::Null,
        })
        .unwrap();

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
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        c1.set_property(&prop1).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let _ = c2.get_edge(&k.out_key()).unwrap();
        let _ = c3.get_edge(&k.out_key()).unwrap();
        c2.drop_property(&Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Null })
            .unwrap();
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(2) };
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
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        c1.set_property(&prop1).unwrap();
        c1.commit().unwrap();

        let mut c2 = ctx(&store);
        let mut c3 = ctx(&store);
        let _ = c2.get_edge(&k.out_key()).unwrap();
        let _ = c3.get_edge(&k.out_key()).unwrap();
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(2) };
        c2.set_property(&prop2).unwrap();
        let prop3 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Null };
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
        let prop = Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("x"), value: Primitive::Int32(1) };
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
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(1) };
        c2.set_property(&prop1).unwrap();
        let prop2 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Null };
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
        let prop1 = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("x"), value: Primitive::Int32(1) };
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
            let prop = Property {
                owner: CanonicalKey::Vertex(key),
                key: SmolStr::new("name"),
                value: Primitive::String(SmolStr::new("Alice")),
            };
            c.set_property(&prop).unwrap();
            c.commit().unwrap();
            key
        };

        let fv = store.get_vertex(id).unwrap().unwrap();
        assert_eq!(fv.label_id, 7);
        assert_eq!(fv.props.len(), 1);
        assert_eq!(fv.props[0].value, Primitive::String(SmolStr::new("Alice")));
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
            let prop = Property { owner: CanonicalKey::Edge(k), key: SmolStr::new("w"), value: Primitive::Int32(99) };
            c.set_property(&prop).unwrap();
            c.commit().unwrap();
        }

        let edges = store.get_edges(v1, Direction::OUT, None, None, None).unwrap();
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert_eq!(e.props.len(), 1);
        assert_eq!(e.props[0].value, Primitive::Int32(99));
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

        let edges = c.get_edges(v1, Direction::OUT, None, None, None).unwrap();
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

        let edges = c.get_edges(v1, Direction::OUT, None, None, None).unwrap();
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn get_edges_direction_in_vs_out() {
        let (store, _dir) = open();
        let mut c = ctx(&store);
        let v1 = c.add_vertex(1, 1).unwrap();
        let v2 = c.add_vertex(2, 1).unwrap();
        c.add_edge(&cek(v1, 1, v2).out_key()).unwrap();

        let out = c.get_edges(v1, Direction::OUT, None, None, None).unwrap();
        let in_ = c.get_edges(v2, Direction::IN, None, None, None).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(in_.len(), 1);
        // Vertex v1 has no incoming edges; vertex v2 has no outgoing.
        assert!(c.get_edges(v1, Direction::IN, None, None, None).unwrap().is_empty());
        assert!(c.get_edges(v2, Direction::OUT, None, None, None).unwrap().is_empty());
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

        let label1 = c.get_edges(v1, Direction::OUT, Some(1), None, None).unwrap();
        assert_eq!(label1.len(), 2);
        assert!(label1.iter().all(|ek| ek.label_id == 1));

        let label2 = c.get_edges(v1, Direction::OUT, Some(2), None, None).unwrap();
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

        let result = c.get_edges(v1, Direction::OUT, None, Some(&[v10, v30]), None).unwrap();
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

        let result = c.get_edges(v1, Direction::OUT, None, None, Some(2)).unwrap();
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
        let edges = c.get_edges(v1, Direction::OUT, None, None, None).unwrap();
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
                    let prop = Property {
                        owner: CanonicalKey::Vertex(v1),
                        key: SmolStr::new("x"),
                        value: Primitive::Int32(1),
                    };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn add_edge_vs_drop_vertex_property() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let prop = Property {
                        owner: CanonicalKey::Vertex(v1),
                        key: SmolStr::new("x"),
                        value: Primitive::Int32(1),
                    };
                    c.set_property(&prop).unwrap();
                    let v2 = c.add_vertex(2, 1).unwrap();
                    (v1, v2)
                },
                |c, (v1, v2)| {
                    c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
                },
                |c, (v1, _)| {
                    c.get_vertex(v1).unwrap();
                    c.drop_property(&Property {
                        owner: CanonicalKey::Vertex(v1),
                        key: SmolStr::new("x"),
                        value: Primitive::Null,
                    })
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
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
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
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_element(&CanonicalKey::Edge(e)).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    c.drop_property(&Property {
                        owner: CanonicalKey::Edge(e),
                        key: SmolStr::new("x"),
                        value: Primitive::Null,
                    })
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
                    let prop = Property {
                        owner: CanonicalKey::Vertex(v1),
                        key: SmolStr::new("x"),
                        value: Primitive::Int32(1),
                    };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn drop_edge_vs_drop_vertex_property() {
            run_non_conflict(
                |c| {
                    let v1 = c.add_vertex(1, 1).unwrap();
                    let prop = Property {
                        owner: CanonicalKey::Vertex(v1),
                        key: SmolStr::new("x"),
                        value: Primitive::Int32(1),
                    };
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
                    c.drop_property(&Property {
                        owner: CanonicalKey::Vertex(v1),
                        key: SmolStr::new("x"),
                        value: Primitive::Null,
                    })
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
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(2) };
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
                    let prop =
                        Property { owner: CanonicalKey::Edge(e1), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e2), key: SmolStr::new("y"), value: Primitive::Int32(2) };
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
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Null };
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
                    let prop1 =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    let prop2 =
                        Property { owner: CanonicalKey::Edge(e2), key: SmolStr::new("y"), value: Primitive::Int32(2) };
                    c.set_property(&prop1).unwrap();
                    c.set_property(&prop2).unwrap();
                    (e, e2)
                },
                |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e1), key: SmolStr::new("x"), value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e2), key: SmolStr::new("y"), value: Primitive::Null };
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
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    e
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, e| {
                    c.get_edge(&e.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Null };
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
                    let prop1 =
                        Property { owner: CanonicalKey::Edge(e), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    let prop2 =
                        Property { owner: CanonicalKey::Edge(e2), key: SmolStr::new("y"), value: Primitive::Int32(2) };
                    c.set_property(&prop1).unwrap();
                    c.set_property(&prop2).unwrap();
                    (e, e2)
                },
                |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e1.out_key()).unwrap();
                    let val = c.get_value(&CanonicalKey::Edge(e1), &SmolStr::new("x")).unwrap();
                    assert_eq!(val, Some(Primitive::Int32(1)));
                    let prop =
                        Property { owner: CanonicalKey::Edge(e1), key: SmolStr::new("x"), value: Primitive::Null };
                    c.drop_property(&prop).unwrap();
                },
                |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                    c.get_edge(&e2.out_key()).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Edge(e2), key: SmolStr::new("y"), value: Primitive::Null };
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
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(2) };
                    c.set_property(&prop).unwrap();
                },
            );
        }

        #[test]
        fn set_vertex_property_vs_drop_vertex_property() {
            run_conflict(
                |c| {
                    let v = c.add_vertex(100, 1).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    v
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(2) };
                    c.set_property(&prop).unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap().unwrap();
                    let val = c.get_value(&CanonicalKey::Vertex(v), &SmolStr::new("x")).unwrap();
                    assert_eq!(val, Some(Primitive::Int32(1)));
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Null };
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
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(1) };
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
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    v
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_property(&Property {
                        owner: CanonicalKey::Vertex(v),
                        key: SmolStr::new("x"),
                        value: Primitive::Null,
                    })
                    .unwrap();
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_property(&Property {
                        owner: CanonicalKey::Vertex(v),
                        key: SmolStr::new("x"),
                        value: Primitive::Null,
                    })
                    .unwrap();
                },
            );
        }

        #[test]
        fn drop_vertex_property_vs_drop_vertex() {
            run_conflict(
                |c| {
                    let v = c.add_vertex(100, 1).unwrap();
                    let prop =
                        Property { owner: CanonicalKey::Vertex(v), key: SmolStr::new("x"), value: Primitive::Int32(1) };
                    c.set_property(&prop).unwrap();
                    v
                },
                |c, v| {
                    c.get_vertex(v).unwrap();
                    c.drop_property(&Property {
                        owner: CanonicalKey::Vertex(v),
                        key: SmolStr::new("x"),
                        value: Primitive::Null,
                    })
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
        let out = c.get_edges(hub, Direction::OUT, Some(1), None, None).unwrap();
        assert_eq!(out.len(), 4);

        // check vertex counter is correct after multiple contexts
        let (out_e, in_e) = c.vertex_degree_for_test(hub).unwrap().unwrap();
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
            let in_edges = c.get_edges(spoke, Direction::IN, Some(1), None, None).unwrap();
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
            let name_prop = Property {
                owner: CanonicalKey::Vertex(key),
                key: SmolStr::new("name"),
                value: Primitive::String(SmolStr::new("Alice")),
            };
            c1.set_property(&name_prop).unwrap();
            let age_prop =
                Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("age"), value: Primitive::Int32(30) };
            c1.set_property(&age_prop).unwrap();
            key
        };

        // ctx2 — person: Bob
        let mut c2 = ctx(&store);
        let bob = {
            let key = c2.add_vertex(102, 1).unwrap();
            let name_prop = Property {
                owner: CanonicalKey::Vertex(key),
                key: SmolStr::new("name"),
                value: Primitive::String(SmolStr::new("Bob")),
            };
            c2.set_property(&name_prop).unwrap();
            let age_prop =
                Property { owner: CanonicalKey::Vertex(key), key: SmolStr::new("age"), value: Primitive::Int32(25) };
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
                key: SmolStr::new("name"),
                value: Primitive::String(SmolStr::new("London")),
            };
            c.set_property(&name_prop).unwrap();
            // Alice -> London
            let e1 = cek(alice, 2, city_key);
            c.add_edge(&e1.out_key()).unwrap();
            let since_prop =
                Property { owner: CanonicalKey::Edge(e1), key: SmolStr::new("since"), value: Primitive::Int32(2015) };
            c.set_property(&since_prop).unwrap();
            // Bob -> London
            let e2 = cek(bob, 2, city_key);
            c.add_edge(&e2.out_key()).unwrap();
            let since_prop2 =
                Property { owner: CanonicalKey::Edge(e2), key: SmolStr::new("since"), value: Primitive::Int32(2019) };
            c.set_property(&since_prop2).unwrap();
            c.commit().unwrap();
            city_key
        };

        // ctx4 — read-only verification
        let mut c = ctx(&store);

        // Vertices survive across contexts.
        let _ = c.get_vertex(alice).unwrap().unwrap();
        assert_eq!(
            c.get_value(&CanonicalKey::Vertex(alice), &SmolStr::new("name")).unwrap(),
            Some(Primitive::String(SmolStr::new("Alice")))
        );
        assert_eq!(
            c.get_value(&CanonicalKey::Vertex(alice), &SmolStr::new("age")).unwrap(),
            Some(Primitive::Int32(30))
        );
        let (alice_out_e, alice_in_e) = c.vertex_degree_for_test(alice).unwrap().unwrap();
        assert_eq!(alice_out_e, 1);
        assert_eq!(alice_in_e, 0);

        let _ = c.get_vertex(bob).unwrap().unwrap();
        assert_eq!(
            c.get_value(&CanonicalKey::Vertex(bob), &SmolStr::new("name")).unwrap(),
            Some(Primitive::String(SmolStr::new("Bob")))
        );
        let (bob_out_e, bob_in_e) = c.vertex_degree_for_test(bob).unwrap().unwrap();
        assert_eq!(bob_out_e, 1);
        assert_eq!(bob_in_e, 0);

        let _ = c.get_vertex(london).unwrap().unwrap();
        assert_eq!(
            c.get_value(&CanonicalKey::Vertex(london), &SmolStr::new("name")).unwrap(),
            Some(Primitive::String(SmolStr::new("London")))
        );
        let (london_out_e, london_in_e) = c.vertex_degree_for_test(london).unwrap().unwrap();
        assert_eq!(london_out_e, 0);
        assert_eq!(london_in_e, 2);

        // Both outgoing "lives_in" edges from Alice land at London.
        let alice_out = c.get_edges(alice, Direction::OUT, Some(2), None, None).unwrap();
        assert_eq!(alice_out.len(), 1);
        let e_ek = alice_out[0];
        assert_eq!(e_ek.secondary_id, london);
        let since_val = c.get_value(&CanonicalKey::Edge(e_ek.canonical_edge_key()), &SmolStr::new("since")).unwrap();
        assert_eq!(since_val, Some(Primitive::Int32(2015)));

        // London has two incoming edges: one from Alice, one from Bob.
        let london_in = c.get_edges(london, Direction::IN, Some(2), None, None).unwrap();
        assert_eq!(london_in.len(), 2);
        let mut src_ids: Vec<i64> = london_in.iter().map(|ek| ek.secondary_id).collect();
        src_ids.sort_unstable();
        assert_eq!(src_ids, vec![alice.min(bob), alice.max(bob)]);
    }

    // Tests that operations depending on vertex counters (like adding an edge or dropping the vertex)
    // fail gracefully when the vertex is deleted by a concurrent transaction.
    #[test]
    fn concurrent_vertex_deletion_fails_dependent_operations() {
        let (store, _dir) = open();

        // step 1, insert a vertex and set properties, commit the transaction txn1
        let mut txn1 = ctx(&store);
        let v1 = txn1.add_vertex(1, 1).unwrap();
        let name_prop = Property {
            owner: CanonicalKey::Vertex(v1),
            key: SmolStr::new("name"),
            value: Primitive::String(SmolStr::new("Alice")),
        };
        txn1.set_property(&name_prop).unwrap();
        txn1.commit().unwrap();

        // step 2, in a new Transaction txn2, get_vertex
        let mut txn2 = ctx(&store);
        assert!(txn2.get_vertex(v1).unwrap().is_some());

        // step 3, the vertex was deleted in another transaction, commit the deleting transaction which should succeed
        let mut txn3 = ctx(&store);
        txn3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();
        txn3.commit().unwrap();

        // As a result, adding an edge in txn2 using the deleted vertex should gracefully error out
        let err = txn2.add_edge(&cek(v1, 5, 2).out_key());
        assert!(matches!(err, Err(StoreError::NotFound)));

        // As a result, dropping the deleted vertex in txn2 should gracefully error out
        let err = txn2.drop_element(&CanonicalKey::Vertex(v1));
        assert!(matches!(err, Err(StoreError::NotFound)));

        // step 4, check that get_vertex in txn2 now returns None for the deleted vertex
        let counts = txn2.vertex_degree_for_test(v1).unwrap();
        assert!(counts.is_none());
    }
}
