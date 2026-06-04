// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

//! Thin RocksDB transaction adapter.
//!
//! # Responsibility
//!
//! `Transaction` is a pure I/O layer: it encodes and decodes graph elements
//! to/from RocksDB bytes and stages reads/writes on an `OptimisticTransactionDB`
//! handle.  All overlay logic (dirty tracking, query-scoped caching, key
//! allocation) lives in `LogicalGraph`, one layer above.
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
//! We transmute the lifetime to `'static` so the transaction can live
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
            build_full_edge, build_full_vertex, decode_edge_key_in, decode_edge_key_out, edge_scan_prefix,
            encode_edge_key_in, encode_edge_key_out, encode_props, encode_vertex_key, prefix_upper_bound, EdgeValue,
            VertexDegree, VertexValue, CF_EDGES_IN, CF_EDGES_OUT, CF_VERTEX_DEGREE, CF_VERTICES,
        },
        traits::GraphTransaction,
    },
    types::{element::Property, CanonicalEdgeKey, Direction, Edge, LabelId, StoreError, Vertex, VertexKey},
};

type EdgeKeyDecoder = fn(&[u8]) -> Option<CanonicalEdgeKey>;

// ── Lifetime-erased RocksDB transaction ──────────────────────────────────────

type OwnedRocksTxn = rocksdb::Transaction<'static, OptimisticTransactionDB>;

/// Create a new optimistic transaction, erasing the `'db` lifetime.
///
/// # Safety
///
/// The caller must ensure the returned `OwnedRocksTxn` is dropped before the
/// `Arc<OptimisticTransactionDB>` it was created from.  In `Transaction` this
/// is guaranteed by field declaration order (`db_txn` before `db`).
fn begin_txn(db: &Arc<OptimisticTransactionDB>) -> OwnedRocksTxn {
    let txn: rocksdb::Transaction<'_, OptimisticTransactionDB> = db.transaction();
    // SAFETY: see module doc and function safety note.
    unsafe { std::mem::transmute(txn) }
}

// ── Transaction ───────────────────────────────────────────────────────────────

pub struct Transaction {
    // IMPORTANT: `db_txn` must come before `db` — drop order is declaration order.
    db_txn: Option<OwnedRocksTxn>,
    db: Arc<OptimisticTransactionDB>,
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if let Some(txn) = self.db_txn.take() {
            let _ = txn.rollback();
        }
        // `db_txn` is now None; the `Arc<OTD>` in `db` drops after this.
    }
}

impl Transaction {
    pub fn new(db: Arc<OptimisticTransactionDB>) -> Self {
        let db_txn = begin_txn(&db);
        Self { db_txn: Some(db_txn), db }
    }
}

// ── GraphTransaction ──────────────────────────────────────────────────────────

impl GraphTransaction for Transaction {
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let vv_raw = self
            .db_txn
            .as_ref()
            .expect("no active transaction")
            .get_for_update_cf(&cf_vertices, encode_vertex_key(key), true)
            .map_err(StoreError::RocksDb)?;

        match vv_raw {
            Some(vv_bytes) => {
                let vv = VertexValue::decode(&vv_bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                Ok(Some(build_full_vertex(key, &vv)?))
            }
            _ => Ok(None),
        }
    }

    fn get_vertex_degree(&mut self, key: VertexKey) -> Result<Option<(u32, u32)>, StoreError> {
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let vd_raw = self
            .db_txn
            .as_ref()
            .expect("no active transaction")
            .get_for_update_cf(&cf_degree, encode_vertex_key(key), true)
            .map_err(StoreError::RocksDb)?;
        match vd_raw {
            Some(vd_bytes) => {
                let vd = VertexDegree::decode(&vd_bytes).ok_or(StoreError::CorruptData("vertex degree"))?;
                Ok(Some((vd.out_e_cnt, vd.in_e_cnt)))
            }
            _ => Ok(None),
        }
    }

    fn get_edge(&mut self, key: CanonicalEdgeKey, direction: Direction) -> Result<Option<Edge>, StoreError> {
        let (cf_name, key_bytes) = match direction {
            Direction::OUT => (CF_EDGES_OUT, encode_edge_key_out(key)),
            Direction::IN => (CF_EDGES_IN, encode_edge_key_in(key)),
        };
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let raw_opt = self
            .db_txn
            .as_ref()
            .expect("no active transaction")
            .get_for_update_cf(&cf, key_bytes, false)
            .map_err(StoreError::RocksDb)?;

        match raw_opt {
            None => Ok(None),
            Some(raw) => Ok(Some(build_full_edge(key, &EdgeValue::decode(&raw))?)),
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
        let (cf_name, decode_fn): (&str, EdgeKeyDecoder) = match direction {
            Direction::OUT => (CF_EDGES_OUT, decode_edge_key_out),
            Direction::IN => (CF_EDGES_IN, decode_edge_key_in),
        };

        let prefix = edge_scan_prefix(vertex, label);
        let mut read_opts = ReadOptions::default();

        if let Some(upper) = prefix_upper_bound(&prefix) {
            read_opts.set_iterate_upper_bound(upper);
        }

        let dst_set: Option<HashSet<VertexKey>> = dst.map(|k| k.iter().copied().collect());

        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let iter = txn.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&prefix, ScanDir::Forward));

        let mut result = Vec::new();
        for item in iter {
            let (key_bytes, val_bytes) = item.map_err(StoreError::RocksDb)?;
            if !key_bytes.starts_with(&prefix) {
                break;
            }
            let cek = decode_fn(&key_bytes).ok_or(StoreError::CorruptData("edge key"))?;
            if let Some(ref set) = dst_set {
                let remote = match direction {
                    Direction::OUT => cek.dst_id,
                    Direction::IN => cek.src_id,
                };
                if !set.contains(&remote) {
                    continue;
                }
            }
            result.push(build_full_edge(cek, &EdgeValue::decode(&val_bytes))?);
            if let Some(max) = limit {
                if result.len() >= max as usize {
                    break;
                }
            }
        }
        Ok(result)
    }

    fn put_vertex(&mut self, key: VertexKey, label_id: LabelId, props: &[Property]) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let vv = VertexValue { label_id, property_blob: encode_props(props) };
        txn.put_cf(&cf_vertices, encode_vertex_key(key), vv.encode()).map_err(StoreError::RocksDb)
    }

    fn put_vertex_degree(&mut self, key: VertexKey, out_e_cnt: u32, in_e_cnt: u32) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let vd = VertexDegree { out_e_cnt, in_e_cnt };
        txn.put_cf(&cf_degree, encode_vertex_key(key), vd.encode()).map_err(StoreError::RocksDb)
    }

    fn put_edge(&mut self, key: CanonicalEdgeKey, direction: Direction, props: &[Property]) -> Result<(), StoreError> {
        let txn = self.db_txn.as_ref().expect("no active transaction");
        let (cf_name, key_bytes) = match direction {
            Direction::OUT => (CF_EDGES_OUT, encode_edge_key_out(key)),
            Direction::IN => (CF_EDGES_IN, encode_edge_key_in(key)),
        };
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let ev_bytes = EdgeValue { property_blob: encode_props(props) }.encode().to_vec();
        txn.put_cf(&cf, key_bytes, &ev_bytes).map_err(StoreError::RocksDb)
    }

    fn delete_vertex(&mut self, key: VertexKey) -> Result<(), StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        txn.delete_cf(&cf_vertices, encode_vertex_key(key)).map_err(StoreError::RocksDb)
    }

    fn delete_vertex_degree(&mut self, key: VertexKey) -> Result<(), StoreError> {
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        txn.delete_cf(&cf_degree, encode_vertex_key(key)).map_err(StoreError::RocksDb)
    }

    fn delete_edge(&mut self, key: CanonicalEdgeKey, direction: Direction) -> Result<(), StoreError> {
        let (cf_name, key_bytes) = match direction {
            Direction::OUT => (CF_EDGES_OUT, encode_edge_key_out(key)),
            Direction::IN => (CF_EDGES_IN, encode_edge_key_in(key)),
        };
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let txn = self.db_txn.as_ref().expect("no active transaction");
        txn.delete_cf(&cf, key_bytes).map_err(StoreError::RocksDb)
    }

    fn commit(&mut self) -> Result<(), StoreError> {
        let txn = self.db_txn.take().expect("no active transaction");
        let result = txn.commit().map_err(|e| {
            if e.to_string().contains("Resource busy") {
                StoreError::Conflict
            } else {
                StoreError::RocksDb(e)
            }
        });
        self.db_txn = Some(begin_txn(&self.db));
        result
    }

    fn abort(&mut self) {
        if let Some(txn) = self.db_txn.take() {
            let _ = txn.rollback();
        }
        self.db_txn = Some(begin_txn(&self.db));
    }
}

#[cfg(test)]
mod tests {

    use rocksdb::{DBCommon, OptimisticTransactionDB, Options, SingleThreaded, DB};

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
}
