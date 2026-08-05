// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pure file persistence and snapshot format for vector indexes.
//!
//! Provides atomic file writes (via temporary files and renaming) and CRC-32C
//! verified snapshot file reading and writing.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use smol_str::SmolStr;

use super::brute_force::BruteForceIndex;
use super::error::{VectorEntityType, VectorError};
use super::hnsw::UsearchHnswIndex;
use super::traits::{AnnAlgorithm, DistanceMetric, HnswConfig, Quantization, VectorIndexConfig};
use super::VectorIndexMap;
use crate::store::RocksStorage;

// ── Snapshot constants ──────────────────────────────────────────────────────

/// Magic bytes: "RG_V" in ASCII, big-endian u32.
pub(crate) const SNAPSHOT_MAGIC: u32 = 0x52475F56;

/// Current snapshot format version.
pub(crate) const SNAPSHOT_FORMAT_VERSION: u16 = 2;

/// Header size in bytes:
/// magic(4) + version(2) + timestamp(8) + dim(4) + metric(1) + algorithm(1)
/// + tombstone(8) + next_edge_label(8) + payload_len(8) = 44 bytes.
pub(crate) const SNAPSHOT_HEADER_SIZE: usize = 44;

// Named byte offsets into the header, so a future field insertion is a
// visible diff to every offset below it rather than a silent off-by-N read.
const HDR_OFF_MAGIC: usize = 0; // u32 BE
const HDR_OFF_VERSION: usize = 4; // u16 BE
const HDR_OFF_TIMESTAMP: usize = 6; // u64 LE
const HDR_OFF_DIMENSION: usize = 14; // u32 LE
const HDR_OFF_METRIC: usize = 18; // u8
const HDR_OFF_ALGORITHM: usize = 19; // u8 (1 = HNSW; only algorithm supported in v0.2)
const HDR_OFF_TOMBSTONE_COUNT: usize = 20; // u64 LE
const HDR_OFF_NEXT_EDGE_LABEL: usize = 28; // u64 LE (reserved, always 0 in v0.2)
const HDR_OFF_PAYLOAD_LEN: usize = 36; // u64 LE

/// Metadata header stored at the beginning of each vector snapshot file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotHeader {
    pub last_replayed_timestamp: u64,
    pub dimension: usize,
    pub metric: DistanceMetric,
    pub tombstone_count: u64,
    pub payload_len: usize,
}

/// Compute the path to a vector index snapshot file given the database path.
pub(crate) fn vector_snapshot_path(db_path: &Path, entity_type: VectorEntityType, property: &str) -> PathBuf {
    match entity_type {
        VectorEntityType::Vertex => db_path.join(format!("vector_idx_{property}.snapshot")),
        VectorEntityType::Edge => db_path.join(format!("vector_idx_edge_{property}.snapshot")),
    }
}

/// Save an index payload and header to a snapshot file atomically.
///
/// Writes to `<path>.tmp` first, flushes, syncs to disk, and renames to `<path>`.
/// Appends a CRC-32C checksum at the end of the file.
pub(crate) fn save_snapshot_file(path: &Path, header: &SnapshotHeader, payload: &[u8]) -> Result<(), VectorError> {
    let tmp_path = path.with_extension("snapshot.tmp");
    let mut file = std::fs::File::create(&tmp_path)?;

    let mut header_buf = [0u8; SNAPSHOT_HEADER_SIZE];
    header_buf[HDR_OFF_MAGIC..HDR_OFF_MAGIC + 4].copy_from_slice(&SNAPSHOT_MAGIC.to_be_bytes());
    header_buf[HDR_OFF_VERSION..HDR_OFF_VERSION + 2].copy_from_slice(&SNAPSHOT_FORMAT_VERSION.to_be_bytes());
    header_buf[HDR_OFF_TIMESTAMP..HDR_OFF_TIMESTAMP + 8].copy_from_slice(&header.last_replayed_timestamp.to_le_bytes());
    header_buf[HDR_OFF_DIMENSION..HDR_OFF_DIMENSION + 4].copy_from_slice(&(header.dimension as u32).to_le_bytes());
    header_buf[HDR_OFF_METRIC] = header.metric as u8;
    header_buf[HDR_OFF_ALGORITHM] = 1u8; // algorithm byte: 1=HNSW
    header_buf[HDR_OFF_TOMBSTONE_COUNT..HDR_OFF_TOMBSTONE_COUNT + 8]
        .copy_from_slice(&header.tombstone_count.to_le_bytes());
    header_buf[HDR_OFF_NEXT_EDGE_LABEL..HDR_OFF_NEXT_EDGE_LABEL + 8].copy_from_slice(&0u64.to_le_bytes());
    header_buf[HDR_OFF_PAYLOAD_LEN..HDR_OFF_PAYLOAD_LEN + 8].copy_from_slice(&(payload.len() as u64).to_le_bytes());

    file.write_all(&header_buf)?;
    file.write_all(payload)?;

    // CRC-32C of all header + payload bytes
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header_buf);
    hasher.update(payload);
    let crc = hasher.finalize();

    file.write_all(&crc.to_le_bytes())?;
    file.sync_all()?;
    drop(file);

    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Load and validate a snapshot file, returning its header and payload bytes.
pub(crate) fn load_snapshot_file(
    path: &Path,
    expected_dim: usize,
    expected_metric: DistanceMetric,
) -> Result<(SnapshotHeader, Vec<u8>), VectorError> {
    let bytes = std::fs::read(path)?;

    if bytes.len() < SNAPSHOT_HEADER_SIZE + 4 {
        return Err(VectorError::Internal("snapshot file too short".into()));
    }

    let magic = u32::from_be_bytes(bytes[HDR_OFF_MAGIC..HDR_OFF_MAGIC + 4].try_into().unwrap());
    if magic != SNAPSHOT_MAGIC {
        return Err(VectorError::Internal("snapshot magic mismatch".into()));
    }

    let format_version = u16::from_be_bytes(bytes[HDR_OFF_VERSION..HDR_OFF_VERSION + 2].try_into().unwrap());
    if format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(VectorError::Unsupported(format!("unsupported snapshot format version {format_version}")));
    }

    let timestamp = u64::from_le_bytes(bytes[HDR_OFF_TIMESTAMP..HDR_OFF_TIMESTAMP + 8].try_into().unwrap());
    let stored_dim = u32::from_le_bytes(bytes[HDR_OFF_DIMENSION..HDR_OFF_DIMENSION + 4].try_into().unwrap()) as usize;
    let stored_metric_byte = bytes[HDR_OFF_METRIC];
    let stored_tombstone =
        u64::from_le_bytes(bytes[HDR_OFF_TOMBSTONE_COUNT..HDR_OFF_TOMBSTONE_COUNT + 8].try_into().unwrap());
    let payload_len =
        u64::from_le_bytes(bytes[HDR_OFF_PAYLOAD_LEN..HDR_OFF_PAYLOAD_LEN + 8].try_into().unwrap()) as usize;

    if bytes.len() < SNAPSHOT_HEADER_SIZE + payload_len + 4 {
        return Err(VectorError::Internal("snapshot file too short".into()));
    }

    if stored_dim != expected_dim {
        return Err(VectorError::DimensionMismatch { expected: expected_dim, actual: stored_dim });
    }

    let stored_metric = match stored_metric_byte {
        0 => DistanceMetric::Cosine,
        1 => DistanceMetric::Euclidean,
        2 => DistanceMetric::DotProduct,
        _ => return Err(VectorError::Internal("unknown metric in snapshot".into())),
    };
    if stored_metric != expected_metric {
        return Err(VectorError::Unsupported(format!(
            "snapshot metric ({stored_metric:?}) does not match config ({expected_metric:?})",
        )));
    }

    // Verify CRC-32C
    let crc_offset = SNAPSHOT_HEADER_SIZE + payload_len;
    let expected_crc = u32::from_le_bytes(bytes[crc_offset..crc_offset + 4].try_into().unwrap());
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&bytes[..crc_offset]);
    let actual_crc = hasher.finalize();
    if expected_crc != actual_crc {
        return Err(VectorError::Internal("snapshot CRC mismatch — file may be corrupt".into()));
    }

    let payload = bytes[SNAPSHOT_HEADER_SIZE..crc_offset].to_vec();
    let header = SnapshotHeader {
        last_replayed_timestamp: timestamp,
        dimension: stored_dim,
        metric: stored_metric,
        tombstone_count: stored_tombstone,
        payload_len,
    };

    Ok((header, payload))
}

// ── Config persistence (CF_SCHEMA) ──────────────────────────────────────────

// Named byte offsets into the CF_SCHEMA config value, so a future field
// insertion is a visible diff to every offset below it rather than a silent
// off-by-N read.
// Wire format: [entity_type: u8][dim: u32 LE][metric: u8]
//   [algo_kind: u8][m: u32 LE][ef_cons: u32 LE][ef_search: u32 LE][quant: u8]
const CFG_OFF_ENTITY_TYPE: usize = 0; // u8
const CFG_OFF_DIMENSION: usize = 1; // u32 LE
const CFG_OFF_METRIC: usize = 5; // u8
const CFG_OFF_ALGORITHM: usize = 6; // u8
const CFG_OFF_HNSW_M: usize = 7; // u32 LE
const CFG_OFF_HNSW_EF_CONSTRUCTION: usize = 11; // u32 LE
const CFG_OFF_HNSW_EF_SEARCH: usize = 15; // u32 LE
const CFG_OFF_QUANTIZATION: usize = 19; // u8
const CFG_MIN_LEN: usize = 20;

/// Decode a single vector index config from the binary value format used
/// in CF_SCHEMA.  Returns `None` when the bytes are too short or contain
/// an unrecognised metric / algorithm tag.
fn decode_vector_config_bytes(property: &str, value: &[u8]) -> Option<VectorIndexConfig> {
    if value.len() < CFG_MIN_LEN {
        return None;
    }
    let entity_type = match value[CFG_OFF_ENTITY_TYPE] {
        0 => VectorEntityType::Vertex,
        1 => VectorEntityType::Edge,
        _ => return None,
    };
    let dimension = u32::from_le_bytes(value[CFG_OFF_DIMENSION..CFG_OFF_DIMENSION + 4].try_into().unwrap()) as usize;
    let metric = match value[CFG_OFF_METRIC] {
        0 => DistanceMetric::Cosine,
        1 => DistanceMetric::Euclidean,
        2 => DistanceMetric::DotProduct,
        _ => return None,
    };
    let algorithm = match value[CFG_OFF_ALGORITHM] {
        0 => AnnAlgorithm::BruteForce,
        1 => AnnAlgorithm::Hnsw(HnswConfig {
            m: u32::from_le_bytes(value[CFG_OFF_HNSW_M..CFG_OFF_HNSW_M + 4].try_into().unwrap()) as usize,
            ef_construction: u32::from_le_bytes(
                value[CFG_OFF_HNSW_EF_CONSTRUCTION..CFG_OFF_HNSW_EF_CONSTRUCTION + 4].try_into().unwrap(),
            ) as usize,
            ef_search: u32::from_le_bytes(value[CFG_OFF_HNSW_EF_SEARCH..CFG_OFF_HNSW_EF_SEARCH + 4].try_into().unwrap())
                as usize,
        }),
        _ => return None,
    };
    let quantization = match value[CFG_OFF_QUANTIZATION] {
        0 => Quantization::F16,
        1 => Quantization::F32,
        _ => Quantization::default(),
    };
    Some(VectorIndexConfig {
        property: SmolStr::from(property),
        entity_type,
        dimension,
        metric,
        algorithm,
        quantization,
    })
}

/// Load every declared vector index config from CF_SCHEMA, loading each
/// index's on-disk snapshot if present or creating a fresh empty index
/// otherwise. Populates `map` in place.
pub(crate) fn load_vector_configs(store: &RocksStorage, map: &mut VectorIndexMap) {
    use crate::store::rocks::CF_SCHEMA;
    use rocksdb::IteratorMode;

    let Some(cf) = store.db.cf_handle(CF_SCHEMA) else { return };

    let iter = store.db.iterator_cf(&cf, IteratorMode::Start);
    for item in iter {
        let Ok((key, value)) = item else { continue };
        if key.len() < 3 || key[0] != 0x10 {
            continue;
        }
        let Ok(prop_name) = std::str::from_utf8(&key[2..]) else { continue };
        let Some(config) = decode_vector_config_bytes(prop_name, &value) else { continue };
        if !matches!(config.algorithm, AnnAlgorithm::Hnsw(_)) {
            // BruteForce: ephemeral — no snapshot. WAL replay will rebuild the entries.
            let index = BruteForceIndex::with_config(&config);
            map.insert((config.entity_type, SmolStr::from(prop_name)), Arc::new(RwLock::new(Box::new(index))));
            continue;
        }
        let snap_path = vector_snapshot_path(&store.path, config.entity_type, prop_name);
        let index_res = if snap_path.exists() {
            super::hnsw::load_vector_index(&snap_path, &config).or_else(|e| {
                eprintln!(
                    "vector index load warning: failed to load snapshot '{}' ({e}), creating fresh index",
                    prop_name
                );
                UsearchHnswIndex::new(&config)
            })
        } else {
            UsearchHnswIndex::new(&config)
        };
        if let Ok(index) = index_res {
            map.insert((config.entity_type, SmolStr::from(prop_name)), Arc::new(RwLock::new(Box::new(index))));
        }
    }
}

/// Read a single vector index config from CF_SCHEMA.
///
/// Uses the same key format and binary encoding as [`load_vector_configs`].
pub(crate) fn read_vector_config(
    store: &RocksStorage,
    entity_type: VectorEntityType,
    property: &str,
) -> Result<VectorIndexConfig, VectorError> {
    use crate::store::rocks::CF_SCHEMA;

    let cf = store
        .db
        .cf_handle(CF_SCHEMA)
        .ok_or_else(|| VectorError::IndexNotFound { entity_type, property: SmolStr::from(property) })?;

    // Key: [0x10][entity_type_byte][prop_name_bytes]
    let mut key = Vec::with_capacity(2 + property.len());
    key.push(0x10);
    key.push(entity_type as u8);
    key.extend_from_slice(property.as_bytes());

    let value = store
        .db
        .get_cf(&cf, &key)
        .map_err(|_| VectorError::IndexNotFound { entity_type, property: SmolStr::from(property) })?
        .ok_or_else(|| VectorError::IndexNotFound { entity_type, property: SmolStr::from(property) })?;

    // Note: entity_type in the returned config comes from the stored bytes
    // (value[0]), not the caller-supplied parameter.  The stored value is
    // authoritative; the parameter is only used for key construction and
    // error messages.
    decode_vector_config_bytes(property, &value)
        .ok_or_else(|| VectorError::Internal("vector config value too short or invalid".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_header_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.snapshot");

        let header = SnapshotHeader {
            last_replayed_timestamp: 12345,
            dimension: 128,
            metric: DistanceMetric::Cosine,
            tombstone_count: 7,
            payload_len: 4,
        };
        let payload = vec![1, 2, 3, 4];

        save_snapshot_file(&path, &header, &payload).unwrap();
        let (loaded_header, loaded_payload) = load_snapshot_file(&path, 128, DistanceMetric::Cosine).unwrap();

        assert_eq!(header, loaded_header);
        assert_eq!(payload, loaded_payload);
    }
}
