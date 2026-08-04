// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

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

// ── Overlay scan tests ────────────────────────────────────────────────────

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
fn test_self_loop_degree_correct() {
    // Regression: add_edge(V→V) used two independent reads of vertex_degree[V]
    // before either insert. The second read returned the pre-increment value,
    // so the first insert's out_e_cnt was silently overwritten. Result: a self-loop
    // vertex reported out_e_cnt=0 but had 1 edge in edges_out CF.
    let (store, _dir) = open();
    let mut c = ctx(&store);

    let v1 = c.add_vertex(1, 1).unwrap();
    let v2 = c.add_vertex(2, 1).unwrap();

    // Normal edge V1 -> V2
    c.add_edge(&cek(v1, 10, v2).out_key()).unwrap();
    // Self-loop V1 -> V1
    c.add_edge(&cek(v1, 20, v1).out_key()).unwrap();

    // Verify uncommitted degree in overlay
    let (out_e, in_e, _) = c.vertex_degree_for_test(v1).unwrap().unwrap();
    assert_eq!(out_e, 2, "V1 out-degree should be 2 (self-loop + normal edge)");
    assert_eq!(in_e, 1, "V1 in-degree should be 1 (self-loop)");

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

// ── Gap coverage: G16-G21 (Blob-state mutation and commit invariants) ─────────

#[test]
fn g16_set_property_on_blob_vertex() {
    // Load a vertex from store (Blob state), mutate it, commit, verify.
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(1, 5).unwrap();
        c.set_property(&Property { owner: CanonicalKey::Vertex(key), key: 20, value: Primitive::Int32(30) }).unwrap();
        c.commit().unwrap();
        key
    };
    // tx2: vertex loads as Blob; mutation triggers Blob → Map.
    {
        let mut c = ctx(&store);
        c.set_property(&Property { owner: CanonicalKey::Vertex(id), key: 20, value: Primitive::Int32(31) }).unwrap();
        c.commit().unwrap();
    }
    let mut fv = store.get_vertex(id).unwrap().unwrap();
    assert_eq!(fv.props().get(&20), Some(&Primitive::Int32(31)));
}

#[test]
fn g17_drop_property_on_blob_vertex() {
    // Load vertex from store (Blob state), drop a property, commit, verify gone.
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(1, 5).unwrap();
        c.set_property(&Property { owner: CanonicalKey::Vertex(key), key: 20, value: Primitive::Int32(99) }).unwrap();
        c.set_property(&Property { owner: CanonicalKey::Vertex(key), key: 21, value: Primitive::Int32(88) }).unwrap();
        c.commit().unwrap();
        key
    };
    {
        let mut c = ctx(&store);
        c.drop_property(&Property { owner: CanonicalKey::Vertex(id), key: 20, value: Primitive::Null }).unwrap();
        c.commit().unwrap();
    }
    let mut fv = store.get_vertex(id).unwrap().unwrap();
    assert_eq!(fv.props().get(&20), None);
    assert_eq!(fv.props().get(&21), Some(&Primitive::Int32(88)));
}

#[test]
fn g18_set_property_on_empty_property_vertex() {
    // Vertex with no user properties — add the first one.
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(1, 5).unwrap();
        c.commit().unwrap();
        key
    };
    {
        let mut c = ctx(&store);
        c.set_property(&Property { owner: CanonicalKey::Vertex(id), key: 30, value: Primitive::Int64(42) }).unwrap();
        c.commit().unwrap();
    }
    let mut fv = store.get_vertex(id).unwrap().unwrap();
    assert_eq!(fv.props().get(&30), Some(&Primitive::Int64(42)));
}

#[test]
fn g19_get_value_on_empty_property_vertex_returns_none() {
    // Vertex with no user properties — get_value for any user key returns None.
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(1, 5).unwrap();
        c.commit().unwrap();
        key
    };
    let mut c = ctx(&store);
    assert_eq!(c.get_value(&CanonicalKey::Vertex(id), 99).unwrap(), None);
}

#[test]
fn g20_get_value_nonexistent_key_on_loaded_vertex() {
    // Request a key that doesn't exist in the stored blob → None, no error.
    let (store, _dir) = open();
    // prop key 20 is not pre-registered in open()'s schema (open() types keys 4–12 only).
    let present_key: u16 = 20;
    let absent_key: u16 = 21;
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(100, 1).unwrap();
        c.set_property(&Property { owner: CanonicalKey::Vertex(key), key: present_key, value: Primitive::Int32(42) })
            .unwrap();
        c.commit().unwrap();
        key
    };
    let mut c = ctx(&store);
    // present_key exists in the blob, absent_key does not — both return correct answers.
    assert_eq!(c.get_value(&CanonicalKey::Vertex(id), present_key).unwrap(), Some(Primitive::Int32(42)));
    assert_eq!(c.get_value(&CanonicalKey::Vertex(id), absent_key).unwrap(), None);
}

#[test]
fn g21_blob_vertex_not_dirtied_by_read() {
    // Reading a vertex (Blob state) must not mark it dirty; commit must not rewrite it.
    let (store, _dir) = open();
    let id = {
        let mut c = ctx(&store);
        let key = c.add_vertex(1, 5).unwrap();
        c.set_property(&Property { owner: CanonicalKey::Vertex(key), key: 10, value: Primitive::Int32(7) }).unwrap();
        c.commit().unwrap();
        key
    };
    {
        let mut c = ctx(&store);
        let _ = c.get_vertex(id).unwrap(); // loads as Blob
                                           // Blob-state reads must never mark the element dirty.
        assert!(!c.dirty.contains_key(&CanonicalKey::Vertex(id)));
        c.commit().unwrap(); // no-op for this vertex
    }
    // Vertex must be readable and correct after the no-op commit.
    let mut c = ctx(&store);
    assert_eq!(c.get_value(&CanonicalKey::Vertex(id), 10).unwrap(), Some(Primitive::Int32(7)));
}
