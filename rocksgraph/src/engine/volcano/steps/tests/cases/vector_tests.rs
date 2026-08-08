// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Physical step tests for `NearestStep`, `SimilarityStep`, and `NeighborsStep`.
//!
//! Traversers carry `GValue::FloatVector` directly so no graph context is
//! needed — `resolve_vector` handles that branch without a property lookup.

use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::steps::{
            traits::{BufferedStep, CoreStep, StepRef},
            vec_source::VecSourceStep,
            vector::{NearestStep, NeighborsStep, SimilarityStep},
        },
    },
    types::gvalue::{GValue, Primitive},
    vector::DistanceMetric,
};
use smallvec::smallvec;
use std::rc::Rc;

fn fv(values: Vec<f32>) -> Rc<Traverser> {
    Traverser::new_rc(GValue::FloatVector(values))
}

fn drain_all(step: &mut NearestStep, ctx: &mut NoopCtx) -> Vec<Rc<Traverser>> {
    let mut out = Vec::new();
    while let Some(batch) = step.produce(ctx).unwrap() {
        out.extend(batch);
    }
    out
}

fn drain_similarity(step: &mut SimilarityStep, ctx: &mut NoopCtx) -> Vec<f32> {
    let mut scores = Vec::new();
    while let Some(batch) = step.produce(ctx).unwrap() {
        for t in batch {
            if let GValue::Scalar(Primitive::Float32(s)) = t.value {
                scores.push(s);
            }
        }
    }
    scores
}

// ── NearestStep ────────────────────────────────────────────────────────────

#[test]
fn test_nearest_returns_top_k() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![1.0, 0.0]), // id would be 0 — exact match for query
        fv(vec![0.0, 1.0]), // orthogonal
        fv(vec![0.7, 0.7]), // 45 degrees
    ]);
    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 2, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 2, "top-k=2 should return exactly 2 results");
}

#[test]
fn test_nearest_ordering() {
    let src = BufferedStep::new(VecSourceStep::empty());
    // Feed in order: orthogonal, 45-deg, exact — result must be re-ordered by similarity
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![0.0, 1.0]), // sim ≈ 0.0
        fv(vec![0.7, 0.7]), // sim ≈ 0.71
        fv(vec![1.0, 0.0]), // sim = 1.0
    ]);
    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 3, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 3);
    // First result must be the exact match [1.0, 0.0]
    assert_eq!(results[0].value, GValue::FloatVector(vec![1.0, 0.0]));
    // Last must be the orthogonal one [0.0, 1.0]
    assert_eq!(results[2].value, GValue::FloatVector(vec![0.0, 1.0]));
}

#[test]
fn test_nearest_k_larger_than_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0]), fv(vec![0.0, 1.0])]);
    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 10, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 2, "should return all available when k > input size");
}

#[test]
fn test_nearest_empty_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 5, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert!(results.is_empty(), "empty input yields empty output");
}

#[test]
fn test_nearest_skips_non_vector_traversers() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![1.0, 0.0]),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(42))), // non-vector — must be skipped
        fv(vec![0.0, 1.0]),
    ]);
    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 5, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 2, "non-vector traversers must be silently skipped");
}

// ── SimilarityStep ──────────────────────────────────────────────────────

#[test]
fn test_similarity_identical() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = SimilarityStep::new("emb".into(), vec![1.0, 0.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!((scores[0] - 1.0).abs() < 1e-6, "identical vectors must score 1.0");
}

#[test]
fn test_similarity_orthogonal() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = SimilarityStep::new("emb".into(), vec![0.0, 1.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!(scores[0].abs() < 1e-6, "orthogonal vectors must score 0.0");
}

#[test]
fn test_similarity_opposite() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = SimilarityStep::new("emb".into(), vec![-1.0, 0.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!((scores[0] + 1.0).abs() < 1e-6, "opposite vectors must score -1.0");
}

#[test]
fn test_similarity_multiple_traversers() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0]), fv(vec![0.0, 1.0]), fv(vec![0.7, 0.7]),]);
    let mut step = SimilarityStep::new("emb".into(), vec![1.0, 0.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 3);
    assert!((scores[0] - 1.0).abs() < 1e-6);
    assert!(scores[1].abs() < 1e-6);
    assert!(scores[2] > 0.0 && scores[2] < 1.0);
}

#[test]
fn test_similarity_empty_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = SimilarityStep::new("emb".into(), vec![1.0, 0.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert!(scores.is_empty());
}

#[test]
fn test_similarity_skips_non_vector_traversers() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![1.0, 0.0]),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(99))), // skipped
        fv(vec![0.0, 1.0]),
    ]);
    let mut step = SimilarityStep::new("emb".into(), vec![1.0, 0.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 2, "non-vector traversers must be silently skipped");
}

// ── metric selection ────────────────────────────────────────────────────────

#[test]
fn test_similarity_dot_product_metric() {
    // [1.0, 0.0] · [0.5, 0.5] = 0.5; cosine([1,0],[0.5,0.5]) = 1/sqrt(2) ≈ 0.707
    // Passing DotProduct must produce 0.5, not 0.707.
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = SimilarityStep::new("emb".into(), vec![0.5, 0.5], DistanceMetric::DotProduct);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!((scores[0] - 0.5).abs() < 1e-6, "dot product score must be 0.5, got {}", scores[0]);
}

#[test]
fn test_nearest_brute_force_metric_override_changes_ordering() {
    // With query [1.0, 0.0]:
    //   A = [0.1, 0.0]: cosine = 1.0, dot = 0.1
    //   B = [0.5, 0.866]: cosine ≈ 0.5, dot = 0.5
    // Cosine picks A first; dot product picks B first.
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![0.1, 0.0]), fv(vec![0.5, 0.866_f32])]);

    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 1, None, Some(DistanceMetric::DotProduct));
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 1);
    // With dot product, B wins (0.5 > 0.1).
    assert_eq!(results[0].value, GValue::FloatVector(vec![0.5, 0.866_f32]));
}

// ── NeighborsStep ───────────────────────────────────────────────────────────

#[test]
fn test_neighbors_no_index_with_vector_traverser_returns_error() {
    // NoopCtx has no vector index — neighbors() must surface a clear error.
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = NeighborsStep::new("emb".into(), "emb".into(), 3, crate::vector::VectorEntityType::Vertex, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let result = step.produce(&mut ctx);
    assert!(result.is_err(), "neighbors() without an index must return an error");
    let msg = format!("{:?}", result.unwrap_err());
    assert!(msg.contains("neighbors()"), "error message must mention neighbors(), got: {msg}");
}

#[test]
fn test_neighbors_skips_non_vector_traversers_no_error() {
    // Traversers with no embedding are skipped before the index check —
    // so an all-non-vector upstream produces empty output even without an index.
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(1))),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(2))),
    ]);
    let mut step = NeighborsStep::new("emb".into(), "emb".into(), 3, crate::vector::VectorEntityType::Vertex, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    // All traversers have no embedding → skipped → Ok(None)
    let result = step.produce(&mut ctx);
    assert!(result.is_ok(), "no-embedding upstream must not error, got: {:?}", result);
    assert!(result.unwrap().is_none());
}

// ── Score consistency, mixed streams & edge cases ──────────────────────────

#[test]
fn test_nearest_and_similarity_score_consistency() {
    let v1 = vec![1.0, 0.0];
    let v2 = vec![0.6, 0.8];
    let v3 = vec![0.0, 1.0];
    let query = vec![1.0, 0.0];

    for metric in [DistanceMetric::Cosine, DistanceMetric::Euclidean, DistanceMetric::DotProduct] {
        // SimilarityStep scores
        let src_sim = BufferedStep::new(VecSourceStep::empty());
        src_sim.inner.borrow_mut().core.inject(smallvec![fv(v1.clone()), fv(v2.clone()), fv(v3.clone())]);
        let mut sim_step = SimilarityStep::new("emb".into(), query.clone(), metric);
        sim_step.add_upper(src_sim as StepRef);
        let mut ctx = NoopCtx;
        let sim_scores = drain_similarity(&mut sim_step, &mut ctx);
        assert_eq!(sim_scores.len(), 3);

        // NearestStep ordering
        let src_near = BufferedStep::new(VecSourceStep::empty());
        src_near.inner.borrow_mut().core.inject(smallvec![fv(v3.clone()), fv(v1.clone()), fv(v2.clone())]);
        let mut near_step = NearestStep::new("emb".into(), query.clone(), 3, None, Some(metric));
        near_step.add_upper(src_near as StepRef);
        let mut ctx = NoopCtx;
        let near_results = drain_all(&mut near_step, &mut ctx);
        assert_eq!(near_results.len(), 3);

        // Top-1 must be the vector with highest similarity score
        let mut expected_order =
            vec![(v1.clone(), sim_scores[0]), (v2.clone(), sim_scores[1]), (v3.clone(), sim_scores[2])];
        expected_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        for (i, (expected_vec, expected_sim)) in expected_order.into_iter().enumerate() {
            assert_eq!(near_results[i].value, GValue::FloatVector(expected_vec.clone()));
            let manual_sim = crate::vector::metric_sim(metric, &expected_vec, &query);
            assert!(
                (manual_sim - expected_sim).abs() < 1e-5,
                "manual_sim ({manual_sim}) != expected_sim ({expected_sim}) for {metric:?}"
            );
        }
    }
}

#[test]
fn test_nearest_with_mixed_stream_entity_inference() {
    use crate::types::keys::CanonicalEdgeKey;

    let edge_key = CanonicalEdgeKey { src_id: 10, label_id: 1, dst_id: 20, rank: 0 }.out_key();

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        Traverser::new_rc(GValue::Vertex(1)),
        Traverser::new_rc(GValue::Edge(edge_key)),
        fv(vec![1.0, 0.0]),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(42))),
        fv(vec![0.0, 1.0]),
    ]);

    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 5, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].value, GValue::FloatVector(vec![1.0, 0.0]));
    assert_eq!(results[1].value, GValue::FloatVector(vec![0.0, 1.0]));
}

#[test]
fn test_similarity_on_non_vector_property() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        Traverser::new_rc(GValue::Scalar(Primitive::String("hello".into()))),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(123))),
        Traverser::new_rc(GValue::Scalar(Primitive::Bool(true))),
    ]);
    let mut step = SimilarityStep::new("name".into(), vec![1.0, 0.0], DistanceMetric::Cosine);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert!(scores.is_empty(), "non-vector properties must be skipped silently without error");
}

#[test]
fn test_nearest_k_zero() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0]), fv(vec![0.0, 1.0]),]);
    let mut step = NearestStep::new("emb".into(), vec![1.0, 0.0], 0, None, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert!(results.is_empty(), "k=0 must short-circuit and return 0 results");
}

#[test]
fn test_neighbors_k_zero() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0]),]);
    let mut step = NeighborsStep::new("emb".into(), "emb".into(), 0, crate::vector::VectorEntityType::Vertex, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let result = step.produce(&mut ctx);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none(), "k=0 must return None without searching or erroring");
}

#[test]
fn test_neighbors_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(GValue::Scalar(Primitive::Int64(1))),]);
    let mut step = NeighborsStep::new("emb".into(), "emb".into(), 5, crate::vector::VectorEntityType::Vertex, None);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;

    // Produce once
    let _ = step.produce(&mut ctx);

    // Reset should clear buffer and cursor
    step.reset();

    // Re-inject and produce again
    let src2 = BufferedStep::new(VecSourceStep::empty());
    src2.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(GValue::Scalar(Primitive::Int64(2))),]);
    step.add_upper(src2 as StepRef);
    let res = step.produce(&mut ctx);
    assert!(res.is_ok());
    assert!(res.unwrap().is_none());
}
