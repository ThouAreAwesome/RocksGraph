from tests.conftest import addv
"""Persistence across reopen — §7 of TODO.md."""
from rocksgraph import Graph, Int64, Float64
import os


class TestPersistence:
    def test_data_survives_reopen(self, tmpdir):
        # Open, write, close
        g1 = Graph(tmpdir)
        tx = g1.tx()
        addv(tx, "person", name="Alice", age=Int64(30))
        tx.commit()
        del g1  # close

        # Reopen
        g2 = Graph(tmpdir)
        rs = g2.read()
        result = rs.traversal().V().hasLabel("person").values("name").to_list()
        assert result == ["Alice"]

    def test_all_types_survive_reopen(self, tmpdir):
        g1 = Graph(tmpdir)
        tx = g1.tx()
        addv(tx, "test", i=42, f=Float64(3.14), s="hi", b=True)
        tx.commit()
        del g1

        g2 = Graph(tmpdir)
        rs = g2.read()
        # Properties must be read via traversal, not from next() dict
        assert rs.traversal().V().hasLabel("test").values("s").to_list() == ["hi"]
        assert rs.traversal().V().hasLabel("test").values("b").to_list() == [True]
        assert rs.traversal().V().hasLabel("test").values("i").to_list() == [42]

    def test_ids_stable_across_reopen(self, tmpdir):
        g1 = Graph(tmpdir)
        tx = g1.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        vid = v["id"]
        del g1

        g2 = Graph(tmpdir)
        rs = g2.read()
        found = rs.traversal().V(vid).to_list()
        assert len(found) == 1
        assert found[0]["id"] == vid

    def test_edge_survives_reopen(self, tmpdir):
        from rocksgraph import Graph, Int64
        g1 = Graph(tmpdir)
        tx = g1.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).property("since", Int64(2020)).next()
        tx.commit()
        del g1

        g2 = Graph(tmpdir)
        rs = g2.read()
        edges = rs.traversal().V(v1["id"]).outE("knows").to_list()
        assert len(edges) == 1
        edge = edges[0]
        assert edge["src"] == v1["id"]
        assert edge["dst"] == v2["id"]
