import pytest, os, shutil, itertools
import tempfile

@pytest.fixture
def tmpdir():
    """A temporary directory that is cleaned up after the test."""
    path = tempfile.mkdtemp()
    yield path
    try:
        shutil.rmtree(path)
    except PermissionError:
        pass  # Windows: RocksDB LOCK file may still be held

@pytest.fixture
def graph(tmpdir):
    """A fresh Graph instance on a temp directory."""
    from rocksgraph import Graph
    g = Graph(tmpdir)
    yield g
    g.close()

_id_counter = itertools.count(1)


def addv(tx, label, **properties):
    """Add a vertex with an auto-generated id. Returns the vertex dict."""
    vid = next(_id_counter)
    t = tx.g().addV(label).property("id", vid)
    for k, v in properties.items():
        t = t.property(k, v)
    return t.next()
