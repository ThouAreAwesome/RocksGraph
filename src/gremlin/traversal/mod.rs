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
//!
//! # Anonymous sub-traversals (`__()`)
//!
//! Sub-traversal steps (`where`, `union`, `coalesce`, `repeat`, `not`, `choose`,
//! `until`) accept a `GraphTraversal` — but the type is `#[doc(hidden)]`, so you
//! never name it directly.  Import `__` from the crate root and call it inline:
//!
//! ```
//! use rocksgraph::{Graph, TraversalBuilder, __};
//! # let dir = tempfile::tempdir().unwrap();
//! # let graph = Graph::open(dir.path()).unwrap();
//! # let mut snap = graph.read();
//! snap.g().V([1]).outE(["knows"]).r#where(__().otherV().hasId([2])).count().next().unwrap();
//! # graph.close().unwrap();
//! ```
//!
//! If `GraphTraversal` appears in a compiler error, it's the hidden type behind
//! `__()` — the same way the compiler prints `[closure@…]` for anonymous functions.

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::{
    engine::{volcano::builder::PhysicalPlanBuilder, GraphCtx},
    gremlin::{
        type_bridge,
        type_bridge::{push_has_step, value_to_primitive},
        value::{Predicate, Value},
    },
    planner::{
        apply_rules,
        logical_step::{
            AddEStep, AddVStep, AndStep, AsStep, BothEStep, BothStep, ChooseStep, CoalesceStep, ConstantStep,
            CountStep, CyclicPathStep, DedupStep, DropStep, EStep, EmitSpec, FoldStep, FromStep, GroupCountStep,
            GroupStep, HasIdStep, HasLabelStep, HasRankStep, IdStep, IdentityStep, InEStep, InStep, InVStep, LabelStep,
            LimitStep, LocalStep, LogicalPlan, LogicalStep, MaxStep, MeanStep, MinStep, NotStep, OrStep, Order,
            OrderKey, OrderKeySpec, OrderStep, OtherVStep, OutEStep, OutStep, OutVStep, PathStep, PropertiesStep,
            PropertyStep, RangeStep, RankStep, RepeatStep, ScalarFilterStep, SelectStep, SimplePathStep, SkipStep,
            SumStep, TailStep, ToStep, UnfoldStep, UnionStep, ValuesStep, WhereStep,
        },
    },
    types::{prop_key::LABEL, StoreError},
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
    times: Option<i64>,
    emit: EmitSpec,
}

/// `group()`/`group_count()` have no `by()` modulator yet (`docs/design_group_step.md`).
/// Without this check, `.by()`/`.order_by()` would silently insert a new `order()`
/// step after them instead — sorting the resulting `Map` traverser by a property it
/// doesn't have, rather than doing what the caller almost certainly intended.
fn follows_group_step(plan: &LogicalPlan) -> bool {
    matches!(plan.steps.last(), Some(LogicalStep::Group(_)) | Some(LogicalStep::GroupCount(_)))
}

fn by_after_group_error(caller: &str) -> StoreError {
    StoreError::TraversalError(format!(
        "{caller} is not supported immediately after group()/group_count() — they have no by() \
         modulator yet; see docs/design_group_step.md"
    ))
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

    /// Look up edges by their canonical id string (from `.id()`), or scan all edges with `E([])`.
    ///
    /// Takes `String` rather than `impl Into<SmolStr>` — edge ids are always 30 Base64
    /// characters, well past `SmolStr`'s 23-byte inline cap, so there's no inlining benefit
    /// to preserve, and a concrete `Item` type is what lets `E([])` (empty = "all edges",
    /// matching `V([])`) infer without a type-annotation error. Pass owned `String`s — e.g.
    /// the value captured from `.id()` — or `.to_string()` a literal.
    #[allow(non_snake_case)]
    fn E(mut self, keys: impl IntoIterator<Item = String>) -> Self {
        self.push_step(LogicalStep::E(EStep { keys: keys.into_iter().collect() }));
        self
    }

    /// Takes `&'a str` rather than `impl Into<SmolStr>` so a bare `out([])` (empty =
    /// no label filter, matching `V([])`'s `[] = all` convention) infers without a
    /// type-annotation error — a concrete `Item` type is what makes that work, the
    /// same reason `V()`/`E()` use concrete types. Every real label list is a literal
    /// or `&str` constant already, so this costs nothing at existing call sites; an
    /// owned `String`/`SmolStr` needs `.as_str()` first.
    fn out<'a>(mut self, labels: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::Out(OutStep {
            labels: labels.into_iter().map(SmolStr::from).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    fn r#in<'a>(mut self, labels: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::In(InStep {
            labels: labels.into_iter().map(SmolStr::from).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    fn both<'a>(mut self, labels: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::Both(BothStep {
            labels: labels.into_iter().map(SmolStr::from).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn outE<'a>(mut self, labels: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::OutE(OutEStep {
            labels: labels.into_iter().map(SmolStr::from).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn inE<'a>(mut self, labels: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::InE(InEStep {
            labels: labels.into_iter().map(SmolStr::from).collect(),
            end_vertex_ids: None,
            rank: None,
        }));
        self
    }

    #[allow(non_snake_case)]
    fn bothE<'a>(mut self, labels: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::BothE(BothEStep {
            labels: labels.into_iter().map(SmolStr::from).collect(),
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

    /// Filter by a user-defined property key and predicate.
    ///
    /// `key` is a plain property name — `"id"`/`"label"`/`"rank"` are rejected (use
    /// `hasId()`/`hasLabel()`/`hasRank()` instead; see `docs/design_reserved_keys.md`).
    /// `pred` accepts any scalar (→ `Predicate::Eq`) or an explicit predicate from
    /// [`eq`](crate::gremlin::value::eq), [`gt`](crate::gremlin::value::gt), etc.
    fn has(mut self, key: impl Into<SmolStr>, pred: impl Into<Predicate>) -> Self {
        if let Err(err) = push_has_step(self.plan_mut().steps.as_mut(), key.into(), pred.into()) {
            self.record_error(err);
        }
        self
    }

    /// `pred` accepts a bare label name (→ `Eq`), a fixed-size array of names (→ `Eq`/
    /// `Within`, e.g. `hasLabel(["person", "software"])`), or an explicit predicate —
    /// `eq`/`ne`/`within`/`without` are supported; range predicates (`gt`/`lt`/`between`)
    /// are rejected, since lexicographic ordering on label names isn't a meaningful query.
    #[allow(non_snake_case)]
    fn hasLabel(mut self, pred: impl Into<Predicate>) -> Self {
        let pred = pred.into();
        if let Err(err) = type_bridge::validate_label_predicate(&pred) {
            self.record_error(err);
            return self;
        }
        match type_bridge::predicate_to_primitive_predicate(pred) {
            Ok(prim_pred) => self.push_step(LogicalStep::HasLabel(HasLabelStep { pred: prim_pred })),
            Err(err) => self.record_error(err),
        }
        self
    }

    /// `pred` accepts a bare id (vertex `i64` or edge `String`, → `Eq`), a fixed-size
    /// array (→ `Eq`/`Within`, e.g. `hasId([1, 2, 3])`), or an explicit predicate —
    /// `eq`/`ne`/`gt`/`lt`/`between`/`within`/`without` all work for vertex ids; edge
    /// ids are opaque strings, so only `eq`/`ne`/`within`/`without` are meaningful there
    /// (`gt`/`lt`/`between` on an edge id never matches — see `HasIdStep`'s
    /// `EdgeIdPredicate`). `hasId([])` is unsupported (same `[]`-inference error the
    /// other collection-taking methods had before their fix) — a documented, deliberate
    /// latent gap, not yet hit by any real caller.
    #[allow(non_snake_case)]
    fn hasId(mut self, pred: impl Into<Predicate>) -> Self {
        match type_bridge::predicate_to_primitive_predicate(pred.into()) {
            Ok(prim_pred) => self.push_step(LogicalStep::HasId(HasIdStep { pred: prim_pred })),
            Err(err) => self.record_error(err),
        }
        self
    }

    /// Filter by the rank of the current edge. Edge-only — a vertex traverser
    /// never matches (consistent with `hasLabel`'s type-mismatch handling).
    #[allow(non_snake_case)]
    fn hasRank(mut self, pred: impl Into<Predicate>) -> Self {
        match type_bridge::predicate_to_primitive_predicate(pred.into()) {
            Ok(prim_pred) => self.push_step(LogicalStep::HasRank(HasRankStep { pred: prim_pred })),
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

    /// Extract scalar property values for the given keys.
    ///
    /// `"id"`/`"label"`/`"rank"` are rejected (use `id()`/`label()`/`rank()` instead;
    /// see `docs/design_reserved_keys.md`). Plain `&'a str` rather than `impl Into<SmolStr>`
    /// so `values([])` infers without a type annotation, matching `out()`/`properties()`.
    fn values<'a>(mut self, keys: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::Values(ValuesStep {
            property_keys: keys.into_iter().map(SmolStr::from).collect(),
        }));
        self
    }

    /// Extract [`Property`](crate::gremlin::value::Property) elements for user-defined keys.
    ///
    /// `"id"`/`"label"`/`"rank"` are rejected (use `id()`/`label()`/`rank()` instead).
    fn properties<'a>(mut self, keys: impl IntoIterator<Item = &'a str>) -> Self {
        self.push_step(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(SmolStr::from).collect(),
        }));
        self
    }

    fn count(mut self) -> Self {
        self.push_step(LogicalStep::Count(CountStep {}));
        self
    }

    fn limit(mut self, n: i64) -> Self {
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

    fn range(mut self, lo: i64, hi: i64) -> Self {
        self.push_step(LogicalStep::Range(RangeStep { lo, hi }));
        self
    }

    fn skip(mut self, n: i64) -> Self {
        self.push_step(LogicalStep::Skip(SkipStep { n }));
        self
    }

    fn tail(mut self, n: i64) -> Self {
        self.push_step(LogicalStep::Tail(TailStep { n }));
        self
    }

    fn order(mut self) -> Self {
        let keys = smallvec::smallvec![OrderKey { spec: OrderKeySpec::Value, order: Order::Asc }];
        self.push_step(LogicalStep::Order(OrderStep { keys }));
        self
    }

    /// Modulates the most recent `order()` step to sort by a property value
    /// instead of traverser identity.  If the last key is still the default
    /// `Value` placeholder left by `order()`, it is replaced; otherwise the
    /// new key is appended (enabling multi-key tie-breaking):
    ///
    /// ```
    /// # use rocksgraph::{Graph, TraversalBuilder};
    /// # let dir = tempfile::tempdir().unwrap();
    /// # let graph = Graph::open(dir.path()).unwrap();
    /// # let mut snap = graph.read();
    /// // sort by age, ties broken by name
    /// snap.g().V([]).order().by("age").by("name");
    /// ```
    ///
    /// `by()` is not supported immediately after `group()`/`group_count()` — unlike
    /// every other step, where a missing `order()` is auto-inserted, doing that here
    /// would silently sort the resulting `Map` traverser by a property it doesn't
    /// have, rather than setting a group key/value modulator (`group()`/
    /// `group_count()` don't have modulators at all yet — see
    /// `docs/design_group_step.md`).
    fn by(mut self, key: impl Into<SmolStr>) -> Self {
        if follows_group_step(self.plan_mut()) {
            self.record_error(by_after_group_error("by()"));
            return self;
        }
        let key: SmolStr = key.into();
        let key2 = key.clone();
        let needs_order = {
            let plan = self.plan_mut();
            match plan.steps.last_mut() {
                Some(LogicalStep::Order(order_step)) => {
                    let is_default = matches!(order_step.keys.as_slice(), [OrderKey { spec: OrderKeySpec::Value, .. }]);
                    if is_default {
                        order_step.keys =
                            smallvec::smallvec![OrderKey { spec: OrderKeySpec::Property(key), order: Order::Asc }];
                    } else {
                        order_step.keys.push(OrderKey { spec: OrderKeySpec::Property(key), order: Order::Asc });
                    }
                    false
                }
                _ => true,
            }
        };
        if needs_order {
            self = self.order();
            let plan = self.plan_mut();
            match plan.steps.last_mut() {
                Some(LogicalStep::Order(order_step)) => {
                    let is_default = matches!(order_step.keys.as_slice(), [OrderKey { spec: OrderKeySpec::Value, .. }]);
                    if is_default {
                        order_step.keys =
                            smallvec::smallvec![OrderKey { spec: OrderKeySpec::Property(key2), order: Order::Asc }];
                    } else {
                        order_step.keys.push(OrderKey { spec: OrderKeySpec::Property(key2), order: Order::Asc });
                    }
                }
                _ => unreachable!("order() just pushed an Order step"),
            }
        }
        self
    }

    /// Creates or modulates an `order()` step with an explicit sort direction.
    /// Follows the same accumulate-vs-replace logic as [`by`](Self::by), including
    /// the same rejection immediately after `group()`/`group_count()`.
    fn order_by(mut self, key: impl Into<SmolStr>, order: Order) -> Self {
        if follows_group_step(self.plan_mut()) {
            self.record_error(by_after_group_error("order_by()"));
            return self;
        }
        let key: SmolStr = key.into();
        let needs_order = {
            let plan = self.plan_mut();
            match plan.steps.last_mut() {
                Some(LogicalStep::Order(order_step)) => {
                    let is_default = matches!(order_step.keys.as_slice(), [OrderKey { spec: OrderKeySpec::Value, .. }]);
                    if is_default {
                        order_step.keys =
                            smallvec::smallvec![OrderKey { spec: OrderKeySpec::Property(key.clone()), order }];
                    } else {
                        order_step.keys.push(OrderKey { spec: OrderKeySpec::Property(key.clone()), order });
                    }
                    false
                }
                _ => true,
            }
        };
        if needs_order {
            self = self.order();
            let plan = self.plan_mut();
            match plan.steps.last_mut() {
                Some(LogicalStep::Order(order_step)) => {
                    let is_default = matches!(order_step.keys.as_slice(), [OrderKey { spec: OrderKeySpec::Value, .. }]);
                    if is_default {
                        order_step.keys =
                            smallvec::smallvec![OrderKey { spec: OrderKeySpec::Property(key.clone()), order }];
                    } else {
                        order_step.keys.push(OrderKey { spec: OrderKeySpec::Property(key.clone()), order });
                    }
                }
                _ => unreachable!("order() just pushed an Order step"),
            }
        }
        self
    }

    fn simple_path(mut self) -> Self {
        self.push_step(LogicalStep::SimplePath(SimplePathStep {}));
        self
    }

    fn cyclic_path(mut self) -> Self {
        self.push_step(LogicalStep::CyclicPath(CyclicPathStep {}));
        self
    }

    fn choose(
        mut self,
        mut predicate: GraphTraversal,
        mut true_choice: GraphTraversal,
        false_choice: Option<GraphTraversal>,
    ) -> Self {
        if let Some(err) = predicate.error.take() {
            self.record_error(err);
        }
        if let Some(err) = true_choice.error.take() {
            self.record_error(err);
        }
        let fc = false_choice.map(|mut f| {
            if let Some(err) = f.error.take() {
                self.record_error(err);
            }
            f.into_plan()
        });
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

    fn id(mut self) -> Self {
        self.push_step(LogicalStep::Id(IdStep {}));
        self
    }

    fn label(mut self) -> Self {
        self.push_step(LogicalStep::Label(LabelStep {}));
        self
    }

    /// Extract the rank of the current edge. Edge-only — errors if the upstream
    /// traverser is a vertex (vertices have no rank).
    fn rank(mut self) -> Self {
        self.push_step(LogicalStep::Rank(RankStep {}));
        self
    }

    fn identity(mut self) -> Self {
        self.push_step(LogicalStep::Identity(IdentityStep {}));
        self
    }

    fn constant(mut self, value: impl Into<crate::types::gvalue::Primitive>) -> Self {
        self.push_step(LogicalStep::Constant(ConstantStep { value: value.into() }));
        self
    }

    fn local(mut self, mut traversal: GraphTraversal) -> Self {
        if let Some(err) = traversal.error.take() {
            self.record_error(err);
        }
        self.push_step(LogicalStep::Local(LocalStep { plan: traversal.into_plan() }));
        self
    }

    fn dedup(mut self) -> Self {
        self.push_step(LogicalStep::Dedup(DedupStep {}));
        self
    }

    fn group(mut self) -> Self {
        self.push_step(LogicalStep::Group(GroupStep { key: None }));
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

    fn times(mut self, n: i64) -> Self {
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

// ── ReadTraversal / WriteTraversal ────────────────────────────────────────
// Terminal traversal types live in terminals.rs.
mod terminals;
pub use terminals::{ReadTraversal, WriteTraversal};
