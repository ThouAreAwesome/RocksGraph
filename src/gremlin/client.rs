use crate::{
    planner::logical_step::{
        AddEStep, AddVStep, BothEStep, BothStep, CoalesceStep, CountStep, FromStep, HasIdStep, HasLabelStep,
        HasPropertyStep, InEStep, InStep, InVStep, LimitStep, LogicalPlan, LogicalStep, OtherVStep, OutEStep, OutStep,
        OutVStep, PropertyStep, ScalarFilterStep, ToStep, UnionStep, ValuesStep, WhereStep,
    },
    types::Primitive,
};
use smol_str::SmolStr;
use std::collections::HashMap;
#[derive(Clone)]
pub enum GremlinArgument {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    NestedAst(GremlinQueryAst),
    List(Vec<GremlinArgument>),
    Map(HashMap<String, GremlinArgument>),
}

#[derive(Clone)]
pub struct GremlinQueryAst {
    pub steps: Vec<LogicalStep>,
}

#[derive(Clone)]
pub struct GraphTraversal {
    ast: GremlinQueryAst,
}

// ── Fluent Query Builder ──────────────────────────────────────────────────────

#[allow(non_snake_case)]
pub fn graphTraversalSource() -> GraphTraversal {
    GraphTraversal { ast: GremlinQueryAst { steps: vec![] } }
}

/// Entry point for anonymous traversals (sub-traversals).
/// Mimics Gremlin's `__` (double underscore) for nested traversals.
pub fn __() -> GraphTraversal {
    GraphTraversal { ast: GremlinQueryAst { steps: vec![] } }
}

#[allow(non_snake_case)]
impl GraphTraversal {
    pub fn build(&self) -> LogicalPlan {
        LogicalPlan { steps: self.ast.steps.clone() }
    }

    pub fn has(&mut self, key: SmolStr, value: Primitive) -> &mut Self {
        self.ast.steps.push(LogicalStep::HasProperty(HasPropertyStep { key, value }));
        self
    }

    /// Spawns a traversal with the `V()` step.
    /// This method is available on `GraphTraversal` for sub-traversals (e.g., `__.V()`).
    pub fn V(&mut self, ids: &[i64]) -> &mut Self {
        self.ast.steps.push(LogicalStep::V(crate::planner::logical_step::VStep { ids: ids.to_vec() }));
        self
    }

    pub fn addV(&mut self, label_id: u16) -> &mut Self {
        self.ast.steps.push(LogicalStep::AddV(AddVStep { label_id, vertex_id: None, properties: HashMap::new() }));
        self
    }

    pub fn addE(&mut self, label_id: u16) -> &mut Self {
        self.ast.steps.push(LogicalStep::AddE(AddEStep {
            label_id,
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    pub fn from(&mut self, vertex_id: i64) -> &mut Self {
        self.ast.steps.push(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(&mut self, vertex_id: i64) -> &mut Self {
        self.ast.steps.push(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn out(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::Out(OutStep { label_ids: labels.to_vec(), end_vertex_ids: None }));
        self
    }

    pub fn outE(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::OutE(OutEStep { label_ids: labels.to_vec(), end_vertex_ids: None }));
        self
    }

    pub fn r#in(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::In(InStep { label_ids: labels.to_vec(), end_vertex_ids: None }));
        self
    }

    pub fn inE(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::InE(InEStep { label_ids: labels.to_vec(), end_vertex_ids: None }));
        self
    }

    pub fn both(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::Both(BothStep { label_ids: labels.to_vec(), end_vertex_ids: None }));
        self
    }

    pub fn bothE(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::BothE(BothEStep { label_ids: labels.to_vec(), end_vertex_ids: None }));
        self
    }

    pub fn count(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::Count(CountStep {}));
        self
    }

    pub fn hasLabel(&mut self, labels: &[u16]) -> &mut Self {
        self.ast.steps.push(LogicalStep::HasLabel(HasLabelStep { label_ids: labels.to_vec() }));
        self
    }

    pub fn inV(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::InV(InVStep {}));
        self
    }

    pub fn otherV(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::OtherV(OtherVStep {}));
        self
    }

    pub fn outV(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::OutV(OutVStep {}));
        self
    }

    pub fn is(&mut self, value: Primitive) -> &mut Self {
        self.ast.steps.push(LogicalStep::ScalarFilter(ScalarFilterStep { value }));
        self
    }

    pub fn property(&mut self, key: SmolStr, value: Primitive) -> &mut Self {
        self.ast.steps.push(LogicalStep::Property(PropertyStep { prop_key: key, prop_value: value }));
        self
    }

    pub fn values(&mut self, keys: &[SmolStr]) -> &mut Self {
        self.ast.steps.push(LogicalStep::Values(ValuesStep { property_keys: keys.to_vec() }));
        self
    }

    pub fn r#where(&mut self, traversal: &mut GraphTraversal) -> &mut Self {
        self.ast.steps.push(LogicalStep::Where(WhereStep { plan: traversal.build() }));
        self
    }

    pub fn union(&mut self, traversals: Vec<&mut GraphTraversal>) -> &mut Self {
        self.ast
            .steps
            .push(LogicalStep::Union(UnionStep { plans: traversals.into_iter().map(|t| t.build()).collect() }));
        self
    }

    pub fn coalesce(&mut self, traversals: Vec<&mut GraphTraversal>) -> &mut Self {
        self.ast
            .steps
            .push(LogicalStep::Coalesce(CoalesceStep { plans: traversals.into_iter().map(|t| t.build()).collect() }));
        self
    }
    pub fn limit(&mut self, limit: u32) -> &mut Self {
        self.ast.steps.push(LogicalStep::Limit(LimitStep { limit }));
        self
    }
    pub fn hasId(&mut self, ids: &[i64]) -> &mut Self {
        self.ast.steps.push(LogicalStep::HasId(HasIdStep { ids: ids.to_vec() }));
        self
    }
}
