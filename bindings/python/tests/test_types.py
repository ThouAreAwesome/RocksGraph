"""Property type round-trip tests — write, commit, read, compare."""
from tests.conftest import addv
from rocksgraph import Graph, Int32, Int64, UInt16, Float32, Float64, Uuid

def assert_roundtrip(graph, key, value, checker=None):
    """Write a vertex with property(key, value), commit, read it back."""
    txn = graph.begin()
    addv(txn, "test", **{key: value})
    txn.commit()

    rs = graph.read()
    results = rs.g().V().hasLabel("test").values(key).to_list()
    assert len(results) >= 1, f"No results for key={key}"
    got = results[0]
    if checker:
        checker(got)
    else:
        # Type wrappers round-trip as plain Python types; compare .value if applicable
        expected = value.value if hasattr(value, 'value') else value
        assert got == expected, f"expected {expected!r}, got {got!r}"


class TestPrimitiveTypes:
    def test_int32_positive(self, graph):
        assert_roundtrip(graph, "n", Int32(42))

    def test_int32_negative(self, graph):
        assert_roundtrip(graph, "n", Int32(-42))

    def test_int32_min(self, graph):
        assert_roundtrip(graph, "n", Int32(-(2**31)))

    def test_int32_max(self, graph):
        assert_roundtrip(graph, "n", Int32(2**31 - 1))

    def test_int64_min(self, graph):
        assert_roundtrip(graph, "n", Int64(-(2**63)))

    def test_int64_max(self, graph):
        assert_roundtrip(graph, "n", Int64(2**63 - 1))

    def test_uint16_zero(self, graph):
        assert_roundtrip(graph, "n", UInt16(0))

    def test_uint16_max(self, graph):
        assert_roundtrip(graph, "n", UInt16(65535))

    def test_float32(self, graph):
        def check(got):
            assert abs(got - 3.14) < 1e-6
        # Float32(3.14) may round-trip as f32 → f64, so use epsilon
        assert_roundtrip(graph, "f", Float32(3.14), check)

    def test_float64(self, graph):
        assert_roundtrip(graph, "f", Float64(1e300))

    def test_bool_true(self, graph):
        assert_roundtrip(graph, "flag", True)

    def test_bool_false(self, graph):
        assert_roundtrip(graph, "flag", False)

    def test_str_ascii(self, graph):
        assert_roundtrip(graph, "name", "Alice")

    def test_str_utf8(self, graph):
        assert_roundtrip(graph, "name", "こんにちは")

    def test_bytes(self, graph):
        assert_roundtrip(graph, "data", b"\x00\xff\x01\xfe")

    def test_uuid(self, graph):
        uid = Uuid("550e8400-e29b-41d4-a716-446655440000")
        def check(got):
            assert "550e8400" in str(got)
        assert_roundtrip(graph, "uid", uid, check)

    def test_plain_int_roundtrip_as_int64(self, graph):
        assert_roundtrip(graph, "n", 42)

    def test_plain_float_roundtrip_as_float64(self, graph):
        assert_roundtrip(graph, "f", 3.14)


class TestHasExistence:
    """§5: has("key") — property existence check."""

    def test_has_key_matches_vertex_with_property(self, graph):
        txn = graph.begin()
        addv(txn, "user", email="a@b.com")
        addv(txn, "user")  # no email
        txn.commit()

        rs = graph.read()
        with_email = rs.g().V().has("email").count().to_list()
        assert with_email == [1]

    def test_has_key_does_not_match_vertex_without_property(self, graph):
        txn = graph.begin()
        addv(txn, "user", email="a@b.com")
        addv(txn, "user")
        txn.commit()

        rs = graph.read()
        total = rs.g().V().count().to_list()
        with_email = rs.g().V().has("email").count().to_list()
        assert total == [2]
        assert with_email == [1]
