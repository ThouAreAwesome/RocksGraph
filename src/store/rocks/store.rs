// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

use std::{path::Path, sync::Arc};

use rocksdb::{BlockBasedOptions, ColumnFamilyDescriptor, OptimisticTransactionDB, Options, SliceTransform};

use crate::{
    store::{
        rocks::{
            encoding::{CF_EDGES_IN, CF_EDGES_OUT, CF_VERTEX_DEGREE, CF_VERTICES, EDGE_PREFIX_LENGHT},
            transaction::Transaction,
        },
        traits::GraphStore,
    },
    types::StoreError,
};

/// RocksDB-backed graph store using `OptimisticTransactionDB`.
///
/// Owns the database handle.  Call
/// `begin` to start a transaction.
pub struct RocksStorage {
    pub(super) db: Arc<OptimisticTransactionDB>,
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

        // 1. Tell RocksDB the prefix length for prefix seeks
        let mut edge_cf_opts = Options::default();
        edge_cf_opts.create_if_missing(true);
        edge_cf_opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(EDGE_PREFIX_LENGHT));

        // 2. Bloom filter in SST block index (full filter, not block-based, so prefix seek hits it)
        let mut block_opts = BlockBasedOptions::default();
        block_opts.set_bloom_filter(10.0, false); // false = full filter (better for prefix)
        edge_cf_opts.set_block_based_table_factory(&block_opts);

        // 3. Bloom filter in memtable too
        edge_cf_opts.set_memtable_prefix_bloom_ratio(0.1);

        let cfs = [CF_VERTICES, CF_VERTEX_DEGREE, CF_EDGES_OUT, CF_EDGES_IN]
            .into_iter()
            .map(|name| match name {
                CF_EDGES_OUT | CF_EDGES_IN => ColumnFamilyDescriptor::new(name, edge_cf_opts.clone()),
                _ => ColumnFamilyDescriptor::new(name, Options::default()),
            })
            .collect::<Vec<_>>();

        let db = OptimisticTransactionDB::open_cf_descriptors(&opts, path, cfs).map_err(StoreError::RocksDb)?;

        Ok(Self { db: Arc::new(db) })
    }
}

impl GraphStore for RocksStorage {
    type Txn = Transaction;

    fn begin(&self) -> Transaction {
        Transaction::new(Arc::clone(&self.db))
    }
}
