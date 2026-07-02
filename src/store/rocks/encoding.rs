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

//! On-disk byte layout for RocksDB keys and values.
//!
//! All multi-byte integers are big-endian so that lexicographic byte order
//! matches numeric order, enabling efficient range scans.
//!
//! # Responsibilities
//!
//! This module owns the **structural** encoding of RocksDB rows: how vertex/edge
//! keys are laid out, the `schema` CF key/value format, and the fixed-prefix
//! framing of `VertexValue` / `EdgeValue` / `VertexDegree` (the 4-byte label
//! prefix followed by the trailing `prop_blob`).
//!
//! The `prop_blob` codec — tag constants, `encode_props`, `decode_prop_by_key`,
//! `decode_all_to_map` — lives in [`crate::types::prop_codec`], which is the
//! single source of truth for the property binary format.
//!
//! # Column families
//!
//! | CF name         | Key layout                               | Value layout                        |
//! |-----------------|------------------------------------------|-------------------------------------|
//! | `vertices`      | `[ VertexId:i64 ]`                       | `[ label_id:LabelId | prop_blob ]`  |
//! | `vertex_degree` | `[ VertexId:i64 ]`                       | `[ vertex_label_id:LabelId | out_e_cnt:u32 | in_e_cnt:u32 ]` |
//! | `edges_out`     | `[ SrcId:i64 | LabelId:i32 | DstId:i64 | Rank:u16 ]` | `[ end_vertex_label:LabelId | prop_blob ]` |
//! | `edges_in`      | `[ DstId:i64 | LabelId:i32 | SrcId:i64 | Rank:u16 ]` | `[ end_vertex_label:LabelId | prop_blob ]` |
//! | `schema`        | `[ kind:u8 | name ]` (or the 1-byte meta key) | kind-dependent, see below      |
//!
//! Edge properties are duplicated across `edges_out` and `edges_in`.
//!
//! ## `schema` CF entries
//!
//! | Kind                  | Value layout                                  |
//! |------------------------|------------------------------------------------|
//! | vertex/edge label      | `[ id:LabelId ]`                                   |
//! | property key            | `[ id:u16 \| data_type:u8 ]` |
//! | meta (singleton)        | `[ version:u64 \| edge_mode:u8 \| schema_mode:u8 ]` |
//!
//! # Prefix scan lengths
//!
//! | Prefix          | Bytes | Enables                              |
//! |-----------------|-------|--------------------------------------|
//! | `vertex_id`     | 8     | all incident edges (`bothE`)          |
//! | `vertex_id | label_id` | 12 | `outE(label)` / `inE(label)`    |

use smallvec::SmallVec;

use crate::types::{Direction, Edge, EdgeKey, LabelId, Rank, Vertex, VertexKey};

// ── Scan helpers ──────────────────────────────────────────────────────────────

pub(crate) const EDGE_PREFIX_LENGTH: usize = 8;

pub fn edge_scan_prefix(vertex: VertexKey, label: Option<LabelId>) -> SmallVec<[u8; SCAN_PREFIX_LENGTH]> {
    let mut prefix = SmallVec::<[u8; SCAN_PREFIX_LENGTH]>::new();
    prefix.extend_from_slice(&flip_sign_bit(vertex).to_be_bytes());
    if let Some(lbl) = label {
        prefix.extend_from_slice(&lbl.to_be_bytes());
    }
    prefix
}

pub fn prefix_upper_bound(prefix: &[u8]) -> Option<SmallVec<[u8; SCAN_PREFIX_LENGTH]>> {
    let mut upper = SmallVec::<[u8; SCAN_PREFIX_LENGTH]>::new();
    upper.extend_from_slice(prefix);
    for byte in upper.iter_mut().rev() {
        if *byte < 0xFF {
            *byte += 1;
            return Some(upper);
        }
        *byte = 0x00;
    }
    None
}

// ── Column-family name constants ──────────────────────────────────────────────

pub const CF_VERTICES: &str = "vertices";
pub const CF_VERTEX_DEGREE: &str = "vertex_degree";
pub const CF_EDGES_OUT: &str = "edges_out";
pub const CF_EDGES_IN: &str = "edges_in";
pub const CF_SCHEMA: &str = "schema";

// ── Schema kind discriminants & encoders ──────────────────────────────────────

pub const SCHEMA_KIND_VERTEX_LABEL: u8 = 0;
pub const SCHEMA_KIND_EDGE_LABEL: u8 = 1;
pub const SCHEMA_KIND_PROP_KEY: u8 = 2;
pub const SCHEMA_KIND_META: u8 = 3;
pub const SCHEMA_META_KEY: [u8; 1] = [SCHEMA_KIND_META];
pub const SCHEMA_META_NAME: &str = "";

#[inline]
pub fn encode_schema_key(kind: u8, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + name.len());
    key.push(kind);
    key.extend_from_slice(name.as_bytes());
    key
}

#[inline]
pub fn encode_schema_meta(version: u64, edge_mode: u8, schema_mode: u8) -> [u8; SCHEMA_META_SIZE] {
    let mut bytes = [0u8; SCHEMA_META_SIZE];
    bytes[0..8].copy_from_slice(&version.to_be_bytes());
    bytes[8] = edge_mode;
    bytes[9] = schema_mode;
    bytes
}

#[inline]
pub fn decode_schema_meta(bytes: &[u8]) -> Option<(u64, u8, u8)> {
    if bytes.len() < SCHEMA_META_SIZE {
        return None;
    }
    let version = u64::from_be_bytes(bytes[0..8].try_into().ok()?);
    let edge_mode = bytes[8];
    let schema_mode = bytes[9];
    Some((version, edge_mode, schema_mode))
}

#[inline]
pub fn encode_schema_label_value(id: LabelId) -> [u8; 4] {
    id.to_be_bytes()
}

#[inline]
pub fn decode_schema_label_value(bytes: &[u8]) -> Option<LabelId> {
    Some(LabelId::from_be_bytes(bytes.try_into().ok()?))
}

#[inline]
pub fn encode_schema_prop_value(id: u16, data_type: u8) -> [u8; SCHEMA_PROP_VALUE_SIZE] {
    let mut bytes = [0u8; SCHEMA_PROP_VALUE_SIZE];
    bytes[0..2].copy_from_slice(&id.to_be_bytes());
    bytes[2] = data_type;
    bytes
}

#[inline]
pub fn decode_schema_prop_value(bytes: &[u8]) -> Option<(u16, u8)> {
    if bytes.len() < SCHEMA_PROP_VALUE_SIZE {
        return None;
    }
    let id = u16::from_be_bytes(bytes[0..2].try_into().ok()?);
    let data_type = bytes[2];
    Some((id, data_type))
}

// ── Size constants ────────────────────────────────────────────────────────────

pub const VERTEX_KEY_SIZE: usize = 8;
pub const EDGE_KEY_SIZE: usize = 22;
const SCHEMA_META_SIZE: usize = 10;
const SCHEMA_PROP_VALUE_SIZE: usize = 3;
const VERTEX_DEGREE_SIZE: usize = 12;
const SCAN_PREFIX_LENGTH: usize = 8 + 4;

#[inline]
const fn flip_sign_bit(v: i64) -> i64 {
    v ^ (1i64 << 63)
}

// ── VertexKey encoding ────────────────────────────────────────────────────────

#[inline]
pub fn encode_vertex_key(key: VertexKey) -> [u8; VERTEX_KEY_SIZE] {
    flip_sign_bit(key).to_be_bytes()
}

#[inline]
pub fn decode_vertex_key(bytes: &[u8]) -> Option<VertexKey> {
    Some(flip_sign_bit(i64::from_be_bytes(bytes.try_into().ok()?)))
}

// ── Edge key encoding ─────────────────────────────────────────────────────────

#[inline]
pub fn encode_edge_key(k: &EdgeKey) -> [u8; EDGE_KEY_SIZE] {
    let mut buf = [0u8; EDGE_KEY_SIZE];
    buf[0..8].copy_from_slice(&flip_sign_bit(k.primary_id).to_be_bytes());
    buf[8..12].copy_from_slice(&k.label_id.to_be_bytes());
    buf[12..20].copy_from_slice(&flip_sign_bit(k.secondary_id).to_be_bytes());
    buf[20..22].copy_from_slice(&k.rank.to_be_bytes());
    buf
}

#[inline]
pub fn decode_edge_key(bytes: &[u8], dir: Direction) -> Option<EdgeKey> {
    if bytes.len() < EDGE_KEY_SIZE {
        return None;
    }
    Some(EdgeKey {
        primary_id: flip_sign_bit(i64::from_be_bytes(bytes[0..8].try_into().ok()?)),
        direction: dir,
        label_id: LabelId::from_be_bytes(bytes[8..12].try_into().ok()?),
        secondary_id: flip_sign_bit(i64::from_be_bytes(bytes[12..20].try_into().ok()?)),
        rank: u16::from_be_bytes(bytes[20..22].try_into().ok()?) as Rank,
    })
}

// ── VertexValue ───────────────────────────────────────────────────────────────

/// `[ label_id:LabelId | prop_blob ]` — value in the `vertices` CF.
#[derive(Debug, Clone)]
pub struct VertexValue {
    pub label_id: LabelId,
    pub property_blob: Vec<u8>,
}

impl VertexValue {
    #[inline]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + self.property_blob.len());
        buf.extend_from_slice(&self.label_id.to_be_bytes());
        buf.extend_from_slice(&self.property_blob);
        buf
    }

    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        let label_id = LabelId::from_be_bytes(bytes[0..4].try_into().ok()?);
        let property_blob = bytes[4..].to_vec();
        Some(Self { label_id, property_blob })
    }
}

// ── VertexDegree ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct VertexDegree {
    pub vertex_label_id: LabelId,
    pub out_e_cnt: u32,
    pub in_e_cnt: u32,
}

impl VertexDegree {
    #[inline]
    pub fn encode(&self) -> [u8; VERTEX_DEGREE_SIZE] {
        let mut buf = [0u8; VERTEX_DEGREE_SIZE];
        buf[0..4].copy_from_slice(&self.vertex_label_id.to_be_bytes());
        buf[4..8].copy_from_slice(&self.out_e_cnt.to_be_bytes());
        buf[8..12].copy_from_slice(&self.in_e_cnt.to_be_bytes());
        buf
    }

    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != VERTEX_DEGREE_SIZE {
            return None;
        }
        let vertex_label_id = LabelId::from_be_bytes(bytes[0..4].try_into().ok()?);
        let out_e_cnt = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
        let in_e_cnt = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
        Some(Self { vertex_label_id, out_e_cnt, in_e_cnt })
    }
}

// ── EdgeValue ─────────────────────────────────────────────────────────────────

/// `[ end_vertex_label:LabelId | prop_blob ]` — value in both `edges_out` and `edges_in` CFs.
#[derive(Debug, Clone)]
pub struct EdgeValue {
    pub end_vertex_label: LabelId,
    pub property_blob: Vec<u8>,
}

impl EdgeValue {
    #[inline]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + self.property_blob.len());
        buf.extend_from_slice(&self.end_vertex_label.to_be_bytes());
        buf.extend_from_slice(&self.property_blob);
        buf
    }

    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        let end_vertex_label = LabelId::from_be_bytes(bytes[0..4].try_into().ok()?);
        Some(Self { end_vertex_label, property_blob: bytes[4..].to_vec() })
    }
}

// ── Element builders ──────────────────────────────────────────────────────────

/// Build a lazy `Vertex` from store bytes (Blob state — no decoding yet).
#[inline]
pub(super) fn build_lazy_vertex(id: VertexKey, vv: &VertexValue) -> Vertex {
    Vertex::from_raw(id, vv.label_id, vv.property_blob.clone().into_boxed_slice())
}

/// Build a lazy `Edge` from store bytes (Blob state — no decoding yet).
#[inline]
pub(super) fn build_lazy_edge(ek: &EdgeKey, ev: &EdgeValue) -> Edge {
    let cek = ek.canonical_edge_key();
    let (src_label, dst_label) = match ek.direction {
        Direction::OUT => (None, Some(ev.end_vertex_label)),
        Direction::IN => (Some(ev.end_vertex_label), None),
    };
    Edge::from_raw(
        cek.src_id,
        cek.label_id,
        cek.dst_id,
        cek.rank,
        ev.property_blob.clone().into_boxed_slice(),
        src_label,
        dst_label,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::types::keys::LabelId;
    use smol_str::SmolStr;

    use super::{decode_vertex_key, encode_vertex_key, EdgeValue, VertexDegree, VertexValue};
    use crate::types::{
        element::{Edge, Vertex},
        CanonicalEdgeKey, CanonicalKey, Primitive,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn props_map(pairs: &[(u16, Primitive)]) -> HashMap<u16, Primitive> {
        pairs.iter().map(|(k, v)| (*k, v.clone())).collect()
    }

    fn make_vertex(id: i64, label_id: LabelId, raw: &[(u16, Primitive)]) -> Vertex {
        Vertex::with_props(id, label_id, props_map(raw))
    }

    fn make_edge(cek: CanonicalEdgeKey, raw: &[(u16, Primitive)]) -> Edge {
        Edge::with_props(cek.src_id, cek.label_id, cek.dst_id, cek.rank, props_map(raw), None, None)
    }

    fn encode_edge_key_out(cek: CanonicalEdgeKey) -> [u8; super::EDGE_KEY_SIZE] {
        super::encode_edge_key(&cek.out_key())
    }

    fn decode_edge_key_out(bytes: &[u8]) -> Option<CanonicalEdgeKey> {
        Some(super::decode_edge_key(bytes, crate::types::Direction::OUT)?.canonical_edge_key())
    }

    fn encode_edge_key_in(cek: CanonicalEdgeKey) -> [u8; super::EDGE_KEY_SIZE] {
        super::encode_edge_key(&cek.in_key())
    }

    fn decode_edge_key_in(bytes: &[u8]) -> Option<CanonicalEdgeKey> {
        Some(super::decode_edge_key(bytes, crate::types::Direction::IN)?.canonical_edge_key())
    }

    // ── VertexKey ─────────────────────────────────────────────────────────────

    #[test]
    fn vertex_key_encode_decode() {
        let id: i64 = 42;
        assert_eq!(decode_vertex_key(&encode_vertex_key(id)).unwrap(), id);
    }

    #[test]
    fn vertex_key_decode_bad_length() {
        assert!(decode_vertex_key(&[0u8; 4]).is_none());
        assert!(decode_vertex_key(&[]).is_none());
    }

    // ── EdgeKey ───────────────────────────────────────────────────────────────

    #[test]
    fn edge_key_out_encode_decode() {
        let k = CanonicalEdgeKey { src_id: 100, label_id: 3, rank: 0, dst_id: 200 };
        let encoded = encode_edge_key_out(k);
        assert_eq!(encoded.len(), 22);
        let decoded = decode_edge_key_out(&encoded).unwrap();
        assert_eq!(decoded, k);
    }

    #[test]
    fn edge_key_in_encode_decode() {
        let k = CanonicalEdgeKey { src_id: 100, label_id: 5, rank: 2, dst_id: 200 };
        let in_bytes = encode_edge_key_in(k);
        assert_eq!(in_bytes.len(), 22);
        assert_eq!(i64::from_be_bytes(in_bytes[0..8].try_into().unwrap()) ^ (1 << 63), 200i64);
        let decoded = decode_edge_key_in(&in_bytes).unwrap();
        assert_eq!(decoded, k);
    }

    #[test]
    fn edge_key_in_encode_decode_negative_dst() {
        let k = CanonicalEdgeKey { src_id: 100, label_id: 5, rank: 2, dst_id: -200 };
        let in_bytes = encode_edge_key_in(k);
        assert_eq!(i64::from_be_bytes(in_bytes[0..8].try_into().unwrap()) ^ (1 << 63), -200i64);
        let decoded = decode_edge_key_in(&in_bytes).unwrap();
        assert_eq!(decoded, k);
    }

    #[test]
    fn lexicographic_ordering_of_signed_keys() {
        let keys = vec![i64::MIN, -100, -1, 0, 1, 100, i64::MAX];
        let mut encoded: Vec<_> = keys.iter().copied().map(encode_vertex_key).collect();
        encoded.sort();
        let decoded: Vec<_> = encoded.iter().map(|b| decode_vertex_key(b).unwrap()).collect();
        assert_eq!(decoded, keys);
    }

    #[test]
    fn edge_key_out_in_roundtrip() {
        let k = CanonicalEdgeKey { src_id: 1, label_id: 7, rank: 3, dst_id: 99 };
        assert_eq!(decode_edge_key_out(&encode_edge_key_out(k)).unwrap(), k);
        assert_eq!(decode_edge_key_in(&encode_edge_key_in(k)).unwrap(), k);
    }

    // ── VertexValue ───────────────────────────────────────────────────────────

    #[test]
    fn vertex_value_encode_decode() {
        let raw = props_map(&[(1u16, Primitive::String(SmolStr::new("Alice"))), (2u16, Primitive::Int32(30))]);
        let vv = VertexValue { label_id: 7, property_blob: crate::types::prop_codec::encode_props(&raw) };
        let bytes = vv.encode();
        let dec = VertexValue::decode(&bytes).unwrap();
        assert_eq!(dec.label_id, 7);
        let map = crate::types::prop_codec::decode_all_to_map(&dec.property_blob);
        assert_eq!(map.get(&1), Some(&Primitive::String(SmolStr::new("Alice"))));
        assert_eq!(map.get(&2), Some(&Primitive::Int32(30)));
    }

    #[test]
    fn vertex_value_decode_bad_length() {
        assert!(VertexValue::decode(&[0u8; 1]).is_none());
        assert!(VertexValue::decode(&[]).is_none());
    }

    // ── VertexDegree ──────────────────────────────────────────────────────────

    #[test]
    fn vertex_degree_encode_decode() {
        let vd = VertexDegree { vertex_label_id: 7, out_e_cnt: 10, in_e_cnt: 20 };
        let bytes = vd.encode();
        assert_eq!(bytes.len(), 12);
        let dec = VertexDegree::decode(&bytes).unwrap();
        assert_eq!(dec.vertex_label_id, 7);
        assert_eq!(dec.out_e_cnt, 10);
        assert_eq!(dec.in_e_cnt, 20);
    }

    #[test]
    fn vertex_degree_decode_bad_length() {
        assert!(VertexDegree::decode(&[0u8; 7]).is_none());
        assert!(VertexDegree::decode(&[0u8; 9]).is_none());
        assert!(VertexDegree::decode(&[0u8; 11]).is_none());
    }

    // ── Full roundtrips ───────────────────────────────────────────────────────

    #[test]
    fn full_vertex_roundtrip() {
        let raw = props_map(&[(1u16, Primitive::String(SmolStr::new("Bob"))), (2u16, Primitive::Float64(9.9))]);
        let key_bytes = encode_vertex_key(42);
        let val_bytes =
            VertexValue { label_id: 1, property_blob: crate::types::prop_codec::encode_props(&raw) }.encode();
        let id = decode_vertex_key(&key_bytes).unwrap();
        let vv = VertexValue::decode(&val_bytes).unwrap();
        assert_eq!(id, 42);
        assert_eq!(vv.label_id, 1);
        let mut fv =
            make_vertex(id, vv.label_id, &[(1, Primitive::String(SmolStr::new("Bob"))), (2, Primitive::Float64(9.9))]);
        assert_eq!(fv.id, 42);
        assert_eq!(fv.label_id, 1);
        assert_eq!(fv.props().len(), 2);
        assert_eq!(fv.props().get(&1), Some(&Primitive::String(SmolStr::new("Bob"))));
        let owner_check = fv.get_property(1).unwrap();
        assert_eq!(owner_check.owner, CanonicalKey::Vertex(42));
    }

    #[test]
    fn full_edge_roundtrip() {
        let cek = CanonicalEdgeKey { src_id: 10, label_id: 7, rank: 0, dst_id: 20 };
        // Use non-reserved keys (>= 10); keys 1=ID, 2=LABEL, 3=RANK are reserved.
        let raw = props_map(&[
            (10u16, Primitive::Float64(std::f64::consts::PI)),
            (11u16, Primitive::String(SmolStr::new("friend"))),
        ]);
        let key_bytes = encode_edge_key_out(cek);
        let val_bytes =
            EdgeValue { end_vertex_label: 7, property_blob: crate::types::prop_codec::encode_props(&raw) }.encode();
        let dec_cek = decode_edge_key_out(&key_bytes).unwrap();
        let ev = EdgeValue::decode(&val_bytes).unwrap();
        assert_eq!(dec_cek, cek);
        let map = crate::types::prop_codec::decode_all_to_map(&ev.property_blob);
        let fe = make_edge(dec_cek, &[(10, map[&10].clone()), (11, map[&11].clone())]);
        assert_eq!(fe.src_id, 10);
        assert_eq!(fe.dst_id, 20);
        assert_eq!(fe.label_id, 7);
        let p10 = fe.get_property(10).unwrap();
        assert_eq!(p10.owner, CanonicalKey::Edge(cek));
        assert_eq!(fe.get_value(11), Some(Primitive::String(SmolStr::new("friend"))));
    }

    #[test]
    fn property_owner_is_canonical_key() {
        let cek = CanonicalEdgeKey { src_id: 5, label_id: 1, rank: 0, dst_id: 6 };
        let fe = make_edge(cek, &[(1u16, Primitive::Float32(0.5))]);
        assert_eq!(fe.get_property(1).unwrap().owner, CanonicalKey::Edge(cek));

        let fv = make_vertex(99, 2, &[(2u16, Primitive::Int32(7))]);
        assert_eq!(fv.get_property(2).unwrap().owner, CanonicalKey::Vertex(99));
    }

    #[test]
    fn blob_state_get_value() {
        // Verify Blob-state binary search works without triggering ensure_map
        let mut m = HashMap::new();
        m.insert(5u16, Primitive::Int64(999));
        m.insert(10u16, Primitive::Bool(false));
        let blob = crate::types::prop_codec::encode_props(&m);
        let v = Vertex::from_raw(1, 0, blob.into_boxed_slice());
        assert_eq!(v.get_value(5), Some(Primitive::Int64(999)));
        assert_eq!(v.get_value(10), Some(Primitive::Bool(false)));
        assert_eq!(v.get_value(99), None);
        // Still in Blob state (get_value doesn't mutate)
        assert!(matches!(v.props, crate::types::element::PropertyMap::Blob(_)));
    }
}
