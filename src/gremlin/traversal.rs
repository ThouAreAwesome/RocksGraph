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
                let mut properties: HashMap<String, Vec<Value>> = HashMap::new();
                for (key, prim) in props {
                    properties.entry(key.to_string()).or_default().push(primitive_to_value(prim));
                }
                Ok(Value::Vertex(UserVertex { id: vk, label_id, properties }))
            }
        },
        GValue::Edge(ek) => match ctx.get_all_props(&CanonicalKey::Edge(ek.canonical_edge_key()))? {
            None => Err(StoreError::NotFound),
            Some((label_id, props)) => {
                let cek = ek.canonical_edge_key();
                let mut properties: HashMap<String, Value> = HashMap::new();
                for (key, prim) in props {
                    properties.insert(key.to_string(), primitive_to_value(prim));
                }
                Ok(Value::Edge(UserEdge { out_v: cek.src_id, in_v: cek.dst_id, label_id, rank: cek.rank, properties }))
            }
        },
        GValue::Property(p) => {
            let schema_guard = ctx.schema();
            let schema = schema_guard.read().unwrap();
            let key_str = schema.prop_key_str(p.key).map(|k| k.to_string()).unwrap_or_else(|| format!("key_{}", p.key));
            Ok(Value::Property(UserProperty { key: key_str, value: Box::new(primitive_to_value(p.value)) }))
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

// ── GraphTraversal (anonymous / sub-traversal) ────────────────────────────────

/// An anonymous traversal for use inside `where_`, `coalesce`, and `union`.
///
/// Obtain one with [`__`].  All step methods take `self` by value and return
/// `Self`, making the chain a pure sequence of moves with no hidden state.
#[derive(Clone)]
pub struct GraphTraversal {
    plan: LogicalPlan,
}

/// Entry point for anonymous sub-traversals (mirrors Gremlin's `__`).
pub fn __() -> GraphTraversal {
    GraphTraversal { plan: LogicalPlan { steps: vec![] } }
}

#[allow(non_snake_case)]
impl GraphTraversal {
    pub(crate) fn build(self, graph: &mut dyn GraphCtx) -> Result<BuiltTraversal<'_>, StoreError> {
        let mut logical = self.plan;
        apply_rules(&mut logical)?;
        let schema = graph.schema();
        let plan = PhysicalPlanBuilder {}.build(&logical, &schema)?;
        Ok(BuiltTraversal { graph, plan })
    }

    pub(crate) fn into_plan(self) -> LogicalPlan {
        self.plan
    }

    pub fn V(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        self.plan.steps.push(LogicalStep::V(crate::planner::logical_step::VStep { ids: ids.into_iter().collect() }));
        self
    }

    pub fn E(mut self, keys: impl IntoIterator<Item = EdgeKey>) -> Self {
        self.plan.steps.push(LogicalStep::E(EStep { keys: keys.into_iter().collect() }));
        self
    }

    pub fn addV(mut self, label: impl Into<SmolStr>) -> Self {
        self.plan.steps.push(LogicalStep::AddV(AddVStep {
            label: label.into(),
            vertex_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    pub fn addE(mut self, label: impl Into<SmolStr>) -> Self {
        self.plan.steps.push(LogicalStep::AddE(AddEStep {
            label: label.into(),
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
            rank: None,
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.plan.steps.push(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.plan.steps.push(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn out(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::Out(OutStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn outE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::OutE(OutEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    pub fn r#in(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::In(InStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn inE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::InE(InEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    pub fn both(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::Both(BothStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn bothE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::BothE(BothEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    pub fn count(mut self) -> Self {
        self.plan.steps.push(LogicalStep::Count(CountStep {}));
        self
    }

    pub fn hasLabel(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan
            .steps
            .push(LogicalStep::HasLabel(HasLabelStep { labels: labels.into_iter().map(Into::into).collect() }));
        self
    }

    pub fn inV(mut self) -> Self {
        self.plan.steps.push(LogicalStep::InV(InVStep {}));
        self
    }

    pub fn otherV(mut self) -> Self {
        self.plan.steps.push(LogicalStep::OtherV(OtherVStep {}));
        self
    }

    pub fn outV(mut self) -> Self {
        self.plan.steps.push(LogicalStep::OutV(OutVStep {}));
        self
    }

    pub fn has(mut self, key: impl Into<Key>, pred: impl Into<Predicate>) -> Self {
        push_has_step(&mut self.plan.steps, key.into(), pred.into());
        self
    }

    pub fn is(mut self, pred: impl Into<Predicate>) -> Self {
        if let Predicate::Eq(v) = pred.into() {
            if let Some(p) = value_to_primitive(v) {
                self.plan.steps.push(LogicalStep::ScalarFilter(ScalarFilterStep { value: p }));
            }
        }
        self
    }

    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Value>) -> Self {
        if let Some(prim) = value_to_primitive(value.into()) {
            self.plan.steps.push(LogicalStep::Property(PropertyStep { prop_key: key.into(), prop_value: prim }));
        }
        self
    }

    pub fn values(mut self, keys: impl IntoIterator<Item = impl Into<Key>>) -> Self {
        self.plan.steps.push(LogicalStep::Values(ValuesStep {
            property_keys: keys.into_iter().map(|k| key_to_prop_key(k.into())).collect(),
        }));
        self
    }

    pub fn properties(mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan.steps.push(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(Into::into).collect(),
        }));
        self
    }

    pub fn r#where(mut self, sub: GraphTraversal) -> Self {
        self.plan.steps.push(LogicalStep::Where(WhereStep { plan: sub.into_plan() }));
        self
    }

    pub fn union(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.plan
            .steps
            .push(LogicalStep::Union(UnionStep { plans: subs.into_iter().map(|t| t.into_plan()).collect() }));
        self
    }

    pub fn coalesce(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.plan
            .steps
            .push(LogicalStep::Coalesce(CoalesceStep { plans: subs.into_iter().map(|t| t.into_plan()).collect() }));
        self
    }

    pub fn limit(mut self, n: u32) -> Self {
        self.plan.steps.push(LogicalStep::Limit(LimitStep { limit: n }));
        self
    }

    pub fn hasId(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        self.plan.steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() }));
        self
    }

    pub fn properties_step(mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.plan.steps.push(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(|k| SmolStr::new(k.as_ref())).collect(),
        }));
        self
    }

    pub fn path(mut self) -> Self {
        self.plan.steps.push(LogicalStep::Path(PathStep {}));
        self
    }

    pub fn dedup(mut self) -> Self {
        self.plan.steps.push(LogicalStep::Dedup(DedupStep {}));
        self
    }

    pub fn fold(mut self) -> Self {
        self.plan.steps.push(LogicalStep::Fold(FoldStep {}));
        self
    }
}

// ── TraversalBuilder ──────────────────────────────────────────────────────────

/// Shared read pipeline steps for both [`ReadTraversal`] and [`WriteTraversal`].
pub trait TraversalBuilder: Sized {
    #[doc(hidden)]
    fn plan_mut(&mut self) -> &mut LogicalPlan;

    #[allow(non_snake_case)]
    fn V(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        use crate::planner::logical_step::VStep;
        self.plan_mut().steps.push(LogicalStep::V(VStep { ids: ids.into_iter().collect() }));
        self
    }

    #[allow(non_snake_case)]
    fn E(mut self, keys: impl IntoIterator<Item = EdgeKey>) -> Self {
        self.plan_mut().steps.push(LogicalStep::E(EStep { keys: keys.into_iter().collect() }));
        self
    }

    fn out(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::Out(OutStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    fn in_(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::In(InStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    fn both(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::Both(BothStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn outE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::OutE(OutEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn inE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::InE(InEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn bothE(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::BothE(BothEStep {
            labels: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn inV(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::InV(InVStep {}));
        self
    }

    #[allow(non_snake_case)]
    fn outV(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::OutV(OutVStep {}));
        self
    }

    #[allow(non_snake_case)]
    fn otherV(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::OtherV(OtherVStep {}));
        self
    }

    /// Filter by a property key and predicate.
    ///
    /// `key` accepts `&str` / `String` (→ `Key::Property`), `Key::Id`, or `Key::Label`.
    /// `pred` accepts any scalar (→ `Predicate::Eq`) or an explicit predicate from
    /// [`eq`](crate::gremlin::value::eq), [`gt`](crate::gremlin::value::gt), etc.
    fn has(mut self, key: impl Into<Key>, pred: impl Into<Predicate>) -> Self {
        push_has_step(self.plan_mut().steps.as_mut(), key.into(), pred.into());
        self
    }

    #[allow(non_snake_case)]
    fn hasLabel(mut self, labels: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut()
            .steps
            .push(LogicalStep::HasLabel(HasLabelStep { labels: labels.into_iter().map(Into::into).collect() }));
        self
    }

    #[allow(non_snake_case)]
    fn hasId(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        self.plan_mut().steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() }));
        self
    }

    /// Filter the current scalar to equal `pred`.
    fn is(mut self, pred: impl Into<Predicate>) -> Self {
        if let Predicate::Eq(v) = pred.into() {
            if let Some(p) = value_to_primitive(v) {
                self.plan_mut().steps.push(LogicalStep::ScalarFilter(ScalarFilterStep { value: p }));
            }
        }
        self
    }

    /// Extract scalar values for the given keys (including `Key::Id` and `Key::Label`).
    fn values(mut self, keys: impl IntoIterator<Item = impl Into<Key>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::Values(ValuesStep {
            property_keys: keys.into_iter().map(|k| key_to_prop_key(k.into())).collect(),
        }));
        self
    }

    /// Extract [`Property`](crate::gremlin::value::Property) elements for user-defined keys only.
    ///
    /// `Key::Id` and `Key::Label` are not property elements — use `.values()` for those.
    fn properties(mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.plan_mut().steps.push(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(Into::into).collect(),
        }));
        self
    }

    fn count(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::Count(CountStep {}));
        self
    }

    fn limit(mut self, n: u32) -> Self {
        self.plan_mut().steps.push(LogicalStep::Limit(LimitStep { limit: n }));
        self
    }

    fn path(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::Path(PathStep {}));
        self
    }

    fn dedup(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::Dedup(DedupStep {}));
        self
    }

    fn fold(mut self) -> Self {
        self.plan_mut().steps.push(LogicalStep::Fold(FoldStep {}));
        self
    }

    fn r#where(mut self, sub: GraphTraversal) -> Self {
        self.plan_mut().steps.push(LogicalStep::Where(WhereStep { plan: sub.into_plan() }));
        self
    }

    fn coalesce(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.plan_mut()
            .steps
            .push(LogicalStep::Coalesce(CoalesceStep { plans: subs.into_iter().map(|t| t.into_plan()).collect() }));
        self
    }

    fn union(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.plan_mut()
            .steps
            .push(LogicalStep::Union(UnionStep { plans: subs.into_iter().map(|t| t.into_plan()).collect() }));
        self
    }
}

// ── ReadTraversal ─────────────────────────────────────────────────────────────

/// A read-only traversal bound to a [`ReadSession`](crate::api::ReadSession) context.
pub struct ReadTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
}

impl<'s> ReadTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx }
    }

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        GraphTraversal { plan: self.plan }.build(self.ctx)
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

impl TraversalBuilder for ReadTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
}

// ── WriteTraversal ────────────────────────────────────────────────────────────

/// A read-write traversal bound to a [`TxSession`](crate::api::TxSession) context.
pub struct WriteTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
}

impl<'s> WriteTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx }
    }

    // ── Write steps ───────────────────────────────────────────────────────────

    /// Add a vertex with the given label.
    ///
    /// In `SchemaMode::Auto` (the default), an unrecognized `label` is registered
    /// automatically on first use. In `SchemaMode::Strict`, `label` must already have been
    /// declared via [`Graph::open_management`](crate::api::Graph::open_management) — an
    /// undeclared label fails with `StoreError::SchemaViolation` instead. Same rule applies
    /// to property keys passed to [`property`](Self::property). See
    /// [`SchemaManagement`](crate::schema::SchemaManagement) for a worked example.
    #[allow(non_snake_case)]
    pub fn addV(mut self, label: impl Into<SmolStr>) -> Self {
        self.plan.steps.push(LogicalStep::AddV(AddVStep {
            label: label.into(),
            vertex_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    /// Add an edge with the given label. See [`addV`](Self::addV) for how `label` is resolved
    /// against the schema depending on `SchemaMode`.
    #[allow(non_snake_case)]
    pub fn addE(mut self, label: impl Into<SmolStr>) -> Self {
        self.plan.steps.push(LogicalStep::AddE(AddEStep {
            label: label.into(),
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
            rank: None,
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.plan.steps.push(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.plan.steps.push(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    /// Set a property on the current element.
    ///
    /// `value` must be a scalar — passing `Value::Vertex`, `Value::List`, etc. is a
    /// programming error and the step will be silently dropped.
    ///
    /// `key` is resolved against the schema the same way `label` is in [`addV`](Self::addV) —
    /// implicitly registered in `SchemaMode::Auto`, or rejected with
    /// `StoreError::SchemaViolation` in `SchemaMode::Strict` unless already declared.
    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Value>) -> Self {
        if let Some(prim) = value_to_primitive(value.into()) {
            self.plan.steps.push(LogicalStep::Property(PropertyStep { prop_key: key.into(), prop_value: prim }));
        }
        self
    }

    pub fn drop(mut self) -> Self {
        self.plan.steps.push(LogicalStep::Drop(DropStep {}));
        self
    }

    // ── Terminal ops ──────────────────────────────────────────────────────────

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        GraphTraversal { plan: self.plan }.build(self.ctx)
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

impl TraversalBuilder for WriteTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
}
