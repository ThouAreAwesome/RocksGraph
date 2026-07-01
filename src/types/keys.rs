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

//! Key and identifier types for graph elements.
//!
//! RocksGraph uses a small family of types to address vertices and edges at
//! different levels of abstraction:
//!
//! | Type | What it identifies | Copy? | Size |
//! |---|---|---|---|
//! | [`VertexKey`] | A single vertex | yes | 8 B |
//! | [`CanonicalEdgeKey`] | An edge, direction-free | yes | 22 B |
//! | [`EdgeKey`] | An edge **with** traversal direction | yes | 24 B |
//! | [`CanonicalKey`] | Either kind of element (or nothing) | yes | 24 B |
//!
//! # Canonical vs directed edges
//!
//! RocksDB stores each edge exactly once in `edges_out` CF using [`CanonicalEdgeKey`]
//! (always in Out orientation: `src → dst`).  An `edges_in` CF stores the symmetric
//! reverse index.
//!
//! Inside the traversal pipeline, [`EdgeKey`] records which direction the traverser
//! arrived from.  This is required so that `path()` and `select()` can distinguish
//! "the same edge traversed outward" from "the same edge traversed inward".
//!
//! Use [`EdgeKey::canonical_edge_key`] to strip the direction and obtain the key
//! suitable for storage lookups.

use std::fmt::Display;
use std::str::FromStr;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use smol_str::SmolStr;

use crate::types::error::StoreError;

/// Unique identifier for a vertex.
pub type VertexKey = i64;

/// Numeric id for a vertex/edge label, mapped via the schema registry.
/// Valid range is `1..=i32::MAX`; `0` is reserved for "no such label" (never assigned
/// by the schema).  The sign bit (bit 31) is reserved — negative values are used as
/// internal sentinels (see `UNRESOLVED_LABEL_ID`) and are never written to disk.
pub type LabelId = i32;

/// Disambiguates parallel edges sharing the same (src, label, dst) triple.
pub type Rank = u16;

/// Default rank for single-edge mode and default relationships.
pub const DEFAULT_RANK: Rank = 0;

/// Byte width of the packed `[src_id][label_id][dst_id][rank]` encoding used by
/// `CanonicalEdgeKey`/`EdgeKey::to_id_string`: `src_id (i64, 8B) + label_id (i32, 4B)
/// + dst_id (i64, 8B) + rank (u16, 2B)`.
const EDGE_ID_ENCODED_LEN: usize = 8 + 4 + 8 + 2;

// ── Direction ─────────────────────────────────────────────────────────────────

/// The traversal direction of an edge reference.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Outgoing: the traverser moves from `src` to `dst`.
    OUT,
    /// Incoming: the traverser moves from `dst` to `src`.
    IN,
}

/// Direction for the `degree_pushdown` optimizer — extends `Direction` with a `Both` variant
/// so that `both([]).count()` can also be rewritten to an O(1) degree lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DegreeDirection {
    Out,
    In,
    Both,
}

/// Suffix cursor for paginating edges adjacent to a specific vertex.
/// Represents the physical key of the last returned edge (specifically, the suffix parts).
/// Ordered exactly as the database sorting keys: (label_id, secondary_id, rank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdjacentEdgeCursor {
    pub label_id: LabelId,
    pub secondary_id: VertexKey,
    pub rank: Rank,
}

impl AdjacentEdgeCursor {
    /// Create a cursor from an existing Edge.
    #[inline]
    pub fn from_edge(edge: &super::element::Edge, direction: Direction) -> Self {
        AdjacentEdgeCursor {
            label_id: edge.label_id,
            secondary_id: match direction {
                Direction::OUT => edge.dst_id,
                Direction::IN => edge.src_id,
            },
            rank: edge.rank,
        }
    }
}

/// Optional filters and pagination parameters for querying adjacent edges.
#[derive(Debug, Clone, Copy)]
pub struct AdjacentEdgesOptions<'a> {
    pub label: Option<LabelId>,
    pub dst: Option<&'a [VertexKey]>,
    pub rank: Option<&'a [Rank]>,
    pub start_from: Option<AdjacentEdgeCursor>,
}

/// Scenarios for configurable batch sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatchScenario {
    ScanVertices,
    ScanEdges,
    GetAdjacentEdges,
}

// ── CanonicalEdgeKey ──────────────────────────────────────────────────────────

/// A direction-free edge identity in canonical `Out` orientation.
///
/// Used as the key type in the transaction's edge index and the dirty set.
/// Maps 1-to-1 with the `edges_out` CF key on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalEdgeKey {
    pub src_id: VertexKey,
    pub label_id: LabelId,
    pub dst_id: VertexKey,
    pub rank: Rank,
}

impl CanonicalEdgeKey {
    /// Build a directed `EdgeKey` for Out-direction traversal.
    #[inline]
    pub fn out_key(&self) -> EdgeKey {
        EdgeKey {
            primary_id: self.src_id,
            direction: Direction::OUT,
            label_id: self.label_id,
            secondary_id: self.dst_id,
            rank: self.rank,
        }
    }

    /// Build a directed `EdgeKey` for In-direction traversal.
    #[inline]
    pub fn in_key(&self) -> EdgeKey {
        EdgeKey {
            primary_id: self.dst_id,
            direction: Direction::IN,
            label_id: self.label_id,
            secondary_id: self.src_id,
            rank: self.rank,
        }
    }
}

impl Display for CanonicalEdgeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({} -{}-> {})[rank={}]", self.src_id, self.label_id, self.dst_id, self.rank)
    }
}

impl CanonicalEdgeKey {
    /// Encode as Base64 (URL-safe, no padding) of `[src_id:8B][label_id:4B][dst_id:8B][rank:2B]` big-endian.
    pub fn to_id_string(self) -> String {
        let mut buf = [0u8; EDGE_ID_ENCODED_LEN];
        buf[0..8].copy_from_slice(&self.src_id.to_be_bytes());
        buf[8..12].copy_from_slice(&self.label_id.to_be_bytes());
        buf[12..20].copy_from_slice(&self.dst_id.to_be_bytes());
        buf[20..22].copy_from_slice(&self.rank.to_be_bytes());
        URL_SAFE_NO_PAD.encode(buf)
    }
}

impl FromStr for CanonicalEdgeKey {
    type Err = StoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| StoreError::UnexpectedDataType(format!("invalid edge id '{}': not valid base64", s)))?;
        if bytes.len() != EDGE_ID_ENCODED_LEN {
            return Err(StoreError::UnexpectedDataType(format!(
                "invalid edge id '{}': expected {EDGE_ID_ENCODED_LEN} decoded bytes, got {}",
                s,
                bytes.len()
            )));
        }
        Ok(CanonicalEdgeKey {
            src_id: i64::from_be_bytes(bytes[0..8].try_into().unwrap()),
            label_id: i32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            dst_id: i64::from_be_bytes(bytes[12..20].try_into().unwrap()),
            rank: u16::from_be_bytes(bytes[20..22].try_into().unwrap()),
        })
    }
}

// ── EdgeKey ───────────────────────────────────────────────────────────────────

/// A directed edge key carried by traversers.
///
/// `GValue::Edge` wraps an `EdgeKey` so that traversal direction (Out vs In)
/// is preserved for `path()` / `select()` identity.
/// Persistence always uses `CanonicalEdgeKey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeKey {
    pub primary_id: VertexKey,
    pub direction: Direction,
    pub label_id: LabelId,
    pub secondary_id: VertexKey,
    pub rank: Rank,
}

impl EdgeKey {
    /// Canonical `Out`-direction key for `(src → dst)`.
    #[inline]
    pub fn out_e(src: VertexKey, label: LabelId, dst: VertexKey, rank: Rank) -> Self {
        Self { primary_id: src, direction: Direction::OUT, label_id: label, secondary_id: dst, rank }
    }

    /// `IN`-direction key viewed from the destination.
    #[inline]
    pub fn in_e(src: VertexKey, label: LabelId, dst: VertexKey, rank: Rank) -> Self {
        Self { primary_id: dst, direction: Direction::IN, label_id: label, secondary_id: src, rank }
    }

    /// Flip to the opposite direction (swaps `primary_id` ↔ `secondary_id`).
    #[inline]
    pub fn flip(&self) -> Self {
        Self {
            primary_id: self.secondary_id,
            direction: match self.direction {
                Direction::OUT => Direction::IN,
                Direction::IN => Direction::OUT,
            },
            label_id: self.label_id,
            secondary_id: self.primary_id,
            rank: self.rank,
        }
    }

    /// Return the canonical `Out`-direction form.
    #[inline]
    pub fn canonical(self) -> Self {
        match self.direction {
            Direction::OUT => self,
            Direction::IN => self.flip(),
        }
    }

    /// Extract the direction-free `CanonicalEdgeKey`.
    #[inline]
    pub fn canonical_edge_key(self) -> CanonicalEdgeKey {
        match self.direction {
            Direction::OUT => CanonicalEdgeKey {
                src_id: self.primary_id,
                label_id: self.label_id,
                dst_id: self.secondary_id,
                rank: self.rank,
            },
            Direction::IN => CanonicalEdgeKey {
                src_id: self.secondary_id,
                label_id: self.label_id,
                dst_id: self.primary_id,
                rank: self.rank,
            },
        }
    }

    /// Stable globally-unique string id: Base64 of the packed CanonicalEdgeKey.
    #[inline]
    pub fn to_id_string(self) -> SmolStr {
        let (src, dst) = match self.direction {
            Direction::OUT => (self.primary_id, self.secondary_id),
            Direction::IN => (self.secondary_id, self.primary_id),
        };
        let mut buf = [0u8; EDGE_ID_ENCODED_LEN];
        buf[0..8].copy_from_slice(&src.to_be_bytes());
        buf[8..12].copy_from_slice(&self.label_id.to_be_bytes());
        buf[12..20].copy_from_slice(&dst.to_be_bytes());
        buf[20..22].copy_from_slice(&self.rank.to_be_bytes());
        URL_SAFE_NO_PAD.encode(buf).into()
    }
}

// ── CanonicalKey ──────────────────────────────────────────────────────────────

/// Direction-free identity for any graph element.
///
/// Used in `Property.owner` and the transaction dirty set.  All variants are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalKey {
    /// Placeholder for properties that have not yet been assigned to an element
    /// (e.g. properties accumulated inside `AddVStep` before the vertex is committed).
    Empty,
    Vertex(VertexKey),
    Edge(CanonicalEdgeKey),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_id_round_trip() {
        let cek = CanonicalEdgeKey { src_id: 1, label_id: 3, dst_id: 2, rank: 0 };
        let s = cek.to_id_string();
        assert_eq!(s.len(), 30, "Base64 edge id must be 30 chars");
        let parsed: CanonicalEdgeKey = s.parse().unwrap();
        assert_eq!(parsed, cek);

        // Large values
        let big = CanonicalEdgeKey { src_id: i64::MAX, label_id: i32::MAX, dst_id: i64::MIN, rank: u16::MAX };
        let s2 = big.to_id_string();
        assert_eq!(s2.len(), 30);
        assert_eq!(s2.parse::<CanonicalEdgeKey>().unwrap(), big);
    }

    #[test]
    fn test_edge_id_from_str_rejects_bad_input() {
        assert!("".parse::<CanonicalEdgeKey>().is_err());
        assert!("too-short".parse::<CanonicalEdgeKey>().is_err());
        assert!("not-valid-base64!!!".parse::<CanonicalEdgeKey>().is_err());
        // Valid base64 but wrong byte length (e.g. 4 bytes → 6 chars after decode?)
        // Actually URL_SAFE_NO_PAD.decode of "" gives 0 bytes → fails the len check
        // A 30-char string that decodes to 22 bytes → should work.
    }
}
