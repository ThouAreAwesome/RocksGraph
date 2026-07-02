//! Physical step tests for `id`, `label`, `constant`, `identity`, and `local`.

use crate::engine::volcano::steps::traits::CoreStep;
use crate::engine::{
    context::{GraphCtx, NoopCtx},
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
    error::StoreError,
    gvalue::{GValue, Primitive},
    keys::{CanonicalKey, EdgeKey, LabelId, VertexKey},
    prop_key::LABEL_KEY_ID,
    BatchScenario, Direction,
};
use smallvec::smallvec;
use smol_str::SmolStr;
use std::collections::HashMap;
use std::{rc::Rc, sync::RwLock};

fn scalar_t(value: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Scalar(Primitive::Int64(value)))
}

fn vertex_t(vk: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Vertex(vk))
}

fn edge_t(primary_id: i64, label_id: LabelId) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Edge(EdgeKey {
        primary_id,
        direction: Direction::OUT,
        label_id,
        secondary_id: 0,
        rank: 0,
    }))
}

// ── IdStep ────────────────────────────────────────────────────────────────

#[test]
fn test_id_step_vertex() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(42)]);
    let mut step = IdStep::new(false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(42)));
}

#[test]
fn test_id_step_edge() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![edge_t(99, 1)]);
    let mut step = IdStep::new(false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::String(SmolStr::from("AAAAAAAAAGMAAAABAAAAAAAAAAAAAA"))));
}

#[test]
fn test_id_step_scalar_passthrough() {
    // id() on a non-element should error, not silently pass through.
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(7)]);
    let mut step = IdStep::new(false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).is_err());
}

#[test]
fn test_id_step_no_upstream() {
    let mut step = IdStep::new(false);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_id_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1)]);
    let mut step = IdStep::new(false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_id_step_upper() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = IdStep::new(false);
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_id_step_explain() {
    let step = IdStep::new(false);
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
    let mut step = ConstantStep::new(Primitive::Int64(99), false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    let res = step.produce(&mut ctx).unwrap().unwrap();
    assert_eq!(res[0].value, GValue::Scalar(Primitive::Int64(99)));
}

#[test]
fn test_constant_step_no_upstream() {
    let mut step = ConstantStep::new(Primitive::Int64(0), false);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_constant_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(1)]);
    let mut step = ConstantStep::new(Primitive::Int64(42), false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_constant_step_upper() {
    let src = BufferedStep::new(VecSourceStep::empty());
    let mut step = ConstantStep::new(Primitive::Int64(0), false);
    step.add_upper(src.clone() as StepRef);
    assert!(step.upper().is_some());
}

#[test]
fn test_constant_step_explain() {
    let step = ConstantStep::new(Primitive::Int64(99), false);
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
        .build(&LogicalPlan { steps: vec![LogicalStep::Count(LogicalCountStep {})] }, &RwLock::new(Schema::default()))
        .unwrap();

    let mut step = LocalStep::new(sub_plan, false);
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
    let sub_plan = PhysicalPlanBuilder::default().build(&LogicalPlan { steps: vec![] }, &RwLock::new(Schema::default())).unwrap();
    let mut step = LocalStep::new(sub_plan, false);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_local_step_reset() {
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(42)]);

    let sub_plan = PhysicalPlanBuilder::default()
        .build(&LogicalPlan { steps: vec![LogicalStep::Count(LogicalCountStep {})] }, &RwLock::new(Schema::default()))
        .unwrap();

    let mut step = LocalStep::new(sub_plan, false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = NoopCtx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    step.reset();
}

#[test]
fn test_local_step_upper() {
    let sub_plan = PhysicalPlanBuilder::default().build(&LogicalPlan { steps: vec![] }, &RwLock::new(Schema::default())).unwrap();
    let mut step = LocalStep::new(sub_plan, false);
    let src: StepRef = BufferedStep::new(VecSourceStep::empty());
    step.add_upper(src.clone());
    assert!(step.upper().is_some());
}

#[test]
fn test_local_step_explain() {
    let sub_plan = PhysicalPlanBuilder::default().build(&LogicalPlan { steps: vec![] }, &RwLock::new(Schema::default())).unwrap();
    let step = LocalStep::new(sub_plan, false);
    let node = step.explain();
    assert_eq!(node.name, "LocalStep");
}

// ── decode_label (namespace collision) ───────────────────────────────────

#[test]
fn test_decode_label_vertex_edge_namespace_collision() {
    let mut schema = crate::schema::Schema::default();
    let person_id = schema.register_vertex_label(SmolStr::from("person")).unwrap();
    assert_eq!(person_id, 1);
    let knows_id = schema.register_edge_label(SmolStr::from("knows")).unwrap();
    assert_eq!(knows_id, 1);

    let schema = std::sync::Arc::new(std::sync::RwLock::new(schema));
    let v_name = crate::engine::volcano::steps::label_step::decode_label(1, true, schema.clone());
    assert_eq!(v_name.as_str(), "person");
    let e_name = crate::engine::volcano::steps::label_step::decode_label(1, false, schema.clone());
    assert_eq!(e_name.as_str(), "knows");
}

#[test]
fn test_decode_label_vertex_only() {
    let mut schema = crate::schema::Schema::default();
    schema.register_vertex_label(SmolStr::from("person")).unwrap();
    let schema = std::sync::Arc::new(std::sync::RwLock::new(schema));
    assert_eq!(crate::engine::volcano::steps::label_step::decode_label(1, true, schema.clone()).as_str(), "person");
}

#[test]
fn test_decode_label_edge_only() {
    let mut schema = crate::schema::Schema::default();
    schema.register_edge_label(SmolStr::from("knows")).unwrap();
    let schema = std::sync::Arc::new(std::sync::RwLock::new(schema));
    assert_eq!(crate::engine::volcano::steps::label_step::decode_label(1, false, schema.clone()).as_str(), "knows");
}

#[test]
fn test_decode_label_unknown_falls_back() {
    let schema = std::sync::Arc::new(std::sync::RwLock::new(crate::schema::Schema::default()));
    let name = crate::engine::volcano::steps::label_step::decode_label(99, true, schema.clone());
    assert_eq!(name.as_str(), "label_99");
}

// ── LabelStep explain ────────────────────────────────────────────────────

#[test]
fn test_label_step_explain() {
    let step = crate::engine::volcano::steps::label_step::LabelStep::new(false);
    let node = step.explain();
    assert_eq!(node.name, "LabelStep");
}

// ── LabelStep end-to-end (produce) ───────────────────────────────────────

use crate::engine::volcano::steps::label_step::LabelStep;
use std::sync::Arc;

/// A minimal `GraphCtx` for testing `LabelStep::produce()` in isolation.
struct LabelCtx {
    schema: Arc<RwLock<Schema>>,
    vertex_labels: HashMap<VertexKey, LabelId>,
}

impl LabelCtx {
    fn new(schema: Schema) -> Self {
        LabelCtx { schema: Arc::new(RwLock::new(schema)), vertex_labels: HashMap::new() }
    }
    fn with_vertex(mut self, vk: i64, label_id: LabelId) -> Self {
        self.vertex_labels.insert(vk, label_id);
        self
    }
}

impl GraphCtx for LabelCtx {
    fn get_vertex(&mut self, _key: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        Ok(None)
    }
    fn get_vertices(&mut self, _keys: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        Ok(vec![])
    }
    fn get_edge(&mut self, _key: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        Ok(None)
    }
    fn get_edges(&mut self, _keys: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
        Ok(vec![])
    }
    fn get_adjacent_edges(
        &mut self,
        _vk: VertexKey,
        _dir: Direction,
        _opts: crate::types::keys::AdjacentEdgesOptions<'_>,
        _limit: Option<u32>,
    ) -> Result<(Vec<EdgeKey>, Option<crate::types::keys::AdjacentEdgeCursor>), StoreError> {
        Ok((vec![], None))
    }
    fn scan_vertices(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<VertexKey>,
        _limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        Ok((vec![], None))
    }
    fn scan_edges(
        &mut self,
        _label: Option<LabelId>,
        _start_from: Option<crate::types::keys::CanonicalEdgeKey>,
        _limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<crate::types::keys::CanonicalEdgeKey>), StoreError> {
        Ok((vec![], None))
    }
    fn get_property(
        &mut self,
        _key: &CanonicalKey,
        _prop_key_id: u16,
    ) -> Result<Option<crate::types::element::Property>, StoreError> {
        Ok(None)
    }
    fn get_value(&mut self, key: &CanonicalKey, prop_key_id: u16) -> Result<Option<Primitive>, StoreError> {
        if prop_key_id == LABEL_KEY_ID {
            if let CanonicalKey::Vertex(vk) = key {
                return Ok(self.vertex_labels.get(vk).copied().map(Primitive::Int32));
            }
        }
        Ok(None)
    }
    fn add_vertex(&mut self, _id: VertexKey, _label_id: LabelId) -> Result<VertexKey, StoreError> {
        Ok(0)
    }
    fn add_edge(&mut self, _ek: &EdgeKey) -> Result<EdgeKey, StoreError> {
        Ok(EdgeKey::out_e(0, 0, 0, 0))
    }
    fn set_property(&mut self, _prop: &crate::types::element::Property) -> Result<(), StoreError> {
        Ok(())
    }
    fn drop_property(&mut self, _prop: &crate::types::element::Property) -> Result<(), StoreError> {
        Ok(())
    }
    fn drop_vertex(&mut self, _vk: VertexKey) -> Result<(), StoreError> {
        Ok(())
    }
    fn drop_edge(&mut self, _ek: &EdgeKey) -> Result<(), StoreError> {
        Ok(())
    }
    fn get_all_props(
        &mut self,
        _key: &CanonicalKey,
    ) -> Result<Option<(LabelId, Vec<(SmolStr, Primitive)>)>, StoreError> {
        Ok(None)
    }
    fn batch_size(&self, _scenario: BatchScenario) -> u32 {
        1
    }
    fn get_degree(
        &mut self,
        _key: crate::types::VertexKey,
        _direction: crate::types::DegreeDirection,
    ) -> Result<u64, StoreError> {
        Ok(0)
    }
    fn schema(&self) -> Arc<RwLock<Schema>> {
        self.schema.clone()
    }
}

#[test]
fn test_label_step_vertex_label() {
    let mut schema = Schema::default();
    schema.register_vertex_label(SmolStr::from("person")).unwrap();

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(42)]);

    let mut step = LabelStep::new(false);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = LabelCtx::new(schema).with_vertex(42, 1);
    let traversers = step.produce(&mut ctx).unwrap().unwrap();
    match &traversers[0].value {
        GValue::Scalar(Primitive::String(s)) => assert_eq!(s.as_str(), "person"),
        other => panic!("expected String label, got {:?}", other),
    }
}

#[test]
fn test_label_step_edge_label() {
    let mut schema = Schema::default();
    schema.register_edge_label(SmolStr::from("knows")).unwrap();

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![edge_t(99, 1)]);

    let mut step = LabelStep::new(false);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = LabelCtx::new(schema);
    let traversers = step.produce(&mut ctx).unwrap().unwrap();
    match &traversers[0].value {
        GValue::Scalar(Primitive::String(s)) => assert_eq!(s.as_str(), "knows"),
        other => panic!("expected String label, got {:?}", other),
    }
}

#[test]
fn test_label_step_scalar_passthrough() {
    // label() on a non-element should error, not silently pass through.
    let schema = Schema::default();
    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![scalar_t(7)]);
    let mut step = LabelStep::new(false);
    step.add_upper(src.clone() as StepRef);
    let mut ctx = LabelCtx::new(schema);
    assert!(step.produce(&mut ctx).is_err());
}

#[test]
fn test_label_step_no_upstream() {
    let schema = Schema::default();
    let mut step = LabelStep::new(false);
    let mut ctx = LabelCtx::new(schema);
    assert!(step.produce(&mut ctx).unwrap().is_none());
}
