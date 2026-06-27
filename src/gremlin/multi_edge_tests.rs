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
mod multi_edge_integration_test {
    use crate::{
        api::Graph,
        gremlin::{traversal::TraversalBuilder, value::Value},
        types::StoreError,
    };

    fn setup_multi_edge_graph(path: &std::path::Path) -> Graph {
        let options = crate::schema::GraphOptions {
            mode: crate::schema::SchemaMode::Auto,
            edge_mode: crate::schema::EdgeMode::Multi,
        };
        let graph = Graph::open_with_options(path, options).unwrap();

        // Pre-register labels
        {
            let schema_arc = graph.schema();
            let mut schema = schema_arc.write().unwrap();
            schema.register_vertex_label("person").unwrap();
            schema.register_edge_label("purchased").unwrap();
            schema.register_edge_label("reviewed").unwrap();
            schema.register_edge_label("knows").unwrap();
        }
        graph
    }

    #[test]
    fn test_multi_edge_mode_explicit_rank() {
        let dir = tempfile::tempdir().unwrap();
        let graph = setup_multi_edge_graph(dir.path());

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 10i64).next().unwrap();
        tx.g().addV("person").property("id", 20i64).next().unwrap();

        // 1. Add first edge with default rank 0
        tx.g().addE("purchased").from(10).to(20).property("since", "morning").next().unwrap();

        // 2. Add second edge with custom rank 1
        tx.g().addE("purchased").from(10).to(20).property("rank", 1i32).property("since", "evening").next().unwrap();

        // 3. Add third edge with custom rank 2
        tx.g().addE("purchased").from(10).to(20).property("rank", 2i64).property("since", "night").next().unwrap();

        // 4. Adding duplicate edge with rank 1 should fail
        let dup_res = tx.g().addE("purchased").from(10).to(20).property("rank", 1i32).next();
        assert!(matches!(dup_res, Err(StoreError::DuplicateEdge(_))));

        tx.commit().unwrap();

        // Query snapshot
        let mut read = graph.read();
        let edges = read.g().V([10]).outE(["purchased"]).to_list().unwrap();
        assert_eq!(edges.len(), 3);

        // Let's check ranks and property values
        let values = read.g().V([10]).outE(["purchased"]).values(["since"]).to_list().unwrap();
        assert_eq!(values.len(), 3);
        assert!(values.contains(&Value::String("morning".into())));
        assert!(values.contains(&Value::String("evening".into())));
        assert!(values.contains(&Value::String("night".into())));

        // 5. Verify rank property accesses return correct values (shortcut logic test)
        let ranks = read.g().V([10]).outE(["purchased"]).rank().to_list().unwrap();
        assert_eq!(ranks.len(), 3);
        assert!(ranks.contains(&Value::UInt16(0)));
        assert!(ranks.contains(&Value::UInt16(1)));
        assert!(ranks.contains(&Value::UInt16(2)));

        // 6. Verify filtering by rank works
        let evening_edge_count = read.g().V([10]).outE(["purchased"]).hasRank(1u16).count().next().unwrap().unwrap();
        assert_eq!(evening_edge_count, Value::Int64(1));

        let evening_since =
            read.g().V([10]).outE(["purchased"]).hasRank(1u16).values(["since"]).next().unwrap().unwrap();
        assert_eq!(evening_since, Value::String("evening".into()));
    }

    #[test]
    fn test_multi_edge_distinct_labels() {
        let dir = tempfile::tempdir().unwrap();
        let graph = setup_multi_edge_graph(dir.path());

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();

        // Add multiple ranks for "purchased"
        tx.g().addE("purchased").from(1).to(2).property("rank", 0i32).property("item", "book").next().unwrap();
        tx.g().addE("purchased").from(1).to(2).property("rank", 1i32).property("item", "pen").next().unwrap();

        // Add multiple ranks for "reviewed"
        tx.g().addE("reviewed").from(1).to(2).property("rank", 0i32).property("rating", 5i32).next().unwrap();
        tx.g().addE("reviewed").from(1).to(2).property("rank", 1i32).property("rating", 4i32).next().unwrap();

        tx.commit().unwrap();

        // Verify independent querying
        let mut read = graph.read();
        assert_eq!(read.g().V([1]).outE(["purchased"]).count().next().unwrap().unwrap(), Value::Int64(2));
        assert_eq!(read.g().V([1]).outE(["reviewed"]).count().next().unwrap().unwrap(), Value::Int64(2));

        let items = read.g().V([1]).outE(["purchased"]).values(["item"]).to_list().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.contains(&Value::String("book".into())));
        assert!(items.contains(&Value::String("pen".into())));
    }

    #[test]
    fn test_multi_edge_property_updates_on_specific_rank() {
        let dir = tempfile::tempdir().unwrap();
        let graph = setup_multi_edge_graph(dir.path());

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();

        // Insert rank 0 and rank 1
        tx.g().addE("knows").from(1).to(2).property("rank", 0i32).property("weight", 0.1f64).next().unwrap();
        tx.g().addE("knows").from(1).to(2).property("rank", 1i32).property("weight", 0.2f64).next().unwrap();

        // Update rank 1 property specifically
        tx.g().V([1]).outE(["knows"]).hasRank(1u16).property("weight", 0.9f64).next().unwrap();

        tx.commit().unwrap();

        // Verify updates
        let mut read = graph.read();
        let w0 = read.g().V([1]).outE(["knows"]).hasRank(0u16).values(["weight"]).next().unwrap().unwrap();
        let w1 = read.g().V([1]).outE(["knows"]).hasRank(1u16).values(["weight"]).next().unwrap().unwrap();

        assert_eq!(w0, Value::Float64(0.1));
        assert_eq!(w1, Value::Float64(0.9));
    }

    #[test]
    fn test_multi_edge_path_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let graph = setup_multi_edge_graph(dir.path());

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();

        tx.g().addE("knows").from(1).to(2).property("rank", 5i32).next().unwrap();
        tx.commit().unwrap();

        let mut read = graph.read();
        let paths = read.g().V([1]).outE(["knows"]).inV().path().to_list().unwrap();
        assert_eq!(paths.len(), 1);

        if let Value::Path(path) = &paths[0] {
            assert_eq!(path.objects.len(), 3); // [Vertex(1), Edge, Vertex(2)]
            if let Value::Edge(edge) = &path.objects[1] {
                assert_eq!(edge.out_v, 1);
                assert_eq!(edge.in_v, 2);
                assert_eq!(edge.rank, 5);
                assert_eq!(edge.label, "knows");
            } else {
                panic!("Expected Edge in path segment");
            }
        } else {
            panic!("Expected Path object");
        }
    }

    #[test]
    fn test_multi_edge_deletion_of_specific_rank() {
        let dir = tempfile::tempdir().unwrap();
        let graph = setup_multi_edge_graph(dir.path());

        let mut tx = graph.begin();
        tx.g().addV("person").property("id", 1i64).next().unwrap();
        tx.g().addV("person").property("id", 2i64).next().unwrap();

        // 3 parallel edges: rank 0, 1, 2
        tx.g().addE("knows").from(1).to(2).property("rank", 0i32).next().unwrap();
        tx.g().addE("knows").from(1).to(2).property("rank", 1i32).next().unwrap();
        tx.g().addE("knows").from(1).to(2).property("rank", 2i32).next().unwrap();

        tx.commit().unwrap();

        // Let's check initial degree (should be 3 out, 3 in)
        {
            let mut read = graph.read();
            let counts = read.g().V([1]).outE(["knows"]).count().next().unwrap().unwrap();
            assert_eq!(counts, Value::Int64(3));
        }

        // Delete rank 1 edge
        let mut tx2 = graph.begin();
        tx2.g().V([1]).outE(["knows"]).hasRank(1u16).drop().next().unwrap();
        tx2.commit().unwrap();

        // Verify remaining edges (rank 0 and 2 should remain, rank 1 should be gone)
        let mut read2 = graph.read();
        let remaining_ranks = read2.g().V([1]).outE(["knows"]).rank().to_list().unwrap();
        assert_eq!(remaining_ranks.len(), 2);
        assert!(remaining_ranks.contains(&Value::UInt16(0)));
        assert!(remaining_ranks.contains(&Value::UInt16(2)));
        assert!(!remaining_ranks.contains(&Value::UInt16(1)));

        // Verify degree count is decremented to 2
        let final_counts = read2.g().V([1]).outE(["knows"]).count().next().unwrap().unwrap();
        assert_eq!(final_counts, Value::Int64(2));
    }
}
