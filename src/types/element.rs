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

use crate::types::{
    gvalue::Primitive,
    keys::{CanonicalEdgeKey, CanonicalKey, LabelId, Rank, VertexKey},
    prop_key::{PropKey, ID, LABEL},
    EdgeKey,
};

use std::hash::{Hash, Hasher};

// ── Vertex ────────────────────────────────────────────────────────────────

/// The ground-truth vertex record crossing the store ↔ context boundary.
///
/// Returned by `GraphTransaction::get_vertex` and stored inside `LogicalGraph`'s
/// overlay.  The traversal engine accesses properties directly via
/// `ctx.get_vertex(key)` without copying or dereferencing an extra wrapper.
/// There is no `Existence` field — the store never returns tombstoned elements.
#[derive(Debug)]
pub struct Vertex {
    pub id: VertexKey,
    pub label_id: LabelId,
    pub props: Vec<Property>,
}

impl Vertex {
    #[inline]
    pub fn get_property(&self, key: &PropKey) -> Option<Property> {
        if ID == *key {
            return Some(Property { owner: CanonicalKey::Vertex(self.id), key: ID, value: Primitive::Int64(self.id) });
        }
        if LABEL == *key {
            return Some(Property {
                owner: CanonicalKey::Vertex(self.id),
                key: LABEL,
                value: Primitive::Int32(self.label_id as i32),
            });
        }
        self.props.iter().find(|p| p.key == *key).cloned()
    }
    #[inline]
    pub fn get_value(&self, key: &PropKey) -> Option<Primitive> {
        if ID == *key {
            return Some(Primitive::Int64(self.id));
        }
        if LABEL == *key {
            return Some(Primitive::Int32(self.label_id as i32));
        }
        self.props.iter().find(|p| p.key == *key).map(|p| p.value.clone())
    }
}
// ── Edge ──────────────────────────────────────────────────────────────────

/// The ground-truth edge record crossing the store ↔ context boundary.
///
/// Always in canonical `Out` orientation.  The engine derives the directed
/// `EdgeKey` from `canonical_key()` plus the direction it requested.
#[derive(Debug)]
pub struct Edge {
    pub src_id: VertexKey,
    pub label_id: LabelId,
    pub dst_id: VertexKey,
    pub props: Vec<Property>,
    pub rank: Rank,
}

impl Edge {
    /// Extract the direction-free canonical key (same as the `edges_out` CF key).
    #[inline]
    pub fn canonical_key(&self) -> CanonicalEdgeKey {
        CanonicalEdgeKey { src_id: self.src_id, label_id: self.label_id, rank: self.rank, dst_id: self.dst_id }
    }
    #[inline]
    pub fn edge_key_out(&self) -> EdgeKey {
        EdgeKey {
            primary_id: self.src_id,
            direction: super::Direction::OUT,
            label_id: self.label_id,
            secondary_id: self.dst_id,
            rank: 0,
        }
    }

    #[inline]
    pub fn edge_key_in(&self) -> EdgeKey {
        EdgeKey {
            primary_id: self.dst_id,
            direction: super::Direction::IN,
            label_id: self.label_id,
            secondary_id: self.src_id,
            rank: 0,
        }
    }

    #[inline]
    pub fn get_property(&self, key: &PropKey) -> Option<Property> {
        if LABEL == *key {
            return Some(Property {
                owner: CanonicalKey::Edge(self.canonical_key()),
                key: LABEL,
                value: Primitive::Int32(self.label_id as i32),
            });
        }
        self.props.iter().find(|p| p.key == *key).cloned()
    }
    #[inline]
    pub fn get_value(&self, key: &PropKey) -> Option<Primitive> {
        if LABEL == *key {
            return Some(Primitive::Int32(self.label_id as i32));
        }
        self.props.iter().find(|p| *key == p.key).map(|p| p.value.clone())
    }
}

impl PartialEq for Vertex {
    fn eq(&self, other: &Self) -> bool {
        // Compare basic fields
        if self.id != other.id || self.label_id != other.label_id {
            return false;
        }

        // Lock both sides to compare properties
        self.props == other.props
    }
}

impl Eq for Vertex {}

impl PartialEq for Edge {
    fn eq(&self, other: &Self) -> bool {
        // Compare basic fields
        if self.src_id != other.src_id
            || self.label_id != other.label_id
            || self.rank != other.rank
            || self.dst_id != other.dst_id
        {
            return false;
        }

        // Lock both sides to compare properties
        self.props == other.props
    }
}

impl Eq for Edge {}

// ── Property ─────────────────────────────────────────────────────────────────

/// A single property value together with its owning element.
///
/// `owner` identifies the vertex or edge this property belongs to.  The engine
/// uses `owner` to call mutation methods on the transaction (e.g. for `drop()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub owner: CanonicalKey,
    pub key: PropKey,
    pub value: Primitive,
}

impl Hash for Property {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner.hash(state);
        self.key.hash(state);
        self.value.hash(state);
    }
}
