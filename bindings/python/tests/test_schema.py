import os

import pytest

from rocksgraph import (
    AnnAlgorithm,
    DataType,
    DistanceMetric,
    Graph,
    GraphOptions,
    Int64,
    Quantization,
    SchemaError,
    SchemaMode,
    Vector,
    VectorEntityType,
    VectorIndexConfig,
)


def test_schema_session_basic_declaration(tmp_path):
    db_path = os.path.join(tmp_path, "schema_db")
    g = Graph(db_path)

    schema = g.open_schema()
    schema.set_schema_mode(SchemaMode.Strict)
    schema.add_vertex_label("person")
    schema.add_vertex_label("software")
    schema.add_edge_label("knows")
    schema.add_edge_label("created")
    schema.add_property_key("name", DataType.String)
    schema.add_property_key("age", DataType.Int64)
    schema.commit()

    g.close()

    # Reopen in strict mode — declared labels should succeed
    g_strict = Graph.open_with_options(db_path, options=GraphOptions(mode=SchemaMode.Strict))
    with g_strict.begin() as txn:
        txn.g().addV("person", 1).property("name", "Alice").property("age", Int64(30)).next()
        txn.g().addV("software", 2).property("name", "RocksGraph").next()
        txn.g().addE("created").from_(1).to(2).next()

    snap = g_strict.read()
    assert snap.g().V(1).values("name").to_list() == ["Alice"]
    assert snap.g().V(1).out("created").values("name").to_list() == ["RocksGraph"]

    # Undeclared label in strict mode should fail
    with g_strict.begin() as txn:
        with pytest.raises(SchemaError) as exc_info:
            txn.g().addV("unknown_label", 3).next()
        assert "schema violation" in str(exc_info.value).lower() or "undeclared" in str(exc_info.value).lower()

    g_strict.close()


def test_schema_session_context_manager(tmp_path):
    db_path = os.path.join(tmp_path, "schema_cm_db")
    g = Graph(db_path)

    with g.open_schema() as s:
        s.add_vertex_label("device").add_property_key("ip", DataType.String)

    g.close()

    g_strict = Graph.open_with_options(db_path, options=GraphOptions(mode="strict"))
    with g_strict.begin() as txn:
        txn.g().addV("device", 10).property("ip", "192.168.1.1").next()

    snap = g_strict.read()
    assert snap.g().V(10).values("ip").to_list() == ["192.168.1.1"]
    g_strict.close()


def test_schema_vector_index_declaration(tmp_path):
    db_path = os.path.join(tmp_path, "schema_vec_db")
    g = Graph(db_path)

    with g.open_schema() as s:
        s.add_vertex_label("doc")
        s.add_property_key("embedding", DataType.FloatVector)
        s.add_vector_index(
            property="embedding",
            dimension=4,
            entity_type=VectorEntityType.Vertex,
            metric=DistanceMetric.Cosine,
            algorithm=AnnAlgorithm.Hnsw,
            m=16,
            ef_construction=100,
            ef_search=30,
            quantization=Quantization.F16,
        )

    # Insert vertices with vectors
    with g.begin() as txn:
        txn.g().addV("doc", 1).property("embedding", Vector([1.0, 0.0, 0.0, 0.0])).next()
        txn.g().addV("doc", 2).property("embedding", Vector([0.0, 1.0, 0.0, 0.0])).next()

    snap = g.read()
    results = snap.g().V().nearest("embedding", Vector([1.0, 0.1, 0.0, 0.0]), 1).to_list()
    assert len(results) == 1
    assert results[0].id == 1

    # Drop vector index
    with g.open_schema() as s:
        s.drop_vector_index(VectorEntityType.Vertex, "embedding")

    g.close()


def test_schema_vector_index_config_object(tmp_path):
    db_path = os.path.join(tmp_path, "schema_config_obj_db")
    g = Graph(db_path)

    cfg = VectorIndexConfig(
        property="vec",
        dimension=3,
        entity_type="vertex",
        metric="euclidean",
        algorithm="hnsw",
        m=8,
        ef_construction=50,
        ef_search=20,
        quantization="f32",
    )

    with g.open_schema() as s:
        s.add_vertex_label("item")
        s.add_property_key("vec", DataType.FloatVector)
        s.add_vector_index(cfg)

    with g.begin() as txn:
        txn.g().addV("item", 100).property("vec", Vector([1.0, 2.0, 3.0])).next()

    snap = g.read()
    assert snap.g().V(100).count().to_list() == [1]

    g.close()


def test_schema_invalid_modes_raise_error(tmp_path):
    db_path = os.path.join(tmp_path, "schema_invalid_db")
    g = Graph(db_path)

    # Invalid mode in open_with_options
    with pytest.raises(ValueError):
        Graph.open_with_options(db_path, options=GraphOptions(mode="invalid_mode"))

    with pytest.raises(ValueError):
        Graph.open_with_options(db_path, options=GraphOptions(edge_mode="invalid_edge_mode"))

    # Invalid mode in schema session
    with g.open_schema() as s:
        with pytest.raises(ValueError):
            s.set_schema_mode("invalid_mode")

        with pytest.raises(ValueError):
            s.set_edge_mode("invalid_edge_mode")

        with pytest.raises(ValueError):
            s.drop_vector_index("invalid_entity", "vec")

        with pytest.raises(ValueError):
            s.add_vector_index(property="vec", dimension=4, entity_type="invalid_entity")

        with pytest.raises(ValueError):
            s.add_vector_index(property="vec", dimension=4, metric="invalid_metric")

        with pytest.raises(ValueError):
            s.add_vector_index(property="vec", dimension=4, algorithm="invalid_algo")

        with pytest.raises(ValueError):
            s.add_vector_index(property="vec", dimension=4, quantization="invalid_quant")

    # Invalid VectorIndexConfig initialization
    with pytest.raises(ValueError):
        VectorIndexConfig(property="vec", dimension=4, entity_type="invalid_entity")

    with pytest.raises(ValueError):
        VectorIndexConfig(property="vec", dimension=4, metric="invalid_metric")

    with pytest.raises(ValueError):
        VectorIndexConfig(property="vec", dimension=4, algorithm="invalid_algo")

    with pytest.raises(ValueError):
        VectorIndexConfig(property="vec", dimension=4, quantization="invalid_quant")

    g.close()
