//! Physical step tests for `as` and `select`.

use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::{
            steps::{
                as_select::{AsStep, SelectStep},
                traits::{BufferedStep, StepRef},
                vec_source::VecSourceStep,
            },
        },
    },
};
use smallvec::smallvec;
use smol_str::SmolStr;
use std::rc::Rc;

fn labeled(label: &str, value: crate::types::GValue) -> Rc<Traverser> {
    Rc::new(Traverser {
        value,
        parent: None,
        labels: Some(smallvec![SmolStr::from(label)]),
    })
}

#[test]
fn test_as_step_attaches_label() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(
        crate::types::GValue::Scalar(crate::types::gvalue::Primitive::Int64(42))
    )]);
    let mut step = AsStep::new(smallvec![SmolStr::from("x")]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].labels.as_ref().unwrap()[0], SmolStr::from("x"));
    assert_eq!(res[0].value, crate::types::GValue::Scalar(crate::types::gvalue::Primitive::Int64(42)));
}

#[test]
fn test_as_no_upstream() {
    let mut step = AsStep::new(smallvec![SmolStr::from("x")]);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_as_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(
        crate::types::GValue::Scalar(crate::types::gvalue::Primitive::Int64(1))
    )]);
    let mut step = AsStep::new(smallvec![SmolStr::from("x")]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![Traverser::new_rc(
        crate::types::GValue::Scalar(crate::types::gvalue::Primitive::Int64(2))
    )]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_select_finds_labeled_ancestor() {
    use crate::types::gvalue::Primitive;

    // Create a chain: t1 (labeled "a") → t2 → t3 (current)
    let t1 = labeled("a", crate::types::GValue::Scalar(Primitive::Int64(10)));
    let t2 = Rc::new(Traverser {
        value: crate::types::GValue::Scalar(Primitive::Int64(20)),
        parent: Some(Rc::clone(&t1)),
        labels: None,
    });
    let t3 = Rc::new(Traverser {
        value: crate::types::GValue::Scalar(Primitive::Int64(30)),
        parent: Some(Rc::clone(&t2)),
        labels: None,
    });

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Rc::clone(&t3)]);
    let mut step = SelectStep::new(smallvec![SmolStr::from("a")]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    // Should find t1 (labeled "a") and emit it
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, crate::types::GValue::Scalar(Primitive::Int64(10)));
}

#[test]
fn test_select_no_match_filters_out() {
    use crate::types::gvalue::Primitive;

    let t1 = Rc::new(Traverser {
        value: crate::types::GValue::Scalar(Primitive::Int64(10)),
        parent: None,
        labels: Some(smallvec![SmolStr::from("x")]),
    });

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t1]);
    let mut step = SelectStep::new(smallvec![SmolStr::from("y")]);  // looking for "y", but has "x"
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    // No matching label → filtered out → upstream exhausted → None
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_select_no_upstream() {
    let mut step = SelectStep::new(smallvec![SmolStr::from("x")]);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_select_reset() {
    use crate::types::gvalue::Primitive;

    let t = labeled("a", crate::types::GValue::Scalar(Primitive::Int64(10)));
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![Rc::clone(&t)]);
    let mut step = SelectStep::new(smallvec![SmolStr::from("a")]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![Rc::clone(&t)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_select_upper() {
    let mut step = SelectStep::new(smallvec![SmolStr::from("x")]);
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}
