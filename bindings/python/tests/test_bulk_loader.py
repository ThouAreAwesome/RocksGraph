import os

from rocksgraph import (
    BulkEdge,
    BulkLoadStats,
    BulkVertex,
    DataType,
    DistanceMetric,
    Graph,
    Int64,
    Vector,
)


def test_bulk_loader_basic(tmp_path):
    db_path = os.path.join(tmp_path, "bulk_db")
    work_dir = os.path.join(tmp_path, "bulk_work")
    g = Graph(db_path)

    # 1. Define schema first (BulkLoader requires declared schema)
    with g.open_schema() as s:
        s.add_vertex_label("person")
        s.add_vertex_label("software")
        s.add_edge_label("knows")
        s.add_edge_label("created")
        s.add_property_key("name", DataType.String)
        s.add_property_key("age", DataType.Int64)
        s.add_property_key("since", DataType.Int64)

    # 2. Bulk load data
    loader = g.open_bulk_loader()
    loader.with_work_dir(work_dir)
    loader.with_max_sst_size(8 * 1024 * 1024)
    loader.with_max_memory(32 * 1024 * 1024)

    # Load with BulkVertex objects and dicts in a single pass
    loader.load_vertices(
        [
            BulkVertex(1, "person", {"name": "Alice", "age": Int64(30)}),
            BulkVertex(2, "person", {"name": "Bob", "age": Int64(25)}),
            {"id": 3, "label": "software", "props": {"name": "RocksGraph"}},
        ]
    )

    # Load edges with BulkEdge objects and dicts in a single pass
    loader.load_edges(
        [
            BulkEdge(1, 2, "knows", {"since": Int64(2020)}),
            BulkEdge(1, 3, "created"),
            {"src": 2, "dst": 3, "label": "created"},
        ]
    )

    stats = loader.commit()

    assert isinstance(stats, BulkLoadStats)
    assert stats.vertices_written == 3
    assert stats.edges_written == 3
    assert stats.sst_files >= 1
    assert stats.duration_secs >= 0.0

    # 3. Query loaded data
    snap = g.read()
    assert snap.g().V().count().to_list() == [3]
    assert snap.g().V(1).values("name").to_list() == ["Alice"]
    assert snap.g().V(2).values("name").to_list() == ["Bob"]
    assert snap.g().V(3).values("name").to_list() == ["RocksGraph"]

    # Traversal over loaded edges
    assert snap.g().V(1).out("knows").values("name").to_list() == ["Bob"]
    created_names = snap.g().V().hasLabel("person").out("created").values("name").to_list()
    assert created_names == ["RocksGraph", "RocksGraph"]

    g.close()


def test_bulk_loader_context_manager(tmp_path):
    db_path = os.path.join(tmp_path, "bulk_cm_db")
    g = Graph(db_path)

    with g.open_schema() as s:
        s.add_vertex_label("node")
        s.add_edge_label("connects")
        s.add_property_key("val", DataType.Int64)

    with g.open_bulk_loader() as loader:
        loader.load_vertices(
            [
                BulkVertex(10, "node", {"val": Int64(100)}),
                BulkVertex(20, "node", {"val": Int64(200)}),
            ]
        )
        loader.load_edges(
            [
                BulkEdge(10, 20, "connects"),
            ]
        )

    snap = g.read()
    assert snap.g().V(10).out("connects").id().to_list() == [20]
    g.close()


def test_bulk_loader_with_vector_index(tmp_path):
    db_path = os.path.join(tmp_path, "bulk_vec_db")
    g = Graph(db_path)

    # 1. Define schema with vector index
    with g.open_schema() as s:
        s.add_vertex_label("doc")
        s.add_property_key("embedding", DataType.FloatVector)
        s.add_vector_index(
            property="embedding",
            dimension=3,
            metric=DistanceMetric.Cosine,
        )

    # 2. Bulk load documents with vector embeddings
    with g.open_bulk_loader() as loader:
        loader.load_vertices(
            [
                BulkVertex(1, "doc", {"embedding": Vector([1.0, 0.0, 0.0])}),
                BulkVertex(2, "doc", {"embedding": Vector([0.0, 1.0, 0.0])}),
                BulkVertex(3, "doc", {"embedding": Vector([0.0, 0.0, 1.0])}),
            ]
        )

    # 3. Vector search should immediately work after bulk load commit
    snap = g.read()
    results = snap.g().V().nearest("embedding", Vector([0.9, 0.1, 0.0]), 1).to_list()
    assert len(results) == 1
    assert results[0].id == 1

    g.close()
