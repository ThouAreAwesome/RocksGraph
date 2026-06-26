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

//! Non-transactional admin read/write operations for `RocksStorage`.
//!
//! These methods are for bulk loads and admin tooling **only**. They bypass
//! OCC conflict detection; use `LogicalGraph` for all write paths that
//! require conflict safety.
//!
//! # Property codec
//!
//! Format: `count:u16 | (key_id:u16 | tag:u8 | value_bytes)*`
//!
//! `key_id` is the interned [`Schema`](crate::schema::Schema) property-key id, not the
//! raw string name — the schema dictionary maps it back to a name on read.
//!
//! Tags: `0`=Bool(1B) `1`=Int32(4B) `2`=Int64(8B) `3`=Float32(4B)
//!       `4`=Float64(8B) `5`=String(len:u16 + UTF-8) `6`=Uuid(16B) `7`=Null(0B)

use std::collections::HashSet;

use rocksdb::{Direction as ScanDir, IteratorMode, ReadOptions, WriteBatchWithTransaction};

use crate::{
    store::rocks::{
        encoding::{
            build_full_edge, build_full_vertex, decode_edge_key, edge_scan_prefix, encode_edge_key, encode_props,
            encode_vertex_key, prefix_upper_bound, EdgeValue, VertexDegree, VertexValue, CF_EDGES_IN, CF_EDGES_OUT,
            CF_VERTEX_DEGREE, CF_VERTICES,
        },
        store::RocksStorage,
    },
    types::{CanonicalEdgeKey, Direction, Edge, EdgeKey, LabelId, StoreError, Vertex, VertexKey},
};

#[allow(dead_code)]
type EdgeKeyDecoder = fn(&[u8]) -> Option<CanonicalEdgeKey>;

// ── Admin reads / writes ──────────────────────────────────────────────────────
// These methods are used in tests and admin tooling.  They are pub(crate) but
// only called from #[cfg(test)] blocks, so clippy flags them as dead code during
// non-test compilation.  The suppression is intentional.
/// Provides administrative read/write operations for `RocksStorage`.
#[allow(dead_code)]
impl RocksStorage {
    pub(crate) fn get_vertex(&self, key: VertexKey) -> Result<Option<Vertex>, StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let vv_raw = self.db.get_cf(&cf_vertices, encode_vertex_key(key)).map_err(StoreError::RocksDb)?;
        match vv_raw {
            Some(vv_bytes) => {
                let vv = VertexValue::decode(&vv_bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                Ok(Some(build_full_vertex(key, &vv)?))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn get_vertices(&self, keys: &[VertexKey]) -> Result<Vec<Vertex>, StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let mut result = Vec::with_capacity(keys.len());
        for &key in keys {
            let vv_raw = self.db.get_cf(&cf_vertices, encode_vertex_key(key)).map_err(StoreError::RocksDb)?;
            if let Some(vv_bytes) = vv_raw {
                let vv = VertexValue::decode(&vv_bytes).ok_or(StoreError::CorruptData("vertex value"))?;
                result.push(build_full_vertex(key, &vv)?);
            }
        }
        Ok(result)
    }

    pub(crate) fn get_edge(&self, key: &EdgeKey) -> Result<Option<Edge>, StoreError> {
        let cf_name = match key.direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let key_bytes = encode_edge_key(key);
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        match self.db.get_cf(&cf, key_bytes).map_err(StoreError::RocksDb)? {
            None => Ok(None),
            Some(raw) => {
                let ev = EdgeValue::decode(&raw).ok_or(StoreError::CorruptData("edge value"))?;
                Ok(Some(build_full_edge(key, &ev)?))
            }
        }
    }

    pub(crate) fn get_edges(
        &self,
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

        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;

        let prefix = edge_scan_prefix(vertex, label);
        let mut read_opts = ReadOptions::default();
        if let Some(upper) = prefix_upper_bound(&prefix) {
            read_opts.set_iterate_upper_bound(upper.to_vec());
        }

        let dst_set: Option<HashSet<VertexKey>> = dst.map(|k| k.iter().copied().collect());
        let iter = self.db.iterator_cf_opt(&cf, read_opts, IteratorMode::From(&prefix, ScanDir::Forward));

        let mut result = Vec::new();
        for item in iter {
            let (key_bytes, val_bytes) = item.map_err(StoreError::RocksDb)?;
            if !key_bytes.starts_with(&prefix) {
                break;
            }
            let ek = decode_edge_key(&key_bytes, direction).ok_or(StoreError::CorruptData("edge key"))?;
            if let Some(ref set) = dst_set {
                if !set.contains(&ek.secondary_id) {
                    continue;
                }
            }
            let ev = EdgeValue::decode(&val_bytes).ok_or(StoreError::CorruptData("edge value"))?;
            result.push(build_full_edge(&ek, &ev)?);
            if let Some(max) = limit {
                if result.len() >= max as usize {
                    break;
                }
            }
        }
        Ok(result)
    }

    // ── Admin writes ──────────────────────────────────────────────────────────
    // All write methods use `WriteBatchWithTransaction::<true>` (TRANSACTION=true).
    // `OptimisticTransactionDB::write()` requires this type; using the plain
    // `WriteBatch` (TRANSACTION=false) is a compile-time type mismatch.

    pub(crate) fn insert_vertices(&mut self, vertices: &mut [Vertex]) -> Result<(), StoreError> {
        let cf_vertices = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let cf_degree = self.db.cf_handle(CF_VERTEX_DEGREE).ok_or(StoreError::MissingColumnFamily("vertex_degree"))?;
        let mut batch = WriteBatchWithTransaction::<true>::default();
        for vv in vertices {
            let val = VertexValue { label_id: vv.label_id, property_blob: encode_props(vv.all_props()) };
            let degree = VertexDegree { vertex_label_id: vv.label_id, out_e_cnt: 0, in_e_cnt: 0 };
            batch.put_cf(&cf_vertices, encode_vertex_key(vv.id), val.encode());
            batch.put_cf(&cf_degree, encode_vertex_key(vv.id), degree.encode());
        }
        self.db.write(batch).map_err(StoreError::RocksDb)
    }

    pub(crate) fn insert_edges(&mut self, edges: &mut [Edge], direction: Direction) -> Result<(), StoreError> {
        let cf_name = match direction {
            Direction::OUT => CF_EDGES_OUT,
            Direction::IN => CF_EDGES_IN,
        };
        let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
        let mut batch = WriteBatchWithTransaction::<true>::default();
        for ev in edges {
            let key_bytes = match direction {
                Direction::OUT => encode_edge_key(&ev.edge_key_out()),
                Direction::IN => encode_edge_key(&ev.edge_key_in()),
            };
            let bytes = EdgeValue { end_vertex_label: 0, property_blob: encode_props(ev.all_props()) }.encode();
            batch.put_cf(&cf, key_bytes, &bytes);
        }
        self.db.write(batch).map_err(StoreError::RocksDb)
    }

    pub(crate) fn delete_vertices(&mut self, keys: &[VertexKey]) -> Result<(), StoreError> {
        let cf = self.db.cf_handle(CF_VERTICES).ok_or(StoreError::MissingColumnFamily("vertices"))?;
        let mut batch = WriteBatchWithTransaction::<true>::default();
        for &key in keys {
            batch.delete_cf(&cf, encode_vertex_key(key));
        }
        self.db.write(batch).map_err(StoreError::RocksDb)
    }

    pub(crate) fn delete_edges(&mut self, keys: &[EdgeKey]) -> Result<(), StoreError> {
        let mut batch = WriteBatchWithTransaction::<true>::default();
        for key in keys {
            let cf_name = match key.direction {
                Direction::OUT => CF_EDGES_OUT,
                Direction::IN => CF_EDGES_IN,
            };
            let cf = self.db.cf_handle(cf_name).ok_or(StoreError::MissingColumnFamily(cf_name))?;
            let key_bytes = encode_edge_key(key);
            batch.delete_cf(&cf, key_bytes);
        }
        self.db.write(batch).map_err(StoreError::RocksDb)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::types::keys::LabelId;
    use smol_str::SmolStr;

    use crate::{
        store::rocks::store::RocksStorage,
        types::{
            element::{Edge, Property, Vertex},
            gvalue::Primitive,
            CanonicalEdgeKey, CanonicalKey, Direction,
        },
    };

    fn open_temp_store() -> (RocksStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        (store, dir)
    }

    fn make_vertex(id: i64, label_id: LabelId, props: Vec<(u16, Primitive)>) -> Vertex {
        let owner = CanonicalKey::Vertex(id);
        Vertex::with_props(id, label_id, props.into_iter().map(|(k, v)| Property { owner, key: k, value: v }).collect())
    }

    fn make_edge(cek: CanonicalEdgeKey, props: Vec<(u16, Primitive)>) -> Edge {
        let owner = CanonicalKey::Edge(cek);
        Edge::with_props(
            cek.src_id,
            cek.label_id,
            cek.dst_id,
            cek.rank,
            props.into_iter().map(|(k, v)| Property { owner, key: k, value: v }).collect(),
            None,
            None,
        )
    }

    fn cek(src: i64, label: LabelId, dst: i64) -> CanonicalEdgeKey {
        CanonicalEdgeKey { src_id: src, label_id: label, rank: 0, dst_id: dst }
    }

    #[test]
    fn insert_and_get_single_vertex() {
        let (mut store, _dir) = open_temp_store();
        let v = make_vertex(1, 3, vec![(1u16, Primitive::String(SmolStr::new("Alice"))), (2u16, Primitive::Int32(30))]);
        store.insert_vertices(&mut [v]).unwrap();
        let mut fv = store.get_vertex(1).unwrap().unwrap();
        assert_eq!(fv.id, 1);
        assert_eq!(fv.label_id, 3);
        assert_eq!(fv.all_props().len(), 2);
        assert_eq!(fv.all_props()[0].key, 1u16);
        assert_eq!(fv.all_props()[0].value, Primitive::String(SmolStr::new("Alice")));
        assert_eq!(fv.all_props()[0].owner, CanonicalKey::Vertex(1));
        assert_eq!(fv.all_props()[1].value, Primitive::Int32(30));
    }

    #[test]
    fn get_vertex_not_found_returns_none() {
        let (store, _dir) = open_temp_store();
        assert!(store.get_vertex(999).unwrap().is_none());
    }

    #[test]
    fn insert_vertex_with_no_props() {
        let (mut store, _dir) = open_temp_store();
        store.insert_vertices(&mut [make_vertex(42, 1, vec![])]).unwrap();
        let mut fv = store.get_vertex(42).unwrap().unwrap();
        assert_eq!(fv.label_id, 1);
        assert!(fv.all_props().is_empty());
    }

    #[test]
    fn insert_vertex_overwrite_updates_value() {
        let (mut store, _dir) = open_temp_store();
        store.insert_vertices(&mut [make_vertex(1, 1, vec![(2u16, Primitive::Int32(20))])]).unwrap();
        store.insert_vertices(&mut [make_vertex(1, 2, vec![(2u16, Primitive::Int32(99))])]).unwrap();
        let mut fv = store.get_vertex(1).unwrap().unwrap();
        assert_eq!(fv.label_id, 2);
        assert_eq!(fv.all_props()[0].value, Primitive::Int32(99));
    }

    #[test]
    fn get_vertices_returns_all_inserted() {
        let (mut store, _dir) = open_temp_store();
        store
            .insert_vertices(&mut [make_vertex(1, 1, vec![]), make_vertex(2, 1, vec![]), make_vertex(3, 2, vec![])])
            .unwrap();
        let results = store.get_vertices(&[1, 2, 3]).unwrap();
        assert_eq!(results.len(), 3);
        let mut ids: Vec<i64> = results.iter().map(|v| v.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn get_vertices_silently_omits_missing_keys() {
        let (mut store, _dir) = open_temp_store();
        store.insert_vertices(&mut [make_vertex(10, 1, vec![])]).unwrap();
        let results = store.get_vertices(&[10, 20, 30]).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, 10);
    }

    #[test]
    fn get_vertices_all_missing_returns_empty() {
        let (store, _dir) = open_temp_store();
        assert!(store.get_vertices(&[1, 2, 3]).unwrap().is_empty());
    }

    #[test]
    fn insert_edge_readable_out() {
        let (mut store, _dir) = open_temp_store();
        let k = cek(1, 5, 2);
        store.insert_edges(&mut [make_edge(k, vec![(1u16, Primitive::Float64(1.5))])], Direction::OUT).unwrap();
        let mut edges = store.get_edges(1, Direction::OUT, None, None, None).unwrap();
        assert_eq!(edges.len(), 1);
        let fe = &mut edges[0];
        assert_eq!(fe.src_id, 1);
        assert_eq!(fe.dst_id, 2);
        assert_eq!(fe.label_id, 5);
        assert_eq!(fe.all_props()[0].value, Primitive::Float64(1.5));
        assert_eq!(fe.all_props()[0].owner, CanonicalKey::Edge(k));
    }

    #[test]
    fn insert_edge_readable_in() {
        let (mut store, _dir) = open_temp_store();
        store.insert_edges(&mut [make_edge(cek(1, 5, 2), vec![])], Direction::IN).unwrap();
        let edges = store.get_edges(2, Direction::IN, None, None, None).unwrap();
        assert_eq!(edges.len(), 1);
        let fe = &edges[0];
        assert_eq!(fe.src_id, 1);
        assert_eq!(fe.dst_id, 2);
        assert_eq!(fe.label_id, 5);
    }

    #[test]
    fn get_edges_filter_by_label() {
        let (mut store, _dir) = open_temp_store();
        store
            .insert_edges(
                &mut [
                    make_edge(cek(1, 1, 10), vec![]),
                    make_edge(cek(1, 2, 20), vec![]),
                    make_edge(cek(1, 1, 30), vec![]),
                ],
                Direction::OUT,
            )
            .unwrap();
        let label1 = store.get_edges(1, Direction::OUT, Some(1), None, None).unwrap();
        assert_eq!(label1.len(), 2);
        assert!(label1.iter().all(|e| e.label_id == 1));
        let label2 = store.get_edges(1, Direction::OUT, Some(2), None, None).unwrap();
        assert_eq!(label2.len(), 1);
        assert_eq!(label2[0].dst_id, 20);
    }

    #[test]
    fn get_edges_filter_by_dst() {
        let (mut store, _dir) = open_temp_store();
        store
            .insert_edges(
                &mut [
                    make_edge(cek(1, 1, 10), vec![]),
                    make_edge(cek(1, 1, 20), vec![]),
                    make_edge(cek(1, 1, 30), vec![]),
                ],
                Direction::OUT,
            )
            .unwrap();
        let result = store.get_edges(1, Direction::OUT, None, Some(&[10, 30]), None).unwrap();
        assert_eq!(result.len(), 2);
        let mut dst_ids: Vec<i64> = result.iter().map(|e| e.dst_id).collect();
        dst_ids.sort_unstable();
        assert_eq!(dst_ids, vec![10, 30]);
    }

    #[test]
    fn get_edges_no_match_returns_empty() {
        let (store, _dir) = open_temp_store();
        assert!(store.get_edges(99, Direction::OUT, None, None, None).unwrap().is_empty());
        assert!(store.get_edges(99, Direction::IN, None, None, None).unwrap().is_empty());
    }

    #[test]
    fn get_edges_multiple_from_same_source() {
        let (mut store, _dir) = open_temp_store();
        store
            .insert_edges(
                &mut [
                    make_edge(cek(1, 1, 10), vec![]),
                    make_edge(cek(1, 1, 20), vec![]),
                    make_edge(cek(1, 1, 30), vec![]),
                    make_edge(cek(2, 1, 10), vec![]),
                ],
                Direction::OUT,
            )
            .unwrap();
        let edges = store.get_edges(1, Direction::OUT, None, None, None).unwrap();
        assert_eq!(edges.len(), 3);
        assert!(edges.iter().all(|e| e.src_id == 1));
    }
    #[test]
    fn get_edges_with_limit_from_same_source() {
        let (mut store, _dir) = open_temp_store();
        store
            .insert_edges(
                &mut [
                    make_edge(cek(1, 1, 10), vec![]),
                    make_edge(cek(1, 1, 20), vec![]),
                    make_edge(cek(1, 1, 30), vec![]),
                    make_edge(cek(2, 1, 10), vec![]),
                ],
                Direction::OUT,
            )
            .unwrap();
        let edges = store.get_edges(1, Direction::OUT, None, None, Some(3)).unwrap();
        assert_eq!(edges.len(), 3);
        assert!(edges.iter().all(|e| e.src_id == 1));

        let edges = store.get_edges(1, Direction::OUT, None, None, Some(2)).unwrap();
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.src_id == 1));

        let edges = store.get_edges(1, Direction::OUT, None, None, Some(1)).unwrap();
        assert_eq!(edges.len(), 1);
        assert!(edges.iter().all(|e| e.src_id == 1));
    }
}
