// Physical tests: choose()
use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{context::NoopCtx, traverser::Traverser, volcano::{
        builder::PhysicalPlan,
        steps::{choose::ChooseStep, scalar_filter::ScalarFilterStep, traits::{BufferedStep, StepRef}, vec_source::VecSourceStep},
    }},
    types::gvalue::{GValue, Primitive, PrimitivePredicate},
};
use smallvec::smallvec;
use std::rc::Rc;

fn t(v: i64) -> Rc<Traverser> { Traverser::new_rc(GValue::Scalar(Primitive::Int64(v))) }

fn eq_plan(v: i64) -> PhysicalPlan {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut f = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(v)));
    f.add_upper(src.clone() as StepRef);
    PhysicalPlan { source: src.clone(), tail: BufferedStep::new(f) as StepRef }
}

fn identity_plan() -> PhysicalPlan {
    let src = BufferedStep::new(VecSourceStep::empty());
    PhysicalPlan { source: src.clone(), tail: src.clone() as StepRef }
}

#[test]
fn test_choose_true_branch() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(42)]);
    let mut step = ChooseStep::new(eq_plan(42), identity_plan(), None);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(42)));
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_choose_pass_through() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(99)]);
    let mut step = ChooseStep::new(eq_plan(42), identity_plan(), None);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(99)));
}

#[test]
fn test_choose_false_branch() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(99)]);
    let mut step = ChooseStep::new(eq_plan(42), identity_plan(), Some(identity_plan()));
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(99)));
}

#[test]
fn test_choose_no_upstream() {
    let mut step = ChooseStep::new(eq_plan(42), identity_plan(), None);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_choose_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(42)]);
    let mut step = ChooseStep::new(eq_plan(42), identity_plan(), None);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![t(42)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}
