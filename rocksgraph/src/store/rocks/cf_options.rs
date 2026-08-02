// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared CF option factories used by both `RocksStorage::open` and `SstBulkLoader`.
//!
//! The bulk loader must generate SST files with block-based options identical to
//! the target column families, or RocksDB will reject the SST at ingest time.

use rocksdb::{BlockBasedOptions, Options, SliceTransform};

use crate::{store::rocks::store::RocksOptions, types::kv_codec::EDGE_PREFIX_LENGTH};

const BLOOM_FILTER_BITS_PER_KEY: f64 = 10.0;
const BLOCK_FORMAT_VERSION: i32 = 6;

/// Block-based options shared by vertex and vertex_degree CFs (without block cache —
/// the caller sets `block_cache` if needed).
pub(crate) fn vertex_block_opts(rocksdb_opts: &RocksOptions) -> BlockBasedOptions {
    let mut opts = BlockBasedOptions::default();
    opts.set_bloom_filter(BLOOM_FILTER_BITS_PER_KEY, false);
    opts.set_format_version(BLOCK_FORMAT_VERSION);
    opts.set_block_size(rocksdb_opts.vertex_block_size);
    opts.set_cache_index_and_filter_blocks(rocksdb_opts.cache_index_and_filter_blocks);
    if rocksdb_opts.cache_index_and_filter_blocks {
        opts.set_pin_l0_filter_and_index_blocks_in_cache(true);
    }
    opts
}

/// Full CF options for `CF_VERTICES` and `CF_VERTEX_DEGREE`.
pub(crate) fn vertex_cf_opts(rocksdb_opts: &RocksOptions, block_opts: &BlockBasedOptions) -> Options {
    let mut opts = Options::default();
    opts.set_block_based_table_factory(block_opts);
    opts.set_write_buffer_size(rocksdb_opts.write_buffer_size);
    opts.set_max_write_buffer_number(rocksdb_opts.max_write_buffer_number);
    opts
}

/// Block-based options shared by edges_out and edges_in CFs (without block cache).
pub(crate) fn edge_block_opts(rocksdb_opts: &RocksOptions) -> BlockBasedOptions {
    let mut opts = BlockBasedOptions::default();
    // full filter (not block-based) so prefix seeks hit the bloom filter
    opts.set_bloom_filter(BLOOM_FILTER_BITS_PER_KEY, false);
    opts.set_format_version(BLOCK_FORMAT_VERSION);
    opts.set_block_size(rocksdb_opts.edge_block_size);
    opts.set_cache_index_and_filter_blocks(rocksdb_opts.cache_index_and_filter_blocks);
    if rocksdb_opts.cache_index_and_filter_blocks {
        opts.set_pin_l0_filter_and_index_blocks_in_cache(true);
    }
    opts
}

/// Full CF options for `CF_EDGES_OUT` and `CF_EDGES_IN`.
pub(crate) fn edge_cf_opts(rocksdb_opts: &RocksOptions, block_opts: &BlockBasedOptions) -> Options {
    let mut opts = Options::default();
    opts.set_prefix_extractor(SliceTransform::create_fixed_prefix(EDGE_PREFIX_LENGTH));
    opts.set_block_based_table_factory(block_opts);
    opts.set_write_buffer_size(rocksdb_opts.write_buffer_size);
    opts.set_max_write_buffer_number(rocksdb_opts.max_write_buffer_number);
    // bloom filter in memtable for in-flight writes
    opts.set_memtable_prefix_bloom_ratio(0.1);
    opts
}
