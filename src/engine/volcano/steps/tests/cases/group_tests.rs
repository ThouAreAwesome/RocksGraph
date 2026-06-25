// Physical tests: group(), groupCount()
use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::steps::{
            group::{GroupCountStep, GroupStep},
            traits::{BufferedStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
    types::gvalue::{GValue, Primitive},
};
use smallvec::smallvec;
use std::rc::Rc;

fn t(v: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(Primitive::Int64(v)))
}

#[test]
fn test_group_step() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2), t(1), t(3)]);
    let mut step = GroupStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res.len(), 1);
    if let GValue::Map(entries) = &res[0].value {
        assert_eq!(entries.len(), 3); // keys: 1, 2, 3
    } else {
        panic!("expected Map");
    }
}

#[test]
fn test_group_count_step() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1), t(2), t(1), t(1), t(3)]);
    let mut step = GroupCountStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res.len(), 1);
    if let GValue::Map(entries) = &res[0].value {
        assert_eq!(entries.len(), 3); // keys: 1, 2, 3
        let cnt_1 = entries.iter().find(|(k, _)| k == &GValue::Scalar(Primitive::Int64(1))).unwrap();
        assert_eq!(cnt_1.1, GValue::Scalar(Primitive::Int64(3)));
    } else {
        panic!("expected Map");
    }
}

#[test]
fn test_group_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = GroupStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some()); // emits empty map
}

#[test]
fn test_group_count_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = GroupCountStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_group_no_upstream() {
    let mut step = GroupStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_group_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1)]);
    let mut step = GroupStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_group_count_no_upstream() {
    let mut step = GroupCountStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_group_count_upper() {
    let mut step = GroupCountStep::default();
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_group_upper() {
    let mut step = GroupStep::default();
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_group_count_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1)]);
    let mut step = GroupCountStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none());
}
