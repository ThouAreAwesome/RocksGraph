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

//! Graph element records: [`Vertex`], [`Edge`], and [`Property`].
//!
//! These structs represent the in-memory state of a graph element as it flows
//! through the engine.  Properties are decoded lazily: a vertex or edge loaded
//! from the store carries the raw property blob in `raw_props` and only
//! deserializes it on first access via one of the public property accessors.
//! For elements created in-memory (mutations), the decoded `props` vec is
//! populated directly and `raw_props` is `None`.
//!
//! A vertex can also enter a third state: [`Vertex::label_only`], where
//! `label_id` is known (learned for free from an adjacent edge's value prefix)
//! but no properties have been loaded yet.  Access beyond `id`/`label` requires
//! an upgrade via `ensure_vertex_props_loaded` on the owning [`LogicalGraph`]
//! or [`LogicalSnapshot`](crate::graph::LogicalSnapshot).
//!
//! # Relationship to keys
//!
//! The traversal pipeline usually carries lightweight *keys* ([`VertexKey`],
//! [`EdgeKey`]) inside [`GValue`](crate::types::GValue), and only calls
//! `ctx.get_vertex` / `ctx.get_edges` to obtain the full element record when
//! property data is actually required.  This keeps hot traversal paths allocation-free.
//!
//! # Property access
//!
//! Both [`Vertex`] and [`Edge`] expose accessors:
//!
//! - `get_property` — returns a [`Property`] wrapper (needed when the property itself must flow downstream as a
//!   `GValue::Property`).
//! - `get_value` — returns the bare [`Primitive`] scalar (cheaper when only the value is needed, e.g. in `values()`
//!   steps).
//! - `all_props` — returns a shared view of all properties.
//! - `props_mut` — returns a mutable view of all properties.
//!
//! All accessors take `&mut self` and decode the property blob automatically on
//! first call. No explicit decoding call is required from callers.
//!
//! The reserved keys `"id"` and `"label"` are synthesized on-the-fly rather than
//! stored in `props`, so they are always available.

use crate::types::{
    gvalue::Primitive,
    keys::{CanonicalEdgeKey, CanonicalKey, LabelId, Rank, VertexKey},
    EdgeKey,
};

use std::hash::{Hash, Hasher};

pub type PropDecoder = fn(blob: &[u8], owner: CanonicalKey) -> Option<Vec<Property>>;

// ── Vertex ────────────────────────────────────────────────────────────────

/// The ground-truth vertex record crossing the store ↔ context boundary.
///
/// Returned by `GraphTransaction::get_vertex` and stored inside `LogicalGraph`s
/// overlay.  Properties are decoded lazily on first access via `get_property`,
/// `get_value`, `all_props`, or `props_mut`.
#[derive(Debug)]
pub struct Vertex {
    pub id: VertexKey,
    pub label_id: LabelId,
    /// `true` when only `label_id` is known — learned for free from an
    /// adjacent edge's value. `raw_props`/`props` are both empty placeholders
    /// in this state, not real data — any access beyond `id`/`label` needs a
    /// real fetch first.
    label_only: bool,
    /// Raw property blob from the store. `Some` until first decode, then `None`.
    raw_props: Option<(Box<[u8]>, PropDecoder)>,
    /// Decoded properties. Empty until decoded on first property accessor call (or `None`
    /// raw_props means this was constructed with known props already).
    props: Vec<Property>,
}

impl Vertex {
    /// Construct a vertex with already-decoded properties (mutation / admin path).
    #[inline]
    pub fn with_props(id: VertexKey, label_id: LabelId, props: Vec<Property>) -> Self {
        Vertex { id, label_id, label_only: false, raw_props: None, props }
    }

    /// Construct a vertex from raw store bytes (lazy-decode path).
    ///
    /// `props` starts empty and is decoded lazily on first property access.
    #[inline]
    pub fn from_raw(id: VertexKey, label_id: LabelId, raw: Box<[u8]>, decoder: PropDecoder) -> Self {
        Vertex { id, label_id, label_only: false, raw_props: Some((raw, decoder)), props: Vec::new() }
    }

    /// Construct a label-only vertex — `label_id` is known (from an adjacent edge's
    /// value prefix), but no property data has been loaded yet. Access to any
    /// property beyond `id`/`label` requires an upgrade via `ensure_vertex_props_loaded`.
    #[inline]
    pub fn label_only(id: VertexKey, label_id: LabelId) -> Self {
        Vertex { id, label_id, label_only: true, raw_props: None, props: Vec::new() }
    }

    /// Returns `true` if this vertex carries only a label (no property data).
    #[inline]
    pub fn is_label_only(&self) -> bool {
        self.label_only
    }

    #[inline]
    fn ensure_decoded(&mut self) {
        if let Some((raw, decoder)) = self.raw_props.take() {
            let owner = CanonicalKey::Vertex(self.id);
            self.props = decoder(&raw, owner).unwrap_or_default();
        }
    }

    /// Returns a shared view of all decoded properties, triggering decoding on first call.
    #[inline]
    pub fn all_props(&mut self) -> &[Property] {
        self.ensure_decoded();
        &self.props
    }

    /// Returns a mutable view of all decoded properties, triggering decoding on first call.
    #[inline]
    pub fn props_mut(&mut self) -> &mut Vec<Property> {
        self.ensure_decoded();
        &mut self.props
    }

    /// Returns a [`Property`] wrapper for `prop_key_id`, or `None` if not present.
    ///
    /// Decodes the property blob on first call; subsequent calls are O(props) scans.
    /// The reserved keys `"id"` and `"label"` are synthesized without a `props` scan.
    #[inline]
    pub fn get_property(&mut self, prop_key_id: u16) -> Option<Property> {
        use crate::types::prop_key::{ID_KEY_ID, LABEL_KEY_ID};
        if prop_key_id == ID_KEY_ID {
            return Some(Property {
                owner: CanonicalKey::Vertex(self.id),
                key: ID_KEY_ID,
                value: Primitive::Int64(self.id),
            });
        }
        if prop_key_id == LABEL_KEY_ID {
            return Some(Property {
                owner: CanonicalKey::Vertex(self.id),
                key: LABEL_KEY_ID,
                value: Primitive::Int32(self.label_id),
            });
        }
        self.ensure_decoded();
        self.props.iter().find(|p| p.key == prop_key_id).cloned()
    }

    /// Returns the bare [`Primitive`] scalar for `prop_key_id`, or `None` if not present.
    ///
    /// Decodes the property blob on first call; cheaper than [`get_property`](Vertex::get_property)
    /// when the `Property` wrapper is not needed downstream.
    #[inline]
    pub fn get_value(&mut self, prop_key_id: u16) -> Option<Primitive> {
        use crate::types::prop_key::{ID_KEY_ID, LABEL_KEY_ID};
        if prop_key_id == ID_KEY_ID {
            return Some(Primitive::Int64(self.id));
        }
        if prop_key_id == LABEL_KEY_ID {
            return Some(Primitive::Int32(self.label_id));
        }
        self.ensure_decoded();
        self.props.iter().find(|p| p.key == prop_key_id).map(|p| p.value.clone())
    }
}

// ── Edge ──────────────────────────────────────────────────────────────────

/// The ground-truth edge record crossing the store ↔ context boundary.
///
/// Always in canonical `Out` orientation.  Properties are decoded lazily on first
/// access via `get_property`, `get_value`, `all_props`, or `props_mut`.
#[derive(Debug)]
pub struct Edge {
    pub src_id: VertexKey,
    pub label_id: LabelId,
    pub dst_id: VertexKey,
    pub rank: Rank,
    /// Label of the source vertex, when known from the edge's value prefix
    /// (IN-direction reads give this the src label; OUT-direction reads give this the dst label).
    pub src_label: Option<LabelId>,
    /// Label of the destination vertex, when known from the edge's value prefix.
    pub dst_label: Option<LabelId>,
    /// Raw property blob from the store. `Some` until first decode, then `None`.
    raw_props: Option<(Box<[u8]>, PropDecoder)>,
    /// Decoded properties. Empty until decoded on first property accessor call.
    props: Vec<Property>,
}

impl Edge {
    /// Construct an edge with already-decoded properties (mutation / admin path).
    #[inline]
    pub fn with_props(
        src_id: VertexKey,
        label_id: LabelId,
        dst_id: VertexKey,
        rank: Rank,
        props: Vec<Property>,
        src_label: Option<LabelId>,
        dst_label: Option<LabelId>,
    ) -> Self {
        Edge { src_id, label_id, dst_id, rank, src_label, dst_label, raw_props: None, props }
    }

    /// Construct an edge from raw store bytes (lazy-decode path).
    ///
    /// `props` starts empty and is decoded lazily on first property access.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn from_raw(
        src_id: VertexKey,
        label_id: LabelId,
        dst_id: VertexKey,
        rank: Rank,
        raw: Box<[u8]>,
        decoder: PropDecoder,
        src_label: Option<LabelId>,
        dst_label: Option<LabelId>,
    ) -> Self {
        Edge {
            src_id,
            label_id,
            dst_id,
            rank,
            src_label,
            dst_label,
            raw_props: Some((raw, decoder)),
            props: Vec::new(),
        }
    }

    #[inline]
    fn ensure_decoded(&mut self) {
        if let Some((raw, decoder)) = self.raw_props.take() {
            let cek =
                CanonicalEdgeKey { src_id: self.src_id, label_id: self.label_id, rank: self.rank, dst_id: self.dst_id };
            let owner = CanonicalKey::Edge(cek);
            self.props = decoder(&raw, owner).unwrap_or_default();
        }
    }

    /// Returns a shared view of all decoded properties, triggering decoding on first call.
    #[inline]
    pub fn all_props(&mut self) -> &[Property] {
        self.ensure_decoded();
        &self.props
    }

    /// Returns a mutable view of all decoded properties, triggering decoding on first call.
    #[inline]
    pub fn props_mut(&mut self) -> &mut Vec<Property> {
        self.ensure_decoded();
        &mut self.props
    }

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
            rank: self.rank,
        }
    }

    #[inline]
    pub fn edge_key_in(&self) -> EdgeKey {
        EdgeKey {
            primary_id: self.dst_id,
            direction: super::Direction::IN,
            label_id: self.label_id,
            secondary_id: self.src_id,
            rank: self.rank,
        }
    }

    /// Returns a [`Property`] wrapper for `prop_key_id`, or `None` if not present.
    ///
    /// Decodes the property blob on first call. The reserved key `"label"` is
    /// synthesized without a `props` scan.
    #[inline]
    pub fn get_property(&mut self, prop_key_id: u16) -> Option<Property> {
        use crate::types::prop_key::{LABEL_KEY_ID, RANK_KEY_ID};
        if LABEL_KEY_ID == prop_key_id {
            return Some(Property {
                owner: CanonicalKey::Edge(self.canonical_key()),
                key: LABEL_KEY_ID,
                value: Primitive::Int32(self.label_id),
            });
        }
        if RANK_KEY_ID == prop_key_id {
            return Some(Property {
                owner: CanonicalKey::Edge(self.canonical_key()),
                key: RANK_KEY_ID,
                value: Primitive::UInt16(self.rank),
            });
        }
        self.ensure_decoded();
        self.props.iter().find(|p| p.key == prop_key_id).cloned()
    }

    /// Returns the bare [`Primitive`] scalar for `prop_key_id`, or `None` if not present.
    ///
    /// Decodes the property blob on first call; cheaper than [`get_property`](Edge::get_property)
    /// when the `Property` wrapper is not needed downstream.
    #[inline]
    pub fn get_value(&mut self, prop_key_id: u16) -> Option<Primitive> {
        use crate::types::prop_key::{LABEL_KEY_ID, RANK_KEY_ID};
        if LABEL_KEY_ID == prop_key_id {
            return Some(Primitive::Int32(self.label_id));
        }
        if RANK_KEY_ID == prop_key_id {
            return Some(Primitive::UInt16(self.rank));
        }
        self.ensure_decoded();
        self.props.iter().find(|p| prop_key_id == p.key).map(|p| p.value.clone())
    }
}

/// Identity-only equality: two `Vertex` handles are the same vertex iff `id` and `label_id`
/// match, regardless of their property contents. This is deliberate, not an oversight —
/// properties are excluded so that comparing a vertex fetched before a property update against
/// one fetched after still reports them as the same vertex, matching how callers reason about
/// graph identity (a vertex doesn't become "a different vertex" because a property changed).
impl PartialEq for Vertex {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.label_id == other.label_id
    }
}

impl Eq for Vertex {}

/// Identity-only equality, mirroring [`Vertex`]'s rationale above: `src_id` + `label_id` +
/// `rank` + `dst_id` form an edge's full identity tuple (`rank` distinguishes parallel edges in
/// multi-edge mode); properties are intentionally excluded.
impl PartialEq for Edge {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.src_id == other.src_id
            && self.label_id == other.label_id
            && self.rank == other.rank
            && self.dst_id == other.dst_id
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
    pub key: u16,
    pub value: Primitive,
}

impl Hash for Property {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner.hash(state);
        self.key.hash(state);
        self.value.hash(state);
    }
}
