// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

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
    assert_eq!(fv.props().len(), 1);
    assert_eq!(fv.props().get(&5u16), Some(&Primitive::String(SmolStr::new("Alice"))));
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
    assert_eq!(e.props().len(), 1);
    assert_eq!(e.props().get(&11u16), Some(&Primitive::Int32(99)));
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
