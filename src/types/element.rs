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

//! Graph element records: [`Vertex`], [`Edge`], [`Property`], and [`PropertyMap`].
//!
//! # Property storage — two-state design
//!
//! Properties on an element live in one of three states via [`PropertyMap`]:
//!
//! - **`LabelOnly`** — only `label_id` is known (learned for free from an adjacent
//!   edge's value prefix). No property data has been loaded. Access beyond `id`/`label`
//!   requires an upgrade via `ensure_vertex_props_loaded` on the owning [`LogicalGraph`].
//!
//! - **`Blob`** — the raw v1 offset-index bytes from the store. Reads go through a
//!   binary search on the sorted directory: O(log P), zero allocation. The element
//!   stays in Blob state until the first mutation.
//!
//! - **`Map`** — a `HashMap<u16, Primitive>` produced on the first mutation.
//!   All subsequent reads and writes are O(1). This is also the initial state for
//!   newly created elements (`addV`/`addE`), which have no raw bytes.
//!
//! Only Map-state elements ever appear in the dirty map, so the commit path only
//! calls `encode_props` on Maps — never re-encodes an unchanged Blob.
//!
//! # Relationship to keys
//!
//! The traversal pipeline usually carries lightweight *keys* ([`VertexKey`], [`EdgeKey`])
//! inside [`GValue`](crate::types::GValue), and only fetches full element records when
//! property data is actually required.
//!
//! # `Property` at the API boundary
//!
//! [`Property`] (wrapping `owner + key + value`) is the public type used by [`GraphCtx`]
//! method signatures (`get_property`, `set_property`, `drop_property`). It is NOT stored
//! inside the overlay — the overlay uses `HashMap<u16, Primitive>` and synthesizes a
//! `Property` on demand when callers need the full wrapper.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use crate::types::{
    gvalue::Primitive,
    keys::{CanonicalEdgeKey, CanonicalKey, LabelId, Rank, VertexKey},
    prop_codec, EdgeKey,
};

// ── Shared empty-map sentinel ─────────────────────────────────────────────────

/// Returned by `props()` when the element is in `LabelOnly` state.
static EMPTY_PROPS: OnceLock<HashMap<u16, Primitive>> = OnceLock::new();

#[inline]
fn empty_props() -> &'static HashMap<u16, Primitive> {
    EMPTY_PROPS.get_or_init(HashMap::new)
}

// ── PropertyMap ───────────────────────────────────────────────────────────────

/// Two-state (three-variant) property storage for an element in the overlay.
///
/// See the module-level documentation for the state-transition diagram.
/// Decoding is handled directly by [`prop_codec`] — no function pointers needed.
#[derive(Debug)]
pub(crate) enum PropertyMap {
    /// Only `label_id` is known — no property bytes loaded yet.
    LabelOnly,
    /// Raw v1 offset-index bytes from the store. Reads use binary search; no allocation.
    Blob(Box<[u8]>),
    /// Mutable property map — after the first mutation, or for new elements.
    Map(HashMap<u16, Primitive>),
}

impl PropertyMap {
    #[inline]
    pub(crate) fn is_label_only(&self) -> bool {
        matches!(self, PropertyMap::LabelOnly)
    }

    /// Look up a single property value without triggering a full decode.
    ///
    /// - `LabelOnly` → `None`
    /// - `Blob` → O(log P) binary search, zero allocation
    /// - `Map` → O(1) hash lookup
    #[inline]
    pub(crate) fn get_value(&self, key: u16) -> Option<Primitive> {
        match self {
            PropertyMap::LabelOnly => None,
            PropertyMap::Blob(bytes) => prop_codec::decode_prop_by_key(bytes, key),
            PropertyMap::Map(map) => map.get(&key).cloned(),
        }
    }

    /// Transition `Blob → Map` by fully decoding the blob.
    ///
    /// No-op if already in `Map` or `LabelOnly` state.
    /// For `LabelOnly`, the overlay must load the element from the store before
    /// calling `ensure_map`.
    #[inline]
    pub(crate) fn ensure_map(&mut self) {
        if let PropertyMap::Blob(bytes) = self {
            let map = prop_codec::decode_all_to_map(bytes);
            *self = PropertyMap::Map(map);
        }
    }

    /// Returns a reference to the underlying `HashMap`, or `None` if not in Map state.
    #[inline]
    pub(crate) fn as_map(&self) -> Option<&HashMap<u16, Primitive>> {
        match self {
            PropertyMap::Map(m) => Some(m),
            _ => None,
        }
    }

    /// Returns a mutable reference to the underlying `HashMap`, or `None` if not in Map state.
    #[inline]
    pub(crate) fn as_map_mut(&mut self) -> Option<&mut HashMap<u16, Primitive>> {
        match self {
            PropertyMap::Map(m) => Some(m),
            _ => None,
        }
    }
}

// ── Vertex ────────────────────────────────────────────────────────────────────

/// The ground-truth vertex record crossing the store ↔ context boundary.
///
/// Returned by `GraphTransaction::get_vertex` / `GraphSnapshot::get_vertex` and stored
/// inside the `LogicalGraph` overlay. Properties are accessed through [`PropertyMap`].
#[derive(Debug)]
pub struct Vertex {
    pub id: VertexKey,
    pub label_id: LabelId,
    pub(crate) props: PropertyMap,
}

impl Vertex {
    /// New element created in-memory (`addV`). Starts in empty Map state.
    #[inline]
    pub fn new(id: VertexKey, label_id: LabelId) -> Self {
        Vertex { id, label_id, props: PropertyMap::Map(HashMap::new()) }
    }

    /// Construct a vertex with pre-populated properties.
    /// Used in tests and admin paths.
    #[inline]
    pub(crate) fn with_props(id: VertexKey, label_id: LabelId, props: HashMap<u16, Primitive>) -> Self {
        Vertex { id, label_id, props: PropertyMap::Map(props) }
    }

    /// Construct a vertex from raw v1 blob bytes (lazy-decode path).
    ///
    /// The element stays in `Blob` state until the first mutation or `props()` call.
    #[inline]
    pub(crate) fn from_raw(id: VertexKey, label_id: LabelId, bytes: Box<[u8]>) -> Self {
        Vertex { id, label_id, props: PropertyMap::Blob(bytes) }
    }

    /// Construct a label-only vertex (label learned from an adjacent edge value prefix).
    ///
    /// Access beyond `id`/`label` requires an upgrade via `ensure_vertex_props_loaded`.
    #[inline]
    pub fn label_only(id: VertexKey, label_id: LabelId) -> Self {
        Vertex { id, label_id, props: PropertyMap::LabelOnly }
    }

    /// Returns `true` if this vertex carries only a label (no property data).
    #[inline]
    pub fn is_label_only(&self) -> bool {
        self.props.is_label_only()
    }

    /// Returns the value of property `prop_key_id`, or `None` if not present.
    ///
    /// - In `Blob` state: O(log P) binary search, no allocation.
    /// - In `Map` state: O(1) hash lookup.
    /// - `ID_KEY_ID` and `LABEL_KEY_ID` are synthesized without touching the blob.
    #[inline]
    pub fn get_value(&self, prop_key_id: u16) -> Option<Primitive> {
        use crate::types::prop_key::{ID_KEY_ID, LABEL_KEY_ID};
        if prop_key_id == ID_KEY_ID {
            return Some(Primitive::Int64(self.id));
        }
        if prop_key_id == LABEL_KEY_ID {
            return Some(Primitive::Int32(self.label_id));
        }
        self.props.get_value(prop_key_id)
    }

    /// Returns a [`Property`] wrapper for `prop_key_id`, or `None` if not present.
    #[inline]
    pub fn get_property(&self, prop_key_id: u16) -> Option<Property> {
        let value = self.get_value(prop_key_id)?;
        Some(Property { owner: CanonicalKey::Vertex(self.id), key: prop_key_id, value })
    }

    /// Returns all properties as a `HashMap`, triggering `Blob → Map` if needed.
    ///
    /// **`LabelOnly` state**: returns a shared empty map. The overlay must call
    /// `ensure_vertex_props_loaded` before this method on a `LabelOnly` vertex;
    /// calling `props_mut()` on `LabelOnly` panics.
    #[inline]
    pub(crate) fn props(&mut self) -> &HashMap<u16, Primitive> {
        self.props.ensure_map();
        self.props.as_map().unwrap_or_else(|| empty_props())
    }

    /// Returns a mutable reference to the property map.
    ///
    /// Triggers `Blob → Map` if needed. Panics if the vertex is in `LabelOnly` state
    /// (the overlay must call `ensure_vertex_props_loaded` first).
    #[inline]
    pub(crate) fn props_mut(&mut self) -> &mut HashMap<u16, Primitive> {
        self.props.ensure_map();
        self.props.as_map_mut().expect("props_mut called on LabelOnly vertex")
    }
}

// ── Edge ──────────────────────────────────────────────────────────────────────

/// The ground-truth edge record crossing the store ↔ context boundary.
///
/// Always in canonical `Out` orientation. Properties are accessed through [`PropertyMap`].
#[derive(Debug)]
pub struct Edge {
    pub src_id: VertexKey,
    pub label_id: LabelId,
    pub dst_id: VertexKey,
    pub rank: Rank,
    /// Label of the source vertex when known from the edge's value prefix.
    pub src_label: Option<LabelId>,
    /// Label of the destination vertex when known from the edge's value prefix.
    pub dst_label: Option<LabelId>,
    pub(crate) props: PropertyMap,
}

impl Edge {
    /// New element created in-memory (`addE`). Starts in empty Map state.
    #[inline]
    pub fn new(
        src_id: VertexKey,
        label_id: LabelId,
        dst_id: VertexKey,
        rank: Rank,
        src_label: Option<LabelId>,
        dst_label: Option<LabelId>,
    ) -> Self {
        Edge { src_id, label_id, dst_id, rank, src_label, dst_label, props: PropertyMap::Map(HashMap::new()) }
    }

    /// Construct an edge with pre-populated properties.
    /// Used in tests and admin paths.
    #[inline]
    pub(crate) fn with_props(
        src_id: VertexKey,
        label_id: LabelId,
        dst_id: VertexKey,
        rank: Rank,
        props: HashMap<u16, Primitive>,
        src_label: Option<LabelId>,
        dst_label: Option<LabelId>,
    ) -> Self {
        Edge { src_id, label_id, dst_id, rank, src_label, dst_label, props: PropertyMap::Map(props) }
    }

    /// Construct an edge from raw v1 blob bytes (lazy-decode path).
    #[inline]
    pub(crate) fn from_raw(
        src_id: VertexKey,
        label_id: LabelId,
        dst_id: VertexKey,
        rank: Rank,
        bytes: Box<[u8]>,
        src_label: Option<LabelId>,
        dst_label: Option<LabelId>,
    ) -> Self {
        Edge { src_id, label_id, dst_id, rank, src_label, dst_label, props: PropertyMap::Blob(bytes) }
    }

    /// Returns the value of property `prop_key_id`, or `None` if not present.
    ///
    /// `LABEL_KEY_ID` and `RANK_KEY_ID` are synthesized without touching the blob.
    #[inline]
    pub fn get_value(&self, prop_key_id: u16) -> Option<Primitive> {
        use crate::types::prop_key::{LABEL_KEY_ID, RANK_KEY_ID};
        if prop_key_id == LABEL_KEY_ID {
            return Some(Primitive::Int32(self.label_id));
        }
        if prop_key_id == RANK_KEY_ID {
            return Some(Primitive::UInt16(self.rank));
        }
        self.props.get_value(prop_key_id)
    }

    /// Returns a [`Property`] wrapper for `prop_key_id`, or `None` if not present.
    #[inline]
    pub fn get_property(&self, prop_key_id: u16) -> Option<Property> {
        let value = self.get_value(prop_key_id)?;
        Some(Property { owner: CanonicalKey::Edge(self.canonical_key()), key: prop_key_id, value })
    }

    /// Returns all properties as a `HashMap`, triggering `Blob → Map` if needed.
    #[inline]
    pub(crate) fn props(&mut self) -> &HashMap<u16, Primitive> {
        self.props.ensure_map();
        self.props.as_map().unwrap_or_else(|| empty_props())
    }

    /// Returns a mutable reference to the property map, triggering `Blob → Map` if needed.
    #[inline]
    pub(crate) fn props_mut(&mut self) -> &mut HashMap<u16, Primitive> {
        self.props.ensure_map();
        self.props.as_map_mut().expect("props_mut called on LabelOnly edge")
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
}

impl PartialEq for Vertex {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.label_id == other.label_id
    }
}

impl Eq for Vertex {}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_only_props_is_empty() {
        let mut v = Vertex::label_only(1, 7);
        assert!(v.is_label_only());
        assert!(v.props().is_empty(), "LabelOnly props should return empty map");
        // G10: props() on LabelOnly must leave the element in LabelOnly state (ensure_map is a no-op).
        assert!(v.is_label_only(), "props() must not transition LabelOnly to Map");
        // get_value for id/label still works without loading props
        assert_eq!(v.get_value(crate::types::prop_key::ID_KEY_ID), Some(Primitive::Int64(1)));
        assert_eq!(v.get_value(crate::types::prop_key::LABEL_KEY_ID), Some(Primitive::Int32(7)));
        // any user property is absent
        assert_eq!(v.get_value(100), None);
    }

    #[test]
    fn label_only_stays_label_only_after_get_value() {
        let v = Vertex::label_only(42, 3);
        // get_value never transitions state
        let _ = v.get_value(100);
        assert!(v.is_label_only());
    }

    // ── Gap coverage: G9, G11, G12, G14 ──────────────────────────────────────

    #[test]
    fn g9_props_idempotent_on_map_state() {
        // Calling props() twice on a Map-state vertex must be a no-op — no panic, same result.
        let m: HashMap<u16, Primitive> = [(10u16, Primitive::Int32(42))].into();
        let mut v = Vertex::with_props(1, 1, m);
        assert_eq!(v.props().get(&10), Some(&Primitive::Int32(42)));
        assert_eq!(v.props().get(&10), Some(&Primitive::Int32(42))); // second call
        assert!(matches!(v.props, PropertyMap::Map(_)));
    }

    #[test]
    fn g11_props_mut_on_blob_transitions_to_map() {
        // props_mut() calls ensure_map() internally — Blob → Map, no panic.
        let blob = crate::types::prop_codec::encode_props(&[(5u16, Primitive::Bool(true))].into());
        let mut v = Vertex::from_raw(1, 1, blob.into_boxed_slice());
        assert!(matches!(v.props, PropertyMap::Blob(_)));
        v.props_mut().insert(6, Primitive::Int32(99));
        assert!(matches!(v.props, PropertyMap::Map(_)));
        assert_eq!(v.props_mut().get(&5), Some(&Primitive::Bool(true))); // original prop preserved
        assert_eq!(v.props_mut().get(&6), Some(&Primitive::Int32(99))); // new prop present
    }

    #[test]
    #[should_panic(expected = "props_mut called on LabelOnly vertex")]
    fn g12_props_mut_on_label_only_panics() {
        let mut v = Vertex::label_only(1, 1);
        let _ = v.props_mut(); // must panic
    }

    #[test]
    fn g14_props_on_blob_transitions_to_map() {
        // props() triggers Blob → Map; as_map() returns Some afterward.
        let blob = crate::types::prop_codec::encode_props(&[(10u16, Primitive::Int64(7))].into());
        let mut v = Vertex::from_raw(1, 1, blob.into_boxed_slice());
        assert!(matches!(v.props, PropertyMap::Blob(_)));
        let map = v.props();
        assert_eq!(map.get(&10), Some(&Primitive::Int64(7)));
        // State must now be Map.
        assert!(matches!(v.props, PropertyMap::Map(_)));
        // as_map() must return Some.
        assert!(v.props.as_map().is_some());
    }
}

// ── Property ─────────────────────────────────────────────────────────────────

/// A single property value together with its owning element.
///
/// Used at the [`GraphCtx`] API boundary (`get_property`, `set_property`,
/// `drop_property`, `ValuesStep` with `emit_property=true`). Not stored in the
/// overlay — synthesized on demand from the `HashMap<u16, Primitive>` in Map state.
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
