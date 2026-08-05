// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Structured Python exception hierarchy mirroring [`rocksgraph::StoreError`]'s
//! layer classification (`StoreError::category()`), so callers can catch by
//! failure class (`except rocksgraph.SchemaError`) instead of a flat
//! `RuntimeError` for every failure.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

use rocksgraph::StoreError as CoreError;

create_exception!(
    _rocksgraph,
    StoreError,
    PyException,
    "Base exception for all RocksGraph storage/query errors. Never raised directly — always \
     one of its subclasses (StorageError, TransactionError, SchemaError, IntegrityError, \
     VectorError, QueryError)."
);
create_exception!(
    _rocksgraph,
    StorageError,
    StoreError,
    "The storage engine itself is unhealthy (RocksDB failure, I/O error, corrupt data, \
     misconfiguration). Not retryable."
);
create_exception!(
    _rocksgraph,
    TransactionError,
    StoreError,
    "A transaction could not proceed due to concurrent access (OCC conflict, lock error) or \
     an in-progress bulk load. `Conflict` is retryable — retry the whole transaction."
);
create_exception!(
    _rocksgraph,
    SchemaError,
    StoreError,
    "A schema declaration or strictness rule was violated, conflicted, or exhausted its ID space."
);
create_exception!(
    _rocksgraph,
    IntegrityError,
    StoreError,
    "A data integrity constraint was violated: missing key, duplicate vertex/edge, dropping a \
     vertex with incident edges, or writing to a read-only snapshot."
);
create_exception!(
    _rocksgraph,
    VectorError,
    StoreError,
    "A vector index operation failed (dimension mismatch, capacity, I/O, etc.)."
);
create_exception!(
    _rocksgraph,
    QueryError,
    StoreError,
    "The query itself is invalid: unsupported construct, type mismatch, or malformed traversal."
);

/// Register the exception hierarchy on the `_rocksgraph` module.
pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("StoreError", py.get_type_bound::<StoreError>())?;
    m.add("StorageError", py.get_type_bound::<StorageError>())?;
    m.add("TransactionError", py.get_type_bound::<TransactionError>())?;
    m.add("SchemaError", py.get_type_bound::<SchemaError>())?;
    m.add("IntegrityError", py.get_type_bound::<IntegrityError>())?;
    m.add("VectorError", py.get_type_bound::<VectorError>())?;
    m.add("QueryError", py.get_type_bound::<QueryError>())?;
    Ok(())
}

/// Map a `rocksgraph::StoreError` to the matching Python exception subclass,
/// using `StoreError::category()` as the single source of truth for the
/// Rust-layer → Python-class mapping.
pub(crate) fn store_error_to_pyerr(e: CoreError) -> PyErr {
    let msg = e.to_string();
    match e.category() {
        "storage" => StorageError::new_err(msg),
        "transaction" => TransactionError::new_err(msg),
        "schema" => SchemaError::new_err(msg),
        "integrity" => IntegrityError::new_err(msg),
        "vector" => VectorError::new_err(msg),
        "query" => QueryError::new_err(msg),
        other => StoreError::new_err(format!("[{other}] {msg}")),
    }
}
