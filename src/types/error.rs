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

//! [`StoreError`] — the unified error type for storage and runtime failures.
//!
//! All fallible operations in the store and traversal engine return
//! `Result<_, StoreError>`.  The variants cover both expected, recoverable
//! conditions (e.g. [`Conflict`](StoreError::Conflict) — retry the transaction)
//! and hard failures (e.g. [`RocksDb`](StoreError::RocksDb) — underlying I/O error).
//!
//! # Retryable errors
//!
//! [`StoreError::Conflict`] is the only variant that callers are expected to retry:
//! it means an OCC (optimistic concurrency control) check failed because another
//! transaction modified a key in this transaction's read-set before the commit.
//! All other errors are terminal for the current transaction.

use std::fmt;

use crate::types::{CanonicalEdgeKey, VertexKey};

#[derive(Debug)]
pub enum StoreError {
    /// A required key was not found.
    ///
    /// Not emitted by the storage layer itself (absent keys return `Ok(None)`);
    /// reserved for higher-level callers that treat absence as a hard error
    /// (e.g. a mutation step that requires a vertex to exist).
    NotFound,
    /// OCC commit failed because a key in the read-set was modified by a
    /// concurrent transaction.  Callers should retry from scratch.
    Conflict,
    /// A lock was poisoned or otherwise could not be acquired. Happens when several traversals mutate the properties
    /// of the same vertex/edge in parallel.
    LockError,
    DuplicateVertex(VertexKey),
    DuplicateEdge(CanonicalEdgeKey),
    /// The element has already been deleted in this transaction's overlay.
    Tombstoned,
    /// A vertex cannot be deleted because it still has one or more incident edges.
    IncidentEdges,
    /// A write operation was attempted on a read-only snapshot context.
    ReadOnly,
    /// A stored byte sequence could not be decoded. The carried string names the
    /// field that failed (e.g. `"vertex value"`, `"edge key"`).
    CorruptData(&'static str),
    /// A required RocksDB column-family handle was not found. Indicates a
    /// database schema mismatch or misconfiguration.
    MissingColumnFamily(&'static str),
    /// An error returned directly by the RocksDB storage engine.
    RocksDb(rocksdb::Error),
    Io(std::io::Error),
    /// A schema definition or strictness rule was violated.
    SchemaViolation(String),
    /// A schema version mismatch or concurrency conflict occurred.
    SchemaConflict(String),
    /// The ID space or limit for schema labels/keys was exhausted.
    SchemaExhausted(String),
    /// A traversal step or feature that is not yet implemented.
    UnsupportedOperation(String),
    /// A value in the pipeline had a type that the current step cannot handle.
    UnexpectedDataType(String),
    /// A generic runtime error from the traversal engine.
    RuntimeError(String),
    /// Catch-all for errors that don't fit any other variant.
    Other(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound => write!(f, "key not found"),
            StoreError::Conflict => write!(f, "transaction conflict; retry"),
            StoreError::LockError => write!(f, "lock error"),
            StoreError::DuplicateVertex(key) => write!(f, "duplicate vertex: {key}"),
            StoreError::DuplicateEdge(key) => write!(f, "duplicate edge: {key}"),
            StoreError::Tombstoned => write!(f, "element is tombstoned"),
            StoreError::IncidentEdges => write!(f, "cannot drop vertex with incident edges"),
            StoreError::ReadOnly => write!(f, "write operation on read-only snapshot"),
            StoreError::CorruptData(ctx) => write!(f, "corrupt data: {ctx}"),
            StoreError::MissingColumnFamily(name) => write!(f, "missing column family: {name}"),
            StoreError::RocksDb(e) => write!(f, "storage engine error: {e}"),
            StoreError::Io(e) => write!(f, "I/O error: {e}"),
            StoreError::SchemaViolation(msg) => write!(f, "schema violation: {msg}"),
            StoreError::SchemaConflict(msg) => write!(f, "schema conflict: {msg}"),
            StoreError::SchemaExhausted(msg) => write!(f, "schema exhausted: {msg}"),
            StoreError::UnsupportedOperation(msg) => write!(f, "unsupported operation: {msg}"),
            StoreError::RuntimeError(msg) => write!(f, "runtime error: {msg}"),
            StoreError::UnexpectedDataType(msg) => write!(f, "unexpected datatype: {msg}"),
            StoreError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::RocksDb(e) => Some(e),
            StoreError::Io(e) => Some(e),
            StoreError::RuntimeError(_) => None,
            StoreError::UnsupportedOperation(_) => None,
            _ => None,
        }
    }
}

impl From<rocksdb::Error> for StoreError {
    fn from(e: rocksdb::Error) -> Self {
        StoreError::RocksDb(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}
