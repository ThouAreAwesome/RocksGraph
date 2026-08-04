// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

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

use parking_lot::RwLock;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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
        BatchScenario, StoreError,
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
    pub(crate) store: Arc<RocksStorage>,
    pub(crate) schema: Arc<RwLock<Schema>>,
    pub(crate) bulk_load_in_progress: AtomicBool,
    pub(crate) vector_indexes: Arc<RwLock<VectorIndexMap>>,
    pub(crate) index_options: IndexOptions,
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
    ///     GraphOptions {
    ///         mode: SchemaMode::Strict,
    ///         storage: RocksOptions {
    ///             block_cache_size: 5 * 1024 * 1024 * 1024, // 5 GiB
    ///             ..Default::default()
    ///         },
    ///         ..Default::default()
    ///     },
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
        if let Some(ref default_limit) = options.index.default_limit {
            let limit_bytes = default_limit.memory_limit_bytes;
            let map = vector_indexes.read();
            for ((_entity_type, _prop_name), arc) in map.iter() {
                let mut guard = arc.write();
                guard.set_memory_limit(limit_bytes);
                drop(guard);
            }
            drop(map);
        }
        for ov in &options.index.per_index_overrides {
            let map = vector_indexes.read();
            let key = (ov.entity_type, ov.property.clone());
            if let Some(arc) = map.get(&key) {
                let mut guard = arc.write();
                guard.set_memory_limit(ov.limit.memory_limit_bytes);
                drop(guard);
            }
            drop(map);
        }

        Ok(Self {
            store,
            schema: Arc::new(RwLock::new(schema)),
            bulk_load_in_progress: AtomicBool::new(false),
            vector_indexes,
            index_options: options.index,
        })
    }

    /// Open a schema management session for explicit, [`SchemaMode::Strict`]-style schema
    /// declaration. See [`SchemaSession`] for a worked example.
    ///
    /// [`SchemaMode::Strict`]: crate::schema::SchemaMode::Strict
    pub fn open_schema(&self) -> SchemaSession {
        SchemaSession::new(Arc::clone(&self.store), Arc::clone(&self.schema), Some(Arc::clone(&self.vector_indexes)))
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

    /// Return every edge label name currently registered in the schema.
    ///
    /// In [`SchemaMode::Strict`](crate::schema::SchemaMode::Strict) this is the
    /// complete authoritative list.  In [`SchemaMode::Auto`](crate::schema::SchemaMode::Auto)
    /// it reflects whatever labels have been auto-registered by writes so far.
    pub fn edge_label_names(&self) -> Vec<SmolStr> {
        self.schema.read().edge_labels.iter().map(|(_, n)| n.clone()).collect()
    }

    /// Open a read-only snapshot session pinned to the current committed state.
    pub fn read(&self) -> ReadSession {
        ReadSession {
            ctx: LogicalSnapshot::new(
                self.store.snapshot(),
                Arc::clone(&self.schema),
                Arc::clone(&self.vector_indexes),
            ),
        }
    }

    /// Open a read-write transaction session with OCC (Optimistic Concurrency Control).
    pub fn begin(&self) -> TxSession {
        TxSession {
            ctx: LogicalGraph::new(self.store.begin(), Arc::clone(&self.schema), Arc::clone(&self.vector_indexes)),
            committed: false,
        }
    }

    /// Explicitly rebuild the in-memory vector index for `(entity_type, property)`.
    ///
    /// Scans all vertices in the graph, extracts the float vector for `property`,
    /// builds a new in-memory HNSW index from scratch, and atomically swaps it in.
    ///
    /// # Concurrency
    /// Rebuilding does not hold the graph lock or block transactions. Only read
    /// queries that arrive during the rebuild block briefly on the index
    /// write lock (not for the full scan duration).
    ///
    /// # Errors
    /// Returns [`StoreError::VectorIndex`] if no index is declared for
    /// `(entity_type, property)`, if property is missing from schema, or if
    /// `entity_type == Edge` (edge indexes are deferred to v0.3).
    pub fn rebuild_vector_index(&self, entity_type: VectorEntityType, property: &str) -> Result<(), StoreError> {
        use crate::vector::traits::VectorIndex;

        if entity_type == VectorEntityType::Edge {
            return Err(VectorError::Unsupported("edge vector index rebuild is not yet supported (v0.3)".into()).into());
        }

        // 1. Read config from CF_SCHEMA — same format as load_vector_configs.
        let config = read_vector_config(&self.store, entity_type, property)?;

        // 2. Resolve prop_key_id from schema.
        let prop_key_id = {
            let schema = self.schema.read();
            let Some(id) = schema.prop_key_id(property) else {
                return Err(StoreError::VectorIndex(format!("property key '{property}' is not defined in schema")));
            };
            id
        };

        // 3. Build fresh index.
        let mut index = UsearchHnswIndex::new(&config)?;

        // 4. Scan vertices via LogicalSnapshot — reuses existing codec stack.
        let mut snap = LogicalSnapshot::new(
            self.store.snapshot(),
            Arc::clone(&self.schema),
            Arc::new(RwLock::new(HashMap::new())), // not used during rebuild
        );
        let mut start_from: Option<VertexKey> = None;
        loop {
            let (vertices, next) = snap.scan_vertices(None, start_from, 1000)?;
            for vk in vertices {
                let key = CanonicalKey::Vertex(vk);
                if let Ok(Some(Primitive::FloatVector(v))) = snap.get_value(&key, prop_key_id) {
                    index.insert(&EntityKey::Vertex(vk), &v)?;
                }
            }
            match next {
                Some(v) => start_from = Some(v),
                None => break,
            }
        }

        let rebuild_ts = crate::vector::wal::next_timestamp();
        index.set_last_replayed_timestamp(rebuild_ts);

        // Immediately persist the rebuilt snapshot to disk.
        let snap_path = vector_snapshot_path(&self.store.path, entity_type, property);
        if let Err(e) = index.save(&snap_path, rebuild_ts) {
            eprintln!("vector index warning: failed to save rebuilt snapshot for '{property}' ({e})");
        }

        // 5. Swap into map (or insert if added after last open).
        let key = (entity_type, SmolStr::from(property));
        {
            let mut map_guard = self.vector_indexes.write();
            // Overwrite existing entry or insert a new one — handles the case
            // where add_vector_index was called after Graph::open() populated
            // vector_indexes from CF_SCHEMA.
            map_guard.insert(key, Arc::new(RwLock::new(Box::new(index))));
        }

        Ok(())
    }

    /// Persist on-disk snapshots for all declared vector indexes.
    ///
    /// Note: Snapshot persistence on close is an optimization to accelerate cold starts
    /// and bound WAL replay time. Crash recovery via WAL replay guarantees consistency
    /// even if no snapshot file is present.
    pub fn save_vector_indexes(&self) -> Result<(), StoreError> {
        let map = self.vector_indexes.read();
        for ((entity_type, prop_name), arc) in map.iter() {
            let snap_path = vector_snapshot_path(&self.store.path, *entity_type, prop_name);
            let guard = arc.read();
            let ts = guard.last_replayed_timestamp();
            guard.save(&snap_path, ts).map_err(|e| StoreError::VectorIndex(e.to_string()))?;
        }
        Ok(())
    }

    /// Persist on-disk snapshot for a specific named vector index.
    pub fn save_vector_index(&self, entity_type: VectorEntityType, property: &str) -> Result<(), StoreError> {
        let map = self.vector_indexes.read();
        let key = (entity_type, SmolStr::from(property));
        if let Some(arc) = map.get(&key) {
            let snap_path = vector_snapshot_path(&self.store.path, entity_type, property);
            let guard = arc.read();
            let ts = guard.last_replayed_timestamp();
            guard.save(&snap_path, ts).map_err(|e| StoreError::VectorIndex(e.to_string()))?;
        }
        Ok(())
    }

    /// Close the database, persisting snapshots for all vector indexes and releasing RocksDB resources.
    ///
    /// After calling this, no further sessions or queries can be created
    /// from this `Graph` handle or any clone.  In tests, call this before
    /// the temporary directory is dropped so RocksDB can flush and close
    /// its files cleanly.
    pub fn close(self) -> Result<(), StoreError> {
        self.save_vector_indexes()?;
        // GC WAL entries that are already covered by the saved snapshots.
        {
            let schema = self.schema.read();
            gc_vector_wal(&self.store, &self.vector_indexes, &schema)?;
        }
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
        Self {
            store: Arc::clone(&self.store),
            schema: Arc::clone(&self.schema),
            bulk_load_in_progress: AtomicBool::new(false),
            vector_indexes: Arc::clone(&self.vector_indexes),
            index_options: self.index_options.clone(),
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
    ctx: LogicalGraph,
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

    /// Execute a bytecode-encoded traversal, returning results natively.
    pub fn execute(
        &mut self,
        bytes: &[u8],
        prop_keys: Option<Vec<String>>,
    ) -> Result<Vec<crate::gremlin::value::Value>, crate::types::StoreError> {
        let keys = prop_keys.map(|v| v.into_iter().map(smol_str::SmolStr::from).collect());
        crate::bytecode::execute_write(&mut self.ctx, bytes, keys)
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
