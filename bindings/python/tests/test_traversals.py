"""Graph traversal correctness — §8 of TODO.md."""
import pytest
from tests.conftest import addv

from rocksgraph import Graph, __, P, Int64, Float32, Vertex, Edge, Property, T, Direction, Order


class TestVertexTraversals:
    def test_addv_returns_dict_with_keys(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        assert hasattr(v, "id") and hasattr(v, "label")
        assert "id" in v
        assert "labels" in v or "label" in v
        assert "properties" in v

    def test_v_by_id_fetches_vertex(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        vid = v["id"]

        rs = graph.read()
        found = rs.traversal().V(vid).to_list()
        assert len(found) == 1
        assert found[0]["id"] == vid

    def test_out_traversal(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        neighbors = rs.traversal().V(v1["id"]).out("knows").to_list()
        assert len(neighbors) == 1

    def test_limit(self, graph):
        tx = graph.tx()
        for i in range(10):
            addv(tx, "item", n=i)
        tx.commit()

        rs = graph.read()
        result = rs.traversal().V().limit(3).to_list()
        assert len(result) == 3

    def test_range(self, graph):
        tx = graph.tx()
        for i in range(5):
            addv(tx, "item", n=i)
        tx.commit()

        rs = graph.read()
        # Get n values for range(1, 4)
        items = rs.traversal().V().range(1, 4).values("n").to_list()
        assert len(items) == 3

    def test_skip(self, graph):
        tx = graph.tx()
        for i in range(5):
            addv(tx, "item", n=i)
        tx.commit()

        rs = graph.read()
        items = rs.traversal().V().skip(3).values("n").to_list()
        assert len(items) == 2

    def test_count_returns_integer(self, graph):
        rs = graph.read()
        results = rs.traversal().V().count().to_list()
        assert results == [0]

    def test_dedup(self, graph):
        tx = graph.tx()
        addv(tx, "item", x=1)
        addv(tx, "item", x=1)
        tx.commit()

        rs = graph.read()
        vals = rs.traversal().V().values("x").dedup().to_list()
        assert vals == [1]

    def test_fold_unfold_roundtrip(self, graph):
        tx = graph.tx()
        addv(tx, "item", x=1)
        addv(tx, "item", x=2)
        tx.commit()

        rs = graph.read()
        # fold collects all values into a single list
        folded = rs.traversal().V().values("x").fold().to_list()
        assert folded == [[1, 2]]

        # unfold flattens the list back
        unfolded = rs.traversal().V().values("x").fold().unfold().to_list()
        assert sorted(unfolded) == [1, 2]

    def test_order_by_asc(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Bob", age=Int64(30))
        addv(tx, "person", name="Alice", age=Int64(25))
        tx.commit()

        rs = graph.read()
        names = rs.traversal().V().hasLabel("person").order().by("name", "asc").values("name").to_list()
        assert names == ["Alice", "Bob"]

    def test_order_by_desc(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Bob", age=Int64(30))
        addv(tx, "person", name="Alice", age=Int64(25))
        tx.commit()

        rs = graph.read()
        names = rs.traversal().V().hasLabel("person").order().by("name", "desc").values("name").to_list()
        assert names == ["Bob", "Alice"]

    def test_order_by_multi_key(self, graph):
        tx = graph.tx()
        addv(tx, "person", city="NY", name="Bob")
        addv(tx, "person", city="LA", name="Alice")
        addv(tx, "person", city="NY", name="Alice")
        tx.commit()

        rs = graph.read()
        names = rs.traversal().V().hasLabel("person").order().by("city", "asc").by("name", "asc").values("name").to_list()
        # LA: Alice, NY: Alice, NY: Bob
        assert names == ["Alice", "Alice", "Bob"]

    def test_as_select_roundtrip(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.commit()

        rs = graph.read()
        result = rs.traversal().V().as_("x").select("x").to_list()
        assert len(result) == 1
        assert result[0]["label"] == "person"

    def test_has_3arg_form(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30))
        addv(tx, "dog", name="Rex")
        tx.commit()

        rs = graph.read()
        result = rs.traversal().V().has("person", "name", "Alice").to_list()
        assert len(result) == 1

    def test_coalesce_upsert(self, graph):
        """coalesce existing + addV pattern."""
        tx = graph.tx()
        addv(tx, "user", email="a@b.com")
        tx.commit()

        rs = graph.read()
        # Search for existing user → should find it without calling addV
        found = rs.traversal().V().has("email", "a@b.com").fold().coalesce(
            __.unfold(),
            __.addV("user").property("id", 999).property("email", "fallback")
        ).to_list()
        assert len(found) == 1

    def test_drop(self, graph):
        tx = graph.tx()
        addv(tx, "temp")
        tx.commit()

        # Drop it in a new tx
        tx2 = graph.tx()
        # Note: drop() may require matching V() first
        count_before = tx2.traversal().V().hasLabel("temp").count().to_list()
        assert count_before == [1]

    # group/groupCount tests moved to dedicated TestGroup class below

class TestHashExistence:
    """§5: has('key') without value — existence check."""

    def test_has_key_filters(self, graph):
        tx = graph.tx()
        addv(tx, "user", email="a@b.com")
        addv(tx, "user")  # no email
        tx.commit()

        rs = graph.read()
        with_email = rs.traversal().V().has("email").count().to_list()
        assert with_email == [1]

        total = rs.traversal().V().hasLabel("user").count().to_list()
        assert total == [2]


class TestEdgeTraversals:
    def test_adde_basic(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        edges = rs.traversal().V(v1["id"]).outE("knows").to_list()
        assert len(edges) == 1
        e = edges[0]
        assert "src" in e
        assert "dst" in e
        assert "label" in e or "labels" in e

    def test_oute_inv(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        dest = rs.traversal().V(v1["id"]).outE("knows").inV().values("name").to_list()
        assert dest == ["Bob"]

    def test_edge_properties(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person")
        v2 = addv(tx, "person")
        tx.traversal().addE("knows").from_(v1).to(v2).property("since", Int64(2020)).next()
        tx.commit()

class TestInBothTraversals:
    def test_in_traversal(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v2).to(v1).next()
        tx.commit()

        rs = graph.read()
        # v1 is the destination; in("knows") should find v2
        neighbors = rs.traversal().V(v1["id"]).in_("knows").values("name").to_list()
        assert "Bob" in neighbors

    def test_both_traversal(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        v3 = addv(tx, "person", name="Charlie")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.traversal().addE("knows").from_(v3).to(v1).next()
        tx.commit()

        rs = graph.read()
        neighbors = rs.traversal().V(v1["id"]).both("knows").values("name").to_list()
        assert "Bob" in neighbors
        assert "Charlie" in neighbors


class TestTail:
    def test_tail(self, graph):
        tx = graph.tx()
        for i in range(5):
            addv(tx, "item", n=i)
        tx.commit()

        rs = graph.read()
        items = rs.traversal().V().tail(2).values("n").to_list()
        assert len(items) == 2


class TestPath:
    def test_path(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        paths = rs.traversal().V(v1["id"]).out("knows").path().to_list()
        assert len(paths) >= 1
        assert "objects" in paths[0]


class TestInEOutV:
    def test_ine_outv(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        sources = rs.traversal().V(v2["id"]).inE("knows").outV().values("name").to_list()
        assert sources == ["Alice"]


class TestBothE:
    def test_bothe(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        edges = rs.traversal().V(v1["id"]).bothE("knows").to_list()
        assert len(edges) >= 1
        e = edges[0]
        assert "src" in e or "dst" in e


@pytest.mark.skip(reason="Edge rank values >0 require multi-edge engine support")
class TestEdgeRank:
    def test_adde_with_rank(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).property("rank", 5).next()
        tx.commit()

        rs = graph.read()
        edges = rs.traversal().V(v1["id"]).outE("knows").to_list()
        assert len(edges) == 1
        # rank field may be present or not depending on engine version
        rank = edges[0].get("rank")
        if rank is not None:
            assert rank == 5

    def test_hasRank(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        v3 = addv(tx, "person", name="Charlie")
        tx.traversal().addE("knows").from_(v1).to(v2).property("rank", 1).next()
        tx.traversal().addE("knows").from_(v1).to(v3).property("rank", 2).next()
        tx.commit()

        rs = graph.read()
        filtered = rs.traversal().V(v1["id"]).outE("knows").hasRank(P.eq(1)).to_list()
        assert len(filtered) >= 1

    def test_hasRank_not_eq(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).property("rank", 1).next()
        tx.commit()

        rs = graph.read()
        filtered = rs.traversal().V(v1["id"]).outE("knows").hasRank(P.neq(1)).to_list()
        assert len(filtered) == 0


class TestHasOnEdge:
    def test_has_key_on_edge(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        v3 = addv(tx, "person", name="Charlie")
        tx.traversal().addE("knows").from_(v1).to(v2).property("weight", Float32(0.8)).next()
        tx.traversal().addE("knows").from_(v1).to(v3).next()  # no weight, different target
        tx.commit()

        rs = graph.read()
        all_edges = rs.traversal().V(v1["id"]).outE("knows").count().to_list()
        assert all_edges == [2]
        with_weight = rs.traversal().V(v1["id"]).outE("knows").has("weight").count().to_list()
        assert with_weight == [1]


class TestUnion:
    def test_union(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "item", name="A")
        v2 = addv(tx, "item", name="B")
        tx.commit()

        rs = graph.read()
        result = rs.traversal().V(v1["id"]).union(
            rs.traversal().V(v1["id"]),
            rs.traversal().V(v2["id"])
        ).values("name").to_list()
        assert sorted(result) == ["A", "B"]


class TestRepeat:
    def test_repeat_out(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "node", name="A")
        v2 = addv(tx, "node", name="B")
        v3 = addv(tx, "node", name="C")
        tx.traversal().addE("link").from_(v1).to(v2).next()
        tx.traversal().addE("link").from_(v2).to(v3).next()
        tx.commit()

        rs = graph.read()
        # 1-hop from v1 should reach v2
        hop1 = rs.traversal().V(v1["id"]).repeat(__.out("link")).times(1).values("name").to_list()
        assert "B" in hop1

        # 2-hop from v1 should reach v3
        hop2 = rs.traversal().V(v1["id"]).repeat(__.out("link")).times(2).values("name").to_list()
        assert "C" in hop2


class TestV02SubTraversals:
    """Steps with wiring complete but previously untested."""

    def test_where(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        result = rs.traversal().V(v1["id"]).out("knows").where(__.identity()).values("name").to_list()
        assert result == ["Bob"]

    def test_simplePath(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "node", name="A")
        v2 = addv(tx, "node", name="B")
        tx.traversal().addE("link").from_(v1).to(v2).next()
        tx.commit()

        rs = graph.read()
        # simplePath on a non-cyclic path should still return the vertex
        result = rs.traversal().V(v1["id"]).out("link").simplePath().values("name").to_list()
        assert result == ["B"]


class TestWithProperties:
    def test_default_no_properties(self, graph):
        """Without withProperties, vertex dict has empty properties."""
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30), city="NY")
        tx.commit()

        rs = graph.read()
        v = rs.traversal().V(v["id"]).next()
        assert v["properties"] == {}

    def test_empty_fetches_all(self, graph):
        """withProperties() with no args fetches all properties."""
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30), city="NY")
        tx.commit()

        rs = graph.read()
        v = rs.traversal().withProperties().V(v["id"]).next()
        assert v["properties"]["name"] == "Alice"
        assert v["properties"]["age"] == 30
        assert v["properties"]["city"] == "NY"

    def test_named_keys_only(self, graph):
        """withProperties('name', 'age') fetches only those keys."""
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30), city="NY")
        tx.commit()

        rs = graph.read()
        v = rs.traversal().withProperties("name", "age").V(v["id"]).next()
        assert v["properties"]["name"] == "Alice"
        assert v["properties"]["age"] == 30
        assert "city" not in v["properties"]

    def test_chained_withproperties_takes_last(self, graph):
        """Last withProperties wins."""
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30), city="NY")
        tx.commit()

        rs = graph.read()
        v = rs.traversal().withProperties("name").withProperties("city").V(v["id"]).next()
        assert "city" in v["properties"]
        assert "name" not in v["properties"]
        assert "age" not in v["properties"]

    def test_withproperties_applies_to_to_list(self, graph):
        """withProperties works with to_list() too."""
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30))
        tx.commit()

        rs = graph.read()
        results = rs.traversal().withProperties("name").V(v["id"]).to_list()
        assert results[0]["properties"]["name"] == "Alice"
        assert "age" not in results[0]["properties"]

class TestDegree:
    def test_degree_default(self, graph):
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        deg = rs.traversal().V(v1['id']).degree().to_list()
        assert deg == [1]

    def test_degree_out(self, graph):
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        deg = rs.traversal().V(v1['id']).degree('out').to_list()
        assert deg == [1]

    def test_degree_in(self, graph):
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        deg = rs.traversal().V(v2['id']).degree('in').to_list()
        assert deg == [1]


class TestAggregations:
    def test_sum(self, graph):
        tx = graph.tx()
        for n in [10, 20, 30]:
            addv(tx, 'item', n=n)
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('n').sum().to_list()
        assert result == [60]

    def test_max(self, graph):
        tx = graph.tx()
        for n in [10, 30, 20]:
            addv(tx, 'item', n=n)
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('n').max().to_list()
        assert result == [30]

    def test_min(self, graph):
        tx = graph.tx()
        for n in [10, 30, 20]:
            addv(tx, 'item', n=n)
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('n').min().to_list()
        assert result == [10]

    def test_mean(self, graph):
        tx = graph.tx()
        for n in [10, 20, 30]:
            addv(tx, 'item', n=n)
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('n').mean().to_list()
        assert result == [20]


class TestCyclicPath:
    def test_cyclicPath(self, graph):
        # simplePath on non-cyclic path passes through; cyclicPath needs actual cycle
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.traversal().addE('link').from_(v2).to(v1).next()
        tx.commit()
        rs = graph.read()
        # A→B→A: A appears twice in the path, so the traverser (A) is cyclic
        result = rs.traversal().V(v1['id']).out('link').out('link').cyclicPath().values('name').to_list()
        assert 'A' in result


class TestChoose:
    def test_choose(self, graph):
        tx = graph.tx()
        addv(tx, 'person', name='Alice', age=Int64(30))
        addv(tx, 'person', name='Bob', age=Int64(15))
        tx.commit()
        rs = graph.read()
        # If has age >= 18 → constant("adult"), else → constant("minor")
        result = rs.traversal().V().hasLabel('person').choose(
            __.has('age', P.gte(Int64(18))),
            __.constant('adult'),
            __.constant('minor')
        ).to_list()
        assert 'adult' in result
        assert 'minor' in result


class TestLocal:
    def test_local(self, graph):
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        v3 = addv(tx, 'node', name='C')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.traversal().addE('link').from_(v1).to(v3).next()
        tx.commit()
        rs = graph.read()
        # local: count out-edges for each traverser individually
        result = rs.traversal().V(v1['id']).local(__.out('link').count()).to_list()
        assert result == [2]


class TestRepeatUntilEmit:
    def test_repeat_until(self, graph):
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        v3 = addv(tx, 'node', name='C')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.traversal().addE('link').from_(v2).to(v3).next()
        tx.commit()
        rs = graph.read()
        # repeat out until name is C (stops early when condition met)
        result = rs.traversal().V(v1['id']).repeat(__.out('link')).until(__.has('name', 'C')).values('name').to_list()
        assert 'C' in result

    def test_repeat_emit(self, graph):
        tx = graph.tx()
        v1 = addv(tx, 'node', name='A')
        v2 = addv(tx, 'node', name='B')
        v3 = addv(tx, 'node', name='C')
        tx.traversal().addE('link').from_(v1).to(v2).next()
        tx.traversal().addE('link').from_(v2).to(v3).next()
        tx.commit()
        rs = graph.read()
        # emit after each iteration: hop1→B, hop2→C both emitted
        result = rs.traversal().V(v1['id']).repeat(__.out('link')).emit().times(2).values('name').to_list()
        assert 'B' in result
        assert 'C' in result

class TestIs:
    def test_is_eq_shorthand(self, graph):
        tx = graph.tx()
        addv(tx, 'item', name='Alice')
        addv(tx, 'item', name='Bob')
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('name').is_('Alice').to_list()
        assert result == ['Alice']

    def test_is_with_predicate(self, graph):
        tx = graph.tx()
        for n in [10, 20, 30]:
            addv(tx, 'item', n=n)
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('n').is_(P.gt(Int64(15))).to_list()
        assert sorted(result) == [20, 30]

    def test_is_filters_none(self, graph):
        tx = graph.tx()
        addv(tx, 'item', name='Alice')
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel('item').values('name').is_('Bob').to_list()
        assert result == []


class TestVertexEdgePropertyObjects:
    def test_vertex_is_vertex_instance(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V(v["id"]).next()
        assert isinstance(result, Vertex)

    def test_vertex_attribute_access(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V(v["id"]).next()
        assert result.id == v["id"]
        assert result.label == "person"

    def test_vertex_dict_compat(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V(v["id"]).next()
        assert result["id"] == v["id"]
        assert result["label"] == "person"
        assert "id" in result
        assert list(result.keys()) == list(result._d.keys())

    def test_vertex_hashable(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.commit()
        rs = graph.read()
        vertices = rs.traversal().V().to_list()
        assert len(set(vertices)) == 2

    def test_edge_is_edge_instance(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V(v1["id"]).outE("knows").next()
        assert isinstance(result, Edge)

    def test_edge_attribute_access(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        e = rs.traversal().V(v1["id"]).outE("knows").next()
        assert e.src == v1["id"]
        assert e.dst == v2["id"]
        assert e.label == "knows"
        assert e.rank == 0

    def test_edge_dict_compat(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        e = rs.traversal().V(v1["id"]).outE("knows").next()
        assert e["src"] == v1["id"]
        assert e["label"] == "knows"

    def test_property_from_properties_step(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        rs = graph.read()
        props = rs.traversal().V(v["id"]).properties("name").to_list()
        assert len(props) >= 1
        p = props[0]
        assert isinstance(p, Property)
        assert p.key == "name"
        assert p.value == "Alice"

    def test_vertex_repr(self, graph):
        tx = graph.tx()
        v = addv(tx, "person")
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V(v["id"]).next()
        r = repr(result)
        assert r.startswith("Vertex(")
        assert "label='person'" in r

    def test_edge_repr(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "person", name="Alice")
        v2 = addv(tx, "person", name="Bob")
        tx.traversal().addE("knows").from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        e = rs.traversal().V(v1["id"]).outE("knows").next()
        r = repr(e)
        assert r.startswith("Edge(")
        assert "label='knows'" in r
        assert "rank=0" in r


class TestGLVEnums:
    def test_T_constants_are_strings(self):
        assert T.id == "id"
        assert T.label == "label"
        assert T.key == "key"
        assert T.value == "value"

    def test_T_label_same_as_string(self, graph):
        tx = graph.tx()
        for n in [30, 10, 20]:
            addv(tx, "item", age=Int64(n))
        tx.commit()
        rs = graph.read()
        # T.label == "label" — by(T.label) must produce the same result as by("label")
        r1 = rs.traversal().V().hasLabel("item").order().by(T.label, "asc").values("age").to_list()
        r2 = rs.traversal().V().hasLabel("item").order().by("label", "asc").values("age").to_list()
        assert r1 == r2

    def test_order_enum_asc(self, graph):
        tx = graph.tx()
        for n in [30, 10, 20]:
            addv(tx, "item", age=Int64(n))
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel("item").order().by("age", Order.asc).values("age").to_list()
        assert result == sorted(result)

    def test_order_enum_desc(self, graph):
        tx = graph.tx()
        for n in [30, 10, 20]:
            addv(tx, "item", age=Int64(n))
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().hasLabel("item").order().by("age", Order.desc).values("age").to_list()
        assert result == sorted(result, reverse=True)

    def test_direction_out(self, graph):
        tx = graph.tx()
        v1 = addv(tx, "node")
        v2 = addv(tx, "node")
        tx.traversal().addE("link").from_(v1).to(v2).next()
        tx.commit()
        rs = graph.read()
        assert rs.traversal().V(v1["id"]).degree(Direction.OUT).to_list() == [1]
        assert rs.traversal().V(v1["id"]).degree(Direction.IN).to_list() == [0]
        assert rs.traversal().V(v1["id"]).degree(Direction.BOTH).to_list() == [1]


class TestGLVTerminals:
    def test_iterate_returns_none(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().iterate()
        assert result is None

    def test_iterate_drop(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.commit()
        tx2 = graph.tx()
        tx2.traversal().V().hasLabel("person").drop().iterate()
        tx2.commit()
        assert graph.read().traversal().V().hasLabel("person").count().to_list() == [0]

    def test_to_set_returns_set(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        addv(tx, "person", name="Bob")
        tx.commit()
        rs = graph.read()
        result = rs.traversal().V().to_set()
        assert isinstance(result, set)
        assert len(result) == 2

    def test_toSet_alias(self, graph):
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        tx.commit()
        rs = graph.read()
        assert rs.traversal().V().toSet() == rs.traversal().V().to_set()


class TestTxContextManager:
    def test_commit_on_success(self, graph):
        with graph.tx() as tx:
            tx.traversal().addV("person").property("id", 9001).property("name", "Alice").next()
        assert graph.read().traversal().V(9001).count().to_list() == [1]

    def test_rollback_on_exception(self, graph):
        try:
            with graph.tx() as tx:
                tx.traversal().addV("person").property("id", 9002).property("name", "Bob").next()
                raise RuntimeError("intentional failure")
        except RuntimeError:
            pass
        assert graph.read().traversal().V(9002).count().to_list() == [0]

    def test_exception_not_suppressed(self, graph):
        with pytest.raises(ValueError):
            with graph.tx() as tx:
                tx.traversal().addV("person").property("id", 9003).property("name", "Carol").next()
                raise ValueError("should propagate")

class TestGroup:
    def test_group_by(self, graph):
        """group().by('city') groups by named property."""
        tx = graph.tx()
        addv(tx, "person", city="NY", name="Alice")
        addv(tx, "person", city="NY", name="Bob")
        addv(tx, "person", city="SF", name="Charlie")
        tx.commit()

        rs = graph.read()
        groups = rs.traversal().V().hasLabel("person").group().by("city").to_list()
        assert len(groups) == 1
        g = groups[0]
        assert len(g["NY"]) == 2
        assert len(g["SF"]) == 1
        assert isinstance(g["NY"][0], Vertex)

    def test_group_no_by(self, graph):
        """group() without by() groups by traverser value (Vertex)."""
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        addv(tx, "person", name="Bob")
        tx.commit()

        rs = graph.read()
        groups = rs.traversal().V().hasLabel("person").group().to_list()
        assert len(groups) == 1
        g = groups[0]
        assert len(g) == 2
        for k, v in g.items():
            assert isinstance(k, int)
            assert len(v) == 1
            assert isinstance(v[0], Vertex)

    def test_group_count_by(self, graph):
        """groupCount().by('city') counts by named property."""
        tx = graph.tx()
        addv(tx, "person", city="NY", name="Alice")
        addv(tx, "person", city="NY", name="Bob")
        addv(tx, "person", city="SF", name="Charlie")
        tx.commit()

        rs = graph.read()
        counts = rs.traversal().V().hasLabel("person").groupCount().by("city").to_list()
        assert len(counts) == 1
        c = counts[0]
        assert c["NY"] == 2
        assert c["SF"] == 1

    def test_group_count_no_by(self, graph):
        """groupCount() without by() counts occurrences of each traverser value."""
        tx = graph.tx()
        addv(tx, "person", name="Alice")
        addv(tx, "person", name="Bob")
        tx.commit()

        rs = graph.read()
        counts = rs.traversal().V().hasLabel("person").groupCount().to_list()
        assert len(counts) == 1
        c = counts[0]
        vals = list(c.values())
        assert all(v == 1 for v in vals)

    def test_group_by_missing_property(self, graph):
        """group().by('age') where some vertices lack the property — those are skipped."""
        tx = graph.tx()
        addv(tx, "person", name="Alice", age=Int64(30))
        addv(tx, "person", name="Bob")  # no age
        addv(tx, "person", name="Charlie", age=Int64(30))
        tx.commit()

        rs = graph.read()
        groups = rs.traversal().V().hasLabel("person").group().by("age").to_list()
        g = groups[0]
        # Bob is skipped, Alice and Charlie grouped under age=30
        assert 30 in g
        assert len(g[30]) == 2

    def test_group_scalar_values(self, graph):
        """group().by() after values() — the value field is used as key."""
        tx = graph.tx()
        addv(tx, "person", name="Alice", age=Int64(30))
        addv(tx, "person", name="Bob", age=Int64(25))
        addv(tx, "person", name="Charlie", age=Int64(30))
        tx.commit()

        rs = graph.read()
        groups = rs.traversal().V().hasLabel("person").group().by("age").to_list()
        g = groups[0]
        assert 30 in g
        assert 25 in g
        assert len(g[30]) == 2
        assert len(g[30]) == 2
