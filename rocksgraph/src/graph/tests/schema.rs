// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

// ── vertex label cache & schema tests ─────────────────────────────────────
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

// 5. Edge loaded (not created) in this txn, then mutated, commits with
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

    let mut txn = ctx(&store);
    let hop1 = get_adjacent_edges_test(&mut txn, a, Direction::OUT, Some(4), None, None);
    assert_eq!(hop1.len(), 1);
    let hop2 = get_adjacent_edges_test(&mut txn, b, Direction::OUT, Some(4), None, None);
    assert_eq!(hop2.len(), 1);

    let lbl = txn.get_value(&CanonicalKey::Vertex(c), LABEL_KEY_ID).unwrap();
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
