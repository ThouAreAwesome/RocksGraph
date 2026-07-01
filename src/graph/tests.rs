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

use smol_str::SmolStr;

use super::LogicalGraph;
use crate::store::traits::GraphStore;

use crate::{
    store::RocksStorage,
    types::{
        element::Property,
        element::Vertex,
        gvalue::Primitive,
        keys::{AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId, VertexKey},
        prop_key::LABEL_KEY_ID,
        StoreError,
    },
};

fn open() -> (RocksStorage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = RocksStorage::open(dir.path(), &Default::default()).unwrap();
    {
        let loaded = store.load_schema(crate::schema::GraphOptions::default()).unwrap();
        let schema = std::sync::Arc::new(std::sync::RwLock::new(loaded));
        let mut c = LogicalGraph::<RocksStorage>::new(store.begin(), schema.clone());
        {
            let mut s = schema.write().unwrap();
            s.resolve_prop_key("age", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("name", crate::schema::DataType::String).unwrap();
            s.resolve_prop_key("x", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("y", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("w", crate::schema::DataType::Float64).unwrap();
            s.resolve_prop_key("a", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("b", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("since", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("nonexistent", crate::schema::DataType::Int32).unwrap();

            s.resolve_vertex_label("person").unwrap();
            s.resolve_vertex_label("software").unwrap();
            s.resolve_edge_label("knows").unwrap();
            s.resolve_edge_label("created").unwrap();
        }
        for label_id in 0..10 {
            c.staged_schema.staged_vertex_labels.insert(label_id);
            c.staged_schema.staged_edge_labels.insert(label_id);
        }
        for prop_key_id in 0..20 {
            c.staged_schema.staged_prop_keys.insert(prop_key_id);
        }
        c.commit().unwrap();
    }
    (store, dir)
}

fn ctx(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
    let loaded = store.load_schema(crate::schema::GraphOptions::default()).unwrap();
    let schema = std::sync::Arc::new(std::sync::RwLock::new(loaded));
    LogicalGraph::new(store.begin(), schema)
}

fn cek(src: i64, label: LabelId, dst: i64) -> CanonicalEdgeKey {
    CanonicalEdgeKey { src_id: src, label_id: label, rank: 0, dst_id: dst }
}

fn get_adjacent_edges_test(
    c: &mut LogicalGraph<RocksStorage>,
    vertex: VertexKey,
    direction: Direction,
    label: Option<LabelId>,
    dst: Option<&[VertexKey]>,
    limit: Option<u32>,
) -> Vec<EdgeKey> {
    c.get_adjacent_edges(vertex, direction, AdjacentEdgesOptions { label, dst, rank: None, start_from: None }, limit)
        .unwrap()
        .0
}

// ── add_vertex / get_vertex ───────────────────────────────────────────────

#[test]
fn add_vertex_visible_via_get_vertex() {
    let (store, _dir) = open();
    let mut c = ctx(&store);

    let key = c.add_vertex(100, 1).unwrap();
    let result = c.get_vertex(key).unwrap();
    assert_eq!(result, Some(key));
}

#[test]
fn get_vertex_absent_returns_none() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    assert!(c.get_vertex(9999).unwrap().is_none());
}

#[test]
fn get_vertex_returns_same_idx_on_repeated_calls() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 2).unwrap();
    assert_eq!(c.get_vertex(key).unwrap(), Some(key));
}

// ── add_edge / get_edge ───────────────────────────────────────────────────

#[test]
fn add_edge_visible_via_get_edge() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    let key = c.add_edge(&k.out_key()).unwrap();
    let result = c.get_edge(&k.out_key()).unwrap().unwrap();
    assert_eq!(k.out_key(), key);
    assert_eq!(result, key);
    assert_eq!((result.primary_id, result.label_id, result.secondary_id), (v1, 5, v2));
}

#[test]
fn add_duplicated_edge_should_fail() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c.add_edge(&k.out_key()).unwrap();

    c.commit().unwrap();

    let mut c = ctx(&store);
    let result = c.add_edge(&k.out_key());
    assert!(result.is_err());
}

#[test]
fn add_duplicated_edge_in_mem_should_fail() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c.add_edge(&k.out_key()).unwrap();

    let result = c.add_edge(&k.out_key());
    assert!(result.is_err());
}

#[test]
fn add_edge_vs_add_same_edge_handmade() {
    let (store, _dir) = open();
    let mut c0 = ctx(&store);
    let v1 = c0.add_vertex(1, 1).unwrap();
    let v2 = c0.add_vertex(2, 1).unwrap();
    c0.commit().unwrap();

    let mut c1 = ctx(&store);
    let mut c2 = ctx(&store);
    let k = cek(v1, 5, v2);

    c1.add_edge(&k.out_key()).unwrap();
    c2.add_edge(&k.out_key()).unwrap();

    c1.commit().unwrap();
    let result = c2.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn commit_resets_overlay_even_on_conflict() {
    let (store, _dir) = open();
    let mut c0 = ctx(&store);
    let v1 = c0.add_vertex(1, 1).unwrap();
    let v2 = c0.add_vertex(2, 1).unwrap();
    c0.commit().unwrap();

    let mut c1 = ctx(&store);
    let mut c2 = ctx(&store);
    let k = cek(v1, 5, v2);

    c1.add_edge(&k.out_key()).unwrap();
    c2.add_edge(&k.out_key()).unwrap();

    c1.commit().unwrap();
    let result = c2.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));

    // The failed commit must still clear the overlay -- see the doc comment on
    // `commit`: callers are allowed to reuse the same context for a fresh attempt
    // rather than discarding it for a brand-new one.
    assert!(c2.dirty.is_empty(), "overlay must be cleared even when the underlying commit conflicts");

    // And the context must genuinely be usable afterward, not just empty.
    let v3 = c2.add_vertex(3, 1).unwrap();
    c2.commit().unwrap();
    assert!(store.get_vertex(v3).unwrap().is_some());
}
// ── set_property ─────────────────────────────────────────────────────────

#[test]
fn set_property_on_new_vertex_read_your_writes() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();

    let prop = Property { owner: CanonicalKey::Vertex(key), key: 4, value: Primitive::Int32(42) };
    c.set_property(&prop).unwrap();

    let v = c.get_vertex(key).unwrap();
    assert_eq!(v, Some(key));
    let val = c.get_value(&CanonicalKey::Vertex(key), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(42)));
}

#[test]
fn set_property_upserts_existing_key() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();

    let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
    let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
    c.set_property(&prop1).unwrap();
    c.set_property(&prop2).unwrap();

    let _ = c.get_vertex(key).unwrap().unwrap();
    let val = c.get_value(&CanonicalKey::Vertex(key), 6).unwrap();
    assert_eq!(val, Some(Primitive::Int32(2)));
}

#[test]
fn set_property_on_edge_read_your_writes() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c.add_edge(&k.out_key()).unwrap();

    let prop = Property { owner: CanonicalKey::Edge(k), key: 8, value: Primitive::Float64(1.5) };
    c.set_property(&prop).unwrap();

    let _ = c.get_edge(&k.out_key()).unwrap().unwrap();
    let val = c.get_value(&CanonicalKey::Edge(k), 8).unwrap();
    assert_eq!(val, Some(Primitive::Float64(1.5)));
}

#[test]
fn set_vertex_property_vs_set_vertex_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let key = c1.add_vertex(100, 1).unwrap();
    c1.commit().unwrap();

    // Two contexts concurrently update the same property key with different values.
    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
    let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
    c2.set_property(&prop1).unwrap();
    c3.set_property(&prop2).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
    let mut c4 = ctx(&store);
    let _ = c4.get_vertex(key).unwrap().unwrap();
    let val = c4.get_value(&CanonicalKey::Vertex(key), 6).unwrap();
    assert_eq!(val, Some(Primitive::Int32(1)));
}

#[test]
fn set_edge_property_vs_set_edge_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c1.add_edge(&k.out_key()).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    c2.get_edge(&k.out_key()).unwrap();
    c3.get_edge(&k.out_key()).unwrap();
    let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
    let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(2) };
    c2.set_property(&prop1).unwrap();
    c3.set_property(&prop2).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

// ── drop_property ─────────────────────────────────────────────────────────

#[test]
fn drop_property_removes_key() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();

    let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 9, value: Primitive::Int32(1) };
    let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 10, value: Primitive::Int32(2) };
    c.set_property(&prop1).unwrap();
    c.set_property(&prop2).unwrap();
    c.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 9, value: Primitive::Null }).unwrap();

    let _ = c.get_vertex(key).unwrap().unwrap();
    let val_a = c.get_value(&CanonicalKey::Vertex(key), 9).unwrap();
    let val_b = c.get_value(&CanonicalKey::Vertex(key), 10).unwrap();
    assert_eq!(val_a, None);
    assert_eq!(val_b, Some(Primitive::Int32(2)));
}

#[test]
fn drop_property_on_missing_key_is_noop() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();
    c.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 12, value: Primitive::Null }).unwrap();
    let _ = c.get_vertex(key).unwrap().unwrap();
    let val = c.get_value(&CanonicalKey::Vertex(key), 12).unwrap();
    assert_eq!(val, None);
}

#[test]
fn drop_vertex_property_vs_set_vertex_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let key = c1.add_vertex(100, 1).unwrap();
    let prop = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
    c1.set_property(&prop).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    c2.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Null }).unwrap();
    let prop = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
    c3.set_property(&prop).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn set_vertex_property_vs_drop_vertex_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let key = c1.add_vertex(100, 1).unwrap();
    let prop1 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
    c1.set_property(&prop1).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    let prop2 = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(2) };
    c2.set_property(&prop2).unwrap();
    c3.drop_property(&Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Null }).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn drop_edge_property_vs_set_edge_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c1.add_edge(&k.out_key()).unwrap();
    let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
    c1.set_property(&prop1).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    let _ = c2.get_edge(&k.out_key()).unwrap();
    let _ = c3.get_edge(&k.out_key()).unwrap();
    c2.drop_property(&Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Null }).unwrap();
    let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(2) };
    c3.set_property(&prop2).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn set_edge_property_vs_drop_edge_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c1.add_edge(&k.out_key()).unwrap();
    let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
    c1.set_property(&prop1).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    let _ = c2.get_edge(&k.out_key()).unwrap();
    let _ = c3.get_edge(&k.out_key()).unwrap();
    let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(2) };
    c2.set_property(&prop2).unwrap();
    let prop3 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Null };
    c3.drop_property(&prop3).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

// ── drop_element ──────────────────────────────────────────────────────────

#[test]
fn tombstoned_vertex_invisible_to_get_vertex() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();
    let v = c.get_vertex(key).unwrap().unwrap();
    assert_eq!(v, key);
    c.drop_element(&CanonicalKey::Vertex(key)).unwrap();
    assert!(c.get_vertex(key).unwrap().is_none());
}

#[test]
fn tombstoned_edge_invisible_to_get_edge() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c.add_edge(&k.out_key()).unwrap();
    let e = c.get_edge(&k.out_key()).unwrap().unwrap();
    assert_eq!(e.canonical_edge_key(), k);
    c.drop_element(&CanonicalKey::Edge(k)).unwrap();
    assert!(c.get_edge(&k.out_key()).unwrap().is_none());
}

#[test]
fn drop_vertex_with_edges_errors() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c.add_edge(&k.out_key()).unwrap();

    let err = c.drop_element(&CanonicalKey::Vertex(v1));
    assert!(err.is_err());
    assert_eq!(err.unwrap_err().to_string(), "cannot drop vertex with incident edges");

    c.commit().unwrap();

    let mut c2 = ctx(&store);
    let err = c2.drop_element(&CanonicalKey::Vertex(v1));
    assert!(err.is_err());
    assert_eq!(err.unwrap_err().to_string(), "cannot drop vertex with incident edges");
}

#[test]
fn set_property_on_tombstoned_vertex_errors() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();
    c.drop_element(&CanonicalKey::Vertex(key)).unwrap();
    let prop = Property { owner: CanonicalKey::Vertex(key), key: 6, value: Primitive::Int32(1) };
    let err = c.set_property(&prop);
    assert!(err.is_err());
    assert_eq!(err.unwrap_err().to_string(), "element is tombstoned");
}

#[test]
fn add_edge_vs_drop_edge_handmade() {
    let (store, _dir) = open();
    let mut c0 = ctx(&store);
    let v1 = c0.add_vertex(1, 1).unwrap();
    let v2 = c0.add_vertex(2, 1).unwrap();
    c0.commit().unwrap();

    let mut c1 = ctx(&store);
    let mut c2 = ctx(&store);
    let k = cek(v1, 5, v2);

    c1.add_edge(&k.out_key()).unwrap();
    c2.add_edge(&k.out_key()).unwrap();

    c1.commit().unwrap();
    c2.drop_element(&CanonicalKey::Edge(k)).unwrap();
    let result = c2.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn drop_vertex_vs_add_edge_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 2).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);

    let k = cek(v1, 5, v2);
    c2.add_edge(&k.out_key()).unwrap();
    c3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();

    assert!(c3.commit().is_ok(), "c3 should commit successfully");

    let result = c2.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn add_edge_vs_drop_vertex_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 2).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);

    let k = cek(v1, 5, v2);
    c2.add_edge(&k.out_key()).unwrap();
    c3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();

    assert!(c2.commit().is_ok(), "c2 should commit successfully");

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn drop_dst_vertex_vs_add_edge_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 2).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);

    let k = cek(v1, 5, v2);
    c2.add_edge(&k.out_key()).unwrap();
    c3.drop_element(&CanonicalKey::Vertex(v2)).unwrap();

    assert!(c3.commit().is_ok(), "c3 should commit successfully");

    let result = c2.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn add_edge_vs_drop_dst_vertex_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 2).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);

    let k = cek(v1, 5, v2);
    c2.add_edge(&k.out_key()).unwrap();
    c3.drop_element(&CanonicalKey::Vertex(v2)).unwrap();

    assert!(c2.commit().is_ok(), "c2 should commit successfully");

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn set_edge_property_vs_drop_edge_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c1.add_edge(&k.out_key()).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    let _ = c2.get_edge(&k.out_key()).unwrap();
    let _ = c3.get_edge(&k.out_key()).unwrap();
    let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
    c2.set_property(&prop1).unwrap();
    let prop2 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Null };
    c3.drop_property(&prop2).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

#[test]
fn drop_edge_vs_set_edge_property_handmade() {
    let (store, _dir) = open();
    let mut c1 = ctx(&store);
    let v1 = c1.add_vertex(1, 1).unwrap();
    let v2 = c1.add_vertex(2, 1).unwrap();
    let k = cek(v1, 5, v2);
    c1.add_edge(&k.out_key()).unwrap();
    c1.commit().unwrap();

    let mut c2 = ctx(&store);
    let mut c3 = ctx(&store);
    let _ = c2.get_edge(&k.out_key()).unwrap();
    let _ = c3.get_edge(&k.out_key()).unwrap();
    let prop1 = Property { owner: CanonicalKey::Edge(k), key: 6, value: Primitive::Int32(1) };
    c2.drop_element(&CanonicalKey::Edge(k)).unwrap();
    c3.set_property(&prop1).unwrap();

    c2.commit().unwrap();

    let result = c3.commit();
    assert!(matches!(result, Err(StoreError::Conflict)));
}

// ── commit ────────────────────────────────────────────────────────────────

#[test]
fn commit_persists_vertex_to_store() {
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(77, 7).unwrap();
        let prop =
            Property { owner: CanonicalKey::Vertex(key), key: 5, value: Primitive::String(SmolStr::new("Alice")) };
        c.set_property(&prop).unwrap();
        c.commit().unwrap();
        key
    };

    let mut fv = store.get_vertex(id).unwrap().unwrap();
    assert_eq!(fv.label_id, 7);
    assert_eq!(fv.all_props().len(), 1);
    assert_eq!(fv.all_props()[0].value, Primitive::String(SmolStr::new("Alice")));
}

#[test]
fn commit_persists_edge_to_store() {
    let (store, _dir) = open();
    let (v1, v2) = {
        let mut c0 = ctx(&store);
        let v_1 = c0.add_vertex(1, 1).unwrap();
        let v_2 = c0.add_vertex(2, 1).unwrap();
        c0.commit().unwrap();
        (v_1, v_2)
    };
    let k = cek(v1, 3, v2);
    {
        let mut c = ctx(&store);
        c.add_edge(&k.out_key()).unwrap();
        let prop = Property { owner: CanonicalKey::Edge(k), key: 11, value: Primitive::Int32(99) };
        c.set_property(&prop).unwrap();
        c.commit().unwrap();
    }

    let mut edges = store.get_edges(v1, Direction::OUT, None, None, None).unwrap();
    assert_eq!(edges.len(), 1);
    let e = &mut edges[0];
    assert_eq!(e.all_props().len(), 1);
    assert_eq!(e.all_props()[0].value, Primitive::Int32(99));
}

#[test]
fn commit_persists_vertex_deletion() {
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.commit().unwrap();
        key
    };
    assert!(store.get_vertex(id).unwrap().is_some());

    {
        let mut c = ctx(&store);
        let _ = c.get_vertex(id).unwrap();
        c.drop_element(&CanonicalKey::Vertex(id)).unwrap();
        c.commit().unwrap();
    }
    assert!(store.get_vertex(id).unwrap().is_none());
}

#[test]
fn commit_resets_overlay_for_reuse() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let key = c.add_vertex(100, 1).unwrap();
    c.commit().unwrap();
    // Overlay is cleared — the same key must now load from store, not the old overlay.
    let vertex = c.get_vertex(key).unwrap().unwrap();
    assert_eq!(vertex, key);
}

// ── abort ─────────────────────────────────────────────────────────────────

#[test]
fn abort_discards_pending_writes() {
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.abort();
        key
    };
    assert!(store.get_vertex(id).unwrap().is_none());
}

// ── get_edges ─────────────────────────────────────────────────────────────

#[test]
fn get_edges_returns_new_dirty_edges_before_commit() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v10 = c.add_vertex(10, 1).unwrap();
    let v20 = c.add_vertex(20, 1).unwrap();
    c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();

    let edges = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
    assert_eq!(edges.len(), 2);
}

#[test]
fn get_edges_filters_tombstoned_edges() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v10 = c.add_vertex(10, 1).unwrap();
    let v20 = c.add_vertex(20, 1).unwrap();
    c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
    c.drop_element(&CanonicalKey::Edge(cek(v1, 1, v10))).unwrap();

    let edges = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
    assert_eq!(edges.len(), 1);
}

#[test]
fn get_edges_direction_in_vs_out() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();
    c.add_edge(&cek(v1, 1, v2).out_key()).unwrap();

    let out = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
    let in_ = get_adjacent_edges_test(&mut c, v2, Direction::IN, None, None, None);
    assert_eq!(out.len(), 1);
    assert_eq!(in_.len(), 1);
    // Vertex v1 has no incoming edges; vertex v2 has no outgoing.
    assert!(get_adjacent_edges_test(&mut c, v1, Direction::IN, None, None, None).is_empty());
    assert!(get_adjacent_edges_test(&mut c, v2, Direction::OUT, None, None, None).is_empty());
}

#[test]
fn get_edges_label_filter() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v10 = c.add_vertex(10, 1).unwrap();
    let v20 = c.add_vertex(20, 1).unwrap();
    let v30 = c.add_vertex(30, 1).unwrap();
    c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
    c.add_edge(&cek(v1, 2, v20).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v30).out_key()).unwrap();

    let label1 = get_adjacent_edges_test(&mut c, v1, Direction::OUT, Some(1), None, None);
    assert_eq!(label1.len(), 2);
    assert!(label1.iter().all(|ek| ek.label_id == 1));

    let label2 = get_adjacent_edges_test(&mut c, v1, Direction::OUT, Some(2), None, None);
    assert_eq!(label2.len(), 1);
}

#[test]
fn get_edges_dst_filter() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v10 = c.add_vertex(10, 1).unwrap();
    let v20 = c.add_vertex(20, 1).unwrap();
    let v30 = c.add_vertex(30, 1).unwrap();
    c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v30).out_key()).unwrap();

    let result = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, Some(&[v10, v30]), None);
    assert_eq!(result.len(), 2);
    let mut secondaries: Vec<i64> = result.iter().map(|ek| ek.secondary_id).collect();
    secondaries.sort_unstable();
    let mut expected = vec![v10, v30];
    expected.sort_unstable();
    assert_eq!(secondaries, expected);
}

#[test]
fn get_edges_limit_filter() {
    let (store, _dir) = open();
    let mut c = ctx(&store);
    let v1 = c.add_vertex(1, 1).unwrap();
    let v10 = c.add_vertex(10, 1).unwrap();
    let v20 = c.add_vertex(20, 1).unwrap();
    let v30 = c.add_vertex(30, 1).unwrap();
    c.add_edge(&cek(v1, 1, v10).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
    c.add_edge(&cek(v1, 1, v30).out_key()).unwrap();

    let result = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, Some(2));
    assert_eq!(result.len(), 2);
}

#[test]
fn get_edges_merges_committed_and_dirty() {
    let (store, _dir) = open();

    // Commit one edge, then add another in a new context.
    let (v1, v10, v20) = {
        let mut c0 = ctx(&store);
        let v_1 = c0.add_vertex(1, 1).unwrap();
        let v_10 = c0.add_vertex(10, 1).unwrap();
        let v_20 = c0.add_vertex(20, 1).unwrap();
        c0.commit().unwrap();
        (v_1, v_10, v_20)
    };

    let k1 = cek(v1, 1, v10);
    {
        let mut c = ctx(&store);
        c.add_edge(&k1.out_key()).unwrap();
        c.commit().unwrap();
    }

    let mut c = ctx(&store);
    c.add_edge(&cek(v1, 1, v20).out_key()).unwrap();
    let edges = get_adjacent_edges_test(&mut c, v1, Direction::OUT, None, None, None);
    assert_eq!(edges.len(), 2);
}

// ── Concurrency & Conflict Test Matrix ────────────────────────────────────
//
// This matrix documents the test coverage for Optimistic Concurrency Control
// (OCC) conflicts. It shows which concurrent operations on the same or
// related elements are tested to guarantee a `StoreError::Conflict` on `commit()`.
// Both commit orders (Txn1 -> Txn2, and Txn2 -> Txn1) are tested for every cell
// in `conflict_matrix`, alongside specific handmade tests.
//
// | Txn 1 \ Txn 2   | Add Edge       | Drop Edge      | Set Prop(E)    | Drop Prop(E)   | Set Prop(V)    | Drop Prop(V)   | Drop Vertex    |
// |-----------------|----------------|----------------|----------------|----------------|----------------|----------------|----------------|
// | Add Edge        | [1], [20]      | [2], [21]      | N/A            | N/A            | [3]            | [4]            | [5], [22..25]  |
// | Drop Edge       | [2], [21]      | [6]            | [7], [26,27]   | [8]            | [9]            | [10]           | N/A            |
// | Set Prop(E)     | N/A            | [7], [26,27]   | [11], [28]     | [12], [29,30]  | N/A            | N/A            | N/A            |
// | Drop Prop(E)    | N/A            | [8]            | [12], [29,30]  | [13]           | N/A            | N/A            | N/A            |
// | Set Prop(V)     | [3]            | [9]            | N/A            | N/A            | [14], [31]     | [15], [32,33]  | [16]           |
// | Drop Prop(V)    | [4]            | [10]           | N/A            | N/A            | [15], [32,33]  | [17]           | [18]           |
// | Drop Vertex     | [5], [22..25]  | N/A            | N/A            | N/A            | [16]           | [18]           | [19]           |
//
// ── Automated conflict_matrix tests:
// [1]  add_edge_vs_add_edge
// [2]  add_edge_vs_drop_edge
// [3]  add_edge_vs_set_vertex_property
// [4]  add_edge_vs_drop_vertex_property
// [5]  add_edge_vs_drop_vertex
// [6]  drop_edge_vs_drop_edge
// [7]  drop_edge_vs_set_edge_property
// [8]  drop_edge_vs_drop_edge_property
// [9]  drop_edge_vs_set_vertex_property
// [10] drop_edge_vs_drop_vertex_property
// [11] set_edge_property_vs_set_edge_property
// [12] set_edge_property_vs_drop_edge_property
// [13] drop_edge_property_vs_drop_edge_property
// [14] set_vertex_property_vs_set_vertex_property
// [15] set_vertex_property_vs_drop_vertex_property
// [16] set_vertex_property_vs_drop_vertex
// [17] drop_vertex_property_vs_drop_vertex_property
// [18] drop_vertex_property_vs_drop_vertex
// [19] drop_vertex_vs_drop_vertex
//
// ── Handmade concurrent tests:
// [20] add_edge_vs_add_edge_handmade
// [21] add_edge_vs_drop_edge_handmade
// [22] drop_vertex_vs_add_edge_handmade
// [23] add_edge_vs_drop_vertex_handmade
// [24] drop_dst_vertex_vs_add_edge_handmade
// [25] add_edge_vs_drop_dst_vertex_handmade
// [26] set_edge_property_vs_drop_edge_handmade
// [27] drop_edge_vs_set_edge_property_handmade
// [28] set_edge_property_vs_set_edge_property_handmade
// [29] drop_edge_property_vs_set_edge_property_handmade
// [30] set_edge_property_vs_drop_edge_property_handmade
// [31] set_vertex_property_vs_set_vertex_property_handmade
// [32] drop_vertex_property_vs_set_vertex_property_handmade
// [33] set_vertex_property_vs_drop_vertex_property_handmade
//
// N/A: Combinations that don't conflict (mutate distinct elements without read dependencies)
// or are impossible (e.g. dropping a vertex with an existing edge fails validation early).
// ──────────────────────────────────────────────────────────────────────────

mod conflict_matrix {
    use super::*;

    fn run_non_conflict<State: Copy, Setup, Op1, Op2>(setup: Setup, op1: Op1, op2: Op2)
    where
        Setup: Fn(&mut LogicalGraph<RocksStorage>) -> State,
        Op1: Fn(&mut LogicalGraph<RocksStorage>, State),
        Op2: Fn(&mut LogicalGraph<RocksStorage>, State),
    {
        // Order 1: Txn1 commits, Txn2 conflicts
        {
            let (store, _dir) = open();
            let mut c0 = ctx(&store);
            let state = setup(&mut c0);
            c0.commit().unwrap();

            let mut c1 = ctx(&store);
            let mut c2 = ctx(&store);

            op1(&mut c1, state);
            op2(&mut c2, state);

            c1.commit().unwrap();
            let res = c2.commit();
            assert!(res.is_ok(), "unexpected conflict in non-conflicting operations. Order 1 (Txn1 commits, Txn2 should succeed) failed with error: {:?}", res.err());
        }

        // Order 2: Txn2 commits, Txn1 conflicts
        {
            let (store, _dir) = open();
            let mut c0 = ctx(&store);
            let state = setup(&mut c0);
            c0.commit().unwrap();

            let mut c1 = ctx(&store);
            let mut c2 = ctx(&store);

            op1(&mut c1, state);
            op2(&mut c2, state);

            c2.commit().unwrap();
            let res = c1.commit();
            assert!(res.is_ok(), "unexpected conflict in non-conflicting operations. Order 2 (Txn2 commits, Txn1 should succeed) failed with error: {:?}", res.err());
        }
    }

    fn run_conflict<State: Copy, Setup, Op1, Op2>(setup: Setup, op1: Op1, op2: Op2)
    where
        Setup: Fn(&mut LogicalGraph<RocksStorage>) -> State,
        Op1: Fn(&mut LogicalGraph<RocksStorage>, State),
        Op2: Fn(&mut LogicalGraph<RocksStorage>, State),
    {
        // Order 1: Txn1 commits, Txn2 conflicts
        {
            let (store, _dir) = open();
            let mut c0 = ctx(&store);
            let state = setup(&mut c0);
            c0.commit().unwrap();

            let mut c1 = ctx(&store);
            let mut c2 = ctx(&store);

            op1(&mut c1, state);
            op2(&mut c2, state);

            c1.commit().unwrap();
            let res = c2.commit();
            assert!(
                matches!(res, Err(StoreError::Conflict)),
                "Order 1 (Txn1 commits, Txn2 conflicts) failed. Expected Conflict, got {:?}",
                res
            );
        }

        // Order 2: Txn2 commits, Txn1 conflicts
        {
            let (store, _dir) = open();
            let mut c0 = ctx(&store);
            let state = setup(&mut c0);
            c0.commit().unwrap();

            let mut c1 = ctx(&store);
            let mut c2 = ctx(&store);

            op1(&mut c1, state);
            op2(&mut c2, state);

            c2.commit().unwrap();
            let res = c1.commit();
            assert!(
                matches!(res, Err(StoreError::Conflict)),
                "Order 2 (Txn2 commits, Txn1 conflicts) failed. Expected Conflict, got {:?}",
                res
            );
        }
    }

    #[test]
    fn add_edge_vs_add_edge() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                (v1, v2)
            },
            |c, (v1, v2)| {
                c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
            },
            |c, (v1, v2)| {
                c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
            },
        );
    }

    #[test]
    fn add_edge_vs_add_edge_with_same_vertex() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                (v1, v2, v3)
            },
            |c, (v1, v2, _v3)| {
                c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
            },
            |c, (v1, _v2, v3)| {
                c.add_edge(&cek(v1, 5, v3).out_key()).unwrap();
            },
        );
    }

    #[test]
    fn add_edge_vs_drop_edge() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                let e1 = cek(v1, 5, v2);
                c.add_edge(&e1.out_key()).unwrap();
                (v1, e1, v3)
            },
            |c, (v1, _, v3)| {
                c.add_edge(&cek(v1, 6, v3).out_key()).unwrap();
            },
            |c, (_, e1, _)| {
                c.get_edge(&e1.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e1)).unwrap();
            },
        );
    }

    #[test]
    fn add_edge_vs_drop_edge_with_same_vertex() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                let e1 = cek(v1, 5, v2);
                c.add_edge(&e1.out_key()).unwrap();
                (v1, e1, v3)
            },
            |c, (v1, _, v3)| {
                c.add_edge(&cek(v1, 6, v3).out_key()).unwrap();
            },
            |c, (_, e1, _)| {
                c.get_edge(&e1.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e1)).unwrap();
            },
        );
    }

    #[test]
    fn add_edge_vs_set_vertex_property() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                (v1, v2)
            },
            |c, (v1, v2)| {
                c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
            },
            |c, (v1, _)| {
                c.get_vertex(v1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn add_edge_vs_drop_vertex_property() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                (v1, v2)
            },
            |c, (v1, v2)| {
                c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
            },
            |c, (v1, _)| {
                c.get_vertex(v1).unwrap();
                c.drop_property(&Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Null }).unwrap();
            },
        );
    }

    #[test]
    fn add_edge_vs_drop_vertex() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                (v1, v2)
            },
            |c, (v1, v2)| {
                c.add_edge(&cek(v1, 5, v2).out_key()).unwrap();
            },
            |c, (_, v2)| {
                c.get_vertex(v2).unwrap();
                c.drop_element(&CanonicalKey::Vertex(v2)).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_vs_drop_edge() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                e
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e)).unwrap();
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e)).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_vs_drop_edge_with_same_vertex() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                let e = cek(v1, 5, v2);
                let e2 = cek(v1, 6, v3);
                c.add_edge(&e.out_key()).unwrap();
                c.add_edge(&e2.out_key()).unwrap();
                (e, e2)
            },
            |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e1.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e1)).unwrap();
            },
            |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e2.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e2)).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_vs_set_edge_property() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                e
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e)).unwrap();
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_vs_drop_edge_property() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                e
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e)).unwrap();
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_property(&Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null }).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_vs_set_vertex_property() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                (v1, e)
            },
            |c, (_, e)| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e)).unwrap();
            },
            |c, (v1, _)| {
                c.get_vertex(v1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_vs_drop_vertex_property() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                (v1, e)
            },
            |c, (_, e)| {
                c.get_edge(&e.out_key()).unwrap();
                c.drop_element(&CanonicalKey::Edge(e)).unwrap();
            },
            |c, (v1, _)| {
                c.get_vertex(v1).unwrap();
                c.drop_property(&Property { owner: CanonicalKey::Vertex(v1), key: 6, value: Primitive::Null }).unwrap();
            },
        );
    }

    #[test]
    fn set_edge_property_vs_set_edge_property() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                e
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(2) };
                c.set_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn set_edge_property_vs_set_edge_property_with_same_vertex() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                let e = cek(v1, 5, v2);
                let e2 = cek(v1, 6, v3);
                c.add_edge(&e.out_key()).unwrap();
                c.add_edge(&e2.out_key()).unwrap();
                (e, e2)
            },
            |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e1.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e1), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
            |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e2.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Int32(2) };
                c.set_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn set_edge_property_vs_drop_edge_property() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                e
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn set_edge_property_vs_drop_edge_property_with_same_vertex() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                let e = cek(v1, 5, v2);
                let e2 = cek(v1, 6, v3);
                c.add_edge(&e.out_key()).unwrap();
                c.add_edge(&e2.out_key()).unwrap();
                let prop1 = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                let prop2 = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Int32(2) };
                c.set_property(&prop1).unwrap();
                c.set_property(&prop2).unwrap();
                (e, e2)
            },
            |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e1.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e1), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
            |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e2.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_property_vs_drop_edge_property() {
        run_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let e = cek(v1, 5, v2);
                c.add_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                e
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
            |c, e| {
                c.get_edge(&e.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn drop_edge_property_vs_drop_edge_property_with_same_vertex() {
        run_non_conflict(
            |c| {
                let v1 = c.add_vertex(1, 1).unwrap();
                let v2 = c.add_vertex(2, 1).unwrap();
                let v3 = c.add_vertex(3, 1).unwrap();
                let e = cek(v1, 5, v2);
                let e2 = cek(v1, 6, v3);
                c.add_edge(&e.out_key()).unwrap();
                c.add_edge(&e2.out_key()).unwrap();
                let prop1 = Property { owner: CanonicalKey::Edge(e), key: 6, value: Primitive::Int32(1) };
                let prop2 = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Int32(2) };
                c.set_property(&prop1).unwrap();
                c.set_property(&prop2).unwrap();
                (e, e2)
            },
            |c, (e1, _e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e1.out_key()).unwrap();
                let val = c.get_value(&CanonicalKey::Edge(e1), 6).unwrap();
                assert_eq!(val, Some(Primitive::Int32(1)));
                let prop = Property { owner: CanonicalKey::Edge(e1), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
            |c, (_e1, e2): (CanonicalEdgeKey, CanonicalEdgeKey)| {
                c.get_edge(&e2.out_key()).unwrap();
                let prop = Property { owner: CanonicalKey::Edge(e2), key: 7, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn set_vertex_property_vs_set_vertex_property() {
        run_conflict(
            |c| c.add_vertex(100, 1).unwrap(),
            |c, v| {
                c.get_vertex(v).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(2) };
                c.set_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn set_vertex_property_vs_drop_vertex_property() {
        run_conflict(
            |c| {
                let v = c.add_vertex(100, 1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                v
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(2) };
                c.set_property(&prop).unwrap();
            },
            |c, v| {
                c.get_vertex(v).unwrap().unwrap();
                let val = c.get_value(&CanonicalKey::Vertex(v), 6).unwrap();
                assert_eq!(val, Some(Primitive::Int32(1)));
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null };
                c.drop_property(&prop).unwrap();
            },
        );
    }

    #[test]
    fn set_vertex_property_vs_drop_vertex() {
        run_conflict(
            |c| c.add_vertex(100, 1).unwrap(),
            |c, v| {
                c.get_vertex(v).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
            },
        );
    }

    #[test]
    fn drop_vertex_property_vs_drop_vertex_property() {
        run_conflict(
            |c| {
                let v = c.add_vertex(100, 1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                v
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_property(&Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null }).unwrap();
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_property(&Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null }).unwrap();
            },
        );
    }

    #[test]
    fn drop_vertex_property_vs_drop_vertex() {
        run_conflict(
            |c| {
                let v = c.add_vertex(100, 1).unwrap();
                let prop = Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Int32(1) };
                c.set_property(&prop).unwrap();
                v
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_property(&Property { owner: CanonicalKey::Vertex(v), key: 6, value: Primitive::Null }).unwrap();
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
            },
        );
    }

    #[test]
    fn drop_vertex_vs_drop_vertex() {
        run_conflict(
            |c| c.add_vertex(100, 1).unwrap(),
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
            },
            |c, v| {
                c.get_vertex(v).unwrap();
                c.drop_element(&CanonicalKey::Vertex(v)).unwrap();
            },
        );
    }
}

// ── Integration tests ─────────────────────────────────────────────────────

#[test]
fn sequential_contexts_accumulate_edges() {
    let (store, _dir) = open();

    // Build edges in separate contexts; each must see all previously committed edges.
    let hub = {
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.commit().unwrap();
        key
    };

    let spokes: Vec<i64> = (0..4)
        .map(|i| {
            let mut c = ctx(&store);
            let key = c.add_vertex(i, 1).unwrap();
            c.add_edge(&cek(hub, 1, key).out_key()).unwrap();
            c.commit().unwrap();
            key
        })
        .collect();

    // A final context must see all 4 outgoing edges from hub.
    let mut c = ctx(&store);
    let out = get_adjacent_edges_test(&mut c, hub, Direction::OUT, Some(1), None, None);
    assert_eq!(out.len(), 4);

    // check vertex counter is correct after multiple contexts
    let (out_e, in_e, _label) = c.vertex_degree_for_test(hub).unwrap().unwrap();
    assert_eq!(out_e, 4);
    assert_eq!(in_e, 0);

    // The 4 edges must land at the 4 spoke vertices.
    let mut dst_ids: Vec<i64> = out.iter().map(|ek| ek.secondary_id).collect();
    dst_ids.sort_unstable();
    let mut expected = spokes.clone();
    expected.sort_unstable();
    assert_eq!(dst_ids, expected);

    // Each spoke has exactly one incoming edge from hub.
    for &spoke in &spokes {
        let in_edges = get_adjacent_edges_test(&mut c, spoke, Direction::IN, Some(1), None, None);
        assert_eq!(in_edges.len(), 1);
        assert_eq!(in_edges[0].secondary_id, hub);
    }
}

#[test]
fn two_concurrent_contexts_build_graph_fourth_reads_all() {
    let (store, _dir) = open();

    // ctx1 — person: Alice
    let mut c1 = ctx(&store);
    let alice = {
        let key = c1.add_vertex(101, 1).unwrap();
        let name_prop =
            Property { owner: CanonicalKey::Vertex(key), key: 5, value: Primitive::String(SmolStr::new("Alice")) };
        c1.set_property(&name_prop).unwrap();
        let age_prop = Property { owner: CanonicalKey::Vertex(key), key: 4, value: Primitive::Int32(30) };
        c1.set_property(&age_prop).unwrap();
        key
    };

    // ctx2 — person: Bob
    let mut c2 = ctx(&store);
    let bob = {
        let key = c2.add_vertex(102, 1).unwrap();
        let name_prop =
            Property { owner: CanonicalKey::Vertex(key), key: 5, value: Primitive::String(SmolStr::new("Bob")) };
        c2.set_property(&name_prop).unwrap();
        let age_prop = Property { owner: CanonicalKey::Vertex(key), key: 4, value: Primitive::Int32(25) };
        c2.set_property(&age_prop).unwrap();
        key
    };

    c2.commit().unwrap();
    c1.commit().unwrap(); // commit after c2 to test concurrent visibility of both contexts

    // ctx3 — city: London + two "lives_in" edges (label=2) from each person
    let london = {
        let mut c = ctx(&store);
        let city_key = c.add_vertex(201, 2).unwrap();
        let name_prop = Property {
            owner: CanonicalKey::Vertex(city_key),
            key: 5,
            value: Primitive::String(SmolStr::new("London")),
        };
        c.set_property(&name_prop).unwrap();
        // Alice -> London
        let e1 = cek(alice, 2, city_key);
        c.add_edge(&e1.out_key()).unwrap();
        let since_prop = Property { owner: CanonicalKey::Edge(e1), key: 11, value: Primitive::Int32(2015) };
        c.set_property(&since_prop).unwrap();
        // Bob -> London
        let e2 = cek(bob, 2, city_key);
        c.add_edge(&e2.out_key()).unwrap();
        let since_prop2 = Property { owner: CanonicalKey::Edge(e2), key: 11, value: Primitive::Int32(2019) };
        c.set_property(&since_prop2).unwrap();
        c.commit().unwrap();
        city_key
    };

    // ctx4 — read-only verification
    let mut c = ctx(&store);

    // Vertices survive across contexts.
    let _ = c.get_vertex(alice).unwrap().unwrap();
    assert_eq!(c.get_value(&CanonicalKey::Vertex(alice), 5).unwrap(), Some(Primitive::String(SmolStr::new("Alice"))));
    assert_eq!(c.get_value(&CanonicalKey::Vertex(alice), 4).unwrap(), Some(Primitive::Int32(30)));
    let (alice_out_e, alice_in_e, _label) = c.vertex_degree_for_test(alice).unwrap().unwrap();
    assert_eq!(alice_out_e, 1);
    assert_eq!(alice_in_e, 0);

    let _ = c.get_vertex(bob).unwrap().unwrap();
    assert_eq!(c.get_value(&CanonicalKey::Vertex(bob), 5).unwrap(), Some(Primitive::String(SmolStr::new("Bob"))));
    let (bob_out_e, bob_in_e, _label) = c.vertex_degree_for_test(bob).unwrap().unwrap();
    assert_eq!(bob_out_e, 1);
    assert_eq!(bob_in_e, 0);

    let _ = c.get_vertex(london).unwrap().unwrap();
    assert_eq!(c.get_value(&CanonicalKey::Vertex(london), 5).unwrap(), Some(Primitive::String(SmolStr::new("London"))));
    let (london_out_e, london_in_e, _label) = c.vertex_degree_for_test(london).unwrap().unwrap();
    assert_eq!(london_out_e, 0);
    assert_eq!(london_in_e, 2);

    // Both outgoing "lives_in" edges from Alice land at London.
    let alice_out = get_adjacent_edges_test(&mut c, alice, Direction::OUT, Some(2), None, None);
    assert_eq!(alice_out.len(), 1);
    let e_ek = alice_out[0];
    assert_eq!(e_ek.secondary_id, london);
    let since_val = c.get_value(&CanonicalKey::Edge(e_ek.canonical_edge_key()), 11).unwrap();
    assert_eq!(since_val, Some(Primitive::Int32(2015)));

    // London has two incoming edges: one from Alice, one from Bob.
    let london_in = get_adjacent_edges_test(&mut c, london, Direction::IN, Some(2), None, None);
    assert_eq!(london_in.len(), 2);
    let mut src_ids: Vec<i64> = london_in.iter().map(|ek| ek.secondary_id).collect();
    src_ids.sort_unstable();
    assert_eq!(src_ids, vec![alice.min(bob), alice.max(bob)]);
}

// Tests that operations depending on vertex counters (like adding an edge or dropping the vertex)
// succeed during the transaction due to snapshot isolation but fail with a Conflict at commit time
// when the vertex is deleted concurrently by another transaction.
#[test]
fn concurrent_vertex_deletion_fails_dependent_operations() {
    let (store, _dir) = open();

    // step 1, insert a vertex and set properties, commit the transaction txn1
    let mut txn1 = ctx(&store);
    let v1 = txn1.add_vertex(1, 1).unwrap();
    txn1.add_vertex(2, 1).unwrap();
    let v3 = txn1.add_vertex(3, 1).unwrap();
    let name_prop =
        Property { owner: CanonicalKey::Vertex(v1), key: 5, value: Primitive::String(SmolStr::new("Alice")) };
    txn1.set_property(&name_prop).unwrap();
    txn1.commit().unwrap();

    // step 2, in a new Transaction txn2, get_vertex
    let mut txn2 = ctx(&store);
    assert!(txn2.get_vertex(v1).unwrap().is_some());
    assert!(txn2.get_vertex(v3).unwrap().is_some());

    // step 3, the vertices were deleted in another transaction, commit the deleting transaction which should
    // succeed
    let mut txn3 = ctx(&store);
    txn3.drop_element(&CanonicalKey::Vertex(v1)).unwrap();
    txn3.drop_element(&CanonicalKey::Vertex(v3)).unwrap();
    txn3.commit().unwrap();

    // Under Repeatable Reads, adding an edge in txn2 using the vertex (which is still visible in txn2's snapshot)
    // should succeed
    assert!(txn2.add_edge(&cek(v1, 5, 2).out_key()).is_ok());

    // Similarly, dropping v3 in txn2 (still visible, degree 0) should succeed
    assert!(txn2.drop_element(&CanonicalKey::Vertex(v3)).is_ok());

    // But when txn2 tries to commit, it should fail with Conflict due to the concurrent deletion committed by txn3
    let commit_err = txn2.commit();
    assert!(matches!(commit_err, Err(StoreError::Conflict)));
}

#[test]
fn test_logical_scan_vertices_overlays() {
    let (store, _dir) = open();

    // 1. Add some committed vertices: 1, 2, 3
    let mut txn = ctx(&store);
    txn.add_vertex(1, 1).unwrap();
    txn.add_vertex(2, 1).unwrap();
    txn.add_vertex(3, 1).unwrap();
    txn.commit().unwrap();

    // 2. Start a new transaction. Add 4 (dirty new), delete 2 (tombstone)
    let mut txn = ctx(&store);
    txn.add_vertex(4, 1).unwrap();
    txn.drop_element(&CanonicalKey::Vertex(2)).unwrap();

    // 3. Scan vertices with limit 2
    let (batch1, cursor1) = txn.scan_vertices(None, None, 2).unwrap();
    assert_eq!(batch1, vec![1]);
    assert_eq!(cursor1, Some(2));

    // 4. Scan next batch using cursor1
    let (batch2, cursor2) = txn.scan_vertices(None, cursor1, 2).unwrap();
    assert_eq!(batch2, vec![3, 4]);
    assert_eq!(cursor2, None);
}

#[test]
fn test_logical_scan_edges_overlays() {
    let (store, _dir) = open();

    // 1. Add some committed vertices and edges
    let mut txn = ctx(&store);
    txn.add_vertex(1, 1).unwrap();
    txn.add_vertex(2, 1).unwrap();
    txn.add_vertex(3, 1).unwrap();

    let ek1 = cek(1, 10, 2).out_key();
    let ek2 = cek(2, 10, 3).out_key();
    let ek3 = cek(1, 10, 3).out_key();

    txn.add_edge(&ek1).unwrap();
    txn.add_edge(&ek2).unwrap();
    txn.add_edge(&ek3).unwrap();
    txn.commit().unwrap();

    // 2. Start a new transaction. Add ek4 (dirty), delete ek2 (tombstone)
    let mut txn = ctx(&store);
    let ek4 = cek(2, 10, 1).out_key();
    txn.add_edge(&ek4).unwrap();

    // Edge must be loaded into memory before drop
    txn.get_edge(&ek2).unwrap().unwrap();
    txn.drop_element(&CanonicalKey::Edge(ek2.canonical_edge_key())).unwrap();

    // 3. Scan edges with limit 2
    let (batch1, cursor1) = txn.scan_edges(None, None, 2).unwrap();
    assert_eq!(batch1.len(), 2);
    assert_eq!(batch1[0], ek1);
    assert_eq!(batch1[1], ek3);
    assert_eq!(cursor1, Some(ek3.canonical_edge_key()));

    // 4. Scan next batch using cursor1
    let (batch2, cursor2) = txn.scan_edges(None, cursor1, 2).unwrap();
    assert_eq!(batch2.len(), 1);
    assert_eq!(batch2[0], ek4);
    assert_eq!(cursor2, None);
}

#[test]
fn test_logical_get_adjacent_edges_overlays() {
    let (store, _dir) = open();

    // 1. Add some committed vertices and edges from vertex 1
    let mut txn = ctx(&store);
    txn.add_vertex(1, 1).unwrap();
    txn.add_vertex(2, 1).unwrap();
    txn.add_vertex(3, 1).unwrap();
    txn.add_vertex(4, 1).unwrap();

    let ek1 = cek(1, 10, 2).out_key();
    let ek2 = cek(1, 10, 3).out_key();

    txn.add_edge(&ek1).unwrap();
    txn.add_edge(&ek2).unwrap();
    txn.commit().unwrap();

    // 2. Start a new transaction. Add ek3 (dirty), delete ek2 (tombstone)
    let mut txn = ctx(&store);
    let ek3 = cek(1, 10, 4).out_key();
    txn.add_edge(&ek3).unwrap();

    // Edge must be loaded into memory before drop
    txn.get_edge(&ek2).unwrap().unwrap();
    txn.drop_element(&CanonicalKey::Edge(ek2.canonical_edge_key())).unwrap();

    // 3. Scan adjacent edges with limit 1
    let opts = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: None };
    let (batch1, cursor1) = txn.get_adjacent_edges(1, Direction::OUT, opts, Some(1)).unwrap();
    assert_eq!(batch1.len(), 1);
    assert_eq!(batch1[0], ek1);
    assert!(cursor1.is_some());

    // 4. Scan next batch using cursor1
    let opts2 = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: cursor1 };
    let (batch2, cursor2) = txn.get_adjacent_edges(1, Direction::OUT, opts2, Some(1)).unwrap();
    // Since ek2 is tombstoned and the DB scan hit limit 1, ek3 is excluded as it is > ek2.
    // So batch2 is empty, but cursor2 is Some(ek2).
    assert_eq!(batch2.len(), 0);
    assert!(cursor2.is_some());

    // 5. Scan third batch using cursor2
    let opts3 = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: cursor2 };
    let (batch3, cursor3) = txn.get_adjacent_edges(1, Direction::OUT, opts3, Some(1)).unwrap();
    // Now database scan reaches the end (cursor is None), so ek3 is included and returned.
    assert_eq!(batch3.len(), 1);
    assert_eq!(batch3[0], ek3);
    assert_eq!(cursor3, None);
}

#[test]
fn test_concurrent_scan_isolation() {
    let (store, _dir) = open();

    // 1. Add some initial committed vertices and edges
    let mut txn = ctx(&store);
    txn.add_vertex(1, 1).unwrap();
    txn.add_vertex(2, 1).unwrap();
    let ek1 = cek(1, 10, 2).out_key();
    txn.add_edge(&ek1).unwrap();
    txn.commit().unwrap();

    // 2. Start Transaction 1. This captures a snapshot.
    let mut txn1 = ctx(&store);

    // Perform first paginated scans (limit 1)
    let (v_batch1, v_cursor1) = txn1.scan_vertices(None, None, 1).unwrap();
    assert_eq!(v_batch1, vec![1]);
    assert!(v_cursor1.is_some());

    let opts = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: None };
    let (e_batch1, e_cursor1) = txn1.get_adjacent_edges(1, Direction::OUT, opts, Some(1)).unwrap();
    assert_eq!(e_batch1.len(), 1);
    assert_eq!(e_batch1[0], ek1);

    // 3. Start Transaction 2 concurrently. Add vertex 3 and edge 1 -> 10 -> 3, then commit it.
    let mut txn2 = ctx(&store);
    txn2.add_vertex(3, 1).unwrap();
    let ek2 = cek(1, 10, 3).out_key();
    txn2.add_edge(&ek2).unwrap();
    txn2.commit().unwrap();

    // 4. Continue pagination in Transaction 1.
    // Under Snapshot Isolation, subsequent pagination requests do NOT see
    // concurrently committed inserts that occurred after Transaction 1 started.
    let (v_batch2, v_cursor2) = txn1.scan_vertices(None, v_cursor1, 1).unwrap();
    assert_eq!(v_batch2, vec![2]);
    assert_eq!(v_cursor2, Some(2));

    // A third scan reaches the end of the snapshot (vertex 3 is isolated/invisible)
    let (v_batch2_next, v_cursor2_next) = txn1.scan_vertices(None, v_cursor2, 1).unwrap();
    assert_eq!(v_batch2_next.len(), 0);
    assert_eq!(v_cursor2_next, None);

    let opts2 = AdjacentEdgesOptions { label: None, dst: None, rank: None, start_from: e_cursor1 };
    let (e_batch2, e_cursor2) = txn1.get_adjacent_edges(1, Direction::OUT, opts2, Some(1)).unwrap();
    // The concurrently committed edge ek2 is not visible (isolated)
    assert_eq!(e_batch2.len(), 0);
    assert_eq!(e_cursor2, None);

    // 5. Start a new Transaction 3. It should see vertex 3 and edge ek2.
    let mut txn3 = ctx(&store);
    let (v_batch3, _) = txn3.scan_vertices(None, None, 10).unwrap();
    assert!(v_batch3.contains(&3));

    let (e_batch3, _) = txn3.get_adjacent_edges(1, Direction::OUT, opts, Some(10)).unwrap();
    assert!(e_batch3.contains(&ek2));
}

#[test]
fn test_snapshot_scan_isolation() {
    let (store, _dir) = open();

    // 1. Add some initial committed vertices
    let mut txn = ctx(&store);
    txn.add_vertex(1, 1).unwrap();
    txn.add_vertex(2, 1).unwrap();
    txn.commit().unwrap();

    // 2. Open a read snapshot (LogicalSnapshot)
    // S::Snapshot represents the snapshot type. For RocksStorage, it's Snapshot.
    let mut snap = crate::graph::LogicalSnapshot::<RocksStorage>::new(
        store.snapshot(),
        std::sync::Arc::new(std::sync::RwLock::new(crate::schema::Schema::new())),
    );

    // Perform first paginated scan (limit 1)
    let (v_batch1, v_cursor1) = snap.scan_vertices(None, None, 1).unwrap();
    assert_eq!(v_batch1, vec![1]);

    // 3. Start a transaction concurrently to insert vertex 3 and commit it
    let mut txn2 = ctx(&store);
    txn2.add_vertex(3, 1).unwrap();
    txn2.commit().unwrap();

    // 4. Continue pagination in the snapshot
    // Unlike LogicalGraph transactions, the LogicalSnapshot MUST isolate us from concurrent inserts.
    // So it should NOT see vertex 3!
    let (v_batch2, v_cursor2) = snap.scan_vertices(None, v_cursor1, 1).unwrap();
    assert_eq!(v_batch2, vec![2]);
    assert_eq!(v_cursor2, Some(2)); // Hit limit 1, so cursor is Some(2)

    // A third scan reaches the end of the snapshot (vertex 3 is isolated)
    let (v_batch3, v_cursor3) = snap.scan_vertices(None, v_cursor2, 1).unwrap();
    assert_eq!(v_batch3.len(), 0);
    assert_eq!(v_cursor3, None);
}

// ── Phase 3: vertex label cache tests ────────────────────────────────────
//
// 1. Read-after-mutate through a LabelOnly entry

#[test]
fn labelonly_mutate_then_read_back() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    let prop = Property { owner: CanonicalKey::Vertex(x), key: 4, value: Primitive::Int32(42) };
    c.set_property(&prop).unwrap();

    let val = c.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(42)));
}

// 2. Mutating one property must not lose a different, pre-existing one

#[test]
fn labelonly_mutate_preserves_existing_property() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let name_prop = Property { owner: CanonicalKey::Vertex(x), key: 4, value: Primitive::Int32(10) };
    base.set_property(&name_prop).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    let score_prop = Property { owner: CanonicalKey::Vertex(x), key: 6, value: Primitive::Int32(99) };
    c.set_property(&score_prop).unwrap();

    let val = c.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(10)));
}

// 3. drop_property through a LabelOnly entry

#[test]
fn labelonly_drop_property() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let name_prop = Property { owner: CanonicalKey::Vertex(x), key: 4, value: Primitive::Int32(10) };
    base.set_property(&name_prop).unwrap();
    let temp_prop = Property { owner: CanonicalKey::Vertex(x), key: 6, value: Primitive::Int32(99) };
    base.set_property(&temp_prop).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    let drop = Property { owner: CanonicalKey::Vertex(x), key: 6, value: Primitive::Null };
    c.drop_property(&drop).unwrap();

    assert_eq!(c.get_value(&CanonicalKey::Vertex(x), 6).unwrap(), None);
    let val = c.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(10)));
}

// 4. Mutation through LabelOnly survives commit

#[test]
fn labelonly_mutation_survives_commit() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);
    let prop = Property { owner: CanonicalKey::Vertex(x), key: 4, value: Primitive::Int32(42) };
    c.set_property(&prop).unwrap();
    c.commit().unwrap();

    let mut fresh = ctx(&store);
    let val = fresh.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(42)));
}

// 8. LabelOnly placeholder never clobbers stronger data

#[test]
fn labelonly_never_clobbers_decoded() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let name_prop = Property { owner: CanonicalKey::Vertex(x), key: 4, value: Primitive::Int32(10) };
    base.set_property(&name_prop).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    // Fully load X via scan_vertices — brings it in as Raw/Decoded.
    let (verts, _) = c.scan_vertices(None, None, 10).unwrap();
    assert!(verts.contains(&x));
    // Confirm it's real by reading a property.
    let val = c.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(10)));

    // Traverse edge to X — cache_vertex_label must not downgrade.
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    // Property still works — entry was not downgraded to LabelOnly.
    let val2 = c.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val2, Some(Primitive::Int32(10)));
}

// 9. Tombstoned vertex not served from stale cache

#[test]
fn labelonly_tombstoned_vertex_not_served() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    // Traverse edge to X, caching it LabelOnly.
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    // Drop the edge so X has zero incident edges, then drop X.
    c.drop_element(&CanonicalKey::Edge(ek)).unwrap();
    c.drop_element(&CanonicalKey::Vertex(x)).unwrap();

    // X must report absent — not answer from the stale LabelOnly entry.
    assert!(c.get_vertex(x).unwrap().is_none());
}

// 10. scan_vertices upgrades LabelOnly in place

#[test]
fn labelonly_scan_vertices_upgrades_in_place() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let name_prop = Property { owner: CanonicalKey::Vertex(x), key: 4, value: Primitive::Int32(42) };
    base.set_property(&name_prop).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    // Cache X as LabelOnly via edge traversal.
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    // scan_vertices passes over X — must upgrade the LabelOnly entry.
    let (verts, _) = c.scan_vertices(None, None, 10).unwrap();
    assert!(verts.contains(&x));

    // Property access works — upgraded in place, no wasted fetch.
    let val = c.get_value(&CanonicalKey::Vertex(x), 4).unwrap();
    assert_eq!(val, Some(Primitive::Int32(42)));
}

// 13. ensure_vertex_props_loaded surfaces CorruptData on missing vertex

#[test]
fn labelonly_corrupt_data_on_missing_vertex() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    // Cache X as LabelOnly.
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    // Drop the edge and X, then commit so the store loses X entirely.
    c.drop_element(&CanonicalKey::Edge(ek)).unwrap();
    c.drop_element(&CanonicalKey::Vertex(x)).unwrap();
    c.commit().unwrap();

    // Fresh transaction: manually inject a LabelOnly placeholder for X,
    // whose underlying vertex no longer exists in the store.
    let mut c2 = ctx(&store);
    c2.vertices.insert(x, Vertex::label_only(x, 1));

    // Accessing a non-trivial property triggers ensure_vertex_props_loaded,
    // which must fail with CorruptData.
    let err = c2.get_value(&CanonicalKey::Vertex(x), 4);
    assert!(matches!(err, Err(StoreError::CorruptData(_))));
}

// ── Phase 4: remaining test plan items ──────────────────────────────────
//
// 5. Edge loaded (not created) in this tx, then mutated, commits with
//    correct labels on *both* physical rows.

#[test]
fn edge_mutated_commits_correct_labels_on_both_rows() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 2).unwrap();
    let ek = cek(x, 3, y);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let loaded = c.get_edge(&ek.out_key()).unwrap().unwrap();
    assert_eq!(loaded.primary_id, x);

    let prop = Property { owner: CanonicalKey::Edge(ek), key: 4, value: Primitive::Int32(42) };
    c.set_property(&prop).unwrap();
    c.commit().unwrap();

    let out_edge = store.get_edge(&ek.out_key()).unwrap().unwrap();
    assert_eq!(out_edge.dst_label, Some(2));
    let in_edge = store.get_edge(&ek.in_key()).unwrap().unwrap();
    assert_eq!(in_edge.src_label, Some(1));
}

// 5-fastpath: hasLabel() answers from cache without a store read.

#[test]
fn labelonly_haslabel_skips_store_read() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);

    let lbl = c.get_value(&CanonicalKey::Vertex(x), LABEL_KEY_ID).unwrap();
    assert_eq!(lbl, Some(Primitive::Int32(1)));
    assert!(c.vertices.get(&x).unwrap().is_label_only());
}

// 6-fastpath: indirect — outE().inV().hasLabel().

#[test]
fn labelonly_indirect_via_out_e_in_v() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 2).unwrap();
    let ek = cek(x, 3, y);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, x, Direction::OUT, Some(3), None, None);
    assert_eq!(edges.len(), 1);

    let lbl = c.get_value(&CanonicalKey::Vertex(y), LABEL_KEY_ID).unwrap();
    assert_eq!(lbl, Some(Primitive::Int32(2)));
    assert!(c.vertices.get(&y).unwrap().is_label_only());
}

// 7-fastpath: multi-hop — out().out().hasLabel().

#[test]
fn labelonly_multihop_label_correct() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let a = base.add_vertex(10, 1).unwrap();
    let b = base.add_vertex(20, 2).unwrap();
    let c = base.add_vertex(30, 3).unwrap();
    let ek1 = cek(a, 4, b);
    let ek2 = cek(b, 4, c);
    base.add_edge(&ek1.out_key()).unwrap();
    base.add_edge(&ek2.out_key()).unwrap();
    base.commit().unwrap();

    let mut tx = ctx(&store);
    let hop1 = get_adjacent_edges_test(&mut tx, a, Direction::OUT, Some(4), None, None);
    assert_eq!(hop1.len(), 1);
    let hop2 = get_adjacent_edges_test(&mut tx, b, Direction::OUT, Some(4), None, None);
    assert_eq!(hop2.len(), 1);

    let lbl = tx.get_value(&CanonicalKey::Vertex(c), LABEL_KEY_ID).unwrap();
    assert_eq!(lbl, Some(Primitive::Int32(3)));
}

// 11. Two concurrent transactions don't share each other's cache.

#[test]
fn labelonly_no_cross_txn_cache_leak() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut a = ctx(&store);
    let edges_a = get_adjacent_edges_test(&mut a, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges_a.len(), 1);

    let mut b = ctx(&store);
    assert!(!b.vertices.contains_key(&x));
    let lbl = b.get_value(&CanonicalKey::Vertex(x), LABEL_KEY_ID).unwrap();
    assert_eq!(lbl, Some(Primitive::Int32(1)));
}

// 12. Cache state doesn't leak across a commit() reuse.

#[test]
fn labelonly_cache_cleared_on_commit_reuse() {
    let (store, _dir) = open();
    let mut base = ctx(&store);
    let x = base.add_vertex(100, 1).unwrap();
    let y = base.add_vertex(200, 1).unwrap();
    let ek = cek(y, 2, x);
    base.add_edge(&ek.out_key()).unwrap();
    base.commit().unwrap();

    let mut c = ctx(&store);
    let edges = get_adjacent_edges_test(&mut c, y, Direction::OUT, Some(2), None, None);
    assert_eq!(edges.len(), 1);
    assert!(c.vertices.contains_key(&x));

    c.commit().unwrap();

    assert!(!c.vertices.contains_key(&x));
    let v = c.add_vertex(300, 1).unwrap();
    assert_eq!(c.get_vertex(v).unwrap(), Some(v));
}

#[test]
fn test_self_loop_degree_correct() {
    // Regression: add_edge(V→V) used two independent reads of vertex_degree[V]
    // before either insert. The second read returned the pre-increment value,
    // so the first insert's out_e_cnt was silently overwritten. Result: a self-loop
    // vertex reported out_e_cnt=0 but had 1 edge in edges_out CF.
    let (store, _dir) = open();
    let mut c = ctx(&store);

    // Add two vertices, then a self-loop on vertex 1.
    c.add_vertex(1, 1).unwrap();
    c.add_vertex(2, 1).unwrap();

    // Self-loop: src == dst
    let self_loop = cek(1, 10, 1); // V1 → V1
    c.add_edge(&self_loop.out_key()).unwrap();

    // Also a normal edge V1 → V2 to verify non-self-loop still works.
    let normal = cek(1, 10, 2); // V1 → V2
    c.add_edge(&normal.out_key()).unwrap();

    c.commit().unwrap();

    // After commit, verify degree via the CF.
    let mut r = ctx(&store);
    let (out1, in1, _) = r.vertex_degree_for_test(1).unwrap().unwrap();
    // V1 has 2 out-edges (self-loop + normal) and 1 in-edge (self-loop back to itself).
    assert_eq!(out1, 2, "V1 out-degree should be 2 (self-loop + normal edge)");
    assert_eq!(in1, 1, "V1 in-degree should be 1 (self-loop)");

    let (out2, in2, _) = r.vertex_degree_for_test(2).unwrap().unwrap();
    assert_eq!(out2, 0, "V2 out-degree should be 0");
    assert_eq!(in2, 1, "V2 in-degree should be 1 (normal edge from V1)");
}
