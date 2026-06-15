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
//!   │               └── .g() → ReadTraversal   .V([]).out([]).to_list()?
//!   └── .begin() → TxSession     (OCC transaction, read-write)
//!                   ├── .g() → WriteTraversal  .V([]).out([]).to_list()?
//!                   │                          .addV(label).property(…).exec()?
//!                   ├── .commit()
//!                   └── .rollback()
//! ```
//!
//! Sessions manage lifecycle only; traversal steps live on the traversal
//! returned by `.g()`, mirroring Gremlin's `GraphTraversalSource` pattern.

use std::{path::Path, sync::Arc};

use crate::{
    engine::GraphCtx,
    graph::{LogicalGraph, LogicalSnapshot},
    gremlin::traversal::{ReadTraversal, WriteTraversal},
    store::{traits::GraphStore, RocksStorage},
    types::StoreError,
};

// ── Graph ─────────────────────────────────────────────────────────────────────

/// The top-level handle to a RocksDB-backed property graph.
///
/// Cheap to clone — wraps an `Arc` internally.
///
/// # Example
/// ```ignore
/// let graph = Graph::open("./my_graph")?;
/// let mut snap = graph.read();
/// let person = snap.g().V([1]).out([KNOWS]).next()?;
/// ```
pub struct Graph {
    store: Arc<RocksStorage>,
}

impl Graph {
    /// Open (or create) the graph database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Ok(Self { store: Arc::new(RocksStorage::open(path)?) })
    }

    /// Open a read-only snapshot session pinned to the current committed state.
    pub fn read(&self) -> ReadSession {
        ReadSession { ctx: LogicalSnapshot::new(self.store.snapshot()) }
    }

    /// Begin a read-write OCC transaction session.
    pub fn begin(&self) -> TxSession {
        TxSession { ctx: LogicalGraph::new(self.store.begin()), committed: false }
    }
}

impl Clone for Graph {
    fn clone(&self) -> Self {
        Self { store: Arc::clone(&self.store) }
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
/// ```ignore
/// let mut snap = graph.read();
/// let names = snap.g().V([1]).out([KNOWS]).values(["name"]).to_list()?;
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
        ReadTraversal::new(&mut self.ctx as &mut dyn GraphCtx)
    }
}

// ── TxSession ─────────────────────────────────────────────────────────────────

/// A read-write session backed by an OCC transaction.
///
/// Dropped without `commit()` / `rollback()` → automatic rollback.
///
/// # Example
/// ```ignore
/// let mut tx = graph.begin();
/// tx.g().addV(PERSON).property("name", "Alice").exec()?;
/// tx.g().V([1]).out([KNOWS]).values(["name"]).to_list()?;
/// tx.commit()?;
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
}

impl Drop for TxSession {
    fn drop(&mut self) {
        if !self.committed {
            self.ctx.abort();
        }
    }
}
