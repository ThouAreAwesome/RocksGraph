"""Structured StoreError → Python exception hierarchy.

Verifies that Rust-side `StoreError` variants surface as the matching
`rocksgraph.StoreError` subclass (per `StoreError::category()`), not a flat
`RuntimeError`, and that the hierarchy itself is catchable at any level.
"""
import pytest

from rocksgraph import (
    Graph,
    IntegrityError,
    QueryError,
    SchemaError,
    StoreError,
)
from tests.conftest import addv


def test_hierarchy_shape():
    assert issubclass(SchemaError, StoreError)
    assert issubclass(IntegrityError, StoreError)
    assert issubclass(QueryError, StoreError)
    assert issubclass(StoreError, Exception)
    assert not issubclass(StoreError, RuntimeError)


def test_duplicate_vertex_raises_integrity_error(graph):
    txn = graph.begin()
    txn.g().addV("person").property("id", 1).next()
    with pytest.raises(IntegrityError):
        txn.g().addV("person").property("id", 1).next()
    txn.rollback()


def test_missing_vertex_id_raises_query_error(graph):
    txn = graph.begin()
    with pytest.raises(QueryError):
        txn.g().addV("person").next()
    txn.rollback()


def test_store_error_catches_any_subclass(graph):
    """A caller that only wants to know "did a store operation fail" can
    catch the base class without enumerating every subclass."""
    txn = graph.begin()
    txn.g().addV("person").property("id", 1).next()
    with pytest.raises(StoreError):
        txn.g().addV("person").property("id", 1).next()
    txn.rollback()
