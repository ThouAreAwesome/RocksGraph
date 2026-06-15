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

use crate::{
    engine::{
        volcano::builder::{PhysicalPlan, PhysicalPlanBuilder},
        GraphCtx,
    },
    planner::{
        apply_rules,
        logical_step::{
            AddEStep, AddVStep, BothEStep, BothStep, CoalesceStep, CountStep, DedupStep, DropStep, FromStep, HasIdStep,
            HasLabelStep, HasPropertyStep, InEStep, InStep, InVStep, LimitStep, LogicalPlan, LogicalStep, OtherVStep,
            OutEStep, OutStep, OutVStep, PathStep, PropertiesStep, PropertyStep, ScalarFilterStep, ToListStep, ToStep,
            UnionStep, ValuesStep, WhereStep,
        },
    },
    types::{GValue, Primitive, StoreError},
};
use smol_str::SmolStr;
use std::collections::HashMap;

// Return type of build() - user-facing iterator
pub struct BuiltTraversal<'g> {
    graph: &'g mut dyn GraphCtx, // #[doc(hidden)] type, but users never name it
    plan: PhysicalPlan,          // fully hidden
}

impl<'g> Iterator for BuiltTraversal<'g> {
    type Item = Result<GValue, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.plan.next(self.graph).map(|res| res.map(|t| t.value.clone())).transpose()
    }
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

/// Entry point for anonymous traversals (sub-traversals).
/// Mimics Gremlin's `__` (double underscore) for nested traversals.
pub fn __() -> GraphTraversal {
    GraphTraversal { ast: GremlinQueryAst { steps: vec![] } }
}

#[allow(non_snake_case)]
impl GraphTraversal {
    fn build<'g>(&self, graph: &'g mut dyn GraphCtx) -> Result<BuiltTraversal<'g>, StoreError> {
        let mut logical = self.build_logical();
        apply_rules(&mut logical)?; // Apply optimization rules to the logical plan.
        let plan = PhysicalPlanBuilder {}.build(&logical)?; // Construct PhysicalPlanBuilder directly.
        Ok(BuiltTraversal { graph, plan })
    }

    fn build_logical(&self) -> LogicalPlan {
        LogicalPlan { steps: self.ast.steps.clone() }
    }

    pub fn has(&mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> &mut Self {
        self.ast.steps.push(LogicalStep::HasProperty(HasPropertyStep { key: key.into(), value: value.into() }));
        self
    }

    /// Spawns a traversal with the `V()` step.
    /// This method is available on `GraphTraversal` for sub-traversals (e.g., `__.V()`).
    pub fn V(&mut self, ids: impl IntoIterator<Item = i64>) -> &mut Self {
        self.ast.steps.push(LogicalStep::V(crate::planner::logical_step::VStep { ids: ids.into_iter().collect() }));
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

    pub fn out(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Out(OutStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn outE(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::OutE(OutEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn r#in(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::In(InStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn inE(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::InE(InEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn both(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Both(BothStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn bothE(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::BothE(BothEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }

    pub fn count(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::Count(CountStep {}));
        self
    }

    pub fn hasLabel(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        self.ast
            .steps
            .push(LogicalStep::HasLabel(HasLabelStep { label_ids: labels.into_iter().map(Into::into).collect() }));
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

    pub fn is(&mut self, value: impl Into<Primitive>) -> &mut Self {
        self.ast.steps.push(LogicalStep::ScalarFilter(ScalarFilterStep { value: value.into() }));
        self
    }

    pub fn property(&mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Property(PropertyStep { prop_key: key.into(), prop_value: value.into() }));
        self
    }

    pub fn values(&mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Values(ValuesStep {
            property_keys: keys.into_iter().map(|k| SmolStr::new(k.as_ref())).collect(),
        }));
        self
    }

    pub fn r#where(&mut self, traversal: &mut GraphTraversal) -> &mut Self {
        self.ast.steps.push(LogicalStep::Where(WhereStep { plan: traversal.build_logical() }));
        self
    }

    pub fn union<'a>(&mut self, traversals: impl IntoIterator<Item = &'a mut GraphTraversal>) -> &mut Self {
        self.ast
            .steps
            .push(LogicalStep::Union(UnionStep { plans: traversals.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }

    pub fn coalesce<'a>(&mut self, traversals: impl IntoIterator<Item = &'a mut GraphTraversal>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Coalesce(CoalesceStep {
            plans: traversals.into_iter().map(|t| t.build_logical()).collect(),
        }));
        self
    }
    pub fn limit(&mut self, limit: u32) -> &mut Self {
        self.ast.steps.push(LogicalStep::Limit(LimitStep { limit }));
        self
    }
    pub fn hasId(&mut self, ids: impl IntoIterator<Item = i64>) -> &mut Self {
        self.ast.steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() }));
        self
    }
    pub fn properties(&mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(|k| SmolStr::new(k.as_ref())).collect(),
        }));
        self
    }
    pub fn path(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::Path(PathStep {}));
        self
    }

    pub fn dedup(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::Dedup(DedupStep {}));
        self
    }

    pub fn toList(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::ToList(ToListStep {}));
        self
    }
}

// ── Traversal builder types ───────────────────────────────────────────────────

/// Shared read step methods for both [`ReadTraversal`] and [`WriteTraversal`].
///
/// Each method appends one [`LogicalStep`] to the AST and returns `&mut Self`
/// for fluent chaining. Terminal operations (`next`) are
/// inherent methods on each concrete type.
pub trait TraversalBuilder {
    #[doc(hidden)]
    fn ast_mut(&mut self) -> &mut GremlinQueryAst;

    #[allow(non_snake_case)]
    fn V(&mut self, ids: impl IntoIterator<Item = i64>) -> &mut Self {
        use crate::planner::logical_step::VStep;
        self.ast_mut().steps.push(LogicalStep::V(VStep { ids: ids.into_iter().collect() }));
        self
    }
    fn out(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::OutStep;
        self.ast_mut().steps.push(LogicalStep::Out(OutStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    fn in_(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::InStep;
        self.ast_mut().steps.push(LogicalStep::In(InStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    fn both(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::BothStep;
        self.ast_mut().steps.push(LogicalStep::Both(BothStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn outE(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::OutEStep;
        self.ast_mut().steps.push(LogicalStep::OutE(OutEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn inE(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::InEStep;
        self.ast_mut().steps.push(LogicalStep::InE(InEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn bothE(&mut self, labels: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::BothEStep;
        self.ast_mut().steps.push(LogicalStep::BothE(BothEStep {
            label_ids: labels.into_iter().map(Into::into).collect(),
            end_vertex_ids: None,
        }));
        self
    }
    #[allow(non_snake_case)]
    fn inV(&mut self) -> &mut Self {
        use crate::planner::logical_step::InVStep;
        self.ast_mut().steps.push(LogicalStep::InV(InVStep {}));
        self
    }
    #[allow(non_snake_case)]
    fn outV(&mut self) -> &mut Self {
        use crate::planner::logical_step::OutVStep;
        self.ast_mut().steps.push(LogicalStep::OutV(OutVStep {}));
        self
    }
    #[allow(non_snake_case)]
    fn otherV(&mut self) -> &mut Self {
        use crate::planner::logical_step::OtherVStep;
        self.ast_mut().steps.push(LogicalStep::OtherV(OtherVStep {}));
        self
    }
    fn has(&mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::HasProperty(HasPropertyStep { key: key.into(), value: value.into() }));
        self
    }
    #[allow(non_snake_case)]
    fn hasLabel(&mut self, label_ids: impl IntoIterator<Item = impl Into<u16>>) -> &mut Self {
        use crate::planner::logical_step::HasLabelStep;
        self.ast_mut()
            .steps
            .push(LogicalStep::HasLabel(HasLabelStep { label_ids: label_ids.into_iter().map(Into::into).collect() }));
        self
    }
    #[allow(non_snake_case)]
    fn hasId(&mut self, ids: impl IntoIterator<Item = i64>) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() }));
        self
    }
    fn values(&mut self, keys: impl IntoIterator<Item = impl Into<SmolStr>>) -> &mut Self {
        self.ast_mut()
            .steps
            .push(LogicalStep::Values(ValuesStep { property_keys: keys.into_iter().map(Into::into).collect() }));
        self
    }
    fn count(&mut self) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::Count(CountStep {}));
        self
    }
    fn limit(&mut self, n: u32) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::Limit(LimitStep { limit: n }));
        self
    }
    fn path(&mut self) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::Path(PathStep {}));
        self
    }
    fn dedup(&mut self) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::Dedup(DedupStep {}));
        self
    }
    #[allow(non_snake_case)]
    fn toList(&mut self) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::ToList(ToListStep {}));
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
    fn r#where(&mut self, sub: &mut GraphTraversal) -> &mut Self {
        self.ast_mut().steps.push(LogicalStep::Where(WhereStep { plan: sub.build_logical() }));
        self
    }
    /// Evaluate each sub-traversal and emit results from the first that yields
    /// at least one result (short-circuits).
    ///
    /// ```ignore
    /// tx.g().V([id]).coalesce([
    ///     __().values(["name"]),
    ///     __().addV(LABEL).property("id", id).property("name", name)
    /// ]).next()?
    /// ```
    fn coalesce<'a>(&mut self, subs: impl IntoIterator<Item = &'a mut GraphTraversal>) -> &mut Self {
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
    fn union<'a>(&mut self, subs: impl IntoIterator<Item = &'a mut GraphTraversal>) -> &mut Self {
        self.ast_mut()
            .steps
            .push(LogicalStep::Union(UnionStep { plans: subs.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }
}

// ── ReadTraversal ─────────────────────────────────────────────────────────────

/// A read-only traversal bound to a [`ReadSession`] context.
///
/// Write steps (`addV`, `addE`, `property`, `drop`) are not available.
/// Attempting to call them is a compile-time error.
pub struct ReadTraversal<'s> {
    ast: GremlinQueryAst,
    ctx: &'s mut dyn GraphCtx,
}

impl<'s> ReadTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { ast: GremlinQueryAst { steps: vec![] }, ctx }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<GValue>, StoreError> {
        let gt = GraphTraversal { ast: self.ast.clone() };
        gt.build(self.ctx)?.next().transpose()
    }
}

impl TraversalBuilder for ReadTraversal<'_> {
    fn ast_mut(&mut self) -> &mut GremlinQueryAst {
        &mut self.ast
    }
}

// ── WriteTraversal ────────────────────────────────────────────────────────────

/// A read-write traversal bound to a [`TxSession`] context.
///
/// Includes all read steps from [`TraversalBuilder`] plus mutation steps.
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
    pub fn addV(&mut self, label_id: u16) -> &mut Self {
        self.ast.steps.push(LogicalStep::AddV(AddVStep { label_id, vertex_id: None, properties: HashMap::new() }));
        self
    }

    #[allow(non_snake_case)]
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

    pub fn property(&mut self, key: impl Into<SmolStr>, value: impl Into<Primitive>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Property(PropertyStep { prop_key: key.into(), prop_value: value.into() }));
        self
    }

    pub fn drop(&mut self) -> &mut Self {
        self.ast.steps.push(LogicalStep::Drop(DropStep {}));
        self
    }

    // ── Terminal ops ──────────────────────────────────────────────────────────

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<GValue>, StoreError> {
        let gt = GraphTraversal { ast: self.ast.clone() };
        gt.build(self.ctx)?.next().transpose()
    }
}

impl TraversalBuilder for WriteTraversal<'_> {
    fn ast_mut(&mut self) -> &mut GremlinQueryAst {
        &mut self.ast
    }
}
