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
            value::Value,
        },
        types::{BatchScenario, StoreError},
    };
    use smol_str::SmolStr;

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

        // id() / label() — dedicated extraction steps
        let id_val = tx.g().V([1]).id().next().unwrap().unwrap();
        assert_eq!(id_val, Value::Int64(1));

        let label_val = tx.g().V([1]).label().next().unwrap().unwrap();
        assert_eq!(label_val, Value::String("person".to_string()));

        // plain property key → returns the stored scalar
        let name_val = tx.g().V([1]).values(["name"]).next().unwrap().unwrap();
        assert_eq!(name_val, Value::String("marko".to_string()));

        // "id"/"label" are reserved — values()/properties() reject them, must use
        // id()/label() instead.
        assert!(tx.g().V([1]).values(["id"]).next().is_err());
        assert!(tx.g().V([1]).values(["label"]).next().is_err());
        assert!(tx.g().V([1]).properties(["id"]).next().is_err());
        assert!(tx.g().V([1]).properties(["label"]).next().is_err());

        // .properties() returns Property elements for user-defined keys only
        let prop_val = tx.g().V([1]).properties(["name"]).next().unwrap().unwrap();
        if let Value::Property(p) = prop_val {
            assert_eq!(p.key, "name");
            assert_eq!(*p.value, Value::String("marko".to_string()));
        } else {
            panic!("expected Value::Property, got {:?}", prop_val);
        }

        // .hasId(n) filters by vertex id (routes through HasIdStep)
        let ct = tx.g().V([]).hasId([1, 2, 3]).hasId([1i64]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(1));

        // .has("age", n) filters by property value
        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).has("age", 29i32).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(1));

        // "id"/"label" are NOT yielded by .properties() — only user props are
        let ct = tx.g().V([1]).properties(["name", "age"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(2));
    }

    #[test]
    fn test_label_decode_consistency_across_steps() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // .hasLabel(["person"]) routes through HasLabelStep (string-based label resolution).
        let ct = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).hasLabel(["person"]).count().next().unwrap().unwrap();
        assert_eq!(ct, Value::Int64(4));

        // .has("label", "person") (bare string, unfolded) is now rejected — "label" is
        // reserved, must use hasLabel() instead.
        let err = tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).has("label", "person").count().next();
        assert!(err.is_err(), "has(\"label\", ..) should be rejected — use hasLabel() instead");

        // .properties(["label"]) is rejected the same way — use .label() instead.
        assert!(tx.g().V([1]).properties(["label"]).next().is_err());
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

        // Same check via the dedicated id()/label() steps (combined with union(), since
        // id()/label() are reserved and no longer expressible via a single values() call).
        {
            let mut tx = graph.begin();
            let Value::Int64(ct) = tx
                .g()
                .V([1])
                .coalesce([
                    __().V([1]).union([__().id(), __().label()]),
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

        // g.E([]) scan (E with empty keys)
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

        // 3. is() with range predicate is now fully supported on scalar filter
        let res3 = tx.g().V([]).values(["age"]).is(crate::gremlin::value::gt(30i32)).next();
        assert!(res3.is_ok());
        assert_eq!(res3.unwrap(), None);
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

    #[test]
    fn test_e2e_all_supported_data_types() {
        use crate::schema::DataType;

        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();

        // Register keys for all 8 data types
        {
            let mut mgmt = graph.open_management();
            mgmt.make_vertex_label("AllTypesV").make();
            mgmt.make_edge_label("AllTypesE").make();

            mgmt.make_property_key("p_bool", DataType::Bool).make();
            mgmt.make_property_key("p_i32", DataType::Int32).make();
            mgmt.make_property_key("p_i64", DataType::Int64).make();
            mgmt.make_property_key("p_f32", DataType::Float32).make();
            mgmt.make_property_key("p_f64", DataType::Float64).make();
            mgmt.make_property_key("p_string", DataType::String).make();
            mgmt.make_property_key("p_uuid", DataType::Uuid).make();
            mgmt.make_property_key("p_u16", DataType::UInt16).make();
            mgmt.commit().unwrap();
        }

        let mut tx = graph.begin();
        tx.g()
            .addV("AllTypesV")
            .property("id", 1i64)
            .property("p_bool", true)
            .property("p_i32", 42i32)
            .property("p_i64", 999999i64)
            .property("p_f32", 1.25f32)
            .property("p_f64", 123.456f64)
            .property("p_string", "rocks_graph_db")
            .property("p_uuid", 123456789012345678901234567890u128)
            .property("p_u16", 123u16)
            .next()
            .unwrap();

        tx.g().addV("AllTypesV").property("id", 2i64).next().unwrap();

        tx.g()
            .addE("AllTypesE")
            .from(1)
            .to(2)
            .property("p_bool", false)
            .property("p_i32", 100i32)
            .property("p_i64", 888888i64)
            .property("p_f32", 0.5f32)
            .property("p_f64", 0.999f64)
            .property("p_string", "edge_property")
            .property("p_uuid", 98765432109876543210u128)
            .property("p_u16", 456u16)
            .next()
            .unwrap();

        tx.commit().unwrap();

        // Read and verify Vertex properties (withProperties requests all)
        let mut read = graph.read();
        let val_v = read.g().withProperties([]).V([1]).next().unwrap().unwrap();
        if let Value::Vertex(v) = val_v {
            assert_eq!(v.properties.get("p_bool").unwrap()[0], Value::Bool(true));
            assert_eq!(v.properties.get("p_i32").unwrap()[0], Value::Int32(42));
            assert_eq!(v.properties.get("p_i64").unwrap()[0], Value::Int64(999999));
            assert_eq!(v.properties.get("p_f32").unwrap()[0], Value::Float32(1.25));
            assert_eq!(v.properties.get("p_f64").unwrap()[0], Value::Float64(123.456));
            assert_eq!(v.properties.get("p_string").unwrap()[0], Value::String("rocks_graph_db".to_string()));
            assert_eq!(v.properties.get("p_uuid").unwrap()[0], Value::Uuid(123456789012345678901234567890u128));
            assert_eq!(v.properties.get("p_u16").unwrap()[0], Value::UInt16(123));
        } else {
            panic!("Expected Vertex");
        }

        // Read and verify Edge properties (withProperties requests all)
        let val_e = read.g().withProperties([]).V([1]).outE(["AllTypesE"]).next().unwrap().unwrap();
        if let Value::Edge(e) = val_e {
            assert_eq!(*e.properties.get("p_bool").unwrap(), Value::Bool(false));
            assert_eq!(*e.properties.get("p_i32").unwrap(), Value::Int32(100));
            assert_eq!(*e.properties.get("p_i64").unwrap(), Value::Int64(888888));
            assert_eq!(*e.properties.get("p_f32").unwrap(), Value::Float32(0.5));
            assert_eq!(*e.properties.get("p_f64").unwrap(), Value::Float64(0.999));
            assert_eq!(*e.properties.get("p_string").unwrap(), Value::String("edge_property".to_string()));
            assert_eq!(*e.properties.get("p_uuid").unwrap(), Value::Uuid(98765432109876543210u128));
            assert_eq!(*e.properties.get("p_u16").unwrap(), Value::UInt16(456));
        } else {
            panic!("Expected Edge");
        }
    }

    #[test]
    fn test_supported_steps_combinations() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // 1. V + out + values
        let name_list = tx.g().V([1]).out(["knows"]).values(["name"]).to_list().unwrap();
        assert_eq!(name_list.len(), 2);

        // 2. V + r#in + count
        let in_count = tx.g().V([3]).r#in(["created"]).count().next().unwrap().unwrap();
        assert_eq!(in_count, Value::Int64(3));

        // 3. V + both + dedup
        let both_dedup = tx.g().V([4]).both(["knows", "created"]).dedup().count().next().unwrap().unwrap();
        assert_eq!(both_dedup, Value::Int64(3));

        // 4. V + outE + inV
        let in_v_count = tx.g().V([1]).outE(["knows"]).inV().count().next().unwrap().unwrap();
        assert_eq!(in_v_count, Value::Int64(2));

        // 5. V + inE + outV
        let out_v_count = tx.g().V([3]).inE(["created"]).outV().count().next().unwrap().unwrap();
        assert_eq!(out_v_count, Value::Int64(3));

        // 6. V + bothE + otherV + path
        let path_res = tx.g().V([1]).bothE(["knows"]).otherV().path().to_list().unwrap();
        assert_eq!(path_res.len(), 2);

        // 7. E + inV
        let e_in_v = tx.g().E([]).inV().count().next().unwrap().unwrap();
        assert_eq!(e_in_v, Value::Int64(6));

        // 8. E + outV
        let e_out_v = tx.g().E([]).outV().count().next().unwrap().unwrap();
        assert_eq!(e_out_v, Value::Int64(6));

        // 9. V + hasLabel + hasId + limit
        let res_limit = tx.g().V([]).hasLabel(["person"]).hasId([1, 2, 3, 4]).limit(2).to_list().unwrap();
        assert_eq!(res_limit.len(), 2);

        // 10. V + values + is + fold
        let is_fold = tx.g().V([]).values(["age"]).is(29i32).fold().next().unwrap().unwrap();
        if let Value::List(l) = is_fold {
            assert_eq!(l.len(), 1); // marko (29)
        } else {
            panic!("Expected list");
        }

        // 11. V + coalesce + union
        let cu_res = tx
            .g()
            .V([1])
            .coalesce([__().out(["knows"]), __().out(["created"])])
            .union([__().values(["name"]), __().values(["age"])])
            .to_list()
            .unwrap();
        assert_eq!(cu_res.len(), 4);

        // 12. V + out + r#where + path
        let where_path = tx.g().V([1]).out(["knows"]).r#where(__().has("age", 32i32)).path().to_list().unwrap();
        assert_eq!(where_path.len(), 1); // only josh (32)

        // 13. addV + property + drop
        let mut tx_w = graph.begin();
        tx_w.g().addV("person").property("id", 100i64).property("name", "temp_user").next().unwrap();
        let exists = tx_w.g().V([100]).next().unwrap().is_some();
        assert!(exists);
        tx_w.g().V([100]).drop().next().unwrap();
        let deleted = tx_w.g().V([100]).next().unwrap().is_none();
        assert!(deleted);

        // 14. addE + from + to + property (4 steps combined in one write query)
        tx_w.g().addV("person").property("id", 101i64).next().unwrap();
        tx_w.g().addV("person").property("id", 102i64).next().unwrap();
        tx_w.g().addE("knows").from(101).to(102).property("weight", 9.9f64).next().unwrap();
        let new_edge_weight =
            tx_w.g().V([101]).outE(["knows"]).r#where(__().otherV().hasId([102])).values(["weight"]).next().unwrap();
        assert_eq!(new_edge_weight, Some(Value::Float64(9.9)));
        tx_w.commit().unwrap();

        // 15. V + properties + count — the dedicated Property-element step, distinct from values()
        let prop_count = tx.g().V([1]).properties(["name", "age"]).count().next().unwrap().unwrap();
        assert_eq!(prop_count, Value::Int64(2));
    }

    /// `properties([key, ...]).drop()` deletes only the named property keys: other properties on
    /// the same vertex/edge are untouched, and dropping a key that was never set is a graceful
    /// no-op rather than an error (mirroring `drop()` on a `V()`/`E()` traversal that matched
    /// nothing).
    #[test]
    fn test_drop_property_step() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).property("name", "marko").property("age", 29i32).next().unwrap();
        tx.g().addV("person").property("id", 2i64).property("name", "vadas").next().unwrap();
        tx.g().addE("knows").from(1).to(2).property("weight", 0.5f64).property("note", "first meeting").next().unwrap();
        tx.commit().unwrap();

        // Drop a single vertex property; other properties on the same vertex are untouched.
        let mut tx = graph.begin();
        tx.g().V([1]).properties(["age"]).drop().next().unwrap();
        tx.commit().unwrap();
        let mut tx = graph.begin();
        assert_eq!(tx.g().V([1]).values(["age"]).next().unwrap(), None);
        assert_eq!(tx.g().V([1]).values(["name"]).next().unwrap(), Some(Value::String("marko".to_string())));

        // Drop a single edge property reached via a multi-step traversal; other properties on
        // the same edge are untouched.
        tx.g().V([1]).outE(["knows"]).r#where(__().otherV().hasId([2])).properties(["note"]).drop().next().unwrap();
        tx.commit().unwrap();
        let mut tx = graph.begin();
        let note_after = tx.g().V([1]).outE(["knows"]).values(["note"]).next().unwrap();
        let weight_after = tx.g().V([1]).outE(["knows"]).values(["weight"]).next().unwrap();
        assert_eq!(note_after, None);
        assert_eq!(weight_after, Some(Value::Float64(0.5)));

        // Dropping a property key that was never set is a no-op, not an error.
        tx.g().V([1]).properties(["never_set"]).drop().next().unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn test_invalid_and_overflow_values() {
        use crate::schema::DataType;

        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();

        // Setup schema with explicit types
        {
            let mut mgmt = graph.open_management();
            mgmt.make_vertex_label("person").make();
            mgmt.make_property_key("p_i32", DataType::Int32).make();
            mgmt.make_property_key("p_i64", DataType::Int64).make();
            mgmt.make_property_key("p_f32", DataType::Float32).make();
            mgmt.make_property_key("p_bool", DataType::Bool).make();
            mgmt.make_property_key("p_uuid", DataType::Uuid).make();
            mgmt.make_property_key("p_string", DataType::String).make();
            mgmt.commit().unwrap();
        }

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();

        // 1. Assigning i64 (which is distinct from Int32 key) -> SchemaViolation
        let res_1 = tx.g().V([1]).property("p_i32", 1234567890123i64).next();
        assert!(matches!(res_1, Err(StoreError::SchemaViolation(_))));

        // 1b. Assigning i32 to an explicitly Int64-declared key -> SchemaViolation. Int64 was
        // the one DataType variant never exercised as the *protected* declared type anywhere
        // in this file or schema/tests.rs (only ever appearing as the *violating* value).
        let res_1b = tx.g().V([1]).property("p_i64", 42i32).next();
        assert!(matches!(res_1b, Err(StoreError::SchemaViolation(_))));

        // 2. Assigning f64 to Float32 key -> SchemaViolation
        let res_2 = tx.g().V([1]).property("p_f32", 12345.6789f64).next();
        assert!(matches!(res_2, Err(StoreError::SchemaViolation(_))));

        // 3. Assigning String to Bool key -> SchemaViolation
        let res_3 = tx.g().V([1]).property("p_bool", "true").next();
        assert!(matches!(res_3, Err(StoreError::SchemaViolation(_))));

        // 4. Assigning String to Uuid key -> SchemaViolation
        let res_4 = tx.g().V([1]).property("p_uuid", "uuid-string").next();
        assert!(matches!(res_4, Err(StoreError::SchemaViolation(_))));

        // 4b. Assigning Int32 to an explicitly String-declared key -> SchemaViolation
        let res_4b = tx.g().V([1]).property("p_string", 5i32).next();
        assert!(matches!(res_4b, Err(StoreError::SchemaViolation(_))));

        // 5. Invalid rank values on addE
        tx.g().addV("person").property("id", 2i64).next().unwrap();
        // Negative rank value (represented as negative integer) -> UnexpectedDataType
        let res_rank_neg = tx.g().addE("knows").from(1).to(2).property("rank", -1i32).next();
        assert!(
            matches!(res_rank_neg, Err(StoreError::UnexpectedDataType(msg)) if msg.contains("rank must be between 0 and 65535"))
        );

        // Large rank value (exceeds u16::MAX) -> UnexpectedDataType
        let res_rank_large = tx.g().addE("knows").from(1).to(2).property("rank", 70000i64).next();
        assert!(
            matches!(res_rank_large, Err(StoreError::UnexpectedDataType(msg)) if msg.contains("rank must be between 0 and 65535"))
        );
    }

    #[test]
    fn test_filters_across_all_data_types() {
        use crate::{gremlin::value::eq, schema::DataType};

        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();

        // 1. Declare properties of all types
        {
            let mut mgmt = graph.open_management();
            mgmt.make_vertex_label("Item").make();
            mgmt.make_property_key("p_bool", DataType::Bool).make();
            mgmt.make_property_key("p_i32", DataType::Int32).make();
            mgmt.make_property_key("p_i64", DataType::Int64).make();
            mgmt.make_property_key("p_f32", DataType::Float32).make();
            mgmt.make_property_key("p_f64", DataType::Float64).make();
            mgmt.make_property_key("p_string", DataType::String).make();
            mgmt.make_property_key("p_uuid", DataType::Uuid).make();
            mgmt.make_property_key("p_u16", DataType::UInt16).make();
            mgmt.commit().unwrap();
        }

        let mut tx = graph.begin();
        tx.g()
            .addV("Item")
            .property("id", 1i64)
            .property("p_bool", true)
            .property("p_i32", 10i32)
            .property("p_i64", 1000i64)
            .property("p_f32", 1.5f32)
            .property("p_f64", 10.5f64)
            .property("p_string", "target_string")
            .property("p_uuid", 111111u128)
            .property("p_u16", 20u16)
            .next()
            .unwrap();

        tx.g()
            .addV("Item")
            .property("id", 2i64)
            .property("p_bool", false)
            .property("p_i32", 20i32)
            .property("p_i64", 2000i64)
            .property("p_f32", 2.5f32)
            .property("p_f64", 20.5f64)
            .property("p_string", "other_string")
            .property("p_uuid", 222222u128)
            .property("p_u16", 40u16)
            .next()
            .unwrap();

        tx.commit().unwrap();

        let mut read = graph.read();

        // Bool filters
        let b1 = read.g().V([]).has("p_bool", true).count().next().unwrap().unwrap();
        assert_eq!(b1, Value::Int64(1));
        let b2 = read.g().V([]).has("p_bool", eq(false)).count().next().unwrap().unwrap();
        assert_eq!(b2, Value::Int64(1));

        // Int32 filters
        let i32_1 = read.g().V([]).has("p_i32", 10i32).count().next().unwrap().unwrap();
        assert_eq!(i32_1, Value::Int64(1));
        let i32_2 = read.g().V([]).has("p_i32", eq(20i32)).count().next().unwrap().unwrap();
        assert_eq!(i32_2, Value::Int64(1));

        // Int64 filters
        let i64_1 = read.g().V([]).has("p_i64", 1000i64).count().next().unwrap().unwrap();
        assert_eq!(i64_1, Value::Int64(1));
        let i64_2 = read.g().V([]).has("p_i64", eq(2000i64)).count().next().unwrap().unwrap();
        assert_eq!(i64_2, Value::Int64(1));

        // Float32 filters
        let f32_1 = read.g().V([]).has("p_f32", 1.5f32).count().next().unwrap().unwrap();
        assert_eq!(f32_1, Value::Int64(1));

        // Float64 filters
        let f64_1 = read.g().V([]).has("p_f64", 10.5f64).count().next().unwrap().unwrap();
        assert_eq!(f64_1, Value::Int64(1));

        // String filters
        let s1 = read.g().V([]).has("p_string", "target_string").count().next().unwrap().unwrap();
        assert_eq!(s1, Value::Int64(1));
        let s2 = read.g().V([]).has("p_string", eq("other_string".to_string())).count().next().unwrap().unwrap();
        assert_eq!(s2, Value::Int64(1));

        // Uuid filters
        let u1 = read.g().V([]).has("p_uuid", 111111u128).count().next().unwrap().unwrap();
        assert_eq!(u1, Value::Int64(1));
        let u2 = read.g().V([]).has("p_uuid", eq(222222u128)).count().next().unwrap().unwrap();
        assert_eq!(u2, Value::Int64(1));

        // UInt16 filters
        let u16_1 = read.g().V([]).has("p_u16", 20u16).count().next().unwrap().unwrap();
        assert_eq!(u16_1, Value::Int64(1));

        // Within — hasId()/hasLabel() build Within automatically for a multi-element list.
        let id_within = read.g().V([]).hasId([1i64, 2i64]).count().next().unwrap().unwrap();
        assert_eq!(id_within, Value::Int64(2));

        let label_within = read.g().V([]).hasLabel(["Item"]).count().next().unwrap().unwrap();
        assert_eq!(label_within, Value::Int64(2));

        // Without — id()/label() no longer accept a `Predicate` directly (reserved keys are
        // dedicated-step-only), so negation goes through the existing not() combinator
        // instead: not(hasId([1])) == "every vertex except id 1", same result as the old
        // has(Key::Id, without([1])).
        let id_without = read.g().V([]).not(__().hasId([1i64])).count().next().unwrap().unwrap();
        assert_eq!(id_without, Value::Int64(1));

        let label_without = read.g().V([]).not(__().hasLabel(["Item"])).count().next().unwrap().unwrap();
        assert_eq!(label_without, Value::Int64(0));

        let label_without_other = read.g().V([]).not(__().hasLabel(["OtherLabel"])).count().next().unwrap().unwrap();
        assert_eq!(label_without_other, Value::Int64(2));
    }

    #[test]
    fn test_edge_modes_and_rank_validation() {
        // --- Single-edge Mode ---
        {
            let dir = tempfile::tempdir().unwrap();
            let graph = Graph::open(dir.path()).unwrap();
            {
                let schema_arc = graph.schema();
                let mut schema = schema_arc.write().unwrap();
                schema.register_vertex_label("person").unwrap();
                schema.register_edge_label("knows").unwrap();
            }

            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 1i64).next().unwrap();
            tx.g().addV("person").property("id", 2i64).next().unwrap();

            // 1. Add edge
            tx.g().addE("knows").from(1).to(2).next().unwrap();

            // 2. Duplicate edge should fail
            let res_dup = tx.g().addE("knows").from(1).to(2).next();
            assert!(matches!(res_dup, Err(StoreError::DuplicateEdge(_))));

            // 3. Setting non-zero rank on single-edge mode should fail
            let res_rank = tx.g().addE("knows").from(1).to(2).property("rank", 5u16).next();
            assert!(matches!(res_rank, Err(StoreError::UnsupportedOperation(_))));

            // 4. A different edge LABEL between the same (src, dst) pair is NOT a duplicate —
            // single-edge mode restricts at most one edge per (src, label, dst), not per
            // (src, dst) overall.
            tx.g().addE("likes").from(1).to(2).next().unwrap();
            let both_edges = tx.g().V([1]).outE(["knows", "likes"]).count().next().unwrap().unwrap();
            assert_eq!(both_edges, Value::Int64(2));
        }

        // --- Multi-edge Mode ---
        {
            let dir = tempfile::tempdir().unwrap();
            let options = crate::schema::GraphOptions {
                mode: crate::schema::SchemaMode::Auto,
                edge_mode: crate::schema::EdgeMode::Multi,
            };
            let graph = Graph::open_with_options(dir.path(), options).unwrap();

            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 1i64).next().unwrap();
            tx.g().addV("person").property("id", 2i64).next().unwrap();

            // 1. Add edge rank 0
            tx.g().addE("knows").from(1).to(2).property("rank", 0i32).next().unwrap();

            // 2. Duplicate rank 0 edge should fail
            let res_dup = tx.g().addE("knows").from(1).to(2).property("rank", 0i32).next();
            assert!(matches!(res_dup, Err(StoreError::DuplicateEdge(_))));

            // 3. Add edge rank 5 (which should succeed)
            tx.g().addE("knows").from(1).to(2).property("rank", 5i32).next().unwrap();

            tx.commit().unwrap();

            // 4. Query both ranks
            let mut read = graph.read();
            let ranks = read.g().V([1]).outE(["knows"]).rank().to_list().unwrap();
            assert_eq!(ranks.len(), 2);
            assert!(ranks.contains(&Value::UInt16(0)));
            assert!(ranks.contains(&Value::UInt16(5)));

            // 5. `.has("rank", N)` is rejected once it can't be optimizer-folded. Every rank
            // filter above (and every one in multi_edge_tests.rs) immediately follows
            // `.outE(...)`, which `merge_end_vertex_filter` folds into a dedicated physical
            // step before `reject_reserved_key` ever runs. Wrapping the same `outE` in a
            // `union()` hides it from that optimizer rule (sub-plans inside union()/
            // coalesce() are opaque to it), forcing the filter through unfolded — which must
            // now be rejected, "rank" being reserved.
            let unmerged = read.g().V([1]).union([__().outE(["knows"])]).has("rank", 5i32).count().next();
            assert!(unmerged.is_err(), "unfolded has(\"rank\", ..) should be rejected — use hasRank() instead");

            // `.hasRank()` is the dedicated replacement, and works correctly in the same
            // unmerged-via-union() position — including width-insensitive comparison
            // against the runtime `UInt16` rank value (`PrimitivePredicate::evaluate`'s
            // `loose_eq`, the same mechanism `HasPropertyStep` relied on).
            let unmerged_match =
                read.g().V([1]).union([__().outE(["knows"])]).hasRank(5i32).count().next().unwrap().unwrap();
            assert_eq!(unmerged_match, Value::Int64(1));

            let unmerged_no_match =
                read.g().V([1]).union([__().outE(["knows"])]).hasRank(99i32).count().next().unwrap().unwrap();
            assert_eq!(unmerged_no_match, Value::Int64(0));
        }
    }

    #[test]
    fn test_auto_schema_conflict_detection() {
        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();

        // 1. String vs Int32
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 1i64).property("p_conflict_1", "string_val").next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addV("person").property("id", 2i64).property("p_conflict_1", 123i32).next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type String, but requested Int32"))
            );
        }

        // 2. Bool vs Float64
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 3i64).property("p_conflict_2", true).next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addV("person").property("id", 4i64).property("p_conflict_2", 12.34f64).next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type Bool, but requested Float64"))
            );
        }

        // 3. Uuid vs String
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 5i64).property("p_conflict_3", 1234567890u128).next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addV("person").property("id", 6i64).property("p_conflict_3", "illegal").next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type Uuid, but requested String"))
            );
        }

        // 4. UInt16 vs Int32
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 7i64).property("p_conflict_4", 5u16).next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addV("person").property("id", 8i64).property("p_conflict_4", 10i32).next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type UInt16, but requested Int32"))
            );
        }

        // 5. Float32 vs Float64
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 9i64).property("p_conflict_5", 1.0f32).next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addV("person").property("id", 10i64).property("p_conflict_5", 2.0f64).next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type Float32, but requested Float64"))
            );
        }

        // 6. Int64 vs Bool — the one DataType variant never exercised as the auto-inferred
        // protected type anywhere above (only ever appearing as the conflicting/violating value).
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 11i64).property("p_conflict_6", 1_000_000i64).next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addV("person").property("id", 12i64).property("p_conflict_6", false).next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type Int64, but requested Bool"))
            );
        }

        // 7. Cross-element-kind conflict: property keys are a single global namespace shared
        // by vertices and edges (not partitioned by element kind), so a key first inferred as
        // Int32 on a VERTEX must also reject a conflicting type written on an EDGE.
        {
            let mut tx = graph.begin();
            tx.g().addV("person").property("id", 13i64).property("p_conflict_cross", 1i32).next().unwrap();
            tx.g().addV("person").property("id", 14i64).next().unwrap();
            tx.commit().unwrap();

            let mut tx2 = graph.begin();
            let res = tx2.g().addE("knows_cross").from(13).to(14).property("p_conflict_cross", "edge_value").next();
            assert!(
                matches!(res, Err(StoreError::SchemaViolation(msg)) if msg.contains("already defined with type Int32, but requested String"))
            );
        }

        // 8. Control case: vertex labels and edge labels are *independent* namespaces (see
        // `Schema::vertex_labels`/`edge_labels`), so reusing the same name for both must NOT
        // be reported as a conflict — confirms the conflict detection above doesn't false-positive.
        {
            let mut tx = graph.begin();
            tx.g().addV("dup_name").property("id", 15i64).next().unwrap();
            tx.g().addV("dup_name").property("id", 16i64).next().unwrap();
            tx.g().addE("dup_name").from(15).to(16).next().unwrap();
            tx.commit().unwrap();

            let mut read = graph.read();
            let v_count = read.g().V([]).hasLabel(["dup_name"]).count().next().unwrap().unwrap();
            assert_eq!(v_count, Value::Int64(2));
            let e_count = read.g().V([15]).outE(["dup_name"]).count().next().unwrap().unwrap();
            assert_eq!(e_count, Value::Int64(1));
        }
    }

    #[test]
    fn test_new_predicate_evaluation() {
        use crate::gremlin::value::{between, eq, gt, gte, lt, lte, ne, within, without};

        let dir = tempfile::tempdir().unwrap();
        let graph = Graph::open(dir.path()).unwrap();

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).property("age", 20i32).property("name", "Alice").next().unwrap();
        tx.g().addV("person").property("id", 2i64).property("age", 30i32).property("name", "Bob").next().unwrap();
        tx.g().addV("person").property("id", 3i64).property("age", 40i32).property("name", "Charlie").next().unwrap();
        tx.g().addV("animal").property("id", 4i64).property("age", 5i32).property("name", "Dog").next().unwrap();
        tx.g().addV("software").property("id", 5i64).property("age", 3i32).property("name", "App").next().unwrap();
        tx.commit().unwrap();

        let mut read = graph.read();

        // 1. Property checks with range predicates
        assert_eq!(read.g().V([]).has("age", gt(25i32)).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(read.g().V([]).has("age", gte(30i32)).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(read.g().V([]).has("age", lt(10i32)).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(read.g().V([]).has("age", lte(20i32)).count().next().unwrap().unwrap(), Value::Int64(3));
        assert_eq!(read.g().V([]).has("age", between(20i32, 40i32)).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(read.g().V([]).has("age", ne(30i32)).count().next().unwrap().unwrap(), Value::Int64(4));
        assert_eq!(read.g().V([]).has("age", within([20i32, 40i32])).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(
            read.g().V([]).has("age", without([20i32, 40i32])).count().next().unwrap().unwrap(),
            Value::Int64(3)
        );

        // 2. Label checks with range rejections & equality/membership
        // Range predicate must be rejected
        let res_label_gt = read.g().V([]).hasLabel(gt("person")).next();
        assert!(matches!(res_label_gt, Err(StoreError::UnsupportedOperation(_))));

        let res_label_between = read.g().V([]).hasLabel(between("animal", "software")).next();
        assert!(matches!(res_label_between, Err(StoreError::UnsupportedOperation(_))));

        // Equality/membership succeeds
        assert_eq!(read.g().V([]).hasLabel(eq("person")).count().next().unwrap().unwrap(), Value::Int64(3));
        assert_eq!(read.g().V([]).hasLabel(ne("person")).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(
            read.g().V([]).hasLabel(within(["person", "software"])).count().next().unwrap().unwrap(),
            Value::Int64(4)
        );
        assert_eq!(
            read.g().V([]).hasLabel(without(["person", "software"])).count().next().unwrap().unwrap(),
            Value::Int64(1)
        );

        // 3. ID checks with various predicates
        assert_eq!(read.g().V([]).hasId(gt(2i64)).count().next().unwrap().unwrap(), Value::Int64(3));
        assert_eq!(read.g().V([]).hasId(between(2i64, 5i64)).count().next().unwrap().unwrap(), Value::Int64(3));
        assert_eq!(read.g().V([]).hasId(ne(3i64)).count().next().unwrap().unwrap(), Value::Int64(4));
        assert_eq!(read.g().V([]).hasId(without([1i64, 5i64])).count().next().unwrap().unwrap(), Value::Int64(3));

        // 4. is() step evaluation
        let ages_gt = read.g().V([]).values(["age"]).is(gt(25i32)).count().next().unwrap().unwrap();
        assert_eq!(ages_gt, Value::Int64(2));

        let ages_between = read.g().V([]).values(["age"]).is(between(10i32, 35i32)).count().next().unwrap().unwrap();
        assert_eq!(ages_between, Value::Int64(2));

        let ages_within = read.g().V([]).values(["age"]).is(within([5i32, 20i32])).count().next().unwrap().unwrap();
        assert_eq!(ages_within, Value::Int64(2));
    }

    // ── repeat / until / emit / emit_if tests ────────────────────────────────

    #[test]
    fn test_repeat_without_bound_is_error() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // repeat() without .times() or .until() must error at build time
        let res = tx.g().V([1]).repeat(__().out(["knows", "created"])).next();
        assert!(
            matches!(res, Err(StoreError::TraversalError(msg)) if msg.contains("repeat() requires at least one stop condition"))
        );
    }

    #[test]
    fn test_repeat_times_zero_is_error() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let res = tx.g().V([1]).repeat(__().out(["knows", "created"])).times(0).next();
        assert!(matches!(res, Err(StoreError::TraversalError(msg)) if msg.contains("times(0)")));
    }

    #[test]
    fn test_until_without_repeat_is_error() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let res = tx.g().V([1]).until(__().hasLabel(["software"])).next();
        assert!(
            matches!(res, Err(StoreError::TraversalError(msg)) if msg.contains("until() must immediately follow repeat()"))
        );
    }

    #[test]
    fn test_emit_without_repeat_is_error() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let res = tx.g().V([1]).emit().next();
        assert!(
            matches!(res, Err(StoreError::TraversalError(msg)) if msg.contains("emit() must immediately follow repeat()"))
        );
    }

    #[test]
    fn test_emit_if_without_repeat_is_error() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        let res = tx.g().V([1]).emit_if(__().hasLabel(["person"])).next();
        assert!(
            matches!(res, Err(StoreError::TraversalError(msg)) if msg.contains("emit_if() must immediately follow repeat()"))
        );
    }

    #[test]
    fn test_back_to_back_repeat() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // Two back-to-back repeat() calls: the first one is flushed when the second starts.
        // V(1).repeat(out(["knows","created"])).times(1).repeat(out(["knows","created"])).times(1)
        let res = tx
            .g()
            .V([1])
            .repeat(__().out(["knows", "created"]))
            .times(1)
            .repeat(__().out(["knows", "created"]))
            .times(1)
            .dedup()
            .count()
            .next()
            .unwrap()
            .unwrap();
        // 1st repeat → [vadas(2), lop(3), josh(4)]. 2nd repeat → [ripple(5), lop(3)]. dedup = 2
        assert_eq!(res, Value::Int64(2));
    }

    #[test]
    fn test_e2e_n_hop_query() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).repeat(out()).times(2).values("name") — find 2-hop neighbor names
        let names = tx.g().V([1]).repeat(__().out(["knows", "created"])).times(2).values(["name"]).to_list().unwrap();
        assert_eq!(names.len(), 2);
        let mut name_strs: Vec<String> = names
            .iter()
            .map(|v| if let Value::String(s) = v { s.clone() } else { panic!("expected string") })
            .collect();
        name_strs.sort();
        // 2-hop from marko: ripple (via josh→created), lop (via josh→created)
        assert_eq!(name_strs, vec!["lop", "ripple"]);
    }

    #[test]
    fn test_e2e_repeat_until_emit() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).repeat(out("knows")).until(hasLabel("software")).emit().values("name")
        // Emit all intermediate people and stop at software.
        // marko→vadas(person, emitted), josh(person, emitted).
        // josh→ripple(software, until match), lop(software, until match).
        // vadas→[] (no outgoing knows).
        // Also lop(3) — wait, marko's out("knows") is [vadas, josh], NOT lop.
        // So: vadas(person, emit), josh(person, emit), josh→ripple(software, until→emit), josh→... hmm
        // Actually josh.out("knows") = [] (josh has no outgoing "knows" edges).
        // So vadas(person, emit), josh(person, emit). Then vadas.out("knows")=[], josh.out("knows")=[].
        // Total: vadas(2), josh(4) = 2.
        let results = tx
            .g()
            .V([1])
            .repeat(__().out(["knows"]))
            .until(__().hasLabel(["software"]))
            .emit()
            .values(["name"])
            .to_list()
            .unwrap();
        let mut name_strs: Vec<String> = results
            .iter()
            .map(|v| if let Value::String(s) = v { s.clone() } else { panic!("expected string") })
            .collect();
        name_strs.sort();
        assert_eq!(name_strs, vec!["josh", "vadas"]);
    }

    #[test]
    fn test_with_properties_default_no_properties() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // Default: no withProperties() → id and label only, no properties.
        let val = read.g().V([1]).next().unwrap().unwrap();
        if let Value::Vertex(v) = val {
            assert_eq!(v.id, 1);
            assert_eq!(v.label, SmolStr::from("person"));
            assert!(v.properties.is_empty(), "default should return empty properties");
        } else {
            panic!("Expected Vertex");
        }
    }

    #[test]
    fn test_with_properties_empty_returns_all() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // Empty keys → all properties (matching `[] = all` convention).
        let val = read.g().withProperties([]).V([1]).next().unwrap().unwrap();
        if let Value::Vertex(v) = val {
            assert_eq!(v.id, 1);
            assert_eq!(v.label, SmolStr::from("person"));
            assert_eq!(v.properties.get("name").unwrap()[0], Value::String("marko".to_string()));
            assert_eq!(v.properties.get("age").unwrap()[0], Value::Int32(29));
        } else {
            panic!("Expected Vertex");
        }
    }

    #[test]
    fn test_with_properties_named_keys() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // Named keys → only requested properties.
        let val = read.g().withProperties(["age"]).V([1]).next().unwrap().unwrap();
        if let Value::Vertex(v) = val {
            assert_eq!(v.id, 1);
            assert_eq!(v.label, SmolStr::from("person"));
            assert!(v.properties.contains_key("age"), "age should be present");
            assert!(!v.properties.contains_key("name"), "name should NOT be present");
            assert_eq!(v.properties.get("age").unwrap()[0], Value::Int32(29));
        } else {
            panic!("Expected Vertex");
        }
    }

    #[test]
    fn test_with_properties_edge_default_no_properties() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // Default on edge: id/label only, no properties, zero store reads.
        let val = read.g().V([1]).outE(["knows"]).next().unwrap().unwrap();
        if let Value::Edge(e) = val {
            assert_eq!(e.out_v, 1);
            assert_eq!(e.label, SmolStr::from("knows"));
            assert!(e.properties.is_empty(), "default should return empty edge properties");
        } else {
            panic!("Expected Edge");
        }
    }

    #[test]
    fn test_with_properties_edge_all() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // Empty keys on edge → all properties.
        let val = read.g().withProperties([]).V([1]).outE(["knows"]).next().unwrap().unwrap();
        if let Value::Edge(e) = val {
            assert_eq!(e.out_v, 1);
            assert_eq!(e.label, SmolStr::from("knows"));
            assert_eq!(*e.properties.get("weight").unwrap(), Value::Float64(0.5));
        } else {
            panic!("Expected Edge");
        }
    }

    #[test]
    fn test_with_properties_edge_named_keys() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // Named keys on edge → only requested properties.
        let val = read.g().withProperties(["weight"]).V([1]).outE(["knows"]).next().unwrap().unwrap();
        if let Value::Edge(e) = val {
            assert_eq!(e.out_v, 1);
            assert_eq!(e.label, SmolStr::from("knows"));
            assert!(e.properties.contains_key("weight"), "weight should be present");
            assert_eq!(e.properties.len(), 1, "only weight should be returned");
        } else {
            panic!("Expected Edge");
        }
    }

    #[test]
    fn test_with_properties_unaffected_by_count() {
        let graph = setup_modern_graph();
        let mut read = graph.read();

        // count() returns a Scalar, unaffected by withProperties.
        let count = read.g().withProperties([]).V([]).count().next().unwrap().unwrap();
        assert_eq!(count, Value::Int64(6));
    }

    // ── not / and / or / sum / mean / max / min / unfold ────────────────────

    #[test]
    fn test_not_filter() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V([]).not(__().hasLabel("person")).values("name") → software vertices only
        let names =
            tx.g().V([]).hasId([1, 2, 3, 4, 5, 6]).not(__().hasLabel(["person"])).values(["name"]).to_list().unwrap();
        let mut s: Vec<String> =
            names.iter().map(|v| if let Value::String(s) = v { s.clone() } else { panic!() }).collect();
        s.sort();
        assert_eq!(s, vec!["lop", "ripple"]);
    }

    #[test]
    fn test_and_filter() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V([]).and([__.hasLabel("person"), __.has("age", gt(30))]).values("name") → josh, peter
        let names = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .and([__().hasLabel(["person"]), __().has("age", crate::gremlin::value::gt(30i32))])
            .values(["name"])
            .to_list()
            .unwrap();
        let mut s: Vec<String> =
            names.iter().map(|v| if let Value::String(s) = v { s.clone() } else { panic!() }).collect();
        s.sort();
        assert_eq!(s, vec!["josh", "peter"]);
    }

    #[test]
    fn test_or_filter() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V([]).or([__.has("name", "marko"), __.has("name", "lop")]).values("name")
        let names = tx
            .g()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .or([__().has("name", "marko"), __().has("name", "lop")])
            .values(["name"])
            .to_list()
            .unwrap();
        let mut s: Vec<String> =
            names.iter().map(|v| if let Value::String(s) = v { s.clone() } else { panic!() }).collect();
        s.sort();
        assert_eq!(s, vec!["lop", "marko"]);
    }

    #[test]
    fn test_sum_mean_max_min() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // Sum of ages: marko(29) + vadas(27) + josh(32) + peter(35) = 123
        let sum_val = tx.g().V([]).hasLabel(["person"]).values(["age"]).sum().next().unwrap().unwrap();
        assert_eq!(sum_val, Value::Int64(123));

        // Mean: 123 / 4 = 30.75
        let mean_val = tx.g().V([]).hasLabel(["person"]).values(["age"]).mean().next().unwrap().unwrap();
        assert_eq!(mean_val, Value::Float64(30.75));

        // Max: 35
        let max_val = tx.g().V([]).hasLabel(["person"]).values(["age"]).max().next().unwrap().unwrap();
        assert_eq!(max_val, Value::Int64(35));

        // Min: 27
        let min_val = tx.g().V([]).hasLabel(["person"]).values(["age"]).min().next().unwrap().unwrap();
        assert_eq!(min_val, Value::Int64(27));
    }

    #[test]
    fn test_unfold() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // fold then unfold: round-trip
        let names = tx.g().V([1]).values(["name", "age"]).fold().unfold().to_list().unwrap();
        assert_eq!(names.len(), 2);
        let s: Vec<String> = names.iter().map(|v| format!("{:?}", v)).collect();
        // Check both expected values are present (order is preserved from the list)
        let joined = s.join(",");
        assert!(joined.contains("marko"), "expected 'marko' in: {}", joined);
        assert!(joined.contains("29"), "expected '29' in: {}", joined);
    }

    // ── as / select ───────────────────────────────────────────────────────

    #[test]
    fn test_as_select_round_trip() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).as_("start").out("knows").as_("friend").select("start").values("name") → marko (the start vertex)
        let names =
            tx.g().V([1]).as_("start").out(["knows"]).as_("friend").select("start").values(["name"]).to_list().unwrap();

        // select("start") returns the traverser labeled "start" = vertex 1 (marko), for each outgoing edge
        assert!(!names.is_empty());
        for n in &names {
            assert_eq!(n, &Value::String("marko".to_string()));
        }
    }

    #[test]
    fn test_as_select_with_path() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).as_("a").out("knows").as_("b").select("b").values("name").path() → paths ending at the friend
        let results =
            tx.g().V([1]).as_("a").out(["knows"]).as_("b").select("b").values(["name"]).path().to_list().unwrap();

        // select("b") picks up the friend, then values("name") extracts their name
        assert!(!results.is_empty());
        for res in &results {
            if let Value::Path(p) = res {
                // Path should include: V(1), outE, friend vertex, name scalar
                assert!(p.len() >= 2, "path should have at least 2 elements");
            }
        }
    }

    #[test]
    fn test_select_without_matching_label_filters_out() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).out("knows").select("nonexistent") → nothing, since no label matches
        let results = tx.g().V([1]).out(["knows"]).select("nonexistent").to_list().unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_where_filter_does_not_disrupt_path_tracking_for_later_select() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).as_("start").where(has("age", 29)).out("knows").select("start").values("name")
        //
        // The where() sub-plan has no as()/select()/path() of its own, so its physical sub-plan
        // is built with track_path=false in isolation — but out() runs *after* the where() filter
        // in the *outer* pipeline, which does need path tracking (for select("start")). This
        // guards against track_path being computed independently per sub-plan instead of
        // inherited from the top-level plan: if out() incorrectly read the where() sub-plan's
        // track_path instead of the outer plan's, it would build a parentless traverser and
        // select("start") would find nothing.
        let names = tx
            .g()
            .V([1])
            .as_("start")
            .r#where(__().has("age", 29i32))
            .out(["knows"])
            .select("start")
            .values(["name"])
            .to_list()
            .unwrap();

        assert!(!names.is_empty());
        for n in &names {
            assert_eq!(n, &Value::String("marko".to_string()));
        }
    }

    #[test]
    fn test_repeat_body_inherits_path_tracking_from_outer_select() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // V(1).as_("start").repeat(out("knows")).times(1).select("start").values("name")
        //
        // The repeat() body (out("knows")) has no as()/select()/path() of its own — computed in
        // isolation its sub-plan would not need path tracking. But the body's output *is* the
        // traverser that flows back into the outer pipeline on each iteration, and the outer
        // pipeline needs path tracking (for select("start") after the loop). track_path must be
        // computed once on the whole top-level plan and inherited into the repeat body, not
        // recomputed independently from the body's own (narrower) shape — otherwise the body
        // would build parentless traversers and select("start") would find nothing.
        let names = tx
            .g()
            .V([1])
            .as_("start")
            .repeat(__().out(["knows"]))
            .times(1)
            .select("start")
            .values(["name"])
            .to_list()
            .unwrap();

        assert!(!names.is_empty());
        for n in &names {
            assert_eq!(n, &Value::String("marko".to_string()));
        }
    }

    // ── range / skip / tail / order / simplePath / choose ──

    #[test]
    fn test_range_skip_tail_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let ages: Vec<i64> = tx
            .g()
            .V([])
            .hasLabel(["person"])
            .values(["age"])
            .order()
            .range(1, 3)
            .to_list()
            .unwrap()
            .iter()
            .map(|v| match v {
                Value::Int32(i) => *i as i64,
                Value::Int64(i) => *i,
                _ => panic!(),
            })
            .collect();
        assert_eq!(ages.len(), 2);
        let last = tx.g().V([]).hasLabel(["person"]).values(["name"]).order().tail(1).to_list().unwrap();
        assert_eq!(last.len(), 1);
    }

    #[test]
    fn test_order_asc_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let ages: Vec<i64> = tx
            .g()
            .V([])
            .hasLabel(["person"])
            .values(["age"])
            .order()
            .to_list()
            .unwrap()
            .iter()
            .map(|v| match v {
                Value::Int32(i) => *i as i64,
                Value::Int64(i) => *i,
                _ => panic!(),
            })
            .collect();
        let mut sorted = ages.clone();
        sorted.sort();
        assert_eq!(ages, sorted);
    }

    #[test]
    fn test_group_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let result = tx.g().V([]).hasLabel(["person"]).values(["age"]).group().next().unwrap().unwrap();
        // Result is a Map<age, List<age>> — e.g. {29: [29, 29], 27: [27], 32: [32], 35: [35]}
        // Marko (29), Vadas (27), Josh (32), Peter (35)
        if let Value::Map(m) = result {
            assert_eq!(m.len(), 4);
            // Each value should be a List with at least one element
            for (_, v) in &m.entries {
                assert!(matches!(v, Value::List(_)));
            }
        } else {
            panic!("expected Map, got {:?}", result);
        }
    }

    #[test]
    fn test_group_count_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let result = tx.g().V([]).hasLabel(["person"]).values(["age"]).group_count().next().unwrap().unwrap();
        // Result is a Map<age, count> — one entry per distinct age.
        // Marko=29, Vadas=27, Josh=32, Peter=35 — each age appears once.
        if let Value::Map(m) = result {
            assert_eq!(m.len(), 4);
            // Every value should be Int64(1) — one occurrence per age.
            for (_, v) in &m.entries {
                assert!(matches!(v, Value::Int64(1)), "expected count 1, got {:?}", v);
            }
        } else {
            panic!("expected Map, got {:?}", result);
        }
    }

    #[test]
    fn test_simple_path_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        // V(1).out("knows").both("knows") produces 2 paths, both cycles back to V(1):
        //   1→2→1 (Vadas back to Marko via incoming knows edge)
        //   1→4→1 (Josh back to Marko via incoming knows edge)
        // simplePath() filters them all out — 0 results.
        let results = tx.g().V([1]).out(["knows"]).both(["knows"]).simple_path().to_list().unwrap();
        assert_eq!(results.len(), 0, "simplePath should filter out the back-edges to V(1)");
    }

    #[test]
    fn test_cyclic_path_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        // Same traversal — cyclicPath() keeps only the 2 cycles.
        let results = tx.g().V([1]).out(["knows"]).both(["knows"]).cyclic_path().to_list().unwrap();
        assert_eq!(results.len(), 2, "cyclicPath should keep only the cyclic back-edges");
    }

    #[test]
    fn test_add_e_variable_source_constant_target() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        // g.V([1]).out("knows").addE("friends").to(3)
        // Marko knows Vadas(2), Josh(4).  For each, create friends->Lop(3).
        let edges: Vec<_> = tx.g().V([1]).out(["knows"]).addE("friends").to(3).to_list().unwrap();
        assert_eq!(edges.len(), 2, "should create edges from each traverser");

        // Both new edges must be visible from BOTH sides (bidirectional indexing,
        // not just the out-side the producing step happened to emit).
        let in_count = tx.g().V([3]).inE(["friends"]).count().next().unwrap().unwrap();
        assert_eq!(in_count, Value::Int64(2), "in-side index should see both new edges");
        let out_count_2 = tx.g().V([2]).outE(["friends"]).count().next().unwrap().unwrap();
        assert_eq!(out_count_2, Value::Int64(1));
        let out_count_4 = tx.g().V([4]).outE(["friends"]).count().next().unwrap().unwrap();
        assert_eq!(out_count_4, Value::Int64(1));
    }

    #[test]
    fn test_add_e_variable_source_with_property() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        // Same traversal with a property.
        let edges: Vec<_> =
            tx.g().V([1]).out(["knows"]).addE("friends").to(3).property("weight", 0.5f64).to_list().unwrap();
        assert_eq!(edges.len(), 2);

        // Verify the property landed on each *real* created edge (vadas->3, josh->3),
        // not just that two edges exist — `vadas->3` and `josh->3` have different
        // out-vertices resolved per-traverser from the upstream `out("knows")`, so this
        // also confirms the property isn't being tagged with a stale/static owner key.
        for src in [2i64, 4i64] {
            let weight =
                tx.g().V([src]).outE(["friends"]).r#where(__().otherV().hasId([3])).values(["weight"]).next().unwrap();
            assert_eq!(weight, Some(Value::Float64(0.5)), "property missing/wrong on edge {src}->3");
        }
    }

    #[test]
    fn test_add_e_no_endpoints_error() {
        let graph = setup_modern_graph();
        let res = graph.begin().g().addE("knows").next();
        assert!(res.is_err(), "addE without from() or to() should error");
    }

    #[test]
    fn test_mean_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let avg = tx.g().V([]).hasLabel(["person"]).values(["age"]).mean().next().unwrap().unwrap();
        if let Value::Float64(f) = avg {
            assert!((f - 30.75).abs() < 0.1);
        }
    }

    #[test]
    fn test_label_vertex_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let labels: Vec<_> = tx.g().V([]).hasLabel(["person"]).label().to_list().unwrap();
        for l in &labels {
            assert!(matches!(l, Value::String(s) if s.as_str() == "person"));
        }
    }

    #[test]
    fn test_label_edge_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let labels: Vec<_> = tx.g().V([1]).outE(["knows"]).label().to_list().unwrap();
        for l in &labels {
            assert!(matches!(l, Value::String(s) if s.as_str() == "knows"));
        }
    }

    #[test]
    fn test_label_on_edge_with_haslabel() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let labels: Vec<_> = tx.g().V([1]).outE(["knows"]).hasLabel(["knows"]).label().to_list().unwrap();
        for l in &labels {
            assert!(matches!(l, Value::String(s) if s.as_str() == "knows"));
        }
    }

    #[test]
    fn test_add_e_variable_target_constant_source() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let edges: Vec<_> = tx.g().V([2, 4]).addE("friends").from(1).to_list().unwrap();
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_get_or_create_vertex_and_edge_in_one_query() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();

        // Use fresh ids that don't collide with the modern graph (ids 1-6).
        let a: i64 = 100;
        let b: i64 = 200;

        // Upsert vertex A: if exists, read id; otherwise create.
        tx.g()
            .V([a])
            .count()
            .coalesce([
                __().V([a]).id(),
                __().addV("person").property("id", a).property("name", "alice").property("age", 25i32),
            ])
            .next()
            .unwrap();

        // Upsert vertex B.
        tx.g()
            .V([b])
            .count()
            .coalesce([
                __().V([b]).id(),
                __().addV("person").property("id", b).property("name", "bob").property("age", 30i32),
            ])
            .next()
            .unwrap();

        // Upsert edge A → B.
        tx.g()
            .V([a])
            .coalesce([
                __().outE(["knows"]).r#where(__().otherV().hasId([b])).label(),
                __().addE("knows").from(a).to(b).property("weight", 0.5f64),
            ])
            .next()
            .unwrap();

        tx.commit().unwrap();

        // Verify: both vertices exist.
        let mut snap = graph.read();
        let a_count = snap.g().V([a]).count().next().unwrap().unwrap();
        assert!(matches!(a_count, Value::Int64(1)));
        let b_count = snap.g().V([b]).count().next().unwrap().unwrap();
        assert!(matches!(b_count, Value::Int64(1)));

        // Verify: edge exists.
        let edge_count = snap.g().V([a]).out(["knows"]).count().next().unwrap().unwrap();
        assert!(matches!(edge_count, Value::Int64(1)));

        // Second pass: run the same upsert again → no new elements (idempotent).
        let mut tx2 = graph.begin();
        tx2.g()
            .V([a])
            .count()
            .coalesce([
                __().V([a]).id(),
                __().addV("person").property("id", a).property("name", "alice").property("age", 25i32),
            ])
            .next()
            .unwrap();
        tx2.g()
            .V([b])
            .count()
            .coalesce([
                __().V([b]).id(),
                __().addV("person").property("id", b).property("name", "bob").property("age", 30i32),
            ])
            .next()
            .unwrap();
        tx2.g()
            .V([a])
            .coalesce([
                __().outE(["knows"]).r#where(__().otherV().hasId([b])).label(),
                __().addE("knows").from(a).to(b).property("weight", 0.5f64),
            ])
            .next()
            .unwrap();
        tx2.commit().unwrap();

        // Still exactly one edge after idempotent re-run.
        let mut snap2 = graph.read();
        let edge_count2 = snap2.g().V([a]).out(["knows"]).count().next().unwrap().unwrap();
        assert!(matches!(edge_count2, Value::Int64(1)));
    }

    #[test]
    fn test_and_explain_has_children() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let traversal = snap.g().V([]).and([__().has("name", "marko"), __().has("age", 29i32)]);
        let node = traversal.explain().unwrap();
        assert!(node.contains("AndStep"));
    }

    #[test]
    fn test_or_explain_has_children() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let traversal = snap.g().V([]).or([__().has("age", 999i32), __().has("name", "marko")]);
        let node = traversal.explain().unwrap();
        assert!(node.contains("OrStep"));
    }

    #[test]
    fn test_and_where_no_traverser_matches_fully() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let ct = snap
            .g()
            .V([])
            .hasLabel(["person"])
            .and([__().has("age", 27i32), __().has("name", "marko")])
            .count()
            .next()
            .unwrap()
            .unwrap();
        assert!(matches!(ct, Value::Int64(0)));
    }

    #[test]
    fn test_sum_on_float64_property() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 99i64).property("name", "test").property("score", 95.5f64).next().unwrap();
        tx.commit().unwrap();
        let mut snap = graph.read();
        let total = snap.g().V([99]).values(["score"]).sum().next().unwrap().unwrap();
        assert!(matches!(total, Value::Float64(f) if (f - 95.5).abs() < 0.01), "got {:?}", total);
    }

    #[test]
    fn test_reducer_on_empty_input_returns_null() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // No vertex has age 999 → empty stream → reducers return Null.
        let sum_val = snap.g().V([]).has("age", 999i32).values(["age"]).sum().next().unwrap().unwrap();
        let min_val = snap.g().V([]).has("age", 999i32).values(["age"]).min().next().unwrap().unwrap();
        let max_val = snap.g().V([]).has("age", 999i32).values(["age"]).max().next().unwrap().unwrap();
        let mean_val = snap.g().V([]).has("age", 999i32).values(["age"]).mean().next().unwrap().unwrap();
        assert!(matches!(sum_val, Value::Null));
        assert!(matches!(min_val, Value::Null));
        assert!(matches!(max_val, Value::Null));
        assert!(matches!(mean_val, Value::Null));
    }

    #[test]
    fn test_edge_id_is_unique_string() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // Marko has 2 knows edges → 2 distinct id strings.
        let ids: Vec<_> = snap.g().V([1]).outE(["knows"]).id().to_list().unwrap();
        assert_eq!(ids.len(), 2, "Marko should have 2 knows edges");
        // All ids are strings, distinct, and non-empty.
        for id in &ids {
            if let Value::String(s) = id {
                assert!(!s.is_empty());
            } else {
                panic!("expected String edge id, got {:?}", id);
            }
        }
    }

    #[test]
    fn test_e_bogus_id_returns_empty() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let result = snap.g().E(["not-a-valid-base64".to_string()]).next().unwrap();
        assert!(result.is_none(), "E with bogus string should return None");
    }

    #[test]
    fn test_e_lookup_captured_id() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // Get the edge id, then look it up by g.E([captured_id]).
        let id_val = snap.g().V([1]).outE(["knows"]).id().next().unwrap().unwrap();
        let id_str = match id_val {
            Value::String(s) => s,
            _ => panic!("expected String id, got {:?}", id_val),
        };
        let result = snap.g().E([id_str]).next().unwrap();
        assert!(result.is_some(), "g.E([captured_id]) should find the edge");
    }

    #[test]
    fn test_edge_ids_are_distinct() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let ids: Vec<_> = snap.g().V([1]).outE(["knows"]).id().to_list().unwrap();
        assert_eq!(ids.len(), 2);
        // The two ids must be distinct.
        if let (Value::String(a), Value::String(b)) = (&ids[0], &ids[1]) {
            assert_ne!(a, b, "two knows edges from Marko should have distinct ids");
        }
    }

    #[test]
    fn test_has_id_with_edge_string() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let id_val = snap.g().V([1]).outE(["knows"]).id().next().unwrap().unwrap();
        let id_str = match id_val {
            Value::String(s) => s,
            _ => panic!("expected String id"),
        };
        let count = snap.g().V([1]).outE(["knows"]).hasId([id_str.as_str()]).count().next().unwrap().unwrap();
        assert!(matches!(count, Value::Int64(1)), "hasId with edge id string should match exactly 1 edge");
    }

    #[test]
    fn test_out_empty_means_all_labels() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // out([]) = traverse all outgoing edges regardless of label.
        let count = snap.g().V([1]).out([]).count().next().unwrap().unwrap();
        // Marko has: knows→2, knows→4, created→3 = 3 total out-edges.
        assert!(matches!(count, Value::Int64(3)), "out([]) should see all out-edges, got {:?}", count);
    }

    #[test]
    fn test_in_empty_means_all_labels() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // in([]) from Lop(3): reverse of created edges.
        let count = snap.g().V([3]).r#in([]).count().next().unwrap().unwrap();
        // Lop has incoming created from 1, 4, 6 = 3.
        assert!(matches!(count, Value::Int64(3)), "in([]) should see all in-edges, got {:?}", count);
    }

    #[test]
    fn test_both_empty_means_all_labels() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // both([]) from Marko(1): out knows→2,4 + out created→3 + in (none) = 3.
        let count = snap.g().V([1]).both([]).count().next().unwrap().unwrap();
        assert!(matches!(count, Value::Int64(3)), "both([]) should see all edges, got {:?}", count);
    }

    #[test]
    fn test_oute_empty_means_all_labels() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let count = snap.g().V([1]).outE([]).count().next().unwrap().unwrap();
        assert!(matches!(count, Value::Int64(3)), "outE([]) should see all out-edges, got {:?}", count);
    }

    #[test]
    fn test_ine_empty_means_all_labels() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let count = snap.g().V([3]).inE([]).count().next().unwrap().unwrap();
        assert!(matches!(count, Value::Int64(3)), "inE([]) should see all in-edges, got {:?}", count);
    }

    #[test]
    fn test_bothe_empty_means_all_labels() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        let count = snap.g().V([1]).bothE([]).count().next().unwrap().unwrap();
        assert!(matches!(count, Value::Int64(3)), "bothE([]) should see all edges, got {:?}", count);
    }

    #[test]
    fn test_has_label_empty_returns_nothing() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // hasLabel([]) → Within([]) → vacuously false. Needs an explicit type since
        // hasLabel() now takes `impl Into<Predicate>` (restored gt/lt/between/within/
        // without expressiveness) rather than a concrete collection Item type — the
        // empty-array-infers-for-free trick only applies to the latter.
        let count = snap.g().V([]).hasLabel([] as [&str; 0]).count().next().unwrap().unwrap();
        assert!(matches!(count, Value::Int64(0)), "hasLabel([]) should match nothing, got {:?}", count);
    }

    #[test]
    fn test_properties_empty_returns_all() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // properties([]) = all properties (TinkerPop convention).
        let count = snap.g().V([1]).properties([]).count().next().unwrap().unwrap();
        // Marko has: name, age = 2 properties.
        assert!(matches!(count, Value::Int64(2)), "properties([]) should return all props, got {:?}", count);
    }

    #[test]
    fn test_values_empty_returns_all() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // values([]) = all property values (TinkerPop convention).
        let count = snap.g().V([1]).values([]).count().next().unwrap().unwrap();
        // Marko has: name="marko", age=29 = 2 values.
        assert!(matches!(count, Value::Int64(2)), "values([]) should return all values, got {:?}", count);
    }

    #[test]
    fn test_values_all_excludes_reserved_keys() {
        let graph = setup_modern_graph();
        let mut snap = graph.read();
        // values([]) should NOT include id, label, or rank.
        let values: Vec<_> = snap.g().V([1]).values([]).to_list().unwrap();
        // Marko has 2 ordinary properties: name="marko", age=29.
        assert_eq!(values.len(), 2, "values([]) should only return ordinary properties");
        // Neither "person" (label string) nor 1 (id) should appear.
        for v in &values {
            if let Value::String(s) = v {
                assert_ne!(s.as_str(), "person", "label should not appear in values([])");
            }
            if let Value::Int64(n) = v {
                assert_ne!(*n, 1, "vertex id should not appear in values([])");
            }
        }
    }

    #[test]
    fn test_reject_label_as_property() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        // property("label", ...) should always be rejected.
        let res = tx.g().V([1]).property("label", "person").next();
        assert!(res.is_err(), "property('label', ...) should be rejected");
    }

    #[test]
    fn test_choose_e2e() {
        let graph = setup_modern_graph();
        let mut tx = graph.begin();
        let results = tx
            .g()
            .V([])
            .hasLabel(["person"])
            .choose(__().has("age", 32i32), __().values(["name"]), None)
            .to_list()
            .unwrap();
        assert!(!results.is_empty());
    }
}
