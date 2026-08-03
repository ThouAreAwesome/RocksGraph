// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::{path::Path, sync::Arc};

use rocksdb::{Cache, ColumnFamilyDescriptor, OptimisticTransactionDB, Options};

use crate::{
    store::{
        rocks::{
            cf_options, snapshot::Snapshot, transaction::Transaction, CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA,
            CF_VERTEX_DEGREE, CF_VERTICES,
        },
        traits::GraphStore,
    },
    types::StoreError,
};

/// Storage-tuning options for the RocksDB backend.
///
/// These settings are re-applied **every time** [`crate::Graph::open`] is called
/// (unlike [`GraphOptions`], which is persisted on first creation).
/// Changes take effect after the next `open()` call.
///
/// ## When each option takes effect
///
/// Options differ in *how retroactively* they apply to existing on-disk data:
///
/// | Option | When effective | Retroactive? |
/// |---|---|---|
/// | `max_background_jobs` | next open | yes — live-configurable in RocksDB |
/// | `block_cache_size` | next open | yes — controls the in-process cache |
/// | `write_buffer_size` | next open | yes — applies to new memtables |
/// | `max_write_buffer_number` | next open | yes — applies to new memtables |
/// | `vertex_block_size` | next open | **no** — only new SST files; existing SSTsretain their block size |
/// | `edge_block_size` | next open | **no** — only new SST files |
/// | `cache_index_and_filter_blocks` | next open | **no** — only affects newly-opened block tables |
///
/// Block-size and index-caching changes take full effect only after the next
/// compaction rewrites the affected SST files.
///
/// # Quick Reference — size by deployment
///
/// | Deployment | `block_cache_size` | `write_buffer_size` | `max_write_buffer_number` | `max_background_jobs` |
/// |---|---|---|---|---|
/// | Dev / CI | 256 MiB (override) | 128 MiB (default) | 3 (default) | 4 (default) |
/// | Small prod (16 GB RAM) | 4–6 GiB | 256 MiB | 4 | 4 |
/// | Medium prod (64 GB RAM) | 20–30 GiB | 512 MiB | 4–6 | 8 |
/// | Large prod (256 GB RAM) | 80–120 GiB | 1 GiB | 6–8 | 16 |
///
/// `block_cache_size` rule of thumb: allocate ~30–50% of available RAM.
/// For power-law graphs (e.g. social networks), this typically covers 90%+
/// of read queries.
///
/// # Example
/// ```
/// # use rocksgraph::{Graph, RocksOptions, vector::VectorRuntimeOptions};
/// # let dir = tempfile::tempdir().unwrap();
/// // Small production server: 16 GB RAM
/// let opts = RocksOptions {
///     block_cache_size:         5 * 1024 * 1024 * 1024, // 5 GiB
///     write_buffer_size:        256 * 1024 * 1024,       // 256 MiB
///     max_write_buffer_number:  4,
///     max_background_jobs:      4,
///     ..RocksOptions::default()
/// };
/// let graph = Graph::open_with_rocksdb_options(dir.path(), Default::default(), opts, VectorRuntimeOptions::default()).unwrap();
/// # graph.close().unwrap();
/// ```
///
/// [`GraphOptions`]: crate::schema::GraphOptions
#[derive(Debug, Clone)]
pub struct RocksOptions {
    // ── In-process memory (retroactive: effective immediately on next open) ──
    /// Shared LRU block cache for the vertex and edge CFs.
    ///
    /// A single cache is shared across all four data CFs so memory flows to
    /// whichever CF is actually hot, rather than being statically partitioned.
    /// This is the **single most impactful tuning knob** for read-heavy workloads.
    ///
    /// **Retroactive**: applies immediately to all reads after the next `open()`.
    ///
    /// **Why 1 GiB default**: close to the full on-disk data of the
    /// soc-LiveJournal1 graph (~900 MB across all 4 CFs: vertices ~243 MB,
    /// edges_out ~300 MB, edges_in ~300 MB, vertex_degree ~58 MB), so it still
    /// keeps most of that working set warm in RAM without assuming a
    /// large-memory host. This is a conservative baseline suitable for a
    /// modest single-node deployment; it does not assume the multi-GB headroom
    /// a dedicated production box would have. For CI / unit tests, override
    /// with `with_block_cache(256 * 1024 * 1024)`. In production, set to
    /// 30–50% of available RAM (see the deployment table above).
    pub block_cache_size: usize,

    /// Per-CF memtable (write buffer) size before a flush to an SST file is
    /// triggered.  Larger values reduce the number of L0 SST files generated
    /// per unit of data written, which lowers compaction pressure.
    ///
    /// **Retroactive**: applies to new memtables created after the next `open()`.
    ///
    /// **Why 128 MiB default**: RocksDB's own built-in default is 64 MiB, which
    /// was tuned for HDDs. On SSDs, flushing more frequently creates smaller L0
    /// files that compact quickly but generate more compaction I/O overhead.
    /// 128 MiB doubles the flush interval, halving the number of L0 files
    /// produced per write volume, while staying well within the memory budget
    /// of a development machine. Together with `max_write_buffer_number = 3`,
    /// the peak in-memory memtable footprint per CF is 3 × 128 MiB = 384 MiB.
    pub write_buffer_size: usize,

    /// Maximum number of memtables (write buffers) that may be held in memory
    /// simultaneously per CF before writes are stalled.  One memtable is
    /// actively receiving writes; the rest are waiting to be flushed.
    /// Increasing this value absorbs write bursts without stalling.
    ///
    /// **Retroactive**: applies to new memtable lifecycles after the next `open()`.
    ///
    /// **Why 3 default**: `1` (only active memtable) would stall writes the
    /// instant a flush is triggered. `2` gives one buffer for flushing, but
    /// any flush latency spike immediately stalls writes. `3` adds a second
    /// waiting memtable, which absorbs a full flush period of incoming writes
    /// even if one flush runs slow — a practical minimum for stable throughput
    /// under bursty write workloads. Values above 3 trade memory for resilience
    /// against sustained flush latency.
    pub max_write_buffer_number: i32,

    // ── Concurrency (retroactive: effective immediately on next open) ─────────
    /// Total number of background threads shared by flush and compaction across
    /// the entire database.  The most direct lever for keeping L0 SST file
    /// count low under sustained write load.
    ///
    /// Insufficient background jobs cause L0 file count to grow, which
    /// increases read amplification (more files to search per point lookup)
    /// and eventually triggers write stalls.
    ///
    /// **Retroactive**: thread count adjusts immediately after the next `open()`.
    ///
    /// **Why 4 default**: matches RocksDB's built-in default. With 4 data CFs
    /// (`vertices`, `vertex_degree`, `edges_out`, `edges_in`), a single
    /// background job per CF is the floor; 4 jobs allow simultaneous flush of
    /// all CFs without queuing. It is also a safe minimum on any machine with
    /// ≥ 4 CPU cores and does not over-subscribe single-core CI environments.
    /// In production, `max(4, num_cpu_cores / 2)` is the standard formula.
    pub max_background_jobs: i32,

    // ── SST block layout (NOT retroactive: only affects newly written SST files)
    /// SST data-block size for the **vertex** CFs (`vertices`, `vertex_degree`).
    ///
    /// Vertex CFs are accessed almost exclusively via point lookups (`hasId`,
    /// `get_degree`).  A block read always fetches the full block even if only
    /// one record is needed, so smaller blocks reduce wasted I/O bandwidth.
    ///
    /// **Not retroactive**: existing SST files keep their original block size;
    /// the new value only applies to files written after the next compaction.
    ///
    /// **Why 4 KiB default**: the SSD NAND page size and OS filesystem block
    /// size are both 4 KB on virtually all modern hardware. A 4 KB RocksDB
    /// block maps exactly to one OS page: the read request is aligned, no
    /// extra bytes are wasted to sub-page rounding, and the block lands cleanly
    /// in one OS page cache entry. For a typical vertex record of ~50–100 bytes
    /// (key + label + a few properties), a 4 KB block holds ~40–80 vertices —
    /// dense enough to keep the SST index small (one index entry per block)
    /// while keeping the over-read per point lookup at most 4 KB.
    pub vertex_block_size: usize,

    /// SST data-block size for the **edge** CFs (`edges_out`, `edges_in`).
    ///
    /// Edge CFs are accessed primarily via prefix-range scans (`outE`, `inE`,
    /// `bothE`), which read consecutive keys.  Larger blocks amortise the SST
    /// seek overhead and decompression cost across more records per I/O,
    /// improving throughput on multi-hop traversals and full-graph scans.
    ///
    /// **Not retroactive**: existing SST files keep their original block size;
    /// the new value only applies to files written after the next compaction.
    ///
    /// **Why 16 KiB default**: edge records are ~30 bytes each (22-byte key +
    /// ~8-byte value for a no-property edge). A 16 KB block holds ~500 such
    /// records. During an `outE()` scan the engine reads edges sequentially, so
    /// each 16 KB block read serves ~500 records before the next I/O — giving
    /// low read-amplification for scans. 16 KB is also 4× the SSD page size,
    /// which means each block read issues 4 consecutive SSD page reads in a
    /// single sequential I/O — optimal for SSD prefetching. Going larger (32–64
    /// KB) helps on scan-heavy workloads but hurts point-lookup latency when
    /// only one edge is needed (e.g., `GetEStep`).
    pub edge_block_size: usize,

    /// Store index and bloom-filter blocks inside `block_cache_size` rather
    /// than in a separate, uncapped memory pool.
    ///
    /// When `false` (the old RocksDB default), index and filter blocks are
    /// allocated outside the block cache, making total memory usage hard to
    /// bound and invisible to cache accounting. When `true`, they compete with
    /// data blocks for the same budget, but cache utilisation is accurate and
    /// total memory usage is predictable.
    ///
    /// Enabling this also activates `pin_l0_filter_and_index_blocks_in_cache`
    /// automatically, which keeps the filter/index blocks for the hottest
    /// (L0) SST files pinned and prevents their eviction.
    ///
    /// **Not retroactive**: affects how newly-opened block tables are configured;
    /// existing open SST table handles retain their previous setting.
    ///
    /// **Why `true` default**: with `false`, bloom filters for 4 data CFs can
    /// quietly allocate hundreds of MiB outside the cache, making the effective
    /// memory footprint unpredictable and causing OOM surprises in constrained
    /// environments. `true` makes total memory usage bounded by `block_cache_size`
    /// and visible. The L0 pinning compensates for the most-frequently-hit
    /// filter/index blocks competing with data blocks for cache space.
    pub cache_index_and_filter_blocks: bool,
}

impl Default for RocksOptions {
    fn default() -> Self {
        Self {
            block_cache_size: 1024 * 1024 * 1024,
            write_buffer_size: 128 * 1024 * 1024,
            max_write_buffer_number: 3,
            max_background_jobs: 4,
            vertex_block_size: 4 * 1024,
            edge_block_size: 16 * 1024,
            cache_index_and_filter_blocks: true,
        }
    }
}

/// RocksDB-backed graph store using `OptimisticTransactionDB`.
/// This struct owns the underlying RocksDB database handle.
/// Call the `begin` method to start a new transaction against this store.
pub struct RocksStorage {
    pub(crate) db: Arc<OptimisticTransactionDB>,
    /// Retained so `get_ticker_count` can be called after the DB is open.
    /// `open_cf_descriptors` takes `&Options`, so `opts` is not consumed.
    /// Wrapped in Mutex because Options is Send but not Sync.
    #[cfg(feature = "rocksdb-stats")]
    opts: std::sync::Mutex<Options>,
}

impl RocksStorage {
    /// Open (or create) the database at `path` with the given storage options.
    ///
    /// Creates all four column families if they do not exist yet:
    /// `vertices`, `vertex_degree`, `edges_out`, and `edges_in`.
    pub fn open(path: impl AsRef<Path>, rocksdb_opts: &RocksOptions) -> Result<Self, StoreError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // Background flush + compaction threads are DB-wide (not per-CF).
        opts.set_max_background_jobs(rocksdb_opts.max_background_jobs);
        #[cfg(feature = "rocksdb-stats")]
        opts.enable_statistics();

        // Shared block cache: one pool across all CFs so memory flows to whichever
        // is hot rather than being statically partitioned per CF.
        let block_cache = Cache::new_lru_cache(rocksdb_opts.block_cache_size);

        // ── CF options via shared factories — the bulk loader uses the same
        // factories without block_cache; here we add the shared live-DB cache.
        let mut edge_block_opts = cf_options::edge_block_opts(rocksdb_opts);
        // Larger blocks amortise SST seek overhead during prefix scans (outE / inE).
        edge_block_opts.set_block_cache(&block_cache);
        let edge_cf_opts = cf_options::edge_cf_opts(rocksdb_opts, &edge_block_opts);

        // Small blocks match point-lookup access patterns (one vertex per read).
        let mut vertex_block_opts = cf_options::vertex_block_opts(rocksdb_opts);
        vertex_block_opts.set_block_cache(&block_cache);
        let vertex_cf_opts = cf_options::vertex_cf_opts(rocksdb_opts, &vertex_block_opts);

        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_VERTICES, vertex_cf_opts.clone()),
            ColumnFamilyDescriptor::new(CF_VERTEX_DEGREE, vertex_cf_opts),
            ColumnFamilyDescriptor::new(CF_EDGES_OUT, edge_cf_opts.clone()),
            ColumnFamilyDescriptor::new(CF_EDGES_IN, edge_cf_opts),
            ColumnFamilyDescriptor::new(CF_SCHEMA, Options::default()),
        ];

        let db = OptimisticTransactionDB::open_cf_descriptors(&opts, path, cfs).map_err(StoreError::RocksDb)?;

        Ok(Self {
            db: Arc::new(db),
            #[cfg(feature = "rocksdb-stats")]
            opts: std::sync::Mutex::new(opts),
        })
    }

    /// Check for and recover from an interrupted bulk load.
    ///
    /// If a `BULK_LOAD_IN_PROGRESS` marker exists in the schema CF, either
    /// auto-clear it (ingest succeeded, cleanup was lost) or return
    /// `IncompleteLoad` (ingest never happened).
    pub(crate) fn recover_bulk_load_crash(&self) -> Result<(), StoreError> {
        let cf_s = self.db.cf_handle(CF_SCHEMA);
        if cf_s.is_none() {
            return Ok(());
        }
        let cf_s = cf_s.unwrap();
        let marker_bytes =
            self.db.get_cf(&cf_s, crate::bulk::loader::BULK_LOAD_IN_PROGRESS_KEY).map_err(StoreError::RocksDb)?;
        let marker_state = marker_bytes.as_ref().and_then(|v| v.first().copied());
        match marker_state {
            None => Ok(()), // clean shutdown
            Some(crate::bulk::loader::MARKER_POST_SNAPSHOT) | Some(crate::bulk::loader::MARKER_POST_INGEST) => {
                // All data committed; just clear the stale marker
                let mut cleanup = rocksdb::WriteBatchWithTransaction::<true>::default();
                cleanup.delete_cf(&cf_s, crate::bulk::loader::BULK_LOAD_IN_PROGRESS_KEY);
                self.db.write(cleanup).map_err(StoreError::RocksDb)?;
                Ok(())
            }
            Some(crate::bulk::loader::MARKER_PRE_INGEST) | Some(_) => {
                // SST files written but not ingested, or unknown marker
                Err(StoreError::IncompleteLoad {
                    msg: "bulk load interrupted before ingest — retry load_initial".into(),
                })
            }
        }
    }

    /// Load schema from CF_SCHEMA, or initialize it with defaults if not present.
    pub fn load_schema(
        &self,
        defaults: crate::schema::definition::GraphOptions,
    ) -> Result<crate::schema::Schema, StoreError> {
        use super::CF_SCHEMA;
        use crate::{
            schema::definition::{DataType, EdgeMode, PropKeyConfig, Schema, SchemaMode},
            types::kv_codec::{
                decode_schema_label_value, decode_schema_meta, decode_schema_prop_value, encode_schema_meta,
                SCHEMA_KIND_EDGE_LABEL, SCHEMA_KIND_META, SCHEMA_KIND_PROP_KEY, SCHEMA_KIND_VERTEX_LABEL,
                SCHEMA_META_KEY,
            },
        };
        use rocksdb::IteratorMode;

        let cf = self.db.cf_handle(CF_SCHEMA).ok_or(StoreError::MissingColumnFamily(CF_SCHEMA))?;

        let mut schema = Schema::new();

        if let Some(meta_bytes) = self.db.get_cf(&cf, SCHEMA_META_KEY).map_err(StoreError::RocksDb)? {
            let (version, edge_mode_u8, schema_mode_u8) =
                decode_schema_meta(&meta_bytes).ok_or(StoreError::CorruptData("invalid schema metadata"))?;
            schema.version = version;
            schema.edge_mode = EdgeMode::from_u8(edge_mode_u8).ok_or(StoreError::CorruptData("invalid edge mode"))?;
            schema.mode = SchemaMode::from_u8(schema_mode_u8).ok_or(StoreError::CorruptData("invalid schema mode"))?;
        } else {
            // Brand new. Save defaults.
            schema.version = 0;
            schema.edge_mode = defaults.edge_mode;
            schema.mode = defaults.mode;

            let meta_bytes = encode_schema_meta(schema.version, schema.edge_mode.to_u8(), schema.mode.to_u8());
            self.db.put_cf(&cf, SCHEMA_META_KEY, meta_bytes).map_err(StoreError::RocksDb)?;
        }

        // Iterate CF_SCHEMA to load everything
        let iter = self.db.iterator_cf(&cf, IteratorMode::Start);
        for item in iter {
            let (k, v) = item.map_err(StoreError::RocksDb)?;
            if k.is_empty() {
                continue;
            }
            let kind = k[0];
            if kind == SCHEMA_KIND_META {
                continue;
            }

            // Only label and prop-key records have UTF-8 names at k[1..];
            // vector index keys (0x10) have a binary prefix and are handled
            // by load_vector_configs.
            if kind == SCHEMA_KIND_VERTEX_LABEL || kind == SCHEMA_KIND_EDGE_LABEL || kind == SCHEMA_KIND_PROP_KEY {
                let name_str = std::str::from_utf8(&k[1..])
                    .map_err(|_| StoreError::CorruptData("invalid schema name encoding"))?;

                match kind {
                    SCHEMA_KIND_VERTEX_LABEL => {
                        let id = decode_schema_label_value(&v)
                            .ok_or(StoreError::CorruptData("invalid vertex label value"))?;
                        schema.vertex_labels.insert(id, smol_str::SmolStr::new(name_str));
                        schema.persisted_vertex_labels.insert(id);
                    }
                    SCHEMA_KIND_EDGE_LABEL => {
                        let id =
                            decode_schema_label_value(&v).ok_or(StoreError::CorruptData("invalid edge label value"))?;
                        schema.edge_labels.insert(id, smol_str::SmolStr::new(name_str));
                        schema.persisted_edge_labels.insert(id);
                    }
                    SCHEMA_KIND_PROP_KEY => {
                        let (id, data_type_u8) =
                            decode_schema_prop_value(&v).ok_or(StoreError::CorruptData("invalid prop key value"))?;
                        let data_type = DataType::from_u8(data_type_u8)
                            .ok_or(StoreError::CorruptData("invalid data type discriminant"))?;
                        schema.prop_keys.insert(id, smol_str::SmolStr::new(name_str));
                        schema.prop_key_types.insert(id, PropKeyConfig { data_type });
                        schema.persisted_prop_keys.insert(id);
                    }
                    _ => unreachable!(),
                }
            }
        }

        Ok(schema)
    }
}

#[cfg(feature = "rocksdb-stats")]
impl RocksStorage {
    /// Returns bloom-filter ticker counters followed by internal RocksDB stats.
    ///
    /// Returns a formatted statistics string covering all data column families.
    ///
    /// # Ticker stats (aggregated across ALL CFs via the shared Statistics object)
    ///
    /// **Bloom filter — SST file full filter (point lookups):**
    /// - `bloom.filter.useful`               — reads short-circuited (key absent, no I/O)
    /// - `bloom.filter.full.positive`        — filter said "might be present" → block read
    /// - `bloom.filter.full.true.positive`   — filter positive AND key found (true positive)
    /// - false-positive rate = (full.positive − full.true.positive) / full.positive
    ///
    /// **Bloom filter — memtable prefix filter (in-flight writes only):**
    /// - `bloom.filter.prefix.*` counters are for the *memtable* prefix bloom filter only.
    ///   They will be 0 when all data is in SST files (e.g., after a bulk load + flush).
    ///   Non-zero values appear only when there are active memtable writes being read.
    ///
    /// **Block cache — data, index, and filter blocks:**
    /// - `block.cache.data.hit/miss`   — data block hits vs misses (the main cache load)
    /// - `block.cache.index.hit/miss`  — index block cache effectiveness
    /// - `block.cache.filter.hit/miss` — filter block cache effectiveness
    /// - hit rate = hit / (hit + miss); < 80% → cache too small for working set
    ///
    /// # Per-CF compaction stats
    /// Compaction, SST file sizes, read/write amplification, and file read latency
    /// histograms for each of the four data CFs (vertices, vertex_degree, edges_out,
    /// edges_in).  The schema CF is intentionally omitted — it is tiny and rarely active.
    pub fn statistics(&self) -> Option<String> {
        use rocksdb::statistics::Ticker;

        // ── Ticker stats (shared Statistics object covers all CFs) ────────────────
        let opts = self.opts.lock().unwrap();
        let hit_b = opts.get_ticker_count(Ticker::BlockCacheDataHit);
        let miss_b = opts.get_ticker_count(Ticker::BlockCacheDataMiss);
        let hit_i = opts.get_ticker_count(Ticker::BlockCacheIndexHit);
        let miss_i = opts.get_ticker_count(Ticker::BlockCacheIndexMiss);
        let hit_f = opts.get_ticker_count(Ticker::BlockCacheFilterHit);
        let miss_f = opts.get_ticker_count(Ticker::BlockCacheFilterMiss);
        let cache_bytes_read = opts.get_ticker_count(Ticker::BlockCacheBytesRead);

        let pct = |hit: u64, miss: u64| -> String {
            let total = hit + miss;
            if total == 0 {
                "n/a".into()
            } else {
                format!("{:.1}%", 100.0 * hit as f64 / total as f64)
            }
        };

        let tickers = format!(
            "--- Bloom Filter (SST file, aggregated across all CFs) ---\n\
             bloom.filter.useful               : {}\n\
             bloom.filter.full.positive        : {}\n\
             bloom.filter.full.true.positive   : {}\n\
             bloom.filter.prefix.checked       : {} (memtable only; 0 when data is in SSTs)\n\
             bloom.filter.prefix.useful        : {}\n\
             bloom.filter.prefix.true.positive : {}\n\
             \n\
             --- Block Cache Hit Rates (aggregated across all CFs) ---\n\
             data  blocks: hit={hit_b:>10}  miss={miss_b:>10}  hit_rate={}\n\
             index blocks: hit={hit_i:>10}  miss={miss_i:>10}  hit_rate={}\n\
             filter blocks:hit={hit_f:>10}  miss={miss_f:>10}  hit_rate={}\n\
             cache_bytes_read: {} MB",
            opts.get_ticker_count(Ticker::BloomFilterUseful),
            opts.get_ticker_count(Ticker::BloomFilterFullPositive),
            opts.get_ticker_count(Ticker::BloomFilterFullTruePositive),
            opts.get_ticker_count(Ticker::BloomFilterPrefixChecked),
            opts.get_ticker_count(Ticker::BloomFilterPrefixUseful),
            opts.get_ticker_count(Ticker::BloomFilterPrefixTruePositive),
            pct(hit_b, miss_b),
            pct(hit_i, miss_i),
            pct(hit_f, miss_f),
            cache_bytes_read / (1024 * 1024),
        );
        drop(opts);

        // ── Per-CF compaction + SST stats (property_value_cf covers each CF) ─────
        // property_value("rocksdb.stats") only reports the "default" CF (schema).
        // Use property_value_cf + "rocksdb.cfstats" to get real data CF stats.
        let cf_stats: String = [CF_VERTICES, CF_VERTEX_DEGREE, CF_EDGES_OUT, CF_EDGES_IN]
            .iter()
            .filter_map(|cf_name| {
                let cf = self.db.cf_handle(cf_name)?;
                let stats = self.db.property_value_cf(&cf, "rocksdb.cfstats").ok().flatten()?;
                Some(format!("\n=== CF: {cf_name} ===\n{stats}"))
            })
            .collect();

        Some(format!("{tickers}\n{cf_stats}"))
    }
}

impl GraphStore for RocksStorage {
    type Snapshot = Snapshot;
    type Txn = Transaction;

    fn snapshot(&self) -> Snapshot {
        Snapshot::new(Arc::clone(&self.db))
    }

    fn begin(&self) -> Transaction {
        Transaction::new(Arc::clone(&self.db))
    }
}
