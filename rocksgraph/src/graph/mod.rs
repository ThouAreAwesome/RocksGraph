// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Query-scoped overlay and dirty-tracking.
//!
//! `LogicalGraph` buffers mutations in an in-memory overlay (`HashMap`). On
//! commit, it flushes dirty entries to the store. `LogicalSnapshot` provides a
//! read view for query-only snapshots. Both implement the `GraphCtx` trait so
//! the engine operates uniformly regardless of read-only vs. read-write mode.
//! Query-scoped logical graph — the ground truth for a single traversal.
//! See [`LogicalGraph`] and [`LogicalSnapshot`] for details.

mod config;
mod existence;
mod helpers;
mod logical;
pub(crate) mod schema_cache;
mod snapshot;
#[cfg(test)]
mod tests;

pub(crate) use config::{ScanConfig, StagedSchema};
pub(crate) use existence::Existence;
pub(crate) use logical::LogicalGraph;
pub(crate) use schema_cache::TxSchemaCache;
pub(crate) use snapshot::LogicalSnapshot;
