//! Physical step tests for `not`, `and`, `or`.

use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::{
            builder::PhysicalPlan,
            steps::{
                and_or::{AndStep, OrStep},
                not::NotStep,
                scalar_filter::ScalarFilterStep,
                traits::{BufferedStep, StepRef},
                vec_source::VecSourceStep,
            },
        },
    },
    types::gvalue::{Primitive, PrimitivePredicate},
};
use smallvec::smallvec;
use std::rc::Rc;

fn scalar_traverser(v: i64) -> Rc<Traverser> {
    Traverser::new_rc(crate::types::GValue::Scalar(Primitive::Int64(v)))
}

#[test]
fn test_not_step_passes_when_sub_fails() {
    // Not(eq(99)): 42 ≠ 99, so sub-plan yields nothing → pass
    let sub_src = BufferedStep::new(VecSourceStep::empty());
    let mut sub_filter = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(99)));
    sub_filter.add_upper(sub_src.clone() as StepRef);
    let sub_plan = PhysicalPlan { source: sub_src.clone(), tail: BufferedStep::new(sub_filter) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut not_step = NotStep::new(sub_plan);
    not_step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    let res = not_step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].value, crate::types::GValue::Scalar(Primitive::Int64(42)));
    assert!(not_step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_not_step_filters_when_sub_passes() {
    // Not(eq(42)): 42 == 42, so sub-plan yields result → filter out
    let sub_src = BufferedStep::new(VecSourceStep::empty());
    let mut sub_filter = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    sub_filter.add_upper(sub_src.clone() as StepRef);
    let sub_plan = PhysicalPlan { source: sub_src.clone(), tail: BufferedStep::new(sub_filter) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut not_step = NotStep::new(sub_plan);
    not_step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    assert!(not_step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_and_step_all_match() {
    // And(eq(42), eq(42)): both match → pass
    let s1 = BufferedStep::new(VecSourceStep::empty());
    let mut f1 = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    f1.add_upper(s1.clone() as StepRef);
    let p1 = PhysicalPlan { source: s1, tail: BufferedStep::new(f1) as StepRef };

    let s2 = BufferedStep::new(VecSourceStep::empty());
    let mut f2 = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    f2.add_upper(s2.clone() as StepRef);
    let p2 = PhysicalPlan { source: s2, tail: BufferedStep::new(f2) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut and_step = AndStep::new(smallvec![p1, p2]);
    and_step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    let res = and_step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res.len(), 1);
}

#[test]
fn test_and_step_one_fails() {
    // And(eq(42), eq(99)): second fails → filter out
    let s1 = BufferedStep::new(VecSourceStep::empty());
    let mut f1 = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    f1.add_upper(s1.clone() as StepRef);
    let p1 = PhysicalPlan { source: s1, tail: BufferedStep::new(f1) as StepRef };

    let s2 = BufferedStep::new(VecSourceStep::empty());
    let mut f2 = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(99)));
    f2.add_upper(s2.clone() as StepRef);
    let p2 = PhysicalPlan { source: s2, tail: BufferedStep::new(f2) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut and_step = AndStep::new(smallvec![p1, p2]);
    and_step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    assert!(and_step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_or_step_first_matches() {
    // Or(eq(42), eq(99)): first matches → pass (short-circuit)
    let s1 = BufferedStep::new(VecSourceStep::empty());
    let mut f1 = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    f1.add_upper(s1.clone() as StepRef);
    let p1 = PhysicalPlan { source: s1, tail: BufferedStep::new(f1) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut or_step = OrStep::new(smallvec![p1]);
    or_step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    let res = or_step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res.len(), 1);
}

#[test]
fn test_or_step_none_match() {
    // Or(eq(99), eq(100)): none match → filter out
    let s1 = BufferedStep::new(VecSourceStep::empty());
    let mut f1 = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(99)));
    f1.add_upper(s1.clone() as StepRef);
    let p1 = PhysicalPlan { source: s1, tail: BufferedStep::new(f1) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut or_step = OrStep::new(smallvec![p1]);
    or_step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    assert!(or_step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_not_no_upstream() {
    let sub_src = BufferedStep::new(VecSourceStep::empty());
    let sub_plan = PhysicalPlan { source: sub_src.clone(), tail: sub_src.clone() as StepRef };
    let mut step = NotStep::new(sub_plan);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_not_reset() {
    let sub_src = BufferedStep::new(VecSourceStep::empty());
    let mut sub_filter = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(99)));
    sub_filter.add_upper(sub_src.clone() as StepRef);
    let sub_plan = PhysicalPlan { source: sub_src.clone(), tail: BufferedStep::new(sub_filter) as StepRef };

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut step = NotStep::new(sub_plan);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    // After reset, should produce again if we re-inject
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(99)]);
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_not_upper() {
    let sub_src = BufferedStep::new(VecSourceStep::empty());
    let sub_plan = PhysicalPlan { source: sub_src.clone(), tail: sub_src.clone() as StepRef };
    let mut step = NotStep::new(sub_plan);
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_and_no_upstream() {
    let s = BufferedStep::new(VecSourceStep::empty());
    let p = PhysicalPlan { source: s.clone(), tail: s.clone() as StepRef };
    let mut step = AndStep::new(smallvec![p]);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_and_reset() {
    let s = BufferedStep::new(VecSourceStep::empty());
    let mut f = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    f.add_upper(s.clone() as StepRef);
    let p = PhysicalPlan { source: s, tail: BufferedStep::new(f) as StepRef };
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut step = AndStep::new(smallvec![p]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_or_no_upstream() {
    let s = BufferedStep::new(VecSourceStep::empty());
    let p = PhysicalPlan { source: s.clone(), tail: s.clone() as StepRef };
    let mut step = OrStep::new(smallvec![p]);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_or_reset() {
    let s = BufferedStep::new(VecSourceStep::empty());
    let mut f = ScalarFilterStep::new(PrimitivePredicate::Eq(Primitive::Int64(42)));
    f.add_upper(s.clone() as StepRef);
    let p = PhysicalPlan { source: s, tail: BufferedStep::new(f) as StepRef };
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(42)]);
    let mut step = OrStep::new(smallvec![p]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![scalar_traverser(99)]);
    assert!(step.produce(&mut ctx).unwrap().is_none());
}
