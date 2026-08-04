// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Write-ahead log for vector index mutations.
//!
//! Every vector insert/remove is written to `CF_VECTOR_WAL` in the same
//! `WriteBatch` as the graph mutation.  On recovery, entries after the
//! index's `last_replayed_timestamp` are replayed into the index.

use crate::schema::Schema;
use crate::store::RocksStorage;
use crate::types::StoreError;
use crate::vector::error::VectorError;
use crate::vector::{EntityKey, VectorIndexMap};
use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

/// Flush pending vector mutations to the WAL column family in the transaction.
pub(crate) fn flush_vector_wal(
    store: &mut crate::store::rocks::transaction::Transaction,
    pending_ops: &[crate::vector::PendingVectorOp],
    resolve_prop_id: impl Fn(&str) -> Option<u16>,
) -> Result<(), StoreError> {
    for op in pending_ops {
        match op {
            crate::vector::PendingVectorOp::Inserted { key, prop_name, vector, ts } => {
                if let Some(prop_key_id) = resolve_prop_id(prop_name) {
                    let wal_key = encode_wal_key(prop_key_id, 0x00, *ts);
                    if let Ok(wal_val) = encode_wal_insert(key, vector) {
                        store.put_wal_entry(&wal_key, &wal_val)?;
                    }
                }
            }
            crate::vector::PendingVectorOp::Removed { key, prop_name, ts } => {
                if let Some(prop_key_id) = resolve_prop_id(prop_name) {
                    let wal_key = encode_wal_key(prop_key_id, 0x00, *ts);
                    if let Ok(wal_val) = encode_wal_remove(key) {
                        store.put_wal_entry(&wal_key, &wal_val)?;
                    }
                }
            }
        }
    }
    Ok(())
}

// ── WAL replay & GC ─────────────────────────────────────────────────────────

/// Replay WAL entries into vector indexes after open. Seeds the global
/// clock from the maximum WAL timestamp so new timestamps are strictly later.
/// Uses targeted prefix seek per declared index to only replay unseen mutations.
pub(crate) fn replay_vector_wal(
    store: &RocksStorage,
    vector_indexes: &Arc<RwLock<VectorIndexMap>>,
    schema: &Schema,
) -> Result<(), StoreError> {
    use crate::store::rocks::CF_VECTOR_WAL;
    use rocksdb::IteratorMode;

    let Some(cf) = store.db.cf_handle(CF_VECTOR_WAL) else {
        seed_clock(0);
        return Ok(());
    };

    let mut max_ts: u64 = 0;

    // Build lookup list: (entity_type, prop_key_id, index_arc).
    let mut by_prop_and_entity = Vec::new();
    let indexes = vector_indexes.read();
    for ((entity_type, prop_name), arc) in indexes.iter() {
        if let Some(id) = schema.prop_key_id(prop_name) {
            by_prop_and_entity.push((*entity_type, id, Arc::clone(arc)));
        }
    }
    drop(indexes);

    // Incremental prefix-seek replay per declared index:
    // seek key = [prop_key_id BE][entity_type BE][seek_ts BE]
    for (entity_type, prop_key_id, arc) in by_prop_and_entity {
        let entity_type_byte = match entity_type {
            crate::vector::VectorEntityType::Vertex => 0x00,
            crate::vector::VectorEntityType::Edge => 0x01,
        };
        let mut guard = arc.write();
        let last_ts = guard.last_replayed_timestamp();
        max_ts = max_ts.max(last_ts);
        let seek_ts = if last_ts == 0 { 0 } else { last_ts.saturating_add(1) };

        let mut seek_key = [0u8; 11];
        seek_key[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
        seek_key[2] = entity_type_byte;
        seek_key[3..11].copy_from_slice(&seek_ts.to_be_bytes());

        let iter = store.db.iterator_cf(&cf, IteratorMode::From(&seek_key, rocksdb::Direction::Forward));
        for item in iter {
            let Ok((key, value)) = item else { continue };
            if key.len() < 11 {
                break;
            }
            let Some(k_prop_id) = decode_wal_key_prop_id(&key) else { break };
            if k_prop_id != prop_key_id {
                break;
            }
            let Some(k_entity_type) = decode_wal_key_entity_type(&key) else { break };
            if k_entity_type != entity_type_byte {
                break;
            }
            let Some(ts) = decode_wal_key_ts(&key) else { break };
            max_ts = max_ts.max(ts);
            if ts <= last_ts {
                continue;
            }
            let Ok((op_type, ek, vector)) = decode_wal_value(&value) else { continue };

            match op_type {
                0 => match guard.insert(&ek, &vector) {
                    Ok(()) => guard.set_last_replayed_timestamp(ts),
                    Err(e) => eprintln!("vector WAL replay: insert failed at ts={ts}: {e}"),
                },
                1 => {
                    let _ = guard.remove(&ek);
                    guard.set_last_replayed_timestamp(ts);
                }
                _ => {}
            }
        }
    }

    seed_clock(max_ts);
    Ok(())
}

/// Delete WAL entries whose timestamps are at or below each index's `last_replayed_timestamp`.
///
/// Called during `close()` after snapshots are saved, so the WAL does not grow without bound.
/// Each index tracks its own high-water mark; entries strictly earlier than that mark are
/// safe to drop because the snapshot already embeds their effect.
pub(crate) fn gc_vector_wal(
    store: &RocksStorage,
    vector_indexes: &Arc<RwLock<VectorIndexMap>>,
    schema: &Schema,
) -> Result<(), StoreError> {
    use crate::store::rocks::CF_VECTOR_WAL;
    use rocksdb::IteratorMode;

    let Some(cf) = store.db.cf_handle(CF_VECTOR_WAL) else {
        return Ok(());
    };

    let indexes = vector_indexes.read();
    let mut keys_to_delete: Vec<Vec<u8>> = Vec::new();

    for ((entity_type, prop_name), arc) in indexes.iter() {
        let Some(prop_key_id) = schema.prop_key_id(prop_name) else { continue };
        let entity_type_byte: u8 = match entity_type {
            crate::vector::VectorEntityType::Vertex => 0x00,
            crate::vector::VectorEntityType::Edge => 0x01,
        };

        let guard = arc.read();
        let cutoff_ts = guard.last_replayed_timestamp();
        if cutoff_ts == 0 {
            continue;
        }

        let mut prefix = [0u8; 3];
        prefix[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
        prefix[2] = entity_type_byte;

        // WAL keys are sorted: [prop_key_id BE][entity_type][ts BE][random BE].
        // Seek to the first key for this (prop_key_id, entity_type) and collect
        // entries whose ts <= cutoff_ts. Stop when the prefix changes or ts exceeds cutoff.
        let iter = store.db.iterator_cf(&cf, IteratorMode::From(&prefix, rocksdb::Direction::Forward));
        for item in iter {
            let Ok((key, _)) = item else { continue };
            if key.len() < 3 || key[0..3] != prefix {
                break;
            }
            let Some(ts) = decode_wal_key_ts(&key) else { continue };
            if ts <= cutoff_ts {
                keys_to_delete.push(key.to_vec());
            } else {
                break;
            }
        }
    }
    drop(indexes);

    if !keys_to_delete.is_empty() {
        let mut batch = rocksdb::WriteBatchWithTransaction::<true>::default();
        for key in &keys_to_delete {
            batch.delete_cf(&cf, key);
        }
        store.db.write(batch).map_err(StoreError::RocksDb)?;
    }

    Ok(())
}
