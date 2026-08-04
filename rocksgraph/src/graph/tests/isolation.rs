// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

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
// [20] add_edge_vs_add_edge_handmade (add_edge_vs_add_same_edge_handmade)
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
        Setup: Fn(&mut LogicalGraph) -> State,
        Op1: Fn(&mut LogicalGraph, State),
        Op2: Fn(&mut LogicalGraph, State),
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
        Setup: Fn(&mut LogicalGraph) -> State,
        Op1: Fn(&mut LogicalGraph, State),
        Op2: Fn(&mut LogicalGraph, State),
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

// ── Handmade concurrent tests ─────────────────────────────────────────────

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

    // step 3, the vertices were deleted in another transaction, commit the deleting transaction which should succeed
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

// ── Scan Isolation tests ──────────────────────────────────────────────────

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
    let mut snap = crate::graph::LogicalSnapshot::new(
        store.snapshot(),
        std::sync::Arc::new(parking_lot::RwLock::new(crate::schema::Schema::new())),
        crate::vector::empty_vector_index_map(),
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
