// Physical tests: order()

use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{
        context::NoopCtx,
        traverser::Traverser,
        volcano::steps::{
            order::OrderStep,
            traits::{BufferedStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
    planner::logical_step::{Order, OrderKey, OrderKeySpec},
    types::gvalue::{GValue, Primitive},
};
use smallvec::smallvec;
use std::rc::Rc;

fn t(v: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(Primitive::Int64(v)))
}

#[test]
fn test_order_asc() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(3), t(1), t(2)]);
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(1)));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(2)));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(3)));
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_order_desc() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(3), t(1), t(2)]);
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Desc }]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(3)));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(2)));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Scalar(Primitive::Int64(1)));
}

#[test]
fn test_order_empty() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_order_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(3), t(1)]);
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    src.inner.borrow_mut().core.inject(smallvec![t(100)]);
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_order_no_upstream() {
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }]);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_order_upper() {
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }]);
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_order_done_flag() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![t(1)]);
    let mut step = OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    assert!(step.produce(&mut ctx).unwrap().is_none()); // drained
}
