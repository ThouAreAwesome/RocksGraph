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

// ── property-based ordering ─────────────────────────────────────────────

use crate::engine::context::GraphCtx;
use crate::schema::Schema;
use crate::types::{
    error::StoreError,
    keys::{CanonicalKey, EdgeKey, LabelId, VertexKey},
    Direction,
};
use smol_str::SmolStr;
use std::collections::HashMap;
use std::sync::Arc;

/// A minimal context that serves property values for testing OrderStep.
struct PropTestCtx {
    schema: Arc<std::sync::RwLock<Schema>>,
    /// VertexKey -> (prop_key_id -> Primitive)
    vertex_props: HashMap<VertexKey, HashMap<u16, Primitive>>,
    /// EdgeKey -> (prop_key_id -> Primitive)
    edge_props: HashMap<crate::types::keys::CanonicalEdgeKey, HashMap<u16, Primitive>>,
}

impl PropTestCtx {
    fn new(schema: Schema) -> Self {
        PropTestCtx {
            schema: Arc::new(std::sync::RwLock::new(schema)),
            vertex_props: HashMap::new(),
            edge_props: HashMap::new(),
        }
    }
    fn with_vertex_prop(mut self, vk: i64, prop_id: u16, value: Primitive) -> Self {
        self.vertex_props.entry(vk).or_default().insert(prop_id, value);
        self
    }
}

impl GraphCtx for PropTestCtx {
    fn get_vertex(&mut self, _k: VertexKey) -> Result<Option<VertexKey>, StoreError> {
        Ok(None)
    }
    fn get_vertices(&mut self, _k: &[VertexKey]) -> Result<Vec<VertexKey>, StoreError> {
        Ok(vec![])
    }
    fn get_edge(&mut self, _k: &EdgeKey) -> Result<Option<EdgeKey>, StoreError> {
        Ok(None)
    }
    fn get_edges(&mut self, _k: &[EdgeKey]) -> Result<Vec<EdgeKey>, StoreError> {
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
        _start: Option<VertexKey>,
        _limit: u32,
    ) -> Result<(Vec<VertexKey>, Option<VertexKey>), StoreError> {
        Ok((vec![], None))
    }
    fn scan_edges(
        &mut self,
        _label: Option<LabelId>,
        _start: Option<crate::types::keys::CanonicalEdgeKey>,
        _limit: u32,
    ) -> Result<(Vec<EdgeKey>, Option<crate::types::keys::CanonicalEdgeKey>), StoreError> {
        Ok((vec![], None))
    }
    fn get_property(
        &mut self,
        _key: &CanonicalKey,
        _id: u16,
    ) -> Result<Option<crate::types::element::Property>, StoreError> {
        Ok(None)
    }
    fn get_value(&mut self, key: &CanonicalKey, prop_id: u16) -> Result<Option<Primitive>, StoreError> {
        match key {
            CanonicalKey::Vertex(vk) => Ok(self.vertex_props.get(vk).and_then(|m| m.get(&prop_id)).cloned()),
            CanonicalKey::Edge(ek) => Ok(self.edge_props.get(ek).and_then(|m| m.get(&prop_id)).cloned()),
            _ => Ok(None),
        }
    }
    fn add_vertex(&mut self, _id: VertexKey, _label_id: LabelId) -> Result<VertexKey, StoreError> {
        Ok(0)
    }
    fn add_edge(&mut self, _ek: &EdgeKey) -> Result<EdgeKey, StoreError> {
        Ok(EdgeKey::out_e(0, 0, 0, 0))
    }
    fn set_property(&mut self, _p: &crate::types::element::Property) -> Result<(), StoreError> {
        Ok(())
    }
    fn drop_property(&mut self, _p: &crate::types::element::Property) -> Result<(), StoreError> {
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
    fn batch_size(&self, _s: crate::types::BatchScenario) -> u32 {
        1
    }
    fn schema(&self) -> Arc<std::sync::RwLock<Schema>> {
        self.schema.clone()
    }
}

fn vertex_t(vk: i64) -> Rc<Traverser> {
    Traverser::new_rc(GValue::Vertex(vk))
}

#[test]
fn test_order_by_property_vertex_asc() {
    let mut schema = Schema::default();
    let age_id = schema.register_prop_key(SmolStr::from("age")).unwrap();

    // Vertices 1,2,3 with ages 30,10,20
    let ctx = PropTestCtx::new(schema)
        .with_vertex_prop(1, age_id, Primitive::Int32(30))
        .with_vertex_prop(2, age_id, Primitive::Int32(10))
        .with_vertex_prop(3, age_id, Primitive::Int32(20));

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1), vertex_t(2), vertex_t(3)]);

    let mut step =
        OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Property(SmolStr::from("age")), order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = ctx;
    // Should emit vertices sorted by age: 2 (10), 3 (20), 1 (30)
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(2));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(3));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(1));
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

#[test]
fn test_order_by_property_vertex_desc() {
    let mut schema = Schema::default();
    let age_id = schema.register_prop_key(SmolStr::from("age")).unwrap();

    let ctx = PropTestCtx::new(schema)
        .with_vertex_prop(1, age_id, Primitive::Int32(30))
        .with_vertex_prop(2, age_id, Primitive::Int32(10))
        .with_vertex_prop(3, age_id, Primitive::Int32(20));

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1), vertex_t(2), vertex_t(3)]);

    let mut step =
        OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Property(SmolStr::from("age")), order: Order::Desc }]);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = ctx;
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(1)); // 30
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(3)); // 20
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(2));
    // 10
}

#[test]
fn test_order_by_missing_property_is_null() {
    let mut schema = Schema::default();
    let age_id = schema.register_prop_key(SmolStr::from("age")).unwrap();

    // Vertex 1 has no age, vertex 2 has age=10, vertex 3 has age=30
    let ctx = PropTestCtx::new(schema).with_vertex_prop(2, age_id, Primitive::Int32(10)).with_vertex_prop(
        3,
        age_id,
        Primitive::Int32(30),
    );

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1), vertex_t(2), vertex_t(3)]);

    let mut step =
        OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Property(SmolStr::from("age")), order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = ctx;
    // Null sorts before non-null values -> 1 (null), 2 (10), 3 (30)
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(1));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(2));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(3));
}

#[test]
fn test_order_by_property_name_resolution_cached() {
    let mut schema = Schema::default();
    schema.register_prop_key(SmolStr::from("score")).unwrap();

    let ctx = PropTestCtx::new(schema);

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1)]);

    let mut step =
        OrderStep::new(smallvec![OrderKey { spec: OrderKeySpec::Property(SmolStr::from("score")), order: Order::Asc }]);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = ctx;
    assert!(step.produce(&mut ctx).unwrap().is_some());
    // The prop_key_cache inside OrderStep should now contain "score" -> id
    // Verify a second produce drains correctly (no crash).
    assert!(step.produce(&mut ctx).unwrap().is_none());
}

// ── multi-key tie-breaking ──────────────────────────────────────────────

#[test]
fn test_order_by_two_properties_tie_break() {
    let mut schema = Schema::default();
    let age_id = schema.register_prop_key(SmolStr::from("age")).unwrap();
    let name_id = schema.register_prop_key(SmolStr::from("name")).unwrap();

    // V1: age=30, name="c"    -> primary key: 30 "c"
    // V2: age=10, name="b"    -> primary key: 10 "b"
    // V3: age=10, name="a"    -> primary key: 10 "a"
    // Sorted by age asc, name asc: V2(10,"b")? No — V3(10,"a") < V2(10,"b"), then V1(30,"c")
    let ctx = PropTestCtx::new(schema)
        .with_vertex_prop(1, age_id, Primitive::Int32(30))
        .with_vertex_prop(1, name_id, Primitive::String(SmolStr::from("c")))
        .with_vertex_prop(2, age_id, Primitive::Int32(10))
        .with_vertex_prop(2, name_id, Primitive::String(SmolStr::from("b")))
        .with_vertex_prop(3, age_id, Primitive::Int32(10))
        .with_vertex_prop(3, name_id, Primitive::String(SmolStr::from("a")));

    let src = BufferedStep::new(VecSourceStep::empty());
    src.inner.borrow_mut().core.inject(smallvec![vertex_t(1), vertex_t(2), vertex_t(3)]);

    let mut step = OrderStep::new(smallvec![
        OrderKey { spec: OrderKeySpec::Property(SmolStr::from("age")), order: Order::Asc },
        OrderKey { spec: OrderKeySpec::Property(SmolStr::from("name")), order: Order::Asc },
    ]);
    step.add_upper(src.clone() as StepRef);

    let mut ctx = ctx;
    // age=10,name="a" (v3), age=10,name="b" (v2), age=30,name="c" (v1)
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(3));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(2));
    assert_eq!(step.produce(&mut ctx).unwrap().unwrap()[0].value, GValue::Vertex(1));
}

// ── builder-level tests (through TraversalBuilder API) ───────────────────

use crate::gremlin::traversal::TraversalBuilder;
use crate::planner::logical_step::LogicalStep;

#[test]
fn test_builder_order_by_single_key() {
    let traversal = crate::gremlin::traversal::__();
    let plan = traversal.order().by("age").into_plan();
    assert_eq!(plan.steps.len(), 1);
    let LogicalStep::Order(ref os) = plan.steps[0] else { panic!("expected Order step") };
    assert_eq!(os.keys.len(), 1);
    assert!(matches!(os.keys[0].spec, OrderKeySpec::Property(ref k) if k == "age"));
    assert_eq!(os.keys[0].order, Order::Asc);
}

#[test]
fn test_builder_order_by_two_keys_tie_break() {
    let traversal = crate::gremlin::traversal::__();
    let plan = traversal.order().by("age").by("name").into_plan();
    assert_eq!(plan.steps.len(), 1);
    let LogicalStep::Order(ref os) = plan.steps[0] else { panic!("expected Order step") };
    assert_eq!(os.keys.len(), 2);
    assert!(matches!(os.keys[0].spec, OrderKeySpec::Property(ref k) if k == "age"));
    assert_eq!(os.keys[0].order, Order::Asc);
    assert!(matches!(os.keys[1].spec, OrderKeySpec::Property(ref k) if k == "name"));
    assert_eq!(os.keys[1].order, Order::Asc);
}

#[test]
fn test_builder_order_by_replaces_default_value_key() {
    // Bare .order() produces a single Value key. .by("age") should replace it.
    let traversal = crate::gremlin::traversal::__();
    let plan = traversal.order().by("age").into_plan();
    let LogicalStep::Order(ref os) = plan.steps[0] else { panic!("expected Order step") };
    assert_eq!(os.keys.len(), 1);
    assert!(matches!(os.keys[0].spec, OrderKeySpec::Property(_)));
}

#[test]
fn test_builder_by_without_order_auto_creates_order_step() {
    // .by("age") without a preceding .order() should auto-push one.
    let traversal = crate::gremlin::traversal::__();
    let plan = traversal.by("age").into_plan();
    assert_eq!(plan.steps.len(), 1);
    let LogicalStep::Order(ref os) = plan.steps[0] else { panic!("expected Order step") };
    assert_eq!(os.keys.len(), 1);
    assert!(matches!(os.keys[0].spec, OrderKeySpec::Property(ref k) if k == "age"));
}
