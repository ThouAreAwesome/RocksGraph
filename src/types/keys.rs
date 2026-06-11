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

use std::fmt::Display;

/// Unique identifier for a vertex.
pub type VertexKey = i64;

/// Numeric id for an edge label, mapped via the schema registry.
/// 12 bits are used semantically (max 4 096 distinct labels); stored as u16.
pub type LabelId = u16;

/// Disambiguates parallel edges sharing the same (src, label, dst) triple.
pub type Rank = u16;

// ── Direction ─────────────────────────────────────────────────────────────────

/// The traversal direction of an edge reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    OUT,
    IN,
}

// ── CanonicalEdgeKey ──────────────────────────────────────────────────────────

/// A direction-free edge identity in canonical `Out` orientation.
///
/// Used as the key type in the transaction's edge index and the dirty set.
/// Maps 1-to-1 with the `edges_out` CF key on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub fn out_e(src: VertexKey, label: LabelId, dst: VertexKey, rank: Rank) -> Self {
        Self { primary_id: src, direction: Direction::OUT, label_id: label, secondary_id: dst, rank }
    }

    /// `IN`-direction key viewed from the destination.
    pub fn in_e(src: VertexKey, label: LabelId, dst: VertexKey, rank: Rank) -> Self {
        Self { primary_id: dst, direction: Direction::IN, label_id: label, secondary_id: src, rank }
    }

    /// Flip to the opposite direction (swaps `primary_id` ↔ `secondary_id`).
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
    pub fn canonical(self) -> Self {
        match self.direction {
            Direction::OUT => self,
            Direction::IN => self.flip(),
        }
    }

    /// Extract the direction-free `CanonicalEdgeKey`.
    pub fn canonical_edge_key(self) -> CanonicalEdgeKey {
        let out = self.canonical();
        CanonicalEdgeKey { src_id: out.primary_id, label_id: out.label_id, rank: out.rank, dst_id: out.secondary_id }
    }
}

// ── CanonicalKey ──────────────────────────────────────────────────────────────

/// Direction-free identity for any graph element.
///
/// Used in `Property.owner` and the transaction dirty set.  All variants are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CanonicalKey {
    Empty, // Used for properties that haven't been assigned an owner yet (e.g. in AddVStep)
    Vertex(VertexKey),
    Edge(CanonicalEdgeKey),
}
