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
        graph::LogicalGraph,
        gremlin::traversal::{graphTraversalSource, __},
        store::{GraphStore, RocksStorage},
        types::{gvalue::Primitive, keys::LabelId, StoreError},
        GValue,
    };

    // Define LabelIds for common labels used across tests
    const PERSON_LABEL_ID: LabelId = 1;
    const SOFTWARE_LABEL_ID: LabelId = 2;
    const KNOWS_LABEL_ID: LabelId = 3;
    const CREATED_LABEL_ID: LabelId = 4;
    const FRIENDS_LABEL_ID: LabelId = 5;

    /// Creates a standard TinkerPop Modern Graph with predefined vertices and edges.
    /// This graph is used as a common baseline for various test cases.
    ///
    /// The graph includes: Marko, Vadas, Lop, Josh, Ripple, Peter and their relationships.
    pub fn create_tinkerpop_modern_graph(graph: &mut LogicalGraph<RocksStorage>) -> Result<(), StoreError> {
        // Add Vertices
        graphTraversalSource()
            .addV(PERSON_LABEL_ID)
            .property("id", 1i64)
            .property("name", "marko")
            .property("age", 29i32)
            .build(graph)?
            .next();
        graphTraversalSource()
            .addV(PERSON_LABEL_ID)
            .property("id", 2i64)
            .property("name", "vadas")
            .property("age", 27i32)
            .build(graph)?
            .next();
        graphTraversalSource()
            .addV(SOFTWARE_LABEL_ID)
            .property("id", 3i64)
            .property("name", "lop")
            .property("lang", "java")
            .build(graph)?
            .next();
        graphTraversalSource()
            .addV(PERSON_LABEL_ID)
            .property("id", 4i64)
            .property("name", "josh")
            .property("age", 32i32)
            .build(graph)?
            .next();
        graphTraversalSource()
            .addV(SOFTWARE_LABEL_ID)
            .property("id", 5i64)
            .property("name", "ripple")
            .property("lang", "java")
            .build(graph)?
            .next();
        graphTraversalSource()
            .addV(PERSON_LABEL_ID)
            .property("id", 6i64)
            .property("name", "peter")
            .property("age", 35i32)
            .build(graph)?
            .next();

        // Add Edges
        graphTraversalSource().addE(KNOWS_LABEL_ID).from(1).to(2).property("weight", 0.5f64).build(graph)?.next();
        graphTraversalSource().addE(KNOWS_LABEL_ID).from(1).to(4).property("weight", 1.0f64).build(graph)?.next();
        graphTraversalSource().addE(CREATED_LABEL_ID).from(1).to(3).property("weight", 0.4f64).build(graph)?.next();
        graphTraversalSource().addE(CREATED_LABEL_ID).from(4).to(5).property("weight", 1.0f64).build(graph)?.next();
        graphTraversalSource().addE(CREATED_LABEL_ID).from(4).to(3).property("weight", 0.4f64).build(graph)?.next();
        graphTraversalSource().addE(CREATED_LABEL_ID).from(6).to(3).property("weight", 0.2f64).build(graph)?.next();

        graph.commit()?;
        Ok(())
    }

    /// Helper to print the TinkerPop Modern Graph in ASCII art format.
    /*
       fn print_tinkerpop_modern_graph_ascii(graph: &mut LogicalGraph<RocksStorage>) {
           // This function is complex and might be slow due to multiple traversals.
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
               let Some(vertex) = graphTraversalSource().V([id]).values("name").build(graph)?.next() else {
                   unreachable!("expected vertex")
               };
               if let GValue::Vertex(vertex_key) = vertex {
                   let name = graph
                       .get_value(&CanonicalKey::Vertex(vertex_key), &SmolStr::new("name"))
                       .unwrap()
                       .unwrap_or(Primitive::String(SmolStr::new("N/A")));
                   let age = graph
                       .get_value(&CanonicalKey::Vertex(vertex_key), &SmolStr::new("age"))
                       .unwrap()
                       .unwrap_or(Primitive::Int32(0));
                   let Primitive::Int32(label) = graph.get_value(&CanonicalKey::Vertex(vertex_key), &LABEL).unwrap()
                   else {
                       unreachable("expected label type")
                   };
                   println!(
                       "  v[{}] ({}): name: {:?}, age: {:?}, label: {}",
                       vertex_key,
                       get_label_name(label.as_i32().unwrap() as u16),
                       name,
                       age,
                       label.as_i32().unwrap()
                   );
               }
           }

           println!("\nEdges:");
           // Iterate through all vertices to get their outgoing edges
           for src_id in 1..=6 {
               if let Ok(out_edges) = graph.get_edges(src_id, Direction::OUT, None, None, None) {
                   for edge_key in out_edges {
                       if let Ok(Some(ek)) = graph.get_edge(&edge_key) {
                           let label_name = get_label_name(ek.label_id);
                           let weight = graph
                               .get_value(&CanonicalKey::Edge(ek.canonical_edge_key()), &SmolStr::new("weight"))
                               .unwrap()
                               .unwrap_or(Primitive::Float64(0.0));
                           println!(
                               "  e[{:?}] ({:?}) --{} (weight: {:?})--> ({:?})",
                               ek.canonical_edge_key(),
                               ek.primary_id,
                               label_name,
                               weight,
                               ek.secondary_id
                           );
                       }
                   }
               }
           }
           println!("-------------------------------------------\n");
       }

    // --- Test Case to print the graph ---
    #[test]
    fn test_print_tinkerpop_modern_graph() {
        let store = open_rocks_store(None).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        let mut graph = create_tinkerpop_modern_graph(&mut g);
        print_tinkerpop_modern_graph_ascii(&mut graph);

        // This test primarily prints the graph, but we can add a simple assertion
        // to ensure the graph is not empty.
        let marko = graph.get_vertex(1).unwrap().unwrap();
        assert_eq!(marko, 1);
    }
    */

    #[test]
    fn test_tinkerpop_modern_vertex_edge_count() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        let mut t = graphTraversalSource().V([1, 2, 3, 4, 5, 6]).count().build(&mut g).unwrap();
        let count = t.next().unwrap().unwrap();
        assert_eq!(count, GValue::Scalar(Primitive::Int64(6)));

        let mut t = graphTraversalSource()
            .V([1, 2, 3, 4, 5, 6])
            .outE([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(6))); // 6 edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([1, 2, 3, 4, 5, 6])
            .inE([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(6))); // 6 edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([1, 2, 3, 4, 5, 6])
            .both([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(12))); // 6 edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .outE([CREATED_LABEL_ID, KNOWS_LABEL_ID])
            .r#where(__().otherV().hasLabel([SOFTWARE_LABEL_ID]))
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(4))); // 4 CREATED edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .bothE([KNOWS_LABEL_ID])
            .otherV()
            .hasLabel([PERSON_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(4))); // 2 KNOWS edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .bothE([KNOWS_LABEL_ID])
            .otherV()
            .hasLabel([PERSON_LABEL_ID])
            .dedup()
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(3))); // 2 KNOWS edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .outE([CREATED_LABEL_ID, KNOWS_LABEL_ID])
            .inV()
            .hasLabel([PERSON_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(2))); // 2 KNOWS edges in TinkerPop Modern Graph

        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .outE([CREATED_LABEL_ID, KNOWS_LABEL_ID])
            .r#where(__().otherV().hasLabel([PERSON_LABEL_ID]))
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(2))); // 2 KNOWS edges in TinkerPop Modern Graph
    }

    #[test]
    fn test_tinkerpop_modern_vertex_properties() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // vertex property values
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .values(["age", "name", "lang"])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(12))); // 6 edges in TinkerPop Modern Graph
    }

    #[test]
    fn test_tinkerpop_modern_has_label() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // person number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(4))); // 6 edges in TinkerPop Modern Graph

        // software number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([SOFTWARE_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(2))); // 6 edges in TinkerPop Modern Graph

        // CREATE edge number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .bothE([CREATED_LABEL_ID, KNOWS_LABEL_ID, FRIENDS_LABEL_ID])
            .hasLabel([CREATED_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(8))); // 8 edges in TinkerPop Modern Graph
    }
    #[test]
    fn test_tinkerpop_modern_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // CREATE edge number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .outE([CREATED_LABEL_ID])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(4))); // 4 edges in TinkerPop Modern Graph

        // software number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .out([CREATED_LABEL_ID])
            .dedup()
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(2))); // 2 software in TinkerPop Modern Graph

        // CREATE edge number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID, SOFTWARE_LABEL_ID])
            .bothE([CREATED_LABEL_ID, KNOWS_LABEL_ID, FRIENDS_LABEL_ID])
            .hasLabel([CREATED_LABEL_ID])
            .dedup()
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(4))); // 4 CREATE edges in TinkerPop Modern Graph
    }
    #[test]
    fn test_tinkerpop_modern_union() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // CREATE edge number
        let mut t = graphTraversalSource()
            .V([])
            .hasId([1, 2, 3, 4, 5, 6])
            .hasLabel([PERSON_LABEL_ID])
            .union([__().outE([CREATED_LABEL_ID]), __().outE([KNOWS_LABEL_ID])])
            .count()
            .build(&mut g)
            .unwrap();
        let ct = t.next().unwrap().unwrap();
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(6))); // 4 edges in TinkerPop Modern Graph
    }

    #[test]
    fn test_tinkerpop_modern_path_step() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // g.V(1).out().path()
        let mut t = graphTraversalSource()
            .V([1])
            .bothE([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .otherV()
            .path()
            .build(&mut g)
            .unwrap();

        let mut results = Vec::new();
        while let Some(Ok(val)) = t.next() {
            results.push(val);
        }

        assert_eq!(results.len(), 3); // Marko (1) has 3 outgoing edges
        for res in results {
            if let GValue::List(p) = res {
                assert_eq!(p.len(), 3); // Each path from out() has 3 elements (start vertex, edge, end vertex)
                assert_eq!(p[0], GValue::Vertex(1)); // First element is always the starting vertex
            } else {
                panic!("Expected path list, got {:?}", res);
            }
        }
    }

    #[test]
    fn test_tinkerpop_modern_to_list_step() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // g.V(1).out().values(["name"]).toList()
        let mut t = graphTraversalSource()
            .V([1])
            .out([KNOWS_LABEL_ID, CREATED_LABEL_ID, FRIENDS_LABEL_ID])
            .values(["name"])
            .toList()
            .build(&mut g)
            .unwrap();
        let result = t.next().unwrap().unwrap();

        if let GValue::List(l) = result {
            assert_eq!(l.len(), 3);
            let mut names: Vec<String> = l
                .iter()
                .map(|v| match v {
                    GValue::Scalar(Primitive::String(s)) => s.to_string(),
                    _ => panic!("Expected string scalar, got {:?}", v),
                })
                .collect();
            names.sort();
            assert_eq!(names, vec!["josh", "lop", "vadas"]);
        } else {
            panic!("Expected List GValue from toList(), got {:?}", result);
        }
    }

    #[test]
    fn test_tinkerpop_modern_coalesce_step() {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksStorage::open(dir.path()).unwrap();
        let mut g = LogicalGraph::<RocksStorage>::new(store.begin());
        create_tinkerpop_modern_graph(&mut g).unwrap();

        // g.V(1).coalesce(__().outE(CREATED), __().outE(KNOWS))
        let mut t = graphTraversalSource()
            .V([1])
            .coalesce([__().outE([CREATED_LABEL_ID]), __().outE([KNOWS_LABEL_ID])])
            .count()
            .build(&mut g)
            .unwrap();

        let ct = t.next().unwrap().unwrap();
        // Marko has both created and knows edges. coalesce picks the first branch that yields results.
        // Created should return 1 (Marko created Lop).
        assert_eq!(ct, GValue::Scalar(Primitive::Int64(1)));
    }
}
