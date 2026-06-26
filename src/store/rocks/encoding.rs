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
//! # Column families
//!
//! | CF name         | Key layout                               | Value layout                        |
//! |-----------------|------------------------------------------|-------------------------------------|
//! | `vertices`      | `[ VertexId:i64 ]`                       | `[ label_id:LabelId | props ]`         |
//! | `vertex_degree` | `[ VertexId:i64 ]`                       | `[ vertex_label_id:LabelId | out_e_cnt:u32 | in_e_cnt:u32 ]` |
//! | `edges_out`     | `[ SrcId:i64 | LabelId:i32 | DstId:i64 | Rank:u16 ]` | `[ end_vertex_label:LabelId | props ]` |
//! | `edges_in`      | `[ DstId:i64 | LabelId:i32 | SrcId:i64 | Rank:u16 ]` | `[ end_vertex_label:LabelId | props ]` |
//! | `schema`        | `[ kind:u8 | name ]` (or the 1-byte meta key) | kind-dependent, see below      |
//!
//! Edge properties are duplicated across `edges_out` and `edges_in`.
//! The direction byte present in previous versions has been removed; each CF
//! encodes direction implicitly via its key layout.
//!
//! ## `schema` CF entries
//!
//! Keyed by a `kind` discriminant byte ([`SCHEMA_KIND_VERTEX_LABEL`], [`SCHEMA_KIND_EDGE_LABEL`],
//! [`SCHEMA_KIND_PROP_KEY`]) followed by the label/key name, or by the single-byte
//! [`SCHEMA_META_KEY`] ([`SCHEMA_KIND_META`]) for the one graph-wide metadata entry:
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

use crate::types::{
    CanonicalKey, Direction, Edge, EdgeKey, LabelId, Primitive, Property, Rank, StoreError, Vertex, VertexKey,
    SCAN_PREFIX_INLINE,
};

// ── Scan helpers ──────────────────────────────────────────────────────────────

pub(crate) const EDGE_PREFIX_LENGTH: usize = 8;

/// Builds the prefix for an edge Column Family (CF) scan.
/// `vertex_id` (8 B), optionally followed by `label_id` (4 B).
///
/// Returns an inline [`SmallVec`] — no heap allocation for the typical
/// 8–10 byte prefix. The caller can pass the result directly to
/// [`prefix_upper_bound`] or clone it into a `Vec<u8>` for RocksDB keys.
pub fn edge_scan_prefix(vertex: VertexKey, label: Option<LabelId>) -> SmallVec<[u8; SCAN_PREFIX_INLINE]> {
    let mut prefix = SmallVec::<[u8; SCAN_PREFIX_INLINE]>::new();
    prefix.extend_from_slice(&(vertex ^ (1 << 63)).to_be_bytes());
    if let Some(lbl) = label {
        prefix.extend_from_slice(&lbl.to_be_bytes());
    }
    prefix
}

/// Computes the exclusive upper-bound for a prefix scan.
///
/// Copies `prefix` into an inline [`SmallVec`], then increments the last
/// non-`0xFF` byte. Returns `None` when every byte is `0xFF` (meaning a
/// scan-to-end should be used instead).
pub fn prefix_upper_bound(prefix: &[u8]) -> Option<SmallVec<[u8; SCAN_PREFIX_INLINE]>> {
    let mut upper = SmallVec::<[u8; SCAN_PREFIX_INLINE]>::new();
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
pub fn encode_schema_meta(version: u64, edge_mode: u8, schema_mode: u8) -> [u8; 10] {
    let mut bytes = [0u8; 10];
    bytes[0..8].copy_from_slice(&version.to_be_bytes());
    bytes[8] = edge_mode;
    bytes[9] = schema_mode;
    bytes
}

#[inline]
pub fn decode_schema_meta(bytes: &[u8]) -> Option<(u64, u8, u8)> {
    if bytes.len() < 10 {
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
pub fn encode_schema_prop_value(id: u16, data_type: u8) -> [u8; 3] {
    let mut bytes = [0u8; 3];
    bytes[0..2].copy_from_slice(&id.to_be_bytes());
    bytes[2] = data_type;
    bytes
}

#[inline]
pub fn decode_schema_prop_value(bytes: &[u8]) -> Option<(u16, u8)> {
    if bytes.len() < 3 {
        return None;
    }
    let id = u16::from_be_bytes(bytes[0..2].try_into().ok()?);
    let data_type = bytes[2];
    Some((id, data_type))
}

// ── Size constants ────────────────────────────────────────────────────────────

pub const VERTEX_KEY_SIZE: usize = 8;
/// Edge key: 8 (vertex) + 4 (label) + 8 (vertex) + 2 (rank) = 22 bytes.
/// No direction byte is included; each Column Family (CF) encodes direction implicitly.
pub const EDGE_KEY_SIZE: usize = 22;

// ── VertexKey encoding ────────────────────────────────────────────────────────

/// Encodes a `VertexKey` (i64) into an 8-byte big-endian array.
/// The `^ (1 << 63)` operation is used to ensure lexicographical ordering matches numerical order for signed integers.
#[inline]
pub fn encode_vertex_key(key: VertexKey) -> [u8; VERTEX_KEY_SIZE] {
    (key ^ (1 << 63)).to_be_bytes()
}

#[inline]
pub fn decode_vertex_key(bytes: &[u8]) -> Option<VertexKey> {
    Some(i64::from_be_bytes(bytes.try_into().ok()?) ^ (1 << 63))
}

// ── Edge key encoding ─────────────────────────────────────────────────────────
//
// edges_out layout:  [ SrcId:i64 | LabelId:u16 | DstId:i64 | Rank:u16 ]
// edges_in  layout:  [ DstId:i64 | LabelId:u16 | SrcId:i64 | Rank:u16 ]
//
// Both are encoded with the same physical byte format; only the semantic
// meaning of the first and last i64 differs by CF.
/// Encode a `EdgeKey`
#[inline]
pub fn encode_edge_key(k: &EdgeKey) -> [u8; EDGE_KEY_SIZE] {
    let mut buf = [0u8; EDGE_KEY_SIZE];
    buf[0..8].copy_from_slice(&(k.primary_id ^ (1 << 63)).to_be_bytes());
    buf[8..12].copy_from_slice(&k.label_id.to_be_bytes());
    buf[12..20].copy_from_slice(&(k.secondary_id ^ (1 << 63)).to_be_bytes());
    buf[20..22].copy_from_slice(&k.rank.to_be_bytes());
    buf
}

/// Decodes a byte slice into an `EdgeKey`.
#[inline]
pub fn decode_edge_key(bytes: &[u8], dir: Direction) -> Option<EdgeKey> {
    if bytes.len() < EDGE_KEY_SIZE {
        return None;
    }
    Some(EdgeKey {
        primary_id: i64::from_be_bytes(bytes[0..8].try_into().ok()?) ^ (1 << 63),
        direction: dir,
        label_id: LabelId::from_be_bytes(bytes[8..12].try_into().ok()?),
        secondary_id: i64::from_be_bytes(bytes[12..20].try_into().ok()?) ^ (1 << 63),
        rank: u16::from_be_bytes(bytes[20..22].try_into().ok()?) as Rank,
    })
}
// ── VertexValue ───────────────────────────────────────────────────────────────

/// `[ label_id:LabelId | property_blob ]` — value in the `vertices` CF.
/// The `label_id` is a numeric identifier for the vertex's label.
/// The label string itself is NOT stored here; `label_id` is resolved to a string
/// via the process-wide `Schema` when needed.
#[derive(Debug, Clone)]
pub struct VertexValue {
    pub label_id: LabelId,
    pub property_blob: Vec<u8>,
}

impl VertexValue {
    /// Encodes the `VertexValue` into a byte vector.
    #[inline]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + self.property_blob.len());
        buf.extend_from_slice(&self.label_id.to_be_bytes());
        buf.extend_from_slice(&self.property_blob);
        buf
    }

    /// Decodes a byte slice into a `VertexValue`.
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

/// `[ vertex_label_id:LabelId | out_e_cnt:u32 | in_e_cnt:u32 ]` — value in the `vertex_degree` CF.
///
/// Stores the vertex label together with out-degree and in-degree for a vertex.
/// The label is written once at `add_vertex` time and never updated again (no
/// "change vertex label" operation exists).  Both counters track incident edges
/// to enforce the invariant that a vertex cannot be dropped while it still has
/// incident edges.
#[derive(Debug, Clone)]
pub struct VertexDegree {
    pub vertex_label_id: LabelId,
    pub out_e_cnt: u32,
    pub in_e_cnt: u32,
}

impl VertexDegree {
    /// Encodes the `VertexDegree` into a 12-byte array.
    pub fn encode(&self) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&self.vertex_label_id.to_be_bytes());
        buf[4..8].copy_from_slice(&self.out_e_cnt.to_be_bytes());
        buf[8..12].copy_from_slice(&self.in_e_cnt.to_be_bytes());
        buf
    }

    /// Decodes a byte slice into a `VertexDegree`.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 12 {
            return None;
        }
        let vertex_label_id = LabelId::from_be_bytes(bytes[0..4].try_into().ok()?);
        let out_e_cnt = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
        let in_e_cnt = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
        Some(Self { vertex_label_id, out_e_cnt, in_e_cnt })
    }
}

// ── EdgeValue ─────────────────────────────────────────────────────────────────

/// `[ end_vertex_label:LabelId | property_blob ]` — value in both `edges_out` and `edges_in` CFs.
///
/// The 4-byte `end_vertex_label` prefix stores the label of the *other* vertex for
/// free at edge-scan time. For an `edges_out` row this is `dst_label`; for an
/// `edges_in` row this is `src_label`. The direction-aware builders
/// (`build_lazy_edge` / `build_full_edge`) attribute it correctly.
#[derive(Debug, Clone)]
pub struct EdgeValue {
    pub end_vertex_label: LabelId,
    pub property_blob: Vec<u8>,
}

impl EdgeValue {
    /// Encodes the `EdgeValue` into a byte vector, prefixing the
    /// `end_vertex_label` as a 4-byte big-endian `i32`.
    #[inline]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + self.property_blob.len());
        buf.extend_from_slice(&self.end_vertex_label.to_be_bytes());
        buf.extend_from_slice(&self.property_blob);
        buf
    }

    /// Decodes a byte slice into an `EdgeValue`.
    /// Returns `None` if the slice is fewer than 2 bytes.
    #[inline]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        let end_vertex_label = LabelId::from_be_bytes(bytes[0..4].try_into().ok()?);
        Some(Self { end_vertex_label, property_blob: bytes[4..].to_vec() })
    }
}

// ── Property codec ────────────────────────────────────────────────────────────

/// Wire-format type tags for `Primitive`, written as the byte preceding each
/// property's value in `encode_props`/`decode_props`. Stable on disk — do not
/// renumber existing tags; append new ones instead.
const TAG_BOOL: u8 = 0;
const TAG_INT32: u8 = 1;
const TAG_INT64: u8 = 2;
const TAG_FLOAT32: u8 = 3;
const TAG_FLOAT64: u8 = 4;
const TAG_STRING: u8 = 5;
const TAG_UUID: u8 = 6;
const TAG_NULL: u8 = 7;
const TAG_UINT16: u8 = 8;

/// Serializes a property list to a binary format.
pub(super) fn encode_props(props: &[Property]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(props.len() as u16).to_be_bytes());
    for prop in props {
        buf.extend_from_slice(&prop.key.to_be_bytes());
        match &prop.value {
            Primitive::Bool(b) => {
                buf.push(TAG_BOOL);
                buf.push(*b as u8);
            }
            Primitive::Int32(n) => {
                buf.push(TAG_INT32);
                buf.extend_from_slice(&n.to_be_bytes());
            }
            Primitive::Int64(n) => {
                buf.push(TAG_INT64);
                buf.extend_from_slice(&n.to_be_bytes());
            }
            Primitive::UInt16(n) => {
                buf.push(TAG_UINT16);
                buf.extend_from_slice(&n.to_be_bytes());
            }
            Primitive::Float32(f) => {
                buf.push(TAG_FLOAT32);
                buf.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            Primitive::Float64(f) => {
                buf.push(TAG_FLOAT64);
                buf.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            Primitive::String(s) => {
                buf.push(TAG_STRING);
                let sb = s.as_bytes();
                buf.extend_from_slice(&(sb.len() as u16).to_be_bytes());
                buf.extend_from_slice(sb);
            }
            Primitive::Uuid(u) => {
                buf.push(TAG_UUID);
                buf.extend_from_slice(&u.to_be_bytes());
            }
            Primitive::Null => {
                buf.push(TAG_NULL);
            }
        }
    }
    buf
}

// ── Element builders ──────────────────────────────────────────────────────────

/// Eagerly decode a `VertexValue` from storage into a fully-materialized `Vertex`.
///
/// Used by the admin / test path (`RocksStorage::get_vertex`) where the caller
/// accesses `props` directly.  Returns an error on a corrupt property blob.
pub(super) fn build_full_vertex(id: VertexKey, vv: &VertexValue) -> Result<Vertex, StoreError> {
    let owner = CanonicalKey::Vertex(id);
    let props = decode_props(&vv.property_blob, owner).ok_or(StoreError::CorruptData("vertex property blob"))?;
    Ok(Vertex::with_props(id, vv.label_id, props))
}

/// Eagerly decode an `EdgeValue` from storage into a fully-materialized `Edge`.
///
/// Used by the admin / test path.  Returns an error on a corrupt property blob.
/// The `ek.direction` determines whether the `EdgeValue`'s `end_vertex_label`
/// prefix belongs to `dst_label` (OUT) or `src_label` (IN).
pub(super) fn build_full_edge(ek: &EdgeKey, ev: &EdgeValue) -> Result<Edge, StoreError> {
    let cek = ek.canonical_edge_key();
    let owner = CanonicalKey::Edge(cek);
    let props = decode_props(&ev.property_blob, owner).ok_or(StoreError::CorruptData("edge property blob"))?;
    let (src_label, dst_label) = match ek.direction {
        Direction::OUT => (None, Some(ev.end_vertex_label)),
        Direction::IN => (Some(ev.end_vertex_label), None),
    };
    Ok(Edge::with_props(cek.src_id, cek.label_id, cek.dst_id, cek.rank, props, src_label, dst_label))
}

/// Build a lazy `Vertex` from storage bytes — properties are not decoded yet.
///
/// Used by `GraphTransaction::get_vertex` and `GraphSnapshot::get_vertex`.
/// The [`LogicalGraph`](crate::graph::LogicalGraph) overlay automatically decodes
/// properties via `all_props()` or `props_mut()` on access.
#[inline]
pub(super) fn build_lazy_vertex(id: VertexKey, vv: &VertexValue) -> Vertex {
    Vertex::from_raw(id, vv.label_id, vv.property_blob.clone().into_boxed_slice(), decode_props)
}

/// Build a lazy `Edge` from storage bytes — properties are not decoded yet.
///
/// Used by `GraphTransaction::get_edge` / `get_edges` and the snapshot equivalents.
/// The `ek.direction` determines whether `EdgeValue::end_vertex_label` becomes
/// `dst_label` (OUT) or `src_label` (IN).
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
        decode_props,
        src_label,
        dst_label,
    )
}

pub(crate) fn decode_props(blob: &[u8], owner: CanonicalKey) -> Option<Vec<Property>> {
    if blob.len() < 2 {
        return None;
    }
    let count = u16::from_be_bytes(blob[0..2].try_into().ok()?) as usize;
    let mut pos = 2;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 2 > blob.len() {
            return None;
        }
        let key = u16::from_be_bytes(blob[pos..pos + 2].try_into().ok()?);
        pos += 2;
        if pos >= blob.len() {
            return None;
        }
        let tag = blob[pos];
        pos += 1;
        let val = match tag {
            TAG_BOOL => {
                if pos >= blob.len() {
                    return None;
                }
                let b = blob[pos] != 0;
                pos += 1;
                Primitive::Bool(b)
            }
            TAG_INT32 => {
                if pos + 4 > blob.len() {
                    return None;
                }
                let n = i32::from_be_bytes(blob[pos..pos + 4].try_into().ok()?);
                pos += 4;
                Primitive::Int32(n)
            }
            TAG_INT64 => {
                if pos + 8 > blob.len() {
                    return None;
                }
                let n = i64::from_be_bytes(blob[pos..pos + 8].try_into().ok()?);
                pos += 8;
                Primitive::Int64(n)
            }
            TAG_UINT16 => {
                if pos + 2 > blob.len() {
                    return None;
                }
                let n = u16::from_be_bytes(blob[pos..pos + 2].try_into().ok()?);
                pos += 2;
                Primitive::UInt16(n)
            }
            TAG_FLOAT32 => {
                if pos + 4 > blob.len() {
                    return None;
                }
                let bits = u32::from_be_bytes(blob[pos..pos + 4].try_into().ok()?);
                pos += 4;
                Primitive::Float32(f32::from_bits(bits))
            }
            TAG_FLOAT64 => {
                if pos + 8 > blob.len() {
                    return None;
                }
                let bits = u64::from_be_bytes(blob[pos..pos + 8].try_into().ok()?);
                pos += 8;
                Primitive::Float64(f64::from_bits(bits))
            }
            TAG_STRING => {
                if pos + 2 > blob.len() {
                    return None;
                }
                let slen = u16::from_be_bytes(blob[pos..pos + 2].try_into().ok()?) as usize;
                pos += 2;
                if pos + slen > blob.len() {
                    return None;
                }
                let s = std::str::from_utf8(&blob[pos..pos + slen]).ok()?;
                pos += slen;
                Primitive::String(smol_str::SmolStr::new(s))
            }
            TAG_UUID => {
                if pos + 16 > blob.len() {
                    return None;
                }
                let u = u128::from_be_bytes(blob[pos..pos + 16].try_into().ok()?);
                pos += 16;
                Primitive::Uuid(u)
            }
            TAG_NULL => Primitive::Null,
            _ => return None,
        };
        out.push(Property { owner, key, value: val });
    }
    Some(out)
}
// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::types::keys::LabelId;
    use smol_str::SmolStr;

    use super::{decode_vertex_key, encode_vertex_key, EdgeValue, VertexDegree, VertexValue};
    use crate::types::{
        element::{Edge, Vertex},
        CanonicalEdgeKey, CanonicalKey, Primitive, Property,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn encode_props(props: &[(u16, Primitive)]) -> Vec<u8> {
        use super::{
            TAG_BOOL, TAG_FLOAT32, TAG_FLOAT64, TAG_INT32, TAG_INT64, TAG_NULL, TAG_STRING, TAG_UINT16, TAG_UUID,
        };
        let mut buf = Vec::new();
        buf.extend_from_slice(&(props.len() as u16).to_be_bytes());
        for (key, val) in props {
            buf.extend_from_slice(&key.to_be_bytes());
            match val {
                Primitive::Bool(b) => {
                    buf.push(TAG_BOOL);
                    buf.push(*b as u8);
                }
                Primitive::Int32(n) => {
                    buf.push(TAG_INT32);
                    buf.extend_from_slice(&n.to_be_bytes());
                }
                Primitive::Int64(n) => {
                    buf.push(TAG_INT64);
                    buf.extend_from_slice(&n.to_be_bytes());
                }
                Primitive::UInt16(n) => {
                    buf.push(TAG_UINT16);
                    buf.extend_from_slice(&n.to_be_bytes());
                }
                Primitive::Float32(f) => {
                    buf.push(TAG_FLOAT32);
                    buf.extend_from_slice(&f.to_bits().to_be_bytes());
                }
                Primitive::Float64(f) => {
                    buf.push(TAG_FLOAT64);
                    buf.extend_from_slice(&f.to_bits().to_be_bytes());
                }
                Primitive::String(s) => {
                    buf.push(TAG_STRING);
                    let sb = s.as_bytes();
                    buf.extend_from_slice(&(sb.len() as u16).to_be_bytes());
                    buf.extend_from_slice(sb);
                }
                Primitive::Uuid(u) => {
                    buf.push(TAG_UUID);
                    buf.extend_from_slice(&u.to_be_bytes());
                }
                Primitive::Null => {
                    buf.push(TAG_NULL);
                }
            }
        }
        buf
    }

    /// Decodes a property blob into a vector of (u16, Primitive) tuples.
    fn decode_props(blob: &[u8]) -> Vec<(u16, Primitive)> {
        use super::{
            TAG_BOOL, TAG_FLOAT32, TAG_FLOAT64, TAG_INT32, TAG_INT64, TAG_NULL, TAG_STRING, TAG_UINT16, TAG_UUID,
        };
        let mut pos = 0;
        let count = u16::from_be_bytes(blob[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let key = u16::from_be_bytes(blob[pos..pos + 2].try_into().unwrap());
            pos += 2;
            let tag = blob[pos];
            pos += 1;
            let val = match tag {
                TAG_BOOL => {
                    let b = blob[pos] != 0;
                    pos += 1;
                    Primitive::Bool(b)
                }
                TAG_INT32 => {
                    let n = i32::from_be_bytes(blob[pos..pos + 4].try_into().unwrap());
                    pos += 4;
                    Primitive::Int32(n)
                }
                TAG_INT64 => {
                    let n = i64::from_be_bytes(blob[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                    Primitive::Int64(n)
                }
                TAG_UINT16 => {
                    let n = u16::from_be_bytes(blob[pos..pos + 2].try_into().unwrap());
                    pos += 2;
                    Primitive::UInt16(n)
                }
                TAG_FLOAT32 => {
                    let bits = u32::from_be_bytes(blob[pos..pos + 4].try_into().unwrap());
                    pos += 4;
                    Primitive::Float32(f32::from_bits(bits))
                }
                TAG_FLOAT64 => {
                    let bits = u64::from_be_bytes(blob[pos..pos + 8].try_into().unwrap());
                    pos += 8;
                    Primitive::Float64(f64::from_bits(bits))
                }
                TAG_STRING => {
                    let slen = u16::from_be_bytes(blob[pos..pos + 2].try_into().unwrap()) as usize;
                    pos += 2;
                    let s = std::str::from_utf8(&blob[pos..pos + slen]).unwrap();
                    pos += slen;
                    Primitive::String(SmolStr::new(s))
                }
                TAG_UUID => {
                    let u = u128::from_be_bytes(blob[pos..pos + 16].try_into().unwrap());
                    pos += 16;
                    Primitive::Uuid(u)
                }
                TAG_NULL => Primitive::Null,
                t => panic!("unknown prop tag {t}"),
            };
            out.push((key, val));
        }
        out
    }

    /// Helper to create a `Vertex` for testing.
    fn make_vertex(id: i64, label_id: LabelId, raw: &[(u16, Primitive)]) -> Vertex {
        let owner = CanonicalKey::Vertex(id);
        let props = raw.iter().map(|(k, v)| Property { owner, key: *k, value: v.clone() }).collect();
        Vertex::with_props(id, label_id, props)
    }

    /// Helper to create an `Edge` for testing.
    fn make_edge(cek: CanonicalEdgeKey, raw: &[(u16, Primitive)]) -> Edge {
        let owner = CanonicalKey::Edge(cek);
        let props = raw.iter().map(|(k, v)| Property { owner, key: *k, value: v.clone() }).collect();
        Edge::with_props(cek.src_id, cek.label_id, cek.dst_id, cek.rank, props, None, None)
    }

    /// Encodes an edge key in the OUT direction.
    fn encode_edge_key_out(cek: CanonicalEdgeKey) -> [u8; super::EDGE_KEY_SIZE] {
        super::encode_edge_key(&cek.out_key())
    }

    /// Decodes an edge key in the OUT direction.
    fn decode_edge_key_out(bytes: &[u8]) -> Option<CanonicalEdgeKey> {
        Some(super::decode_edge_key(bytes, crate::types::Direction::OUT)?.canonical_edge_key())
    }

    /// Encodes an edge key in the IN direction.
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

    // ── EdgeKey (22 bytes, no direction byte) ─────────────────────────────────

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
        assert_eq!(in_bytes.len(), 22);
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
        let raw = vec![(1u16, Primitive::String(SmolStr::new("Alice"))), (2u16, Primitive::Int32(30))];
        let vv = VertexValue { label_id: 7, property_blob: encode_props(&raw) };
        let bytes = vv.encode();
        let dec = VertexValue::decode(&bytes).unwrap();
        assert_eq!(dec.label_id, 7);
        let props = decode_props(&dec.property_blob);
        assert_eq!(props.len(), 2);
        assert_eq!(props[0].0, 1u16);
        assert_eq!(props[0].1, Primitive::String(SmolStr::new("Alice")));
        assert_eq!(props[1].1, Primitive::Int32(30));
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
        let raw = vec![(1u16, Primitive::String(SmolStr::new("Bob"))), (2u16, Primitive::Float64(9.9))];
        let key_bytes = encode_vertex_key(42);
        let val_bytes = VertexValue { label_id: 1, property_blob: encode_props(&raw) }.encode();
        let id = decode_vertex_key(&key_bytes).unwrap();
        let vv = VertexValue::decode(&val_bytes).unwrap();
        assert_eq!(id, 42);
        assert_eq!(vv.label_id, 1);
        let dec_props = decode_props(&vv.property_blob);
        let mut fv = make_vertex(id, vv.label_id, &dec_props);
        assert_eq!(fv.id, 42);
        assert_eq!(fv.label_id, 1);
        assert_eq!(fv.all_props().len(), 2);
        assert_eq!(fv.all_props()[0].key, 1u16);
        assert_eq!(fv.all_props()[0].owner, CanonicalKey::Vertex(42));
    }

    #[test]
    fn full_edge_roundtrip() {
        let cek = CanonicalEdgeKey { src_id: 10, label_id: 7, rank: 0, dst_id: 20 };
        let raw =
            vec![(1u16, Primitive::Float64(std::f64::consts::PI)), (2u16, Primitive::String(SmolStr::new("friend")))];
        let key_bytes = encode_edge_key_out(cek);
        let val_bytes = EdgeValue { end_vertex_label: 7, property_blob: encode_props(&raw) }.encode();
        let dec_cek = decode_edge_key_out(&key_bytes).unwrap();
        let ev = EdgeValue::decode(&val_bytes).unwrap();
        assert_eq!(dec_cek, cek);
        let dec_props = decode_props(&ev.property_blob);
        let mut fe = make_edge(dec_cek, &dec_props);
        assert_eq!(fe.src_id, 10);
        assert_eq!(fe.dst_id, 20);
        assert_eq!(fe.label_id, 7);
        assert_eq!(fe.all_props()[0].owner, CanonicalKey::Edge(cek));
        assert_eq!(fe.all_props()[1].value, Primitive::String(SmolStr::new("friend")));
    }

    #[test]
    fn all_primitive_types_roundtrip() {
        let raw: Vec<(u16, Primitive)> = vec![
            (1, Primitive::Bool(true)),
            (2, Primitive::Int32(-100)),
            (3, Primitive::Int64(i64::MAX)),
            (4, Primitive::Float32(f32::MIN_POSITIVE)),
            (5, Primitive::Float64(f64::MIN_POSITIVE)),
            (6, Primitive::String(SmolStr::new("hello"))),
            (7, Primitive::Uuid(u128::MAX)),
            (8, Primitive::Null),
            (9, Primitive::UInt16(u16::MAX)),
        ];
        let blob = encode_props(&raw);
        let dec = decode_props(&blob);
        assert_eq!(dec.len(), 9);
        for (i, (k, v)) in dec.iter().enumerate() {
            assert_eq!(k, &raw[i].0);
            assert_eq!(v, &raw[i].1);
        }
    }

    #[test]
    fn property_owner_is_canonical_key() {
        let cek = CanonicalEdgeKey { src_id: 5, label_id: 1, rank: 0, dst_id: 6 };
        let mut fe = make_edge(cek, &[(1u16, Primitive::Float32(0.5))]);
        assert_eq!(fe.all_props()[0].owner, CanonicalKey::Edge(cek));

        let mut fv = make_vertex(99, 2, &[(2u16, Primitive::Int32(7))]);
        assert_eq!(fv.all_props()[0].owner, CanonicalKey::Vertex(99));
    }
}
