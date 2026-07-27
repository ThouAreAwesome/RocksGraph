import pytest
import os
import shutil
from rocksgraph import Graph, __, P, Int64, Float32

@pytest.fixture
def graph():
    path = "/tmp/test_rocksgraph_python"
    if os.path.exists(path):
        shutil.rmtree(path)
    g = Graph(path)
    yield g
    if os.path.exists(path):
        shutil.rmtree(path)

def test_basic_crud(graph):
    # Insert vertices and edges
    tx = graph.tx()
    v1 = tx.g().addV("person").property("name", "Alice").property("age", Int64(30)).next()[0]
    v2 = tx.g().addV("person").property("name", "Bob").property("age", Int64(35)).next()[0]
    
    # We didn't supply id, so they are auto-generated.
    tx.g().addE("knows").from_(v1).to(v2).property("weight", Float32(0.8)).next()
    tx.commit()
    
    # Read session
    rs = graph.read()
    
    # Query vertices
    res = rs.g().V().hasLabel("person").order().by("age", "asc").values("name").toList()
    assert res == ["Alice", "Bob"]
    
    # Query edge properties
    res = rs.g().V(v1["id"]).outE("knows").values("weight").toList()
    assert len(res) == 1
    assert abs(res[0] - 0.8) < 1e-6
    
    # Path
    paths = rs.g().V(v1["id"]).out("knows").path().toList()
    assert len(paths) == 1
    assert len(paths[0]["objects"]) == 2
    
def test_group(graph):
    tx = graph.tx()
    tx.g().addV("person").property("name", "Alice").property("city", "NY").next()
    tx.g().addV("person").property("name", "Bob").property("city", "NY").next()
    tx.g().addV("person").property("name", "Charlie").property("city", "SF").next()
    tx.commit()
    
    rs = graph.read()
    groups = rs.g().V().hasLabel("person").group().by("city").toList()
    assert len(groups) == 1
    m = groups[0]
    assert len(m["NY"]) == 2
    assert len(m["SF"]) == 1

def test_coalesce(graph):
    tx = graph.tx()
    tx.g().addV("user").property("id_str", "u1").property("name", "Alice").next()
    tx.commit()
    
    # UPSERT pattern using coalesce
    tx = graph.tx()
    res = tx.g().V().has("user", "id_str", "u1").fold().coalesce(
        __.unfold(),
        __.addV("user").property("id_str", "u1").property("name", "NewName")
    ).toList()
    
    # Should yield existing Alice
    assert len(res) == 1
    assert res[0]["properties"]["name"][0] == "Alice"
    tx.commit()
