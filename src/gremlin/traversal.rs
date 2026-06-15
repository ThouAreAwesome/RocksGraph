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
//! 1. **Source** — obtain a traversal from a session: `snap.g()` → [`ReadTraversal`],
//!    `tx.g()` → [`WriteTraversal`].
//! 2. **Steps** — chain pipeline steps: `.V([1])`, `.out([KNOWS])`, `.values(["name"])`, …
//!    Every step method takes `self` by value and returns `Self`, so each call moves
//!    the traversal forward (no hidden mutation through `&mut`).
//! 3. **Terminal** — execute and collect results with one of:
//!    - [`ReadTraversal::next`] / [`WriteTraversal::next`] — first element as `Option<GValue>`
//!    - [`ReadTraversal::to_list`] / [`WriteTraversal::to_list`] — all elements as `Vec<GValue>`
//!    - [`ReadTraversal::iter`] / [`WriteTraversal::iter`] — lazy [`BuiltTraversal`] iterator
//!
//! Terminal methods consume the traversal and build the physical plan exactly once.
//! There is no hidden re-execution: calling `.iter()?` and then advancing the
//! returned iterator is the correct way to read multiple results.
//!
//! # Sub-traversals
//!
//! [`__`] creates an anonymous [`GraphTraversal`] used inside `where`, `coalesce`,
//! and `union`.  These also use move semantics, so `__().out([A]).hasId([x])` is
//! just a chain of value-returning calls.
//!
//! # TinkerPop alignment
//!
//! | TinkerPop (Java) | RocksGraph (Rust) |
//! |---|---|
//! | `t.next()` | `t.next()` — returns `Result<Option<GValue>>` (like `tryNext()`) |
//! | `t.toList()` | `t.to_list()` — terminal, returns `Result<Vec<GValue>>` |
//! | `t.fold()` | `.fold()` pipeline step — collects into `GValue::List` mid-pipeline |
//! | iterate `Traversal` | `t.iter()?` → `BuiltTraversal` which is `Iterator` |

use crate::{
    engine::{
        volcano::builder::{PhysicalPlan, PhysicalPlanBuilder},
        GraphCtx,
    },
    planner::{
        apply_rules,
        logical_step::{
            AddEStep, AddVStep, BothEStep, BothStep, CoalesceStep, CountStep, DedupStep, DropStep, FoldStep, FromStep,
            HasIdStep, HasLabelStep, HasPropertyStep, InEStep, InStep, InVStep, LimitStep, LogicalPlan, LogicalStep,
            OtherVStep, OutEStep, OutStep, OutVStep, PathStep, PropertiesStep, PropertyStep, ScalarFilterStep, ToStep,
            UnionStep, ValuesStep, WhereStep,
        },
    },
    types::{GValue, Primitive, StoreError},
};
use smol_str::SmolStr;
use std::collections::HashMap;

// ── BuiltTraversal ────────────────────────────────────────────────────────────

/// The result of building a traversal — a pull-based lazy iterator over results.
///
/// Obtained from [`ReadTraversal::iter`] or [`WriteTraversal::iter`].  Implements
/// [`Iterator`]`<Item = Result<GValue, StoreError>>` so it can be used in `for`
/// loops or with standard iterator combinators.
///
/// The traversal is executed lazily: each call to [`Iterator::next`] pulls one
/// result from the volcano pipeline.
pub struct BuiltTraversal<'g> {
    graph: &'g mut dyn GraphCtx,
    plan: PhysicalPlan,
}

impl<'g> Iterator for BuiltTraversal<'g> {
    type Item = Result<GValue, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.plan.next(self.graph).map(|res| res.map(|t| t.value.clone())).transpose()
    }
}

// ── GremlinQueryAst ───────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct GremlinQueryAst {
    pub steps: Vec<LogicalStep>,
}

// ── GraphTraversal (anonymous / sub-traversal) ────────────────────────────────

/// An anonymous traversal for use inside `where`, `coalesce`, and `union`.
///
/// Obtain one with [`__`].  All step methods take `self` by value and return
/// `Self`, making the chain a pure sequence of moves with no hidden state.
///
/// `GraphTraversal` is `#[doc(hidden)]` — users interact with it only through
/// the `__()` helper and the sub-traversal parameters of `where`/`union`/`coalesce`.
#[derive(Clone)]
pub struct GraphTraversal {
    ast: GremlinQueryAst,
}

/// Entry point for anonymous sub-traversals (mirrors Gremlin's `__`).
///
/// ```ignore
/// snap.g().V([]).outE([KNOWS]).r#where(__().otherV().hasId([2])).next()?
/// ```
pub fn __() -> GraphTraversal {
    GraphTraversal { ast: GremlinQueryAst { steps: vec![] } }
}

#[allow(non_snake_case)]
impl GraphTraversal {
    /// Compile this traversal to an optimized [`LogicalPlan`] and wire it to a
    /// physical volcano pipeline, ready for execution.
    pub(crate) fn build(self, graph: &mut dyn GraphCtx) -> Result<BuiltTraversal<'_>, StoreError> {
        let mut logical = self.build_logical();
        apply_rules(&mut logical)?;
        let plan = PhysicalPlanBuilder {}.build(&logical)?;
        Ok(BuiltTraversal { graph, plan })
    }

    /// Convert this traversal into a [`LogicalPlan`], consuming it.
    ///
    /// Called when a sub-traversal is compiled into a parent step (e.g. `WhereStep`).
    /// Steps are moved rather than cloned because the `GraphTraversal` is consumed.
    pub(crate) fn build_logical(self) -> LogicalPlan {
        LogicalPlan { steps: self.ast.steps }
    }

    pub fn has(mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> Self {
        self.ast.steps.push(LogicalStep::HasProperty(HasPropertyStep { key: key.into(), value: value.into() }));
        self
    }

    /// Seed the traversal with the given vertex IDs (Gremlin `V()` step).
    pub fn V(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        self.ast.steps.push(LogicalStep::V(crate::planner::logical_step::VStep { ids: ids.into_iter().collect() }));
        self
    }

    pub fn addV(mut self, label_id: u16) -> Self {
        self.ast.steps.push(LogicalStep::AddV(AddVStep { label_id, vertex_id: None, properties: HashMap::new() }));
        self
    }

    pub fn addE(mut self, label_id: u16) -> Self {
        self.ast.steps.push(LogicalStep::AddE(AddEStep {
            label_id,
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.ast.steps.push(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.ast.steps.push(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn out(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast.steps.push(LogicalStep::Out(OutStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn outE(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast.steps.push(LogicalStep::OutE(OutEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn r#in(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast.steps.push(LogicalStep::In(InStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn inE(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast.steps.push(LogicalStep::InE(InEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn both(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast.steps.push(LogicalStep::Both(BothStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn bothE(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast.steps.push(LogicalStep::BothE(BothEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn count(mut self) -> Self {
        self.ast.steps.push(LogicalStep::Count(CountStep {}));
        self
    }

    pub fn hasLabel(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        self.ast
            .steps
            .push(LogicalStep::HasLabel(HasLabelStep { label_ids: labels.into_iter().map(Into::into).collect() }));
        self
    }

    pub fn inV(mut self) -> Self {
        self.ast.steps.push(LogicalStep::InV(InVStep {}));
        self
    }

    pub fn otherV(mut self) -> Self {
        self.ast.steps.push(LogicalStep::OtherV(OtherVStep {}));
        self
    }

    pub fn outV(mut self) -> Self {
        self.ast.steps.push(LogicalStep::OutV(OutVStep {}));
        self
    }

    pub fn is(mut self, value: impl Into<Primitive>) -> Self {
        self.ast.steps.push(LogicalStep::ScalarFilter(ScalarFilterStep { value: value.into() }));
        self
    }

    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> Self {
        self.ast.steps.push(LogicalStep::Property(PropertyStep { prop_key: key.into(), prop_value: value.into() }));
        self
    }

    pub fn values(mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.ast.steps.push(LogicalStep::Values(ValuesStep {
            property_keys: keys.into_iter().map(|k| SmolStr::new(k.as_ref())).collect(),
        }));
        self
    }

    pub fn r#where(mut self, sub: GraphTraversal) -> Self {
        self.ast.steps.push(LogicalStep::Where(WhereStep { plan: sub.build_logical() }));
        self
    }

    pub fn union(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.ast
            .steps
            .push(LogicalStep::Union(UnionStep { plans: subs.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }

    pub fn coalesce(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.ast
            .steps
            .push(LogicalStep::Coalesce(CoalesceStep { plans: subs.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }

    pub fn limit(mut self, limit: u32) -> Self {
        self.ast.steps.push(LogicalStep::Limit(LimitStep { limit }));
        self
    }

    pub fn hasId(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        self.ast.steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() }));
        self
    }

    pub fn properties(mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.ast.steps.push(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(|k| SmolStr::new(k.as_ref())).collect(),
        }));
        self
    }

    pub fn path(mut self) -> Self {
        self.ast.steps.push(LogicalStep::Path(PathStep {}));
        self
    }

    pub fn dedup(mut self) -> Self {
        self.ast.steps.push(LogicalStep::Dedup(DedupStep {}));
        self
    }

    /// Collect all traversers into a single `GValue::List` mid-pipeline (Gremlin `fold()` step).
    ///
    /// Use this when you need the list as an intermediate value inside a larger
    /// traversal (e.g. inside `coalesce`).  To collect top-level results, prefer
    /// the terminal [`ReadTraversal::to_list`] / [`WriteTraversal::to_list`] instead.
    pub fn fold(mut self) -> Self {
        self.ast.steps.push(LogicalStep::Fold(FoldStep {}));
        self
    }
}

// ── TraversalBuilder ──────────────────────────────────────────────────────────

/// Shared read pipeline steps for both [`ReadTraversal`] and [`WriteTraversal`].
///
/// Every method takes `self` by value and returns `Self`, so the entire traversal
/// chain is a sequence of moves — no hidden `&mut` aliasing.  Terminal operations
/// (`iter`, `next`, `to_list`) are inherent methods on each concrete type.
pub trait TraversalBuilder: Sized {
    #[doc(hidden)]
    fn ast_mut(&mut self) -> &mut GremlinQueryAst;

    #[allow(non_snake_case)]
    fn V(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        use crate::planner::logical_step::VStep;
        self.ast_mut().steps.push(LogicalStep::V(VStep { ids: ids.into_iter().collect() }));
        self
    }
    fn out(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::OutStep;
        self.ast_mut().steps.push(LogicalStep::Out(OutStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    fn in_(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::InStep;
        self.ast_mut().steps.push(LogicalStep::In(InStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    fn both(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::BothStep;
        self.ast_mut().steps.push(LogicalStep::Both(BothStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn outE(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::OutEStep;
        self.ast_mut().steps.push(LogicalStep::OutE(OutEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn inE(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::InEStep;
        self.ast_mut().steps.push(LogicalStep::InE(InEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn bothE(mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::BothEStep;
        self.ast_mut().steps.push(LogicalStep::BothE(BothEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn inV(mut self) -> Self {
        use crate::planner::logical_step::InVStep;
        self.ast_mut().steps.push(LogicalStep::InV(InVStep {}));
        self
    }
    #[allow(non_snake_case)]
    fn outV(mut self) -> Self {
        use crate::planner::logical_step::OutVStep;
        self.ast_mut().steps.push(LogicalStep::OutV(OutVStep {}));
        self
    }
    #[allow(non_snake_case)]
    fn otherV(mut self) -> Self {
        use crate::planner::logical_step::OtherVStep;
        self.ast_mut().steps.push(LogicalStep::OtherV(OtherVStep {}));
        self
    }
    fn has(mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> Self {
        self.ast_mut().steps.push(LogicalStep::HasProperty(HasPropertyStep { key: key.into(), value: value.into() }));
        self
    }
    #[allow(non_snake_case)]
    fn hasLabel(mut self, label_ids: impl IntoIterator<Item = impl Into<u16>>) -> Self {
        use crate::planner::logical_step::HasLabelStep;
        self.ast_mut()
            .steps
            .push(LogicalStep::HasLabel(HasLabelStep { label_ids: label_ids.into_iter().map(Into::into).collect() }));
        self
    }
    #[allow(non_snake_case)]
    fn hasId(mut self, ids: impl IntoIterator<Item = i64>) -> Self {
        self.ast_mut().steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() }));
        self
    }
    fn values(mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> Self {
        self.ast_mut()
            .steps
            .push(LogicalStep::Values(ValuesStep { property_keys: keys.into_iter().map(Into::into).collect() }));
        self
    }
    fn count(mut self) -> Self {
        self.ast_mut().steps.push(LogicalStep::Count(CountStep {}));
        self
    }
    fn limit(mut self, n: u32) -> Self {
        self.ast_mut().steps.push(LogicalStep::Limit(LimitStep { limit: n }));
        self
    }
    fn path(mut self) -> Self {
        self.ast_mut().steps.push(LogicalStep::Path(PathStep {}));
        self
    }
    fn dedup(mut self) -> Self {
        self.ast_mut().steps.push(LogicalStep::Dedup(DedupStep {}));
        self
    }
    /// Collect all traversers into a single `GValue::List` mid-pipeline.
    ///
    /// This is the pipeline-step form of `fold()`.  To collect top-level results
    /// use the terminal [`to_list`](ReadTraversal::to_list) method instead.
    fn fold(mut self) -> Self {
        self.ast_mut().steps.push(LogicalStep::Fold(FoldStep {}));
        self
    }
    /// Filter traversers using an anonymous sub-traversal.
    ///
    /// The sub-traversal is built with [`__`] and carries no execution context
    /// — it is compiled to a logical plan and evaluated at query execution time.
    ///
    /// ```ignore
    /// snap.g().V([]).outE([EDGE]).r#where(__().otherV().hasId([dst])).next()?
    /// ```
    fn r#where(mut self, sub: GraphTraversal) -> Self {
        self.ast_mut().steps.push(LogicalStep::Where(WhereStep { plan: sub.build_logical() }));
        self
    }
    /// Evaluate each sub-traversal and emit results from the first that yields
    /// at least one result (short-circuits).
    ///
    /// ```ignore
    /// tx.g().V([id]).coalesce([
    ///     __().values(["name"]),
    ///     __().addV(LABEL).property("id", id).property("name", name),
    /// ]).next()?
    /// ```
    fn coalesce(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.ast_mut()
            .steps
            .push(LogicalStep::Coalesce(CoalesceStep { plans: subs.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }
    /// Evaluate all sub-traversals and merge their result streams.
    ///
    /// ```ignore
    /// snap.g().V([id]).union([__().outE([A]), __().outE([B])]).count().next()?
    /// ```
    fn union(mut self, subs: impl IntoIterator<Item = GraphTraversal>) -> Self {
        self.ast_mut()
            .steps
            .push(LogicalStep::Union(UnionStep { plans: subs.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }
}

// ── ReadTraversal ─────────────────────────────────────────────────────────────

/// A read-only traversal bound to a [`ReadSession`](crate::api::ReadSession) context.
///
/// Obtained from [`ReadSession::g`](crate::api::ReadSession::g).  Write steps
/// (`addV`, `addE`, `property`, `drop`) are not available — attempting to call them
/// is a compile-time error.
///
/// # Execution
///
/// All step methods take `self` by value and return `Self`.  Execute the traversal
/// with one of the terminal methods:
///
/// ```ignore
/// // First result only
/// let v = snap.g().V([1]).out([KNOWS]).next()?;              // Option<GValue>
///
/// // All results
/// let names = snap.g().V([1]).values(["name"]).to_list()?;   // Vec<GValue>
///
/// // Lazy iterator
/// for item in snap.g().V([]).out([KNOWS]).iter()? { ... }
/// ```
pub struct ReadTraversal<'s> {
    ast: GremlinQueryAst,
    ctx: &'s mut dyn GraphCtx,
}

impl<'s> ReadTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { ast: GremlinQueryAst { steps: vec![] }, ctx }
    }

    /// Build the physical plan and return a lazy iterator over all results.
    ///
    /// This is the primary execution method.  The traversal is consumed (built
    /// exactly once) and the returned [`BuiltTraversal`] advances through results
    /// one at a time on each [`Iterator::next`] call.
    ///
    /// ```ignore
    /// for item in snap.g().V([]).out([KNOWS]).iter()? {
    ///     println!("{:?}", item?);
    /// }
    /// ```
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        GraphTraversal { ast: self.ast }.build(self.ctx)
    }

    /// Execute the traversal and return the first result (Gremlin `tryNext()`).
    ///
    /// Returns `Ok(None)` when the traversal produces no results.
    pub fn next(self) -> Result<Option<GValue>, StoreError> {
        self.iter()?.next().transpose()
    }

    /// Execute the traversal and collect all results into a `Vec` (Gremlin `toList()`).
    pub fn to_list(self) -> Result<Vec<GValue>, StoreError> {
        self.iter()?.collect()
    }
}

impl TraversalBuilder for ReadTraversal<'_> {
    fn ast_mut(&mut self) -> &mut GremlinQueryAst {
        &mut self.ast
    }
}

// ── WriteTraversal ────────────────────────────────────────────────────────────

/// A read-write traversal bound to a [`TxSession`](crate::api::TxSession) context.
///
/// Obtained from [`TxSession::g`](crate::api::TxSession::g).  Includes all read
/// steps from [`TraversalBuilder`] plus mutation steps (`addV`, `addE`, `property`,
/// `drop`).
///
/// # Execution
///
/// All step methods take `self` by value and return `Self`.  Execute with one of:
///
/// ```ignore
/// // Mutations — use next() to execute; the returned GValue is the created element
/// tx.g().addV(PERSON).property("id", 1).property("name", "alice").next()?;
///
/// // Reads inside a transaction
/// let names = tx.g().V([1]).out([KNOWS]).values(["name"]).to_list()?;
/// ```
pub struct WriteTraversal<'s> {
    ast: GremlinQueryAst,
    ctx: &'s mut dyn GraphCtx,
}

impl<'s> WriteTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { ast: GremlinQueryAst { steps: vec![] }, ctx }
    }

    // ── Write steps ───────────────────────────────────────────────────────────

    #[allow(non_snake_case)]
    pub fn addV(mut self, label_id: u16) -> Self {
        self.ast.steps.push(LogicalStep::AddV(AddVStep { label_id, vertex_id: None, properties: HashMap::new() }));
        self
    }

    #[allow(non_snake_case)]
    pub fn addE(mut self, label_id: u16) -> Self {
        self.ast.steps.push(LogicalStep::AddE(AddEStep {
            label_id,
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.ast.steps.push(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.ast.steps.push(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> Self {
        self.ast.steps.push(LogicalStep::Property(PropertyStep { prop_key: key.into(), prop_value: value.into() }));
        self
    }

    pub fn drop(mut self) -> Self {
        self.ast.steps.push(LogicalStep::Drop(DropStep {}));
        self
    }

    // ── Terminal ops ──────────────────────────────────────────────────────────

    /// Build the physical plan and return a lazy iterator over all results.
    ///
    /// The traversal is consumed (built exactly once) and the returned
    /// [`BuiltTraversal`] advances through results one at a time.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        GraphTraversal { ast: self.ast }.build(self.ctx)
    }

    /// Execute the traversal and return the first result (Gremlin `tryNext()`).
    ///
    /// Returns `Ok(None)` when the traversal produces no results.  This is the
    /// standard way to execute mutation traversals (`addV`, `addE`, etc.) where
    /// you just need confirmation that the step ran.
    pub fn next(self) -> Result<Option<GValue>, StoreError> {
        self.iter()?.next().transpose()
    }

    /// Execute the traversal and collect all results into a `Vec` (Gremlin `toList()`).
    pub fn to_list(self) -> Result<Vec<GValue>, StoreError> {
        self.iter()?.collect()
    }
}

impl TraversalBuilder for WriteTraversal<'_> {
    fn ast_mut(&mut self) -> &mut GremlinQueryAst {
        &mut self.ast
    }
}
