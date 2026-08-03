// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`VectorError`] — error type for all vector index operations.

use std::fmt;

use smol_str::SmolStr;

use crate::types::StoreError;

/// Identifies whether a vector property belongs to a vertex or an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorEntityType {
    Vertex = 0,
    Edge = 1,
}

/// Errors raised by vector index operations.
#[derive(Debug)]
pub enum VectorError {
    /// A vector's dimension does not match the declared index dimension.
    DimensionMismatch { expected: usize, actual: usize },
    /// No vector index is declared for the given (entity_type, property) pair.
    IndexNotFound { entity_type: VectorEntityType, property: SmolStr },
    /// An insert was rejected because the index's estimated memory would exceed
    /// the configured limit.
    MemoryLimitExceeded { index: SmolStr, used: usize, limit: usize },
    /// An I/O error outside RocksDB (e.g. snapshot file read/write).
    Io(std::io::Error),
    /// A storage-layer error from the RocksDB engine.
    Store(StoreError),
    /// A runtime error from the underlying ANN engine (capacity, OOM,
    /// internal graph corruption). Distinct from `Unsupported`, which
    /// means "not yet implemented."
    Internal(String),
    /// A feature that is not yet supported (e.g. edge vector indexes in v0.2).
    Unsupported(String),
}

impl fmt::Display for VectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionMismatch { expected, actual } => {
                write!(f, "dimension mismatch: expected {expected}, got {actual}")
            }
            Self::IndexNotFound { entity_type, property } => {
                write!(f, "no vector index for ({entity_type:?}, {property})")
            }
            Self::MemoryLimitExceeded { index, used, limit } => {
                write!(f, "memory limit exceeded for index '{index}': {used} bytes used, limit {limit} bytes")
            }
            Self::Io(e) => write!(f, "vector I/O error: {e}"),
            Self::Store(e) => write!(f, "vector storage error: {e}"),
            Self::Internal(msg) => write!(f, "vector index internal error: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for VectorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Store(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for VectorError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<StoreError> for VectorError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}
