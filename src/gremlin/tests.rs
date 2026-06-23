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
        types::{BatchScenario, StoreError},
    };

    /// Populate the TinkerPop Modern Graph into an open transaction.
    /// Caller is responsible for committing.
    pub fn create_tinkerpop_modern_graph(tx: &mut TxSession) -> Result<(), StoreError> {
        tx.g().addV("person").property("id", 1i64).property("name", "marko").property("age", 29i32).next()?;
        tx.g().addV("person").property("id", 2i64).property("name", "vadas").property("age", 27i32).next()?;
        tx.g().addV("software").property("id", 3i64).property("name", "lop").property("lang", "java").next()?;
        tx.g().addV("person").property("id", 4i64).property("name", "josh").property("age", 32i32).next()?;
        tx.g().addV("software").property("id", 5i64).property("name", "ripple").property("lang", "java").next()?;
        tx.g().addV("person").property("id", 6i64).property("name", "peter").property("age", 35i32).next()?;

        tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).next()?;
        tx.g().addE("knows").from(1).to(4).property("weight", 1.0f64).next()?;
        tx.g().addE("created").from(1).to(3).property("weight", 0.4f64).next()?;
        tx.g().addE("created").from(4).to(5).property("weight", 1.0f64).next()?;
        tx.g().addE("created").from(4).to(3).property("weight", 0.4f64).next()?;
        tx.g().addE("created").from(6).to(3).property("weight", 0.2f64).next()?;
        Ok(())
    }

    fn setup_modern_graph() -> Graph {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        {
            let schema_arc = graph.schema();
            let mut schema = schema_arc.write().unwrap();
            schema.register_vertex_label("dummy").unwrap(); // 0
            schema.register_vertex_label("person").unwrap(); // 1
            schema.register_vertex_label("software").unwrap(); // 2
            schema.register_edge_label("dummy").unwrap(); // 0
            schema.register_edge_label("dummy2").unwrap(); // 1
            schema.register_edge_label("dummy3").unwrap(); // 2
            schema.register_edge_label("knows").unwrap(); // 3
            schema.register_edge_label("created").unwrap(); // 4
            schema.register_edge_label("friends").unwrap(); // 5
        }
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

        let ct = tx.g().V([1, 2, 3, 4, 5, 6]).outE(["knows", "created", "friends"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(6));

        let ct = tx.g().V([1, 2, 3, 4, 5, 6]).inE(["knows", "created", "friends"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(6));

        let ct = tx.g().V([1, 2, 3, 4, 5, 6]).both(["knows", "created", "friends"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(12));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel(["person", "software"])
            .outE(["created", "knows"])
            .r#where(__().otherV().hasLabel(["software"]))
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel(["person"])
            .bothE(["knows"])
            .otherV()
            .hasLabel(["person"])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel(["person"])
            .bothE(["knows"])
            .otherV()
            .hasLabel(["person"])
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
            .hasLabel(["person", "software"])
            .outE(["created", "knows"])
            .inV()
            .hasLabel(["person"])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(2));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel(["person", "software"])
            .outE(["created", "knows"])
            .r#where(__().otherV().hasLabel(["person"]))
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

        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).hasLabel(["person"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).hasLabel(["software"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(2));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel(["person", "software"])
            .bothE(["created", "knows", "friends"])
            .hasLabel(["created"])
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
            .hasLabel(["person"])
            .outE(["created"])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(ct, Value::Int64(4));

        let ct = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel(["person"])
            .out(["created"])
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
            .hasLabel(["person", "software"])
            .bothE(["created", "knows", "friends"])
            .hasLabel(["created"])
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
            .hasLabel(["person"])
            .union([__().outE(["created"]), __().outE(["knows"])])
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

        let results = tx.g().V([1]).bothE(["knows", "created", "friends"]).otherV().path().to_list().unwrap();

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

        let name_list = tx.g().V([1]).out(["knows", "created", "friends"]).values(["name"]).to_list().unwrap();

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

        // Key::Label → returns label name as String
        let label_val = tx.g().V([1]).values([Key::Label]).next().unwrap().unwrap();
        assert_eq!(label_val, Value::String("person".to_string()));

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
    fn test_label_decode_consistency_across_steps() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // .has(Key::Label, "person") routes through HasLabelStep (string-based label
        // resolution) and must match, equivalent to hasLabel(["person"]).
        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).has(Key::Label, "person").count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(4));

        // .has("label", "person") goes through `Key::Property("label")` -> `HasPropertyStep`,
        // which must decode the element's label to a string before comparing, exactly like
        // `.has(Key::Label, ..)` / `.values(["label"])` do.
        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).has("label", "person").count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(4), "has(\"label\", ..) should match by decoded label name, like hasLabel does");

        // .properties(["label"]) should yield a Property whose value is the decoded label
        // name (String), consistent with .values(["label"]) / .values([Key::Label]).
        let prop_val = tx.g().V([1]).properties(["label"]).next().unwrap().unwrap();
        if let Value::Property(p) = prop_val {
            assert_eq!(p.key, "label");
            assert_eq!(*p.value, Value::String("person".to_string()));
        } else {
            panic!("expected Value::Property, got {:?}", prop_val);
        }
    }

    #[test]
    fn test_tinkerpop_modern_coalesce_step() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let ct =
            tx.g().V([1]).coalesce([__().outE(["created"]), __().outE(["knows"])]).count().next().unwrap().unwrap();
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
                    __().addV("person").property("id", 1i64).property("name", "marko").property("age", 29i32),
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
                    __().addV("person").property("id", 1i64).property("name", "marko").property("age", 29i32),
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
                    __().addV("person").property("id", 10i64).property("name", "marko").property("age", 18i32),
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
                    __().addV("person").property("id", 10i64).property("name", "marko").property("age", 18i32),
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
            let knows_count = snap.g().V([1]).outE(["knows"]).count().next().unwrap().unwrap();
            assert_eq!(knows_count, Value::Int64(2));

            // Walk to other vertices
            let names = snap.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
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
            let knows_count = tx.g().V([1]).outE(["knows"]).count().next().unwrap().unwrap();
            assert_eq!(knows_count, Value::Int64(2));

            // Walk to other vertices
            let names = tx.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
            assert_eq!(names.len(), 2);
            assert!(names.contains(&Value::String("vadas".into())));
            assert!(names.contains(&Value::String("josh".into())));
        }
    }

    #[test]
    fn test_single_edge_mode_constraints() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        {
            let schema_arc = graph.schema();
            let mut schema = schema_arc.write().unwrap();
            schema.register_vertex_label("dummy").unwrap(); // 0
            schema.register_vertex_label("person").unwrap(); // 1
            schema.register_edge_label("dummy").unwrap(); // 0
            schema.register_edge_label("dummy2").unwrap(); // 1
            schema.register_edge_label("dummy3").unwrap(); // 2
            schema.register_edge_label("knows").unwrap(); // 3
        }

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();

        // Single-edge mode is active by default (multi_edge = false)
        // 1. Add first edge (default rank 0)
        tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).next().unwrap();

        // 2. Adding duplicate edge should fail with DuplicateEdge
        let res2 = tx.g().addE("knows").from(1).to(2).property("weight", 0.8f64).next();
        assert!(matches!(res2, Err(StoreError::DuplicateEdge(_))));

        // 3. Adding edge with non-zero rank should fail with UnsupportedOperation
        let res3 = tx.g().addE("knows").from(1).to(2).property("rank", 5i32).next();
        assert!(matches!(res3, Err(StoreError::UnsupportedOperation(_))));
    }

    #[test]
    fn test_value_conversions_and_helpers() {
        let v_bool = Value::Bool(true);
        let v_i32 = Value::Int32(42);
        let v_i64 = Value::Int64(100);
        let v_str = Value::String("hello".to_string());

        assert_eq!(v_bool.as_bool(), Some(true));
        assert_eq!(v_i32.as_i32(), Some(42));
        assert_eq!(v_i32.as_i64(), Some(42i64));
        assert_eq!(v_i64.as_i64(), Some(100i64));
        assert_eq!(v_str.as_str(), Some("hello"));

        let b: bool = v_bool.clone().try_into().unwrap();
        assert!(b);
        let i: i64 = v_i64.clone().try_into().unwrap();
        assert_eq!(i, 100);
        let s: String = v_str.clone().try_into().unwrap();
        assert_eq!(s, "hello");

        let err: Result<bool, _> = v_i64.try_into();
        assert!(err.is_err());
    }

    #[test]
    fn test_silent_step_failures_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        let mut tx = graph.begin();

        // 1. Manually writing property("label", ...) is a schema violation
        let res1 = tx.g().addV("person").property("label", "illegal").next();
        assert!(matches!(res1, Err(StoreError::SchemaViolation(_))));

        // 2. Writing non-scalar property value is a datatype error
        let res2 = tx.g().addV("person").property("complex", Value::List(vec![])).next();
        assert!(matches!(res2, Err(StoreError::UnexpectedDataType(_))));

        // 3. is() with range predicate is unsupported on scalar filter
        let res3 = tx.g().V([]).values(["age"]).is(crate::gremlin::value::gt(30i32)).next();
        assert!(matches!(res3, Err(StoreError::UnsupportedOperation(_))));
    }

    #[test]
    fn test_reserved_key_write_validation() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();
        let mut tx = graph.begin();

        // Misplaced "id" will not be folded, and compiling the physical plan must fail with SchemaViolation
        let res_id = tx.g().V([1]).property("id", 999i64).next();
        assert!(
            matches!(res_id, Err(StoreError::SchemaViolation(msg)) if msg.contains("Unfolded or misplaced reserved property key"))
        );

        // Misplaced "rank" will not be folded, and compiling the physical plan must fail with SchemaViolation
        let res_rank = tx.g().V([1]).property("rank", 1i64).next();
        assert!(
            matches!(res_rank, Err(StoreError::SchemaViolation(msg)) if msg.contains("Unfolded or misplaced reserved property key"))
        );

        // Explicitly setting "label" must fail with SchemaViolation early
        let res_label = tx.g().V([1]).property("label", "new_label").next();
        assert!(
            matches!(res_label, Err(StoreError::SchemaViolation(msg)) if msg.contains("Cannot manually set or update the reserved property 'label'"))
        );
    }
}
