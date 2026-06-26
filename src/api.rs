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

//! High-level user-facing API.
//!
//! ```text
//! Graph::open("./db")
//!   ├── .read()  → ReadSession   (snapshot, read-only)
//!   │               └── .g() → ReadTraversal
//!   │                           .V([1]).out(&["knows"]).next()?       // Option<GValue>
//!   │                           .V([]).values(&["name"]).to_list()? // Vec<GValue>
//!   │                           .V([]).out([]).iter().unwrap()             // BuiltTraversal (Iterator)
//!   └── .begin() → TxSession     (OCC transaction, read-write)
//!                   ├── .g() → WriteTraversal
//!                   │           .addV(label).property(…).next()?
//!                   │           .V([]).out([]).to_list()?
//!                   ├── .commit()
//!                   └── .rollback()
//! ```
//!
//! Sessions manage lifecycle only; traversal steps live on the traversal
//! returned by `.g()`, mirroring Gremlin's `GraphTraversalSource` pattern.
//!
//! # Execution model
//!
//! Every step method on [`ReadTraversal`] and [`WriteTraversal`] takes `self` by
//! value and returns `Self` (move semantics, no hidden `&mut` aliasing).  Building
//! the physical plan and executing the pipeline happens only when a **terminal**
//! method is called:
//!
//! | Method | Returns | TinkerPop equivalent |
//! |---|---|---|
//! | `next()` | `Result<Option<GValue>>` | `tryNext()` |
//! | `to_list()` | `Result<Vec<GValue>>` | `toList()` |
//! | `iter()` | `Result<BuiltTraversal>` | iterate `Traversal` |

use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use crate::{
    engine::GraphCtx,
    graph::{LogicalGraph, LogicalSnapshot},
    gremlin::traversal::{ReadTraversal, WriteTraversal},
    schema::{GraphOptions, Schema, SchemaManagement},
    store::{traits::GraphStore, RocksStorage},
    types::{BatchScenario, StoreError},
};

// ── Graph ─────────────────────────────────────────────────────────────────────

/// The top-level handle to a RocksDB-backed property graph.
///
/// Cheap to clone — wraps an `Arc` internally.
///
/// # Example
/// ```
/// # use rocksgraph::{Graph, TraversalBuilder};
/// # let dir = tempfile::tempdir().unwrap();
/// # let graph = Graph::open(dir.path()).unwrap();
/// let mut snap = graph.read();
/// let person = snap.g().V([1]).out(["knows"]).next().unwrap();
/// let names  = snap.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
/// # graph.close().unwrap();
/// ```
pub struct Graph {
    store: Arc<RocksStorage>,
    schema: Arc<RwLock<Schema>>,
}

impl Graph {
    /// Open (or create) the graph database at `path`, in [`SchemaMode::Auto`] with
    /// [`EdgeMode::Single`] — see [`open_with_options`](Self::open_with_options) to choose
    /// strict, explicit schema declaration instead.
    ///
    /// [`SchemaMode::Auto`]: crate::schema::SchemaMode::Auto
    /// [`EdgeMode::Single`]: crate::schema::EdgeMode::Single
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_options(path, GraphOptions::default())
    }

    /// Open (or create) the graph database at `path` with custom options.
    ///
    /// `options.mode` controls how vertex labels, edge labels, and property keys used by
    /// traversals are registered:
    /// - [`SchemaMode::Auto`] (the default) — registered implicitly on first use.
    /// - [`SchemaMode::Strict`] — must be declared first via [`open_management`](Self::open_management); see
    ///   [`SchemaManagement`] for a worked example.
    ///
    /// Options are only applied the first time a database is created; reopening an existing
    /// database always uses its persisted settings, ignoring `options`.
    ///
    /// [`SchemaMode::Auto`]: crate::schema::SchemaMode::Auto
    /// [`SchemaMode::Strict`]: crate::schema::SchemaMode::Strict
    pub fn open_with_options(path: impl AsRef<Path>, options: GraphOptions) -> Result<Self, StoreError> {
        let store = Arc::new(RocksStorage::open(path)?);
        let schema = store.load_schema(options)?;
        Ok(Self { store, schema: Arc::new(RwLock::new(schema)) })
    }

    /// Open a schema management session for explicit, [`SchemaMode::Strict`]-style schema
    /// declaration. See [`SchemaManagement`] for a worked example.
    ///
    /// [`SchemaMode::Strict`]: crate::schema::SchemaMode::Strict
    pub fn open_management(&self) -> SchemaManagement {
        SchemaManagement::new(Arc::clone(&self.store), Arc::clone(&self.schema))
    }

    /// Access the thread-safe schema registry directly, bypassing `SchemaManagement`. Test-only:
    /// real callers declare schema via [`open_management`](Self::open_management) or implicit
    /// auto-registration; this exists purely so test fixtures can seed a `Schema` in one step.
    #[cfg(test)]
    pub(crate) fn schema(&self) -> Arc<RwLock<Schema>> {
        Arc::clone(&self.schema)
    }

    /// Open a read-only snapshot session pinned to the current committed state.
    pub fn read(&self) -> ReadSession {
        ReadSession { ctx: LogicalSnapshot::new(self.store.snapshot(), Arc::clone(&self.schema)) }
    }

    /// Begin a read-write OCC transaction session.
    pub fn begin(&self) -> TxSession {
        TxSession { ctx: LogicalGraph::new(self.store.begin(), Arc::clone(&self.schema)), committed: false }
    }

    /// Close the database, releasing all RocksDB resources.
    ///
    /// After calling this, no further sessions or queries can be created
    /// from this `Graph` handle or any clone.  In tests, call this before
    /// the temporary directory is dropped so RocksDB can flush and close
    /// its files cleanly.
    pub fn close(self) -> Result<(), StoreError> {
        // Dropping the Arc will close RocksDB if this is the last reference.
        match Arc::try_unwrap(self.store) {
            Ok(_store) => Ok(()),
            Err(arc) => {
                // Other references exist (e.g. open snapshots). The DB will
                // close when the last reference drops — this is a best-effort.
                drop(arc);
                Ok(())
            }
        }
    }
}

impl Clone for Graph {
    fn clone(&self) -> Self {
        Self { store: Arc::clone(&self.store), schema: Arc::clone(&self.schema) }
    }
}

#[cfg(feature = "rocksdb-stats")]
impl Graph {
    /// Returns bloom-filter and internal RocksDB statistics.
    pub fn statistics(&self) -> Option<String> {
        self.store.statistics()
    }
}

// ── ReadSession ───────────────────────────────────────────────────────────────

/// A read-only session backed by a point-in-time snapshot.
///
/// Dropped automatically with no side effects.
///
/// # Example
/// ```
/// # use rocksgraph::{Graph, TraversalBuilder};
/// # let dir = tempfile::tempdir().unwrap();
/// # let graph = Graph::open(dir.path()).unwrap();
/// let mut snap = graph.read();
/// let names = snap.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
///
/// // Lazy iteration
/// for item in snap.g().V([]).out(["knows"]).iter().unwrap() {
///     println!("{:?}", item.unwrap());
/// }
/// # graph.close().unwrap();
/// ```
pub struct ReadSession {
    ctx: LogicalSnapshot<RocksStorage>,
}

impl ReadSession {
    /// Return a blank traversal bound to this snapshot.
    ///
    /// Call traversal step methods (`V`, `out`, `has`, …) on the returned
    /// [`ReadTraversal`] to build and execute a query.
    pub fn g(&mut self) -> ReadTraversal<'_> {
        self.ctx.clear_caches();
        ReadTraversal::new(&mut self.ctx as &mut dyn GraphCtx)
    }

    // Clear per-traversal caches so they don't accumulate across g() calls.
    // The underlying RocksDB snapshot is unaffected — all traversals on this
    // session still see the same consistent point-in-time view.
    pub fn clear_caches(&mut self) {
        self.ctx.clear_caches();
    }

    /// Configure the batch size for a given scan or query scenario.
    pub fn set_batch_size(&mut self, scenario: BatchScenario, size: u32) {
        match scenario {
            BatchScenario::ScanVertices => self.ctx.scan_config.scan_vertices_batch_size = size,
            BatchScenario::ScanEdges => self.ctx.scan_config.scan_edges_batch_size = size,
            BatchScenario::GetAdjacentEdges => self.ctx.scan_config.get_adjacent_edges_batch_size = size,
        }
    }
}

// ── TxSession ─────────────────────────────────────────────────────────────────

/// A read-write session backed by an OCC transaction.
///
/// Dropped without `commit()` / `rollback()` → automatic rollback.
///
/// # Example
/// ```
/// # use rocksgraph::{Graph, TraversalBuilder};
/// # let dir = tempfile::tempdir().unwrap();
/// # let graph = Graph::open(dir.path()).unwrap();
/// let mut tx = graph.begin();
/// tx.g().addV("person").property("id", 1i64).property("name", "Alice").next().unwrap();
/// let names = tx.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
/// tx.commit().unwrap();
/// # graph.close().unwrap();
/// ```
pub struct TxSession {
    ctx: LogicalGraph<RocksStorage>,
    committed: bool,
}

impl TxSession {
    /// Return a blank traversal bound to this transaction.
    ///
    /// Call traversal step methods (`V`, `addV`, `out`, `has`, …) on the
    /// returned [`WriteTraversal`] to build and execute a query or mutation.
    pub fn g(&mut self) -> WriteTraversal<'_> {
        WriteTraversal::new(&mut self.ctx as &mut dyn GraphCtx)
    }

    /// Flush all mutations to RocksDB atomically and consume this session.
    ///
    /// Returns [`StoreError::Conflict`] if a concurrent transaction modified
    /// an overlapping key; retry from scratch with a new `TxSession`.
    pub fn commit(mut self) -> Result<(), StoreError> {
        self.committed = true;
        self.ctx.commit()
    }

    /// Discard all mutations and consume this session.
    pub fn rollback(mut self) {
        self.committed = true;
        self.ctx.abort();
    }

    /// Configure the batch size for a given scan or query scenario.
    pub fn set_batch_size(&mut self, scenario: BatchScenario, size: u32) {
        match scenario {
            BatchScenario::ScanVertices => self.ctx.scan_config.scan_vertices_batch_size = size,
            BatchScenario::ScanEdges => self.ctx.scan_config.scan_edges_batch_size = size,
            BatchScenario::GetAdjacentEdges => self.ctx.scan_config.get_adjacent_edges_batch_size = size,
        }
    }
}

impl Drop for TxSession {
    fn drop(&mut self) {
        if !self.committed {
            self.ctx.abort();
        }
    }
}
