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
            AddEStep, AddVStep, BothEStep, BothStep, CoalesceStep, CountStep, FromStep, HasIdStep, HasLabelStep,
            HasPropertyStep, InEStep, InStep, InVStep, LimitStep, LogicalPlan, LogicalStep, OtherVStep, OutEStep,
            OutStep, OutVStep, PropertiesStep, PropertyStep, ScalarFilterStep, ToStep, UnionStep, ValuesStep,
            WhereStep,
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
    pub fn build<'g>(&self, graph: &'g mut impl GraphCtx) -> Result<BuiltTraversal<'g>, StoreError> {
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
    pub fn V(&mut self, ids: impl IntoIterator<Item = impl Into<i64>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::V(crate::planner::logical_step::VStep {
            ids: ids.into_iter().map(Into::into).collect(),
        }));
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

    pub fn union(&mut self, traversals: Vec<&mut GraphTraversal>) -> &mut Self {
        self.ast
            .steps
            .push(LogicalStep::Union(UnionStep { plans: traversals.into_iter().map(|t| t.build_logical()).collect() }));
        self
    }

    pub fn coalesce(&mut self, traversals: Vec<&mut GraphTraversal>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Coalesce(CoalesceStep {
            plans: traversals.into_iter().map(|t| t.build_logical()).collect(),
        }));
        self
    }
    pub fn limit(&mut self, limit: u32) -> &mut Self {
        self.ast.steps.push(LogicalStep::Limit(LimitStep { limit }));
        self
    }
    pub fn hasId(&mut self, ids: impl IntoIterator<Item = impl Into<i64>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::HasId(HasIdStep { ids: ids.into_iter().map(Into::into).collect() }));
        self
    }
    pub fn properties(&mut self, keys: impl IntoIterator<Item = impl AsRef<str>>) -> &mut Self {
        self.ast.steps.push(LogicalStep::Properties(PropertiesStep {
            property_keys: keys.into_iter().map(|k| SmolStr::new(k.as_ref())).collect(),
        }));
        self
    }
}

use crate::store::RocksStorage;
use std::{path::Path, sync::Arc};

pub fn open_rocks_store<P: AsRef<Path>>(path: Option<P>) -> Result<Arc<RocksStorage>, Box<dyn std::error::Error>> {
    match path {
        Some(pth) => Ok(Arc::new(RocksStorage::open(pth)?)),
        None => {
            let dir = tempfile::tempdir()?;
            Ok(Arc::new(RocksStorage::open(dir.path())?))
        }
    }
}
