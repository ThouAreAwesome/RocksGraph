// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

#[cfg(test)]
mod cases {
    use crate::{
        engine::{context::GraphCtx, volcano::builder::PhysicalPlanBuilder},
        graph::LogicalGraph,
        planner::logical_step::{
            AddEStep as LogicalAddEStep, AddVStep as LogicalAddVStep, BothEStep as LogicalBothEStep,
            BothStep as LogicalBothStep, CountStep as LogicalCountStep, HasLabelStep as LogicalHasLabelStep,
            HasPropertyStep as LogicalHasPropertyStep, InEStep as LogicalInEStep, InStep as LogicalInStep,
            InVStep as LogicalInVStep, LogicalPlan, LogicalStep, OtherVStep as LogicalOtherVStep,
            OutEStep as LogicalOutEStep, OutStep as LogicalOutStep, OutVStep as LogicalOutVStep,
            PropertyStep as LogicalPropertyStep, ScalarFilterStep as LogicalScalarFilterStep,
            UnionStep as LogicalUnionStep, VStep as LogicalVStep, ValuesStep as LogicalValuesStep,
            WhereStep as LogicalWhereStep,
        },
        store::{GraphStore, RocksStorage}, // Assuming RocksStorage is in src/store.rs
        types::{
            gvalue::Primitive,
            keys::{CanonicalEdgeKey, CanonicalKey, LabelId, VertexKey},
            GValue,
        },
    };
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
    fn open_rocks_store() -> (RocksStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        (store, dir)
    }

    fn create_logical_graph(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
        //let dir = tempfile::tempdir().unwrap();
        //let store = RocksStorage::open(dir.path()).unwrap();
        LogicalGraph::new(store.begin())
    }

    // Helper to create a TinkerPop Modern Graph
    /// Creates a standard TinkerPop Modern Graph with predefined vertices and edges.
    /// This graph is used as a common baseline for various test cases.
    ///
    /// The graph includes: Marko, Vadas, Lop, Josh, Ripple, Peter and their relationships.
    fn create_tinkerpop_modern_graph(store: &RocksStorage) -> LogicalGraph<RocksStorage> {
        let mut graph = create_logical_graph(store);

        // Define LabelIds for common labels
        // Add Vertices
        let v_marko = graph.add_vertex(1, PERSON_LABEL_ID).unwrap();
        graph
            .set_property(
                CanonicalKey::Vertex(v_marko.0),
                SmolStr::new("name"),
                Primitive::String(SmolStr::new("marko")),
            )
            .unwrap();
        graph.set_property(CanonicalKey::Vertex(v_marko.0), SmolStr::new("age"), Primitive::Int32(29)).unwrap();

        let v_vadas = graph.add_vertex(2, PERSON_LABEL_ID).unwrap();
        graph
            .set_property(
                CanonicalKey::Vertex(v_vadas.0),
                SmolStr::new("name"),
                Primitive::String(SmolStr::new("vadas")),
            )
            .unwrap();
        graph.set_property(CanonicalKey::Vertex(v_vadas.0), SmolStr::new("age"), Primitive::Int32(27)).unwrap();

        let v_lop = graph.add_vertex(3, SOFTWARE_LABEL_ID).unwrap();
        graph
            .set_property(CanonicalKey::Vertex(v_lop.0), SmolStr::new("name"), Primitive::String(SmolStr::new("lop")))
            .unwrap();
        graph
            .set_property(CanonicalKey::Vertex(v_lop.0), SmolStr::new("lang"), Primitive::String(SmolStr::new("java")))
            .unwrap();

        let v_josh = graph.add_vertex(4, PERSON_LABEL_ID).unwrap();
        graph
            .set_property(CanonicalKey::Vertex(v_josh.0), SmolStr::new("name"), Primitive::String(SmolStr::new("josh")))
            .unwrap();
        graph.set_property(CanonicalKey::Vertex(v_josh.0), SmolStr::new("age"), Primitive::Int32(32)).unwrap();

        let v_ripple = graph.add_vertex(5, SOFTWARE_LABEL_ID).unwrap();
        graph
            .set_property(
                CanonicalKey::Vertex(v_ripple.0),
                SmolStr::new("name"),
                Primitive::String(SmolStr::new("ripple")),
            )
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Vertex(v_ripple.0),
                SmolStr::new("lang"),
                Primitive::String(SmolStr::new("java")),
            )
            .unwrap();

        let v_peter = graph.add_vertex(6, PERSON_LABEL_ID).unwrap();
        graph
            .set_property(
                CanonicalKey::Vertex(v_peter.0),
                SmolStr::new("name"),
                Primitive::String(SmolStr::new("peter")),
            )
            .unwrap();
        graph.set_property(CanonicalKey::Vertex(v_peter.0), SmolStr::new("age"), Primitive::Int32(35)).unwrap();
        // Add Edges
        let e1 = graph
            .add_edge(CanonicalEdgeKey { src_id: v_marko.0, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: v_vadas.0 })
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Edge(e1.0.canonical_edge_key()),
                SmolStr::new("weight"),
                Primitive::Float64(0.5),
            )
            .unwrap();

        let e2 = graph
            .add_edge(CanonicalEdgeKey { src_id: v_marko.0, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: v_josh.0 })
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Edge(e2.0.canonical_edge_key()),
                SmolStr::new("weight"),
                Primitive::Float64(1.0),
            )
            .unwrap();

        let e3 = graph
            .add_edge(CanonicalEdgeKey { src_id: v_marko.0, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop.0 })
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Edge(e3.0.canonical_edge_key()),
                SmolStr::new("weight"),
                Primitive::Float64(0.4),
            )
            .unwrap();

        let e4 = graph
            .add_edge(CanonicalEdgeKey { src_id: v_josh.0, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_ripple.0 })
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Edge(e4.0.canonical_edge_key()),
                SmolStr::new("weight"),
                Primitive::Float64(1.0),
            )
            .unwrap();

        let e5 = graph
            .add_edge(CanonicalEdgeKey { src_id: v_josh.0, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop.0 })
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Edge(e5.0.canonical_edge_key()),
                SmolStr::new("weight"),
                Primitive::Float64(0.4),
            )
            .unwrap();

        let e6 = graph
            .add_edge(CanonicalEdgeKey { src_id: v_peter.0, label_id: CREATED_LABEL_ID, rank: 0, dst_id: v_lop.0 })
            .unwrap();
        graph
            .set_property(
                CanonicalKey::Edge(e6.0.canonical_edge_key()),
                SmolStr::new("weight"),
                Primitive::Float64(0.2),
            )
            .unwrap();

        graph.commit().unwrap(); // Commit all initial graph data

        // --- Verification after commit ---
        let mut verification_graph = create_logical_graph(store);

        // Verify Vertices
        let _marko_v = verification_graph.get_vertex(v_marko.0).unwrap().unwrap();
        assert_eq!(_marko_v.label_id, PERSON_LABEL_ID);
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_marko.0), &SmolStr::new("name")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("marko"))
        );
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_marko.0), &SmolStr::new("age")).unwrap().unwrap(),
            Primitive::Int32(29)
        );

        let _vadas_v = verification_graph.get_vertex(v_vadas.0).unwrap().unwrap();
        assert_eq!(_vadas_v.label_id, PERSON_LABEL_ID);
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_vadas.0), &SmolStr::new("name")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("vadas"))
        );
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_vadas.0), &SmolStr::new("age")).unwrap().unwrap(),
            Primitive::Int32(27)
        );

        let _lop_v = verification_graph.get_vertex(v_lop.0).unwrap().unwrap();
        assert_eq!(_lop_v.label_id, SOFTWARE_LABEL_ID);
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_lop.0), &SmolStr::new("name")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("lop"))
        );
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_lop.0), &SmolStr::new("lang")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("java"))
        );

        let _josh_v = verification_graph.get_vertex(v_josh.0).unwrap().unwrap();
        assert_eq!(_josh_v.label_id, PERSON_LABEL_ID);
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_josh.0), &SmolStr::new("name")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("josh"))
        );
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_josh.0), &SmolStr::new("age")).unwrap().unwrap(),
            Primitive::Int32(32)
        );

        let _ripple_v = verification_graph.get_vertex(v_ripple.0).unwrap().unwrap();
        assert_eq!(_ripple_v.label_id, SOFTWARE_LABEL_ID);
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_ripple.0), &SmolStr::new("name")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("ripple"))
        );
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_ripple.0), &SmolStr::new("lang")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("java"))
        );

        let _peter_v = verification_graph.get_vertex(v_peter.0).unwrap().unwrap();
        assert_eq!(_peter_v.label_id, PERSON_LABEL_ID);
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_peter.0), &SmolStr::new("name")).unwrap().unwrap(),
            Primitive::String(SmolStr::new("peter"))
        );
        assert_eq!(
            verification_graph.get_property(CanonicalKey::Vertex(v_peter.0), &SmolStr::new("age")).unwrap().unwrap(),
            Primitive::Int32(35)
        );

        // Verify Edges and their properties
        let _e1_edge = verification_graph.get_edge(e1.0.canonical_edge_key()).unwrap().unwrap();
        assert_eq!(_e1_edge.label_id, KNOWS_LABEL_ID);
        assert_eq!(_e1_edge.src_id, v_marko.0);
        assert_eq!(_e1_edge.dst_id, v_vadas.0);
        assert_eq!(
            verification_graph
                .get_property(CanonicalKey::Edge(e1.0.canonical_edge_key()), &SmolStr::new("weight"))
                .unwrap()
                .unwrap(),
            Primitive::Float64(0.5)
        );

        let _e2_edge = verification_graph.get_edge(e2.0.canonical_edge_key()).unwrap().unwrap();
        assert_eq!(_e2_edge.label_id, KNOWS_LABEL_ID);
        assert_eq!(_e2_edge.src_id, v_marko.0);
        assert_eq!(_e2_edge.dst_id, v_josh.0);
        assert_eq!(
            verification_graph
                .get_property(CanonicalKey::Edge(e2.0.canonical_edge_key()), &SmolStr::new("weight"))
                .unwrap()
                .unwrap(),
            Primitive::Float64(1.0)
        );

        let _e3_edge = verification_graph.get_edge(e3.0.canonical_edge_key()).unwrap().unwrap();
        assert_eq!(_e3_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e3_edge.src_id, v_marko.0);
        assert_eq!(_e3_edge.dst_id, v_lop.0);
        assert_eq!(
            verification_graph
                .get_property(CanonicalKey::Edge(e3.0.canonical_edge_key()), &SmolStr::new("weight"))
                .unwrap()
                .unwrap(),
            Primitive::Float64(0.4)
        );

        let _e4_edge = verification_graph.get_edge(e4.0.canonical_edge_key()).unwrap().unwrap();
        assert_eq!(_e4_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e4_edge.src_id, v_josh.0);
        assert_eq!(_e4_edge.dst_id, v_ripple.0);
        assert_eq!(
            verification_graph
                .get_property(CanonicalKey::Edge(e4.0.canonical_edge_key()), &SmolStr::new("weight"))
                .unwrap()
                .unwrap(),
            Primitive::Float64(1.0)
        );

        let _e5_edge = verification_graph.get_edge(e5.0.canonical_edge_key()).unwrap().unwrap();
        assert_eq!(_e5_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e5_edge.src_id, v_josh.0);
        assert_eq!(_e5_edge.dst_id, v_lop.0);
        assert_eq!(
            verification_graph
                .get_property(CanonicalKey::Edge(e5.0.canonical_edge_key()), &SmolStr::new("weight"))
                .unwrap()
                .unwrap(),
            Primitive::Float64(0.4)
        );

        let _e6_edge = verification_graph.get_edge(e6.0.canonical_edge_key()).unwrap().unwrap();
        assert_eq!(_e6_edge.label_id, CREATED_LABEL_ID);
        assert_eq!(_e6_edge.src_id, v_peter.0);
        assert_eq!(_e6_edge.dst_id, v_lop.0);
        assert_eq!(
            verification_graph
                .get_property(CanonicalKey::Edge(e6.0.canonical_edge_key()), &SmolStr::new("weight"))
                .unwrap()
                .unwrap(),
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
            if let Ok(Some(vertex)) = graph.get_vertex(id) {
                let label_name = get_label_name(vertex.label_id);
                print!("  ({}: {})", vertex.id, label_name);
                let props_guard = vertex.props.read().unwrap();
                if !props_guard.is_empty() {
                    print!(" {{");
                    let mut first = true;
                    for prop in props_guard.iter() {
                        if !first {
                            print!(", ");
                        }
                        print!("{}: {:?}", prop.key, prop.value);
                        first = false;
                    }
                    print!("}}");
                }
                println!();
            }
        }

        println!("\nEdges:");
        // Iterate through all vertices to get their outgoing edges
        for src_id in 1..=6 {
            if let Ok(out_edges) = graph.get_out_edges(src_id, None) {
                for edge_key in out_edges {
                    if let Ok(Some(edge)) = graph.get_edge(edge_key.canonical_edge_key()) {
                        let label_name = get_label_name(edge.label_id);
                        print!("  ({:?}) --{}--> ({:?})", edge.src_id, label_name, edge.dst_id);
                        let props_guard = edge.props.read().unwrap();
                        if !props_guard.is_empty() {
                            print!(" {{");
                            let mut first = true;
                            for prop in props_guard.iter() {
                                if !first {
                                    print!(", ");
                                }
                                print!("{}: {:?}", prop.key, prop.value);
                                first = false;
                            }
                            print!("}}");
                        }
                        println!();
                    }
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
        assert_eq!(marko.id, 1);
        assert_eq!(marko.label_id, PERSON_LABEL_ID);
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
                label_id: PERSON_LABEL_ID,
                vertex_id: test_vertex_id,
                properties,
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();

        if let GValue::Vertex(v_key) = &result.value {
            assert_eq!(*v_key, test_vertex_id); // Check the returned VertexKey
            let added_vertex = graph.get_vertex(*v_key).unwrap().unwrap(); // Fetch the actual vertex
            assert_eq!(added_vertex.label_id, PERSON_LABEL_ID);
            assert_eq!(
                graph.get_property(CanonicalKey::Vertex(*v_key), &SmolStr::new("name")).unwrap().unwrap(),
                Primitive::String(SmolStr::new("marko"))
            );
            assert_eq!(
                graph.get_property(CanonicalKey::Vertex(*v_key), &SmolStr::new("age")).unwrap().unwrap(),
                Primitive::Int32(29)
            );
            assert_eq!(
                graph.get_property(CanonicalKey::Vertex(*v_key), &SmolStr::new("name")).unwrap().unwrap(),
                Primitive::String(SmolStr::new("marko"))
            );
        } else {
            panic!("Expected a Vertex GValue");
        }
        assert!(physical_plan.next(&mut graph).is_none()); // Should only emit once
    }

    // --- Test Cases for AddEStep ---
    #[test]
    fn test_add_e_step_to_tinkerpop_modern_graph() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        // Find existing vertices to connect
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id; // Assuming ID 1 for marko
        let vadas_id = graph.get_vertex(2).unwrap().unwrap().id; // Assuming ID 2 for vadas

        let mut properties = HashMap::new();
        properties.insert(SmolStr::new("since"), Primitive::Int32(2020));

        let logical_plan = LogicalPlan {
            steps: vec![LogicalStep::AddE(LogicalAddEStep {
                label_id: FRIENDS_LABEL_ID,
                out_v_id: marko_id,
                in_v_id: vadas_id,
                properties,
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();
        if let GValue::Edge(e_key) = &result.value {
            let added_edge = graph.get_edge(e_key.canonical_edge_key()).unwrap().unwrap(); // Fetch the actual edge
            assert_eq!(added_edge.label_id, FRIENDS_LABEL_ID);
            assert_eq!(added_edge.src_id, marko_id);
            assert_eq!(added_edge.dst_id, vadas_id);
            assert_eq!(
                graph
                    .get_property(CanonicalKey::Edge(e_key.canonical_edge_key()), &SmolStr::new("since"))
                    .unwrap()
                    .unwrap(),
                Primitive::Int32(2020)
            );
        } else {
            panic!("Expected an Edge GValue");
        }
        assert!(physical_plan.next(&mut graph).is_none()); // Should only emit once
    }

    // --- Test Cases for PropertyStep ---
    #[test]
    fn test_property_step_update_vertex_in_tinkerpop_modern_graph() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id; // Assuming ID 1 for marko
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Property(LogicalPropertyStep {
                    prop_key: SmolStr::new("age"),
                    prop_value: Primitive::Int32(30),
                }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();

        if let GValue::Vertex(v_key) = &result.value {
            let updated_vertex = graph.get_vertex(*v_key).unwrap().unwrap();
            assert_eq!(updated_vertex.id, marko_id);
            assert_eq!(updated_vertex.label_id, PERSON_LABEL_ID); // Assuming label_id 1 for person
            assert_eq!(
                graph.get_property(CanonicalKey::Vertex(*v_key), &SmolStr::new("name")).unwrap().unwrap(),
                Primitive::String(SmolStr::new("marko"))
            );
            assert_eq!(
                graph.get_property(CanonicalKey::Vertex(*v_key), &SmolStr::new("age")).unwrap().unwrap(),
                Primitive::Int32(30)
            ); // Updated
        } else {
            panic!("Expected a Vertex GValue");
        }
        assert!(physical_plan.next(&mut graph).is_none());
    }

    #[test]
    fn test_property_step_add_new_property_to_edge() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;
        let josh_id = graph.get_vertex(4).unwrap().unwrap().id;
        let knows_edge_key = CanonicalEdgeKey { src_id: marko_id, label_id: KNOWS_LABEL_ID, rank: 0, dst_id: josh_id }; // LabelId 3 for "knows"
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![knows_edge_key.label_id] }),
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
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();

        if let GValue::Edge(e_key) = &result.value {
            let updated_edge = graph.get_edge(e_key.canonical_edge_key()).unwrap().unwrap();
            assert_eq!(updated_edge.canonical_key(), knows_edge_key);
            assert_eq!(
                graph
                    .get_property(CanonicalKey::Edge(e_key.canonical_edge_key()), &SmolStr::new("duration"))
                    .unwrap()
                    .unwrap(),
                Primitive::Int32(12)
            ); // New property
        } else {
            panic!("Expected an Edge GValue");
        }
        assert_eq!(
            graph.get_property(CanonicalKey::Edge(knows_edge_key), &SmolStr::new("duration")).unwrap().unwrap(),
            Primitive::Int32(12)
        );
    }

    // --- Test Cases for HasPropertyStep ---
    #[test]
    fn test_has_property_step_match_vertex() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;
        let vadas_id = graph.get_vertex(2).unwrap().unwrap().id; // Assuming ID 2 for vadas
        let logical_plan = LogicalPlan {
            steps: vec![
                // Corrected to use PERSON_LABEL_ID
                LogicalStep::V(LogicalVStep { ids: vec![marko_id, vadas_id] }),
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("age"),
                    value: Primitive::Int32(29),
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();
        if let GValue::Vertex(v_key) = &result.value {
            assert_eq!(*v_key, marko_id);
        } else {
            panic!("Expected Marko");
        }
        assert!(physical_plan.next(&mut graph).is_none());
    }

    #[test]
    fn test_has_property_step_match_edge() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;
        let vadas_id = graph.get_vertex(2).unwrap().unwrap().id;
        let _knows_edge_key = CanonicalEdgeKey { src_id: marko_id, label_id: 3, rank: 0, dst_id: vadas_id };
        let josh_id = graph.get_vertex(4).unwrap().unwrap().id;
        let _lop_id = graph.get_vertex(3).unwrap().unwrap().id;
        let ripple_id = graph.get_vertex(5).unwrap().unwrap().id;
        let created_edge_key =
            CanonicalEdgeKey { src_id: josh_id, label_id: CREATED_LABEL_ID, rank: 0, dst_id: ripple_id }; // Josh created Ripple has weight 1.0
                                                                                                          // Start from Marko and Josh, get their outgoing edges with label CREATED_LABEL_ID, and filter by weight = 1.0
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id, josh_id] }), // Start from Marko and Josh
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![CREATED_LABEL_ID] }), // Get all outgoing edges
                LogicalStep::HasProperty(LogicalHasPropertyStep {
                    key: SmolStr::new("weight"),
                    value: Primitive::Float64(1.0),
                }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();

        if let GValue::Edge(e_key) = &result.value {
            assert_eq!(e_key.canonical_edge_key(), created_edge_key); // Josh created Ripple with weight 1.0
        } else {
            panic!("Expected created_edge_arc");
        }
        assert!(physical_plan.next(&mut graph).is_none());

        // Start from Marko and Josh, get their outgoing edges without label filter, but filter by weight = 0.4 (should
        // match Marko->Lop and Josh->Lop)
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id, josh_id] }), // Start from Marko and Josh
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![] }),      // Get all outgoing edges
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
        let physical_plan = builder.build(&logical_plan);
        let result = physical_plan.next(&mut graph).unwrap();

        if let GValue::Edge(e_key) = &result.value {
            assert_eq!(e_key.canonical_edge_key(), expected_edge_keys[0]); // Josh created Ripple with weight 1.0
        } else {
            panic!("Expected created_edge_arc");
        }

        let result = physical_plan.next(&mut graph).unwrap();
        if let GValue::Edge(e_key) = &result.value {
            assert_eq!(e_key.canonical_edge_key(), expected_edge_keys[1]); // Josh created Ripple with weight 1.0
        } else {
            panic!("Expected created_edge_arc");
        }
        assert!(physical_plan.next(&mut graph).is_none());
    }

    #[test]
    fn test_union_out_e_count_in_e_count() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);

        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        // Sub-plan 1: outE().count()
        let out_e_count_sub_plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![] }),
                LogicalStep::Count(LogicalCountStep {}),
            ],
        };

        // Sub-plan 2: inE().count()
        let in_e_count_sub_plan = LogicalPlan {
            steps: vec![
                LogicalStep::InE(LogicalInEStep { label_ids: vec![] }),
                LogicalStep::Count(LogicalCountStep {}),
            ],
        };

        // Main plan: V(marko_id).union(outE().count(), inE().count())
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Union(LogicalUnionStep { plans: vec![out_e_count_sub_plan, in_e_count_sub_plan] }),
            ],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);

        let mut results = Vec::new();
        while let Some(traverser) = physical_plan.next(&mut graph) {
            results.push(traverser.value);
        }

        // Marko has 3 outgoing edges and 0 incoming edges in the TinkerPop Modern Graph
        assert_eq!(results.len(), 2);
        assert!(results.contains(&GValue::Scalar(Primitive::Int32(3))));
        assert!(results.contains(&GValue::Scalar(Primitive::Int32(0))));
    }

    #[test]
    fn test_out_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { label_ids: vec![] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
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
        let lop_id = graph.get_vertex(3).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![lop_id] }),
                LogicalStep::In(LogicalInStep { label_ids: vec![] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
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
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        // V(1).outE().inV() equivalent to V(1).out()
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![] }),
                LogicalStep::InV(LogicalInVStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(2)));
        assert!(results.contains(&GValue::Vertex(3)));
        assert!(results.contains(&GValue::Vertex(4)));

        // V(1).outE().outV() should return Marko 3 times
        let logical_plan2 = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![] }),
                LogicalStep::OutV(LogicalOutVStep {}),
            ],
        };
        let mut builder2: PhysicalPlanBuilder = Default::default();
        let physical_plan2 = builder2.build(&logical_plan2);
        let mut results2 = Vec::new();
        while let Some(t) = physical_plan2.next(&mut graph) {
            results2.push(t.value);
        }
        assert_eq!(results2.len(), 3);
        assert!(results2.iter().all(|v| v == &GValue::Vertex(1)));
    }

    #[test]
    fn test_both_and_both_e_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let josh_id = graph.get_vertex(4).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![josh_id] }),
                LogicalStep::Both(LogicalBothStep { label_ids: vec![] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(1)));
        assert!(results.contains(&GValue::Vertex(3)));
        assert!(results.contains(&GValue::Vertex(5)));

        let logical_plan_e = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![josh_id] }),
                LogicalStep::BothE(LogicalBothEStep { label_ids: vec![] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan_e = builder.build(&logical_plan_e);
        let mut results_e = Vec::new();
        while let Some(t) = physical_plan_e.next(&mut graph) {
            results_e.push(t.value);
        }
        assert_eq!(results_e.len(), 3);
    }

    #[test]
    fn test_has_label_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { label_ids: vec![] }),
                LogicalStep::HasLabel(LogicalHasLabelStep { label_ids: vec![SOFTWARE_LABEL_ID] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], GValue::Vertex(3)); // Lop
    }

    #[test]
    fn test_other_v_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::OutE(LogicalOutEStep { label_ids: vec![] }),
                LogicalStep::OtherV(LogicalOtherVStep {}),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
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
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Values(LogicalValuesStep {
                    property_keys: vec![SmolStr::new("name"), SmolStr::new("age")],
                }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
        }
        assert_eq!(results.len(), 2);
        assert!(results.contains(&GValue::Scalar(Primitive::String(SmolStr::new("marko")))));
        assert!(results.contains(&GValue::Scalar(Primitive::Int32(29))));
    }

    #[test]
    fn test_scalar_filter_step() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Values(LogicalValuesStep { property_keys: vec![SmolStr::new("age")] }),
                LogicalStep::ScalarFilter(LogicalScalarFilterStep { value: Primitive::Int32(29) }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
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
                LogicalStep::Out(LogicalOutStep { label_ids: vec![] }),
                LogicalStep::HasLabel(LogicalHasLabelStep { label_ids: vec![SOFTWARE_LABEL_ID] }),
            ],
        };
        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![1, 2, 3, 4, 5, 6] }),
                LogicalStep::Where(LogicalWhereStep { plan: sub_plan }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
        }
        assert_eq!(results.len(), 3);
        assert!(results.contains(&GValue::Vertex(1)));
        assert!(results.contains(&GValue::Vertex(4)));
        assert!(results.contains(&GValue::Vertex(6)));
    }

    #[test]
    fn test_out_multiple_labels() {
        let (store, _dir) = open_rocks_store();
        let mut graph = create_tinkerpop_modern_graph(&store);
        let marko_id = graph.get_vertex(1).unwrap().unwrap().id;

        let logical_plan = LogicalPlan {
            steps: vec![
                LogicalStep::V(LogicalVStep { ids: vec![marko_id] }),
                LogicalStep::Out(LogicalOutStep { label_ids: vec![KNOWS_LABEL_ID, CREATED_LABEL_ID] }),
            ],
        };
        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&logical_plan);
        let mut results = Vec::new();
        while let Some(t) = physical_plan.next(&mut graph) {
            results.push(t.value);
        }
        assert_eq!(results.len(), 3); // 2 knows + 1 created
    }
}
