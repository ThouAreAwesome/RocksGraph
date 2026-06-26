//! Physical step tests for `id`, `label`, `constant`, `identity`, and `local`.

use crate::engine::volcano::steps::traits::CoreStep;
use crate::engine::{
    context::NoopCtx,
    traverser::Traverser,
    volcano::{
        builder::PhysicalPlanBuilder,
        steps::{
            constant::ConstantStep,
            id_step::IdStep,
            identity::IdentityStep,
            local::LocalStep,
            traits::{BufferedStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
};
use crate::planner::logical_step::{CountStep as LogicalCountStep, LogicalPlan, LogicalStep};
use crate::schema::Schema;
use crate::types::{
    gvalue::{GValue, Primitive},
    keys::EdgeKey,
    Direction,
};
use smallvec::smallvec;
use std::{rc::Rc, sync::RwLock};

fn scalar_t(value: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(Primitive::Int64(value)))
}

fn vertex_t(vk: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Vertex(vk))
}

fn edge_t(primary_id: i64, _label_id: u16) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Edge(EdgeKey {
        primary_id,
        direction: Direction::OUT,
        label_id: 1,
        secondary_id: 0,
        rank: 0,
    }))
}

// ── IdStep ────────────────────────────────────────────────────────────────

#[test]
fn test_id_step_vertex() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(42)]);
    let mut step = IdStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(42)));
}

#[test]
fn test_id_step_edge() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![edge_t(99, 1)]);
    let mut step = IdStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(99)));
}

#[test]
fn test_id_step_scalar_passthrough() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(7)]);
    let mut step = IdStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(7)));
}

#[test]
fn test_id_step_no_upstream() {
    let mut step = IdStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_id_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1)]);
    let mut step = IdStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_id_step_upper() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = IdStep::default();
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_id_step_explain() {
    let step = IdStep::default();
    let node = step.explain();
    assert_eq!(node.name, "IdStep");
}

// ── IdentityStep ──────────────────────────────────────────────────────────

#[test]
fn test_identity_step_passthrough() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(42)]);
    let mut step = IdentityStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(42)));
}

#[test]
fn test_identity_step_no_upstream() {
    let mut step = IdentityStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_identity_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(1)]);
    let mut step = IdentityStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_identity_step_upper() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = IdentityStep::default();
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_identity_step_explain() {
    let step = IdentityStep::default();
    assert_eq!(step.explain().name, "IdentityStep");
}

// ── ConstantStep ──────────────────────────────────────────────────────────

#[test]
fn test_constant_step_emits_fixed_value() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(1)]);
    let mut step = ConstantStep::new(Primitive::Int64(99));
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(99)));
}

#[test]
fn test_constant_step_no_upstream() {
    let mut step = ConstantStep::new(Primitive::Int64(0));
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_constant_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(1)]);
    let mut step = ConstantStep::new(Primitive::Int64(42));
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_constant_step_upper() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = ConstantStep::new(Primitive::Int64(0));
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_constant_step_explain() {
    let step = ConstantStep::new(Primitive::Int64(99));
    let node = step.explain();
    assert_eq!(node.name, "ConstantStep");
    assert!(node.params.iter().any(|(k, _)| *k == "value"));
}

// ── LocalStep ─────────────────────────────────────────────────────────────

#[test]
fn test_local_step_flatmaps() {
    // local(count()) on 3 input traversers produces 3 results (1 each).
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(1), scalar_t(2), scalar_t(3)]);

    let sub_plan = PhysicalPlanBuilder::default()
        .build(
            &LogicalPlan { steps: vec![LogicalStep::Count(LogicalCountStep {})] },
            &RwLock::new(Schema::default()),
        )
        .unwrap();

    let mut step = LocalStep::new(sub_plan);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;

    for _ in 0..3 {
        let res = step.produce(&mut ctx).unwrap().unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(1)));
    }
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_local_step_no_upstream() {
    let sub_plan = PhysicalPlanBuilder::default()
        .build(&LogicalPlan { steps: vec![] }, &RwLock::new(Schema::default()))
        .unwrap();
    let mut step = LocalStep::new(sub_plan);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_local_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(42)]);

    let sub_plan = PhysicalPlanBuilder::default()
        .build(
            &LogicalPlan { steps: vec![LogicalStep::Count(LogicalCountStep {})] },
            &RwLock::new(Schema::default()),
        )
        .unwrap();

    let mut step = LocalStep::new(sub_plan);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_local_step_upper() {
    let sub_plan = PhysicalPlanBuilder::default()
        .build(&LogicalPlan { steps: vec![] }, &RwLock::new(Schema::default()))
        .unwrap();
    let mut step = LocalStep::new(sub_plan);
    let src: StepRef = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone());
    assert!(step.upper().is_some());
}

#[test]
fn test_local_step_explain() {
    let sub_plan = PhysicalPlanBuilder::default()
        .build(&LogicalPlan { steps: vec![] }, &RwLock::new(Schema::default()))
        .unwrap();
    let step = LocalStep::new(sub_plan);
    let node = step.explain();
    assert_eq!(node.name, "LocalStep");
}
