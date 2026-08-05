import pytest
from rocksgraph import Graph, Vector

class TestVectorSearch:
    def test_store_and_retrieve_vector_values(self, graph):
        tx = graph.begin()
        tx.g().addV("doc").property("id", 1) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        tx.g().addV("doc").property("id", 2) \
            .property("emb", Vector([0.7, 0.7, 0.0])).next()
        tx.commit()

        snap = graph.read()
        v1 = snap.g().V(1).values("emb").next()
        assert v1 is not None, "Should retrieve stored vector value"
        assert isinstance(v1, list), f"Values response must be list, got {type(v1)}"
        assert len(v1) == 3
        assert abs(v1[0] - 0.1) < 1e-6

    def test_nearest_exact_knn(self, graph):
        tx = graph.begin()
        for i in range(10):
            tx.g().addV("doc").property("id", i) \
                .property("emb", Vector([float(i) / 10.0, float(9 - i) / 10.0])).next()
        tx.commit()

        snap = graph.read()
        results = snap.g().V().hasLabel("doc") \
            .nearest("emb", Vector([0.9, 0.0]), 3).to_list()
        assert len(results) == 3, f"Expected 3 results, got {len(results)}"
        ids = [v["id"] for v in results]
        assert 9 in ids, f"Vertex 9 (exact match) should be in top-3, got {ids}"

    def test_nearest_empty_graph(self, graph):
        snap = graph.read()
        results = snap.g().V().hasLabel("doc") \
            .nearest("emb", Vector([1.0, 2.0]), 5).to_list()
        assert results == []

    def test_cosine_similarity(self, graph):
        tx = graph.begin()
        tx.g().addV("doc").property("id", 1) \
            .property("emb", Vector([1.0, 0.0])).next()
        tx.commit()

        snap = graph.read()
        scores = snap.g().V(1) \
            .similarity("emb", Vector([1.0, 0.0])).to_list()
        assert len(scores) == 1
        assert abs(scores[0] - 1.0) < 1e-6

        scores2 = snap.g().V(1) \
            .similarity("emb", Vector([0.0, 1.0])).to_list()
        assert len(scores2) == 1
        assert abs(scores2[0] - 0.0) < 1e-6

    def test_floatvector_codec_roundtrip(self):
        """Python Vector → encode → decode → back (no DB needed)."""
        from rocksgraph._codec import _encode_primitive, PRIM_FLOATVECTOR
        import struct

        original = Vector([1.0, -2.5, 3.14, 0.0])
        buf = bytearray()
        _encode_primitive(original, buf)

        # Verify tag
        assert buf[0] == PRIM_FLOATVECTOR
        # Verify dimension
        dim = struct.unpack(">I", buf[1:5])[0]
        assert dim == 4
        # Decode LE f32 values
        decoded = list(struct.unpack(f"<{dim}f", buf[5:5 + dim * 4]))
        # f32 precision: 3.14 becomes 3.140000104904175
        for a, b in zip(decoded, original.values):
            assert abs(a - b) < 1e-5, f"{a} != {b} within f32 epsilon"
        assert abs(decoded[0] - 1.0) < 1e-6
        assert abs(decoded[1] + 2.5) < 1e-6

    def test_floatvector_hash_dedup(self, graph):
        """Two vertices with identical vector properties are equal."""
        tx = graph.begin()
        tx.g().addV("doc").property("id", 1) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        tx.g().addV("doc").property("id", 2) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        tx.commit()

        snap = graph.read()
        v1 = snap.g().V(1).values("emb").next()
        v2 = snap.g().V(2).values("emb").next()
        assert v1 == v2, "Identical FloatVectors should be equal"

    def test_vector_type_coercion(self):
        """Plain lists are auto-wrapped to Vector in nearest/similarity."""
        from rocksgraph._codec import _encode_step, OP_NEAREST
        buf = bytearray()
        # Passing a plain list should not crash — it's auto-converted to Vector
        _encode_step(OP_NEAREST, ("emb", [1.0, 2.0, 3.0], 5, None), buf)
        assert len(buf) > 0, "Encoding should succeed with auto-coerced list"

    def test_anonymous_traversal_vector_steps(self):
        """__.nearest and __.similarity produce valid anonymous traversals."""
        from rocksgraph import __
        from rocksgraph._codec import OP_NEAREST, OP_SIMILARITY
        t_near = __.nearest("emb", Vector([1.0, 2.0]), 5)
        assert len(t_near.steps) == 1
        assert t_near.steps[0][0] == OP_NEAREST

        t_sim = __.similarity("emb", Vector([1.0, 2.0]))
        assert len(t_sim.steps) == 1
        assert t_sim.steps[0][0] == OP_SIMILARITY

    def test_vector_type_error(self, graph):
        """Passing non-vector type to nearest raises ValueError."""
        snap = graph.read()
        # An integer is not iterable → Vector() raises TypeError
        with pytest.raises(TypeError):
            snap.g().V().nearest("emb", 42, 3).to_list()

    def test_nearest_top_k_ordering(self, graph):
        """nearest returns correct top-k in descending similarity order."""
        tx = graph.begin()
        # Non-collinear 2D vectors so cosine similarity differs meaningfully
        vectors = [(0.0, 1.0), (0.7, 0.7), (1.0, 0.0), (0.3, 0.95), (0.9, 0.4)]
        for i, (x, y) in enumerate(vectors):
            tx.g().addV("doc").property("id", i) \
                .property("emb", Vector([x, y])).next()
        tx.commit()

        snap = graph.read()
        # Query with [1.0, 0.0] — id=2 [1.0, 0.0] is exact, id=4 [0.9, 0.4] next
        results = (
            snap.g().V().hasLabel("doc")
            .nearest("emb", Vector([1.0, 0.0]), 2)
            .to_list()
        )
        assert len(results) == 2
        top_ids = [v["id"] for v in results]
        assert top_ids[0] == 2, f"Best match should be id=2, got {top_ids}"
        assert top_ids[1] == 4, f"Second best should be id=4, got {top_ids}"
