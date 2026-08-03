// SPDX-License-Identifier: MIT OR Apache-2.0

//! Physical step tests for `VectorNearStep` and `VectorSimilarityStep`.
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
            vector::{VectorNearStep, VectorSimilarityStep},
        },
    },
    types::gvalue::{GValue, Primitive},
};
use smallvec::smallvec;
use std::rc::Rc;

fn fv(values: Vec<f32>) -> Rc<Traverser> {
    Traverser::new_rc(GValue::FloatVector(values))
}

fn drain_all(step: &mut VectorNearStep, ctx: &mut NoopCtx) -> Vec<Rc<Traverser>> {
    let mut out = Vec::new();
    while let Some(batch) = step.produce(ctx).unwrap() {
        out.extend(batch);
    }
    out
}

fn drain_similarity(step: &mut VectorSimilarityStep, ctx: &mut NoopCtx) -> Vec<f32> {
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

// ── VectorNearStep ────────────────────────────────────────────────────────────

#[test]
fn test_vector_near_returns_top_k() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![1.0, 0.0]), // id would be 0 — exact match for query
        fv(vec![0.0, 1.0]), // orthogonal
        fv(vec![0.7, 0.7]), // 45 degrees
    ]);
    let mut step = VectorNearStep::new("emb".into(), vec![1.0, 0.0], 2);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 2, "top-k=2 should return exactly 2 results");
}

#[test]
fn test_vector_near_ordering() {
    let src = BufferedStep::new(VecSourceStep::empty());
    // Feed in order: orthogonal, 45-deg, exact — result must be re-ordered by similarity
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![0.0, 1.0]), // sim ≈ 0.0
        fv(vec![0.7, 0.7]), // sim ≈ 0.71
        fv(vec![1.0, 0.0]), // sim = 1.0
    ]);
    let mut step = VectorNearStep::new("emb".into(), vec![1.0, 0.0], 3);
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
fn test_vector_near_k_larger_than_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0]), fv(vec![0.0, 1.0])]);
    let mut step = VectorNearStep::new("emb".into(), vec![1.0, 0.0], 10);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 2, "should return all available when k > input size");
}

#[test]
fn test_vector_near_empty_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = VectorNearStep::new("emb".into(), vec![1.0, 0.0], 5);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert!(results.is_empty(), "empty input yields empty output");
}

#[test]
fn test_vector_near_skips_non_vector_traversers() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![1.0, 0.0]),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(42))), // non-vector — must be skipped
        fv(vec![0.0, 1.0]),
    ]);
    let mut step = VectorNearStep::new("emb".into(), vec![1.0, 0.0], 5);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let results = drain_all(&mut step, &mut ctx);
    assert_eq!(results.len(), 2, "non-vector traversers must be silently skipped");
}

// ── VectorSimilarityStep ──────────────────────────────────────────────────────

#[test]
fn test_vector_similarity_identical() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = VectorSimilarityStep::new("emb".into(), vec![1.0, 0.0]);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!((scores[0] - 1.0).abs() < 1e-6, "identical vectors must score 1.0");
}

#[test]
fn test_vector_similarity_orthogonal() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = VectorSimilarityStep::new("emb".into(), vec![0.0, 1.0]);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!(scores[0].abs() < 1e-6, "orthogonal vectors must score 0.0");
}

#[test]
fn test_vector_similarity_opposite() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0])]);
    let mut step = VectorSimilarityStep::new("emb".into(), vec![-1.0, 0.0]);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 1);
    assert!((scores[0] + 1.0).abs() < 1e-6, "opposite vectors must score -1.0");
}

#[test]
fn test_vector_similarity_multiple_traversers() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![fv(vec![1.0, 0.0]), fv(vec![0.0, 1.0]), fv(vec![0.7, 0.7]),]);
    let mut step = VectorSimilarityStep::new("emb".into(), vec![1.0, 0.0]);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 3);
    assert!((scores[0] - 1.0).abs() < 1e-6);
    assert!(scores[1].abs() < 1e-6);
    assert!(scores[2] > 0.0 && scores[2] < 1.0);
}

#[test]
fn test_vector_similarity_empty_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = VectorSimilarityStep::new("emb".into(), vec![1.0, 0.0]);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert!(scores.is_empty());
}

#[test]
fn test_vector_similarity_skips_non_vector_traversers() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![
        fv(vec![1.0, 0.0]),
        Traverser::new_rc(GValue::Scalar(Primitive::Int64(99))), // skipped
        fv(vec![0.0, 1.0]),
    ]);
    let mut step = VectorSimilarityStep::new("emb".into(), vec![1.0, 0.0]);
    step.add_upper(src as StepRef);
    let mut ctx = NoopCtx;
    let scores = drain_similarity(&mut step, &mut ctx);
    assert_eq!(scores.len(), 2, "non-vector traversers must be silently skipped");
}
