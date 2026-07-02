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

//! Property blob (v1) format — canonical binary representation for element properties.
//!
//! The v1 format is the single, graph-engine-level encoding for property data.
//! It is not tied to RocksDB specifically; `store/rocks/encoding.rs` uses this
//! codec to write blobs and this module owns the decode side.
//!
//! ## Wire layout
//!
//! ```text
//! [ ver:4 | cnt:12 | (key:u16 | off:u16)×cnt | (tag:u8 | payload)×cnt ]
//!  ─── u16 BE ───   ─────── directory ───────   ──── value section ────
//! ```
//!
//! - `ver` (bits 15–12): `0x1` for this version
//! - `cnt` (bits 11–0): number of properties, up to 4095
//! - Directory: `(key, offset)` pairs sorted ascending by key — enables binary search
//! - `off`: byte offset from the start of the value section to the entry's `tag` byte
//! - Value section is written in the same key-sorted order as the directory
//!
//! ## Decode semantics
//!
//! `decode_all_to_map` is **best-effort**: malformed entries are silently skipped.
//! RocksDB block-level CRC checksums surface filesystem corruption before data
//! reaches these functions, so a partial decode result is unreachable in practice.

use std::cmp::Ordering;
use std::collections::HashMap;

use smallvec::SmallVec;

use crate::types::gvalue::Primitive;

// ── Format constants ──────────────────────────────────────────────────────────

/// v1 format version nibble stored in `header >> 12`.
const PROP_BLOB_VERSION: u16 = 0x1;
/// Size of the 2-byte `(ver:4 | cnt:12)` header at the start of every prop_blob.
const HEADER_BYTES: usize = 2;
/// Size of one directory entry: `key:u16 + off:u16 = 4 bytes`.
const DIR_ENTRY_SIZE: usize = 4;
/// Bitmask to extract the 12-bit property count from the header word.
const COUNT_MASK: u16 = 0x0FFF;

/// Wire-format type tags for `Primitive`, written as the byte preceding each
/// property's value payload in the v1 value section.
const TAG_NULL: u8 = 0;
const TAG_BOOL: u8 = 1;
const TAG_INT32: u8 = 2;
const TAG_INT64: u8 = 3;
const TAG_FLOAT32: u8 = 4;
const TAG_FLOAT64: u8 = 5;
const TAG_STRING: u8 = 6;
const TAG_UUID: u8 = 7;
const TAG_UINT16: u8 = 8;
const TAG_BYTES: u8 = 9;

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Decode a single `(tag | payload)` entry starting at byte `pos` in `blob`.
#[inline]
fn decode_single_value(blob: &[u8], pos: usize) -> Option<Primitive> {
    if pos >= blob.len() {
        return None;
    }
    let tag = blob[pos];
    let p = pos + 1;
    match tag {
        TAG_NULL => Some(Primitive::Null),
        TAG_BOOL => {
            if p >= blob.len() {
                return None;
            }
            Some(Primitive::Bool(blob[p] != 0))
        }
        TAG_INT32 => {
            if p + 4 > blob.len() {
                return None;
            }
            Some(Primitive::Int32(i32::from_be_bytes(blob[p..p + 4].try_into().ok()?)))
        }
        TAG_INT64 => {
            if p + 8 > blob.len() {
                return None;
            }
            Some(Primitive::Int64(i64::from_be_bytes(blob[p..p + 8].try_into().ok()?)))
        }
        TAG_UINT16 => {
            if p + 2 > blob.len() {
                return None;
            }
            Some(Primitive::UInt16(u16::from_be_bytes(blob[p..p + 2].try_into().ok()?)))
        }
        TAG_FLOAT32 => {
            if p + 4 > blob.len() {
                return None;
            }
            let bits = u32::from_be_bytes(blob[p..p + 4].try_into().ok()?);
            Some(Primitive::Float32(f32::from_bits(bits)))
        }
        TAG_FLOAT64 => {
            if p + 8 > blob.len() {
                return None;
            }
            let bits = u64::from_be_bytes(blob[p..p + 8].try_into().ok()?);
            Some(Primitive::Float64(f64::from_bits(bits)))
        }
        TAG_STRING => {
            if p + 2 > blob.len() {
                return None;
            }
            let slen = u16::from_be_bytes(blob[p..p + 2].try_into().ok()?) as usize;
            let start = p + 2;
            if start + slen > blob.len() {
                return None;
            }
            let s = std::str::from_utf8(&blob[start..start + slen]).ok()?;
            Some(Primitive::String(smol_str::SmolStr::new(s)))
        }
        TAG_UUID => {
            if p + 16 > blob.len() {
                return None;
            }
            let u = u128::from_be_bytes(blob[p..p + 16].try_into().ok()?);
            Some(Primitive::Uuid(u))
        }
        TAG_BYTES => {
            if p + 2 > blob.len() {
                return None;
            }
            let len = u16::from_be_bytes(blob[p..p + 2].try_into().ok()?) as usize;
            let start = p + 2;
            if start + len > blob.len() {
                return None;
            }
            Some(Primitive::Bytes(blob[start..start + len].to_vec()))
        }
        _ => None,
    }
}

// ── Encode ────────────────────────────────────────────────────────────────────

/// Write a single `tag + payload` into `buf` (no key, no offset).
#[inline]
fn write_value(buf: &mut Vec<u8>, value: &Primitive) {
    match value {
        Primitive::Null => buf.push(TAG_NULL),
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
        Primitive::Bytes(b) => {
            assert!(b.len() <= u16::MAX as usize, "Bytes property exceeds 65535-byte limit");
            buf.push(TAG_BYTES);
            buf.extend_from_slice(&(b.len() as u16).to_be_bytes());
            buf.extend_from_slice(b);
        }
    }
}

/// Encode a property map into the v1 prop_blob format.
///
/// Sorts by key using a stack-allocated index (heap only for P > 32), reserves
/// directory space, then fills offsets in-place while writing the value section —
/// single pass, one heap allocation (the output buffer).
pub(crate) fn encode_props(props: &HashMap<u16, Primitive>) -> Vec<u8> {
    let n = props.len();
    let mut idx: SmallVec<[u16; 32]> = props.keys().copied().collect();
    idx.sort_unstable();

    let dir_end = HEADER_BYTES + n * DIR_ENTRY_SIZE;
    let mut buf = Vec::with_capacity(dir_end + n * 9);

    let header: u16 = (PROP_BLOB_VERSION << 12) | (n as u16 & COUNT_MASK);
    buf.extend_from_slice(&header.to_be_bytes());
    buf.resize(dir_end, 0u8); // directory placeholder slots

    let mut voff: u16 = 0;
    for (slot, &key) in idx.iter().enumerate() {
        let dir_pos = HEADER_BYTES + slot * DIR_ENTRY_SIZE;
        buf[dir_pos..dir_pos + 2].copy_from_slice(&key.to_be_bytes());
        buf[dir_pos + 2..dir_pos + DIR_ENTRY_SIZE].copy_from_slice(&voff.to_be_bytes());
        let before = buf.len();
        write_value(&mut buf, props.get(&key).expect("key came from map"));
        voff += (buf.len() - before) as u16;
    }
    buf
}

// ── Decode ────────────────────────────────────────────────────────────────────

/// Binary-search the v1 directory for `target_key` and decode its value.
///
/// O(log P) comparisons, zero allocation, zero full-blob parse.
/// Returns `None` if the key is absent, the blob is malformed, or the version
/// nibble does not match [`PROP_BLOB_VERSION`].
pub(crate) fn decode_prop_by_key(blob: &[u8], target_key: u16) -> Option<Primitive> {
    if blob.len() < HEADER_BYTES {
        return None;
    }
    let header = u16::from_be_bytes(blob[0..HEADER_BYTES].try_into().ok()?);
    if header >> 12 != PROP_BLOB_VERSION {
        return None;
    }
    let count = (header & COUNT_MASK) as usize;
    let value_start = HEADER_BYTES + count * DIR_ENTRY_SIZE;
    // Single bounds check: verify the entire directory fits before entering the loop.
    // For any mid < count: base+DIR_ENTRY_SIZE ≤ value_start ≤ blob.len().
    if value_start > blob.len() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = count;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let base = HEADER_BYTES + mid * DIR_ENTRY_SIZE;
        let k = u16::from_be_bytes([blob[base], blob[base + 1]]);
        match k.cmp(&target_key) {
            Ordering::Equal => {
                let off = u16::from_be_bytes([blob[base + 2], blob[base + 3]]) as usize;
                return decode_single_value(blob, value_start + off);
            }
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
        }
    }
    None
}

/// Decode the entire v1 prop_blob into a `HashMap<u16, Primitive>`.
///
/// Used for the `Blob → Map` transition on first mutation.
///
/// **Best-effort semantics**: malformed individual entries are silently skipped
/// rather than propagating an error. RocksDB block-level CRC checksums surface
/// filesystem corruption before data reaches this function, so a partial decode
/// result is unreachable in practice. Returns an empty map for unknown versions.
pub(crate) fn decode_all_to_map(blob: &[u8]) -> HashMap<u16, Primitive> {
    if blob.len() < HEADER_BYTES {
        return HashMap::new();
    }
    let Ok(hdr_bytes) = blob[0..HEADER_BYTES].try_into() else { return HashMap::new() };
    let header = u16::from_be_bytes(hdr_bytes);
    if header >> 12 != PROP_BLOB_VERSION {
        return HashMap::new();
    }
    let count = (header & COUNT_MASK) as usize;
    let value_start = HEADER_BYTES + count * DIR_ENTRY_SIZE;
    let mut map = HashMap::with_capacity(count);
    for i in 0..count {
        let base = HEADER_BYTES + i * DIR_ENTRY_SIZE;
        if base + DIR_ENTRY_SIZE > blob.len() {
            break;
        }
        let Ok(kb) = blob[base..base + 2].try_into() else { break };
        let Ok(ob) = blob[base + 2..base + DIR_ENTRY_SIZE].try_into() else { break };
        let key = u16::from_be_bytes(kb);
        let off = u16::from_be_bytes(ob) as usize;
        if let Some(value) = decode_single_value(blob, value_start + off) {
            map.insert(key, value);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use smol_str::SmolStr;

    use super::{decode_all_to_map, decode_prop_by_key, encode_props};
    use crate::types::gvalue::Primitive;

    #[test]
    fn empty_props_roundtrip() {
        let blob = encode_props(&HashMap::new());
        assert_eq!(blob.len(), 2, "empty blob should be header-only (2 bytes)");
        assert!(decode_all_to_map(&blob).is_empty());
    }

    #[test]
    fn single_prop_blob_size() {
        // header(2) + directory(4) + tag(1) + i64(8) = 15 B
        let blob = encode_props(&[(1u16, Primitive::Int64(42))].into());
        assert_eq!(blob.len(), 15, "single Int64 prop should be 15 bytes");
    }

    #[test]
    fn encode_decode_by_key() {
        let m: HashMap<u16, Primitive> = [
            (10u16, Primitive::String(SmolStr::new("Alice"))),
            (20u16, Primitive::Int32(30)),
            (5u16, Primitive::Bool(true)),
        ]
        .into();
        let blob = encode_props(&m);

        assert_eq!(decode_prop_by_key(&blob, 5), Some(Primitive::Bool(true)));
        assert_eq!(decode_prop_by_key(&blob, 10), Some(Primitive::String(SmolStr::new("Alice"))));
        assert_eq!(decode_prop_by_key(&blob, 20), Some(Primitive::Int32(30)));
        assert_eq!(decode_prop_by_key(&blob, 99), None);
    }

    #[test]
    fn encode_decode_all_to_map() {
        let m: HashMap<u16, Primitive> = [
            (1u16, Primitive::Null),
            (2u16, Primitive::Bool(true)),
            (3u16, Primitive::Int32(-100)),
            (4u16, Primitive::Int64(i64::MAX)),
            (5u16, Primitive::Float32(1.5f32)),
            (6u16, Primitive::Float64(f64::MIN_POSITIVE)),
            (7u16, Primitive::String(SmolStr::new("hello"))),
            (8u16, Primitive::Uuid(u128::MAX)),
            (9u16, Primitive::UInt16(u16::MAX)),
            (10u16, Primitive::Bytes(vec![0xFF; 4])),
        ]
        .into();
        let blob = encode_props(&m);
        let decoded = decode_all_to_map(&blob);
        assert_eq!(decoded.len(), m.len());
        for (k, v) in &m {
            assert_eq!(decoded.get(k), Some(v), "key {k} mismatch");
        }
    }

    #[test]
    fn binary_search_absent_key() {
        let blob = encode_props(
            &[(2u16, Primitive::Int32(1)), (4u16, Primitive::Int32(2)), (6u16, Primitive::Int32(3))].into(),
        );
        for absent in [1u16, 3, 5, 7] {
            assert_eq!(decode_prop_by_key(&blob, absent), None, "key {absent} should be absent");
        }
    }

    // ── Corruption / robustness ───────────────────────────────────────────────

    #[test]
    fn decode_prop_by_key_short_blob() {
        assert_eq!(decode_prop_by_key(&[], 1), None);
        assert_eq!(decode_prop_by_key(&[0x10], 1), None); // one byte — header incomplete
    }

    #[test]
    fn decode_prop_by_key_truncated_directory() {
        // Valid header claiming 3 entries, but blob truncated before directory ends.
        let mut blob = encode_props(
            &[(1u16, Primitive::Int32(1)), (2u16, Primitive::Int32(2)), (3u16, Primitive::Int32(3))].into(),
        );
        blob.truncate(blob.len() - 5); // chop part of value section and last directory entry
                                       // Should not panic — binary search hits the out-of-bounds guard.
        let _ = decode_prop_by_key(&blob, 1);
        let _ = decode_prop_by_key(&blob, 3);
    }

    #[test]
    fn decode_prop_by_key_offset_past_end() {
        // Manually craft: version=0x1, count=1, key=5, offset=9999 (past end).
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x1001u16.to_be_bytes()); // header
        blob.extend_from_slice(&5u16.to_be_bytes()); // key
        blob.extend_from_slice(&9999u16.to_be_bytes()); // offset far past end
        blob.push(0u8); // one value byte so blob isn't completely empty
        assert_eq!(decode_prop_by_key(&blob, 5), None);
    }

    #[test]
    fn decode_all_to_map_short_and_missing_value_section() {
        assert!(decode_all_to_map(&[]).is_empty());
        assert!(decode_all_to_map(&[0x10]).is_empty()); // one byte — header incomplete
                                                        // Valid header (count=1) but no value section — best-effort, no panic.
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x1001u16.to_be_bytes()); // count=1
        blob.extend_from_slice(&10u16.to_be_bytes()); // key
        blob.extend_from_slice(&0u16.to_be_bytes()); // offset=0
                                                     // Value section missing entirely — entry silently skipped.
        assert!(decode_all_to_map(&blob).is_empty());
    }

    #[test]
    fn decode_all_to_map_offset_past_end() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&0x1001u16.to_be_bytes()); // version=1, count=1
        blob.extend_from_slice(&7u16.to_be_bytes()); // key
        blob.extend_from_slice(&9999u16.to_be_bytes()); // offset past blob end
        blob.push(0u8); // placeholder
        assert!(decode_all_to_map(&blob).is_empty());
    }

    #[test]
    fn unknown_version_rejected() {
        // Build a valid v1 blob, then flip the version nibble to 0x2.
        let mut blob = encode_props(&[(1u16, Primitive::Int32(42))].into());
        blob[0] = (blob[0] & 0x0F) | 0x20; // version nibble → 0x2
        assert_eq!(decode_prop_by_key(&blob, 1), None);
        assert!(decode_all_to_map(&blob).is_empty());
    }

    // ── Gap coverage: G1-G7 ───────────────────────────────────────────────────

    #[test]
    fn g1_zero_length_string_roundtrip() {
        let blob = encode_props(&[(5u16, Primitive::String(SmolStr::new("")))].into());
        assert_eq!(decode_prop_by_key(&blob, 5), Some(Primitive::String(SmolStr::new(""))));
        let map = decode_all_to_map(&blob);
        assert_eq!(map.get(&5), Some(&Primitive::String(SmolStr::new(""))));
    }

    #[test]
    fn g2_zero_length_bytes_roundtrip() {
        let blob = encode_props(&[(7u16, Primitive::Bytes(vec![]))].into());
        assert_eq!(decode_prop_by_key(&blob, 7), Some(Primitive::Bytes(vec![])));
        let map = decode_all_to_map(&blob);
        assert_eq!(map.get(&7), Some(&Primitive::Bytes(vec![])));
    }

    #[test]
    fn g3_max_key_boundary_values() {
        // key=0 (smallest possible) and key=u16::MAX (largest possible)
        let m: HashMap<u16, Primitive> = [(0u16, Primitive::Bool(true)), (u16::MAX, Primitive::Bool(false))].into();
        let blob = encode_props(&m);
        assert_eq!(decode_prop_by_key(&blob, 0), Some(Primitive::Bool(true)));
        assert_eq!(decode_prop_by_key(&blob, u16::MAX), Some(Primitive::Bool(false)));
        let decoded = decode_all_to_map(&blob);
        assert_eq!(decoded.len(), 2);
    }

    #[test]
    fn g4_large_string_value() {
        // 10 000-char string: exercises u16 length-prefix for values > 255 bytes.
        let big: String = "x".repeat(10_000);
        let blob = encode_props(&[(42u16, Primitive::String(SmolStr::new(&big)))].into());
        let v = decode_prop_by_key(&blob, 42).unwrap();
        assert!(matches!(v, Primitive::String(ref s) if s.len() == 10_000));
    }

    #[test]
    fn g5_sequential_keys_binary_search_edges() {
        // Keys 1..=8 — verify first, last, and a middle key are all found.
        let m: HashMap<u16, Primitive> = (1u16..=8).map(|k| (k, Primitive::Int32(k as i32 * 10))).collect();
        let blob = encode_props(&m);
        assert_eq!(decode_prop_by_key(&blob, 1), Some(Primitive::Int32(10))); // first
        assert_eq!(decode_prop_by_key(&blob, 8), Some(Primitive::Int32(80))); // last
        assert_eq!(decode_prop_by_key(&blob, 4), Some(Primitive::Int32(40))); // middle
        assert_eq!(decode_prop_by_key(&blob, 9), None); // just past end
    }

    #[test]
    fn g6_sparse_key_range_binary_search() {
        let m: HashMap<u16, Primitive> =
            [(1000u16, Primitive::Int32(1)), (50000u16, Primitive::Int32(2)), (65500u16, Primitive::Int32(3))].into();
        let blob = encode_props(&m);
        assert_eq!(decode_prop_by_key(&blob, 1000), Some(Primitive::Int32(1)));
        assert_eq!(decode_prop_by_key(&blob, 50000), Some(Primitive::Int32(2)));
        assert_eq!(decode_prop_by_key(&blob, 65500), Some(Primitive::Int32(3)));
        // Keys in the gaps should return None.
        assert_eq!(decode_prop_by_key(&blob, 999), None);
        assert_eq!(decode_prop_by_key(&blob, 25000), None);
        assert_eq!(decode_prop_by_key(&blob, 65501), None);
    }

    #[test]
    fn g7_decode_prop_by_key_on_empty_blob() {
        // An empty-property blob (count=0) should return None for any key without panicking.
        let blob = encode_props(&HashMap::new());
        assert_eq!(decode_prop_by_key(&blob, 0), None);
        assert_eq!(decode_prop_by_key(&blob, 100), None);
        assert_eq!(decode_prop_by_key(&blob, u16::MAX), None);
    }
}
