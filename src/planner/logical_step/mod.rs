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

//! Engine-agnostic logical IR — the intermediate representation shared by the
//! optimizer and all execution engines.
//!
//! A [`LogicalPlan`] is an ordered list of [`LogicalStep`]s. It carries only
//! *what* to compute, with no reference to any physical operator or execution
//! strategy. The volcano builder ([`engine::volcano::builder`]) is responsible
//! for compiling a `LogicalPlan` into a chain of physical steps.
//!
//! [`engine::volcano::builder`]: crate::engine::volcano::builder

use crate::types::{
    gvalue::Primitive,
    keys::{EdgeKey, Rank, VertexKey},
    prop_key::PropKey,
    LabelId, StoreError,
};
use smallvec::SmallVec;
use std::collections::HashMap;

// Reuse the same rewrite/optimize rule for both LogicalPlan and LogicalStep.
pub type OptimizerRule = fn(&mut LogicalPlan) -> Result<bool, StoreError>;

pub trait Optimizer {
    /// Applies an optimization rule to the implementor.
    fn optimize(&mut self, _: &OptimizerRule) -> Result<bool, StoreError> {
        Ok(false)
    }
}

/// Represents a sequence of logical steps that form a query plan.
#[derive(Clone)]
pub struct LogicalPlan {
    pub steps: Vec<LogicalStep>,
}

impl Optimizer for LogicalPlan {
    fn optimize(&mut self, rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        // apply the rule to each step first, which allows rules to target patterns in sub plans of steps.
        // for most optimizations.
        for step in self.steps.iter_mut() {
            changed |= step.optimize(rule)?;
        }
        // apply the rule to the whole plan.
        // in the whole plan.
        changed |= rule(self)?;
        Ok(changed)
    }
}

/// An enumeration of all possible logical steps in a query plan.
#[derive(Clone)]
pub enum LogicalStep {
    Both(BothStep),
    BothE(BothEStep),
    Count(CountStep),
    HasLabel(HasLabelStep),
    HasProperty(HasPropertyStep),
    In(InStep),
    InE(InEStep),
    Out(OutStep),
    OutE(OutEStep),
    InV(InVStep),
    OtherV(OtherVStep),
    OutV(OutVStep),
    ScalarFilter(ScalarFilterStep),
    Values(ValuesStep),
    Properties(PropertiesStep),
    Where(WhereStep),
    Union(UnionStep),
    AddV(AddVStep),
    AddE(AddEStep),
    From(FromStep),
    To(ToStep),
    Property(PropertyStep),
    V(VStep),
    E(EStep),
    Limit(LimitStep),
    HasId(HasIdStep),
    Coalesce(CoalesceStep),
    EndVertexFilter(EndVertexFilter),
    Drop(DropStep),
    Path(PathStep),
    Dedup(DedupStep),
    Fold(FoldStep),
}

/// Represents a logical `drop` step in a query plan.
#[derive(Clone)]
pub struct DropStep {}

impl Optimizer for DropStep {}

/// Represents a logical `path` step in a query plan.
#[derive(Clone, Debug)]
pub struct PathStep {}

impl Optimizer for PathStep {}

/// Represents a logical `dedup` step in a query plan.
#[derive(Clone, Debug)]
pub struct DedupStep {}

impl Optimizer for DedupStep {}

/// Collects all traversers into a single `GValue::List` (Gremlin `fold()` step).
#[derive(Clone, Debug)]
pub struct FoldStep {}

impl Optimizer for FoldStep {}
/// Implements the `Optimizer` trait for `LogicalStep`, allowing optimization rules to be applied to individual steps.
impl Optimizer for LogicalStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        match self {
            LogicalStep::Where(wh) => changed |= wh.optimize(optimizer_rule)?,
            LogicalStep::Union(u) => changed |= u.optimize(optimizer_rule)?,
            LogicalStep::Coalesce(c) => changed |= c.optimize(optimizer_rule)?,
            _ => {}
        }
        Ok(changed)
    }
}

/// Represents a filter step that checks if the current traverser's vertex ID is among a list of target IDs.
#[derive(Clone)]
pub struct EndVertexFilter {
    pub ids: SmallVec<[VertexKey; 4]>,
}

/// Implements the `Optimizer` trait for `EndVertexFilter`.
impl Optimizer for EndVertexFilter {}

#[derive(Clone)]
pub struct CoalesceStep {
    pub plans: Vec<LogicalPlan>,
}

impl Optimizer for CoalesceStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        for plan in self.plans.iter_mut() {
            changed |= optimizer_rule(plan)?;
        }
        Ok(changed)
    }
}

/// Represents a logical `count` step in a query plan.
#[derive(Clone)]
pub struct CountStep {}

impl Optimizer for CountStep {}
#[derive(Clone)]
/// Represents a logical `both` step, traversing both incoming and outgoing edges.
pub struct BothStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for BothStep {}

/// Represents a logical `bothE` step, traversing both incoming and outgoing edges and returning the edges themselves.
#[derive(Clone)]
pub struct BothEStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The edge rank to filter by, folded in from a trailing `.has("rank", N)` (see
    /// `merge_end_vertex_filter`). `None` means no rank constraint is known at plan time.
    pub rank: Option<Rank>,
}

impl Optimizer for BothEStep {}

/// Represents a logical `hasLabel` step, filtering elements by their label IDs.
#[derive(Clone)]
pub struct HasLabelStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
}

/// Implements the `Optimizer` trait for `HasLabelStep`.
impl Optimizer for HasLabelStep {}

#[derive(Clone)]
pub struct HasPropertyStep {
    pub key: PropKey,
    pub value: Primitive,
}

impl Optimizer for HasPropertyStep {}

/// Represents a logical `in` step, traversing incoming edges and returning the source vertices.
#[derive(Clone)]
pub struct InStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

/// Implements the `Optimizer` trait for `InStep`.
impl Optimizer for InStep {}

#[derive(Clone)]
pub struct InEStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The edge rank to filter by, folded in from a trailing `.has("rank", N)` (see
    /// `merge_end_vertex_filter`). `None` means no rank constraint is known at plan time.
    pub rank: Option<Rank>,
}
impl Optimizer for InEStep {}

/// Represents a logical `out` step, traversing outgoing edges and returning the destination vertices.
#[derive(Clone)]
pub struct OutStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

/// Implements the `Optimizer` trait for `OutStep`.
impl Optimizer for OutStep {}

#[derive(Clone)]
pub struct OutEStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The edge rank to filter by, folded in from a trailing `.has("rank", N)` (see
    /// `merge_end_vertex_filter`). `None` means no rank constraint is known at plan time.
    pub rank: Option<Rank>,
}

/// Implements the `Optimizer` trait for `OutEStep`.
impl Optimizer for OutEStep {}

/// Represents a logical `inV` step, which extracts the incoming vertex from an edge traverser.
#[derive(Clone)]
pub struct InVStep {}

impl Optimizer for InVStep {}

#[derive(Clone)]
pub struct OtherVStep {}

impl Optimizer for OtherVStep {}

/// Represents a logical `outV` step, which extracts the outgoing vertex from an edge traverser.
#[derive(Clone)]
pub struct OutVStep {}

impl Optimizer for OutVStep {}

/// Represents a logical `scalarFilter` step, filtering traversers based on a scalar value.
#[derive(Clone)]
pub struct ScalarFilterStep {
    pub value: Primitive,
}

impl Optimizer for ScalarFilterStep {}

/// Represents a logical `values` step, extracting property values from elements.
#[derive(Clone)]
pub struct ValuesStep {
    pub property_keys: SmallVec<[PropKey; 4]>,
}

/// Implements the `Optimizer` trait for `ValuesStep`.
impl Optimizer for ValuesStep {}

#[derive(Clone)]
pub struct PropertiesStep {
    pub property_keys: SmallVec<[PropKey; 4]>,
}
impl Optimizer for PropertiesStep {}

/// Represents a logical `where` step, applying a sub-plan as a filter.
#[derive(Clone)]
pub struct WhereStep {
    pub plan: LogicalPlan,
}

/// Implements the `Optimizer` trait for `WhereStep`, optimizing its sub-plan.
impl Optimizer for WhereStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        optimizer_rule(&mut self.plan)
    }
}

#[derive(Clone)]
/// Represents a logical `union` step, combining results from multiple sub-plans.
pub struct UnionStep {
    pub plans: SmallVec<[LogicalPlan; 0]>,
}

/// Implements the `Optimizer` trait for `UnionStep`, optimizing its sub-plans.
impl Optimizer for UnionStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        for plan in self.plans.iter_mut() {
            changed |= optimizer_rule(plan)?;
        }
        Ok(changed)
    }
}

#[derive(Clone)]
/// Represents a logical `addV` step, adding a new vertex to the graph.
pub struct AddVStep {
    pub label_id: LabelId,
    pub vertex_id: Option<VertexKey>,
    pub properties: HashMap<PropKey, Primitive>,
}

impl Optimizer for AddVStep {}

/// Represents a logical `addE` step, adding a new edge to the graph.
#[derive(Clone)]
pub struct AddEStep {
    pub label_id: LabelId,
    pub out_v_id: Option<VertexKey>,
    pub in_v_id: Option<VertexKey>,
    pub properties: HashMap<PropKey, Primitive>,
    pub rank: Option<Rank>,
}

impl Optimizer for AddEStep {}

/// Represents a logical `from` step, specifying the source vertex for an edge.
#[derive(Clone)]
pub struct FromStep {
    pub vertex_id: VertexKey,
}

/// Implements the `Optimizer` trait for `FromStep`.
impl Optimizer for FromStep {}

#[derive(Clone)]
pub struct ToStep {
    pub vertex_id: VertexKey,
}

impl Optimizer for ToStep {}

/// Represents a logical `property` step, setting a property on an element.
#[derive(Clone)]
pub struct PropertyStep {
    pub prop_key: PropKey,
    pub prop_value: Primitive,
}

/// Implements the `Optimizer` trait for `PropertyStep`.
impl Optimizer for PropertyStep {}

#[derive(Clone)]
pub struct VStep {
    pub ids: SmallVec<[VertexKey; 4]>,
}

impl Optimizer for VStep {}

#[derive(Clone)]
pub struct EStep {
    pub keys: SmallVec<[EdgeKey; 4]>,
}

impl Optimizer for EStep {}

/// Represents a logical `limit` step, restricting the number of traversers.
#[derive(Clone)]
pub struct LimitStep {
    pub limit: u32,
}

/// Implements the `Optimizer` trait for `LimitStep`.
impl Optimizer for LimitStep {}

#[derive(Clone)]
pub struct HasIdStep {
    pub ids: SmallVec<[VertexKey; 4]>,
}

impl Optimizer for HasIdStep {}
