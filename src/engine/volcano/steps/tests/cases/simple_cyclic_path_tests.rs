// Physical tests: simplePath(), cyclicPath()
use crate::engine::volcano::steps::traits::CoreStep;
use crate::{
    engine::{context::NoopCtx, traverser::Traverser, volcano::steps::{
        simple_cyclic_path::{CyclicPathStep, SimplePathStep},
        traits::{BufferedStep, StepRef}, vec_source::VecSourceStep,
    }},
    types::GValue,
};
use smallvec::smallvec;
use std::rc::Rc;

fn vertex_t(id: i64) -> Rc<Traverser> { Traverser::new_rc(GValue::Vertex(id)) }

// Build a traverser with parent chain: [v1] → [v2] → [v3]
fn chain() -> Rc<Traverser> {
    let v1 = vertex_t(1);
    let v2 = Rc::new(Traverser { value: GValue::Vertex(2), parent: Some(Rc::clone(&v1)), labels: None });
    Rc::new(Traverser { value: GValue::Vertex(3), parent: Some(v2), labels: None })
}

#[test]
fn test_simple_path_passes_unique() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![chain()]);
    let mut step = SimplePathStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_simple_path_filters_cycle() {
    let v1 = vertex_t(1);
    let v2 = Rc::new(Traverser { value: GValue::Vertex(2), parent: Some(Rc::clone(&v1)), labels: None });
    let cycle = Rc::new(Traverser { value: GValue::Vertex(1), parent: Some(v2), labels: None }); // back to v1
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![cycle]);
    let mut step = SimplePathStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_cyclic_path_keeps_cycle() {
    let v1 = vertex_t(1);
    let v2 = Rc::new(Traverser { value: GValue::Vertex(2), parent: Some(Rc::clone(&v1)), labels: None });
    let cycle = Rc::new(Traverser { value: GValue::Vertex(1), parent: Some(v2), labels: None });
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![cycle]);
    let mut step = CyclicPathStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
}

#[test]
fn test_cyclic_path_filters_unique() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![chain()]);
    let mut step = CyclicPathStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_simple_path_no_upstream() {
    let mut step = SimplePathStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_simple_path_upper() {
    let mut step = SimplePathStep::default();
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_cyclic_path_no_upstream() {
    let mut step = CyclicPathStep::default();
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_cyclic_path_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![chain()]);
    let mut step = CyclicPathStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none()); // chain is unique
    step.reset();
    let v1 = vertex_t(1);
    let v2 = Rc::new(Traverser { value: GValue::Vertex(2), parent: Some(Rc::clone(&v1)), labels: None });
    let cycle = Rc::new(Traverser { value: GValue::Vertex(1), parent: Some(v2), labels: None });
    src.inner.borrow_mut().core.inject(smallvec![cycle]);
    assert!(step.produce(&mut ctx).unwrap().is_some()); // cycle after reset
}

#[test]
fn test_cyclic_path_upper() {
    let mut step = CyclicPathStep::default();
    assert!(step.upper().is_none());
    let src = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_simple_path_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![chain()]);
    let mut step = SimplePathStep::default();
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
    let v1 = vertex_t(1);
    let v2 = Rc::new(Traverser { value: GValue::Vertex(2), parent: Some(Rc::clone(&v1)), labels: None });
    let cycle = Rc::new(Traverser { value: GValue::Vertex(1), parent: Some(v2), labels: None });
    src.inner.borrow_mut().core.inject(smallvec![cycle]);
    assert!(step.produce(&mut ctx).unwrap().is_none());
}
