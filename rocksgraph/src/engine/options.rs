// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runtime execution options for the Gremlin query engine.

/// Gremlin engine runtime configuration.
///
/// Controls batch sizes for iterator scans and query execution bounds.
/// Can be configured globally on [`GraphOptions`](crate::GraphOptions) or overridden per session
/// via [`ReadSession::with_execution_options`](crate::ReadSession::with_execution_options)
/// and [`TxnSession::with_execution_options`](crate::TxnSession::with_execution_options).
///
/// # Example
/// ```
/// use rocksgraph::ExecutionOptions;
///
/// let opts = ExecutionOptions::default()
///     .with_scan_vertices_batch_size(512)
///     .with_get_adjacent_edges_batch_size(32);
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionOptions {
    /// Number of vertices fetched per batch during full table scans (`g.V()`).
    /// Default: `1024`.
    pub scan_vertices_batch_size: u32,
    /// Number of edges fetched per batch during full table scans (`g.E()`).
    /// Default: `1024`.
    pub scan_edges_batch_size: u32,
    /// Number of adjacent edges fetched per batch when expanding vertices (`out()`, `in()`, `both()`, etc.).
    /// Default: `64`.
    pub get_adjacent_edges_batch_size: u32,
}

impl Default for ExecutionOptions {
    fn default() -> Self {
        Self { scan_vertices_batch_size: 1024, scan_edges_batch_size: 1024, get_adjacent_edges_batch_size: 64 }
    }
}

impl ExecutionOptions {
    /// Create a new `ExecutionOptions` with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the batch size for vertex scans (`g.V()`).
    pub fn with_scan_vertices_batch_size(mut self, size: u32) -> Self {
        self.scan_vertices_batch_size = size;
        self
    }

    /// Set the batch size for edge scans (`g.E()`).
    pub fn with_scan_edges_batch_size(mut self, size: u32) -> Self {
        self.scan_edges_batch_size = size;
        self
    }

    /// Set the batch size for adjacent edge expansions (`out()`, `in()`, `both()`).
    pub fn with_get_adjacent_edges_batch_size(mut self, size: u32) -> Self {
        self.get_adjacent_edges_batch_size = size;
        self
    }
}
