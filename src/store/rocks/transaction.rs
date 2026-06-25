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

//! Thin RocksDB transaction adapter.
//!
//! # Responsibility
//!
//! `Transaction` is a pure I/O layer: it encodes and decodes graph elements
//! to/from RocksDB bytes and stages reads/writes on an `OptimisticTransactionDB`
//! handle.  All overlay logic (dirty tracking, query-scoped caching, key
//! allocation) lives in [`LogicalGraph`], one layer above.
//!
//! # Read path
//!
//! `get_vertex` uses `GetForUpdate` to enrol the key in the OCC read-set.
//! Point edge reads (`get_edge`) also use `GetForUpdate`. Edge scans (`get_edges`)
//! use snapshot scans; their write-write conflicts are detected automatically
//! by the OCC at commit time because any modified edge key is in the write-set.
//!
//! # Write path
//!
//! This layer is physically pure: `put_edge` and `delete_edge` write or
//! delete exactly one record (either `CF_EDGES_OUT` or `CF_EDGES_IN`). Graph
//! consistency logic (like ensuring forward and backward edges exist) is entirely
//! deferred to `LogicalGraph`. All staged operations are flushed atomically by `commit`.
//!
//! # Lifetime erasure
//!
//! `rocksdb::Transaction<'_, OptimisticTransactionDB>` borrows the DB.
//! This implementation transmutes the lifetime to `'static` so the transaction can live
//! alongside `Arc<OptimisticTransactionDB>` in the same struct.
//!
//! **Safety invariant**: `db_txn` is declared *before* `db` in the struct.
//! Rust drops fields in declaration order, so `db_txn` is always destroyed
//! before `db`'s `Arc` decrements its refcount.  The `Arc` ensures the
//! underlying `OptimisticTransactionDB` is alive for the entire duration of
//! both fields.

use std::{collections::HashSet, sync::Arc};

use rocksdb::{Direction as ScanDir, IteratorMode, OptimisticTransactionDB, ReadOptions};

use crate::{
    store::{
        rocks::encoding::{
            build_lazy_edge, build_lazy_vertex, decode_edge_key, decode_vertex_key, edge_scan_prefix, encode_edge_key,
            encode_props, encode_schema_key, encode_vertex_key, prefix_upper_bound, EdgeValue, VertexDegree,
            VertexValue, CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VERTEX_DEGREE, CF_VERTICES,
        },
        traits::GraphTransaction,
    },
    types::{
        element::Property, AdjacentEdgeCursor, AdjacentEdgesOptions, CanonicalEdgeKey, Direction, Edge, EdgeKey,
        LabelId, Rank, StoreError, Vertex, VertexKey,
    },
};

// ── Lifetime-erased RocksDB transaction ──────────────────────────────────────

type OwnedRocksTxn = rocksdb::Transaction<'static, OptimisticTransactionDB>;
type OwnedRocksTxnSnap = rocksdb::SnapshotWithThreadMode<'static, OwnedRocksTxn>;

/// Create a new optimistic transaction, erasing the `'db` lifetime.
///
/// # Safety
///
/// The caller must ensure the returned `OwnedRocksTxn` is dropped before the `Arc<OptimisticTransactionDB>`
/// it was created from. In the `Transaction` struct, this is guaranteed by field declaration order
/// (`db_txn` is declared before `db`).
fn begin_txn(db: &Arc<OptimisticTransactionDB>) -> OwnedRocksTxn {
    let mut tx_opts = rocksdb::OptimisticTransactionOptions::default();
    tx_opts.set_snapshot(true);
    let txn = db.transaction_opt(&rocksdb::WriteOptions::default(), &tx_opts);
    // SAFETY: see module doc and function safety note.
    unsafe { std::mem::transmute(txn) }
}

// ── Transaction ───────────────────────────────────────────────────────────────

/// A wrapper around `rocksdb::Transaction` that manages its lifetime and provides `GraphTransaction` capabilities.
pub struct Transaction {
    // IMPORTANT: Drop order is declaration order.
    // `db_txn_snap` borrows `db_txn`, so it must be declared first.
    db_txn_snap: Option<OwnedRocksTxnSnap>,
    db_txn: Option<OwnedRocksTxn>,
    db: Arc<OptimisticTransactionDB>,
}

impl Drop for Transaction {
    fn drop(&mut self) {
        self.db_txn_snap.take();
        // Ensures that if the transaction is dropped without an explicit commit or abort, it is rolled back.
        if let Some(txn) = self.db_txn.take() {
            let _ = txn.rollback();
        }
        // `db_txn` is now None; the `Arc<OTD>` in `db` drops after this.
    }
}

impl Transaction {
    /// Creates a new `Transaction` instance, initiating an optimistic RocksDB transaction.
    #[allow(clippy::missing_transmute_annotations)]
    pub fn new(db: Arc<OptimisticTransactionDB>) -> Self {
        let db_txn = begin_txn(&db);
        let snap = db_txn.snapshot();
        let db_txn_snap = unsafe { std::mem::transmute(snap) };
        Self { db_txn_snap: Some(db_txn_snap), db_txn: Some(db_txn), db }
    }

    #[inline]
    fn read_opts(&self) -> ReadOptions {
        let mut opts = ReadOptions::default();
        if let Some(ref snap) = self.db_txn_snap {
            opts.set_snapshot(snap);
        }
        opts
    }
}

// ── GraphTransaction ──────────────────────────────────────────────────────────

impl GraphTransaction for Transaction {
    /// Retrieves a vertex by its key, enrolling it in the OCC read-set.
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let vv_raw = self
            .db_txn
            .as_ref()
            .expect("no active transaction")
            .get_for_update_cf_opt(&cf_vertices, encode_vertex_key(key), true, &self.read_opts())
            .map_err(StoreError::RocksDb)?;

        match vv_raw {
            Some(vv_bytes) => {
                let vv = VertexValue::decode(&vv_bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                Ok(Some(build_lazy_vertex(key, &vv)))
            }
            _ => Ok(None),
        }
    }

    /// Retrieves the degree (in-edges, out-edges) of a vertex, enrolling it in the OCC read-set.
    fn get_vertex_degree(&mut self, key: VertexKey) -> Result<Option<(u32, u32)>, StoreError> {
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let vd_raw = self
            .db_txn
            .as_ref()
            .expect("no active transaction")
            .get_for_update_cf_opt(&cf_degree, encode_vertex_key(key), true, &self.read_opts())
            .map_err(StoreError::RocksDb)?;
        match vd_raw {
            Some(vd_bytes) => {
                let vd = VertexDegree::decode(&vd_bytes).ok_or(StoreError::CorruptData("vertex degree"))?;
                Ok(Some((vd.out_e_cnt, vd.in_e_cnt)))
            }
            _ => Ok(None),
        }
    }

    /// Retrieves a single edge by its key, enrolling it in the OCC read-set.
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<Edge>, StoreError> {
        let cf_name = match key.direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let key_bytes = encode_edge_key(key);
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let raw_opt = self
            .db_txn
            .as_ref()
            .expect("no active transaction")
            .get_for_update_cf_opt(&cf, key_bytes, false, &self.read_opts())
            .map_err(StoreError::RocksDb)?;

        match raw_opt {
            None => Ok(None),
            Some(raw) => Ok(Some(build_lazy_edge(key, &EdgeValue::decode(&raw)))),
        }
    }

    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<Vertex>, StoreError> {
        let cf = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let mut out = Vec::with_capacity(keys.len());
        for &k in keys {
            let vv_raw = txn
                .get_for_update_cf_opt(&cf, encode_vertex_key(k), true, &self.read_opts())
                .map_err(StoreError::RocksDb)?;
            if let Some(bytes) = vv_raw {
                let vv = VertexValue::decode(&bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                out.push(build_lazy_vertex(k, &vv));
            }
        }
        Ok(out)
    }

    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<Edge>, StoreError> {
        let cf_out = self.db.cf_handle(CF_EDGES_OUT).ok_or(StoreError::MissingColumnFamily(CF_EDGES_OUT))?;
        let cf_in = self.db.cf_handle(CF_EDGES_IN).ok_or(StoreError::MissingColumnFamily(CF_EDGES_IN))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            let cf = match key.direction {
                Direction::OUT => &cf_out,
                Direction::IN => &cf_in,
            };
            let raw = txn
                .get_for_update_cf_opt(cf, encode_edge_key(key), false, &self.read_opts())
                .map_err(StoreError::RocksDb)?;
            if let Some(bytes) = raw {
                out.push(build_lazy_edge(key, &EdgeValue::decode(&bytes)));
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
        let prefix = edge_scan_prefix(vertex, opts.label);
        let mut read_opts = self.read_opts();
        read_opts.set_prefix_same_as_start(true);
        if let Some(upper) = prefix_upper_bound(&prefix) {
            read_opts.set_iterate_upper_bound(upper.to_vec());
        }

        let seek_key = if let Some(cursor) = opts.start_from {
            let mut key = Vec::with_capacity(20);
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
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let iter = txn.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&seek_key, ScanDir::Forward));

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

            result.push(build_lazy_edge(&ek, &EdgeValue::decode(&val_bytes)));
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

        let txn = self.db_txn.as_ref().expect("no active transaction");
        let iter = txn.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&seek_key, ScanDir::Forward));
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
        // of the keyspace.
        let mut read_opts = self.read_opts();
        read_opts.set_total_order_seek(true);

        let seek_key = if let Some(cek) = start_from { encode_edge_key(&cek.out_key()).to_vec() } else { Vec::new() };

        let txn = self.db_txn.as_ref().expect("no active transaction");
        let iter = txn.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&seek_key, ScanDir::Forward));
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

            result.push(build_lazy_edge(&ek, &EdgeValue::decode(&val_bytes)));
            if result.len() >= limit as usize {
                break;
            }
        }

        let next_cursor = if result.len() >= limit as usize { result.last().map(|e| e.canonical_key()) } else { None };

        Ok((result, next_cursor))
    }

    /// Inserts or updates a vertex record with its label and properties.
    fn put_vertex(&mut self, key: VertexKey, label_id: LabelId, props: &[Property]) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let vv = VertexValue { label_id, property_blob: encode_props(props) };
        txn.put_cf(&cf_vertices, encode_vertex_key(key), vv.encode()).map_err(StoreError::RocksDb)
    }

    /// Inserts or updates the degree counts for a vertex.
    fn put_vertex_degree(&mut self, key: VertexKey, out_e_cnt: u32, in_e_cnt: u32) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let vd = VertexDegree { out_e_cnt, in_e_cnt };
        txn.put_cf(&cf_degree, encode_vertex_key(key), vd.encode()).map_err(StoreError::RocksDb)
    }

    /// Inserts or updates a single edge record (either `edges_out` or `edges_in`).
    fn put_edge(&mut self, key: &EdgeKey, props: &[Property]) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let cf_name = match key.direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let key_bytes = encode_edge_key(key);
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let ev_bytes = EdgeValue { property_blob: encode_props(props) }.encode().to_vec();
        txn.put_cf(&cf, key_bytes, &ev_bytes).map_err(StoreError::RocksDb)
    }

    /// Stage a schema key-value entry for persistence.
    fn put_schema_entry(&mut self, kind: u8, name: &str, value: &[u8]) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let cf_schema = self.db.cf_handle(CF_SCHEMA).ok_or(StoreError::MissingColumnFamily(CF_SCHEMA))?;
        let key = encode_schema_key(kind, name);
        txn.put_cf(&cf_schema, key, value).map_err(StoreError::RocksDb)
    }

    /// Deletes a vertex record.
    fn delete_vertex(&mut self, key: VertexKey) -> Result<(), StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        txn.delete_cf(&cf_vertices, encode_vertex_key(key)).map_err(StoreError::RocksDb)
    }

    /// Deletes a vertex degree record.
    fn delete_vertex_degree(&mut self, key: VertexKey) -> Result<(), StoreError> {
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        txn.delete_cf(&cf_degree, encode_vertex_key(key)).map_err(StoreError::RocksDb)
    }

    /// Deletes a single edge record from the appropriate column family.
    fn delete_edge(&mut self, key: &EdgeKey) -> Result<(), StoreError> {
        let cf_name = match key.direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let key_bytes = encode_edge_key(key);
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        txn.delete_cf(&cf, key_bytes).map_err(StoreError::RocksDb)
    }

    /// Attempts to commit the transaction. Returns `StoreError::Conflict` on OCC failure.
    ///
    /// # Reuse
    /// Calling `commit` automatically resets this transaction instance, spawning a fresh
    /// RocksDB transaction and snapshot. This allows reusing the same `Transaction`
    /// object for subsequent operations.
    #[allow(clippy::missing_transmute_annotations)]
    fn commit(&mut self) -> Result<(), StoreError> {
        self.db_txn_snap.take();
        let txn = self.db_txn.take().expect("no active transaction");
        let result = txn.commit().map_err(|e| {
            if e.to_string().contains("Resource busy") {
                StoreError::Conflict
            } else {
                StoreError::RocksDb(e)
            }
        });
        let new_txn = begin_txn(&self.db);
        self.db_txn = Some(new_txn);
        let snap = self.db_txn.as_ref().unwrap().snapshot();
        self.db_txn_snap = Some(unsafe { std::mem::transmute(snap) });
        result
    }

    /// Rolls back the transaction, discarding all staged writes.
    ///
    /// # Reuse
    /// Calling `abort` automatically resets this transaction instance, spawning a fresh
    /// RocksDB transaction and snapshot. This allows reusing the same `Transaction`
    /// object for subsequent operations.
    #[allow(clippy::missing_transmute_annotations)]
    fn abort(&mut self) {
        self.db_txn_snap.take();
        if let Some(txn) = self.db_txn.take() {
            let _ = txn.rollback();
        }
        let new_txn = begin_txn(&self.db);
        self.db_txn = Some(new_txn);
        let snap = self.db_txn.as_ref().unwrap().snapshot();
        self.db_txn_snap = Some(unsafe { std::mem::transmute(snap) });
    }
}

// ── Test coverage summary ─────────────────────────────────────────────────────
//
// Each row maps a `GraphTransaction` method to the test(s) that cover it.
//
// | Method                | Tests                                                    |
// |-----------------------|----------------------------------------------------------|
// | get_vertex            | test_put_and_get_vertex                                  |
// |                       | test_put_and_get_vertex_with_properties                  |
// | put_vertex            | test_put_and_get_vertex                                  |
// |                       | test_put_vertex_overwrite (overwrites existing record)   |
// | get_vertex_degree     | test_put_and_get_vertex_degree                           |
// | put_vertex_degree     | test_put_and_get_vertex_degree                           |
// | get_edge              | test_put_and_get_edge                                    |
// |                       | test_put_and_get_edge_with_properties                    |
// | put_edge              | test_put_and_get_edge                                    |
// |                       | test_put_edge_overwrite (overwrites existing record)     |
// | get_edges             | test_get_edges                                           |
// |   OUT: no filter      |   - no filter, label, dst, limit, negative vertex ID     |
// |   IN: no filter       |   - no filter                                            |
// |   IN: all filters     | test_get_edges_in_direction_filters                      |
// |                       |   - label, limit, src, combined label+src                |
// | put_schema_entry      | test_put_schema_entry                                    |
// | delete_vertex         | test_delete_vertex (positive and negative IDs)           |
// | delete_vertex_degree  | test_delete_vertex                                       |
// | delete_edge           | test_delete_edge (positive and negative IDs)             |
// |                       | test_edges_with_nonzero_rank (rank-specific delete)      |
// | commit                | test_commit_and_abort (success path)                     |
// |                       | test_commit_returns_conflict (OCC conflict → Conflict)   |
// | abort                 | test_commit_and_abort (staged writes discarded)          |
// | non-zero rank         | test_edges_with_nonzero_rank                             |
// |                       |   - distinct keys, scan, point-delete                    |

#[cfg(test)]
mod tests {
    use rocksdb::{DBCommon, OptimisticTransactionDB, Options, SingleThreaded, DB};
    use smol_str::SmolStr;

    use crate::{
        store::{
            traits::{GraphStore, GraphTransaction},
            RocksStorage,
        },
        types::{
            AdjacentEdgesOptions, CanonicalKey, Direction, Edge, EdgeKey, LabelId, Primitive, Property, Vertex,
            VertexKey,
        },
    };
    /// This test simulates a read-write conflict between two transactions (`txn1` and `txn2`) on the same keys in a
    /// RocksDB database using `OptimisticTransactionDB`. The test verifies that if `txn2` commits first after
    /// modifying a key that `txn1` has read, then `txn1` should fail to commit due to a conflict.
    #[test]
    fn test_read_write_conflict() {
        let dir = tempfile::tempdir().unwrap();
        // fix this
        let db: DBCommon<SingleThreaded, _> = OptimisticTransactionDB::open_default(dir.path()).unwrap();

        let txn = db.transaction();
        // Seed initial values
        txn.put(b"Key_A", b"initial_A").unwrap();
        txn.put(b"Key_B", b"initial_B").unwrap();
        txn.commit().unwrap();

        let snapshot = false; // Required to track read/write conflict baselines

        // ==========================================
        // STEP-BY-STEP TIMELINE EXECUTION
        // ==========================================

        // Time 0: txn1 begins
        let txn1 = db.transaction();
        println!("[Time 0] txn1 started.");

        // Time 1: txn2 begins
        let txn2 = db.transaction();
        println!("[Time 1] txn2 started.");

        // Time 2: txn1 reads A & B, modifies B
        let _ = txn1.get_for_update(b"Key_A", snapshot).unwrap();
        let _ = txn1.get_for_update(b"Key_B", snapshot).unwrap();
        txn1.put(b"Key_B", b"new_value_1").unwrap();
        println!("[Time 2] txn1 executed GetForUpdate(A, B) and Put(B).");

        // Time 3: txn2 reads A, modifies A
        let _ = txn2.get_for_update(b"Key_A", snapshot).unwrap();
        txn2.put(b"Key_A", b"new_value_2").unwrap();
        println!("[Time 3] txn2 executed GetForUpdate(A) and Put(A).");

        // Scenario B: txn2 races ahead and commits first
        println!("\n--- Entering Commit Phase (Scenario B) ---");

        assert!(txn2.commit().is_ok(), "[Result] txn2 failed to commit! (Unexpected)");
        print!("[Result] txn2 committed successfully. ");

        assert!(txn1.commit().is_err(), "[Result] txn1 successfully committed! (Unexpected)");
        print!("[Result] txn1 failed to commit as expected due to conflict.");
        // Clean up
        let _ = DB::destroy(&Options::default(), dir.path());
    }

    // Helper function to open a temporary RocksDB store
    fn open_temp_store() -> (RocksStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        (store, dir)
    }

    // Helper to get a new transaction from the store
    fn ctx(store: &RocksStorage) -> super::Transaction {
        // Begins a new `Transaction` for this `RocksStorage`.
        store.begin()
    }

    fn get_adjacent_edges_test(
        txn: &mut super::Transaction,
        vertex: VertexKey,
        direction: Direction,
        label: Option<LabelId>,
        dst: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Vec<Edge> {
        txn.get_adjacent_edges(
            vertex,
            direction,
            AdjacentEdgesOptions { label, dst, rank: None, start_from: None },
            limit,
        )
        .unwrap()
        .0
    }

    // Helper to create a simple vertex
    fn create_test_vertex(id: i64, label_id: u16) -> Vertex {
        Vertex::with_props(id, label_id, vec![])
    }

    // Helper to create a simple edge
    fn create_test_edge(src: i64, label: u16, dst: i64, _dir: Direction) -> Edge {
        Edge::with_props(src, label, dst, 0, vec![])
    }

    #[test]
    fn test_put_and_get_vertex() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        // Test with positive ID
        let mut v_pos = create_test_vertex(1, 100);
        txn.put_vertex(v_pos.id, v_pos.label_id, v_pos.all_props()).unwrap();
        let fetched_v_pos = txn.get_vertex(v_pos.id).unwrap().unwrap();
        assert_eq!(fetched_v_pos.id, v_pos.id);
        assert_eq!(fetched_v_pos.label_id, v_pos.label_id);

        // Test with negative ID
        let mut v_neg = create_test_vertex(-2, 200);
        txn.put_vertex(v_neg.id, v_neg.label_id, v_neg.all_props()).unwrap();
        let fetched_v_neg = txn.get_vertex(v_neg.id).unwrap().unwrap();
        assert_eq!(fetched_v_neg.id, v_neg.id);
        assert_eq!(fetched_v_neg.label_id, v_neg.label_id);

        // Test non-existent vertex
        assert!(txn.get_vertex(999).unwrap().is_none());
    }

    #[test]
    fn test_put_and_get_vertex_degree() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        // Test with positive ID
        txn.put_vertex_degree(1, 5, 10).unwrap();
        let (out_pos, in_pos) = txn.get_vertex_degree(1).unwrap().unwrap();
        assert_eq!(out_pos, 5);
        assert_eq!(in_pos, 10);

        // Test with negative ID
        txn.put_vertex_degree(-2, 15, 20).unwrap();
        let (out_neg, in_neg) = txn.get_vertex_degree(-2).unwrap().unwrap();
        assert_eq!(out_neg, 15);
        assert_eq!(in_neg, 20);

        // Test non-existent vertex degree
        assert!(txn.get_vertex_degree(999).unwrap().is_none());
    }

    #[test]
    fn test_put_schema_entry() {
        use crate::store::rocks::encoding::{encode_schema_key, CF_SCHEMA, SCHEMA_KIND_VERTEX_LABEL};

        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        txn.put_schema_entry(SCHEMA_KIND_VERTEX_LABEL, "person", &7u16.to_be_bytes()).unwrap();
        txn.commit().unwrap();

        // No `get_schema_entry` on the trait -- the schema CF is read in bulk by
        // `RocksStorage::load_schema`, not per-transaction -- so verify the write
        // landed by reading the column family directly.
        let cf = store.db.cf_handle(CF_SCHEMA).unwrap();
        let key = encode_schema_key(SCHEMA_KIND_VERTEX_LABEL, "person");
        let value = store.db.get_cf(&cf, key).unwrap().unwrap();
        assert_eq!(value, 7u16.to_be_bytes());
    }

    #[test]
    fn test_put_and_get_edge() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        // Test with positive IDs
        let ek_pos = EdgeKey::out_e(1, 100, 2, 0);
        let mut e_pos = create_test_edge(1, 100, 2, Direction::OUT);
        txn.put_edge(&ek_pos, e_pos.all_props()).unwrap();
        let fetched_e_pos = txn.get_edge(&ek_pos).unwrap().unwrap();
        assert_eq!(fetched_e_pos.src_id, ek_pos.primary_id);
        assert_eq!(fetched_e_pos.dst_id, ek_pos.secondary_id);

        // Test with negative IDs
        let ek_neg = EdgeKey::in_e(-3, 200, -4, 0);
        let mut e_neg = create_test_edge(-3, 200, -4, Direction::IN);
        txn.put_edge(&ek_neg, e_neg.all_props()).unwrap();
        let fetched_e_neg = txn.get_edge(&ek_neg).unwrap().unwrap();
        assert_eq!(fetched_e_neg.src_id, ek_neg.secondary_id); // For IN edge, primary_id is dst, secondary_id is src
        assert_eq!(fetched_e_neg.dst_id, ek_neg.primary_id);

        // Test non-existent edge
        let non_existent_ek = EdgeKey::out_e(999, 1, 1000, 0);
        assert!(txn.get_edge(&non_existent_ek).unwrap().is_none());
    }

    #[test]
    fn test_get_edges() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        // Add some vertices
        txn.put_vertex(1, 1, &[]).unwrap();
        txn.put_vertex(2, 1, &[]).unwrap();
        txn.put_vertex(3, 1, &[]).unwrap();
        txn.put_vertex(-1, 1, &[]).unwrap();
        txn.put_vertex(-2, 1, &[]).unwrap();

        // Add some edges
        txn.put_edge(&EdgeKey::out_e(1, 10, 2, 0), &[]).unwrap(); // 1 --10--> 2
        txn.put_edge(&EdgeKey::out_e(1, 10, 3, 0), &[]).unwrap(); // 1 --10--> 3
        txn.put_edge(&EdgeKey::out_e(1, 20, 2, 0), &[]).unwrap(); // 1 --20--> 2
        txn.put_edge(&EdgeKey::in_e(1, 10, 2, 0), &[]).unwrap(); // 1 --10--> 2 (in-direction for 2)
        txn.put_edge(&EdgeKey::in_e(1, 20, 2, 0), &[]).unwrap(); // 1 --20--> 2 (in-direction for 2)
        txn.put_edge(&EdgeKey::out_e(-1, 30, -2, 0), &[]).unwrap(); // -1 --30--> -2

        // Test get_edges with positive vertex ID, OUT direction, no filters
        let edges = get_adjacent_edges_test(&mut txn, 1, Direction::OUT, None, None, None);
        assert_eq!(edges.len(), 3);

        // Test get_edges with positive vertex ID, OUT direction, label filter
        let edges_label_10 = get_adjacent_edges_test(&mut txn, 1, Direction::OUT, Some(10), None, None);
        assert_eq!(edges_label_10.len(), 2);
        assert!(edges_label_10.iter().all(|e| e.label_id == 10));

        // Test get_edges with positive vertex ID, OUT direction, destination filter
        let edges_dst_2 = get_adjacent_edges_test(&mut txn, 1, Direction::OUT, None, Some(&[2]), None);
        assert_eq!(edges_dst_2.len(), 2); // Edges (1,10,2) and (1,20,2)

        // Test get_edges with positive vertex ID, OUT direction, limit
        let edges_limit_1 = get_adjacent_edges_test(&mut txn, 1, Direction::OUT, None, None, Some(1));
        assert_eq!(edges_limit_1.len(), 1);

        // Test get_edges with negative vertex ID, OUT direction, no filters
        let edges_neg = get_adjacent_edges_test(&mut txn, -1, Direction::OUT, None, None, None);
        assert_eq!(edges_neg.len(), 1);
        assert_eq!(edges_neg[0].src_id, -1);
        assert_eq!(edges_neg[0].dst_id, -2);

        // Test get_edges with IN direction
        let edges_in_2 = get_adjacent_edges_test(&mut txn, 2, Direction::IN, None, None, None);
        assert_eq!(edges_in_2.len(), 2); // Edges from 1 to 2
    }

    #[test]
    fn test_delete_vertex() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        // Add and delete positive ID vertex
        let mut v_pos = create_test_vertex(1, 100);
        txn.put_vertex(v_pos.id, v_pos.label_id, v_pos.all_props()).unwrap();
        txn.put_vertex_degree(v_pos.id, 0, 0).unwrap();
        assert!(txn.get_vertex(v_pos.id).unwrap().is_some());
        txn.delete_vertex(v_pos.id).unwrap();
        txn.delete_vertex_degree(v_pos.id).unwrap();
        assert!(txn.get_vertex(v_pos.id).unwrap().is_none());
        assert!(txn.get_vertex_degree(v_pos.id).unwrap().is_none());

        // Add and delete negative ID vertex
        let mut v_neg = create_test_vertex(-2, 200);
        txn.put_vertex(v_neg.id, v_neg.label_id, v_neg.all_props()).unwrap();
        txn.put_vertex_degree(v_neg.id, 0, 0).unwrap();
        assert!(txn.get_vertex(v_neg.id).unwrap().is_some());
        txn.delete_vertex(v_neg.id).unwrap();
        txn.delete_vertex_degree(v_neg.id).unwrap();
        assert!(txn.get_vertex(v_neg.id).unwrap().is_none());
        assert!(txn.get_vertex_degree(v_neg.id).unwrap().is_none());
    }

    #[test]
    fn test_delete_edge() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);
        // Add and delete positive ID edge
        let ek_pos = EdgeKey::out_e(1, 100, 2, 0);
        let mut e_pos = create_test_edge(1, 100, 2, Direction::OUT);
        txn.put_edge(&ek_pos, e_pos.all_props()).unwrap();
        assert!(txn.get_edge(&ek_pos).unwrap().is_some());
        txn.delete_edge(&ek_pos).unwrap();
        assert!(txn.get_edge(&ek_pos).unwrap().is_none());

        // Add and delete negative ID edge
        let ek_neg = EdgeKey::in_e(-3, 200, -4, 0);
        let mut e_neg = create_test_edge(-3, 200, -4, Direction::IN);
        txn.put_edge(&ek_neg, e_neg.all_props()).unwrap();
        assert!(txn.get_edge(&ek_neg).unwrap().is_some());
        txn.delete_edge(&ek_neg).unwrap();
        assert!(txn.get_edge(&ek_neg).unwrap().is_none());
    }

    #[test]
    fn test_commit_and_abort() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        // Test commit
        let mut v1 = create_test_vertex(1, 1);
        txn.put_vertex(v1.id, v1.label_id, v1.all_props()).unwrap();
        txn.commit().unwrap();

        // Verify committed data in a new transaction from the same store
        let mut new_txn = ctx(&store);
        assert!(new_txn.get_vertex(v1.id).unwrap().is_some());

        // Test abort
        let mut v2 = create_test_vertex(2, 2);
        txn.put_vertex(v2.id, v2.label_id, v2.all_props()).unwrap();
        txn.abort();
        // After abort, txn is reset, so we can use it again.
        // Verify aborted data is not present
        let mut new_txn_after_abort = ctx(&store);
        assert!(new_txn_after_abort.get_vertex(v2.id).unwrap().is_none());

        // Verify previously committed data is still there after abort
        assert!(new_txn_after_abort.get_vertex(v1.id).unwrap().is_some());
    }

    #[test]
    fn test_put_and_get_vertex_with_properties() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        let v1_id = 1;
        let v1_label = 10;
        let props = vec![
            Property { owner: CanonicalKey::Vertex(v1_id), key: 1, value: Primitive::String(SmolStr::new("Alice")) },
            Property { owner: CanonicalKey::Vertex(v1_id), key: 2, value: Primitive::Int32(30) },
        ];

        txn.put_vertex(v1_id, v1_label, &props).unwrap();
        let mut fetched_v = txn.get_vertex(v1_id).unwrap().unwrap();

        assert_eq!(fetched_v.id, v1_id);
        assert_eq!(fetched_v.label_id, v1_label);
        let fetched_props = fetched_v.all_props();
        assert_eq!(fetched_props.len(), 2);
        assert!(fetched_props.contains(&props[0]));
        assert!(fetched_props.contains(&props[1]));
    }

    // ── Gap coverage ─────────────────────────────────────────────────────────

    #[test]
    fn test_get_edges_in_direction_filters() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        // Store in-edges for vertex 5: two with label 10, one with label 20
        txn.put_edge(&EdgeKey::in_e(1, 10, 5, 0), &[]).unwrap(); // 1 --10--> 5
        txn.put_edge(&EdgeKey::in_e(2, 10, 5, 0), &[]).unwrap(); // 2 --10--> 5
        txn.put_edge(&EdgeKey::in_e(3, 20, 5, 0), &[]).unwrap(); // 3 --20--> 5

        // Label filter on IN direction
        let by_label = get_adjacent_edges_test(&mut txn, 5, Direction::IN, Some(10), None, None);
        assert_eq!(by_label.len(), 2);
        assert!(by_label.iter().all(|e| e.label_id == 10));

        // Limit on IN direction
        let limited = get_adjacent_edges_test(&mut txn, 5, Direction::IN, None, None, Some(2));
        assert_eq!(limited.len(), 2);

        // Src filter on IN direction: secondary_id is the source vertex
        let by_src = get_adjacent_edges_test(&mut txn, 5, Direction::IN, None, Some(&[2, 3]), None);
        assert_eq!(by_src.len(), 2);
        assert!(by_src.iter().all(|e| e.src_id == 2 || e.src_id == 3));

        // Combined label + src filter
        let combined = get_adjacent_edges_test(&mut txn, 5, Direction::IN, Some(10), Some(&[2]), None);
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].src_id, 2);
        assert_eq!(combined[0].label_id, 10);
    }

    #[test]
    fn test_commit_returns_conflict() {
        let (store, _dir) = open_temp_store();

        // Seed a vertex that both transactions will read
        let mut seed = ctx(&store);
        seed.put_vertex(42, 1, &[]).unwrap();
        seed.commit().unwrap();

        // txn1 reads vertex 42 (enrolls it in its OCC read-set)
        let mut txn1 = ctx(&store);
        txn1.get_vertex(42).unwrap();

        // txn2 overwrites vertex 42 and commits first
        let mut txn2 = ctx(&store);
        txn2.put_vertex(42, 2, &[]).unwrap();
        txn2.commit().unwrap();

        // txn1 now writes and tries to commit — must see Conflict
        txn1.put_vertex(42, 3, &[]).unwrap();
        assert!(matches!(txn1.commit(), Err(crate::types::StoreError::Conflict)));
    }

    #[test]
    fn test_put_vertex_overwrite() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        txn.put_vertex(7, 1, &[]).unwrap();
        let first = txn.get_vertex(7).unwrap().unwrap();
        assert_eq!(first.label_id, 1);

        txn.put_vertex(7, 99, &[]).unwrap();
        let second = txn.get_vertex(7).unwrap().unwrap();
        assert_eq!(second.label_id, 99);
    }

    #[test]
    fn test_put_edge_overwrite() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        let ek = EdgeKey::out_e(1, 10, 2, 0);
        txn.put_edge(&ek, &[]).unwrap();
        let mut first = txn.get_edge(&ek).unwrap().unwrap();
        assert_eq!(first.all_props().len(), 0);

        let props = vec![Property {
            owner: crate::types::CanonicalKey::Edge(ek.canonical_edge_key()),
            key: 1,
            value: Primitive::Int32(7),
        }];
        txn.put_edge(&ek, &props).unwrap();
        let mut second = txn.get_edge(&ek).unwrap().unwrap();
        let second_props = second.all_props();
        assert_eq!(second_props.len(), 1);
        assert_eq!(second_props[0].value, Primitive::Int32(7));
    }

    #[test]
    fn test_edges_with_nonzero_rank() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        // Two parallel edges between the same vertices — differ only by rank
        let ek0 = EdgeKey::out_e(1, 10, 2, 0);
        let ek1 = EdgeKey::out_e(1, 10, 2, 1);
        txn.put_edge(&ek0, &[]).unwrap();
        txn.put_edge(&ek1, &[]).unwrap();

        // Both are distinct keys and are independently readable
        assert!(txn.get_edge(&ek0).unwrap().is_some());
        assert!(txn.get_edge(&ek1).unwrap().is_some());

        // Scan returns both
        let edges = get_adjacent_edges_test(&mut txn, 1, Direction::OUT, Some(10), None, None);
        assert_eq!(edges.len(), 2);

        // Deleting rank-0 leaves rank-1 intact
        txn.delete_edge(&ek0).unwrap();
        assert!(txn.get_edge(&ek0).unwrap().is_none());
        assert!(txn.get_edge(&ek1).unwrap().is_some());
    }

    #[test]
    fn test_put_and_get_edge_with_properties() {
        let (store, _dir) = open_temp_store();
        let mut txn = ctx(&store);

        let ek = EdgeKey::out_e(1, 100, 2, 0);
        let props = vec![
            Property { owner: CanonicalKey::Edge(ek.canonical_edge_key()), key: 1, value: Primitive::Float64(0.5) },
            Property { owner: CanonicalKey::Edge(ek.canonical_edge_key()), key: 2, value: Primitive::Int64(12345) },
        ];

        txn.put_edge(&ek, &props).unwrap();
        let mut fetched_e = txn.get_edge(&ek).unwrap().unwrap();

        assert_eq!(fetched_e.src_id, ek.primary_id);
        assert_eq!(fetched_e.dst_id, ek.secondary_id);
        assert_eq!(fetched_e.label_id, ek.label_id);
        let fetched_props = fetched_e.all_props();
        assert_eq!(fetched_props.len(), 2);
        assert!(fetched_props.contains(&props[0]));
        assert!(fetched_props.contains(&props[1]));
    }

    #[test]
    fn test_transaction_repeatable_reads_all_scenarios() {
        let (store, _dir) = open_temp_store();

        // 1. Seed initial data
        let mut seed = ctx(&store);
        seed.put_vertex(1, 1, &[]).unwrap();
        seed.put_vertex(2, 1, &[]).unwrap();
        seed.put_vertex_degree(1, 1, 0).unwrap();
        let ek_seed = EdgeKey::out_e(1, 10, 2, 0);
        seed.put_edge(&ek_seed, &[]).unwrap();
        seed.commit().unwrap();

        // 2. Start Transaction 1 (captures snapshot)
        let mut txn1 = ctx(&store);

        // 3. Start Transaction 2 concurrently, perform updates, and commit
        let mut txn2 = ctx(&store);
        // Insert new vertex 3
        txn2.put_vertex(3, 100, &[]).unwrap();
        // Update existing vertex 1's label
        txn2.put_vertex(1, 99, &[]).unwrap();
        // Update vertex 1's degree
        txn2.put_vertex_degree(1, 1, 1).unwrap();
        // Insert new edge 1 --20--> 3
        let ek_new = EdgeKey::out_e(1, 20, 3, 0);
        txn2.put_edge(&ek_new, &[]).unwrap();
        txn2.commit().unwrap();

        // 4. In Transaction 1, verify strict snapshot isolation (repeatable reads)
        // for all GraphTransaction read interfaces:

        // A. Point Vertex Reads (get_vertex / get_vertices)
        let v1 = txn1.get_vertex(1).unwrap().unwrap();
        assert_eq!(v1.label_id, 1); // Should see original label 1, not 99
        let v3_opt = txn1.get_vertex(3).unwrap();
        assert!(v3_opt.is_none()); // Vertex 3 should be invisible

        let batch_v = txn1.get_vertices(&[1, 3]).unwrap();
        assert_eq!(batch_v.len(), 1);
        assert_eq!(batch_v[0].id, 1);
        assert_eq!(batch_v[0].label_id, 1);

        // B. Vertex Degree Point Read (get_vertex_degree)
        let (deg_out, deg_in) = txn1.get_vertex_degree(1).unwrap().unwrap();
        assert_eq!(deg_out, 1);
        assert_eq!(deg_in, 0); // Should see original degree (1, 0), not (1, 1)

        // C. Point Edge Reads (get_edge / get_edges)
        let e_seed = txn1.get_edge(&ek_seed).unwrap();
        assert!(e_seed.is_some());
        let e_new = txn1.get_edge(&ek_new).unwrap();
        assert!(e_new.is_none()); // New edge should be invisible

        let batch_e = txn1.get_edges(&[ek_seed, ek_new]).unwrap();
        assert_eq!(batch_e.len(), 1);
        assert_eq!(batch_e[0].src_id, 1);
        assert_eq!(batch_e[0].dst_id, 2);

        // D. Adjacent Edges range scan (get_adjacent_edges)
        let (adj_edges, _) = txn1
            .get_adjacent_edges(
                1,
                Direction::OUT,
                AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: None },
                None,
            )
            .unwrap();
        assert_eq!(adj_edges.len(), 1);
        assert_eq!(adj_edges[0].dst_id, 2); // Should not see edge to 3

        // E. Full vertices range scan (scan_vertices)
        let (vertices_scan, _) = txn1.scan_vertices(None, None, 10).unwrap();
        let vertex_ids: Vec<_> = vertices_scan.iter().map(|v| v.id).collect();
        assert!(vertex_ids.contains(&1));
        assert!(vertex_ids.contains(&2));
        assert!(!vertex_ids.contains(&3)); // Should not see vertex 3

        // F. Full edges range scan (scan_edges)
        let (edges_scan, _) = txn1.scan_edges(None, None, 10).unwrap();
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

        for src in [1i64, 2, 3, 4, 5] {
            let mut txn = ctx(&store);
            txn.put_edge(&EdgeKey::out_e(src, 10, 100, 0), &[]).unwrap();
            txn.commit().unwrap();
            store.db.flush_cf(&cf).unwrap();
        }

        let mut txn = ctx(&store);
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let (page, next) = txn.scan_edges(None, cursor, 2).unwrap();
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
        assert_eq!(seen, vec![1, 2, 3, 4, 5]);
    }
}
