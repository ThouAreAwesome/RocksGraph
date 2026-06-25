//! Physical step tests for `sum`, `mean`, `max`, `min`.

use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::steps::{
            numeric_reducers::{MaxStep, MeanStep, MinStep, SumStep},
            traits::{BufferedStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
    types::gvalue::{GValue, Primitive},
};
use smallvec::smallvec;
use std::rc::Rc;

fn t(v: i64) -> Rc<Traverser> { Traverser::new_rc(GValue::Scalar(Primitive::Int64(v))) }

fn int_t(v: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(Primitive::Int64(v)))
}

fn float_t(v: f64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(Primitive::Float64(v)))
}

#[test]
fn test_sum_int_stream() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), int_t(20), int_t(30)]);
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(60)));
}

#[test]
fn test_sum_mixed_stream() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), float_t(1.5)]);
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(11.5)));
}

#[test]
fn test_sum_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Null));
}

#[test]
fn test_mean() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), int_t(20), int_t(30)]);
    let mut step = MeanStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(20.0)));
}

#[test]
fn test_max() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), int_t(30), int_t(20)]);
    let mut step = MaxStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(30)));
}

#[test]
fn test_min() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), int_t(5), int_t(20)]);
    let mut step = MinStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(5)));
}

#[test]
fn test_reducer_no_upstream() {
    let mut step = SumStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_reducer_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10)]);
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    // First produce returns result
    assert!(step.produce(&mut ctx).unwrap().is_some());
    // Second produce returns None (done flag)
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_reducer_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10)]);
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    // After reset, should be able to produce again
    src.inner.borrow_mut().core.inject(smallvec![int_t(20)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_max_mixed_float() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), float_t(15.5), int_t(12)]);
    let mut step = MaxStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(15.5)));
}

#[test]
fn test_min_mixed_float() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), float_t(3.5), int_t(12)]);
    let mut step = MinStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(3.5)));
}

#[test]
fn test_max_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = MaxStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Null));
}

#[test]
fn test_mean_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = MeanStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Null));
}

#[test]
fn test_skip_non_numeric() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), Traverser::new_rc(GValue::Vertex(1)), int_t(20),]);
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(30)));
}

#[test]
fn test_max_all_float() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![float_t(1.5), float_t(3.5), float_t(2.0)]);
    let mut step = MaxStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(3.5)));
}

#[test]
fn test_min_all_float() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![float_t(1.5), float_t(0.5), float_t(2.0)]);
    let mut step = MinStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(0.5)));
}

#[test]
fn test_max_float_first_then_int() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![float_t(3.0), int_t(5)]);
    let mut step = MaxStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Float64(5.0)));
}

#[test]
fn test_reducer_upper() {
    let mut step = SumStep::default();
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_mean_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![int_t(10), int_t(20)]);
    let mut step = MeanStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let r1 = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(r1[0].value, GValue::Scalar(Primitive::Float64(15.0)));
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![int_t(100)]);
    let r2 = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(r2[0].value, GValue::Scalar(Primitive::Float64(100.0)));
}

#[test]
fn test_sum_no_upstream() {
    let mut step = SumStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_sum_upper() {
    let mut step = SumStep::default();
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_mean_no_upstream() {
    let mut step = MeanStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_max_no_upstream() {
    let mut step = MaxStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_min_no_upstream() {
    let mut step = MinStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_sum_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1)]);
    let mut step = SumStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_mean_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(10)]);
    let mut step = MeanStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_max_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(5)]);
    let mut step = MaxStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_min_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(5)]);
    let mut step = MinStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none());
}
