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

#[cfg(test)]
mod integration_test {

    use crate::{
        api::{Graph, TxSession},
        gremlin::{
            traversal::{TraversalBuilder, __},
            value::{Key, Value},
        },
        types::{keys::LabelId, BatchScenario, StoreError},
    };

    const PERSON_LABEL_ID: LabelId = 1;
    const SOFTWARE_LABEL_ID: LabelId = 2;
    const KNOWS_LABEL_ID: LabelId = 3;
    const CREATED_LABEL_ID: LabelId = 4;
    const FRIENDS_LABEL_ID: LabelId = 5;

    /// Populate the TinkerPop Modern Graph into an open transaction.
    /// Caller is responsible for committing.
    pub fn create_tinkerpop_modern_graph(tx: &mut TxSession) -> Result<(), StoreError> {
        tx.g().addV(PERSON_LABEL_ID).property("id", 1i64).property("name", "marko").property("age", 29i32).next()?;
        tx.g().addV(PERSON_LABEL_ID).property("id", 2i64).property("name", "vadas").property("age", 27i32).next()?;
        tx.g().addV(SOFTWARE_LABEL_ID).property("id", 3i64).property("name", "lop").property("lang", "java").next()?;
        tx.g().addV(PERSON_LABEL_ID).property("id", 4i64).property("name", "josh").property("age", 32i32).next()?;
        tx.g()
            .addV(SOFTWARE_LABEL_ID)
            .property("id", 5i64)
            .property("name", "ripple")
            .property("lang", "java")
            .next()?;
        tx.g().addV(PERSON_LABEL_ID).property("id", 6i64).property("name", "peter").property("age", 35i32).next()?;

        tx.g().addE(KNOWS_LABEL_ID).from(1).to(2).property("weight", 0.5f64).next()?;
        tx.g().addE(KNOWS_LABEL_ID).from(1).to(4).property("weight", 1.0f64).next()?;
        tx.g().addE(CREATED_LABEL_ID).from(1).to(3).property("weight", 0.4f64).next()?;
        tx.g().addE(CREATED_LABEL_ID).from(4).to(5).property("weight", 1.0f64).next()?;
        tx.g().addE(CREATED_LABEL_ID).from(4).to(3).property("weight", 0.4f64).next()?;
        tx.g().addE(CREATED_LABEL_ID).from(6).to(3).property("weight", 0.2f64).next()?;
        Ok(())
    }

    fn setup_modern_graph() -> Graph {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        let mut tx = graph.begin();
        create_tinkerpop_modern_graph(&mut tx).unwrap();
        tx.commit().unwrap();
        // Leak the tempdir so the DB path remains valid for the test.
        // In practice, `Graph` outlives `dir` here because `dir` is returned
        // first from the tempdir but we need the path to stay valid.
        // Simplest workaround: box leak the dir.
        std::mem::forget(dir);
        graph
    }

    #[test]
    fn test_tinkerpop_modern_vertex_edge_count() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let count = tx.g().V([1, 2, 3, 4, 5, 6]).count().next().unwrap().unwrap();
        assert_eq!(count, Value::Int64(6));

        let ct = tx
            .g()
            .V([1, 2, 3, 4, 5, 6])
            .outE([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(6));

        let ct = tx
            .g()
            .V([1, 2, 3, 4, 5, 6])
            .inE([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(6));

        let ct = tx
            .g()
            .V([1, 2, 3, 4, 5, 6])
            .both([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(12));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .outE([CREATED_LABEL_ID, KNOWS_LABEL_ID])
            .r#where(__().otherV().hasLabel([SOFTWARE_LABEL_ID]))
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .bothE([KNOWS_LABEL_ID])
            .otherV()
            .hasLabel([PERSON_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .bothE([KNOWS_LABEL_ID])
            .otherV()
            .hasLabel([PERSON_LABEL_ID])
            .dedup()
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(3));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .outE([CREATED_LABEL_ID, KNOWS_LABEL_ID])
            .inV()
            .hasLabel([PERSON_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(2));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .outE([CREATED_LABEL_ID, KNOWS_LABEL_ID])
            .r#where(__().otherV().hasLabel([PERSON_LABEL_ID]))
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(2));
    }

    #[test]
    fn test_tinkerpop_modern_vertex_properties() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let ct =
            tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).values(["age", "name", "lang"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(12));
    }

    #[test]
    fn test_tinkerpop_modern_has_label() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).hasLabel([PERSON_LABEL_ID]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).hasLabel([SOFTWARE_LABEL_ID]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(2));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .bothE([CREATED_LABEL_ID, KNOWS_LABEL_ID, FRIENDS_LABEL_ID])
            .hasLabel([CREATED_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(8));
    }

    #[test]
    fn test_tinkerpop_modern_dedup() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .outE([CREATED_LABEL_ID])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .out([CREATED_LABEL_ID])
            .dedup()
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(2));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .bothE([CREATED_LABEL_ID, KNOWS_LABEL_ID, FRIENDS_LABEL_ID])
            .hasLabel([CREATED_LABEL_ID])
            .dedup()
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));
    }

    #[test]
    fn test_tinkerpop_modern_union() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .union([__().outE([CREATED_LABEL_ID]), __().outE([KNOWS_LABEL_ID])])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(6));
    }

    #[test]
    fn test_tinkerpop_modern_path_step() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let results = tx
            .g()
            .V([1])
            .bothE([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .otherV()
            .path()
            .to_list()
            .unwrap();

        assert_eq!(results.len(), 3);
        for res in results.iter() {
            if let Value::Path(p) = res {
                assert_eq!(p.len(), 3);
                if let Value::Vertex(v) = &p.objects[0] {
                    assert_eq!(v.id, 1);
                } else {
                    panic!("Expected vertex at path[0], got {:?}", &p.objects[0]);
                }
            } else {
                panic!("Expected path, got {:?}", res);
            }
        }
    }

    #[test]
    fn test_tinkerpop_modern_to_list_step() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let name_list =
            tx.g().V([1]).out([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID]).values(["name"]).to_list().unwrap();

        let mut names = Vec::new();
        for v in name_list.iter() {
            match v {
                Value::String(s) => names.push(s.clone()),
                _ => panic!("Expected string scalar, got {:?}", v),
            };
        }
        names.sort();
        assert_eq!(names.len(), 3);
        assert_eq!(names, vec!["josh", "lop", "vadas"]);
    }

    #[test]
    fn test_values_id_label_property_distinction() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // Key::Id → returns the vertex id as Int64
        let id_val = tx.g().V([1]).values([Key::Id]).next().unwrap().unwrap();
        assert_eq!(id_val, Value::Int64(1));

        // Key::Label → returns label_id as Int32
        let label_val = tx.g().V([1]).values([Key::Label]).next().unwrap().unwrap();
        assert_eq!(label_val, Value::Int32(PERSON_LABEL_ID as i32));

        // plain property key → returns the stored scalar
        let name_val = tx.g().V([1]).values(["name"]).next().unwrap().unwrap();
        assert_eq!(name_val, Value::String("marko".to_string()));

        // mixing id, label, and a property — count must be 3
        let ct = tx.g().V([1]).values([Key::Id, Key::Label, "name".into()]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(3));

        // .properties() returns Property elements for user-defined keys only
        let prop_val = tx.g().V([1]).properties(["name"]).next().unwrap().unwrap();
        if let Value::Property(p) = prop_val {
            assert_eq!(p.key, "name");
            assert_eq!(*p.value, Value::String("marko".to_string()));
        } else {
            panic!("expected Value::Property, got {:?}", prop_val);
        }

        // .has(Key::Id, n) filters by vertex id (routes through HasIdStep)
        let ct = tx.g().V([]).hasId([1, 2, 3]).has(Key::Id, 1i64).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(1));

        // .has("age", n) filters by property value
        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).has("age", 29i32).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(1));

        // Key::Id and Key::Label are NOT yielded by .properties() — only user props are
        let ct = tx.g().V([1]).properties(["name", "age"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(2));
    }

    #[test]
    fn test_tinkerpop_modern_coalesce_step() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let ct = tx
            .g()
            .V([1])
            .coalesce([__().outE([CREATED_LABEL_ID]), __().outE([KNOWS_LABEL_ID])])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(1));
    }

    #[test]
    fn test_tinkerpop_modern_coalesce_upsert_vertex() {
        let graph = setup_modern_graph();

        // Vertex 1 already exists → coalesce takes the values([...]) branch → 2 values
        {
            let mut tx = graph.begin();
            let Value::Int64(ct) = tx
                .g()
                .V([1])
                .coalesce([
                    __().V([1]).values(["name", "age"]),
                    __().addV(PERSON_LABEL_ID).property("id", 1i64).property("name", "marko").property("age", 29i32),
                ])
                .count()
                .next()
                .unwrap()
                .unwrap()
            else {
                panic!("unexpected result type")
            };
            assert_eq!(ct, 2);
            tx.commit().unwrap();
        }

        // Same check via Key::Label / Key::Id
        {
            let mut tx = graph.begin();
            let Value::Int64(ct) = tx
                .g()
                .V([1])
                .coalesce([
                    __().V([1]).values([Key::Label, Key::Id]),
                    __().addV(PERSON_LABEL_ID).property("id", 1i64).property("name", "marko").property("age", 29i32),
                ])
                .count()
                .next()
                .unwrap()
                .unwrap()
            else {
                panic!("unexpected result type")
            };
            assert_eq!(ct, 2);
            tx.commit().unwrap();
        }

        // Vertex 10 does not exist → coalesce takes the addV branch → 1 new vertex
        {
            let mut tx = graph.begin();
            let Value::Int64(ct) = tx
                .g()
                .V([10])
                .count()
                .coalesce([
                    __().V([10]).values(["name", "age"]),
                    __().addV(PERSON_LABEL_ID).property("id", 10i64).property("name", "marko").property("age", 18i32),
                ])
                .count()
                .next()
                .unwrap()
                .unwrap()
            else {
                panic!("unexpected result type")
            };
            assert_eq!(ct, 1);
            tx.commit().unwrap();
        }

        // Vertex 10 now exists → coalesce takes the values([...]) branch → 2 values
        {
            let mut tx = graph.begin();
            let Value::Int64(ct) = tx
                .g()
                .V([10])
                .count()
                .coalesce([
                    __().V([10]).values(["name", "age"]),
                    __().addV(PERSON_LABEL_ID).property("id", 10i64).property("name", "marko").property("age", 18i32),
                ])
                .count()
                .next()
                .unwrap()
                .unwrap()
            else {
                panic!("unexpected result type")
            };
            assert_eq!(ct, 2);
            tx.commit().unwrap();
        }
    }

    #[test]
    fn test_tinkerpop_modern_scan_v_and_scan_e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // g.V() scan (V with empty IDs)
        let v_count = tx.g().V([]).count().next().unwrap().unwrap();
        assert_eq!(v_count, Value::Int64(6));

        // g.E() scan (E with empty keys)
        let e_count = tx.g().E([]).count().next().unwrap().unwrap();
        assert_eq!(e_count, Value::Int64(6));
    }

    #[test]
    fn test_custom_batch_sizes_correctness() {
        let graph = setup_modern_graph();

        // Test with ReadSession
        {
            let mut snap = graph.read();
            snap.set_batch_size(BatchScenario::ScanVertices, 1);
            snap.set_batch_size(BatchScenario::ScanEdges, 1);
            snap.set_batch_size(BatchScenario::GetAdjacentEdges, 1);

            // Vertices scan
            let v_count = snap.g().V([]).count().next().unwrap().unwrap();
            assert_eq!(v_count, Value::Int64(6));

            // Edges scan
            let e_count = snap.g().E([]).count().next().unwrap().unwrap();
            assert_eq!(e_count, Value::Int64(6));

            // Adjacent edge expansions (e.g., marko -> knows)
            // marko is id 1. Outgoing knows edges count should be 2.
            let knows_count = snap.g().V([1]).outE([KNOWS_LABEL_ID]).count().next().unwrap().unwrap();
            assert_eq!(knows_count, Value::Int64(2));

            // Walk to other vertices
            let names = snap.g().V([1]).out([KNOWS_LABEL_ID]).values(["name"]).to_list().unwrap();
            assert_eq!(names.len(), 2);
            assert!(names.contains(&Value::String("vadas".into())));
            assert!(names.contains(&Value::String("josh".into())));
        }

        // Test with TxSession
        {
            let mut tx = graph.begin();
            tx.set_batch_size(BatchScenario::ScanVertices, 1);
            tx.set_batch_size(BatchScenario::ScanEdges, 1);
            tx.set_batch_size(BatchScenario::GetAdjacentEdges, 1);

            // Vertices scan
            let v_count = tx.g().V([]).count().next().unwrap().unwrap();
            assert_eq!(v_count, Value::Int64(6));

            // Edges scan
            let e_count = tx.g().E([]).count().next().unwrap().unwrap();
            assert_eq!(e_count, Value::Int64(6));

            // Adjacent edge expansions (e.g., marko -> knows)
            let knows_count = tx.g().V([1]).outE([KNOWS_LABEL_ID]).count().next().unwrap().unwrap();
            assert_eq!(knows_count, Value::Int64(2));

            // Walk to other vertices
            let names = tx.g().V([1]).out([KNOWS_LABEL_ID]).values(["name"]).to_list().unwrap();
            assert_eq!(names.len(), 2);
            assert!(names.contains(&Value::String("vadas".into())));
            assert!(names.contains(&Value::String("josh".into())));
        }
    }
}
