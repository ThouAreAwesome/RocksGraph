// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{
    gremlin::traversal::TraversalBuilder,
    schema::DataType,
    types::error::StoreError,
    vector::{AnnAlgorithm, DistanceMetric, Quantization, VectorEntityType, VectorIndexConfig},
    Graph, Value,
};

fn declare_index(g: &Graph, prop: &str, dim: usize, metric: DistanceMetric) {
    let mut sess = g.open_schema();
    sess.add_vector_index(VectorIndexConfig {
        property: prop.into(),
        entity_type: VectorEntityType::Vertex,
        dimension: dim,
        metric,
        algorithm: AnnAlgorithm::Hnsw(Default::default()),
        quantization: Quantization::F32,
    });
    sess.commit().unwrap();
}

fn ids_from_results(results: Vec<Value>) -> Vec<i64> {
    results
        .into_iter()
        .filter_map(|v| match v {
            Value::Int64(id) => Some(id),
            _ => None,
        })
        .collect()
}

#[test]
fn rebuild_vector_index_empty_db() {
    use crate::schema::{AnnAlgorithm, DistanceMetric, Quantization, VectorEntityType, VectorIndexConfig};
    let dir = tempfile::tempdir().unwrap();
    let g = crate::Graph::open(dir.path()).unwrap();
    let mut sess = g.open_schema();
    sess.add_vector_index(VectorIndexConfig {
        property: "emb".into(),
        entity_type: VectorEntityType::Vertex,
        dimension: 4,
        metric: DistanceMetric::Cosine,
        algorithm: AnnAlgorithm::Hnsw(Default::default()),
        quantization: Quantization::F32,
    });
    sess.commit().unwrap();
    g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();
    g.close().unwrap();
}

#[test]
fn rebuild_vector_index_roundtrip() {
    use crate::{
        schema::{AnnAlgorithm, DistanceMetric, HnswConfig, Quantization, VectorEntityType, VectorIndexConfig},
        Graph, TraversalBuilder,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // 1. Declare vector index + property key via SchemaSession.
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(HnswConfig::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();
        g.close().unwrap();
    }

    // 2. Insert 3 vertices with FloatVector embeddings via public insert path.
    {
        let g = Graph::open(path).unwrap();
        let mut txn = g.begin();
        txn.g()
            .addV("test")
            .property("id", 1i64)
            .property("emb", crate::Value::FloatVector(vec![1.0, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.g()
            .addV("test")
            .property("id", 2i64)
            .property("emb", crate::Value::FloatVector(vec![0.0, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.g()
            .addV("test")
            .property("id", 3i64)
            .property("emb", crate::Value::FloatVector(vec![0.0, 0.0, 1.0, 0.0]))
            .next()
            .unwrap();
        txn.commit().unwrap();
        g.close().unwrap();
    }

    // 3. Re-open, rebuild, and verify search correctness.
    {
        let g = Graph::open(path).unwrap();
        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

        let mut snap = g.read();
        let results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 3)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                crate::Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();

        assert_eq!(results.len(), 3, "should return 3 results");
        assert_eq!(results[0], 1, "exact match [1,0,0,0] should be first");
        g.close().unwrap();
    }
}

#[test]
fn test_schema_vector_index_validation_and_drop() {
    use crate::{
        vector::{AnnAlgorithm, DistanceMetric, Quantization, VectorEntityType, VectorIndexConfig},
        Graph, StoreError,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // 1. Edge entity type is rejected
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "edge_emb".into(),
            entity_type: VectorEntityType::Edge,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        let err = sess.commit().unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedOperation(_)));

        let mut sess_drop = g.open_schema();
        sess_drop.drop_vector_index(VectorEntityType::Edge, "edge_emb");
        let err_drop = sess_drop.commit().unwrap_err();
        assert!(matches!(err_drop, StoreError::UnsupportedOperation(_)));
        g.close().unwrap();
    }

    // 2. Declare vector index and check duplicate conflict
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();

        let mut sess_conflict = g.open_schema();
        sess_conflict.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 8,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        let err = sess_conflict.commit().unwrap_err();
        assert!(matches!(err, StoreError::SchemaConflict(_)));
        g.close().unwrap();
    }

    // 3. Drop vector index and verify removal
    {
        let g = Graph::open(path).unwrap();
        assert!(g.vector_indexes.read().contains_key(&(VectorEntityType::Vertex, "emb".into())));

        let mut sess = g.open_schema();
        sess.drop_vector_index(VectorEntityType::Vertex, "emb");
        // Dropping non-existent is safe / idempotent
        sess.drop_vector_index(VectorEntityType::Vertex, "nonexistent");
        sess.commit().unwrap();
        g.close().unwrap();

        // Reopening should no longer load the dropped index from CF_SCHEMA
        let g_reopened = Graph::open(path).unwrap();
        assert!(!g_reopened.vector_indexes.read().contains_key(&(VectorEntityType::Vertex, "emb".into())));
        g_reopened.close().unwrap();
    }
}

#[test]
fn test_nearest_upstream_filter_and_missing_props() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // 1. Declare schema
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();
        g.close().unwrap();
    }

    // 2. Insert mixed vertices (some match filter, some don't, some lack embedding)
    {
        let g = Graph::open(path).unwrap();
        let mut txn = g.begin();
        // v1: tech, emb [1, 0, 0, 0] (exact match to query)
        txn.g()
            .addV("node")
            .property("id", 1i64)
            .property("category", "tech")
            .property("emb", Value::FloatVector(vec![1.0, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        // v2: finance, emb [0.99, 0.01, 0.0, 0.0] (high similarity, but different category)
        txn.g()
            .addV("node")
            .property("id", 2i64)
            .property("category", "finance")
            .property("emb", Value::FloatVector(vec![0.99, 0.01, 0.0, 0.0]))
            .next()
            .unwrap();
        // v3: tech, emb [0.0, 1.0, 0.0, 0.0] (orthogonal)
        txn.g()
            .addV("node")
            .property("id", 3i64)
            .property("category", "tech")
            .property("emb", Value::FloatVector(vec![0.0, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        // v4: tech, no embedding property at all
        txn.g().addV("node").property("id", 4i64).property("category", "tech").next().unwrap();
        txn.commit().unwrap();

        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

        let mut snap = g.read();

        // Mid-stream .nearest() after .has() is rejected as invalid pattern
        let err = snap
            .g()
            .V([])
            .has("category", "tech")
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap_err();
        assert!(matches!(err, StoreError::UnsupportedOperation(_)));

        // Mid-stream .nearest() after bounded V([1, 2]) is also rejected
        let err2 = snap.g().V([1, 2]).nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5).id().to_list().unwrap_err();
        assert!(matches!(err2, StoreError::UnsupportedOperation(_)));

        // Downstream filter after .nearest() works properly
        let filtered_results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .has("category", "tech")
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();

        // v2 (finance) is filtered out, v4 (no emb) was never emitted by nearest -> only v1 and v3 remain
        assert_eq!(filtered_results, vec![1, 3]);

        // Unfiltered query returns all 3 vector-bearing vertices in order: v1, v2, v3
        let unfiltered_results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();

        assert_eq!(unfiltered_results, vec![1, 2, 3]);

        g.close().unwrap();
    }
}

#[test]
fn test_vector_index_in_memory_commit_ryow_and_wal_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // 1. Declare HNSW vector index and test uncommitted RYOW & in-memory commit update.
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();

        // Transaction 1: Add vertex 1, verify RYOW, add vertex 2, commit.
        let mut txn = g.begin();
        txn.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0f32, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();

        // Uncommitted RYOW: nearest() should find vertex 1
        let ryow_1: Vec<i64> = txn
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ryow_1, vec![1], "RYOW should see uncommitted vertex 1");

        txn.g()
            .addV("doc")
            .property("id", 2i64)
            .property("emb", Value::FloatVector(vec![0.0f32, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();

        let ryow_2: Vec<i64> = txn
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ryow_2, vec![1, 2], "RYOW should see uncommitted vertices 1 and 2 in score order");

        txn.commit().unwrap();

        // Query immediately on existing open graph instance — in-memory index was updated on commit!
        let mut snap = g.read();
        let committed_results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(committed_results, vec![1, 2], "Committed vectors should be visible immediately in-memory");

        // Transaction 2: Drop vertex 2, verify RYOW removal, commit.
        let mut tx2 = g.begin();
        tx2.g().V([2]).drop().next().unwrap();

        let ryow_after_drop: Vec<i64> = tx2
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ryow_after_drop, vec![1], "RYOW should exclude dropped vertex 2");

        tx2.commit().unwrap();

        // Verify post-commit in-memory index updated on deletion
        let mut snap2 = g.read();
        let post_delete: Vec<i64> = snap2
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(post_delete, vec![1]);

        g.close().unwrap();
    }

    // 2. Re-open graph without rebuild — WAL replay should populate in-memory index automatically!
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();
        let recovered_results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 5)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(recovered_results, vec![1], "WAL replay on open should restore vector index without rebuild");
        g.close().unwrap();
    }
}

#[test]
fn test_vector_index_snapshot_save_on_close_and_incremental_wal_seek_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let snapshot_file = path.join("vector_idx_emb.snapshot");

    // Phase 1: Define schema, insert vectors 1 and 2, close cleanly.
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();

        let mut txn = g.begin();
        txn.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0f32, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.g()
            .addV("doc")
            .property("id", 2i64)
            .property("emb", Value::FloatVector(vec![0.0f32, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.commit().unwrap();

        // Clean close — saves snapshot (WAL entries for 1 and 2 are GC'd afterwards).
        g.close().unwrap();
    }

    // Verify snapshot file was created on close.
    assert!(snapshot_file.exists(), "Snapshot file should exist after clean close");

    // Phase 2 (crash simulation): Open, verify [1, 2], insert vector 3, commit.
    // Drop the graph WITHOUT calling close() — the snapshot is NOT updated and WAL
    // entries for vector 3 are NOT GC'd, simulating a process crash.
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();
        let results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 2)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(results, vec![1, 2]);

        let mut txn = g.begin();
        txn.g()
            .addV("doc")
            .property("id", 3i64)
            .property("emb", Value::FloatVector(vec![0.9f32, 0.1, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.commit().unwrap();

        // Drop g without close() — leaves Phase 1 snapshot on disk and WAL entry for
        // vector 3 un-GC'd.
        drop(g);
    }

    // Phase 3: Open graph -> loads Phase 1 snapshot (vectors 1 and 2) and
    // replays the WAL entry for vector 3.  Nearest to [1, 0, 0, 0] should be [1, 3].
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();
        let results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 2)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(results, vec![1, 3], "Incremental WAL seek replay must catch up from snapshot timestamp");

        // Drop vector index via schema session -> should remove snapshot file.
        let mut sess = g.open_schema();
        sess.drop_vector_index(VectorEntityType::Vertex, "emb");
        sess.commit().unwrap();

        assert!(!snapshot_file.exists(), "Snapshot file should be deleted when vector index is dropped");
        g.close().unwrap();
    }
}

#[test]
fn test_vector_index_drop_property_wal_remove_and_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // Step 1: Create vector index, add vertex 1 with embedding, then drop embedding property
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();

        let mut txn = g.begin();
        txn.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0f32, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.commit().unwrap();

        // Verify in-memory index has vertex 1
        let mut snap = g.read();
        let res: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 1)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(res, vec![1]);

        // Drop only the 'emb' property from vertex 1
        let mut tx2 = g.begin();
        tx2.g().V([1]).properties(["emb"]).drop().next().unwrap();
        tx2.commit().unwrap();

        // Verify in-memory index immediately reflects property drop
        let mut snap2 = g.read();
        let res2: Vec<i64> = snap2
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 1)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(res2, Vec::<i64>::new(), "Property drop should remove vector from in-memory index");

        // Crash-exit (drop without close) to verify WAL replay of Remove operation
        drop(g);
    }

    // Step 2: Re-open without close() - WAL replay should replay both Put and Remove, resulting in empty index
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();
        let recovered: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 1)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(recovered, Vec::<i64>::new(), "WAL replay should remove vector after property drop");
        g.close().unwrap();
    }
}

#[test]
fn test_neighbors_with_hnsw_index() {
    use crate::{
        schema::{AnnAlgorithm, DistanceMetric, HnswConfig, Quantization, VectorEntityType, VectorIndexConfig},
        Graph, TraversalBuilder, Value,
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // 1. Declare HNSW vector index.
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 2,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(HnswConfig::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();
        g.close().unwrap();
    }

    // 2. Insert 3 vertices with known 2D embeddings.
    {
        let g = Graph::open(path).unwrap();
        let mut txn = g.begin();
        // v1: [1.0, 0.0] — along x-axis
        txn.g().addV("doc").property("id", 1i64).property("emb", Value::FloatVector(vec![1.0, 0.0])).next().unwrap();
        // v2: [0.9, 0.436] — ~25° from v1 (close in cosine)
        txn.g().addV("doc").property("id", 2i64).property("emb", Value::FloatVector(vec![0.9, 0.436])).next().unwrap();
        // v3: [0.0, 1.0] — orthogonal to v1 (far)
        txn.g().addV("doc").property("id", 3i64).property("emb", Value::FloatVector(vec![0.0, 1.0])).next().unwrap();
        txn.commit().unwrap();
        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();
        g.close().unwrap();
    }

    // 3. Query neighbors of v1 using its own embedding as the query vector.
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();

        let neighbor_ids: Vec<i64> = snap
            .g()
            .V([1i64])
            .neighbors("emb", "emb", 2, VectorEntityType::Vertex)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();

        assert_eq!(neighbor_ids.len(), 2, "neighbors() must return k=2 results");
        // v2 is the closest non-self neighbor; v3 is farther — both must appear in top-2.
        assert!(
            neighbor_ids.contains(&2),
            "v2 (closest cosine neighbor of v1) must be in results, got {:?}",
            neighbor_ids
        );

        g.close().unwrap();
    }
}

#[test]
fn test_memory_limit_blocks_insert() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    // Declare schema first
    {
        let g = Graph::open(path).unwrap();
        declare_index(&g, "emb", 4, DistanceMetric::Cosine);
        g.close().unwrap();
    }
    // Reopen WITH limit
    let options = crate::schema::GraphOptions {
        index: crate::vector::IndexOptions {
            default_limit: Some(crate::vector::VectorIndexLimit { memory_limit_bytes: 16 * 1024 }),
            ..Default::default()
        },
        ..Default::default()
    };
    let g = crate::Graph::open_with_options(path, options).unwrap();
    let mut txn = g.begin();
    for i in 0..1500u64 {
        txn.g()
            .addV("doc")
            .property("id", (i + 1) as i64)
            .property("emb", Value::FloatVector(vec![(i as f32).sin(), (i as f32).cos(), 0.0, 0.0]))
            .next()
            .unwrap();
    }
    let result = txn.commit();
    assert!(result.is_err(), "pre-flight memory limit check should block commit");
    g.close().unwrap();
}

#[test]
fn test_per_index_memory_limit_override() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // Declare both indexes first
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "small".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.add_vector_index(VectorIndexConfig {
            property: "large".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();
        g.close().unwrap();
    }

    // Reopen with per-index limits
    let options = crate::schema::GraphOptions {
        index: crate::vector::IndexOptions {
            default_limit: None,
            per_index: vec![crate::vector::PerIndexOptions {
                entity_type: VectorEntityType::Vertex,
                property: "small".into(),
                memory_limit: Some(crate::vector::VectorIndexLimit { memory_limit_bytes: 16 * 1024 }),
            }],
        },
        ..Default::default()
    };
    let g = crate::Graph::open_with_options(path, options).unwrap();

    // "large" index: unlimited — should succeed
    {
        let mut txn = g.begin();
        for i in 0..1500u64 {
            txn.g()
                .addV("doc")
                .property("id", (i + 1) as i64)
                .property("large", Value::FloatVector(vec![(i as f32).sin(), (i as f32).cos(), 0.0, 0.0]))
                .next()
                .unwrap();
        }
        txn.commit().unwrap();
    }

    // "small" index: 16KB limit — should be blocked
    {
        let mut txn = g.begin();
        for i in 0..1500u64 {
            txn.g()
                .addV("doc")
                .property("id", (2000 + i + 1) as i64)
                .property("small", Value::FloatVector(vec![(i as f32).sin(), (i as f32).cos(), 0.0, 0.0]))
                .next()
                .unwrap();
        }
        let result = txn.commit();
        assert!(result.is_err(), "per-index 16KB limit should block commit for 'small'");
    }

    g.close().unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
// P1 — Index Lifecycle
// ═══════════════════════════════════════════════════════════════════════════════

use super::vector_fixtures;

const LCG_N: usize = 1001;
const LCG_DIM: usize = 16;
const LCG_K: usize = 10;
const RECALL_THRESHOLD: f32 = 0.90;

#[test]
fn test_rebuild_preserves_search_quality() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(LCG_N, LCG_DIM, 42);
    vector_fixtures::build_vector_graph(dir.path(), &vectors, DistanceMetric::Cosine, Quantization::F32, true);
    let src_ids: Vec<i64> = (1..=LCG_N as i64).step_by(50).take(20).collect();

    let g = Graph::open(dir.path()).unwrap();
    let mut snap = g.read();
    let (pre, failures) = measure_recall(&mut snap, &vectors, &src_ids, LCG_K, RECALL_THRESHOLD);
    drop(snap);

    // Rebuild on open graph
    g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

    let mut snap2 = g.read();
    let (post, failures2) = measure_recall(&mut snap2, &vectors, &src_ids, LCG_K, RECALL_THRESHOLD);

    if !failures.is_empty() {
        for (src_id, r) in &failures {
            eprintln!("rebuild pre: src={src_id} recall={r:.3}");
        }
    }
    if !failures2.is_empty() {
        for (src_id, r) in &failures2 {
            eprintln!("rebuild post: src={src_id} recall={r:.3}");
        }
    }
    assert!(pre >= RECALL_THRESHOLD, "pre-rebuild recall {:.3}", pre);
    assert!(post >= RECALL_THRESHOLD, "post-rebuild recall {:.3}", post);
    g.close().unwrap();
}

fn measure_recall(
    snap: &mut crate::ReadSession,
    vectors: &[Vec<f32>],
    src_ids: &[i64],
    k: usize,
    threshold: f32,
) -> (f32, Vec<(i64, f32)>) {
    let mut total = 0.0f32;
    let mut failures = Vec::new();
    for &src_id in src_ids {
        let q = &vectors[(src_id - 1) as usize];
        let ids = ids_from_results(
            snap.g().V([src_id]).neighbors("emb", "emb", k, VectorEntityType::Vertex).id().to_list().unwrap(),
        );
        let exact = vector_fixtures::exact_top_k(vectors, q, k, DistanceMetric::Cosine);
        let r = vector_fixtures::recall(&ids, &exact, k);
        total += r;
        if r < threshold {
            failures.push((src_id, r));
        }
    }
    (total / src_ids.len() as f32, failures)
}

#[test]
fn test_save_and_reload_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(LCG_N, LCG_DIM, 42);
    vector_fixtures::build_vector_graph(dir.path(), &vectors, DistanceMetric::Cosine, Quantization::F32, true);

    // Explicit save
    let g = Graph::open(dir.path()).unwrap();
    g.index_manager().save_all().unwrap();
    g.close().unwrap();

    // Reopen — snapshot should load without WAL replay
    let g2 = Graph::open(dir.path()).unwrap();
    let mut snap = g2.read();
    let queries = vector_fixtures::lcg_vectors(10, LCG_DIM, 99);
    let mut total = 0.0f32;
    for q in &queries {
        let ids = ids_from_results(snap.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        total += vector_fixtures::recall(&ids, &exact, LCG_K);
    }
    let avg = total / queries.len() as f32;
    assert!(avg >= RECALL_THRESHOLD, "snapshot reload recall {:.3}", avg);
    g2.close().unwrap();
}

#[test]
fn test_incremental_insert_nearest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let dim = 4usize;

    // Phase 1: Insert 5 vectors
    {
        let g = Graph::open(path).unwrap();
        declare_index(&g, "emb", dim, DistanceMetric::Cosine);
        let mut txn = g.begin();
        for i in 0..5u64 {
            let emb: Vec<f32> = (0..dim).map(|d| ((i * 7 + d as u64 * 3) as f32).sin()).collect();
            txn.g().addV("doc").property("id", (i + 1) as i64).property("emb", Value::FloatVector(emb)).next().unwrap();
        }
        txn.commit().unwrap();
        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();
        g.close().unwrap();
    }

    // Phase 2: Verify 5, then add 5 more
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();
        let pre = ids_from_results(snap.g().V([]).nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 10).id().to_list().unwrap());
        assert_eq!(pre.len(), 5, "should have 5 vectors before insert");
        drop(snap);

        let mut txn = g.begin();
        for i in 5..10u64 {
            let emb: Vec<f32> = (0..dim).map(|d| ((i * 7 + d as u64 * 3) as f32).sin()).collect();
            txn.g().addV("doc").property("id", (i + 1) as i64).property("emb", Value::FloatVector(emb)).next().unwrap();
        }
        txn.commit().unwrap();
        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

        let mut snap2 = g.read();
        let post =
            ids_from_results(snap2.g().V([]).nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 10).id().to_list().unwrap());
        assert_eq!(post.len(), 10, "should have 10 vectors after second insert");
        g.close().unwrap();
    }
}

#[test]
fn test_rebuild_while_reads_active() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(500, LCG_DIM, 42);
    vector_fixtures::build_vector_graph(dir.path(), &vectors, DistanceMetric::Cosine, Quantization::F32, true);

    let queries = vector_fixtures::lcg_vectors(10, LCG_DIM, 99);

    let g = Graph::open(dir.path()).unwrap();
    let mut snap = g.read();

    let mut pre_total = 0.0f32;
    for q in &queries {
        let ids = ids_from_results(snap.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        pre_total += vector_fixtures::recall(&ids, &exact, LCG_K);
    }
    let pre_avg = pre_total / queries.len() as f32;
    assert!(pre_avg >= RECALL_THRESHOLD, "pre-rebuild recall {:.3}", pre_avg);

    // Rebuild while read session is alive
    g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

    // Old read session still works (snapshot isolation)
    let mut post_total = 0.0f32;
    for q in &queries {
        let ids = ids_from_results(snap.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        post_total += vector_fixtures::recall(&ids, &exact, LCG_K);
    }
    let post_avg = post_total / queries.len() as f32;
    assert!(post_avg >= RECALL_THRESHOLD, "read session after rebuild {:.3}", post_avg);
    drop(snap);

    // New session works with rebuilt index
    let mut snap2 = g.read();
    let mut new_total = 0.0f32;
    for q in &queries {
        let ids = ids_from_results(snap2.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        new_total += vector_fixtures::recall(&ids, &exact, LCG_K);
    }
    let new_avg = new_total / queries.len() as f32;
    assert!(new_avg >= RECALL_THRESHOLD, "new session after rebuild {:.3}", new_avg);
    g.close().unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
// P0 — Large-scale recall tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_nearest_hnsw_recall_vs_exact() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(LCG_N, LCG_DIM, 42);
    vector_fixtures::build_vector_graph(dir.path(), &vectors, DistanceMetric::Cosine, Quantization::F32, true);
    let queries = vector_fixtures::lcg_vectors(20, LCG_DIM, 99);

    let g = Graph::open(dir.path()).unwrap();
    let mut snap = g.read();
    let mut total = 0.0f32;
    let mut failures = Vec::new();
    for (i, q) in queries.iter().enumerate() {
        let ids = ids_from_results(snap.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        let r = vector_fixtures::recall(&ids, &exact, LCG_K);
        total += r;
        if r < RECALL_THRESHOLD {
            failures.push((i, r));
        }
    }
    let avg = total / queries.len() as f32;
    if !failures.is_empty() {
        for (i, r) in &failures {
            eprintln!("nearest recall: query={i} recall={r:.3}");
        }
    }
    assert!(avg >= RECALL_THRESHOLD, "avg nearest recall {:.3} < {RECALL_THRESHOLD}", avg);
    g.close().unwrap();
}

#[test]
fn test_neighbors_hnsw_recall_vs_exact() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(LCG_N, LCG_DIM, 42);
    vector_fixtures::build_vector_graph(dir.path(), &vectors, DistanceMetric::Cosine, Quantization::F32, true);
    let src_ids: Vec<i64> = (1..=LCG_N as i64).step_by(50).take(20).collect();

    let g = Graph::open(dir.path()).unwrap();
    let mut snap = g.read();
    let mut total = 0.0f32;
    let mut failures = Vec::new();
    for &src_id in &src_ids {
        let q = &vectors[(src_id - 1) as usize];
        let ids = ids_from_results(
            snap.g().V([src_id]).neighbors("emb", "emb", LCG_K, VectorEntityType::Vertex).id().to_list().unwrap(),
        );
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        let r = vector_fixtures::recall(&ids, &exact, LCG_K);
        total += r;
        if r < RECALL_THRESHOLD {
            failures.push((src_id, r));
        }
    }
    let avg = total / src_ids.len() as f32;
    if !failures.is_empty() {
        for (src_id, r) in &failures {
            eprintln!("neighbors recall: src={src_id} recall={r:.3}");
        }
    }
    assert!(avg >= RECALL_THRESHOLD, "avg neighbors recall {:.3} < {RECALL_THRESHOLD}", avg);
    g.close().unwrap();
}

#[test]
fn test_f16_vs_f32_recall_comparison() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(LCG_N, LCG_DIM, 42);
    let path_f32 = dir.path().join("f32_db");
    let path_f16 = dir.path().join("f16_db");
    vector_fixtures::build_vector_graph(&path_f32, &vectors, DistanceMetric::Cosine, Quantization::F32, true);
    vector_fixtures::build_vector_graph(&path_f16, &vectors, DistanceMetric::Cosine, Quantization::F16, true);

    let queries = vector_fixtures::lcg_vectors(20, LCG_DIM, 99);
    let g_f32 = Graph::open(&path_f32).unwrap();
    let g_f16 = Graph::open(&path_f16).unwrap();
    let mut snap_f32 = g_f32.read();
    let mut snap_f16 = g_f16.read();

    let mut r32 = 0.0f32;
    let mut r16 = 0.0f32;
    let mut failures = Vec::new();
    for (qi, q) in queries.iter().enumerate() {
        let ids_f32 = ids_from_results(snap_f32.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let ids_f16 = ids_from_results(snap_f16.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::Cosine);
        let rf = vector_fixtures::recall(&ids_f32, &exact, LCG_K);
        let rh = vector_fixtures::recall(&ids_f16, &exact, LCG_K);
        r32 += rf;
        r16 += rh;
        if rf < RECALL_THRESHOLD || rh < 0.85 {
            failures.push((qi, rf, rh));
        }
    }
    let avg32 = r32 / queries.len() as f32;
    let avg16 = r16 / queries.len() as f32;
    if !failures.is_empty() {
        for (qi, rf, rh) in &failures {
            eprintln!("F32/F16: query={qi} F32={rf:.3} F16={rh:.3}");
        }
    }
    assert!(avg32 >= 0.90, "F32 recall {:.3}", avg32);
    assert!(avg16 >= 0.85, "F16 recall {:.3}", avg16);
    assert!((avg32 - avg16).abs() < 0.10, "F32-F16 gap {:.3}", avg32 - avg16);
    g_f32.close().unwrap();
    g_f16.close().unwrap();
}

#[test]
fn test_nearest_hnsw_recall_dotproduct() {
    let dir = tempfile::tempdir().unwrap();
    let vectors = vector_fixtures::lcg_vectors(LCG_N, LCG_DIM, 42);
    vector_fixtures::build_vector_graph(dir.path(), &vectors, DistanceMetric::DotProduct, Quantization::F32, true);
    let queries = vector_fixtures::lcg_vectors(20, LCG_DIM, 99);

    let g = Graph::open(dir.path()).unwrap();
    let mut snap = g.read();
    let mut total = 0.0f32;
    let mut failures = Vec::new();
    for (qi, q) in queries.iter().enumerate() {
        let ids = ids_from_results(snap.g().V([]).nearest("emb", q.clone(), LCG_K).id().to_list().unwrap());
        let exact = vector_fixtures::exact_top_k(&vectors, q, LCG_K, DistanceMetric::DotProduct);
        let r = vector_fixtures::recall(&ids, &exact, LCG_K);
        total += r;
        if r < RECALL_THRESHOLD {
            failures.push((qi, r));
        }
    }
    let avg = total / queries.len() as f32;
    if !failures.is_empty() {
        for (qi, r) in &failures {
            eprintln!("DotProduct recall: query={qi} recall={r:.3}");
        }
    }
    assert!(avg >= RECALL_THRESHOLD, "avg DotProduct recall {:.3}", avg);
    g.close().unwrap();
}

#[test]
fn test_nearest_and_similarity_score_consistency_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    {
        let g = Graph::open(path).unwrap();
        declare_index(&g, "emb", 4, DistanceMetric::Cosine);

        let mut txn = g.begin();
        txn.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.g()
            .addV("doc")
            .property("id", 2i64)
            .property("emb", Value::FloatVector(vec![0.6, 0.8, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.g()
            .addV("doc")
            .property("id", 3i64)
            .property("emb", Value::FloatVector(vec![0.0, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        txn.commit().unwrap();
        g.close().unwrap();
    }

    let g = Graph::open(path).unwrap();
    let mut snap = g.read();
    let query = vec![1.0, 0.0, 0.0, 0.0];

    // nearest() should return vertex 1 first, then 2, then 3
    let nearest_ids = ids_from_results(snap.g().V([]).nearest("emb", query.clone(), 3).id().to_list().unwrap());
    assert_eq!(nearest_ids, vec![1, 2, 3]);

    // similarity() on each individual vertex must produce matching scores
    let sim_1 = snap.g().V([1i64]).similarity("emb", query.clone(), DistanceMetric::Cosine).to_list().unwrap();
    let sim_2 = snap.g().V([2i64]).similarity("emb", query.clone(), DistanceMetric::Cosine).to_list().unwrap();
    let sim_3 = snap.g().V([3i64]).similarity("emb", query.clone(), DistanceMetric::Cosine).to_list().unwrap();

    assert_eq!(sim_1.len(), 1);
    assert_eq!(sim_2.len(), 1);
    assert_eq!(sim_3.len(), 1);

    let s1 = match sim_1[0] {
        Value::Float32(s) => s,
        _ => panic!("expected float"),
    };
    let s2 = match sim_2[0] {
        Value::Float32(s) => s,
        _ => panic!("expected float"),
    };
    let s3 = match sim_3[0] {
        Value::Float32(s) => s,
        _ => panic!("expected float"),
    };

    assert!((s1 - 1.0).abs() < 1e-5);
    assert!((s2 - 0.6).abs() < 1e-5);
    assert!(s3.abs() < 1e-5);
    assert!(s1 > s2 && s2 > s3);

    g.close().unwrap();
}

#[test]
fn test_similarity_on_non_vector_property_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vertex_label("person");
        sess.add_property_key("name", DataType::String);
        sess.commit().unwrap();

        let mut txn = g.begin();
        txn.g().addV("person").property("id", 1i64).property("name", "Alice").next().unwrap();
        txn.commit().unwrap();
        g.close().unwrap();
    }

    let g = Graph::open(path).unwrap();
    let mut snap = g.read();

    // Calling .similarity() on a string property must silently skip without error or panic
    let results =
        snap.g().V([]).similarity("name", vec![1.0, 0.0, 0.0, 0.0], DistanceMetric::Cosine).to_list().unwrap();
    assert!(results.is_empty(), "non-vector property should yield 0 similarity results");

    g.close().unwrap();
}

#[test]
fn test_rebuild_changes_quantization_e2e() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let dim = 32;

    // 1. Create graph with F32 quantization
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: dim,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();

        let mut txn = g.begin();
        for i in 1..=50i64 {
            let vec: Vec<f32> = (0..dim).map(|d| ((i * 13 + d as i64 * 37) as f32).sin()).collect();
            txn.g().addV("doc").property("id", i).property("emb", Value::FloatVector(vec)).next().unwrap();
        }
        txn.commit().unwrap();
        g.close().unwrap();
    }

    let snap_path = crate::vector::persistence::vector_snapshot_path(path, VectorEntityType::Vertex, "emb");
    let f32_size = std::fs::metadata(&snap_path).unwrap().len();

    // 2. Re-open, drop old F32 index, add new F16 index, and rebuild
    {
        let g = Graph::open(path).unwrap();
        let mut sess = g.open_schema();
        sess.drop_vector_index(VectorEntityType::Vertex, "emb");
        sess.commit().unwrap();

        let mut sess2 = g.open_schema();
        sess2.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: dim,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F16,
        });
        sess2.commit().unwrap();

        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

        let f16_size = std::fs::metadata(&snap_path).unwrap().len();
        assert!(f16_size < f32_size, "F16 snapshot ({f16_size} bytes) should be smaller than F32 ({f32_size} bytes)");

        // 3. Verify search works on rebuilt F16 index
        let mut snap = g.read();
        let query: Vec<f32> = (0..dim).map(|d| ((10 * 13 + d as i64 * 37) as f32).sin()).collect();
        let results = ids_from_results(snap.g().V([]).nearest("emb", query, 3).id().to_list().unwrap());
        assert_eq!(results.len(), 3);
        assert_eq!(results[0], 10, "nearest match for vertex 10 embedding should be vertex 10 itself");

        g.close().unwrap();
    }
}

#[test]
fn test_mid_session_save_all_crash_recovery() {
    // save_all() mid-session removes WAL entries via GC (not just close()).
    // Verify: save → more inserts → crash (drop without close) → reopen → all data present.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    let dim = 4usize;

    // Phase 1: insert 3 vectors, save checkpoint
    {
        let g = Graph::open(path).unwrap();
        declare_index(&g, "emb", dim, DistanceMetric::Cosine);
        let mut txn = g.begin();
        for i in 0..3u64 {
            let emb: Vec<f32> = (0..dim).map(|d| ((i * 7 + d as u64 * 3) as f32).sin()).collect();
            txn.g().addV("doc").property("id", (i + 1) as i64).property("emb", Value::FloatVector(emb)).next().unwrap();
        }
        txn.commit().unwrap();
        // Mid-session save_all — triggers WAL GC
        g.index_manager().save_all().unwrap();
        g.close().unwrap();
    }

    // Phase 2: insert 2 more vectors, crash (drop without close)
    {
        let g = Graph::open(path).unwrap();
        let mut txn = g.begin();
        for i in 3..5u64 {
            let emb: Vec<f32> = (0..dim).map(|d| ((i * 7 + d as u64 * 3) as f32).sin()).collect();
            txn.g().addV("doc").property("id", (i + 1) as i64).property("emb", Value::FloatVector(emb)).next().unwrap();
        }
        txn.commit().unwrap();
        drop(g); // crash — no close(), no save_all()
    }

    // Phase 3: reopen — should recover all 5 vectors
    {
        let g = Graph::open(path).unwrap();
        let mut snap = g.read();
        let ids = ids_from_results(snap.g().V([]).nearest("emb", vec![1.0, 0.0, 0.0, 0.0], 10).id().to_list().unwrap());
        assert_eq!(ids.len(), 5, "mid-session save_all + crash should recover all vectors");
        g.close().unwrap();
    }
}

#[test]
fn test_rebuild_preserves_memory_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    {
        let g = Graph::open(path).unwrap();
        declare_index(&g, "emb", 4, DistanceMetric::Cosine);
        g.close().unwrap();
    }

    // Open WITH memory limit
    let options = crate::schema::GraphOptions {
        index: crate::vector::IndexOptions {
            default_limit: Some(crate::vector::VectorIndexLimit { memory_limit_bytes: 16 * 1024 }),
            ..Default::default()
        },
        ..Default::default()
    };
    let g = crate::Graph::open_with_options(path, options).unwrap();

    // Rebuild index
    g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();

    // Inserts exceeding memory limit after rebuild should still be rejected cleanly
    let mut txn = g.begin();
    for i in 0..1500u64 {
        txn.g()
            .addV("doc")
            .property("id", (i + 1) as i64)
            .property("emb", Value::FloatVector(vec![(i as f32).sin(), (i as f32).cos(), 0.0, 0.0]))
            .next()
            .unwrap();
    }
    let result = txn.commit();
    assert!(result.is_err(), "memory limit must be preserved after rebuild()");
    g.close().unwrap();
}

#[test]
fn test_schema_session_applies_configured_memory_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();

    // Open graph WITH memory limit configured in options
    let options = crate::schema::GraphOptions {
        index: crate::vector::IndexOptions {
            default_limit: Some(crate::vector::VectorIndexLimit { memory_limit_bytes: 16 * 1024 }),
            ..Default::default()
        },
        ..Default::default()
    };
    let g = crate::Graph::open_with_options(path, options).unwrap();

    // Add vector index dynamically via SchemaSession
    let mut sess = g.open_schema();
    sess.add_vector_index(VectorIndexConfig {
        property: "dyn_emb".into(),
        entity_type: VectorEntityType::Vertex,
        dimension: 4,
        metric: DistanceMetric::Cosine,
        algorithm: AnnAlgorithm::Hnsw(Default::default()),
        quantization: Quantization::F32,
    });
    sess.commit().unwrap();

    // Inserts exceeding memory limit on the newly added index should be blocked
    let mut txn = g.begin();
    for i in 0..1500u64 {
        txn.g()
            .addV("doc")
            .property("id", (i + 1) as i64)
            .property("dyn_emb", Value::FloatVector(vec![(i as f32).sin(), (i as f32).cos(), 0.0, 0.0]))
            .next()
            .unwrap();
    }
    let result = txn.commit();
    assert!(result.is_err(), "dynamically created vector index must inherit configured memory limit");
    g.close().unwrap();
}

#[test]
fn test_nearest_k_zero_live_hnsw() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = crate::Graph::open(tmp.path()).unwrap();

    let mut schema = graph.open_schema();
    schema.add_vector_index(VectorIndexConfig {
        property: "emb".into(),
        entity_type: VectorEntityType::Vertex,
        dimension: 2,
        metric: DistanceMetric::Cosine,
        algorithm: AnnAlgorithm::Hnsw(Default::default()),
        quantization: Quantization::F32,
    });
    schema.commit().unwrap();

    let mut txn = graph.begin();
    txn.g().addV("item").property("id", 1i64).property("emb", Value::FloatVector(vec![1.0, 0.0])).next().unwrap();
    txn.g().addV("item").property("id", 2i64).property("emb", Value::FloatVector(vec![0.0, 1.0])).next().unwrap();
    txn.commit().unwrap();

    let mut snap = graph.read();
    let results = snap.g().V([]).nearest("emb", vec![1.0f32, 0.0], 0).to_list().unwrap();
    assert!(results.is_empty(), "k=0 against live HNSW must return 0 results without error");
}

#[test]
fn test_neighbors_k_zero_live_hnsw() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = crate::Graph::open(tmp.path()).unwrap();

    let mut schema = graph.open_schema();
    schema.add_vector_index(VectorIndexConfig {
        property: "emb".into(),
        entity_type: VectorEntityType::Vertex,
        dimension: 2,
        metric: DistanceMetric::Cosine,
        algorithm: AnnAlgorithm::Hnsw(Default::default()),
        quantization: Quantization::F32,
    });
    schema.commit().unwrap();

    let mut txn = graph.begin();
    txn.g().addV("item").property("id", 1i64).property("emb", Value::FloatVector(vec![1.0, 0.0])).next().unwrap();
    txn.commit().unwrap();

    let mut snap = graph.read();
    let results = snap.g().V([1i64]).neighbors("emb", "emb", 0, VectorEntityType::Vertex).id().to_list().unwrap();
    assert!(results.is_empty(), "k=0 neighbors against live HNSW must return 0 results without error");
}

#[test]
fn test_nearest_with_metric_live_hnsw() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = crate::Graph::open(tmp.path()).unwrap();

    let mut schema = graph.open_schema();
    schema.add_vector_index(VectorIndexConfig {
        property: "emb".into(),
        entity_type: VectorEntityType::Vertex,
        dimension: 2,
        metric: DistanceMetric::Cosine, // index built with Cosine
        algorithm: AnnAlgorithm::Hnsw(Default::default()),
        quantization: Quantization::F32,
    });
    schema.commit().unwrap();

    let mut txn = graph.begin();
    // Item 1: normalized vector [1.0, 0.0]
    txn.g().addV("item").property("id", 1i64).property("emb", Value::FloatVector(vec![1.0, 0.0])).next().unwrap();
    // Item 2: large magnitude vector [10.0, 0.0]
    txn.g().addV("item").property("id", 2i64).property("emb", Value::FloatVector(vec![10.0, 0.0])).next().unwrap();
    txn.commit().unwrap();

    let mut snap = graph.read();

    // With metric override DotProduct: item 2 has dot product 10.0 vs item 1 dot product 1.0 with [1.0, 0.0]
    let results = snap
        .g()
        .V([])
        .nearest("emb", vec![1.0f32, 0.0], 2)
        .with_metric(DistanceMetric::DotProduct)
        .id()
        .to_list()
        .unwrap();

    assert_eq!(results.len(), 2);
    // Under DotProduct, Item 2 (score 10) must rank before Item 1 (score 1)
    assert_eq!(results[0], Value::Int64(2), "item 2 must rank first under DotProduct metric override");
    assert_eq!(results[1], Value::Int64(1));
}
