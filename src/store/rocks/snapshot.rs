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

//! Read-only RocksDB snapshot adapter.
//!
//! `RocksSnapshot` wraps a point-in-time `SnapshotWithThreadMode` pinned
//! against the shared `Arc<OptimisticTransactionDB>`. All reads use plain
//! `get_cf_opt` / `iterator_cf_opt` with the snapshot set in `ReadOptions` —
//! no OCC tracking, no write-set, no locking.
//!
//! # Lifetime erasure
//!
//! `SnapshotWithThreadMode<'db, DB>` borrows the DB. This implementation
//! transmutes the lifetime to `'static` so the snapshot can live alongside
//! the `Arc<OptimisticTransactionDB>` in the same struct.
//!
//! **Safety invariant**: `snap` is declared *before* `db` in the struct.
//! Rust drops fields in declaration order, so the snapshot is destroyed
//! before `db`'s `Arc` decrements its refcount.

use std::{collections::HashSet, sync::Arc};

use rocksdb::{Direction as ScanDir, IteratorMode, OptimisticTransactionDB, ReadOptions};

use crate::{
    store::{
        rocks::encoding::{
            build_full_edge, build_full_vertex, decode_edge_key, edge_scan_prefix, encode_edge_key, encode_vertex_key,
            prefix_upper_bound, EdgeValue, VertexValue, CF_EDGES_IN, CF_EDGES_OUT, CF_VERTICES,
        },
        traits::GraphSnapshot,
    },
    types::{Direction, Edge, EdgeKey, LabelId, StoreError, Vertex, VertexKey},
};

// ── Lifetime-erased RocksDB snapshot ─────────────────────────────────────────

type OwnedRocksSnap = rocksdb::SnapshotWithThreadMode<'static, OptimisticTransactionDB>;

/// # Safety
/// The caller must ensure `OwnedRocksSnap` is dropped before the
/// `Arc<OptimisticTransactionDB>` it was created from. The `RocksSnapshot`
/// struct guarantees this via field declaration order (`snap` before `db`).
fn pin_snapshot(db: &Arc<OptimisticTransactionDB>) -> OwnedRocksSnap {
    let snap: rocksdb::SnapshotWithThreadMode<'_, OptimisticTransactionDB> = db.snapshot();
    unsafe { std::mem::transmute(snap) }
}

// ── RocksSnapshot ─────────────────────────────────────────────────────────────

/// A read-only, point-in-time view of a `RocksStorage` database.
///
/// All reads are consistent with the state at the moment `snapshot()` was
/// called. Writes are not possible; use `Transaction` for mutations.
pub struct Snapshot {
    // IMPORTANT: `snap` must come before `db` — drop order is declaration order.
    snap: Option<OwnedRocksSnap>,
    db: Arc<OptimisticTransactionDB>,
}

impl Snapshot {
    pub(super) fn new(db: Arc<OptimisticTransactionDB>) -> Self {
        let snap = pin_snapshot(&db);
        Self { snap: Some(snap), db }
    }

    fn read_opts(&self) -> ReadOptions {
        let mut opts = ReadOptions::default();
        opts.set_snapshot(self.snap.as_ref().expect("snapshot still active"));
        opts
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // Explicitly drop snap before db's Arc decrements.
        self.snap.take();
    }
}

// ── GraphSnapshot ─────────────────────────────────────────────────────────────

impl GraphSnapshot for Snapshot {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, StoreError> {
        let cf = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let raw = self.db.get_cf_opt(&cf, encode_vertex_key(key), &self.read_opts()).map_err(StoreError::RocksDb)?;
        match raw {
            None => Ok(None),
            Some(bytes) => {
                let vv = VertexValue::decode(&bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                Ok(Some(build_full_vertex(key, &vv)?))
            }
        }
    }

    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<Edge>, StoreError> {
        let cf_name = match key.direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let raw = self.db.get_cf_opt(&cf, encode_edge_key(key), &self.read_opts()).map_err(StoreError::RocksDb)?;
        match raw {
            None => Ok(None),
            Some(bytes) => Ok(Some(build_full_edge(key, &EdgeValue::decode(&bytes))?)),
        }
    }

    fn get_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        label: Option<LabelId>,
        dst: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Result<Vec<Edge>, StoreError> {
        let cf_name = match direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let prefix = edge_scan_prefix(vertex, label);
        let mut opts = self.read_opts();
        opts.set_prefix_same_as_start(true);
        if let Some(upper) = prefix_upper_bound(&prefix) {
            opts.set_iterate_upper_bound(upper);
        }
        let dst_set: Option<HashSet<VertexKey>> = dst.map(|k| k.iter().copied().collect());
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let iter = self.db.iterator_cf_opt(&cf, opts, IteratorMode::From(&prefix, ScanDir::Forward));

        let mut result = Vec::new();
        for item in iter {
            let (key_bytes, val_bytes) = item.map_err(StoreError::RocksDb)?;
            let ek = decode_edge_key(&key_bytes, direction).ok_or(StoreError::CorruptData("edge key"))?;
            if let Some(ref set) = dst_set {
                if !set.contains(&ek.secondary_id) {
                    continue;
                }
            }
            result.push(build_full_edge(&ek, &EdgeValue::decode(&val_bytes))?);
            if let Some(max) = limit {
                if result.len() >= max as usize {
                    break;
                }
            }
        }
        Ok(result)
    }
}
