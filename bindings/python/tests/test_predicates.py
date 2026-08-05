"""Predicate correctness — §4 of TODO.md."""
from tests.conftest import addv
from rocksgraph import P, Int64

def insert_ages(graph):
    """Insert vertices with ages [10, 20, 30, 40, 50], commit, return read session."""
    tx = graph.begin()
    for age in [10, 20, 30, 40, 50]:
        addv(tx, "person", age=Int64(age))
    tx.commit()
    return graph.read()


class TestPredicates:
    def test_gt(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.gt(Int64(20))).values("age").to_list()
        assert sorted(res) == [30, 40, 50]

    def test_gte(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.gte(Int64(20))).values("age").to_list()
        assert sorted(res) == [20, 30, 40, 50]

    def test_lt(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.lt(Int64(30))).values("age").to_list()
        assert sorted(res) == [10, 20]

    def test_lte(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.lte(Int64(30))).values("age").to_list()
        assert sorted(res) == [10, 20, 30]

    def test_between(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.between(Int64(20), Int64(40))).values("age").to_list()
        assert sorted(res) == [20, 30]

    def test_within(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.within(Int64(10), Int64(50))).values("age").to_list()
        assert sorted(res) == [10, 50]

    def test_without(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.without(Int64(10), Int64(50))).values("age").to_list()
        assert sorted(res) == [20, 30, 40]

    def test_neq(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.neq(Int64(30))).values("age").to_list()
        assert sorted(res) == [10, 20, 40, 50]

    def test_eq(self, graph):
        rs = insert_ages(graph)
        res = rs.g().V().has("age", P.eq(Int64(30))).values("age").to_list()
        assert res == [30]
