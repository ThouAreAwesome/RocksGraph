import pytest
from rocksgraph import Graph, Vector

class TestVectorSearch:
    def test_store_and_retrieve_vector_values(self, graph):
        txn = graph.begin()
        txn.g().addV("doc").property("id", 1) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        txn.g().addV("doc").property("id", 2) \
            .property("emb", Vector([0.7, 0.7, 0.0])).next()
        txn.commit()

        snap = graph.read()
        v1 = snap.g().V(1).values("emb").next()
        assert v1 is not None, "Should retrieve stored vector value"
        assert isinstance(v1, list), f"Values response must be list, got {type(v1)}"
        assert len(v1) == 3
        assert abs(v1[0] - 0.1) < 1e-6

    def test_nearest_exact_knn(self, graph):
        txn = graph.begin()
        for i in range(10):
            txn.g().addV("doc").property("id", i) \
                .property("emb", Vector([float(i) / 10.0, float(9 - i) / 10.0])).next()
        txn.commit()

        snap = graph.read()
        results = snap.g().V().nearest("emb", Vector([0.9, 0.0]), 3).hasLabel("doc").to_list()
        assert len(results) == 3, f"Expected 3 results, got {len(results)}"
        ids = [v["id"] for v in results]
        assert 9 in ids, f"Vertex 9 (exact match) should be in top-3, got {ids}"

    def test_nearest_empty_graph(self, graph):
        snap = graph.read()
        results = snap.g().V().nearest("emb", Vector([1.0, 2.0]), 5).hasLabel("doc").to_list()
        assert results == []

    def test_cosine_similarity(self, graph):
        txn = graph.begin()
        txn.g().addV("doc").property("id", 1) \
            .property("emb", Vector([1.0, 0.0])).next()
        txn.commit()

        from rocksgraph import DistanceMetric
        snap = graph.read()
        scores = snap.g().V(1) \
            .similarity("emb", Vector([1.0, 0.0]), DistanceMetric.Cosine).to_list()
        assert len(scores) == 1
        assert abs(scores[0] - 1.0) < 1e-6

        scores2 = snap.g().V(1) \
            .similarity("emb", Vector([0.0, 1.0]), DistanceMetric.Cosine).to_list()
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
        txn = graph.begin()
        txn.g().addV("doc").property("id", 1) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        txn.g().addV("doc").property("id", 2) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        txn.commit()

        snap = graph.read()
        v1 = snap.g().V(1).values("emb").next()
        v2 = snap.g().V(2).values("emb").next()
        assert v1 == v2, "Identical FloatVectors should be equal"

    def test_vector_type_coercion(self):
        """Plain lists are auto-wrapped to Vector in nearest/similarity."""
        from rocksgraph._codec import _encode_step, OP_NEAREST
        buf = bytearray()
        # Passing a plain list should not crash — it's auto-converted to Vector.
        # Tuple format: (prop, query, k, ef_search, metric_override)
        _encode_step(OP_NEAREST, ("emb", [1.0, 2.0, 3.0], 5, None, None), buf)
        assert len(buf) > 0, "Encoding should succeed with auto-coerced list"

    def test_anonymous_traversal_vector_steps(self):
        """__.nearest and __.similarity produce valid anonymous traversals."""
        from rocksgraph import __
        from rocksgraph._codec import OP_NEAREST, OP_SIMILARITY
        t_near = __.nearest("emb", Vector([1.0, 2.0]), 5)
        assert len(t_near.steps) == 1
        assert t_near.steps[0][0] == OP_NEAREST

        from rocksgraph import DistanceMetric
        t_sim = __.similarity("emb", Vector([1.0, 2.0]), DistanceMetric.Cosine)
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
        txn = graph.begin()
        # Non-collinear 2D vectors so cosine similarity differs meaningfully
        vectors = [(0.0, 1.0), (0.7, 0.7), (1.0, 0.0), (0.3, 0.95), (0.9, 0.4)]
        for i, (x, y) in enumerate(vectors):
            txn.g().addV("doc").property("id", i) \
                .property("emb", Vector([x, y])).next()
        txn.commit()

        snap = graph.read()
        # Query with [1.0, 0.0] — id=2 [1.0, 0.0] is exact, id=4 [0.9, 0.4] next
        results = (
            snap.g().V()
            .nearest("emb", Vector([1.0, 0.0]), 2)
            .hasLabel("doc")
            .to_list()
        )
        assert len(results) == 2
        top_ids = [v["id"] for v in results]
        assert top_ids[0] == 2, f"Best match should be id=2, got {top_ids}"
        assert top_ids[1] == 4, f"Second best should be id=4, got {top_ids}"

    def test_nearest_midstream_rejected(self, graph):
        """nearest() placed midstream after hasLabel or bounded V([1]) must raise QueryError."""
        txn = graph.begin()
        txn.g().addV("doc").property("id", 1).property("emb", Vector([1.0, 0.0])).next()
        txn.commit()

        snap = graph.read()
        with pytest.raises(Exception, match="nearest\\(\\) is a vector index entry-point"):
            snap.g().V().hasLabel("doc").nearest("emb", Vector([1.0, 0.0]), 2).to_list()

        with pytest.raises(Exception, match="nearest\\(\\) is a vector index entry-point"):
            snap.g().V(1).nearest("emb", Vector([1.0, 0.0]), 2).to_list()


class TestNeighbors:
    def test_neighbors_requires_index(self, graph):
        """neighbors() without a vector index must surface a clear error."""
        txn = graph.begin()
        txn.g().addV("doc", 1).property("emb", Vector([1.0, 0.0])).next()
        txn.commit()

        from rocksgraph import VectorEntityType
        snap = graph.read()
        with pytest.raises(Exception, match="neighbors"):
            snap.g().V(1).neighbors("emb", "emb", 3, VectorEntityType.Vertex).to_list()

    def test_neighbors_with_index(self, tmpdir):
        """neighbors() returns k nearest vertices using the traverser's own embedding."""
        from rocksgraph import Graph, DataType, VectorEntityType, DistanceMetric

        g = Graph(tmpdir)
        with g.open_schema() as s:
            s.add_vertex_label("doc")
            s.add_property_key("emb", DataType.FloatVector)
            s.add_vector_index(
                property="emb",
                dimension=2,
                entity_type=VectorEntityType.Vertex,
                metric=DistanceMetric.Cosine,
            )

        txn = g.begin()
        txn.g().addV("doc", 1).property("emb", Vector([1.0, 0.0])).next()
        txn.g().addV("doc", 2).property("emb", Vector([0.9, 0.436])).next()  # ~25° from v1
        txn.g().addV("doc", 3).property("emb", Vector([0.0, 1.0])).next()   # orthogonal to v1
        txn.commit()

        mgr = g.index_manager()
        mgr.rebuild(VectorEntityType.Vertex, "emb")

        snap = g.read()
        # k=2 neighbors of vertex 1 ([1.0, 0.0]): v2 is closer than v3
        result = snap.g().V(1).neighbors("emb", "emb", 2, VectorEntityType.Vertex).to_list()
        assert len(result) == 2
        neighbor_ids = {v.id for v in result}
        assert 2 in neighbor_ids, f"Vertex 2 should be among neighbors of v1, got {neighbor_ids}"

        g.close()

    def test_neighbors_builder_opcode(self):
        """neighbors() generates the OP_NEIGHBORS opcode."""
        from rocksgraph import __, VectorEntityType
        from rocksgraph._codec import OP_NEIGHBORS

        t = __.neighbors("emb", "emb", 5, VectorEntityType.Vertex)
        assert len(t.steps) == 1
        assert t.steps[0][0] == OP_NEIGHBORS

    def test_neighbors_skips_no_embedding(self, graph):
        """Vertices with no embedding for the given property are silently skipped."""
        txn = graph.begin()
        txn.g().addV("doc", 1).next()  # no embedding property
        txn.commit()

        from rocksgraph import VectorEntityType
        snap = graph.read()
        # No embedding → skipped before the index check → empty output, not an error
        results = snap.g().V(1).neighbors("emb", "emb", 3, VectorEntityType.Vertex).to_list()
        assert results == []


class TestWithMetric:
    def test_similarity_dot_product_metric(self, graph):
        """similarity(metric=DotProduct) produces raw dot product, not cosine similarity."""
        from rocksgraph import DistanceMetric

        txn = graph.begin()
        # [1.0, 0.0] · [0.5, 0.5] = 0.5; cosine([1,0],[0.5,0.5]) = 1/sqrt(2) ≈ 0.707
        txn.g().addV("doc").property("id", 1).property("emb", Vector([1.0, 0.0])).next()
        txn.commit()

        snap = graph.read()
        score = snap.g().V(1) \
            .similarity("emb", Vector([0.5, 0.5]), DistanceMetric.DotProduct).next()
        assert score is not None
        assert abs(score - 0.5) < 1e-4, f"dot product should be 0.5, got {score}"

    def test_with_metric_builder_patches_nearest(self):
        """with_metric() correctly patches the nearest() step in the builder."""
        from rocksgraph import __, DistanceMetric
        from rocksgraph._codec import OP_NEAREST

        t = __.nearest("emb", Vector([1.0, 0.0]), 5).with_metric(DistanceMetric.Euclidean)
        assert len(t.steps) == 1
        assert t.steps[0][0] == OP_NEAREST
        _, (_, _, _, _, metric) = t.steps[0]
        assert metric == DistanceMetric.Euclidean

    def test_similarity_metric_is_stored_in_step(self):
        """similarity() stores metric directly in the step tuple."""
        from rocksgraph import __, DistanceMetric
        from rocksgraph._codec import OP_SIMILARITY

        t = __.similarity("emb", Vector([1.0, 0.0]), DistanceMetric.DotProduct)
        assert len(t.steps) == 1
        assert t.steps[0][0] == OP_SIMILARITY
        _, (_, _, metric) = t.steps[0]
        assert metric == DistanceMetric.DotProduct

    def test_with_metric_wrong_predecessor_raises(self):
        """with_metric() raises ValueError when not preceded by nearest()."""
        from rocksgraph import __, DistanceMetric
        with pytest.raises(ValueError, match="with_metric"):
            __.identity().with_metric(DistanceMetric.Cosine)
        from rocksgraph import VectorEntityType
        with pytest.raises(ValueError, match="with_metric"):
            __.neighbors("emb", "emb", 3, VectorEntityType.Vertex).with_metric(DistanceMetric.Cosine)
        with pytest.raises(ValueError, match="with_metric"):
            __.similarity("emb", Vector([1.0, 0.0]), DistanceMetric.Cosine).with_metric(DistanceMetric.Euclidean)
