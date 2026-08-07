// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! High-level user-facing API.
//!
//! ```text
//! Graph::open("./db")
//!   ├── .read()           → ReadSession      (snapshot, read-only)
//!   │                         └── .g() → ReadTraversal
//!   ├── .begin()          → TxnSession        (OCC transaction, read-write)
//!   │                         ├── .g() → WriteTraversal
//!   │                         ├── .commit()
//!   │                         └── .rollback()
//!   ├── .open_schema()    → SchemaSession    (schema DDL — add labels, declare indexes)
//!   └── .index_manager()  → IndexManager     (index maintenance — rebuild, save, future export/import)
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

use parking_lot::RwLock;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};

use smol_str::SmolStr;

use crate::{
    bulk::BulkLoader,
    engine::GraphCtx,
    graph::{LogicalGraph, LogicalSnapshot},
    gremlin::traversal::{ReadTraversal, WriteTraversal},
    schema::{GraphOptions, Schema, SchemaSession},
    store::RocksStorage,
    types::{
        gvalue::Primitive,
        keys::{CanonicalKey, VertexKey},
        StoreError,
    },
    vector::{
        error::{VectorEntityType, VectorError},
        hnsw::UsearchHnswIndex,
        persistence::{load_vector_configs, read_vector_config, vector_snapshot_path},
        traits::IndexOptions,
        wal::{gc_vector_wal, replay_vector_wal},
        EntityKey, VectorIndexMap,
    },
};

// ── Graph ─────────────────────────────────────────────────────────────────────

/// The top-level handle to a RocksDB-backed property graph.
///
/// Cheap to clone — wraps an `Arc` internally. Safe to share across threads;
/// create one `Graph` per process and hand out sessions per thread or per request.
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
///
/// # Session methods
///
/// - [`read`](Self::read) — open a read-only snapshot
/// - [`begin`](Self::begin) — open a read-write OCC transaction
/// - [`open_schema`](Self::open_schema) — declare schema in [`SchemaMode::Strict`](crate::schema::SchemaMode::Strict)
/// - [`open_bulk_loader`](Self::open_bulk_loader) — high-throughput SST-based data ingestion
/// - [`close`](Self::close) — flush vector index snapshots and release the RocksDB handle
///
/// # Maintenance methods
///
/// For index maintenance operations (rebuild after bulk ingestion, checkpointing, future
/// export/import), obtain an [`IndexManager`] handle via [`Graph::index_manager`].
pub struct Graph {
    pub(crate) store: Arc<RocksStorage>,
    pub(crate) schema: Arc<RwLock<Schema>>,
    pub(crate) bulk_load_in_progress: AtomicBool,
    pub(crate) vector_indexes: Arc<RwLock<VectorIndexMap>>,
    pub(crate) index_options: IndexOptions,
    pub(crate) execution_options: crate::engine::ExecutionOptions,
}

impl Graph {
    /// Open (or create) the graph database at `path`, with default [`GraphOptions`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_options(path, GraphOptions::default())
    }

    /// Open (or create) the graph database at `path` with custom [`GraphOptions`].
    ///
    /// `options.mode` and `options.edge_mode` are schema options, applied the first time
    /// a database is created; reopening an existing database uses its persisted settings.
    ///
    /// `options.storage` and `options.index` are runtime-only options (block cache, memtable
    /// sizes, vector memory limits) applied every time the database is opened.
    ///
    /// # Example
    /// ```
    /// # use rocksgraph::{Graph, RocksOptions, schema::{GraphOptions, SchemaMode}};
    /// # let dir = tempfile::tempdir().unwrap();
    /// let graph = Graph::open_with_options(
    ///     dir.path(),
    ///     GraphOptions::default()
    ///         .with_mode(SchemaMode::Strict)
    ///         .with_storage(RocksOptions::default().with_block_cache_size(5 * 1024 * 1024 * 1024)),
    /// )?;
    /// # graph.close().unwrap();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_with_options(path: impl AsRef<Path>, options: GraphOptions) -> Result<Self, StoreError> {
        let store = Arc::new(RocksStorage::open(path, &options.storage)?);
        store.recover_bulk_load_crash()?;
        let schema = store.load_schema(options.mode, options.edge_mode)?;
        let vector_indexes = {
            let mut map = HashMap::new();
            load_vector_configs(&store, &mut map);
            Arc::new(RwLock::new(map))
        };

        // Seed the WAL clock and replay pending entries into each index.
        // This catches mutations that were committed but not yet reflected
        // in a saved snapshot (snapshot persistence is deferred to MR 3b).
        replay_vector_wal(&store, &vector_indexes, &schema)?;

        // Apply per-index memory limits from IndexOptions.
        {
            let map = vector_indexes.read();
            for ((entity_type, prop_name), arc) in map.iter() {
                if let Some(limit_bytes) = options.index.memory_limit_bytes(*entity_type, prop_name) {
                    let mut guard = arc.write();
                    guard.set_memory_limit(limit_bytes);
                }
            }
        }

        Ok(Self {
            store,
            schema: Arc::new(RwLock::new(schema)),
            bulk_load_in_progress: AtomicBool::new(false),
            vector_indexes,
            index_options: options.index,
            execution_options: options.execution,
        })
    }

    /// Open a schema management session for explicit, [`SchemaMode::Strict`]-style schema
    /// declaration. See [`SchemaSession`] for a worked example.
    ///
    /// [`SchemaMode::Strict`]: crate::schema::SchemaMode::Strict
    pub fn open_schema(&self) -> SchemaSession {
        SchemaSession::new(
            Arc::clone(&self.store),
            Arc::clone(&self.schema),
            Some(Arc::clone(&self.vector_indexes)),
            Some(self.index_options.clone()),
        )
    }

    /// Open a bulk loading session for fast initial data ingestion via SST generation.
    ///
    /// The loader operates on the open graph database and schema, writing and sorting
    /// SST files offline before atomically ingesting them at [`BulkLoader::commit`].
    ///
    /// # Vector Indexes
    /// Any declared vertex vector indexes are automatically rebuilt from the newly
    /// ingested vertices during [`BulkLoader::commit`].
    pub fn open_bulk_loader(&self) -> Result<BulkLoader<'_>, StoreError> {
        if self.bulk_load_in_progress.swap(true, Ordering::AcqRel) {
            return Err(StoreError::BulkLoadInProgress);
        }
        BulkLoader::new(self)
    }

    /// Access the thread-safe schema registry directly, bypassing `SchemaSession`. Test-only:
    /// real callers declare schema via [`open_schema`](Self::open_schema) or implicit
    /// auto-registration; this exists purely so test fixtures can seed a `Schema` in one step.
    #[cfg(test)]
    pub(crate) fn schema(&self) -> Arc<RwLock<Schema>> {
        Arc::clone(&self.schema)
    }

    /// Open a read-only snapshot session pinned to the current committed state.
    pub fn read(&self) -> ReadSession {
        ReadSession {
            ctx: LogicalSnapshot::new(
                self.store.snapshot(),
                Arc::clone(&self.schema),
                Arc::clone(&self.vector_indexes),
                self.execution_options,
            ),
        }
    }

    /// Open a read-write transaction session with OCC (Optimistic Concurrency Control).
    pub fn begin(&self) -> TxnSession {
        TxnSession {
            ctx: LogicalGraph::new(
                self.store.begin(),
                Arc::clone(&self.schema),
                Arc::clone(&self.vector_indexes),
                self.execution_options,
            ),
            committed: false,
        }
    }

    /// Open an [`IndexManager`] handle for index maintenance operations (rebuild, save, future
    /// export/import).
    pub fn index_manager(&self) -> IndexManager {
        IndexManager {
            store: Arc::downgrade(&self.store),
            schema: Arc::downgrade(&self.schema),
            vector_indexes: Arc::downgrade(&self.vector_indexes),
            index_options: self.index_options.clone(),
            execution_options: self.execution_options,
        }
    }

    /// Close the database, persisting snapshots for all vector indexes and releasing RocksDB resources.
    ///
    /// After calling this, no further sessions or queries can be created
    /// from this `Graph` handle or any clone.  In tests, call this before
    /// the temporary directory is dropped so RocksDB can flush and close
    /// its files cleanly.
    pub fn close(self) -> Result<(), StoreError> {
        // save_all() persists snapshots and GCs WAL entries covered by them.
        // If save fails, WAL entries are preserved for crash recovery on next open.
        self.index_manager().save_all()
    }
}

// ── IndexManager ─────────────────────────────────────────────────────────────

/// Handle for index maintenance operations obtained from [`Graph::index_manager`].
///
/// Unlike [`SchemaSession`] (which accumulates DDL changes and commits them
/// atomically), `IndexManager` executes each operation immediately — there is
/// nothing to commit.
///
/// # Future operations
/// Export and import of individual indexes (for backup / migration) are planned
/// for a future release.
pub struct IndexManager {
    store: Weak<RocksStorage>,
    schema: Weak<RwLock<Schema>>,
    vector_indexes: Weak<RwLock<VectorIndexMap>>,
    index_options: IndexOptions,
    execution_options: crate::engine::ExecutionOptions,
}

impl IndexManager {
    #[allow(clippy::type_complexity)]
    fn try_refs(&self) -> Result<(Arc<RocksStorage>, Arc<RwLock<Schema>>, Arc<RwLock<VectorIndexMap>>), StoreError> {
        let store = self.store.upgrade().ok_or_else(|| StoreError::UnsupportedOperation("graph is closed".into()))?;
        let schema = self.schema.upgrade().ok_or_else(|| StoreError::UnsupportedOperation("graph is closed".into()))?;
        let vector_indexes =
            self.vector_indexes.upgrade().ok_or_else(|| StoreError::UnsupportedOperation("graph is closed".into()))?;
        Ok((store, schema, vector_indexes))
    }

    /// Rebuild the in-memory vector index for `(entity_type, property)` from scratch.
    ///
    /// Scans all stored vectors for `property`, builds a fresh HNSW index, and
    /// atomically swaps it into the live index map. The rebuilt index is immediately
    /// persisted to disk as a snapshot to bound WAL replay time on the next open.
    pub fn rebuild(&self, entity_type: VectorEntityType, property: &str) -> Result<(), StoreError> {
        use crate::vector::traits::VectorIndex;

        if entity_type == VectorEntityType::Edge {
            return Err(VectorError::Unsupported("edge vector index rebuild is not yet supported (v0.3)".into()).into());
        }

        let (store, schema, vector_indexes) = self.try_refs()?;

        let config = read_vector_config(&store, entity_type, property)?;

        let prop_key_id = {
            let schema = schema.read();
            let Some(id) = schema.prop_key_id(property) else {
                return Err(StoreError::VectorIndex(format!("property key '{property}' is not defined in schema")));
            };
            id
        };

        // Capture memory limit from the old live index, or fall back to configured IndexOptions.
        let key = (entity_type, SmolStr::from(property));
        let limit = {
            let map = vector_indexes.read();
            map.get(&key).and_then(|arc| arc.read().memory_limit_bytes())
        }
        .or_else(|| self.index_options.memory_limit_bytes(entity_type, property));

        let mut index = UsearchHnswIndex::new(&config)?;
        if let Some(limit_bytes) = limit {
            index.set_memory_limit(limit_bytes);
        }

        // Capture the WAL timestamp BEFORE taking the RocksDB snapshot, so
        // concurrent commits during the scan write WAL entries with ts > this.
        let scan_start_ts = crate::vector::wal::current_timestamp();
        index.set_last_replayed_timestamp(scan_start_ts);

        let mut snap = LogicalSnapshot::new(
            store.snapshot(),
            Arc::clone(&schema),
            Arc::new(RwLock::new(HashMap::new())),
            self.execution_options,
        );
        let mut start_from: Option<VertexKey> = None;
        loop {
            let (vertices, next) = snap.scan_vertices(None, start_from, 1000)?;
            for vk in vertices {
                let k = CanonicalKey::Vertex(vk);
                if let Ok(Some(Primitive::FloatVector(v))) = snap.get_value(&k, prop_key_id) {
                    index.insert(&EntityKey::Vertex(vk), &v)?;
                }
            }
            match next {
                Some(v) => start_from = Some(v),
                None => break,
            }
        }
        drop(snap);

        // TODO(v0.3): replay WAL entries written concurrently during the scan.
        // scan_start_ts gates which entries are already covered; concurrent
        // commits write WAL entries with ts > scan_start_ts. A future call to
        // replay_vector_wal after insert catches these. The window is bounded
        // by the scan duration (~ms for typical graphs).

        let snap_path = vector_snapshot_path(&store.path, entity_type, property);
        if let Err(e) = index.save(&snap_path, index.last_replayed_timestamp()) {
            eprintln!("vector index warning: failed to save rebuilt snapshot for '{property}' ({e})");
        }

        vector_indexes.write().insert(key, Arc::new(RwLock::new(Box::new(index))));

        Ok(())
    }

    /// Persist on-disk snapshots for all declared vector indexes.
    ///
    /// Called automatically by [`Graph::close`]. Can also be called explicitly to
    /// checkpoint a long-running process without closing the database.
    pub fn save_all(&self) -> Result<(), StoreError> {
        let (store, schema, vector_indexes) = self.try_refs()?;
        let map = vector_indexes.read();
        for ((entity_type, prop_name), arc) in map.iter() {
            let snap_path = vector_snapshot_path(&store.path, *entity_type, prop_name);
            let guard = arc.read();
            let ts = guard.last_replayed_timestamp();
            guard.save(&snap_path, ts).map_err(|e| StoreError::VectorIndex(e.to_string()))?;
        }
        drop(map);
        // Snapshot + WAL GC form a single checkpoint: once the snapshot is on
        // disk, all WAL entries covered by it are safe to discard.
        gc_vector_wal(&store, &vector_indexes, &schema.read()).ok();
        Ok(())
    }

    /// Persist on-disk snapshot for a single named vector index.
    ///
    /// See [`save_all`](IndexManager::save_all) for caveats on allocation and latency.
    pub fn save(&self, entity_type: VectorEntityType, property: &str) -> Result<(), StoreError> {
        let (store, schema, vector_indexes) = self.try_refs()?;
        let map = vector_indexes.read();
        let key = (entity_type, SmolStr::from(property));
        let found = map.contains_key(&key);
        if let Some(arc) = map.get(&key) {
            let snap_path = vector_snapshot_path(&store.path, entity_type, property);
            let guard = arc.read();
            let ts = guard.last_replayed_timestamp();
            guard.save(&snap_path, ts).map_err(|e| StoreError::VectorIndex(e.to_string()))?;
        }
        drop(map);
        // Snapshot saved → WAL entries covered by it are safe to discard.
        // GC operates per-index (each index's own last_replayed_timestamp gates
        // its WAL prefix), so running it for all indexes is safe — only entries
        // already covered by their own snapshots are deleted.
        if found {
            gc_vector_wal(&store, &vector_indexes, &schema.read()).ok();
        }
        Ok(())
    }
}

impl Clone for Graph {
    fn clone(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            schema: Arc::clone(&self.schema),
            bulk_load_in_progress: AtomicBool::new(false),
            vector_indexes: Arc::clone(&self.vector_indexes),
            index_options: self.index_options.clone(),
            execution_options: self.execution_options,
        }
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
    ctx: LogicalSnapshot,
}

impl ReadSession {
    /// Override runtime execution options for this read session (chainable).
    pub fn with_execution_options(mut self, options: crate::engine::ExecutionOptions) -> Self {
        self.ctx.set_execution_options(options);
        self
    }

    /// Update runtime execution options for this read session.
    pub fn set_execution_options(&mut self, options: crate::engine::ExecutionOptions) {
        self.ctx.set_execution_options(options);
    }

    /// Return a blank traversal bound to this snapshot.
    ///
    /// Call traversal step methods (`V`, `out`, `has`, …) on the returned
    /// [`ReadTraversal`] to build and execute a query.
    pub fn g(&mut self) -> ReadTraversal<'_> {
        self.ctx.clear_caches();
        ReadTraversal::new(&mut self.ctx as &mut dyn GraphCtx)
    }

    /// Execute a bytecode-encoded traversal, returning results natively.
    pub fn execute(
        &mut self,
        bytes: &[u8],
        prop_keys: Option<Vec<String>>,
    ) -> Result<Vec<crate::gremlin::value::Value>, crate::types::StoreError> {
        self.ctx.clear_caches();
        let keys = prop_keys.map(|v| v.into_iter().map(smol_str::SmolStr::from).collect());
        crate::bytecode::execute_read(&mut self.ctx, bytes, keys)
    }

    /// Explain a bytecode-encoded traversal, returning the execution plan tree.
    pub fn explain(
        &mut self,
        bytes: &[u8],
        prop_keys: Option<Vec<String>>,
    ) -> Result<String, crate::types::StoreError> {
        self.ctx.clear_caches();
        let keys = prop_keys.map(|v| v.into_iter().map(smol_str::SmolStr::from).collect());
        crate::bytecode::explain_read(&mut self.ctx, bytes, keys)
    }
}

// ── TxnSession ─────────────────────────────────────────────────────────────────

/// A read-write session backed by an OCC transaction.
///
/// Dropped without `commit()` / `rollback()` → automatic rollback.
///
/// # Example
/// ```
/// # use rocksgraph::{Graph, TraversalBuilder};
/// # let dir = tempfile::tempdir().unwrap();
/// # let graph = Graph::open(dir.path()).unwrap();
/// let mut txn = graph.begin();
/// txn.g().addV("person").property("id", 1i64).property("name", "Alice").next().unwrap();
/// let names = txn.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
/// txn.commit().unwrap();
/// # graph.close().unwrap();
/// ```
pub struct TxnSession {
    ctx: LogicalGraph,
    committed: bool,
}

impl TxnSession {
    /// Override runtime execution options for this transaction session (chainable).
    pub fn with_execution_options(mut self, options: crate::engine::ExecutionOptions) -> Self {
        self.ctx.set_execution_options(options);
        self
    }

    /// Update runtime execution options for this transaction session.
    pub fn set_execution_options(&mut self, options: crate::engine::ExecutionOptions) {
        self.ctx.set_execution_options(options);
    }

    /// Return a blank traversal bound to this transaction.
    ///
    /// Call traversal step methods (`V`, `addV`, `out`, `has`, …) on the
    /// returned [`WriteTraversal`] to build and execute a query or mutation.
    pub fn g(&mut self) -> WriteTraversal<'_> {
        WriteTraversal::new(&mut self.ctx as &mut dyn GraphCtx)
    }

    /// Execute a bytecode-encoded traversal, returning results natively.
    pub fn execute(
        &mut self,
        bytes: &[u8],
        prop_keys: Option<Vec<String>>,
    ) -> Result<Vec<crate::gremlin::value::Value>, crate::types::StoreError> {
        let keys = prop_keys.map(|v| v.into_iter().map(smol_str::SmolStr::from).collect());
        crate::bytecode::execute_write(&mut self.ctx, bytes, keys)
    }

    /// Explain a bytecode-encoded traversal, returning the execution plan tree.
    pub fn explain(
        &mut self,
        bytes: &[u8],
        prop_keys: Option<Vec<String>>,
    ) -> Result<String, crate::types::StoreError> {
        let keys = prop_keys.map(|v| v.into_iter().map(smol_str::SmolStr::from).collect());
        crate::bytecode::explain_write(&mut self.ctx, bytes, keys)
    }

    /// Flush all mutations to RocksDB atomically and consume this session.
    ///
    /// Returns [`StoreError::Conflict`] if a concurrent transaction modified
    /// an overlapping key; retry from scratch with a new `TxnSession`.
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

impl Drop for TxnSession {
    fn drop(&mut self) {
        if !self.committed {
            self.ctx.abort();
        }
    }
}
