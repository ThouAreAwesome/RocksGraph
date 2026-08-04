// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{
    gremlin::traversal::TraversalBuilder,
    vector::{AnnAlgorithm, DistanceMetric, Quantization, VectorEntityType, VectorIndexConfig},
    Graph, Value,
};

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
    g.rebuild_vector_index(VectorEntityType::Vertex, "emb").unwrap();
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
        let mut tx = g.begin();
        tx.g()
            .addV("test")
            .property("id", 1i64)
            .property("emb", crate::Value::FloatVector(vec![1.0, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        tx.g()
            .addV("test")
            .property("id", 2i64)
            .property("emb", crate::Value::FloatVector(vec![0.0, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        tx.g()
            .addV("test")
            .property("id", 3i64)
            .property("emb", crate::Value::FloatVector(vec![0.0, 0.0, 1.0, 0.0]))
            .next()
            .unwrap();
        tx.commit().unwrap();
        g.close().unwrap();
    }

    // 3. Re-open, rebuild, and verify search correctness.
    {
        let g = Graph::open(path).unwrap();
        g.rebuild_vector_index(VectorEntityType::Vertex, "emb").unwrap();

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
        let mut tx = g.begin();
        // v1: tech, emb [1, 0, 0, 0] (exact match to query)
        tx.g()
            .addV("node")
            .property("id", 1i64)
            .property("category", "tech")
            .property("emb", Value::FloatVector(vec![1.0, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        // v2: finance, emb [0.99, 0.01, 0.0, 0.0] (high similarity, but different category)
        tx.g()
            .addV("node")
            .property("id", 2i64)
            .property("category", "finance")
            .property("emb", Value::FloatVector(vec![0.99, 0.01, 0.0, 0.0]))
            .next()
            .unwrap();
        // v3: tech, emb [0.0, 1.0, 0.0, 0.0] (orthogonal)
        tx.g()
            .addV("node")
            .property("id", 3i64)
            .property("category", "tech")
            .property("emb", Value::FloatVector(vec![0.0, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        // v4: tech, no embedding property at all
        tx.g().addV("node").property("id", 4i64).property("category", "tech").next().unwrap();
        tx.commit().unwrap();

        g.rebuild_vector_index(VectorEntityType::Vertex, "emb").unwrap();

        let mut snap = g.read();

        // Query with upstream filter .has("category", "tech")
        let filtered_results: Vec<i64> = snap
            .g()
            .V([])
            .has("category", "tech")
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

        // v2 (finance) is filtered out, v4 (no emb) is ignored -> only v1 and v3 remain
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
        let mut tx = g.begin();
        tx.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0f32, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();

        // Uncommitted RYOW: nearest() should find vertex 1
        let ryow_1: Vec<i64> = tx
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

        tx.g()
            .addV("doc")
            .property("id", 2i64)
            .property("emb", Value::FloatVector(vec![0.0f32, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();

        let ryow_2: Vec<i64> = tx
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

        tx.commit().unwrap();

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

        let mut tx = g.begin();
        tx.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0f32, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        tx.g()
            .addV("doc")
            .property("id", 2i64)
            .property("emb", Value::FloatVector(vec![0.0f32, 1.0, 0.0, 0.0]))
            .next()
            .unwrap();
        tx.commit().unwrap();

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

        let mut tx = g.begin();
        tx.g()
            .addV("doc")
            .property("id", 3i64)
            .property("emb", Value::FloatVector(vec![0.9f32, 0.1, 0.0, 0.0]))
            .next()
            .unwrap();
        tx.commit().unwrap();

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

        let mut tx = g.begin();
        tx.g()
            .addV("doc")
            .property("id", 1i64)
            .property("emb", Value::FloatVector(vec![1.0f32, 0.0, 0.0, 0.0]))
            .next()
            .unwrap();
        tx.commit().unwrap();

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
