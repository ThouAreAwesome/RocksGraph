//! Physical step tests for `unfold`.

use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::steps::{
            traits::{BufferedStep, StepRef},
            unfold::UnfoldStep,
            vec_source::VecSourceStep,
        },
    },
    types::GValue,
};
use smallvec::smallvec;

#[test]
fn test_unfold_list() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(GValue::List(vec![
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(1)),
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(2)),
        GValue::Scalar(crate::types::gvalue::Primitive::Int64(3)),
    ]))]);
    let mut step = UnfoldStep::new(true);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;

    let r1 = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(r1[0].value, GValue::Scalar(crate::types::gvalue::Primitive::Int64(1)));
    let r2 = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(r2[0].value, GValue::Scalar(crate::types::gvalue::Primitive::Int64(2)));
    let r3 = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(r3[0].value, GValue::Scalar(crate::types::gvalue::Primitive::Int64(3)));
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_unfold_scalar_passthrough() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner
        .borrow_mut()
        .core
        .inject(smallvec![Traverser::new_rc(GValue::Scalar(crate::types::gvalue::Primitive::Int64(42)))]);
    let mut step = UnfoldStep::new(true);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(crate::types::gvalue::Primitive::Int64(42)));
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_unfold_no_upstream() {
    let mut step = UnfoldStep::new(true);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_unfold_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(GValue::List(vec![GValue::Scalar(
        crate::types::gvalue::Primitive::Int64(1)
    ),]))]);
    let mut step = UnfoldStep::new(true);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    // After reset, should buffer cleared and produce from upstream again
    src.inner
        .borrow_mut()
        .core
        .inject(smallvec![Traverser::new_rc(GValue::Scalar(crate::types::gvalue::Primitive::Int64(99)))]);
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(crate::types::gvalue::Primitive::Int64(99)));
}

#[test]
fn test_unfold_empty_list() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(GValue::List(vec![]))]);
    let mut step = UnfoldStep::new(true);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    // Empty list produces nothing, then upstream exhausted → None
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_unfold_vertex_passthrough() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(GValue::Vertex(1))]);
    let mut step = UnfoldStep::new(true);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Vertex(1));
}
