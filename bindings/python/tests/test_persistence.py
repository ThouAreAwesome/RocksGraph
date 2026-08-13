"""Persistence across reopen — §7 of TODO.md."""

from rocksgraph import ExecutionOptions, Float64, Graph, GraphOptions, Int64, RocksOptions
from tests.conftest import addv


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
        from rocksgraph import Graph, Int64

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
        assert edge["out_v"] == v1["id"]
        assert edge["in_v"] == v2["id"]


def test_open_with_options_strict_mode(tmp_path):
    """Verify strict schema mode rejects undeclared labels."""
    from rocksgraph import Graph, SchemaError

    db_path = str(tmp_path / "strict_db")

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
    from rocksgraph import Graph

    db_path = str(tmp_path / "cache_db")
    g = Graph.open_with_options(
        db_path, options=GraphOptions(storage=RocksOptions(block_cache_size=64 * 1024 * 1024))
    )  # 64 MB

    with g.begin() as txn:
        txn.g().addV("test", 1).next()

    snap = g.read()
    assert snap.g().V().count().to_list() == [1]

    g.close()


def test_open_with_options_custom_execution_batch_sizes(tmp_path):
    """Verify GraphOptions(execution=...) is accepted and doesn't break scans, even with
    batch sizes far smaller than the vertex count (forcing multiple internal fetch rounds)."""
    db_path = str(tmp_path / "execution_options_db")
    g = Graph.open_with_options(
        db_path,
        options=GraphOptions(
            execution=ExecutionOptions(
                scan_vertices_batch_size=4,
                scan_edges_batch_size=4,
                get_adjacent_edges_batch_size=2,
            )
        ),
    )

    with g.begin() as txn:
        for i in range(1, 21):
            txn.g().addV("item", i).next()
        for i in range(1, 20):
            txn.g().addE("next").from_(i).to(i + 1).next()

    snap = g.read()
    assert snap.g().V().count().to_list() == [20]
    assert snap.g().V(1).out("next").out("next").out("next").count().to_list() == [1]

    g.close()


def test_read_session_with_execution_options_overrides_batch_size(tmp_path):
    """Verify ReadSession.with_execution_options() applies a per-session override and
    doesn't affect correctness even with a batch size smaller than the result set."""
    db_path = str(tmp_path / "session_execution_options_db")
    g = Graph(db_path)

    with g.begin() as txn:
        for i in range(1, 11):
            txn.g().addV("item", i).next()

    snap = g.read().with_execution_options(ExecutionOptions(scan_vertices_batch_size=1))
    assert sorted(v["id"] for v in snap.g().V().to_list()) == list(range(1, 11))

    g.close()


def test_txn_session_with_execution_options_overrides_batch_size(tmp_path):
    """Verify TxnSession.with_execution_options() applies a per-session override and
    doesn't affect correctness even with a batch size smaller than the result set."""
    db_path = str(tmp_path / "txn_execution_options_db")
    g = Graph(db_path)

    txn = g.begin().with_execution_options(ExecutionOptions(scan_vertices_batch_size=1))
    for i in range(1, 11):
        txn.g().addV("item", i).next()
    assert sorted(v["id"] for v in txn.g().V().to_list()) == list(range(1, 11))
    txn.commit()

    g.close()
