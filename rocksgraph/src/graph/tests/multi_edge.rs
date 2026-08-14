use crate::{
    api::Graph,
    schema::{EdgeMode, GraphOptions},
    TraversalBuilder, Value,
};
use tempfile::tempdir;

fn open_multi_edge_graph() -> (tempfile::TempDir, Graph) {
    let dir = tempdir().unwrap();
    let opts = GraphOptions::default().with_edge_mode(EdgeMode::Multi);
    let graph = Graph::open_with_options(dir.path(), opts).unwrap();
    (dir, graph)
}

#[test]
fn test_multi_edge_crud() {
    let (_dir, graph) = open_multi_edge_graph();

    // 1. Creation: Add vertices and parallel edges
    {
        let mut txn = graph.begin();
        txn.g().addV("Node").property("id", 10_i64).next().unwrap().unwrap();
        txn.g().addV("Node").property("id", 20_i64).next().unwrap().unwrap();

        // Edge 1: rank 0
        txn.g()
            .V([10_i64])
            .addE("LINK")
            .to(20_i64)
            .property("rank", 0_i64)
            .property("version", 1_i64)
            .next()
            .unwrap()
            .unwrap();

        // Edge 2: rank 1
        txn.g()
            .V([10_i64])
            .addE("LINK")
            .to(20_i64)
            .property("rank", 1_i64)
            .property("version", 2_i64)
            .next()
            .unwrap()
            .unwrap();

        // Edge 3: rank 2
        txn.g()
            .V([10_i64])
            .addE("LINK")
            .to(20_i64)
            .property("rank", 2_i64)
            .property("version", 3_i64)
            .next()
            .unwrap()
            .unwrap();

        txn.commit().unwrap();
    }

    // 2. Degree Counting & Traversal Validation
    {
        let mut snap = graph.read();

        let out_degree = snap.g().V([10_i64]).outE(["LINK"]).count().next().unwrap().unwrap();
        assert_eq!(out_degree, Value::Int64(3));

        let in_degree = snap.g().V([20_i64]).inE(["LINK"]).count().next().unwrap().unwrap();
        assert_eq!(in_degree, Value::Int64(3));

        let versions: Vec<Value> = snap.g().V([10_i64]).outE(["LINK"]).values(["version"]).to_list().unwrap();
        assert_eq!(versions.len(), 3);
        assert!(versions.contains(&Value::Int64(1)));
        assert!(versions.contains(&Value::Int64(2)));
        assert!(versions.contains(&Value::Int64(3)));
    }

    // 3. Property Isolation
    {
        let mut txn = graph.begin();
        // Update version on rank=1
        txn.g().V([10_i64]).outE(["LINK"]).hasRank(1_i64).property("version", 99_i64).next().unwrap();
        txn.commit().unwrap();
    }

    {
        let mut snap = graph.read();
        let versions: Vec<Value> = snap.g().V([10_i64]).outE(["LINK"]).values(["version"]).to_list().unwrap();
        assert!(versions.contains(&Value::Int64(1)));
        assert!(versions.contains(&Value::Int64(99))); // updated
        assert!(versions.contains(&Value::Int64(3)));
        assert!(!versions.contains(&Value::Int64(2)));
    }

    // 4. Deletion
    {
        let mut txn = graph.begin();
        // Drop rank 0
        let _ = txn.g().V([10_i64]).outE(["LINK"]).hasRank(0_i64).drop().next();
        txn.commit().unwrap();
    }

    {
        let mut snap = graph.read();
        // Check degree
        let out_degree = snap.g().V([10_i64]).outE(["LINK"]).count().next().unwrap().unwrap();
        assert_eq!(out_degree, Value::Int64(2));

        let versions: Vec<Value> = snap.g().V([10_i64]).outE(["LINK"]).values(["version"]).to_list().unwrap();
        assert_eq!(versions.len(), 2);
        assert!(!versions.contains(&Value::Int64(1))); // deleted
        assert!(versions.contains(&Value::Int64(99)));
        assert!(versions.contains(&Value::Int64(3)));
    }
}
