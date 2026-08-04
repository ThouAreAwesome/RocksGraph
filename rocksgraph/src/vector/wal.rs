// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Write-ahead log for vector index mutations.
//!
//! Every vector insert/remove is written to `CF_VECTOR_WAL` in the same
//! `WriteBatch` as the graph mutation.  On recovery, entries after the
//! index's `last_replayed_timestamp` are replayed into the index.

use crate::types::StoreError;
use crate::vector::error::VectorError;
use crate::vector::EntityKey;
use std::sync::atomic::{AtomicU64, Ordering};

// ── WAL clock ────────────────────────────────────────────────────────────────

/// Global monotonic timestamp.  Seeded from `SystemTime` on first use.
static WAL_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Initialise the WAL clock from a stored high-water mark.
/// Must be called during `Graph::open` before any WAL writes.
pub(crate) fn seed_clock(hwm: u64) {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0);
    let seed = hwm.max(now);
    WAL_CLOCK.store(seed, Ordering::Release);
}

/// Allocate a new monotonically-increasing timestamp.
pub(crate) fn next_timestamp() -> u64 {
    WAL_CLOCK.fetch_add(1, Ordering::AcqRel)
}

// ── WAL key ──────────────────────────────────────────────────────────────────

/// Key = [prop_key_id: u16 BE][entity_type: u8][ts: u64 BE][random: u32 BE]
pub(crate) fn encode_wal_key(prop_key_id: u16, entity_type_byte: u8, ts: u64) -> [u8; 15] {
    let mut buf = [0u8; 15];
    buf[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
    buf[2] = entity_type_byte;
    buf[3..11].copy_from_slice(&ts.to_be_bytes());
    let r = (ts.wrapping_mul(0x9E3779B97F4A7C15) ^ (ts >> 33)) as u32;
    buf[11..15].copy_from_slice(&r.to_be_bytes());
    buf
}

pub(crate) fn decode_wal_key_ts(key: &[u8]) -> Option<u64> {
    if key.len() < 11 {
        return None;
    }
    Some(u64::from_be_bytes(key[3..11].try_into().unwrap()))
}

pub(crate) fn decode_wal_key_prop_id(key: &[u8]) -> Option<u16> {
    if key.len() < 3 {
        return None;
    }
    Some(u16::from_be_bytes(key[0..2].try_into().unwrap()))
}

pub(crate) fn decode_wal_key_entity_type(key: &[u8]) -> Option<u8> {
    if key.len() < 3 {
        return None;
    }
    Some(key[2])
}

// ── WAL value ────────────────────────────────────────────────────────────────

const OP_INSERT: u8 = 0;
const OP_REMOVE: u8 = 1;

pub(crate) fn encode_wal_insert(key: &EntityKey, vector: &[f32]) -> Result<Vec<u8>, VectorError> {
    let ek = encode_entity_key(key)?;
    let mut buf = Vec::with_capacity(1 + ek.len() + 4 + vector.len() * 4);
    buf.push(OP_INSERT);
    buf.extend_from_slice(&ek);
    buf.extend_from_slice(&(vector.len() as u32).to_le_bytes());
    for v in vector {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    Ok(buf)
}

pub(crate) fn encode_wal_remove(key: &EntityKey) -> Result<Vec<u8>, VectorError> {
    let ek = encode_entity_key(key)?;
    let mut buf = Vec::with_capacity(1 + ek.len());
    buf.push(OP_REMOVE);
    buf.extend_from_slice(&ek);
    Ok(buf)
}

/// Returns (op_type, EntityKey, vector_data). op_type: 0=insert, 1=remove.
pub(crate) fn decode_wal_value(value: &[u8]) -> Result<(u8, EntityKey, Vec<f32>), StoreError> {
    if value.is_empty() {
        return Err(StoreError::CorruptData("empty WAL value"));
    }
    let op_type = value[0];
    let (ek, ek_len) = decode_entity_key(&value[1..])?;
    let header_len = 1 + ek_len;

    let vec = if op_type == OP_INSERT {
        if value.len() < header_len + 4 {
            return Err(StoreError::CorruptData("truncated WAL insert value"));
        }
        let len = u32::from_le_bytes(value[header_len..header_len + 4].try_into().unwrap()) as usize;
        let data_start = header_len + 4;
        if value.len() < data_start + len * 4 {
            return Err(StoreError::CorruptData("truncated WAL vector data"));
        }
        let mut v = Vec::with_capacity(len);
        for i in 0..len {
            let pos = data_start + i * 4;
            v.push(f32::from_le_bytes(value[pos..pos + 4].try_into().unwrap()));
        }
        v
    } else {
        Vec::new()
    };

    Ok((op_type, ek, vec))
}

// ── Entity key ───────────────────────────────────────────────────────────────

fn encode_entity_key(key: &EntityKey) -> Result<Vec<u8>, VectorError> {
    match key {
        EntityKey::Vertex(vk) => {
            let mut buf = Vec::with_capacity(9);
            buf.push(0x00);
            buf.extend_from_slice(&vk.to_le_bytes());
            Ok(buf)
        }
        EntityKey::Edge(_) => Err(VectorError::Unsupported("edge vector indexes are not supported".into())),
    }
}

fn decode_entity_key(bytes: &[u8]) -> Result<(EntityKey, usize), StoreError> {
    if bytes.is_empty() {
        return Err(StoreError::CorruptData("empty entity key in WAL"));
    }
    match bytes[0] {
        0x00 => {
            if bytes.len() < 9 {
                return Err(StoreError::CorruptData("truncated vertex key in WAL"));
            }
            let id = i64::from_le_bytes(bytes[1..9].try_into().unwrap());
            Ok((EntityKey::Vertex(id), 9))
        }
        0x01 => Err(StoreError::UnsupportedOperation("edge vector WAL entries are not yet supported".into())),
        _ => Err(StoreError::CorruptData("unknown entity key discriminant in WAL")),
    }
}
