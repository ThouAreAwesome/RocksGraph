// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Persistent storage backends.
//!
//! The storage layer abstracts over RocksDB via the `GraphStore` trait.
//! Key/encoding layout is defined in `rocks/encoding.rs`. The `RocksGraph`
//! implementation wraps an `OptimisticTransactionDB` with OCC-based transactions.
pub(crate) mod rocks;

pub use rocks::RocksOptions;
pub(crate) use rocks::RocksStorage;
