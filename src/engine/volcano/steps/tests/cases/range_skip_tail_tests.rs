// Physical tests: range(), skip(), tail()

use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::steps::{
            range_skip_tail::{RangeStep, SkipStep, TailStep},
            traits::{BufferedStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
    types::gvalue::GValue,
};
use smallvec::smallvec;
use std::rc::Rc;

fn t(v: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(crate::types::gvalue::Primitive::Int64(v)))
}

// ── RangeStep ──

#[test]
fn test_range_keep_middle() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2), t(3), t(4), t(5)]);
    let mut step = RangeStep::new(1, 4);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(2))
    );
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(3))
    );
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(4))
    );
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_range_lo_zero() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(10), t(20)]);
    let mut step = RangeStep::new(0, 1);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(10))
    );
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_range_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = RangeStep::new(0, 10);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_range_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2)]);
    let mut step = RangeStep::new(1, 2);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

// ── SkipStep ──

#[test]
fn test_skip() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2), t(3)]);
    let mut step = SkipStep::new(2);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(3))
    );
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_skip_all() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2)]);
    let mut step = SkipStep::new(5);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_skip_zero() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(42)]);
    let mut step = SkipStep::new(0);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

// ── TailStep ──

#[test]
fn test_tail() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2), t(3), t(4), t(5)]);
    let mut step = TailStep::new(2);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(4))
    );
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(5))
    );
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_tail_more_than_input() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2)]);
    let mut step = TailStep::new(10);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(1))
    );
    assert_eq!(
        step.produce(&mut ctx).unwrap().unwrap()[0].value,
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(2))
    );
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_tail_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = TailStep::new(3);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_range_no_upstream() {
    let mut step = RangeStep::new(0, 10);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_range_upper() {
    let mut step = RangeStep::new(0, 1);
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_skip_no_upstream() {
    let mut step = SkipStep::new(0);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_skip_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2)]);
    let mut step = SkipStep::new(1);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![t(3), t(4)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_tail_no_upstream() {
    let mut step = TailStep::new(3);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_tail_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(3), t(4)]);
    let mut step = TailStep::new(1);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none()); // done flag
}

#[test]
fn test_tail_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2)]);
    let mut step = TailStep::new(1);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![t(99)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}
