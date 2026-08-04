// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use tempfile::tempdir;

use crate::{
    schema::{definition::EdgeMode, GraphOptions, SchemaMode},
    store::RocksOptions,
    types::{gvalue::Primitive, StoreError},
    Graph, TraversalBuilder, Value,
};

use super::degree::{DegreeCounter, SortedLabelFile};
use super::edge_annotator::annotate_edges;
#[allow(deprecated)]
use super::loader::{
    write_sst_from_iter_dedup, BulkEdge, BulkSchema, BulkVertex, SstBulkLoader, BULK_LOAD_IN_PROGRESS_KEY,
    MARKER_POST_INGEST,
};
use super::sort::ExternalSorter;

fn small_schema() -> BulkSchema {
    BulkSchema { vertex_labels: vec!["Person".into()], edge_labels: vec!["Knows".into()], prop_keys: vec![] }
}

fn small_vertices() -> Vec<BulkVertex> {
    (1..=5).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect()
}

fn small_edges() -> Vec<BulkEdge> {
    vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 1, dst: 3, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 2, dst: 3, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 3, dst: 4, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 4, dst: 5, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 5, dst: 1, label: "Knows".into(), props: HashMap::new(), rank: None },
    ]
}

#[test]
fn test_bulk_loader_fluent_api() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let graph = Graph::open(&db_path).unwrap();

    let mut loader = graph.open_bulk_loader().unwrap();
    loader.load_vertices(small_vertices()).unwrap();
    loader.load_edges(small_edges()).unwrap();
    let stats = loader.commit().unwrap();

    assert_eq!(stats.vertices_written, 5);
    assert_eq!(stats.edges_written, 6);
    assert!(stats.sst_files >= 4);

    let mut snap = graph.read();
    let v_count = snap.g().V([]).count().next().unwrap().unwrap();
    assert_eq!(v_count, Value::Int64(5));

    let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
    assert_eq!(out_count, Value::Int64(2));
    graph.close().unwrap();
}

#[test]
fn test_bulk_loader_auto_schema_discovery() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let graph = Graph::open(&db_path).unwrap();

    let vertices = vec![
        BulkVertex { id: 1, label: "User".into(), props: [("name".into(), Primitive::String("Alice".into()))].into() },
        BulkVertex { id: 2, label: "User".into(), props: [("name".into(), Primitive::String("Bob".into()))].into() },
    ];
    let edges = vec![BulkEdge {
        src: 1,
        dst: 2,
        label: "Follows".into(),
        props: [("since".into(), Primitive::Int64(2026))].into(),
        rank: None,
    }];

    let mut loader = graph.open_bulk_loader().unwrap();
    loader.load_vertices(vertices).unwrap();
    loader.load_edges(edges).unwrap();
    loader.commit().unwrap();

    let mut snap = graph.read();
    assert_eq!(snap.g().V([1_i64]).values(["name"]).next().unwrap().unwrap(), Value::String("Alice".into()));
    assert_eq!(snap.g().V([1_i64]).outE(["Follows"]).values(["since"]).next().unwrap().unwrap(), Value::Int64(2026));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_load_initial_small() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let loader = SstBulkLoader::new(&db_path, dir.path().join("_bulk_work"));
    let stats = loader
        .load_initial(
            small_schema(),
            small_vertices().into_iter(),
            small_edges().into_iter(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();
    assert_eq!(stats.vertices_written, 5);
    assert_eq!(stats.edges_written, 6);
    assert!(stats.sst_files >= 4);

    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    let v_count = snap.g().V([]).count().next().unwrap().unwrap();
    assert_eq!(v_count, Value::Int64(5));
    let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
    assert_eq!(out_count, Value::Int64(2));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_duplicate_edge_single_mode() {
    let dir = tempdir().unwrap();
    let vertices: Vec<BulkVertex> =
        (1..=3).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
    ];
    let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { edge_mode: EdgeMode::Single, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::DuplicateEdge(_)));
}

#[test]
#[allow(deprecated)]
fn test_strict_mode_unknown_label() {
    let dir = tempdir().unwrap();
    let vertices = vec![BulkVertex { id: 1, label: "Unknown".into(), props: HashMap::new() }];
    let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            std::iter::empty(),
            GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

#[test]
#[allow(deprecated)]
fn test_crash_marker_detection() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");

    SstBulkLoader::new(&db_path, dir.path().join("_bulk_work"))
        .load_initial(
            small_schema(),
            small_vertices().into_iter(),
            small_edges().into_iter(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();

    {
        use crate::store::rocks::cf_options;
        use crate::store::rocks::{CF_EDGES_IN, CF_EDGES_OUT, CF_SCHEMA, CF_VECTOR_WAL, CF_VERTEX_DEGREE, CF_VERTICES};
        use rocksdb::{
            ColumnFamilyDescriptor, MultiThreaded, OptimisticTransactionDB, Options, WriteBatchWithTransaction,
        };
        let storage_opts = RocksOptions::default();
        let v_bo = cf_options::vertex_block_opts(&storage_opts, None);
        let e_bo = cf_options::edge_block_opts(&storage_opts, None);
        let mut dbo = Options::default();
        dbo.create_if_missing(false);
        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_VERTICES, cf_options::vertex_cf_opts(&storage_opts, &v_bo)),
            ColumnFamilyDescriptor::new(CF_VERTEX_DEGREE, cf_options::vertex_cf_opts(&storage_opts, &v_bo)),
            ColumnFamilyDescriptor::new(CF_EDGES_OUT, cf_options::edge_cf_opts(&storage_opts, &e_bo)),
            ColumnFamilyDescriptor::new(CF_EDGES_IN, cf_options::edge_cf_opts(&storage_opts, &e_bo)),
            ColumnFamilyDescriptor::new(CF_SCHEMA, Options::default()),
            ColumnFamilyDescriptor::new(CF_VECTOR_WAL, Options::default()),
        ];
        let db: OptimisticTransactionDB<MultiThreaded> =
            OptimisticTransactionDB::open_cf_descriptors(&dbo, &db_path, cfs).unwrap();
        let cf = db.cf_handle(CF_SCHEMA).unwrap();
        let mut batch = WriteBatchWithTransaction::<true>::default();
        batch.put_cf(&cf, BULK_LOAD_IN_PROGRESS_KEY, [MARKER_POST_INGEST]);
        db.write(batch).unwrap();
    }

    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    let v_count = snap.g().V([]).count().next().unwrap().unwrap();
    assert_eq!(v_count, Value::Int64(5));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_multiple_edges_multi_mode_auto_rank() {
    let dir = tempdir().unwrap();
    let vertices: Vec<BulkVertex> =
        (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
    ];
    let stats = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap();
    assert_eq!(stats.edges_written, 2);

    let graph = Graph::open(dir.path().join("db")).unwrap();
    let mut snap = graph.read();
    let edges_count = snap.g().V([1_i64]).outE(["Knows"]).count().next().unwrap().unwrap();
    assert_eq!(edges_count, Value::Int64(2));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_edge_referencing_unknown_vertex() {
    let dir = tempdir().unwrap();
    let vertices = vec![BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() }];
    let edges = vec![BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None }];
    let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_bulk_work"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

#[test]
#[allow(deprecated)]
fn test_load_initial_external_sort() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let loader = SstBulkLoader::new(&db_path, dir.path().join("_bulk_work")).with_max_memory(1);
    let stats = loader
        .load_initial(
            small_schema(),
            small_vertices().into_iter(),
            small_edges().into_iter(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();
    assert_eq!(stats.vertices_written, 5);
    assert_eq!(stats.edges_written, 6);

    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    let v_count = snap.g().V([]).count().next().unwrap().unwrap();
    assert_eq!(v_count, Value::Int64(5));
    let out_count = snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap();
    assert_eq!(out_count, Value::Int64(2));
    graph.close().unwrap();
}

#[test]
fn test_dedup_iter_returns_correct_duplicate_edge() {
    use crate::{
        store::rocks::cf_options,
        types::{keys::CanonicalEdgeKey, kv_codec},
    };

    let dir = tempdir().unwrap();
    let storage_opts = RocksOptions::default();
    let e_bo = cf_options::edge_block_opts(&storage_opts, None);
    let e_opts = cf_options::edge_cf_opts(&storage_opts, &e_bo);

    let cek = CanonicalEdgeKey { src_id: 10, label_id: 1, dst_id: 20, rank: 0 };
    let key = kv_codec::encode_edge_key(&cek.out_key()).to_vec();
    let val = kv_codec::EdgeValue { end_vertex_label: 1, property_blob: vec![] }.encode();

    let pairs = vec![Ok((key.clone(), val.clone())), Ok((key.clone(), val.clone()))];

    let err =
        write_sst_from_iter_dedup("test_edges", pairs.into_iter(), dir.path(), 64 * 1024 * 1024, &e_opts).unwrap_err();

    match err {
        StoreError::DuplicateEdge(detected) => {
            assert_eq!(detected.src_id, 10);
            assert_eq!(detected.label_id, 1);
            assert_eq!(detected.dst_id, 20);
            assert_eq!(detected.rank, 0);
        }
        other => panic!("expected DuplicateEdge, got: {other}"),
    }
}

#[test]
fn test_sorted_label_file_basic_and_dedup() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("labels.bin");
    let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);

    sorter.push(20i64.to_be_bytes().to_vec(), 2i32.to_be_bytes().to_vec()).unwrap();
    sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
    sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
    sorter.push(30i64.to_be_bytes().to_vec(), 3i32.to_be_bytes().to_vec()).unwrap();

    let file = SortedLabelFile::write_from(sorter, &path).unwrap();
    assert_eq!(file.count, 3);

    let reader: Vec<_> = file.reader().unwrap().map(|r| r.unwrap()).collect();
    assert_eq!(reader, vec![(10, 1), (20, 2), (30, 3)]);
}

#[test]
fn test_sorted_label_file_conflicting_labels() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("labels.bin");
    let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);

    sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
    sorter.push(10i64.to_be_bytes().to_vec(), 2i32.to_be_bytes().to_vec()).unwrap();

    let err = SortedLabelFile::write_from(sorter, &path).unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

#[test]
#[allow(clippy::type_complexity)]
fn test_degree_counter() {
    let items: Vec<Result<(Vec<u8>, Vec<u8>), StoreError>> = vec![
        Ok((10i64.to_be_bytes().to_vec(), vec![])),
        Ok((10i64.to_be_bytes().to_vec(), vec![])),
        Ok((10i64.to_be_bytes().to_vec(), vec![])),
        Ok((20i64.to_be_bytes().to_vec(), vec![])),
        Ok((30i64.to_be_bytes().to_vec(), vec![])),
        Ok((30i64.to_be_bytes().to_vec(), vec![])),
    ];

    let mut counter = DegreeCounter::new(items.into_iter()).unwrap();
    assert_eq!(counter.count_for(10).unwrap(), 3);
    assert_eq!(counter.count_for(15).unwrap(), 0);
    assert_eq!(counter.count_for(20).unwrap(), 1);
    assert_eq!(counter.count_for(30).unwrap(), 2);
    assert_eq!(counter.count_for(40).unwrap(), 0);
}

#[test]
#[allow(clippy::type_complexity)]
fn test_annotate_edges_mismatched_vertex() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("labels.bin");
    let mut sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);
    sorter.push(10i64.to_be_bytes().to_vec(), 1i32.to_be_bytes().to_vec()).unwrap();
    let file = SortedLabelFile::write_from(sorter, &path).unwrap();

    let annot_item: Vec<Result<(Vec<u8>, Vec<u8>), StoreError>> = vec![Ok((
        {
            let mut k = vec![0u8; 30];
            k[0..8].copy_from_slice(&20i64.to_be_bytes());
            k
        },
        vec![],
    ))];

    let mut out_sorter = ExternalSorter::new(dir.path().join("out"), 1024 * 1024);
    let err = annotate_edges(annot_item.into_iter(), &file, &mut out_sorter).unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

#[test]
#[allow(deprecated)]
fn test_empty_input() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
        .load_initial(
            small_schema(),
            std::iter::empty(),
            std::iter::empty(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();
    assert_eq!(stats.vertices_written, 0);
    assert_eq!(stats.edges_written, 0);
    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(0));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_vertices_only_no_edges() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let vertices: Vec<BulkVertex> =
        (1..=3).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            std::iter::empty(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();
    assert_eq!(stats.vertices_written, 3);
    assert_eq!(stats.edges_written, 0);
    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(3));
    assert_eq!(snap.g().V([1_i64]).out([]).count().next().unwrap().unwrap(), Value::Int64(0));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_properties_roundtrip() {
    use crate::{schema::DataType, Primitive};
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let schema = BulkSchema {
        vertex_labels: vec!["Person".into()],
        edge_labels: vec!["Knows".into()],
        prop_keys: vec![("age".into(), DataType::Int64), ("score".into(), DataType::Int64)],
    };
    let vertices = vec![
        BulkVertex { id: 1, label: "Person".into(), props: [("age".into(), Primitive::Int64(42))].into() },
        BulkVertex { id: 2, label: "Person".into(), props: HashMap::new() },
    ];
    let edges = vec![BulkEdge {
        src: 1,
        dst: 2,
        label: "Knows".into(),
        props: [("score".into(), Primitive::Int64(100))].into(),
        rank: None,
    }];
    SstBulkLoader::new(&db_path, dir.path().join("_w"))
        .load_initial(
            schema,
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();
    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    assert_eq!(snap.g().V([1_i64]).values(["age"]).next().unwrap().unwrap(), Value::Int64(42));
    assert_eq!(snap.g().V([1_i64]).outE(["Knows"]).values(["score"]).next().unwrap().unwrap(), Value::Int64(100));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_multi_mode_explicit_ranks() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let vertices: Vec<BulkVertex> =
        (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(5) },
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(10) },
    ];
    let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap();
    assert_eq!(stats.edges_written, 2);
    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    assert_eq!(snap.g().V([1_i64]).outE(["Knows"]).count().next().unwrap().unwrap(), Value::Int64(2));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_multi_mode_explicit_rank_duplicate() {
    let dir = tempdir().unwrap();
    let vertices: Vec<BulkVertex> =
        (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(3) },
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(3) },
    ];
    let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_w"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::DuplicateEdge(_)));
}

#[test]
#[allow(deprecated)]
fn test_multi_mode_rank_overflow() {
    let dir = tempdir().unwrap();
    let vertices: Vec<BulkVertex> =
        (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: Some(65534) },
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
    ];
    let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_w"))
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { edge_mode: EdgeMode::Multi, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

#[test]
#[allow(deprecated)]
fn test_sst_file_splitting() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let stats = SstBulkLoader::new(&db_path, dir.path().join("_w"))
        .with_max_sst_size(1)
        .load_initial(
            small_schema(),
            small_vertices().into_iter(),
            small_edges().into_iter(),
            GraphOptions::default(),
            &RocksOptions::default(),
        )
        .unwrap();
    assert!(stats.sst_files >= 4, "expected at least one file per CF, got {}", stats.sst_files);
    let graph = Graph::open(&db_path).unwrap();
    let mut snap = graph.read();
    assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(5));
    assert_eq!(snap.g().V([1_i64]).out(["Knows"]).count().next().unwrap().unwrap(), Value::Int64(2));
    graph.close().unwrap();
}

#[test]
#[allow(deprecated)]
fn test_work_dir_cleaned_up_on_error() {
    let dir = tempdir().unwrap();
    let work_dir = dir.path().join("_w");
    let vertices: Vec<BulkVertex> =
        (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![BulkEdge { src: 1, dst: 2, label: "Unknown".into(), props: HashMap::new(), rank: None }];
    let err = SstBulkLoader::new(dir.path().join("db"), work_dir.clone())
        .load_initial(
            small_schema(),
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
    assert!(!work_dir.exists(), "WorkDirGuard should have removed work_dir on error");
}

#[test]
fn test_empty_sorted_label_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("labels.bin");
    let sorter = ExternalSorter::new(dir.path().join("sort"), 1024 * 1024);
    let file = SortedLabelFile::write_from(sorter, &path).unwrap();
    assert_eq!(file.count, 0);
    let items: Vec<_> = file.reader().unwrap().collect();
    assert!(items.is_empty());
}

#[test]
#[allow(deprecated)]
fn test_strict_mode_undeclared_edge_property() {
    use crate::{schema::DataType, Primitive};
    let dir = tempdir().unwrap();
    let schema = BulkSchema {
        vertex_labels: vec!["Person".into()],
        edge_labels: vec!["Knows".into()],
        prop_keys: vec![("age".into(), DataType::Int64)],
    };
    let vertices: Vec<BulkVertex> =
        (1..=2).map(|i| BulkVertex { id: i, label: "Person".into(), props: HashMap::new() }).collect();
    let edges = vec![BulkEdge {
        src: 1,
        dst: 2,
        label: "Knows".into(),
        props: [("weight".into(), Primitive::Float64(1.5))].into(),
        rank: None,
    }];
    let err = SstBulkLoader::new(dir.path().join("db"), dir.path().join("_w"))
        .load_initial(
            schema,
            vertices.into_iter(),
            edges.into_iter(),
            GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
            &RocksOptions::default(),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::SchemaViolation(_)));
}

#[test]
fn test_bulk_loader_ordering_enforcement() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path()).unwrap();
    let mut loader = graph.open_bulk_loader().unwrap();

    // 1. Calling load_edges before load_vertices -> VerticesNotLoaded
    let edges = vec![BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None }];
    assert!(matches!(loader.load_edges(edges), Err(StoreError::VerticesNotLoaded)));

    // 2. Calling commit before load_vertices -> VerticesNotLoaded
    drop(loader); // release AtomicBool lock
    let empty_loader = graph.open_bulk_loader().unwrap();
    assert!(matches!(empty_loader.commit(), Err(StoreError::VerticesNotLoaded)));

    // Re-open for tests 3-4
    let mut loader = graph.open_bulk_loader().unwrap();

    // 3. Calling load_vertices twice -> UnsupportedOperation
    let vertices = vec![BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() }];
    loader.load_vertices(vertices.clone()).unwrap();
    assert!(matches!(loader.load_vertices(vertices), Err(StoreError::UnsupportedOperation(_))));

    // 4. Calling load_edges twice -> UnsupportedOperation
    let edges = vec![BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None }];
    loader.load_edges(edges.clone()).unwrap();
    assert!(matches!(loader.load_edges(edges), Err(StoreError::UnsupportedOperation(_))));
}

#[test]
fn test_bulk_loader_commit_without_edges() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path().join("db")).unwrap();
    let mut loader = graph.open_bulk_loader().unwrap();

    let vertices = vec![
        BulkVertex { id: 10, label: "Node".into(), props: HashMap::new() },
        BulkVertex { id: 20, label: "Node".into(), props: HashMap::new() },
    ];

    loader.load_vertices(vertices).unwrap();
    // Skip load_edges entirely
    let stats = loader.commit().unwrap();

    assert_eq!(stats.vertices_written, 2);
    assert_eq!(stats.edges_written, 0);

    let mut snap = graph.read();
    assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(2));
    assert_eq!(snap.g().V([10_i64]).out([]).count().next().unwrap().unwrap(), Value::Int64(0));
}

#[test]
fn test_bulk_loader_auto_schema_sync_back() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let graph = Graph::open(&db_path).unwrap();

    let vertices = vec![BulkVertex {
        id: 1,
        label: "Account".into(),
        props: [("balance".into(), Primitive::Float64(1250.50))].into(),
    }];
    let edges = vec![BulkEdge {
        src: 1,
        dst: 1,
        label: "TransfersTo".into(),
        props: [("amount".into(), Primitive::Float64(50.0))].into(),
        rank: None,
    }];

    let mut loader = graph.open_bulk_loader().unwrap();
    loader.load_vertices(vertices).unwrap();
    loader.load_edges(edges).unwrap();
    loader.commit().unwrap();

    // Verify in-memory schema on Graph was updated
    {
        let schema_lock = graph.schema();
        let schema = schema_lock.read();
        assert!(schema.vertex_label_id("Account").is_some());
        assert!(schema.edge_label_id("TransfersTo").is_some());
        assert!(schema.prop_key_id("balance").is_some());
        assert!(schema.prop_key_id("amount").is_some());
    }

    // Close and re-open graph to verify schema persisted to CF_SCHEMA
    drop(graph);
    let reopened = Graph::open(&db_path).unwrap();
    let mut snap = reopened.read();
    assert_eq!(snap.g().V([]).hasLabel(["Account"]).count().next().unwrap().unwrap(), Value::Int64(1));
    assert_eq!(snap.g().V([1_i64]).values(["balance"]).next().unwrap().unwrap(), Value::Float64(1250.50));
    assert_eq!(
        snap.g().V([1_i64]).outE(["TransfersTo"]).values(["amount"]).next().unwrap().unwrap(),
        Value::Float64(50.0)
    );
}

#[test]
fn test_bulk_loader_strict_schema_enforcement() {
    use crate::schema::DataType;

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");
    let graph =
        Graph::open_with_options(&db_path, GraphOptions { mode: SchemaMode::Strict, ..Default::default() }).unwrap();

    // Declare schema beforehand
    {
        let mut s = graph.open_schema();
        s.add_vertex_label("User");
        s.add_edge_label("Follows");
        s.add_property_key("name", DataType::String);
        s.commit().unwrap();
    }

    // 1. Undeclared vertex label -> SchemaViolation
    {
        let mut loader = graph.open_bulk_loader().unwrap();
        let res = loader.load_vertices(vec![BulkVertex { id: 1, label: "UnknownLabel".into(), props: HashMap::new() }]);
        assert!(matches!(res, Err(StoreError::SchemaViolation(_))));
    }

    // 2. Undeclared edge label -> SchemaViolation
    {
        let mut loader = graph.open_bulk_loader().unwrap();
        loader
            .load_vertices(vec![
                BulkVertex { id: 1, label: "User".into(), props: HashMap::new() },
                BulkVertex { id: 2, label: "User".into(), props: HashMap::new() },
            ])
            .unwrap();
        let res = loader.load_edges(vec![BulkEdge {
            src: 1,
            dst: 2,
            label: "UnknownEdge".into(),
            props: HashMap::new(),
            rank: None,
        }]);
        assert!(matches!(res, Err(StoreError::SchemaViolation(_))));
    }

    // 3. Undeclared property key -> SchemaViolation
    {
        let mut loader = graph.open_bulk_loader().unwrap();
        loader
            .load_vertices(vec![
                BulkVertex { id: 1, label: "User".into(), props: HashMap::new() },
                BulkVertex { id: 2, label: "User".into(), props: HashMap::new() },
            ])
            .unwrap();
        let res = loader.load_edges(vec![BulkEdge {
            src: 1,
            dst: 2,
            label: "Follows".into(),
            props: [("undeclared_prop".into(), Primitive::Int64(1))].into(),
            rank: None,
        }]);
        assert!(matches!(res, Err(StoreError::SchemaViolation(_))));
    }
}

#[test]
fn test_bulk_loader_drop_and_custom_work_dir_cleanup() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path().join("db")).unwrap();
    let custom_work_dir = dir.path().join("my_custom_scratch");

    {
        let mut loader = graph
            .open_bulk_loader()
            .unwrap()
            .with_work_dir(&custom_work_dir)
            .with_max_memory(1024 * 1024)
            .with_max_sst_size(1024 * 1024)
            .with_rocks_options(RocksOptions::default());

        loader.load_vertices(vec![BulkVertex { id: 1, label: "V".into(), props: HashMap::new() }]).unwrap();
        assert!(custom_work_dir.exists());
        // Drop without calling commit()
    }

    // Work directory should be cleaned up by Drop
    assert!(!custom_work_dir.exists(), "work directory should be cleaned up when BulkLoader is dropped");
}

#[test]
fn test_bulk_loader_iterator_workflow() {
    let dir = tempdir().unwrap();
    let graph = Graph::open(dir.path().join("db")).unwrap();
    let mut loader = graph.open_bulk_loader().unwrap();

    let vertices = vec![
        BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() },
        BulkVertex { id: 2, label: "Person".into(), props: HashMap::new() },
        BulkVertex { id: 3, label: "Person".into(), props: HashMap::new() },
    ];
    let edges = vec![
        BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 2, dst: 3, label: "Knows".into(), props: HashMap::new(), rank: None },
        BulkEdge { src: 3, dst: 1, label: "Knows".into(), props: HashMap::new(), rank: None },
    ];

    loader.load_vertices(vertices).unwrap();
    loader.load_edges(edges).unwrap();
    let stats = loader.commit().unwrap();

    assert_eq!(stats.vertices_written, 3);
    assert_eq!(stats.edges_written, 3);

    let mut snap = graph.read();
    assert_eq!(snap.g().V([]).count().next().unwrap().unwrap(), Value::Int64(3));
    assert_eq!(snap.g().V([1_i64]).out([]).count().next().unwrap().unwrap(), Value::Int64(1));
}

#[test]
fn test_bulk_loader_auto_rebuilds_vector_indexes() {
    use crate::vector::{AnnAlgorithm, DistanceMetric, Quantization, VectorEntityType, VectorIndexConfig};

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("db");

    // 1. Open graph and declare a vector index on "emb"
    {
        let graph = Graph::open(&db_path).unwrap();
        let mut sess = graph.open_schema();
        sess.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: 4,
            metric: DistanceMetric::Cosine,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization: Quantization::F32,
        });
        sess.commit().unwrap();

        // 2. Open bulk loader and load vertices with embeddings
        let mut loader = graph.open_bulk_loader().unwrap();
        let mut props1 = HashMap::new();
        props1.insert("emb".into(), Primitive::FloatVector(vec![1.0, 0.0, 0.0, 0.0]));
        let mut props2 = HashMap::new();
        props2.insert("emb".into(), Primitive::FloatVector(vec![0.0, 1.0, 0.0, 0.0]));
        let mut props3 = HashMap::new();
        props3.insert("emb".into(), Primitive::FloatVector(vec![0.9, 0.1, 0.0, 0.0]));

        let vertices = vec![
            BulkVertex { id: 1, label: "Doc".into(), props: props1 },
            BulkVertex { id: 2, label: "Doc".into(), props: props2 },
            BulkVertex { id: 3, label: "Doc".into(), props: props3 },
        ];

        loader.load_vertices(vertices).unwrap();
        let stats = loader.commit().unwrap();
        assert_eq!(stats.vertices_written, 3);

        // 3. Immediately query vector index without manual rebuild
        let mut snap = graph.read();
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
        assert_eq!(results, vec![1, 3], "BulkLoader::commit must auto-populate vector indexes");

        graph.close().unwrap();
    }

    // 4. Reopen graph and verify index persistence via snapshot
    {
        let graph = Graph::open(&db_path).unwrap();
        let mut snap = graph.read();
        let results: Vec<i64> = snap
            .g()
            .V([])
            .nearest("emb", vec![0.0, 1.0, 0.0, 0.0], 1)
            .id()
            .to_list()
            .unwrap()
            .into_iter()
            .filter_map(|v| match v {
                Value::Int64(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(results, vec![2]);
        graph.close().unwrap();
    }
}
