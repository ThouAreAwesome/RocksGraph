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

pub mod degree_pushdown;
pub mod extract_end_vertex_filter;
pub mod merge_adde_ids;
pub mod merge_end_vertex_filter;
pub mod merge_haslabel_into_edge;
pub mod merge_property_into_add;
pub mod merge_v_id_filter;
pub mod normalize_inv_outv;
pub mod reorder_filter;

use crate::types::SMALL_VECTOR_LENGTH;
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

/// Extracts a vertex-id allowlist from a predicate, for folding `hasId`/`has("id", …)` into a
/// preceding `V`/`Out`/`In`/`Both` step.
///
/// Returns `Ok(None)` when the predicate's *shape* simply isn't an id allowlist (`Ne`, `Gt`,
/// `Between`, `Without`, …) — these are left unfolded, not an error, since they're valid
/// predicates that the unfolded step still evaluates correctly. Returns `Err` only when the
/// shape WAS `Eq`/`Within` but carried a non-integer literal, which is always a caller mistake.
///
/// An empty `Within([])` also returns `Ok(None)`: folding it would clear the id list down to
/// empty, which `VStep`/`EndVertexFilter` would then read as "unconstrained" rather than the
/// "match nothing" the predicate actually means — so it's deliberately left unfolded instead,
/// where `HasIdStep`/`HasPropertyStep` evaluate `Within([])` correctly as always-false.
pub(crate) fn extract_ids_from_predicate(
    pred: &crate::types::PrimitivePredicate,
) -> Result<Option<smallvec::SmallVec<[i64; SMALL_VECTOR_LENGTH]>>, StoreError> {
    use crate::types::{Primitive, PrimitivePredicate};
    use smallvec::smallvec;

    fn to_i64(v: &Primitive) -> Result<i64, StoreError> {
        match v {
            Primitive::Int64(n) => Ok(*n),
            Primitive::Int32(n) => Ok(*n as i64),
            other => {
                Err(StoreError::UnexpectedDataType(format!("expect i32 or i64 type for vertex id, got {other:?}")))
            }
        }
    }

    match pred {
        PrimitivePredicate::Eq(v) => Ok(Some(smallvec![to_i64(v)?])),
        PrimitivePredicate::Within(vs) => {
            let parsed: smallvec::SmallVec<[i64; SMALL_VECTOR_LENGTH]> =
                vs.iter().map(to_i64).collect::<Result<_, _>>()?;
            Ok(if parsed.is_empty() { None } else { Some(parsed) })
        }
        _ => Ok(None),
    }
}
