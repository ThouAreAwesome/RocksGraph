import os
import pytest
from rocksgraph import (
    Graph,
    DataType,
    VectorEntityType,
    DistanceMetric,
    Vector,
)


def test_vector_index_rebuild_and_save(tmp_path):
    db_path = os.path.join(tmp_path, "vec_mgmt_db")
    g = Graph(db_path)

    # 1. Define schema
    with g.open_schema() as s:
        s.add_vertex_label("doc")
        s.add_property_key("embedding", DataType.FloatVector)
        s.add_vector_index(
            property="embedding",
            dimension=3,
            entity_type=VectorEntityType.Vertex,
            metric=DistanceMetric.Cosine,
        )

    # 2. Insert data
    with g.begin() as txn:
        txn.g().addV("doc", 1).property("embedding", Vector([1.0, 0.0, 0.0])).next()
        txn.g().addV("doc", 2).property("embedding", Vector([0.0, 1.0, 0.0])).next()
        txn.g().addV("doc", 3).property("embedding", Vector([0.0, 0.0, 1.0])).next()

    # 3. Explicitly rebuild the vector index via IndexManager
    mgr = g.index_manager()
    mgr.rebuild(VectorEntityType.Vertex, "embedding")

    # Verify nearest works
    snap = g.read()
    results = snap.g().V().nearest("embedding", Vector([0.9, 0.1, 0.0]), 1).to_list()
    assert len(results) == 1
    assert results[0].id == 1

    # 4. Save specific vector index and all vector indexes via IndexManager
    mgr.save(VectorEntityType.Vertex, "embedding")
    mgr.save_all()

    del snap
    g.close()  # releases the DB lock even while mgr is still in scope
    del g

    # 5. Reopen graph and verify snapshot persistence allows immediate vector query
    g_reopened = Graph(db_path)
    snap2 = g_reopened.read()
    results2 = snap2.g().V().nearest("embedding", Vector([0.1, 0.9, 0.0]), 1).to_list()
    assert len(results2) == 1
    assert results2[0].id == 2

    # Test string parameter for entity_type
    mgr2 = g_reopened.index_manager()
    mgr2.rebuild("vertex", "embedding")
    mgr2.save("vertex", "embedding")

    g_reopened.close()
