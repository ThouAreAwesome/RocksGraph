from tests.conftest import addv
"""Persistence across reopen — §7 of TODO.md."""
from rocksgraph import Graph, GraphOptions, RocksOptions, IndexOptions, Int64, Float64
import os


class TestPersistence:
    def test_data_survives_reopen(self, tmpdir):
        # Open, write, close
        g1 = Graph(tmpdir)
        txn = g1.begin()
        addv(txn, "person", name="Alice", age=Int64(30))
        txn.commit()
        del g1  # close

        # Reopen
        g2 = Graph(tmpdir)
        rs = g2.read()
        result = rs.g().V().hasLabel("person").values("name").to_list()
        assert result == ["Alice"]

    def test_all_types_survive_reopen(self, tmpdir):
        g1 = Graph(tmpdir)
        txn = g1.begin()
        addv(txn, "test", i=42, f=Float64(3.14), s="hi", b=True)
        txn.commit()
        del g1

        g2 = Graph(tmpdir)
        rs = g2.read()
        # Properties must be read via traversal, not from next() dict
        assert rs.g().V().hasLabel("test").values("s").to_list() == ["hi"]
        assert rs.g().V().hasLabel("test").values("b").to_list() == [True]
        assert rs.g().V().hasLabel("test").values("i").to_list() == [42]

    def test_ids_stable_across_reopen(self, tmpdir):
        g1 = Graph(tmpdir)
        txn = g1.begin()
        v = addv(txn, "person", name="Alice")
        txn.commit()
        vid = v["id"]
        del g1

        g2 = Graph(tmpdir)
        rs = g2.read()
        found = rs.g().V(vid).to_list()
        assert len(found) == 1
        assert found[0]["id"] == vid

    def test_edge_survives_reopen(self, tmpdir):
        from rocksgraph import Graph, GraphOptions, RocksOptions, IndexOptions, Int64
        g1 = Graph(tmpdir)
        txn = g1.begin()
        v1 = addv(txn, "person", name="Alice")
        v2 = addv(txn, "person", name="Bob")
        txn.g().addE("knows").from_(v1).to(v2).property("since", Int64(2020)).next()
        txn.commit()
        del g1

        g2 = Graph(tmpdir)
        rs = g2.read()
        edges = rs.g().V(v1["id"]).outE("knows").to_list()
        assert len(edges) == 1
        edge = edges[0]
        assert edge["src"] == v1["id"]
        assert edge["dst"] == v2["id"]

def test_open_with_options_strict_mode(tmp_path):
    """Verify strict schema mode rejects undeclared labels."""
    import os
    from rocksgraph import Graph, GraphOptions, RocksOptions, IndexOptions, SchemaError

    db_path = os.path.join(tmp_path, "strict_db")

    g = Graph.open_with_options(db_path, options=GraphOptions(mode="strict"))

    with g.begin() as txn:
        try:
            txn.g().addV("undeclared", 1).next()
            assert False, "should have raised"
        except SchemaError as e:
            assert "SchemaViolation" in str(e) or "not declared" in str(e).lower() or "schema" in str(e).lower()

    g.close()


def test_open_with_options_custom_cache(tmp_path):
    """Verify custom block_cache_size is accepted."""
    import os
    from rocksgraph import Graph, GraphOptions, RocksOptions, IndexOptions

    db_path = os.path.join(tmp_path, "cache_db")
    g = Graph.open_with_options(db_path, options=GraphOptions(storage=RocksOptions(block_cache_size=64 * 1024 * 1024)))  # 64 MB

    with g.begin() as txn:
        txn.g().addV("test", 1).next()

    snap = g.read()
    assert snap.g().V().count().to_list() == [1]

    g.close()
