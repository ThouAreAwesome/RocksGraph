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

pub mod extract_end_vertex_filter;
pub mod merge_adde_ids;
pub mod merge_addv_id;
pub mod merge_end_vertex_filter;
pub mod merge_v_id_filter; // Renamed from merget_v_id_filter
pub mod reorder_filter;

use crate::types::{gvalue::Primitive, keys::Rank, StoreError};

/// Converts a `Primitive` rank value (as written by `.property("rank", N)` or
/// `.has("rank", N)`) into a `Rank`, range-checked against `u16`.
///
/// `Primitive::UInt16` — the canonical representation `Edge::get_value(RANK_KEY_ID)` now
/// returns — passes through directly with no range check needed; `Int32`/`Int64` stay
/// supported for ergonomic literals like `.property("rank", 5)`.
///
/// Shared by `merge_adde_ids` (folding `property("rank", N)` into `AddE`),
/// `merge_end_vertex_filter` (folding `has("rank", N)` into `OutE`/`InE`/`BothE`), and
/// `HasPropertyStep::new` (normalizing an unmerged `.has("rank", N)` filter so it compares
/// like-for-like against the `UInt16` runtime value).
pub(crate) fn primitive_to_rank(value: &Primitive) -> Result<Rank, StoreError> {
    match value {
        Primitive::UInt16(r) => Ok(*r),
        Primitive::Int32(r) => {
            if *r < 0 || *r > u16::MAX as i32 {
                return Err(StoreError::UnexpectedDataType("rank must be between 0 and 65535".into()));
            }
            Ok(*r as u16)
        }
        Primitive::Int64(r) => {
            if *r < 0 || *r > u16::MAX as i64 {
                return Err(StoreError::UnexpectedDataType("rank must be between 0 and 65535".into()));
            }
            Ok(*r as u16)
        }
        _ => Err(StoreError::UnexpectedDataType("only integers can be edge rank".into())),
    }
}
