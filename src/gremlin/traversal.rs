// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of RocksGraph.
//
// RocksGraph is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 2 of the License, or
// (at your option) any later version.
//
// RocksGraph is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.

//! Fluent traversal builder and terminal execution API.
//!
//! # Overview
//!
//! A traversal is built in three phases:
//!
//! 1. **Source** — `snap.g()` → [`ReadTraversal`], `tx.g()` → [`WriteTraversal`].
//! 2. **Steps** — chain pipeline steps: `.V([1])`, `.out([KNOWS])`, `.values(["name"])`, … Every step method takes
//!    `self` by value and returns `Self`.
//! 3. **Terminal** — execute and collect results:
//!    - [`ReadTraversal::next`] / [`WriteTraversal::next`] → `Result<Option<Value>>`
//!    - [`ReadTraversal::to_list`] / [`WriteTraversal::to_list`] → `Result<Vec<Value>>`
//!    - [`ReadTraversal::iter`] / [`WriteTraversal::iter`] → `Result<BuiltTraversal>`
//!
//! # Type alignment
//!
//! Every terminal returns [`Value`] — the same type used for step inputs.
//! `.has("age", 42i32)` and `.values("age").next()` both deal in `Value::Int32`.
//!
//! | TinkerPop (Java) | RocksGraph (Rust) |
//! |---|---|
//! | `t.next()` | `t.next()` — `Result<Option<Value>>` |
//! | `t.toList()` | `t.to_list()` — `Result<Vec<Value>>` |
//! | iterate `Traversal` | `t.iter()?` → `BuiltTraversal` (`Iterator<Item=Result<Value>>`) |

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::{
    engine::{
        volcano::builder::{PhysicalPlan, PhysicalPlanBuilder},
        GraphCtx,
    },
    gremlin::{
        conversions,
        conversions::{key_to_prop_key, primitive_to_value, push_has_step, value_to_primitive},
        value::{Edge as UserEdge, Key, Map, Path, Predicate, Property as UserProperty, Value, Vertex as UserVertex},
    },
    planner::{
        apply_rules,
        logical_step::{
            AddEStep, AddVStep, BothEStep, BothStep, CoalesceStep, CountStep, DedupStep, DropStep, EStep, FoldStep,
            FromStep, HasIdStep, HasLabelStep, InEStep, InStep, InVStep, LimitStep, LogicalPlan, LogicalStep,
            OtherVStep, OutEStep, OutStep, OutVStep, PathStep, PropertiesStep, PropertyStep, ScalarFilterStep, ToStep,
            UnionStep, ValuesStep, WhereStep,
        },
    },
    types::{
        gvalue::GValue,
        keys::{CanonicalKey, EdgeKey},
        prop_key::LABEL,
        StoreError,
    },
};

/// Materialize an internal [`GValue`] into a user-facing [`Value`].
///
/// For `Vertex` and `Edge`, fetches the full record (label + all props) from ctx.
/// For scalars and containers, the conversion is direct.
pub(crate) fn materialize(gv: GValue, ctx: &mut dyn GraphCtx) -> Result<Value, StoreError> {
    match gv {
        GValue::Scalar(p) => Ok(primitive_to_value(p)),
        GValue::Vertex(vk) => match ctx.get_all_props(&CanonicalKey::Vertex(vk))? {
            None => Err(StoreError::NotFound),
            Some((label_id, props)) => {
                let schema_guard = ctx.schema();
                let schema = schema_guard.read().unwrap();
                let label = schema
                    .vertex_label_str(label_id)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::from(format!("vertex_{}", label_id)));
                let mut properties: HashMap<SmolStr, Vec<Value>> = HashMap::new();
                for (key, prim) in props {
                    properties.entry(key).or_default().push(primitive_to_value(prim));
                }
                Ok(Value::Vertex(UserVertex { id: vk, label, properties }))
            }
        },
        GValue::Edge(ek) => match ctx.get_all_props(&CanonicalKey::Edge(ek.canonical_edge_key()))? {
            None => Err(StoreError::NotFound),
            Some((label_id, props)) => {
                let cek = ek.canonical_edge_key();
                let schema_guard = ctx.schema();
                let schema = schema_guard.read().unwrap();
                let label = schema
                    .edge_label_str(label_id)
                    .cloned()
                    .unwrap_or_else(|| SmolStr::from(format!("edge_{}", label_id)));
                let mut properties: HashMap<SmolStr, Value> = HashMap::new();
                for (key, prim) in props {
                    properties.insert(key, primitive_to_value(prim));
                }
                Ok(Value::Edge(UserEdge { out_v: cek.src_id, in_v: cek.dst_id, label, rank: cek.rank, properties }))
            }
        },
        GValue::Property(p) => {
            let schema_guard = ctx.schema();
            let schema = schema_guard.read().unwrap();
            let key = schema.prop_key_str(p.key).cloned().unwrap_or_else(|| SmolStr::from(format!("key_{}", p.key)));
            Ok(Value::Property(UserProperty { key, value: Box::new(primitive_to_value(p.value)) }))
        }
        GValue::List(list) => {
            let mut out = Vec::with_capacity(list.len());
            for item in list {
                out.push(materialize(item, ctx)?);
            }
            Ok(Value::List(out))
        }
        GValue::Map(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.entries.push((materialize(k, ctx)?, materialize(v, ctx)?));
            }
            Ok(Value::Map(out))
        }
        GValue::Path(path) => {
            let mut objects = Vec::with_capacity(path.len());
            let mut labels: Vec<Vec<String>> = Vec::with_capacity(path.len());
            for (val, step_labels) in path {
                objects.push(materialize(val, ctx)?);
                labels.push(match step_labels {
                    Some(ls) => ls.iter().map(|s| s.to_string()).collect(),
                    None => vec![],
                });
            }
            Ok(Value::Path(Path { objects, labels }))
        }
    }
}

// ── BuiltTraversal ────────────────────────────────────────────────────────────

/// The result of building a traversal — a pull-based lazy iterator over results.
///
/// Obtained from [`ReadTraversal::iter`] or [`WriteTraversal::iter`].
/// Implements `Iterator<Item = Result<Value, StoreError>>`.
pub struct BuiltTraversal<'g> {
    graph: &'g mut dyn GraphCtx,
    plan: PhysicalPlan,
}

impl<'g> Iterator for BuiltTraversal<'g> {
    type Item = Result<Value, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.plan.next(self.graph) {
            Err(e) => Some(Err(e)),
            Ok(None) => None,
            Ok(Some(t)) => Some(materialize(t.value.clone(), self.graph)),
        }
    }
}

// ── PlanAppender ──────────────────────────────────────────────────────────────

pub trait PlanAppender: Sized {
    fn plan_mut(&mut self) -> &mut LogicalPlan;
    fn record_error(&mut self, err: StoreError);
    fn push_step(&mut self, step: LogicalStep) {
        self.plan_mut().steps.push(step);
    }
}

// ── GraphTraversal (anonymous / sub-traversal) ────────────────────────────────

/// An anonymous traversal for use inside `where_`, `coalesce`, and `union`.
///
/// Obtain one with [`__`].  All step methods take `self` by value and return
/// `Self`, making the chain a pure sequence of moves with no hidden state.
pub struct GraphTraversal {
    plan: LogicalPlan,
    pub(crate) error: Option<StoreError>,
}

impl Clone for GraphTraversal {
    fn clone(&self) -> Self {
        Self { plan: self.plan.clone(), error: None }
    }
}

/// Entry point for anonymous sub-traversals (mirrors Gremlin's `__`).
pub fn __() -> GraphTraversal {
    GraphTraversal { plan: LogicalPlan { steps: vec![] }, error: None }
}

#[allow(non_snake_case)]
impl GraphTraversal {
    pub(crate) fn build(self, graph: &mut dyn GraphCtx) -> Result<BuiltTraversal<'_>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        let mut logical = self.plan;
        apply_rules(&mut logical)?;
        let schema = graph.schema();
        let plan = PhysicalPlanBuilder {}.build(&logical, &schema)?;
        Ok(BuiltTraversal { graph, plan })
    }

    pub(crate) fn into_plan(self) -> LogicalPlan {
        self.plan
    }

    pub fn addV(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::AddV(AddVStep {
            label: label.into(),
            vertex_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    pub fn addE(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::AddE(AddEStep {
            label: label.into(),
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
            rank: None,
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.push_step(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.push_step(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Value>) -> Self {
        let key_smol = key.into();
        if key_smol == LABEL {
            self.record_error(StoreError::SchemaViolation(
                "Cannot manually set or update the reserved property 'label'. Vertex and edge labels must be specified when creating elements via addV()/addE().".to_string()
            ));
            return self;
        }
        let val = value.into();
        if let Some(prim) = value_to_primitive(val.clone()) {
            self.push_step(LogicalStep::Property(PropertyStep { prop_key: key_smol, prop_value: prim }));
        } else {
            self.record_error(StoreError::UnexpectedDataType(format!(
                "property() expects a scalar primitive value, got complex type: {:?}",
                val
            )));
        }
        self
    }

    pub fn drop(mut self) -> Self {
        self.push_step(LogicalStep::Drop(DropStep {}));
        self
    }
}

impl PlanAppender for GraphTraversal {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
}

// ── TraversalBuilder ──────────────────────────────────────────────────────────

/// Shared read pipeline steps for both [`ReadTraversal`] and [`WriteTraversal`].
pub trait TraversalBuilder: PlanAppender {
    #[allow(non_snake_case)]
    fn V(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        use crate::planner::logical_step::VStep;
        self.push_step(LogicalStep::V(VStep { ids: ids.into_iter().collect() }));
        self
    }

    #[allow(non_snake_case)]
    fn E(mut self, keys: impl IntoIterator<Item = EdgeKey>) -> Self {
        self.push_step(LogicalStep::E(EStep { keys: keys.into_iter().collect() }));
        self
    }

    fn out(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::Out(OutStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    fn r#in(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::In(InStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    fn both(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::Both(BothStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn outE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::OutE(OutEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn inE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::InE(InEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn bothE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::BothE(BothEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn inV(mut self) -> Self {
        self.push_step(LogicalStep::InV(InVStep {}));
        self
    }

    #[allow(non_snake_case)]
    fn outV(mut self) -> Self {
        self.push_step(LogicalStep::OutV(OutVStep {}));
        self
    }

    #[allow(non_snake_case)]
    fn otherV(mut self) -> Self {
        self.push_step(LogicalStep::OtherV(OtherVStep {}));
        self
    }

    /// Filter by a property key and predicate.
    ///
    /// `key` accepts `&str` / `String` (→ `Key::Property`), `Key::Id`, or `Key::Label`.
    /// `pred` accepts any scalar (→ `Predicate::Eq`) or an explicit predicate from
    /// [`eq`](crate::gremlin::value::eq), [`gt`](crate::gremlin::value::gt), etc.
    fn has(mut self, key: impl Into<Key>, pred: impl Into<Predicate>) -> Self {
        if let Err(err) = push_has_step(self.plan_mut().steps.as_mut(), key.into(), pred.into()) {
            self.record_error(err);
        }
        self
    }

    #[allow(non_snake_case)]
    fn hasLabel(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        let labels_vec: Vec<Value> = labels.into_iter().map(|l| Value::String(l.into().to_string())).collect();
        let pred =
            if labels_vec.len() == 1 { Predicate::Eq(labels_vec[0].clone()) } else { Predicate::Within(labels_vec) };
        match conversions::predicate_to_primitive_predicate(pred) {
            Ok(prim_pred) => self.push_step(LogicalStep::HasLabel(HasLabelStep { pred: prim_pred })),
            Err(err) => self.record_error(err),
        }
        self
    }

    #[allow(non_snake_case)]
    fn hasId(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        let ids_vec: Vec<Value> = ids.into_iter().map(Value::Int64).collect();
        let pred = if ids_vec.len() == 1 { Predicate::Eq(ids_vec[0].clone()) } else { Predicate::Within(ids_vec) };
        match conversions::predicate_to_primitive_predicate(pred) {
            Ok(prim_pred) => self.push_step(LogicalStep::HasId(HasIdStep { pred: prim_pred })),
            Err(err) => self.record_error(err),
        }
        self
    }

    /// Filter the current scalar.
    fn is(mut self, pred: impl Into<Predicate>) -> Self {
        let p = pred.into();
        match &p {
            Predicate::Eq(v)
            | Predicate::Ne(v)
            | Predicate::Gt(v)
            | Predicate::Gte(v)
            | Predicate::Lt(v)
            | Predicate::Lte(v) => {
                if value_to_primitive(v.clone()).is_none() {
                    self.record_error(StoreError::UnexpectedDataType(format!(
                        "is() expects scalar values, got: {:?}",
                        v
                    )));
                    return self;
                }
            }
            Predicate::Between(lo, hi) => {
                if value_to_primitive(lo.clone()).is_none() || value_to_primitive(hi.clone()).is_none() {
                    self.record_error(StoreError::UnexpectedDataType(format!(
                        "is() expects scalar values, got: {:?}, {:?}",
                        lo, hi
                    )));
                    return self;
                }
            }
            Predicate::Within(vs) | Predicate::Without(vs) => {
                for v in vs {
                    if value_to_primitive(v.clone()).is_none() {
                        self.record_error(StoreError::UnexpectedDataType(format!(
                            "is() expects scalar values, got: {:?}",
                            v
                        )));
                        return self;
                    }
                }
            }
        }
        match conversions::predicate_to_primitive_predicate(p) {
            Ok(prim_pred) => self.push_step(LogicalStep::ScalarFilter(ScalarFilterStep { pred: prim_pred })),
            Err(err) => self.record_error(err),
        }
        self
    }

    /// Extract scalar values for the given keys (including `Key::Id` and `Key::Label`).
    fn values(mut self, keys: impl IntoIterator<Item = impl Into<Key>>) -> Self {
        self.push_step(LogicalStep::Values(ValuesStep {
            property_keys: keys.into_iter().map(|k| key_to_prop_key(k.into())).collect(),
        }));
        self
    }

    /// Extract [`Property`](crate::gremlin::value::Property) elements for user-defined keys only.
    ///
    /// `Key::Id` and `Key::Label` are not property elements — use `.values()` for those.
    fn properties(mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.push_step(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(Into::into).collect(),
        }));
        self
    }

    fn count(mut self) -> Self {
        self.push_step(LogicalStep::Count(CountStep {}));
        self
    }

    fn limit(mut self, n: u32) -> Self {
        self.push_step(LogicalStep::Limit(LimitStep { limit: n }));
        self
    }

    fn path(mut self) -> Self {
        self.push_step(LogicalStep::Path(PathStep {}));
        self
    }

    fn dedup(mut self) -> Self {
        self.push_step(LogicalStep::Dedup(DedupStep {}));
        self
    }

    fn fold(mut self) -> Self {
        self.push_step(LogicalStep::Fold(FoldStep {}));
        self
    }

    fn r#where(mut self, mut sub: GraphTraversal) -> Self {
        if let Some(err) = sub.error.take() {
            self.record_error(err);
        }
        self.push_step(LogicalStep::Where(WhereStep { plan: sub.into_plan() }));
        self
    }

    fn coalesce(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        let mut plans = Vec::new();
        for mut sub in subs {
            if let Some(err) = sub.error.take() {
                self.record_error(err);
            }
            plans.push(sub.into_plan());
        }
        self.push_step(LogicalStep::Coalesce(CoalesceStep { plans }));
        self
    }

    fn union(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        let mut plans = Vec::new();
        for mut sub in subs {
            if let Some(err) = sub.error.take() {
                self.record_error(err);
            }
            plans.push(sub.into_plan());
        }
        self.push_step(LogicalStep::Union(UnionStep { plans: plans.into_iter().collect() }));
        self
    }
}

impl<T: PlanAppender> TraversalBuilder for T {}

// ── ReadTraversal ─────────────────────────────────────────────────────────────

/// A read-only traversal bound to a [`ReadSession`](crate::api::ReadSession) context.
pub struct ReadTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
    pub(crate) error: Option<StoreError>,
}

impl<'s> ReadTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx, error: None }
    }

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        GraphTraversal { plan: self.plan, error: None }.build(self.ctx)
    }

    /// Execute and return the first result (`tryNext()` in Gremlin).
    pub fn next(self) -> Result<Option<Value>, StoreError> {
        self.iter()?.next().transpose()
    }

    /// Execute and collect all results (`toList()` in Gremlin).
    pub fn to_list(self) -> Result<Vec<Value>, StoreError> {
        self.iter()?.collect()
    }
}

impl PlanAppender for ReadTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
}

// ── WriteTraversal ────────────────────────────────────────────────────────────

/// A read-write traversal bound to a [`TxSession`](crate::api::TxSession) context.
pub struct WriteTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
    pub(crate) error: Option<StoreError>,
}

impl<'s> WriteTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx, error: None }
    }

    // ── Concrete mutating methods ─────────────────────────────────────────────

    #[allow(non_snake_case)]
    pub fn addV(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::AddV(AddVStep {
            label: label.into(),
            vertex_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    #[allow(non_snake_case)]
    pub fn addE(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::AddE(AddEStep {
            label: label.into(),
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
            rank: None,
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.push_step(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.push_step(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Value>) -> Self {
        let key_smol = key.into();
        if key_smol == LABEL {
            self.record_error(StoreError::SchemaViolation(
                "Cannot manually set or update the reserved property 'label'. Vertex and edge labels must be specified when creating elements via addV()/addE().".to_string()
            ));
            return self;
        }
        let val = value.into();
        if let Some(prim) = value_to_primitive(val.clone()) {
            self.push_step(LogicalStep::Property(PropertyStep { prop_key: key_smol, prop_value: prim }));
        } else {
            self.record_error(StoreError::UnexpectedDataType(format!(
                "property() expects a scalar primitive value, got complex type: {:?}",
                val
            )));
        }
        self
    }

    pub fn drop(mut self) -> Self {
        self.push_step(LogicalStep::Drop(DropStep {}));
        self
    }

    // ── Terminal ops ──────────────────────────────────────────────────────────

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        GraphTraversal { plan: self.plan, error: None }.build(self.ctx)
    }

    /// Execute and return the first result.
    pub fn next(self) -> Result<Option<Value>, StoreError> {
        self.iter()?.next().transpose()
    }

    /// Execute and collect all results.
    pub fn to_list(self) -> Result<Vec<Value>, StoreError> {
        self.iter()?.collect()
    }
}

impl PlanAppender for WriteTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
}
