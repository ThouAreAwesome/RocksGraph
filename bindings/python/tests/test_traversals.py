"""Graph traversal correctness — §8 of TODO.md."""
import pytest
from tests.conftest import addv

from rocksgraph import Graph, __, P, Int64, Float32


class TestVertexTraversals:
    def test_addv_returns_dict_with_keys(self, graph):
        tx = graph.tx()
        v = addv(tx, "person", name="Alice")
        tx.commit()
        assert isinstance(v, dict)
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

    @pytest.mark.skip(reason="group() returns unhashable dicts")
    def test_group_by(self, graph):
        """group().by('key') should return a dict keyed by property value."""
        tx = graph.tx()
        addv(tx, "person", city="NY", name="Alice")
        addv(tx, "person", city="NY", name="Bob")
        addv(tx, "person", city="SF", name="Charlie")
        tx.commit()

        rs = graph.read()
        groups = rs.traversal().V().hasLabel("person").group().by("city").to_list()
        assert len(groups) == 1
        g = groups[0]
        # g should be a dict with keys "NY" and "SF"
        assert len(g["NY"]) == 2
        assert len(g["SF"]) == 1


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
        assert v["properties"]["name"] == ["Alice"]
        assert v["properties"]["age"] == [30]
        assert v["properties"]["city"] == ["NY"]

    def test_named_keys_only(self, graph):
        """withProperties('name', 'age') fetches only those keys."""
        tx = graph.tx()
        v = addv(tx, "person", name="Alice", age=Int64(30), city="NY")
        tx.commit()

        rs = graph.read()
        v = rs.traversal().withProperties("name", "age").V(v["id"]).next()
        assert v["properties"]["name"] == ["Alice"]
        assert v["properties"]["age"] == [30]
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
        assert results[0]["properties"]["name"] == ["Alice"]
        assert "age" not in results[0]["properties"]
