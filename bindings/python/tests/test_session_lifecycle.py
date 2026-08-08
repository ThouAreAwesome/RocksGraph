"""Session lifecycle tests — lock release and misuse diagnostics.

RocksDB holds a process-level file lock for the duration the database is open.
Any live session that holds an Arc<OptimisticTransactionDB> clone keeps that
lock alive even after Graph.close() is called. These tests confirm that each
session type releases the lock at the expected lifecycle boundary so the DB
can be reopened immediately after.

They also document the exact RuntimeError a user gets when they misuse a
session after it has been closed, so the error is always at the point of
misuse rather than some internal traversal detail.
"""

import pytest

from rocksgraph import Graph, StorageError

# ── ReadSession: lock release ─────────────────────────────────────────────────


class TestReadSessionLifecycle:
    def test_context_manager_releases_lock(self, tmp_path):
        """with g.read() as snap: releases the snapshot on __exit__."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        with g.read() as snap:
            snap.g().V().to_list()
        # snapshot released here — g.close() can now fully close the DB

        g.close()

        # Lock is released — reopen succeeds immediately.
        g2 = Graph(db_path)
        g2.close()

    def test_explicit_close_releases_lock(self, tmp_path):
        """snap.close() releases the snapshot Arc before g.close()."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        snap = g.read()
        snap.g().V().to_list()
        snap.close()

        g.close()

        g2 = Graph(db_path)
        g2.close()

    def test_live_snapshot_holds_lock_after_graph_close(self, tmp_path):
        """A live ReadSession keeps the DB lock even after g.close().

        g.close() releases Graph's own Arc<OptimisticTransactionDB>, but the
        snapshot holds its own clone. The lock stays until snap.close() drops
        that clone. Without snap.close() the user gets a confusing
        StorageError on the next open even though they already called g.close().
        """
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        snap = g.read()
        g.close()

        # snap still in scope — Arc<OptimisticTransactionDB> clone is alive.
        with pytest.raises(StorageError):
            Graph(db_path)  # "lock hold by current process"

        snap.close()  # drops the Arc clone

        # Lock released — reopen succeeds.
        g2 = Graph(db_path)
        g2.close()


# ── ReadSession: misuse diagnostics ──────────────────────────────────────────


class TestReadSessionMisuse:
    def test_g_after_close_raises_at_call_site(self, tmp_path):
        """snap.g() after snap.close() raises immediately, not at the terminal.

        Previously: 'Anonymous traversal cannot be executed' (obscure).
        Now: 'ReadSession is already closed' (pinpoints the problem).
        """
        db_path = str(tmp_path / "db")
        g = Graph(db_path)
        snap = g.read()
        snap.close()

        with pytest.raises(RuntimeError, match="ReadSession is already closed"):
            snap.g()

        g.close()

    def test_double_close_is_silent(self, tmp_path):
        """snap.close() is idempotent — closing an already-closed session is harmless."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)
        snap = g.read()
        snap.close()
        snap.close()  # second close — must not raise
        g.close()


# ── TxnSession: lock release ───────────────────────────────────────────────────


class TestTxnSessionLifecycle:
    def test_commit_releases_lock(self, tmp_path):
        """txn.commit() takes the inner Transaction by value, releasing the Arc."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        txn = g.begin()
        txn.commit()
        # Transaction Arc released — g.close() and immediate reopen both succeed.

        g.close()

        g2 = Graph(db_path)
        g2.close()

    def test_rollback_releases_lock(self, tmp_path):
        """txn.rollback() also releases the Arc immediately."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        txn = g.begin()
        txn.rollback()

        g.close()

        g2 = Graph(db_path)
        g2.close()

    def test_context_manager_commits_and_releases_lock(self, tmp_path):
        """with g.begin() as txn: commits on __exit__ (no exception)."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        with g.begin() as txn:
            txn.g().addV("node", 1).next()

        g.close()

        g2 = Graph(db_path)
        results = g2.read().g().V().to_list()
        assert len(results) == 1
        g2.close()

    def test_context_manager_rolls_back_on_exception(self, tmp_path):
        """with g.begin() as txn: rolls back on exception, still releasing Arc."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        with pytest.raises(RuntimeError), g.begin() as txn:
            txn.g().addV("node", 1).next()
            raise RuntimeError("abort")

        g.close()

        # Rollback happened; reopen succeeds and data is absent.
        g2 = Graph(db_path)
        results = g2.read().g().V().to_list()
        assert len(results) == 0
        g2.close()


# ── TxnSession: misuse diagnostics ────────────────────────────────────────────


class TestTxnSessionMisuse:
    def test_g_after_commit_raises_at_call_site(self, tmp_path):
        """txn.g() after txn.commit() raises immediately, not at the terminal.

        Previously: 'Session already closed' appeared at .next() — pointing
        to the wrong line. Now it fires at txn.g() before a traversal is built.
        """
        db_path = str(tmp_path / "db")
        g = Graph(db_path)
        txn = g.begin()
        txn.commit()

        with pytest.raises(RuntimeError, match="TxnSession is already closed"):
            txn.g()

        g.close()

    def test_g_after_rollback_raises_at_call_site(self, tmp_path):
        """txn.g() after txn.rollback() behaves the same as after commit."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)
        txn = g.begin()
        txn.rollback()

        with pytest.raises(RuntimeError, match="TxnSession is already closed"):
            txn.g()

        g.close()

    def test_manual_commit_inside_with_block_is_safe(self, tmp_path):
        """txn.commit() inside with block: __exit__ detects it and is a no-op.

        Previously: __exit__ attempted a second commit and raised
        'Session already closed' — an error attributed to the with-statement
        line, not to the user's explicit commit call.
        Now: __exit__ sees _session is None and exits cleanly.
        """
        db_path = str(tmp_path / "db")
        g = Graph(db_path)

        with g.begin() as txn:
            txn.g().addV("node", 1).next()
            txn.commit()  # explicit commit — __exit__ must not double-commit

        # Data was committed; reopen confirms it.
        g.close()
        g2 = Graph(db_path)
        assert len(g2.read().g().V().to_list()) == 1
        g2.close()

    def test_double_commit_raises_at_second_call(self, tmp_path):
        """Calling txn.commit() twice raises on the second call, not inside Rust."""
        db_path = str(tmp_path / "db")
        g = Graph(db_path)
        txn = g.begin()
        txn.commit()

        with pytest.raises(RuntimeError, match="TxnSession is already closed"):
            txn.commit()

        g.close()
