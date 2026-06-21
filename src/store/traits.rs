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

//! Store-layer trait contracts.
//!
//! # Layer structure
//!
//! ```text
//! Gremlin Traversal Engine
//!   │  talks only to `LogicalGraph` via inherent methods
//!   ▼
//! LogicalGraph<S: GraphStore>       ← query-scoped ground truth
//!   │  owns the element overlay (VertexKey / EdgeKey)
//!   │  merges committed + dirty state
//!   │  forwards to S::Txn on commit
//!   ▼
//! GraphTransaction                  ← store-layer contract
//!   reads:   get_vertex / get_edge / get_edges
//!   writes:  put_vertex / put_edge / delete_vertex / delete_edge
//!   control: commit / abort
//!
//! GraphStore
//!   begin()  → fresh GraphTransaction
//! ```
//!
//! The engine never imports `GraphTransaction` or `GraphStore` directly —
//! it only touches `LogicalGraph`. Backend details (RocksDB CFs, OCC, encoding)
//! never cross the `GraphTransaction` boundary.

use crate::types::{
    element::Property, AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, Direction, Edge, EdgeKey, LabelId,
    StoreError, Vertex, VertexKey,
};

// ── GraphSnapshot ─────────────────────────────────────────────────────────────

/// A read-only point-in-time view of the persistent graph store.
///
/// Obtained via [`GraphStore::snapshot`]. Uses plain RocksDB `get()` calls
/// pinned to a snapshot for consistent reads without OCC tracking.
/// Independent of [`GraphTransaction`] — the two share no interface.
pub trait GraphSnapshot {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, StoreError>;

    /// Fetch multiple vertices in batch, omitting any keys not found.
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<Vertex>, StoreError> {
        let mut out = Vec::with_capacity(keys.len());
        for &k in keys {
            if let Some(v) = self.get_vertex(k)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<Edge>, StoreError>;

    /// Fetch multiple edges in batch, omitting any keys not found.
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<Edge>, StoreError> {
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(e) = self.get_edge(k)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Scan committed edges adjacent to `vertex` in `direction`.
    fn get_adjacent_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<Edge>, Option<AdjacentEdgeCursor>), StoreError>;

    /// Scan all vertices in the database in batch mode.
    fn scan_vertices(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<VertexKey>,
        _limit: u32,
    ) -> Result<(Vec<Vertex>, Option<VertexKey>), StoreError> {
        Err(StoreError::UnsupportedOperation("scan_vertices is not supported".to_string()))
    }

    /// Scan all unique canonical edges in the database in batch mode.
    fn scan_edges(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<CanonicalEdgeKey>,
        _limit: u32,
    ) -> Result<(Vec<Edge>, Option<CanonicalEdgeKey>), StoreError> {
        Err(StoreError::UnsupportedOperation("scan_edges is not supported".to_string()))
    }
}

// ── GraphTransaction ──────────────────────────────────────────────────────────

/// A single I/O transaction against the persistent graph store.
/// `LogicalGraph` is the only caller. The engine never holds a `GraphTransaction`
/// directly — it always works through `LogicalGraph`.
///
/// # Read semantics
///
/// Reads return owned `Vertex` or `Edge` values. `LogicalGraph` moves them into
/// its overlay map; on mutation it updates the element's properties in place.
/// This trait defines the contract for interacting with the underlying graph storage.
/// # Write semantics
///
/// Writes are purely physical: `GraphTransaction` writes exactly what it is told
/// and operates on individual records. It does not enforce graph consistency
/// (e.g., maintaining matching Out and In edge records, updating vertex edge
/// counts, or checking for dangling edges). That graph-level consistency is
/// strictly the responsibility of `LogicalGraph`.
pub trait GraphTransaction {
    // ── Reads ─────────────────────────────────────────────────────────────────

    /// Fetch a committed vertex; `None` if absent.
    ///
    /// Implementations should register the key in an OCC read-set so that a
    /// concurrent write detected at commit time returns [`StoreError::Conflict`].
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, StoreError>;

    /// Fetch multiple vertices in batch, registering them in OCC read-set.
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<Vertex>, StoreError> {
        let mut out = Vec::with_capacity(keys.len());
        for &k in keys {
            if let Some(v) = self.get_vertex(k)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    /// Fetch a committed vertex's out-degree and in-degree; `None` if absent.
    /// Implementations should register the key in an OCC read-set.
    fn get_vertex_degree(&mut self, key: VertexKey) -> Result<Option<(u32, u32)>, StoreError>;

    /// Fetch a single committed edge record; `None` if absent.
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<Edge>, StoreError>;

    /// Fetch multiple edges in batch, registering them in OCC read-set.
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<Edge>, StoreError> {
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(e) = self.get_edge(k)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// Scan committed edges adjacent to `vertex` in `direction`.
    fn get_adjacent_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<Edge>, Option<AdjacentEdgeCursor>), StoreError>;

    /// Scan all vertices in the database in batch mode.
    fn scan_vertices(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<VertexKey>,
        _limit: u32,
    ) -> Result<(Vec<Vertex>, Option<VertexKey>), StoreError> {
        Err(StoreError::UnsupportedOperation("scan_vertices is not supported".to_string()))
    }

    /// Scan all unique canonical edges in the database in batch mode.
    fn scan_edges(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<CanonicalEdgeKey>,
        _limit: u32,
    ) -> Result<(Vec<Edge>, Option<CanonicalEdgeKey>), StoreError> {
        Err(StoreError::UnsupportedOperation("scan_edges is not supported".to_string()))
    }

    // ── Writes ────────────────────────────────────────────────────────────────

    /// Upsert a vertex record with explicit key, label, and full property list.
    fn put_vertex(&mut self, key: VertexKey, label_id: LabelId, props: &[Property]) -> Result<(), StoreError>;
    /// Upsert the vertex degree record (out-degree and in-degree).
    fn put_vertex_degree(&mut self, key: VertexKey, out_e_cnt: u32, in_e_cnt: u32) -> Result<(), StoreError>;
    /// Upsert a single edge record in the specified physical direction index.
    fn put_edge(&mut self, key: &EdgeKey, props: &[Property]) -> Result<(), StoreError>;
    /// Delete a vertex metadata record.
    fn delete_vertex(&mut self, key: VertexKey) -> Result<(), StoreError>;
    /// Delete the vertex degree record.
    fn delete_vertex_degree(&mut self, key: VertexKey) -> Result<(), StoreError>;
    /// Delete a single edge record from the specified physical direction index.
    fn delete_edge(&mut self, key: &EdgeKey) -> Result<(), StoreError>;

    // ── Control ───────────────────────────────────────────────────────────────

    /// Flush all staged writes atomically.
    /// Returns [`StoreError::Conflict`] on OCC conflict; the caller must retry
    /// the entire traversal from scratch with a new `LogicalGraph`.
    fn commit(&mut self) -> Result<(), StoreError>;

    /// Discard all staged writes and reset the transaction.
    fn abort(&mut self);
}

// ── GraphStore ────────────────────────────────────────────────────────────────

/// A pluggable graph store backend.
///
/// Implementations include `RocksStorage` (local) and future distributed
/// backends. The engine (and `LogicalGraph`) is generic over `S: GraphStore`
/// and never imports concrete backend types.
pub trait GraphStore {
    /// Read-only point-in-time snapshot type.
    type Snapshot: GraphSnapshot;
    /// The concrete transaction type produced by this store.
    type Txn: GraphTransaction;

    /// Open a read-only snapshot pinned to the current committed state.
    fn snapshot(&self) -> Self::Snapshot;
    /// Begin a fresh read-write transaction.
    fn begin(&self) -> Self::Txn;
}
