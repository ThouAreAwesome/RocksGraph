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
            build_lazy_edge, build_lazy_vertex, decode_edge_key, decode_vertex_key, edge_scan_prefix, encode_edge_key,
            encode_vertex_key, prefix_upper_bound, EdgeValue, VertexDegree, VertexValue, CF_EDGES_IN, CF_EDGES_OUT,
            CF_VERTEX_DEGREE, CF_VERTICES, EDGE_KEY_SIZE,
        },
        traits::GraphSnapshot,
    },
    types::{
        AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, Direction, Edge, EdgeKey, LabelId, Rank,
        StoreError, Vertex, VertexKey,
    },
};

// ── Lifetime-erased RocksDB snapshot ─────────────────────────────────────────

type OwnedRocksSnap = rocksdb::SnapshotWithThreadMode<'static, OptimisticTransactionDB>;

/// # Safety
/// The caller must ensure `OwnedRocksSnap` is dropped before the
/// `Arc<OptimisticTransactionDB>` it was created from. The `RocksSnapshot`
/// struct guarantees this via field declaration order (`snap` before `db`).
fn pin_snapshot(db: &Arc<OptimisticTransactionDB>) -> OwnedRocksSnap {
    let snap: rocksdb::SnapshotWithThreadMode<'_, OptimisticTransactionDB> = db.snapshot();
    // SAFETY: see module doc and this function's doc comment — `Snapshot` declares
    // `snap` before `db`, so `snap` always drops before the `Arc<OptimisticTransactionDB>`
    // it borrows from.
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

    #[inline]
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
                Ok(Some(build_lazy_vertex(key, &vv)))
            }
        }
    }

    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<Vertex>, StoreError> {
        let cf = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let db_keys: Vec<_> = keys.iter().map(|&k| (&cf, encode_vertex_key(k))).collect();
        let results = self.db.multi_get_cf_opt(db_keys, &self.read_opts());

        let mut out = Vec::with_capacity(keys.len());
        for (i, res) in results.into_iter().enumerate() {
            let bytes = res.map_err(StoreError::RocksDb)?;
            if let Some(bytes) = bytes {
                let vv = VertexValue::decode(&bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                out.push(build_lazy_vertex(keys[i], &vv));
            }
        }
        Ok(out)
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
            Some(bytes) => {
                let ev = EdgeValue::decode(&bytes).ok_or(StoreError::CorruptData("edge value"))?;
                Ok(Some(build_lazy_edge(key, &ev)))
            }
        }
    }

    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<Edge>, StoreError> {
        let cf_out = self.db.cf_handle(CF_EDGES_OUT).ok_or(StoreError::MissingColumnFamily(CF_EDGES_OUT))?;
        let cf_in = self.db.cf_handle(CF_EDGES_IN).ok_or(StoreError::MissingColumnFamily(CF_EDGES_IN))?;

        let db_keys: Vec<_> = keys
            .iter()
            .map(|k| {
                let cf = match k.direction {
                    Direction::OUT => &cf_out,
                    Direction::IN => &cf_in,
                };
                (cf, encode_edge_key(k))
            })
            .collect();

        let results = self.db.multi_get_cf_opt(db_keys, &self.read_opts());
        let mut out = Vec::with_capacity(keys.len());
        for (i, res) in results.into_iter().enumerate() {
            let bytes = res.map_err(StoreError::RocksDb)?;
            if let Some(bytes) = bytes {
                let ev = EdgeValue::decode(&bytes).ok_or(StoreError::CorruptData("edge value"))?;
                out.push(build_lazy_edge(&keys[i], &ev));
            }
        }
        Ok(out)
    }

    fn get_adjacent_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<Edge>, Option<AdjacentEdgeCursor>), StoreError> {
        let cf_name = match direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;

        let prefix = edge_scan_prefix(vertex, opts.label);
        let mut read_opts = self.read_opts();
        read_opts.set_prefix_same_as_start(true);
        if let Some(upper) = prefix_upper_bound(&prefix) {
            read_opts.set_iterate_upper_bound(upper.to_vec());
        }

        let seek_key = if let Some(cursor) = opts.start_from {
            let mut key = Vec::with_capacity(EDGE_KEY_SIZE);
            key.extend_from_slice(&encode_vertex_key(vertex));
            key.extend_from_slice(&cursor.label_id.to_be_bytes());
            key.extend_from_slice(&encode_vertex_key(cursor.secondary_id));
            key.extend_from_slice(&cursor.rank.to_be_bytes());
            key
        } else {
            prefix.clone().into_vec()
        };

        let dst_set: Option<HashSet<VertexKey>> = opts.dst.map(|k| k.iter().copied().collect());
        let rank_set: Option<HashSet<Rank>> = opts.rank.map(|r| r.iter().copied().collect());
        let iter = self.db.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&seek_key, ScanDir::Forward));

        let mut result = Vec::new();
        let mut first = true;

        for item in iter {
            let (key_bytes, val_bytes) = item.map_err(StoreError::RocksDb)?;
            if !key_bytes.starts_with(&prefix) {
                break;
            }
            let ek = decode_edge_key(&key_bytes, direction).ok_or(StoreError::CorruptData("edge key"))?;

            let current_cursor =
                AdjacentEdgeCursor { label_id: ek.label_id, secondary_id: ek.secondary_id, rank: ek.rank };

            // Seek-and-skip logic
            if first && opts.start_from.is_some() {
                first = false;
                if Some(current_cursor) == opts.start_from {
                    continue;
                }
            }

            // Apply filters
            if let Some(ref set) = dst_set {
                if !set.contains(&ek.secondary_id) {
                    continue;
                }
            }
            if let Some(ref set) = rank_set {
                if !set.contains(&ek.rank) {
                    continue;
                }
            }

            let ev = EdgeValue::decode(&val_bytes).ok_or(StoreError::CorruptData("edge value"))?;
            result.push(build_lazy_edge(&ek, &ev));
            if let Some(max) = limit {
                if result.len() >= max as usize {
                    break;
                }
            }
        }

        let next_cursor = if let Some(last_edge) = result.last() {
            if limit.map(|l| result.len() >= l as usize).unwrap_or(false) {
                Some(AdjacentEdgeCursor::from_edge(last_edge, direction))
            } else {
                None
            }
        } else {
            None
        };

        Ok((result, next_cursor))
    }

    fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<Vertex>, Option<VertexKey>), StoreError> {
        let cf = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        // Full-keyspace scan: must not be restricted to the seek key's prefix bucket.
        let mut read_opts = self.read_opts();
        read_opts.set_total_order_seek(true);

        let seek_key = if let Some(vk) = start_from { encode_vertex_key(vk).to_vec() } else { Vec::new() };

        let iter = self.db.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&seek_key, ScanDir::Forward));
        let mut result = Vec::new();
        let mut first = true;

        for item in iter {
            let (key_bytes, val_bytes) = item.map_err(StoreError::RocksDb)?;
            let key = decode_vertex_key(&key_bytes).ok_or(StoreError::CorruptData("vertex key"))?;

            // Seek-and-skip
            if first && start_from.is_some() {
                first = false;
                if Some(key) == start_from {
                    continue;
                }
            }

            let vv = VertexValue::decode(&val_bytes).ok_or(StoreError::CorruptData("vertex value"))?;

            // Apply label filter
            if let Some(lbl) = label {
                if vv.label_id != lbl {
                    continue;
                }
            }

            result.push(build_lazy_vertex(key, &vv));
            if result.len() >= limit as usize {
                break;
            }
        }

        let next_cursor = if result.len() >= limit as usize { result.last().map(|v| v.id) } else { None };

        Ok((result, next_cursor))
    }

    fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<Edge>, Option<CanonicalEdgeKey>), StoreError> {
        let cf = self.db.cf_handle(CF_EDGES_OUT).ok_or(StoreError::MissingColumnFamily("edges_out"))?;
        // `edges_out` has an 8-byte fixed prefix extractor (src_id) for outE()/inE() scans.
        // A full scan must disable prefix-restricted seeking, or pagination silently
        // truncates once the seek key falls on a different prefix bucket than the rest
        // of the keyspace (see scan_edges cursor below).
        let mut read_opts = self.read_opts();
        read_opts.set_total_order_seek(true);

        let seek_key = if let Some(cek) = start_from { encode_edge_key(&cek.out_key()).to_vec() } else { Vec::new() };

        let iter = self.db.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&seek_key, ScanDir::Forward));
        let mut result = Vec::new();
        let mut first = true;

        for item in iter {
            let (key_bytes, val_bytes) = item.map_err(StoreError::RocksDb)?;
            let ek = decode_edge_key(&key_bytes, Direction::OUT).ok_or(StoreError::CorruptData("edge key"))?;
            let current_cek = ek.canonical_edge_key();

            // Seek-and-skip
            if first && start_from.is_some() {
                first = false;
                if Some(current_cek) == start_from {
                    continue;
                }
            }

            // Apply label filter
            if let Some(lbl) = label {
                if current_cek.label_id != lbl {
                    continue;
                }
            }

            let ev = EdgeValue::decode(&val_bytes).ok_or(StoreError::CorruptData("edge value"))?;
            result.push(build_lazy_edge(&ek, &ev));
            if result.len() >= limit as usize {
                break;
            }
        }

        let next_cursor = if result.len() >= limit as usize { result.last().map(|e| e.canonical_key()) } else { None };

        Ok((result, next_cursor))
    }

    fn get_vertex_degree(&mut self, key: VertexKey) -> Result<Option<(u32, u32, LabelId)>, StoreError> {
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let raw =
            self.db.get_cf_opt(&cf_degree, encode_vertex_key(key), &self.read_opts()).map_err(StoreError::RocksDb)?;
        match raw {
            Some(bytes) => {
                let vd = VertexDegree::decode(&bytes).ok_or(StoreError::CorruptData("vertex degree"))?;
                Ok(Some((vd.out_e_cnt, vd.in_e_cnt, vd.vertex_label_id)))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        store::{
            traits::{GraphSnapshot, GraphStore, GraphTransaction},
            RocksStorage,
        },
        types::{AdjacentEdgesOptions, Direction, EdgeKey},
    };
    use tempfile::TempDir;

    fn open_temp_store() -> (RocksStorage, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path(), &Default::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_snapshot_repeatable_reads_all_scenarios() {
        let (store, _dir) = open_temp_store();

        // 1. Seed initial data
        let mut txn = store.begin();
        txn.put_vertex(1, 1, &std::collections::HashMap::new()).unwrap();
        txn.put_vertex(2, 1, &std::collections::HashMap::new()).unwrap();
        txn.put_vertex_degree(1, 1, 0, 0).unwrap();
        let ek_seed = EdgeKey::out_e(1, 10, 2, 0);
        txn.put_edge(&ek_seed, 0, &std::collections::HashMap::new()).unwrap();
        txn.commit().unwrap();

        // 2. Capture a DB snapshot
        let mut snap = store.snapshot();

        // 3. Concurrently modify the DB in a transaction and commit
        let mut txn2 = store.begin();
        txn2.put_vertex(3, 100, &std::collections::HashMap::new()).unwrap();
        txn2.put_vertex(1, 99, &std::collections::HashMap::new()).unwrap();
        txn2.put_vertex_degree(1, 1, 1, 0).unwrap();
        let ek_new = EdgeKey::out_e(1, 20, 3, 0);
        txn2.put_edge(&ek_new, 0, &std::collections::HashMap::new()).unwrap();
        txn2.commit().unwrap();

        // 4. Verify snapshot isolation (repeatable reads) for all GraphSnapshot read interfaces:

        // A. Point Vertex Reads (get_vertex / get_vertices)
        let v1 = snap.get_vertex(1).unwrap().unwrap();
        assert_eq!(v1.label_id, 1); // Should see original label 1, not 99
        let v3_opt = snap.get_vertex(3).unwrap();
        assert!(v3_opt.is_none()); // Vertex 3 should be invisible

        let batch_v = snap.get_vertices(&[1, 3]).unwrap();
        assert_eq!(batch_v.len(), 1);
        assert_eq!(batch_v[0].id, 1);
        assert_eq!(batch_v[0].label_id, 1);

        // B. Point Edge Reads (get_edge / get_edges)
        let e_seed = snap.get_edge(&ek_seed).unwrap();
        assert!(e_seed.is_some());
        let e_new = snap.get_edge(&ek_new).unwrap();
        assert!(e_new.is_none()); // New edge should be invisible

        let batch_e = snap.get_edges(&[ek_seed, ek_new]).unwrap();
        assert_eq!(batch_e.len(), 1);
        assert_eq!(batch_e[0].src_id, 1);
        assert_eq!(batch_e[0].dst_id, 2);

        // C. Adjacent Edges range scan (get_adjacent_edges)
        let (adj_edges, _) = snap
            .get_adjacent_edges(
                1,
                Direction::OUT,
                AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: None },
                None,
            )
            .unwrap();
        assert_eq!(adj_edges.len(), 1);
        assert_eq!(adj_edges[0].dst_id, 2); // Should not see edge to 3

        // D. Full vertices range scan (scan_vertices)
        let (vertices_scan, _) = snap.scan_vertices(None, None, 10).unwrap();
        let vertex_ids: Vec<_> = vertices_scan.iter().map(|v| v.id).collect();
        assert!(vertex_ids.contains(&1));
        assert!(vertex_ids.contains(&2));
        assert!(!vertex_ids.contains(&3)); // Should not see vertex 3

        // E. Full edges range scan (scan_edges)
        let (edges_scan, _) = snap.scan_edges(None, None, 10).unwrap();
        assert_eq!(edges_scan.len(), 1);
        assert_eq!(edges_scan[0].dst_id, 2); // Should not see edge to 3
    }

    /// Regression test: `edges_out` has a fixed 8-byte (src_id) prefix extractor for
    /// `outE()`/`inE()` scans. Paginated `scan_edges` re-seeks every page after the first
    /// from a full edge key, which *is* within the prefix extractor's domain. Without
    /// `total_order_seek(true)`, RocksDB silently stops the scan once it runs out of keys
    /// sharing that one src_id's prefix bucket, even though edges with other src_ids exist
    /// further in the column family. Each edge below has a distinct src_id, so a page size
    /// smaller than the total count forces a re-seek across prefix boundaries on every page.
    ///
    /// The cutoff only manifests once a seek key's prefix is absent from some *other*,
    /// already-flushed SST file: RocksDB's prefix bloom filter then excludes that file
    /// from the merge iterator entirely, hiding every later src_id it holds. Each src
    /// range below is written and flushed separately so they land in distinct SST files,
    /// and the seek key used for each page's re-seek comes from a *different* file than
    /// the data it needs to find next.
    #[test]
    fn test_scan_edges_paginates_across_src_id_prefixes() {
        let (store, _dir) = open_temp_store();
        let cf = store.db.cf_handle(super::CF_EDGES_OUT).unwrap();

        // A single SST file per src_id isn't enough to trigger the cutoff reliably (RocksDB's
        // file-level bloom-filter exclusion needs enough separate files for a re-seek to land
        // on one that doesn't hold the next src_id); 50 reproduces it deterministically.
        let src_ids: Vec<i64> = (1..=50).collect();
        for &src in &src_ids {
            let mut txn = store.begin();
            txn.put_edge(&EdgeKey::out_e(src, 10, 100, 0), 0, &std::collections::HashMap::new()).unwrap();
            txn.commit().unwrap();
            store.db.flush_cf(&cf).unwrap();
        }

        let mut snap = store.snapshot();
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let (page, next) = snap.scan_edges(None, cursor, 5).unwrap();
            if page.is_empty() {
                break;
            }
            seen.extend(page.iter().map(|e| e.src_id));
            if next.is_none() {
                break;
            }
            cursor = next;
        }

        seen.sort_unstable();
        assert_eq!(seen, src_ids);
    }
}
