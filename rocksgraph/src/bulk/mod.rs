// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Bulk loading and offline SST ingestion subsystem.
//!
//! Provides the [`BulkLoader`] session for high-throughput initial database bootstrap,
//! bypassing transaction and WAL overhead via offline external sorting and RocksDB SST ingestion.

pub(crate) mod loader;
pub(crate) mod sort;

#[allow(deprecated)]
pub use loader::{
    BulkEdge, BulkLoadStats, BulkLoader, BulkSchema, BulkVertex, IntoBulkEdge, IntoBulkVertex, SstBulkLoader,
};
