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

use std::{path::Path, sync::Arc};

use rocksdb::{BlockBasedOptions, ColumnFamilyDescriptor, OptimisticTransactionDB, Options, SliceTransform};

use crate::{
    store::{
        rocks::{
            encoding::{CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VERTEX_DEGREE, CF_VERTICES, EDGE_PREFIX_LENGTH},
            snapshot::Snapshot,
            transaction::Transaction,
        },
        traits::GraphStore,
    },
    types::StoreError,
};

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
    /// Open (or create) the database at `path`.
    ///
    /// Creates all four column families if they do not exist yet:
    /// `vertices`, `vertex_degree`, `edges_out`, and `edges_in`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        #[cfg(feature = "rocksdb-stats")]
        opts.enable_statistics();

        // ── Edge CFs: prefix bloom filter (8-byte vertex_id prefix) ─────────────
        let mut edge_cf_opts = Options::default();
        edge_cf_opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(EDGE_PREFIX_LENGTH));
        let mut edge_block_opts = BlockBasedOptions::default();
        // full filter (not block-based) so prefix seeks hit the bloom filter
        edge_block_opts.set_bloom_filter(10.0, false);
        edge_cf_opts.set_block_based_table_factory(&edge_block_opts);
        // bloom filter in memtable for in-flight writes
        edge_cf_opts.set_memtable_prefix_bloom_ratio(0.1);

        // ── Vertex CFs: point-lookup bloom filter ─────────────────────────────
        let mut vertex_block_opts = BlockBasedOptions::default();
        vertex_block_opts.set_bloom_filter(10.0, false);
        let mut vertex_cf_opts = Options::default();
        vertex_cf_opts.set_block_based_table_factory(&vertex_block_opts);

        let cfs = [CF_VERTICES, CF_VERTEX_DEGREE, CF_EDGES_OUT, CF_EDGES_IN, CF_SCHEMA]
            .into_iter()
            .map(|name| match name {
                CF_EDGES_OUT | CF_EDGES_IN => ColumnFamilyDescriptor::new(name, edge_cf_opts.clone()),
                CF_VERTICES | CF_VERTEX_DEGREE => ColumnFamilyDescriptor::new(name, vertex_cf_opts.clone()),
                CF_SCHEMA => ColumnFamilyDescriptor::new(name, Options::default()),
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();

        let db = OptimisticTransactionDB::open_cf_descriptors(&opts, path, cfs).map_err(StoreError::RocksDb)?;

        Ok(Self {
            db: Arc::new(db),
            #[cfg(feature = "rocksdb-stats")]
            opts: std::sync::Mutex::new(opts),
        })
    }

    /// Load schema from CF_SCHEMA, or initialize it with defaults if not present.
    pub fn load_schema(
        &self,
        defaults: crate::schema::definition::GraphOptions,
    ) -> Result<crate::schema::Schema, StoreError> {
        use crate::{
            schema::definition::{Cardinality, DataType, EdgeMode, PropKeyConfig, Schema, SchemaMode},
            store::rocks::encoding::{
                decode_schema_label_value, decode_schema_meta, decode_schema_prop_value, encode_schema_meta, CF_SCHEMA,
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
            let name_bytes = &k[1..];
            let name_str =
                std::str::from_utf8(name_bytes).map_err(|_| StoreError::CorruptData("invalid schema name encoding"))?;

            match kind {
                SCHEMA_KIND_VERTEX_LABEL => {
                    let id =
                        decode_schema_label_value(&v).ok_or(StoreError::CorruptData("invalid vertex label value"))?;
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
                    let (id, data_type_u8, cardinality_u8) =
                        decode_schema_prop_value(&v).ok_or(StoreError::CorruptData("invalid prop key value"))?;
                    let data_type = DataType::from_u8(data_type_u8)
                        .ok_or(StoreError::CorruptData("invalid data type discriminant"))?;
                    let cardinality = Cardinality::from_u8(cardinality_u8)
                        .ok_or(StoreError::CorruptData("invalid cardinality discriminant"))?;
                    schema.prop_keys.insert(id, smol_str::SmolStr::new(name_str));
                    schema.prop_key_types.insert(id, PropKeyConfig { data_type, cardinality });
                    schema.persisted_prop_keys.insert(id);
                }
                _ => {}
            }
        }

        Ok(schema)
    }
}

#[cfg(feature = "rocksdb-stats")]
impl RocksStorage {
    /// Returns bloom-filter ticker counters followed by internal RocksDB stats.
    ///
    /// Key bloom-filter counters to watch:
    /// - `bloom.filter.useful`               — point-lookup reads skipped (filter said "absent")
    /// - `bloom.filter.full.positive`        — filter said "present" (may be false positive)
    /// - `bloom.filter.full.true.positive`   — filter correctly confirmed presence
    /// - `bloom.filter.prefix.checked`       — prefix seeks checked against the filter
    /// - `bloom.filter.prefix.useful`        — prefix seeks skipped by the filter
    /// - `bloom.filter.prefix.true.positive` — prefix filter correctly confirmed presence
    ///
    /// A healthy ratio is `useful >> full.positive`. If both stay 0 after reads,
    /// the filter is not being reached (check that SST files exist on disk).
    pub fn statistics(&self) -> Option<String> {
        use rocksdb::statistics::Ticker;

        let opts = self.opts.lock().unwrap();
        let bloom = format!(
            "--- Bloom Filter Ticker Stats ---\n\
             bloom.filter.useful               : {}\n\
             bloom.filter.full.positive        : {}\n\
             bloom.filter.full.true.positive   : {}\n\
             bloom.filter.prefix.checked       : {}\n\
             bloom.filter.prefix.useful        : {}\n\
             bloom.filter.prefix.true.positive : {}",
            opts.get_ticker_count(Ticker::BloomFilterUseful),
            opts.get_ticker_count(Ticker::BloomFilterFullPositive),
            opts.get_ticker_count(Ticker::BloomFilterFullTruePositive),
            opts.get_ticker_count(Ticker::BloomFilterPrefixChecked),
            opts.get_ticker_count(Ticker::BloomFilterPrefixUseful),
            opts.get_ticker_count(Ticker::BloomFilterPrefixTruePositive),
        );
        drop(opts);

        let internal = self.db.property_value("rocksdb.stats").ok().flatten().unwrap_or_default();
        Some(format!("{bloom}\n\n{internal}"))
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
