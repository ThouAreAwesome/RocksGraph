from tests.conftest import addv
"""Transaction semantics — §6 of TODO.md."""


class TestTransactionCommit:
    def test_commit_persists_data(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.commit()

        rs = graph.read()
        results = rs.traversal().V().hasLabel("person").values("name").to_list()
        assert results == ["Alice"]

    def test_rollback_discards_writes(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.rollback()

        rs = graph.read()
        results = rs.traversal().V().hasLabel("person").count().to_list()
        assert results == [0]

    def test_double_commit(self, graph):
        tx = graph.tx()
        addv(tx, "person")
        tx.commit()
        # Second commit should raise or be a no-op
        try:
            tx.commit()
            # If no-op, second commit does nothing. Acceptable.
        except Exception as e:
            # If raises, also acceptable. Just verify no crash.
            assert "RuntimeError" in type(e).__name__ or "closed" in str(e).lower()

    def test_snapshot_isolation(self, graph):
        """A read session opened before commit should not see committed data."""
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.commit()

        # Snapshot BEFORE commit
        rs_before = graph.read()
        before_count = rs_before.traversal().V().hasLabel("person").count().to_list()
        assert before_count == [1], "Should see already-committed data"

        # New writes in a separate tx
        tx2 = graph.tx()
        addv(tx2, "person", name="Bob")
        # rs_before should NOT see Bob (snapshot taken before tx2)
        before_count2 = rs_before.traversal().V().hasLabel("person").count().to_list()
        assert before_count2 == [1], "Snapshot should not see uncommitted writes"

        tx2.commit()
        # After commit, a new read session SHOULD see Bob
        rs_after = graph.read()
        after_count = rs_after.traversal().V().hasLabel("person").count().to_list()
        assert after_count == [2]
