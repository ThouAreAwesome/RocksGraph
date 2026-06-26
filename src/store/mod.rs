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

//! Persistent storage backends.
//!
//! The storage layer abstracts over RocksDB via the `GraphStore` trait.
//! Key/encoding layout is defined in `rocks/encoding.rs`. The `RocksGraph`
//! implementation wraps an `OptimisticTransactionDB` with OCC-based transactions.
pub mod rocks;
pub mod traits;

pub use rocks::RocksStorage;
