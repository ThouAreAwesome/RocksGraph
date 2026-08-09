// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! High-throughput offline SST ingestion.
//!
//! [`BulkLoader`] streams vertices and edges into external sorters, generates
//! sorted RocksDB SST files directly, and ingests them atomically — bypassing
//! the WAL and OCC for orders-of-magnitude faster initial imports.
//!
//! ## Key types
//!
//! | Type | Purpose |
//! |------|---------|
//! | [`BulkLoader`] | Main entry point: `Graph::open_bulk_loader()` |
//! | [`BulkVertex`] / [`BulkEdge`] | Pre-serialized vertex/edge records |
//! | [`IntoBulkVertex`] / [`IntoBulkEdge`] | Conversion trait for iterators |
//! | [`BulkLoadStats`] | Throughput and count statistics after commit |
//!
//! Bulk loading and offline SST ingestion subsystem.
//!
//! Provides the [`BulkLoader`] session for high-throughput initial database bootstrap,
//! bypassing transaction and WAL overhead via offline external sorting and RocksDB SST ingestion.

pub(crate) mod degree;
pub(crate) mod edge_annotator;
pub(crate) mod loader;
pub(crate) mod sort;

#[cfg(test)]
mod tests;

#[allow(deprecated)]
pub use loader::{
    BulkEdge, BulkLoadStats, BulkLoader, BulkSchema, BulkVertex, IntoBulkEdge, IntoBulkVertex, SstBulkLoader,
};
