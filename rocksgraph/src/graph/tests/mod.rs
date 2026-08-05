// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

mod crud;
mod isolation;
mod persistence_wal;
mod schema;
mod vector;

use smol_str::SmolStr;

use super::LogicalGraph;

use crate::{
    store::RocksStorage,
    types::{
        element::Property,
        element::Vertex,
        gvalue::Primitive,
        keys::{AdjacentEdgesOptions, CanonicalEdgeKey, CanonicalKey, Direction, EdgeKey, LabelId, VertexKey},
        prop_key::LABEL_KEY_ID,
        StoreError,
    },
};

fn open() -> (RocksStorage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = RocksStorage::open(dir.path(), &Default::default()).unwrap();
    {
        let loaded = store.load_schema(crate::schema::SchemaMode::Auto, crate::schema::EdgeMode::Single).unwrap();
        let schema = std::sync::Arc::new(parking_lot::RwLock::new(loaded));
        let mut c = LogicalGraph::new(
            store.begin(),
            schema.clone(),
            crate::vector::empty_vector_index_map(),
            Default::default(),
        );
        {
            let mut s = schema.write();
            s.resolve_prop_key("age", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("name", crate::schema::DataType::String).unwrap();
            s.resolve_prop_key("x", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("y", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("w", crate::schema::DataType::Float64).unwrap();
            s.resolve_prop_key("a", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("b", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("since", crate::schema::DataType::Int32).unwrap();
            s.resolve_prop_key("nonexistent", crate::schema::DataType::Int32).unwrap();

            s.resolve_vertex_label("person").unwrap();
            s.resolve_vertex_label("software").unwrap();
            s.resolve_edge_label("knows").unwrap();
            s.resolve_edge_label("created").unwrap();
        }
        for label_id in 0..10 {
            c.staged_schema.staged_vertex_labels.insert(label_id);
            c.staged_schema.staged_edge_labels.insert(label_id);
        }
        for prop_key_id in 0..20 {
            c.staged_schema.staged_prop_keys.insert(prop_key_id);
        }
        c.commit().unwrap();
    }
    (store, dir)
}

fn ctx(store: &RocksStorage) -> LogicalGraph {
    let loaded = store.load_schema(crate::schema::SchemaMode::Auto, crate::schema::EdgeMode::Single).unwrap();
    let schema = std::sync::Arc::new(parking_lot::RwLock::new(loaded));
    LogicalGraph::new(store.begin(), schema, crate::vector::empty_vector_index_map(), Default::default())
}

fn cek(src: i64, label: LabelId, dst: i64) -> CanonicalEdgeKey {
    CanonicalEdgeKey { src_id: src, label_id: label, rank: 0, dst_id: dst }
}

fn get_adjacent_edges_test(
    c: &mut LogicalGraph,
    vertex: VertexKey,
    direction: Direction,
    label: Option<LabelId>,
    dst: Option<&[VertexKey]>,
    limit: Option<u32>,
) -> Vec<EdgeKey> {
    c.get_adjacent_edges(vertex, direction, AdjacentEdgesOptions { label, dst, rank: None, start_from: None }, limit)
        .unwrap()
        .0
}
