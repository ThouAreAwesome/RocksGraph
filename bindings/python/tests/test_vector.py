import pytest
from rocksgraph import Graph, Vector

class TestVectorSearch:
    def test_store_and_retrieve_vector_values(self, graph):
        tx = graph.tx()
        tx.traversal().addV("doc").property("id", 1) \
            .property("emb", Vector([0.1, 0.2, 0.3])).next()
        tx.traversal().addV("doc").property("id", 2) \
            .property("emb", Vector([0.7, 0.7, 0.0])).next()
        tx.commit()

        snap = graph.read()
        v1 = snap.traversal().V(1).values("emb").next()
        assert v1 is not None, "Should retrieve stored vector value"
        if isinstance(v1, list):
            assert len(v1) == 3
            assert abs(v1[0] - 0.1) < 1e-6

    def test_vectornear_exact_knn(self, graph):
        tx = graph.tx()
        for i in range(10):
            tx.traversal().addV("doc").property("id", i) \
                .property("emb", Vector([float(i) / 10.0, float(9 - i) / 10.0])).next()
        tx.commit()

        snap = graph.read()
        results = snap.traversal().V().hasLabel("doc") \
            .vectorNear("emb", Vector([0.9, 0.0]), 3).to_list()
        assert len(results) == 3, f"Expected 3 results, got {len(results)}"
        ids = [v["id"] for v in results]
        assert 9 in ids, f"Vertex 9 (exact match) should be in top-3, got {ids}"

    def test_vectornear_empty_graph(self, graph):
        snap = graph.read()
        results = snap.traversal().V().hasLabel("doc") \
            .vectorNear("emb", Vector([1.0, 2.0]), 5).to_list()
        assert results == []

    def test_cosine_similarity(self, graph):
        tx = graph.tx()
        tx.traversal().addV("doc").property("id", 1) \
            .property("emb", Vector([1.0, 0.0])).next()
        tx.commit()

        snap = graph.read()
        scores = snap.traversal().V(1) \
            .vectorSimilarity("emb", Vector([1.0, 0.0])).to_list()
        assert len(scores) == 1
        assert abs(scores[0] - 1.0) < 1e-6

        scores2 = snap.traversal().V(1) \
            .vectorSimilarity("emb", Vector([0.0, 1.0])).to_list()
        assert len(scores2) == 1
        assert abs(scores2[0] - 0.0) < 1e-6
