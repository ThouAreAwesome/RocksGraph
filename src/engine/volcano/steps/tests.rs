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
mod cases {
    use crate::{
        engine::volcano::builder::PhysicalPlanBuilder,
        graph::LogicalGraph,
        planner::{
            apply_rules,
            logical_step::{
                AddEStep as LogicalAddEStep, AddVStep as LogicalAddVStep, BothEStep as LogicalBothEStep,
                BothStep as LogicalBothStep, CoalesceStep as LogicalCoalesceStep, CountStep as LogicalCountStep,
                DropStep as LogicalDropStep, HasIdStep as LogicalHasIdStep, HasLabelStep as LogicalHasLabelStep,
                HasPropertyStep as LogicalHasPropertyStep, InEStep as LogicalInEStep, InStep as LogicalInStep,
                InVStep as LogicalInVStep, LimitStep as LogicalLimitStep, LogicalPlan, LogicalStep,
                OtherVStep as LogicalOtherVStep, OutEStep as LogicalOutEStep, OutStep as LogicalOutStep,
                OutVStep as LogicalOutVStep, PropertiesStep as LogicalPropertiesStep,
                PropertyStep as LogicalPropertyStep, ScalarFilterStep as LogicalScalarFilterStep,
                UnionStep as LogicalUnionStep, VStep as LogicalVStep, ValuesStep as LogicalValuesStep,
                WhereStep as LogicalWhereStep,
            },
        },
        store::{traits::GraphStore, RocksStorage},
        types::{
            element::Property,
            error::StoreError,
            gvalue::Primitive,
            keys::{AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, LabelId, VertexKey},
            prop_key::LABEL,
            Direction, EdgeKey, GValue,
        },
    };
    use smallvec::smallvec;
    use smol_str::SmolStr;
    use std::{
        collections::HashMap, // For PhysicalPlan::inject
    };

    // Define LabelIds for common labels used across tests
    const PERSON_LABEL_ID: LabelId = 1;
    const SOFTWARE_LABEL_ID: LabelId = 2;
    const KNOWS_LABEL_ID: LabelId = 3;
    const CREATED_LABEL_ID: LabelId = 4;
    const FRIENDS_LABEL_ID: LabelId = 5;
    // --- Test Helpers ---
    /// Opens a new `RocksStorage` instance in a temporary directory for testing.
    fn open_rocks_store() -> (RocksStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        (store, dir)
    }

    /// Creates a new `LogicalGraph` instance from the given `RocksStorage`.
    fn create_logical_graph(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
        let schema = store.load_schema(crate::schema::GraphOptions::default()).unwrap();
        LogicalGraph::new(store.begin(), std::sync::Arc::new(std::sync::RwLock::new(schema)))
    }

    fn get_adjacent_edges_test(
        c: &mut LogicalGraph<RocksStorage>,
        vertex: VertexKey,
        direction: Direction,
        label: Option<LabelId>,
        dst: Option<&[VertexKey]>,
        limit: Option<u32>,
    ) -> Vec<EdgeKey> {
        c.get_adjacent_edges(
            vertex,
            direction,
            AdjacentEdgesOptions { label, dst, rank: None, start_from: None },
            limit,
        )
        .unwrap()
        .0
    }

    // Helper to create a TinkerPop Modern Graph
    /// Creates a standard TinkerPop Modern Graph with predefined vertices and edges.
    /// This graph is used as a common baseline for various test cases.
    ///
    /// The graph includes: Marko, Vadas, Lop, Josh, Ripple, Peter and their relationships.
    fn create_tinkerpop_modern_graph(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
        let mut graph = create_logical_graph(store);

        let (name_key, age_key, lang_key, weight_key) = {
            let mut schema = graph.schema.write().unwrap();
            schema.register_vertex_label("dummy").unwrap(); // ID 0
            schema.register_vertex_label("person").unwrap(); // ID 1 (PERSON_LABEL_ID)
            schema.register_vertex_label("software").unwrap(); // ID 2 (SOFTWARE_LABEL_ID)

            schema.register_edge_label("dummy").unwrap(); // ID 0
            schema.register_edge_label("dummy2").unwrap(); // ID 1
            schema.register_edge_label("dummy3").unwrap(); // ID 2
            schema.register_edge_label("knows").unwrap(); // ID 3 (KNOWS_LABEL_ID)
            schema.register_edge_label("created").unwrap(); // ID 4 (CREATED_LABEL_ID)
            schema.register_edge_label("friends").unwrap(); // ID 5 (FRIENDS_LABEL_ID)

            let name_key = schema.resolve_prop_key("name", crate::schema::DataType::String).unwrap();
            let age_key = schema.resolve_prop_key("age", crate::schema::DataType::Int32).unwrap();
            let lang_key = schema.resolve_prop_key("lang", crate::schema::DataType::String).unwrap();
            let weight_key = schema.resolve_prop_key("weight", crate::schema::DataType::Float64).unwrap();
            (name_key, age_key, lang_key, weight_key)
        };

        graph.staged_schema.staged_vertex_labels.insert(0);
        graph.staged_schema.staged_vertex_labels.insert(1);
        graph.staged_schema.staged_vertex_labels.insert(2);
        graph.staged_schema.staged_edge_labels.insert(0);
        graph.staged_schema.staged_edge_labels.insert(1);
        graph.staged_schema.staged_edge_labels.insert(2);
        graph.staged_schema.staged_edge_labels.insert(3);
        graph.staged_schema.staged_edge_labels.insert(4);
        graph.staged_schema.staged_edge_labels.insert(5);
        graph.staged_schema.staged_prop_keys.insert(name_key);
        graph.staged_schema.staged_prop_keys.insert(age_key);
        graph.staged_schema.staged_prop_keys.insert(lang_key);
        graph.staged_schema.staged_prop_keys.insert(weight_key);

        // Define LabelIds for common labels
        // Add Vertices
        let v_marko = graph.add_vertex(1, PERSON_LABEL_ID).unwrap();
        let name = Property {
            owner: CanonicalKey::Vertex(v_marko),
            key: name_key,
            value: Primitive::String(SmolStr::new("marko")),
        };
        graph.set_property(&name).unwrap();

        let age = Property { owner: CanonicalKey::Vertex(v_marko), key: age_key, value: Primitive::Int32(29) };
        graph.set_property(&age).unwrap();

        let v_vadas = graph.add_vertex(2, PERSON_LABEL_ID).unwrap();
        let vadas_name = Property {
            owner: CanonicalKey::Vertex(v_vadas),
            key: name_key,
            value: Primitive::String(SmolStr::new("vadas")),
        };
        graph.set_property(&vadas_name).unwrap();
        let vadas_age = Property { owner: CanonicalKey::Vertex(v_vadas), key: age_key, value: Primitive::Int32(27) };
        graph.set_property(&vadas_age).unwrap();

        let v_lop = graph.add_vertex(3, SOFTWARE_LABEL_ID).unwrap();
        let lop_name = Property {
            owner: CanonicalKey::Vertex(v_lop),
            key: name_key,
            value: Primitive::String(SmolStr::new("lop")),
        };
        graph.set_property(&lop_name).unwrap();
        let lop_lang = Property {
            owner: CanonicalKey::Vertex(v_lop),
            key: lang_key,
            value: Primitive::String(SmolStr::new("java")),
        };
        graph.set_property(&lop_lang).unwrap();

        let v_josh = graph.add_vertex(4, PERSON_LABEL_ID).unwrap();
        let josh_name = Property {
            owner: CanonicalKey::Vertex(v_josh),
            key: name_key,
            value: Primitive::String(SmolStr::new("josh")),
        };
        graph.set_property(&josh_name).unwrap();
        let josh_age = Property { owner: CanonicalKey::Vertex(v_josh), key: age_key, value: Primitive::Int32(32) };
        graph.set_property(&josh_age).unwrap();

        let v_ripple = graph.add_vertex(5, SOFTWARE_LABEL_ID).unwrap();
        let ripple_name = Property {
            owner: CanonicalKey::Vertex(v_ripple),
            key: name_key,
            value: Primitive::String(SmolStr::new("ripple")),
        };
        graph.set_property(&ripple_name).unwrap();
        let ripple_lang = Property {
            owner: CanonicalKey::Vertex(v_ripple),
            key: lang_key,
            value: Primitive::String(SmolStr::new("java")),
        };
        graph.set_property(&ripple_lang).unwrap();

        let v_peter = graph.add_vertex(6, PERSON_LABEL_ID).unwrap();
        let peter_name = Property {
            owner: CanonicalKey::Vertex(v_peter),
            key: name_key,
            value: Primitive::String(SmolStr::new("peter")),
        };
        graph.set_property(&peter_name).unwrap();
        let peter_age = Property { owner: CanonicalKey::Vertex(v_peter), key: age_key, value: Primitive::Int32(35) };
        graph.set_property(&peter_age).unwrap();
        // Add Edges
        let e1 = graph
            .add_edge(&EdgeKey {
                primary_id: v_marko,
                direction: crate::types::Direction::OUT,
                label_id: KNOWS_LABEL_ID,
                secondary_id: v_vadas,
                rank: 0,
            })
            .unwrap();
        let e1_weight = Property {
            owner: CanonicalKey::Edge(e1.canonical_edge_key()),
            key: weight_key,
            value: Primitive::Float64(0.5),
        };
        graph.set_property(&e1_weight).unwrap();

        let e2 = graph
            .add_edge(&EdgeKey {
                primary_id: v_marko,
                direction: crate::types::Direction::OUT,
                label_id: KNOWS_LABEL_ID,
                secondary_id: v_josh,
                rank: 0,
            })
            .unwrap();
        let e2_weight = Property {
            owner: CanonicalKey::Edge(e2.canonical_edge_key()),
            key: weight_key,
            value: Primitive::Float64(1.0),
        };
        graph.set_property(&e2_weight).unwrap();

        let e3 = graph
            .add_edge(
                &CanonicalEdgeKey { src_id: v_marko, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop }.out_key(),
            )
            .unwrap();
        let e3_weight = Property {
            owner: CanonicalKey::Edge(e3.canonical_edge_key()),
            key: weight_key,
            value: Primitive::Float64(0.4),
        };
        graph.set_property(&e3_weight).unwrap();

        let e4 = graph
            .add_edge(
                &CanonicalEdgeKey { src_id: v_josh, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_ripple }.out_key(),
            )
            .unwrap();
        let e4_weight = Property {
            owner: CanonicalKey::Edge(e4.canonical_edge_key()),
            key: weight_key,
            value: Primitive::Float64(1.0),
        };
        graph.set_property(&e4_weight).unwrap();

        let e5 = graph
            .add_edge(
                &CanonicalEdgeKey { src_id: v_josh, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop }.out_key(),
            )
            .unwrap();
        let e5_weight = Property {
            owner: CanonicalKey::Edge(e5.canonical_edge_key()),
            key: weight_key,
            value: Primitive::Float64(0.4),
        };
        graph.set_property(&e5_weight).unwrap();

        let e6 = graph
            .add_edge(
                &CanonicalEdgeKey { src_id: v_peter, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop }.out_key(),
            )
            .unwrap();
        let e6_weight = Property {
            owner: CanonicalKey::Edge(e6.canonical_edge_key()),
            key: weight_key,
            value: Primitive::Float64(0.2),
        };
        graph.set_property(&e6_weight).unwrap();

        graph.commit().unwrap(); // Commit all initial graph data

        // --- Verification after commit ---
        let mut verification_graph = create_logical_graph(store);

        // Verify Vertices
        let name_key = verification_graph.schema.read().unwrap().prop_key_id("name").unwrap();
        let age_key = verification_graph.schema.read().unwrap().prop_key_id("age").unwrap();
        let lang_key = verification_graph.schema.read().unwrap().prop_key_id("lang").unwrap();
        let weight_key = verification_graph.schema.read().unwrap().prop_key_id("weight").unwrap();

        let _marko_v = verification_graph.get_vertex(v_marko).unwrap().unwrap();
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_marko), name_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("marko"))
        );
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_marko), age_key).unwrap().unwrap(),
            Primitive::Int32(29)
        );

        let _vadas_v = verification_graph.get_vertex(v_vadas).unwrap().unwrap();
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_vadas), name_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("vadas"))
        );
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_vadas), age_key).unwrap().unwrap(),
            Primitive::Int32(27)
        );

        let _lop_v = verification_graph.get_vertex(v_lop).unwrap().unwrap();
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_lop), name_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("lop"))
        );
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_lop), lang_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("java"))
        );

        let _josh_v = verification_graph.get_vertex(v_josh).unwrap().unwrap();
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_josh), name_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("josh"))
        );
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_josh), age_key).unwrap().unwrap(),
            Primitive::Int32(32)
        );

        let _ripple_v = verification_graph.get_vertex(v_ripple).unwrap().unwrap();
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_ripple), name_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("ripple"))
        );
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_ripple), lang_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("java"))
        );

        let _peter_v = verification_graph.get_vertex(v_peter).unwrap().unwrap();
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_peter), name_key).unwrap().unwrap(),
            Primitive::String(SmolStr::new("peter"))
        );
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Vertex(v_peter), age_key).unwrap().unwrap(),
            Primitive::Int32(35)
        );

        // Verify Edges and their properties
        let _e1_edge = verification_graph.get_edge(&e1).unwrap().unwrap();
        assert_eq!(_e1_edge.primary_id, v_marko);
        assert_eq!(_e1_edge.secondary_id, v_vadas);
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Edge(e1.canonical_edge_key()), weight_key).unwrap().unwrap(),
            Primitive::Float64(0.5)
        );

        let _e2_edge = verification_graph.get_edge(&e2).unwrap().unwrap();
        assert_eq!(_e2_edge.label_id, KNOWS_LABEL_ID);
        assert_eq!(_e2_edge.primary_id, v_marko);
        assert_eq!(_e2_edge.secondary_id, v_josh);
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Edge(e2.canonical_edge_key()), weight_key).unwrap().unwrap(),
            Primitive::Float64(1.0)
        );

        let _e3_edge = verification_graph.get_edge(&e3).unwrap().unwrap();
        assert_eq!(_e3_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e3_edge.primary_id, v_marko);
        assert_eq!(_e3_edge.secondary_id, v_lop);
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Edge(e3.canonical_edge_key()), weight_key).unwrap().unwrap(),
            Primitive::Float64(0.4)
        );

        let _e4_edge = verification_graph.get_edge(&e4).unwrap().unwrap();
        assert_eq!(_e4_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e4_edge.primary_id, v_josh);
        assert_eq!(_e4_edge.secondary_id, v_ripple);
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Edge(e4.canonical_edge_key()), weight_key).unwrap().unwrap(),
            Primitive::Float64(1.0)
        );

        let _e5_edge = verification_graph.get_edge(&e5).unwrap().unwrap();
        assert_eq!(_e5_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e5_edge.primary_id, v_josh);
        assert_eq!(_e5_edge.secondary_id, v_lop);
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Edge(e5.canonical_edge_key()), weight_key).unwrap().unwrap(),
            Primitive::Float64(0.4)
        );

        let _e6_edge = verification_graph.get_edge(&e6).unwrap().unwrap();
        assert_eq!(_e6_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e6_edge.primary_id, v_peter);
        assert_eq!(_e6_edge.secondary_id, v_lop);
        assert_eq!(
            verification_graph.get_value(&CanonicalKey::Edge(e6.canonical_edge_key()), weight_key).unwrap().unwrap(),
            Primitive::Float64(0.2)
        );
        // --- End Verification ---

        create_logical_graph(store) // Return a fresh context for tests
    }

    /// Helper to print the TinkerPop Modern Graph in ASCII art format.
    fn print_tinkerpop_modern_graph_ascii(graph: &mut LogicalGraph<RocksStorage>) {
        println!("\n--- TinkerPop Modern Graph (ASCII Art) ---");

        // Map LabelIds to names for display
        let get_label_name = |label_id: LabelId| -> &str {
            match label_id {
                PERSON_LABEL_ID => "person",
                SOFTWARE_LABEL_ID => "software",
                KNOWS_LABEL_ID => "knows",
                CREATED_LABEL_ID => "created",
                FRIENDS_LABEL_ID => "friends",
                _ => "unknown",
            }
        };

        println!("\nVertices:");
        for id in 1..=6 {
            if let Ok(Some(vertex_key)) = graph.get_vertex(id) {
                print!("  ({})", vertex_key);
                println!();
            }
        }

        println!("\nEdges:");
        // Iterate through all vertices to get their outgoing edges
        for src_id in 1..=6 {
            let out_edges = get_adjacent_edges_test(graph, src_id, crate::types::Direction::OUT, None, None, None);
            for edge_key in out_edges {
                if let Ok(Some(ek)) = graph.get_edge(&edge_key) {
                    let label_name = get_label_name(ek.label_id);
                    print!("  ({:?}) --{}--> ({:?})", ek.primary_id, label_name, ek.secondary_id);
                    println!();
                }
            }
        }
        println!("-------------------------------------------\n");
    }

    // --- Test Case to print the graph ---
    #[test]
    fn test_print_tinkerpop_modern_graph() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        print_tinkerpop_modern_graph_ascii(&mut graph);

        // This test primarily prints the graph, but we can add a simple assertion
        // to ensure the graph is not empty.
        let marko = graph.get_vertex(1).unwrap().unwrap();
        assert_eq!(marko, 1);
    }

    // --- Mock Upstream for testing steps ---
    // UpstreamMock is no longer needed as we are using LogicalPlan and PhysicalPlanBuilder.

    // --- Test Cases for AddVStep ---
    #[test]
    fn test_add_v_step_to_empty_graph() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_logical_graph(&store);
        let test_vertex_id: VertexKey = 999; // Choose a unique ID for the test

        let mut properties = HashMap::new();
        properties.insert(SmolStr::new("name"), Primitive::String(SmolStr::new("marko")));
        properties.insert(SmolStr::new("age"), Primitive::Int32(29));
        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::AddV(LogicalAddVStep {
                label: "person".into(),
                vertex_id: Some(test_vertex_id),
                properties,
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let Some(result) = physical_plan.next(&mut graph).unwrap() else { panic!("Expected a result") };

        if let GValue::Vertex(v_key) = &result.value {
            assert_eq!(*v_key, test_vertex_id); // Check the returned VertexKey
            let _ = graph.get_vertex(*v_key).unwrap().unwrap(); // Fetch the actual vertex (populates overlay)
            let name_id = graph.schema.read().unwrap().prop_key_id("name").unwrap();
            let age_id = graph.schema.read().unwrap().prop_key_id("age").unwrap();
            assert_eq!(
                graph.get_value(&CanonicalKey::Vertex(*v_key), name_id).unwrap().unwrap(),
                Primitive::String(SmolStr::new("marko"))
            );
            assert_eq!(graph.get_value(&CanonicalKey::Vertex(*v_key), age_id).unwrap().unwrap(), Primitive::Int32(29));
            assert_eq!(
                graph.get_value(&CanonicalKey::Vertex(*v_key), name_id).unwrap().unwrap(),
                Primitive::String(SmolStr::new("marko"))
            );
        } else {
            panic!("Expected a Vertex GValue");
        }
        assert!(physical_plan.next(&mut graph).unwrap().is_none()); // Should only emit once
    }

    // --- Test Cases for AddEStep ---
    #[test]
    fn test_add_e_step_to_tinkerpop_modern_graph() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // Find existing vertices to connect
        let marko_id = graph.get_vertex(1).unwrap().unwrap(); // Assuming ID 1 for marko
        let vadas_id = graph.get_vertex(2).unwrap().unwrap(); // Assuming ID 2 for vadas

        let mut properties = HashMap::new();
        properties.insert(SmolStr::new("since"), Primitive::Int32(2020));

        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::AddE(LogicalAddEStep {
                label: "friends".into(),
                out_v_id: Some(marko_id),
                in_v_id: Some(vadas_id),
                properties,
                rank: None,
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph).unwrap().unwrap();
        if let GValue::Edge(e_key) = &result.value {
            let added_edge = graph.get_edge(e_key).unwrap().unwrap(); // Fetch the actual edge
            assert_eq!(added_edge.label_id, FRIENDS_LABEL_ID);
            assert_eq!(added_edge.primary_id, marko_id);
            assert_eq!(added_edge.secondary_id, vadas_id);
            let since_id = graph.schema.read().unwrap().prop_key_id("since").unwrap();
            assert_eq!(
                graph.get_value(&CanonicalKey::Edge(e_key.canonical_edge_key()), since_id).unwrap().unwrap(),
                Primitive::Int32(2020)
            );
        } else {
            panic!("Expected an Edge GValue");
        }
        assert!(physical_plan.next(&mut graph).unwrap().is_none()); // Should only emit once
    }

    // --- Test Cases for PropertyStep ---
    #[test]
    fn test_property_step_update_vertex_in_tinkerpop_modern_graph() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap(); // Assuming ID 1 for marko
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Property(LogicalPropertyStep {
                    prop_key: SmolStr::new("age"),
                    prop_value: Primitive::Int32(30),
                }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph).unwrap().unwrap();

        if let GValue::Vertex(v_key) = &result.value {
            let updated_vertex = graph.get_vertex(*v_key).unwrap().unwrap();
            assert_eq!(updated_vertex, marko_id);
            let name_id = graph.schema.read().unwrap().prop_key_id("name").unwrap();
            let age_id = graph.schema.read().unwrap().prop_key_id("age").unwrap();
            assert_eq!(
                graph.get_value(&CanonicalKey::Vertex(*v_key), name_id).unwrap().unwrap(),
                Primitive::String(SmolStr::new("marko"))
            );
            assert_eq!(graph.get_value(&CanonicalKey::Vertex(*v_key), age_id).unwrap().unwrap(), Primitive::Int32(30));
        // Updated
        } else {
            panic!("Expected a Vertex GValue");
        }
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
    }

    #[test]
    fn test_property_step_add_new_property_to_edge() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap();
        let josh_id = graph.get_vertex(4).unwrap().unwrap();
        let knows_edge_key = CanonicalEdgeKey { src_id: marko_id, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: josh_id }; // LabelId 3 for "knows"
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("weight"),
                    value: Primitive::Float64(1.0),
                }), // Ensure we are on the correct edge
                LogicalStep::Property(LogicalPropertyStep {
                    prop_key: SmolStr::new("duration"),
                    prop_value: Primitive::Int32(12),
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph).unwrap().unwrap();

        if let GValue::Edge(e_key) = &result.value {
            let updated_edge = graph.get_edge(e_key).unwrap().unwrap();
            assert_eq!(updated_edge.canonical_edge_key(), knows_edge_key);
            let duration_id = graph.schema.read().unwrap().prop_key_id("duration").unwrap();
            assert_eq!(
                graph.get_value(&CanonicalKey::Edge(e_key.canonical_edge_key()), duration_id).unwrap().unwrap(),
                Primitive::Int32(12)
            ); // New property
        } else {
            panic!("Expected an Edge GValue");
        }
        let duration_id = graph.schema.read().unwrap().prop_key_id("duration").unwrap();
        assert_eq!(
            graph.get_value(&CanonicalKey::Edge(knows_edge_key), duration_id).unwrap().unwrap(),
            Primitive::Int32(12)
        );
    }

    // --- Test Cases for HasPropertyStep ---
    #[test]
    fn test_has_property_step_match_vertex() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap();
        let vadas_id = graph.get_vertex(2).unwrap().unwrap(); // Assuming ID 2 for vadas
        let logical_plan = LogicalPlan {
            steps: vec![
                // Corrected to use PERSON_LABEL_ID
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id, vadas_id] }),
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("age"),
                    value: Primitive::Int32(29),
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph).unwrap().unwrap();
        if let GValue::Vertex(v_key) = &result.value {
            assert_eq!(*v_key, marko_id);
        } else {
            panic!("Expected Marko");
        }
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
    }

    #[test]
    fn test_has_property_step_match_edge() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap();
        let vadas_id = graph.get_vertex(2).unwrap().unwrap();
        let _knows_edge_key = CanonicalEdgeKey { src_id: marko_id, label_id: 3, rank: 0, dst_id: vadas_id };
        let josh_id = graph.get_vertex(4).unwrap().unwrap();
        let _lop_id = graph.get_vertex(3).unwrap().unwrap();
        let ripple_id = graph.get_vertex(5).unwrap().unwrap();
        let created_edge_key =
            CanonicalEdgeKey { src_id: josh_id, label_id: CREATED_LABEL_ID, rank: 0, dst_id: ripple_id }; // Josh created Ripple has weight 1
                                                                                                          // Start from Marko and Josh, get their outgoing edges with label CREATED_LABEL_ID, and filter by weight = 1
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id, josh_id] }), // Start from Marko and Josh
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["created".into()],
                    end_vertex_ids: None,
                    rank: None,
                }), /* Get all outgoing edges */
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("weight"),
                    value: Primitive::Float64(1.0),
                }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph).unwrap().unwrap();

        if let GValue::Edge(e_key) = &result.value {
            assert_eq!(e_key.canonical_edge_key(), created_edge_key); // Josh created Ripple with weight 1
        } else {
            panic!("Expected created_edge_arc");
        }
        assert!(physical_plan.next(&mut graph).unwrap().is_none());

        // Start from Marko and Josh, get their outgoing edges without label filter, but filter by weight = 0.4 (should
        // match Marko->Lop and Josh->Lop)
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id, josh_id] }), // Start from Marko and Josh
                LogicalStep::OutE(LogicalOutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }), /* Get all outgoing
                                                                                                               * edges */
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("weight"),
                    value: Primitive::Float64(1.0),
                }),
            ],
        };

        let expected_edge_keys = [
            CanonicalEdgeKey { src_id: marko_id, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: josh_id }, /* Marko created Lop with weight 0.4 */
            CanonicalEdgeKey { src_id: josh_id, label_id: CREATED_LABEL_ID, rank: 0, dst_id: ripple_id }, /* Josh created Lop with weight 0.4 */
        ];

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph).unwrap().unwrap();

        if let GValue::Edge(e_key) = &result.value {
            assert_eq!(e_key.canonical_edge_key(), expected_edge_keys[0]); // Josh created Ripple with weight 1
        } else {
            panic!("Expected created_edge_arc");
        }

        let Some(result) = physical_plan.next(&mut graph).unwrap() else { panic!("Expected a result") };
        if let GValue::Edge(e_key) = &result.value {
            assert_eq!(e_key.canonical_edge_key(), expected_edge_keys[1]); // Josh created Ripple with weight 1
        } else {
            panic!("Expected created_edge_arc");
        }
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
    }

    #[test]
    fn test_union_out_e_count_in_e_count() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        // Sub-plan 1: outE().count()
        let out_e_count_sub_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(LogicalOutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Count(LogicalCountStep {}),
            ],
        };

        // Sub-plan 2: inE().count()
        let in_e_count_sub_plan = LogicalPlan {
            steps: vec![
                LogicalStep::InE(LogicalInEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Count(LogicalCountStep {}),
            ],
        };

        // Main plan: V(marko_id).union(outE().count(), inE().count())
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Union(LogicalUnionStep { plans: smallvec![out_e_count_sub_plan, in_e_count_sub_plan] }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();

        let mut results = Vec::new();
        while let Ok(Some(traverser)) = physical_plan.next(&mut graph) {
            results.push(traverser.as_ref().value.clone());
        }

        // Marko has 3 outgoing edges and 0 incoming edges in the TinkerPop Modern Graph
        assert_eq!(results.len(), 2);
        assert!(results.contains(&GValue::Scalar(Primitive::Int64(3))));
        assert!(results.contains(&GValue::Scalar(Primitive::Int64(0))));
    }

    #[test]
    fn test_out_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(2)));
        assert!(results.contains(&GValue::Vertex(3)));
        assert!(results.contains(&GValue::Vertex(4)));
    }

    #[test]
    fn test_in_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let lop_id = graph.get_vertex(3).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![lop_id] }),
                LogicalStep::In(LogicalInStep { labels: smallvec![], end_vertex_ids: None }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(1)));
        assert!(results.contains(&GValue::Vertex(4)));
        assert!(results.contains(&GValue::Vertex(6)));
    }

    #[test]
    fn test_out_v_in_v_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        // V(1).outE().inV() equivalent to V(1).out()
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::InV(LogicalInVStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(2)));
        assert!(results.contains(&GValue::Vertex(3)));
        assert!(results.contains(&GValue::Vertex(4)));

        // V(1).outE().outV() should return Marko 3 times
        let logical_plan2 = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::OutV(LogicalOutVStep {}),
            ],
        };
        let mut builder2: PhysicalPlanBuilder = Default::default();
        let physical_plan2 = builder2.build(&logical_plan2, &graph.schema).unwrap();
        let mut results2 = Vec::new();
        while let Ok(Some(t)) = physical_plan2.next(&mut graph) {
            results2.push(t.as_ref().value.clone());
        }
        assert_eq!(results2.len(), 3);
        assert!(results2.iter().all(|v| v == &GValue::Vertex(1)));
    }

    #[test]
    fn test_both_and_both_e_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let josh_id = graph.get_vertex(4).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![josh_id] }),
                LogicalStep::Both(LogicalBothStep { labels: smallvec![], end_vertex_ids: None }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(1)));
        assert!(results.contains(&GValue::Vertex(3)));
        assert!(results.contains(&GValue::Vertex(5)));

        let logical_plan_e = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![josh_id] }),
                LogicalStep::BothE(LogicalBothEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan_e = builder.build(&logical_plan_e, &graph.schema).unwrap();
        let mut results_e = Vec::new();
        while let Ok(Some(t)) = physical_plan_e.next(&mut graph) {
            results_e.push(t.as_ref().value.clone());
        }
        assert_eq!(results_e.len(), 3);
    }

    #[test]
    fn test_has_label_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::HasLabel(LogicalHasLabelStep { labels: smallvec!["software".into()] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], GValue::Vertex(3)); // Lop
    }

    #[test]
    fn test_other_v_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::OtherV(LogicalOtherVStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(2)));
        assert!(results.contains(&GValue::Vertex(3)));
        assert!(results.contains(&GValue::Vertex(4)));
    }

    #[test]
    fn test_values_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Values(LogicalValuesStep {
                    property_keys: smallvec![SmolStr::new("name"), SmolStr::new("age")],
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 2);
        assert!(results.contains(&GValue::Scalar(Primitive::String(SmolStr::new("marko")))));
        assert!(results.contains(&GValue::Scalar(Primitive::Int32(29))));
    }

    #[test]
    fn test_properties_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Properties(LogicalPropertiesStep {
                    property_keys: smallvec![SmolStr::new("name"), SmolStr::new("age"), LABEL],
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(matches!(results[0], GValue::Property(_)));
        assert!(matches!(results[1], GValue::Property(_)));
        assert!(matches!(results[2], GValue::Property(_)));
        let keys: Vec<SmolStr> = results
            .iter()
            .map(|p| match p {
                GValue::Property(Property { owner: _, key, value: _ }) => {
                    graph.schema.read().unwrap().prop_key_str(*key).unwrap().clone()
                }
                _ => unreachable!("unexpecte result"),
            })
            .collect();
        assert!(keys.contains(&SmolStr::new("name")));
        assert!(keys.contains(&SmolStr::new("age")));
        assert!(keys.contains(&LABEL));

        let owners: Vec<CanonicalKey> = results
            .iter()
            .map(|p| match p {
                GValue::Property(Property { owner, key: _, value: _ }) => *owner,
                _ => unreachable!("unexpecte result"),
            })
            .collect();
        assert_eq!(
            owners.as_slice(),
            &[CanonicalKey::Vertex(marko_id), CanonicalKey::Vertex(marko_id), CanonicalKey::Vertex(marko_id)]
        )
    }

    #[test]
    fn test_scalar_filter_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Values(LogicalValuesStep { property_keys: smallvec![SmolStr::new("age")] }),
                LogicalStep::ScalarFilter(LogicalScalarFilterStep { value: Primitive::Int32(29) }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], GValue::Scalar(Primitive::Int32(29)));
    }

    #[test]
    fn test_where_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let sub_plan = LogicalPlan {
            steps: vec![
                LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::HasLabel(LogicalHasLabelStep { labels: smallvec!["software".into()] }),
            ],
        };
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![1, 2, 3, 4, 5, 6] }),
                LogicalStep::Where(LogicalWhereStep { plan: sub_plan }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(1)));
        assert!(results.contains(&GValue::Vertex(4)));
        assert!(results.contains(&GValue::Vertex(6)));
    }

    #[test]
    fn test_add_v_step_duplicate_vertex_returns_error() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // Vertex 1 (marko) already exists in the committed graph.
        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::AddV(LogicalAddVStep {
                label: "person".into(),
                vertex_id: Some(1),
                properties: HashMap::new(),
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph);

        assert!(matches!(result, Err(StoreError::DuplicateVertex(1))));
    }

    #[test]
    fn test_add_e_step_duplicate_edge_returns_error() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // The marko->vadas "knows" edge already exists in the committed graph.
        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::AddE(LogicalAddEStep {
                label: "knows".into(),
                out_v_id: Some(1),
                in_v_id: Some(2),
                properties: HashMap::new(),
                rank: None,
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let result = physical_plan.next(&mut graph);

        assert!(matches!(result, Err(StoreError::DuplicateEdge(_))));
    }

    #[test]
    fn test_out_multiple_labels() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Out(LogicalOutStep {
                    labels: smallvec!["knows".into(), "created".into()],
                    end_vertex_ids: None,
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 3); // 2 knows + 1 created
    }

    // --- Test Cases for DropStep ---

    #[test]
    fn test_drop_edge_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id: VertexKey = 1;
        let vadas_id: VertexKey = 2;

        // Drop the marko->vadas "knows" edge.
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: Some(smallvec![vadas_id]),
                    rank: None,
                }),
                LogicalStep::Drop(LogicalDropStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
        graph.commit().unwrap();

        let mut verify = create_logical_graph(&store);
        let cek = CanonicalEdgeKey { src_id: marko_id, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: vadas_id };
        assert!(verify.get_edge(&cek.out_key()).unwrap().is_none());
        // Both endpoint vertices are still present.
        assert!(verify.get_vertex(marko_id).unwrap().is_some());
        assert!(verify.get_vertex(vadas_id).unwrap().is_some());
        // Marko's remaining two outgoing edges are unaffected.
        let remaining = get_adjacent_edges_test(&mut verify, marko_id, Direction::OUT, None, None, None);
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_drop_all_out_edges_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let josh_id: VertexKey = 4;

        // josh has two outgoing "created" edges (to ripple and lop).  Drop both.
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![josh_id] }),
                LogicalStep::OutE(LogicalOutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Drop(LogicalDropStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
        graph.commit().unwrap();

        let mut verify = create_logical_graph(&store);
        assert!(get_adjacent_edges_test(&mut verify, josh_id, Direction::OUT, None, None, None).is_empty());
        // josh and the target vertices still exist.
        assert!(verify.get_vertex(josh_id).unwrap().is_some());
        assert!(verify.get_vertex(3).unwrap().is_some()); // lop
        assert!(verify.get_vertex(5).unwrap().is_some()); // ripple
    }

    #[test]
    fn test_drop_vertex_step() {
        let (store, _dir) = open_rocks_store();

        // Commit an isolated vertex that has no edges.
        {
            let mut setup = create_logical_graph(&store);
            setup.add_vertex(99, PERSON_LABEL_ID).unwrap();
            setup.commit().unwrap();
        }

        // Drop it via the DropStep.
        let mut graph = create_logical_graph(&store);
        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::V(LogicalVStep { ids: smallvec![99] }), LogicalStep::Drop(LogicalDropStep {})],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
        graph.commit().unwrap();

        let mut verify = create_logical_graph(&store);
        assert!(verify.get_vertex(99).unwrap().is_none());
    }

    #[test]
    fn test_drop_vertex_with_incident_edges_fails() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // marko (id=1) has three outgoing edges; dropping it must fail.
        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::V(LogicalVStep { ids: smallvec![1] }), LogicalStep::Drop(LogicalDropStep {})],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(matches!(physical_plan.next(&mut graph), Err(StoreError::IncidentEdges)));
    }

    #[test]
    fn test_drop_property_on_vertex_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id: VertexKey = 1;

        // Drop the "age" property from marko.
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Properties(LogicalPropertiesStep { property_keys: smallvec![SmolStr::new("age")] }),
                LogicalStep::Drop(LogicalDropStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
        graph.commit().unwrap();

        let mut verify = create_logical_graph(&store);
        let _ = verify.get_vertex(marko_id).unwrap().unwrap();
        let age_id = verify.schema.read().unwrap().prop_key_id("age").unwrap();
        let name_id = verify.schema.read().unwrap().prop_key_id("name").unwrap();
        assert!(verify.get_value(&CanonicalKey::Vertex(marko_id), age_id).unwrap().is_none());
        // "name" is untouched.
        assert_eq!(
            verify.get_value(&CanonicalKey::Vertex(marko_id), name_id).unwrap().unwrap(),
            Primitive::String(SmolStr::new("marko"))
        );
    }

    #[test]
    fn test_drop_property_on_edge_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id: VertexKey = 1;
        let josh_id: VertexKey = 4;
        let edge_cek = CanonicalEdgeKey { src_id: marko_id, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: josh_id };

        // Drop the "weight" property from marko->josh "knows" edge.
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: Some(smallvec![josh_id]),
                    rank: None,
                }),
                LogicalStep::Properties(LogicalPropertiesStep { property_keys: smallvec![SmolStr::new("weight")] }),
                LogicalStep::Drop(LogicalDropStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
        graph.commit().unwrap();

        let mut verify = create_logical_graph(&store);
        let _ = verify.get_edge(&edge_cek.out_key()).unwrap().unwrap();
        let weight_id = verify.schema.read().unwrap().prop_key_id("weight").unwrap();
        assert!(verify.get_value(&CanonicalKey::Edge(edge_cek), weight_id).unwrap().is_none());
    }

    #[test]
    fn test_drop_edge_then_drop_vertex() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // vadas (id=2) has exactly one edge: marko->vadas "knows".
        let marko_id: VertexKey = 1;
        let vadas_id: VertexKey = 2;
        let edge_cek = CanonicalEdgeKey { src_id: marko_id, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: vadas_id };

        // Phase 1: drop the incident edge.
        let drop_edge_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: Some(smallvec![vadas_id]),
                    rank: None,
                }),
                LogicalStep::Drop(LogicalDropStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&drop_edge_plan, &graph.schema).unwrap();
        assert!(physical_plan.next(&mut graph).unwrap().is_none());
        graph.commit().unwrap();

        // Phase 2: now that vadas has no edges, drop the vertex.
        let mut graph2 = create_logical_graph(&store);
        let drop_v_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![vadas_id] }),
                LogicalStep::Drop(LogicalDropStep {}),
            ],
        };
        let mut builder2: PhysicalPlanBuilder = Default::default();
        let physical_plan2 = builder2.build(&drop_v_plan, &graph2.schema).unwrap();
        assert!(physical_plan2.next(&mut graph2).unwrap().is_none());
        graph2.commit().unwrap();

        // Verify: edge and vertex are gone; marko is unaffected.
        let mut verify = create_logical_graph(&store);
        assert!(verify.get_vertex(vadas_id).unwrap().is_none());
        assert!(verify.get_edge(&edge_cek.out_key()).unwrap().is_none());
        assert!(verify.get_vertex(marko_id).unwrap().is_some());
    }
    #[test]
    fn test_limit_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // g.V(1).hasLabel("person").outE("knows").limit(1)
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
                LogicalStep::HasLabel(LogicalHasLabelStep { labels: smallvec!["person".into()] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::Limit(LogicalLimitStep { limit: 1 }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        // Marko is the only person with outgoing "knows" edges (he has 2).
        // limit(1) should reduce the output to a single edge.
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], GValue::Edge(_)));
    }

    #[test]
    fn test_coalesce_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = 1;

        // g.V(1).coalesce(__.outE("created"), __.outE("knows"))
        // First branch: outE("created")
        let created_plan = LogicalPlan {
            steps: vec![LogicalStep::OutE(LogicalOutEStep {
                labels: smallvec!["created".into()],
                end_vertex_ids: None,
                rank: None,
            })],
        };

        // Second branch: outE("knows")
        let knows_plan = LogicalPlan {
            steps: vec![LogicalStep::OutE(LogicalOutEStep {
                labels: smallvec!["knows".into()],
                end_vertex_ids: None,
                rank: None,
            })],
        };

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Coalesce(LogicalCoalesceStep { plans: vec![created_plan, knows_plan] }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }

        // Marko has one "created" edge to lop. Coalesce should return this and stop.
        assert_eq!(results.len(), 1);
        if let GValue::Edge(edge) = &results[0] {
            assert_eq!(edge.primary_id, 1);
            assert_eq!(edge.secondary_id, 3);
            assert_eq!(edge.label_id, CREATED_LABEL_ID);
        } else {
            panic!("Expected an edge result");
        }
    }

    #[test]
    fn test_has_id_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = 1;

        // g.V(1).out().hasId(3, 4)
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![3, 4] }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        results.sort_by_key(|v| if let GValue::Vertex(id) = v { *id } else { 0 });

        assert_eq!(results.len(), 2);
        assert_eq!(results[0], GValue::Vertex(3)); // lop
        assert_eq!(results[1], GValue::Vertex(4)); // josh
    }

    #[test]
    fn test_get_e_step_via_optimizer() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = 1;
        let josh_id = 4;

        // g.V(1).outE("knows").where(__.otherV().hasId(4))
        let where_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OtherV(LogicalOtherVStep {}),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![josh_id] }),
            ],
        };

        let mut logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::Where(LogicalWhereStep { plan: where_plan }),
            ],
        };

        // The optimizer should convert this into a plan that uses GetEStep
        apply_rules(&mut logical_plan).unwrap();

        // Verify the optimizer folded the `where` clause into the `OutE` step
        if let LogicalStep::OutE(s) = &logical_plan.steps[1] {
            assert_eq!(s.end_vertex_ids, Some(smallvec![josh_id]));
        } else {
            panic!("Optimizer did not modify OutE step as expected");
        }

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }

        assert_eq!(results.len(), 1);
        if let GValue::Edge(edge) = &results[0] {
            assert_eq!(edge.primary_id, marko_id);
            assert_eq!(edge.secondary_id, josh_id);
            assert_eq!(edge.label_id, KNOWS_LABEL_ID);
        } else {
            panic!("Expected an edge result");
        }
    }

    #[test]
    fn test_get_e_step_in_e_direction_via_optimizer() {
        // g.V(josh).inE("knows").where(otherV().hasId(marko)) — exercises GetEStep's IN-direction
        // path: marko -knows-> josh is stored as an OUT edge from marko, found here by looking it
        // up from josh's side via the symmetric IN-direction key.
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = 1;
        let josh_id = 4;

        let where_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OtherV(LogicalOtherVStep {}),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![marko_id] }),
            ],
        };
        let mut logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![josh_id] }),
                LogicalStep::InE(LogicalInEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::Where(LogicalWhereStep { plan: where_plan }),
            ],
        };
        apply_rules(&mut logical_plan).unwrap();

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(format!("{:?}", physical_plan).contains("GetEStep"), "expected GetEStep to be chosen");

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }

        assert_eq!(results.len(), 1, "expected to find the marko->josh edge from josh's inE side");
        if let GValue::Edge(edge) = &results[0] {
            assert_eq!(edge.canonical_edge_key().src_id, marko_id);
            assert_eq!(edge.canonical_edge_key().dst_id, josh_id);
        } else {
            panic!("Expected an edge result");
        }
    }

    #[test]
    fn test_get_e_step_both_e_direction_via_optimizer() {
        // g.V(josh).bothE("knows").where(otherV().hasId(marko)) — josh has no outgoing "knows"
        // edge to marko, only an incoming one from marko; bothE must check both directions.
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = 1;
        let josh_id = 4;

        let where_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OtherV(LogicalOtherVStep {}),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![marko_id] }),
            ],
        };
        let mut logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![josh_id] }),
                LogicalStep::BothE(LogicalBothEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::Where(LogicalWhereStep { plan: where_plan }),
            ],
        };
        apply_rules(&mut logical_plan).unwrap();

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(format!("{:?}", physical_plan).contains("GetEStep"), "expected GetEStep to be chosen");

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 1, "bothE should find the edge regardless of which side it was stored from");
    }

    #[test]
    fn test_get_e_step_out_vertex_emission_via_optimizer() {
        // g.V(marko).out("knows").hasId(josh) — vertex-emitting Out should also route through
        // GetEStep (output_edges=false) and yield the adjacent Vertex, not an Edge.
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = 1;
        let josh_id = 4;

        let mut logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { labels: smallvec!["knows".into()], end_vertex_ids: None }),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![josh_id] }),
            ],
        };
        apply_rules(&mut logical_plan).unwrap();

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(format!("{:?}", physical_plan).contains("GetEStep"), "expected GetEStep to be chosen");

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results, vec![GValue::Vertex(josh_id)]);
    }

    #[test]
    fn test_get_e_step_exact_rank_point_lookup() {
        // Multi-edge "knows" between marko and josh at rank 0 and rank 1.
        // g.V(marko).outE("knows").where(otherV().hasId(josh)).has("rank", 1) should resolve to a
        // single GetEStep point lookup keyed on the exact rank, not just label+dst.
        let (store, _dir) = open_rocks_store();
        let schema = std::sync::Arc::new(std::sync::RwLock::new(crate::schema::Schema::new()));
        {
            let mut s = schema.write().unwrap();
            s.edge_mode = crate::schema::definition::EdgeMode::Multi;
            s.register_vertex_label("dummy").unwrap(); // 0
            s.register_vertex_label("person").unwrap(); // 1 (PERSON_LABEL_ID)
            s.register_edge_label("dummy").unwrap(); // 0
            s.register_edge_label("dummy2").unwrap(); // 1
            s.register_edge_label("dummy3").unwrap(); // 2
            s.register_edge_label("knows").unwrap(); // 3 (KNOWS_LABEL_ID)
        }
        let mut graph: LogicalGraph<RocksStorage> = LogicalGraph::new(store.begin(), schema);
        graph.staged_schema.staged_vertex_labels.insert(0);
        graph.staged_schema.staged_vertex_labels.insert(1);
        graph.staged_schema.staged_edge_labels.insert(0);
        graph.staged_schema.staged_edge_labels.insert(1);
        graph.staged_schema.staged_edge_labels.insert(2);
        graph.staged_schema.staged_edge_labels.insert(3);

        let marko_id = graph.add_vertex(1, PERSON_LABEL_ID).unwrap();
        let josh_id = graph.add_vertex(4, PERSON_LABEL_ID).unwrap();
        graph
            .add_edge(&EdgeKey {
                primary_id: marko_id,
                direction: crate::types::Direction::OUT,
                label_id: KNOWS_LABEL_ID,
                secondary_id: josh_id,
                rank: 0,
            })
            .unwrap();
        graph
            .add_edge(&EdgeKey {
                primary_id: marko_id,
                direction: crate::types::Direction::OUT,
                label_id: KNOWS_LABEL_ID,
                secondary_id: josh_id,
                rank: 1,
            })
            .unwrap();
        graph.commit().unwrap();

        let where_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OtherV(LogicalOtherVStep {}),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![josh_id] }),
            ],
        };
        let mut logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::Where(LogicalWhereStep { plan: where_plan }),
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("rank"),
                    value: Primitive::Int32(1),
                }),
            ],
        };
        apply_rules(&mut logical_plan).unwrap();

        // Both the end-vertex id and the rank should have folded into the same OutE step.
        if let LogicalStep::OutE(s) = &logical_plan.steps[1] {
            assert_eq!(s.end_vertex_ids, Some(smallvec![josh_id]));
            assert_eq!(s.rank, Some(1));
        } else {
            panic!("expected OutE with end_vertex_ids and rank merged");
        }
        assert_eq!(logical_plan.steps.len(), 2, "the where() and has(\"rank\",1) steps should both be folded away");

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(format!("{:?}", physical_plan).contains("GetEStep"), "expected GetEStep to be chosen");

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 1, "should find exactly the rank=1 edge, not both");
        if let GValue::Edge(edge) = &results[0] {
            assert_eq!(edge.rank, 1);
        } else {
            panic!("Expected an edge result");
        }
    }

    #[test]
    fn test_multi_edge_label_without_rank_filter_falls_back_to_scan() {
        // Same multi-edge "knows" setup as test_get_e_step_exact_rank_point_lookup, but this
        // time there's no explicit `.has("rank", N)` filter — only the end-vertex id is known.
        // build_step must NOT guess GetEStep's implied rank=0 here (that would silently miss
        // the rank=1 edge); it must fall back to the scan, which correctly returns both edges
        // regardless of rank.
        let (store, _dir) = open_rocks_store();
        let schema = std::sync::Arc::new(std::sync::RwLock::new(crate::schema::Schema::new()));
        {
            let mut s = schema.write().unwrap();
            s.edge_mode = crate::schema::definition::EdgeMode::Multi;
            s.register_vertex_label("dummy").unwrap(); // 0
            s.register_vertex_label("person").unwrap(); // 1 (PERSON_LABEL_ID)
            s.register_edge_label("dummy").unwrap(); // 0
            s.register_edge_label("dummy2").unwrap(); // 1
            s.register_edge_label("dummy3").unwrap(); // 2
            s.register_edge_label("knows").unwrap(); // 3 (KNOWS_LABEL_ID)
        }
        let mut graph: LogicalGraph<RocksStorage> = LogicalGraph::new(store.begin(), schema);
        graph.staged_schema.staged_vertex_labels.insert(0);
        graph.staged_schema.staged_vertex_labels.insert(1);
        graph.staged_schema.staged_edge_labels.insert(0);
        graph.staged_schema.staged_edge_labels.insert(1);
        graph.staged_schema.staged_edge_labels.insert(2);
        graph.staged_schema.staged_edge_labels.insert(3);

        let marko_id = graph.add_vertex(1, PERSON_LABEL_ID).unwrap();
        let josh_id = graph.add_vertex(4, PERSON_LABEL_ID).unwrap();
        graph
            .add_edge(&EdgeKey {
                primary_id: marko_id,
                direction: crate::types::Direction::OUT,
                label_id: KNOWS_LABEL_ID,
                secondary_id: josh_id,
                rank: 0,
            })
            .unwrap();
        graph
            .add_edge(&EdgeKey {
                primary_id: marko_id,
                direction: crate::types::Direction::OUT,
                label_id: KNOWS_LABEL_ID,
                secondary_id: josh_id,
                rank: 1,
            })
            .unwrap();
        graph.commit().unwrap();

        let where_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OtherV(LogicalOtherVStep {}),
                LogicalStep::HasId(LogicalHasIdStep { ids: smallvec![josh_id] }),
            ],
        };
        let mut logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::Where(LogicalWhereStep { plan: where_plan }),
            ],
        };
        apply_rules(&mut logical_plan).unwrap();

        if let LogicalStep::OutE(s) = &logical_plan.steps[1] {
            assert_eq!(s.end_vertex_ids, Some(smallvec![josh_id]));
            assert_eq!(s.rank, None, "no rank filter was given");
        } else {
            panic!("expected OutE with end_vertex_ids merged");
        }

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        assert!(
            !format!("{:?}", physical_plan).contains("GetEStep"),
            "must not use GetEStep when rank is unknown for a multi-edge label"
        );

        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 2, "the scan must find both ranks, not just rank=0");
    }

    #[test]
    fn test_dedup_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![1, 1, 4, 4] }),
                LogicalStep::Dedup(crate::planner::logical_step::DedupStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 2);
        assert!(results.contains(&GValue::Vertex(1)));
        assert!(results.contains(&GValue::Vertex(4)));
    }

    #[test]
    fn test_fold_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::Values(LogicalValuesStep {
                    property_keys: smallvec![SmolStr::new("name"), SmolStr::new("age")],
                }),
                LogicalStep::Fold(crate::planner::logical_step::FoldStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }
        assert_eq!(results.len(), 1);
        if let GValue::List(list) = &results[0] {
            assert_eq!(list.len(), 2);
            assert!(list.contains(&GValue::Scalar(Primitive::String(SmolStr::new("marko")))));
            assert!(list.contains(&GValue::Scalar(Primitive::Int32(29))));
        } else {
            panic!("Expected a list result from fold");
        }
    }

    #[test]
    fn test_path_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();

        // g.V(1).outE("knows").inV().path()
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::InV(LogicalInVStep {}),
                LogicalStep::Path(crate::planner::logical_step::PathStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }

        // Marko knows V(2) (vadas) and V(4) (josh)
        assert_eq!(results.len(), 2);
        for res in results {
            if let GValue::Path(path) = res {
                assert_eq!(path.len(), 3);
                assert_eq!(path[0].0, GValue::Vertex(1));
                assert!(matches!(path[1].0, GValue::Edge(_)));
                assert!(matches!(path[2].0, GValue::Vertex(2) | GValue::Vertex(4)));
            } else {
                panic!("Expected path results");
            }
        }
    }

    #[test]
    fn test_end_vertex_filter_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap();
        let josh_id = graph.get_vertex(4).unwrap().unwrap();

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep {
                    labels: smallvec!["knows".into(), "created".into()],
                    end_vertex_ids: None,
                    rank: None,
                }),
                LogicalStep::EndVertexFilter(crate::planner::logical_step::EndVertexFilter { ids: smallvec![josh_id] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let mut results = Vec::new();
        while let Ok(Some(t)) = physical_plan.next(&mut graph) {
            results.push(t.as_ref().value.clone());
        }

        assert_eq!(results.len(), 1);
        if let GValue::Edge(edge) = &results[0] {
            assert_eq!(edge.secondary_id, josh_id);
        } else {
            panic!("Expected an edge result");
        }
    }

    #[test]
    fn test_end_vertex_filter_non_edge_error() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: smallvec![1] }),
                LogicalStep::EndVertexFilter(crate::planner::logical_step::EndVertexFilter { ids: smallvec![2] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan, &graph.schema).unwrap();
        let res = physical_plan.next(&mut graph);
        assert!(matches!(res, Err(StoreError::UnexpectedDataType(_))));
    }

    #[test]
    fn test_step_edge_cases_with_graph() {
        use crate::{
            engine::{
                traverser::Traverser,
                volcano::{
                    builder::PhysicalPlan,
                    steps::{
                        both::BothStep,
                        coalesce::CoalesceStep,
                        drop::DropStep,
                        end_vertex_filter::EndVertexFilter,
                        has_id::HasIdStep,
                        has_label::HasLabelStep,
                        has_property::HasPropertyStep,
                        in_v_out_v::InVOutVStep,
                        limit::LimitStep,
                        other_v::OtherVStep,
                        property::PropertyStep,
                        r#where::WhereStep,
                        traits::{BufferedStep, CoreStep, StepRef},
                        vec_source::VecSourceStep,
                    },
                },
            },
            types::{gvalue::Primitive, Direction, EdgeKey, GValue},
        };
        use smallvec::smallvec;
        use std::rc::Rc;

        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // 1. DropStep without upstream
        {
            let mut step = DropStep::default();
            assert!(step.upper().is_none());
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 2. CoalesceStep multiple branches
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);

            let b1_src = BufferedStep::new(VecSourceStep::empty());
            let mut b1_limit = LimitStep::new(0);
            b1_limit.add_upper(b1_src.clone() as StepRef);
            let b1_limit_step = BufferedStep::new(b1_limit);
            let b1_plan = PhysicalPlan { source: b1_src, tail: b1_limit_step as StepRef };

            let b2_src = BufferedStep::new(VecSourceStep::empty());
            let b2_plan = PhysicalPlan { source: b2_src.clone(), tail: b2_src.clone() as StepRef };

            let mut step = CoalesceStep::new(smallvec![b1_plan, b2_plan]);
            step.add_upper(src.clone() as StepRef);

            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            assert_eq!(res[0].value, GValue::Scalar(Primitive::Int32(42)));
        }

        // 3. DropStep unexpected datatype
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let mut step = DropStep::default();
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).is_err());
        }

        // 4. InVOutVStep unexpected datatype
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let mut step = InVOutVStep::new(Direction::OUT);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 5. OtherVStep unexpected datatype
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let mut step = OtherVStep::default();
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 6. PropertyStep unexpected datatype (skipped)
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let mut step = PropertyStep::new(1u16, Primitive::Int32(1));
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 7. WhereStep non-matching sub-plan
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let b_src = BufferedStep::new(VecSourceStep::empty());
            let mut b_limit = LimitStep::new(0);
            b_limit.add_upper(b_src.clone() as StepRef);
            let b_limit_step = BufferedStep::new(b_limit);
            let b_plan = PhysicalPlan { source: b_src, tail: b_limit_step as StepRef };
            let mut step = WhereStep::new(b_plan);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 8. EndVertexFilterStep unexpected datatype
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let mut step = EndVertexFilter::new(smallvec![1]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).is_err());
        }

        // 9. EndVertexFilterStep non-matching edge
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Edge(EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: 3,
                secondary_id: 2,
                rank: 0,
            })))]);
            let mut step = EndVertexFilter::new(smallvec![99]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 10. HasIdStep non-matching ID
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);
            let mut step = HasIdStep::new(smallvec![99]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 11. HasLabelStep edge non-matching
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Edge(EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: 3,
                secondary_id: 2,
                rank: 0,
            }))),]);
            let mut step = HasLabelStep::new(smallvec![], smallvec![99]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 12. HasPropertyStep matching vertex and non-matching vertex
        {
            let age_id = graph.schema.read().unwrap().prop_key_id("age").unwrap();

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![
                Rc::new(Traverser::new(GValue::Vertex(1))), // marko (age 29)
                Rc::new(Traverser::new(GValue::Vertex(2))), // vadas (age 27)
            ]);

            let mut step = HasPropertyStep::new(age_id, Primitive::Int32(29));
            step.add_upper(src.clone() as StepRef);

            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            if let GValue::Vertex(v) = res[0].value {
                assert_eq!(v, 1);
            } else {
                panic!("expected vertex 1");
            }
            assert!(step.produce(&mut graph).unwrap().is_none());
        }

        // 13. BothStep non-vertex filtered out
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42)))),]);
            let mut step = BothStep::new(smallvec![], None, None, false);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
        }
    }

    #[test]
    fn test_additional_physical_steps_coverage() {
        use crate::{
            engine::{
                traverser::Traverser,
                volcano::{
                    builder::PhysicalPlan,
                    steps::{
                        both::BothStep,
                        coalesce::CoalesceStep,
                        dedup::DedupStep,
                        drop::DropStep,
                        e::EStep,
                        end_vertex_filter::EndVertexFilter,
                        fold::FoldStep,
                        has_id::HasIdStep,
                        has_label::HasLabelStep,
                        has_property::HasPropertyStep,
                        in_v_out_v::InVOutVStep,
                        limit::LimitStep,
                        other_v::OtherVStep,
                        path::PathStep,
                        property::PropertyStep,
                        r#where::WhereStep,
                        scalar_filter::ScalarFilterStep,
                        traits::{BufferedStep, CoreStep, StepRef},
                        union::UnionStep,
                        vec_source::VecSourceStep,
                    },
                },
            },
            types::{
                element::Property,
                gvalue::Primitive,
                keys::{CanonicalKey, EdgeKey},
                Direction, GValue,
            },
        };
        use smallvec::smallvec;
        use std::rc::Rc;

        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // 1. BothStep extra coverage
        {
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Vertex(1))),]);
            let mut step = BothStep::new(smallvec![1u16], None, None, true);
            step.add_upper(src.clone() as StepRef);

            let _ = step.produce(&mut graph);
            assert!(step.upper().is_some());
            step.reset();
        }

        // 2. CoalesceStep empty plans or exhausted plans
        {
            let mut step = CoalesceStep::new(smallvec![]);
            assert!(step.produce(&mut graph).unwrap().is_none());
            assert!(step.upper().is_none());
            step.reset();

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(42))))]);

            let b1_src = BufferedStep::new(VecSourceStep::empty());
            let mut b1_limit = LimitStep::new(0);
            b1_limit.add_upper(b1_src.clone() as StepRef);
            let b1_limit_step = BufferedStep::new(b1_limit);
            let b1_plan = PhysicalPlan { source: b1_src, tail: b1_limit_step as StepRef };

            let mut step = CoalesceStep::new(smallvec![b1_plan]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
            step.reset();
        }

        // 3. DedupStep edge deduplication and upper is none
        {
            let mut step = DedupStep::default();
            assert!(step.produce(&mut graph).unwrap().is_none());
            assert!(step.upper().is_none());
            step.reset();

            let src = BufferedStep::new(VecSourceStep::empty());
            let edge1 = GValue::Edge(EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: 2,
                secondary_id: 3,
                rank: 0,
            });
            let edge2 = GValue::Edge(EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: 2,
                secondary_id: 3,
                rank: 0,
            });
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(edge1)), Rc::new(Traverser::new(edge2)),]);
            let mut step = DedupStep::default();
            step.add_upper(src.clone() as StepRef);
            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            assert!(step.produce(&mut graph).unwrap().is_none());
            step.reset();
        }

        // 4. DropStep property dropping
        {
            let name_id = graph.schema.read().unwrap().prop_key_id("name").unwrap();
            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Property(Property {
                owner: CanonicalKey::Vertex(1),
                key: name_id,
                value: Primitive::String("marko".into()),
            })))]);
            let mut step = DropStep::default();
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
            assert!(graph.get_value(&CanonicalKey::Vertex(1), name_id).unwrap().is_none());

            let src2 = BufferedStep::new(VecSourceStep::empty());
            let mut step = DropStep::default();
            step.add_upper(src2.clone() as StepRef);
            step.reset();
            assert!(step.upper().is_some());
        }

        // 5. EStep panic/pagination/not found
        {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let dummy = BufferedStep::new(VecSourceStep::empty());
                let mut step = EStep::new(smallvec![]);
                step.add_upper(dummy as StepRef);
            }));
            assert!(res.is_err());

            let mut step = EStep::new(smallvec![]);
            let mut edges_count = 0;
            while let Some(res) = step.produce(&mut graph).unwrap() {
                edges_count += res.len();
            }
            assert!(edges_count > 0);
            assert!(step.produce(&mut graph).unwrap().is_none());
            step.reset();

            // Lookup a real edge: Marko (1) -> created -> Lop (3)
            let created_label_id = graph.schema.read().unwrap().edge_label_id("created").unwrap();
            let mut step = EStep::new(smallvec![EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: created_label_id,
                secondary_id: 3,
                rank: 0,
            }]);
            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            assert!(step.produce(&mut graph).unwrap().is_none());
            step.reset();
        }

        // 6. EndVertexFilter basic checks
        {
            let mut step = EndVertexFilter::default();
            assert!(step.produce(&mut graph).unwrap().is_none());
            step.reset();
            assert!(step.upper().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            let mut step = EndVertexFilter::default();
            step.add_upper(src.clone() as StepRef);
            step.reset();
            assert!(step.upper().is_some());
        }

        // 7. FoldStep early return on emitted
        {
            let mut step = FoldStep::default();
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(10))))]);
            let mut step = FoldStep::default();
            step.add_upper(src.clone() as StepRef);
            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_some());
        }

        // 8. HasIdStep non-vertex values and other coverage
        {
            let mut step = HasIdStep::new(smallvec![1]);
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![
                Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(10)))),
                Rc::new(Traverser::new(GValue::Vertex(1))),
            ]);
            let mut step = HasIdStep::new(smallvec![1]);
            step.add_upper(src.clone() as StepRef);
            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            assert_eq!(res[0].value, GValue::Vertex(1));

            step.reset();
            assert!(step.upper().is_some());
        }

        // 9. HasLabelStep non-vertex/non-edge coverage
        {
            let mut step = HasLabelStep::new(smallvec![], smallvec![]);
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(10)))),]);
            let mut step = HasLabelStep::new(smallvec![], smallvec![]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_some());
        }

        // 10. InVOutVStep IN and OUT direction
        {
            let mut step = InVOutVStep::new(Direction::IN);
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Edge(EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: 2,
                secondary_id: 3,
                rank: 0,
            })))]);
            let mut step = InVOutVStep::new(Direction::IN);
            step.add_upper(src.clone() as StepRef);
            let res = step.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res.len(), 1);
            assert_eq!(res[0].value, GValue::Vertex(3));

            let src_out = BufferedStep::new(VecSourceStep::empty());
            src_out.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Edge(EdgeKey {
                primary_id: 1,
                direction: Direction::OUT,
                label_id: 2,
                secondary_id: 3,
                rank: 0,
            })))]);
            let mut step_out = InVOutVStep::new(Direction::OUT);
            step_out.add_upper(src_out.clone() as StepRef);
            let res_out = step_out.produce(&mut graph).unwrap().unwrap();
            assert_eq!(res_out.len(), 1);
            assert_eq!(res_out[0].value, GValue::Vertex(1));

            step.reset();
            assert!(step.upper().is_some());
        }

        // 11. LimitStep upper is none
        {
            let mut step = LimitStep::new(2);
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            let mut step = LimitStep::new(2);
            step.add_upper(src.clone() as StepRef);
            step.reset();
            assert!(step.upper().is_some());
        }

        // 12. OtherVStep non-edge values filtering
        {
            let mut step = OtherVStep::default();
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(10)))),]);
            let mut step = OtherVStep::default();
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_some());
        }

        // 13. PathStep empty paths and emitted true
        {
            let mut step = PathStep::new();
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            let mut step = PathStep::new();
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_some());
        }

        // 14. PropertyStep non-vertex/non-edge elements
        {
            let mut step = PropertyStep::new(1, Primitive::Int32(1));
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner
                .borrow_mut()
                .core
                .inject(smallvec![Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(10)))),]);
            let mut step = PropertyStep::new(1, Primitive::Int32(1));
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_some());
        }

        // 15. ScalarFilterStep non-scalar/non-matching
        {
            let mut step = ScalarFilterStep::new(Primitive::Int32(10));
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![
                Rc::new(Traverser::new(GValue::Vertex(1))),
                Rc::new(Traverser::new(GValue::Scalar(Primitive::Int32(5)))),
            ]);
            let mut step = ScalarFilterStep::new(Primitive::Int32(10));
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());

            step.reset();
            assert!(step.upper().is_some());
        }

        // 16. UnionStep empty plans
        {
            let mut step = UnionStep::new(smallvec![]);
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            src.inner.borrow_mut().core.inject(smallvec![Rc::new(Traverser::new(GValue::Vertex(1))),]);
            let mut step = UnionStep::new(smallvec![]);
            step.add_upper(src.clone() as StepRef);
            assert!(step.produce(&mut graph).unwrap().is_none());

            let b_src = BufferedStep::new(VecSourceStep::empty());
            let b_plan = PhysicalPlan { source: b_src.clone(), tail: b_src.clone() as StepRef };
            let mut step = UnionStep::new(smallvec![b_plan]);
            step.add_upper(src.clone() as StepRef);
            step.reset();
            assert!(step.upper().is_some());
        }

        // 17. WhereStep upper is none
        {
            let b_src = BufferedStep::new(VecSourceStep::empty());
            let b_plan = PhysicalPlan { source: b_src.clone(), tail: b_src.clone() as StepRef };
            let mut step = WhereStep::new(b_plan);
            assert!(step.produce(&mut graph).unwrap().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            let b_src2 = BufferedStep::new(VecSourceStep::empty());
            let b_plan2 = PhysicalPlan { source: b_src2.clone(), tail: b_src2.clone() as StepRef };
            let mut step = WhereStep::new(b_plan2);
            step.add_upper(src.clone() as StepRef);
            step.reset();
            assert!(step.upper().is_some());
        }

        // 18. HasPropertyStep extra coverage
        {
            let age_id = graph.schema.read().unwrap().prop_key_id("age").unwrap();
            let mut step = HasPropertyStep::new(age_id, Primitive::Int32(29));
            assert!(step.produce(&mut graph).unwrap().is_none());
            assert!(step.upper().is_none());

            let src = BufferedStep::new(VecSourceStep::empty());
            step.add_upper(src.clone() as StepRef);
            assert!(step.upper().is_some());
            step.reset();
        }
    }
}
