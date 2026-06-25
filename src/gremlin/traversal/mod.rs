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
    engine::{volcano::builder::PhysicalPlanBuilder, GraphCtx},
    gremlin::{
        type_bridge,
        type_bridge::{key_to_prop_key, push_has_step, value_to_primitive},
        value::{Key, Predicate, Value},
    },
    planner::{
        apply_rules,
        logical_step::{
            AddEStep, AddVStep, AndStep, AsStep, BothEStep, BothStep, ChooseStep, CoalesceStep, CountStep,
            CyclicPathStep, DedupStep, DropStep, EStep, EmitSpec, FoldStep, FromStep, GroupCountStep, HasIdStep, HasLabelStep,
            InEStep, InStep, InVStep, LimitStep, LogicalPlan, LogicalStep, MaxStep, MeanStep, MinStep, NotStep,
            OrStep, Order, OrderKey, OrderKeySpec, OrderStep, OtherVStep, OutEStep, OutStep, OutVStep, PathStep,
            PropertiesStep, PropertyStep, RangeStep, RepeatStep, ScalarFilterStep, SelectStep, SimplePathStep,
            SkipStep, SumStep, TailStep, ToStep, UnfoldStep, UnionStep, ValuesStep, WhereStep,
        },
    },
    types::{keys::EdgeKey, prop_key::LABEL, StoreError},
};

pub(crate) mod built;

pub use built::BuiltTraversal;

// ── RepeatBuilder ──────────────────────────────────────────────────────────────

/// Pending state for a compound `repeat(...).until(...).emit(...)` construct.
/// Flushed onto the plan when the next non-repeat-related step is pushed,
/// or when the traversal is finalized.
#[derive(Clone)]
pub(crate) struct RepeatBuilder {
    body: LogicalPlan,
    until: Option<LogicalPlan>,
    times: Option<u32>,
    emit: EmitSpec,
}

// ── PlanAppender ──────────────────────────────────────────────────────────────

#[allow(private_interfaces)]
pub trait PlanAppender: Sized {
    fn plan_mut(&mut self) -> &mut LogicalPlan;
    fn record_error(&mut self, err: StoreError);

    fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder>;

    fn flush_pending_repeat(&mut self) {
        if let Some(rb) = self.pending_repeat_mut().take() {
            if rb.until.is_none() && rb.times.is_none() {
                self.record_error(StoreError::TraversalError(
                    "repeat() must have at least one stop condition: .times(n) or .until(cond).".to_string(),
                ));
                return;
            }
            self.plan_mut().steps.push(LogicalStep::Repeat(RepeatStep {
                body: rb.body,
                until: rb.until,
                times: rb.times,
                emit: rb.emit,
            }));
        }
    }

    fn push_step(&mut self, step: LogicalStep) {
        self.flush_pending_repeat();
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
    pending_repeat: Option<RepeatBuilder>,
}

impl Clone for GraphTraversal {
    fn clone(&self) -> Self {
        Self { plan: self.plan.clone(), error: None, pending_repeat: None }
    }
}

/// Entry point for anonymous sub-traversals (mirrors Gremlin's `__`).
pub fn __() -> GraphTraversal {
    GraphTraversal { plan: LogicalPlan { steps: vec![] }, error: None, pending_repeat: None }
}

#[allow(non_snake_case)]
impl GraphTraversal {
    pub(crate) fn build(
        self,
        graph: &mut dyn GraphCtx,
        prop_keys: Option<Vec<SmolStr>>,
    ) -> Result<BuiltTraversal<'_>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        let mut logical = self.plan;
        // Flush any pending repeat before building.
        // (We can't call flush_pending_repeat() on self because it's already moved; instead
        // we check inline — the pending_repeat is moved into this method and dropped.)
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        apply_rules(&mut logical)?;
        let schema_lock = graph.schema();
        let plan = PhysicalPlanBuilder {}.build(&logical, &schema_lock)?;
        let schema = graph.schema();
        let cache = built::LabelCache::from_schema(&schema.read().unwrap());
        Ok(BuiltTraversal { graph, plan, cache, schema, prop_keys })
    }

    pub(crate) fn into_plan(self) -> LogicalPlan {
        if self.pending_repeat.is_some() {
            // This should not happen in practice — callers should flush first.
            // Return whatever plan we have; the builder will reject a RepeatStep
            // without stop conditions.
            self.plan
        } else {
            self.plan
        }
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

#[allow(private_interfaces)]
impl PlanAppender for GraphTraversal {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
    fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder> {
        &mut self.pending_repeat
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
        match type_bridge::predicate_to_primitive_predicate(pred) {
            Ok(prim_pred) => self.push_step(LogicalStep::HasLabel(HasLabelStep { pred: prim_pred })),
            Err(err) => self.record_error(err),
        }
        self
    }

    #[allow(non_snake_case)]
    fn hasId(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        let ids_vec: Vec<Value> = ids.into_iter().map(Value::Int64).collect();
        let pred = if ids_vec.len() == 1 { Predicate::Eq(ids_vec[0].clone()) } else { Predicate::Within(ids_vec) };
        match type_bridge::predicate_to_primitive_predicate(pred) {
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
        match type_bridge::predicate_to_primitive_predicate(p) {
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

    fn as_(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::As(AsStep { labels: smallvec::smallvec![label.into()] }));
        self
    }

    fn range(mut self, lo: u64, hi: u64) -> Self {
        self.push_step(LogicalStep::Range(RangeStep { lo, hi }));
        self
    }

    fn skip(mut self, n: u64) -> Self {
        self.push_step(LogicalStep::Skip(SkipStep { n }));
        self
    }

    fn tail(mut self, n: u64) -> Self {
        self.push_step(LogicalStep::Tail(TailStep { n }));
        self
    }

    fn order(mut self) -> Self {
        let keys = smallvec::smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }];
        self.push_step(LogicalStep::Order(OrderStep { keys }));
        self
    }

    fn by(self, _key: impl Into<SmolStr>) -> Self { self }
    fn order_by(self, _key: impl Into<SmolStr>, _order: Order) -> Self { self }

    fn simple_path(mut self) -> Self {
        self.push_step(LogicalStep::SimplePath(SimplePathStep {}));
        self
    }

    fn cyclic_path(mut self) -> Self {
        self.push_step(LogicalStep::CyclicPath(CyclicPathStep {}));
        self
    }

    fn choose(mut self, mut predicate: GraphTraversal, mut true_choice: GraphTraversal, false_choice: Option<GraphTraversal>) -> Self {
        if let Some(err) = predicate.error.take() { self.record_error(err); }
        if let Some(err) = true_choice.error.take() { self.record_error(err); }
        let fc = false_choice.map(|mut f| { if let Some(err) = f.error.take() { self.record_error(err); } f.into_plan() });
        self.push_step(LogicalStep::Choose(ChooseStep {
            predicate: predicate.into_plan(),
            true_choice: true_choice.into_plan(),
            false_choice: fc,
        }));
        self
    }

    fn select(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::Select(SelectStep { labels: smallvec::smallvec![label.into()] }));
        self
    }

    fn dedup(mut self) -> Self {
        self.push_step(LogicalStep::Dedup(DedupStep {}));
        self
    }

    fn group_count(mut self) -> Self {
        self.push_step(LogicalStep::GroupCount(GroupCountStep { key: None }));
        self
    }

    fn fold(mut self) -> Self {
        self.push_step(LogicalStep::Fold(FoldStep {}));
        self
    }

    fn sum(mut self) -> Self {
        self.push_step(LogicalStep::Sum(SumStep {}));
        self
    }

    fn mean(mut self) -> Self {
        self.push_step(LogicalStep::Mean(MeanStep {}));
        self
    }

    fn max(mut self) -> Self {
        self.push_step(LogicalStep::Max(MaxStep {}));
        self
    }

    fn min(mut self) -> Self {
        self.push_step(LogicalStep::Min(MinStep {}));
        self
    }

    fn unfold(mut self) -> Self {
        self.push_step(LogicalStep::Unfold(UnfoldStep {}));
        self
    }

    fn r#where(mut self, mut sub: GraphTraversal) -> Self {
        if let Some(err) = sub.error.take() {
            self.record_error(err);
        }
        self.push_step(LogicalStep::Where(WhereStep { plan: sub.into_plan() }));
        self
    }

    fn not(mut self, mut sub: GraphTraversal) -> Self {
        if let Some(err) = sub.error.take() {
            self.record_error(err);
        }
        self.push_step(LogicalStep::Not(NotStep { plan: sub.into_plan() }));
        self
    }

    fn and(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        let mut plans = Vec::new();
        for mut sub in subs {
            if let Some(err) = sub.error.take() {
                self.record_error(err);
            }
            plans.push(sub.into_plan());
        }
        self.push_step(LogicalStep::And(AndStep { plans }));
        self
    }

    fn or(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        let mut plans = Vec::new();
        for mut sub in subs {
            if let Some(err) = sub.error.take() {
                self.record_error(err);
            }
            plans.push(sub.into_plan());
        }
        self.push_step(LogicalStep::Or(OrStep { plans }));
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

    fn repeat(mut self, body: GraphTraversal) -> Self {
        self.flush_pending_repeat();
        let mut body = body;
        if let Some(err) = body.error.take() {
            self.record_error(err);
        }
        *self.pending_repeat_mut() =
            Some(RepeatBuilder { body: body.into_plan(), until: None, times: None, emit: EmitSpec::Never });
        self
    }

    fn times(mut self, n: u32) -> Self {
        if n == 0 {
            self.record_error(StoreError::TraversalError(
                "times(0) is invalid: a repeat body must run at least once.".to_string(),
            ));
            return self;
        }
        match self.pending_repeat_mut() {
            Some(ref mut rb) => rb.times = Some(n),
            None => {
                self.record_error(StoreError::TraversalError("times() must immediately follow repeat().".to_string()))
            }
        }
        self
    }

    fn until(mut self, cond: GraphTraversal) -> Self {
        let mut cond = cond;
        if let Some(err) = cond.error.take() {
            self.record_error(err);
        }
        match self.pending_repeat_mut() {
            Some(ref mut rb) => rb.until = Some(cond.into_plan()),
            None => {
                self.record_error(StoreError::TraversalError("until() must immediately follow repeat().".to_string()))
            }
        }
        self
    }

    fn emit(mut self) -> Self {
        match self.pending_repeat_mut() {
            Some(ref mut rb) => rb.emit = EmitSpec::Always,
            None => {
                self.record_error(StoreError::TraversalError("emit() must immediately follow repeat().".to_string()))
            }
        }
        self
    }

    fn emit_if(mut self, cond: GraphTraversal) -> Self {
        let mut cond = cond;
        if let Some(err) = cond.error.take() {
            self.record_error(err);
        }
        match self.pending_repeat_mut() {
            Some(ref mut rb) => rb.emit = EmitSpec::If(cond.into_plan()),
            None => {
                self.record_error(StoreError::TraversalError("emit_if() must immediately follow repeat().".to_string()))
            }
        }
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
    pending_repeat: Option<RepeatBuilder>,
    prop_keys: Option<Vec<SmolStr>>,
}

impl<'s> ReadTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx, error: None, pending_repeat: None, prop_keys: None }
    }

    /// Configure property fetching for this traversal.
    ///
    /// With no arguments, all properties are fetched (matching the pre-0.2.0 behavior).
    /// With a list of keys, only those named properties are returned.
    /// Without this call, elements are returned with id + label only.
    #[allow(non_snake_case)]
    pub fn withProperties(mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.prop_keys = Some(keys.into_iter().map(Into::into).collect());
        self
    }

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        GraphTraversal { plan: self.plan, error: None, pending_repeat: None }.build(self.ctx, self.prop_keys)
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

#[allow(private_interfaces)]
impl PlanAppender for ReadTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
    fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder> {
        &mut self.pending_repeat
    }
}

// ── WriteTraversal ────────────────────────────────────────────────────────────

/// A read-write traversal bound to a [`TxSession`](crate::api::TxSession) context.
pub struct WriteTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
    pub(crate) error: Option<StoreError>,
    pending_repeat: Option<RepeatBuilder>,
    prop_keys: Option<Vec<SmolStr>>,
}

impl<'s> WriteTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx, error: None, pending_repeat: None, prop_keys: None }
    }

    /// Configure property fetching for this traversal (see [`ReadTraversal::withProperties`]).
    #[allow(non_snake_case)]
    pub fn withProperties(mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.prop_keys = Some(keys.into_iter().map(Into::into).collect());
        self
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
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        GraphTraversal { plan: self.plan, error: None, pending_repeat: None }.build(self.ctx, self.prop_keys)
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

#[allow(private_interfaces)]
impl PlanAppender for WriteTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
    fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder> {
        &mut self.pending_repeat
    }
}
