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
    gvalue::{Primitive, PrimitivePredicate},
    keys::{Rank, VertexKey},
    prop_key::PropKey,
    StoreError, ORDER_KEY_INLINE, PIPELINE_BATCH_INLINE, STEP_LABEL_INLINE,
};
use smallvec::SmallVec;
use smol_str::SmolStr;
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
        for step in self.steps.iter_mut() {
            changed |= step.optimize(rule)?;
        }
        changed |= rule(self)?;
        Ok(changed)
    }
}

impl LogicalPlan {
    /// Returns `true` if any step in this plan depends on path tracking
    /// (`as()`, `select()`, `path()`). When false, the builder can skip
    /// parent-chain construction, eliminating Rc::clone overhead.
    pub fn has_path_consumer(&self) -> bool {
        fn scan(steps: &[LogicalStep]) -> bool {
            use LogicalStep::*;
            for s in steps {
                match s {
                    As(_) | Select(_) | Path(_) | SimplePath(_) | CyclicPath(_) => return true,
                    Not(NotStep { plan }) if scan(&plan.steps) => {
                        return true;
                    }
                    And(AndStep { plans }) | Or(OrStep { plans }) => {
                        for p in plans {
                            if scan(&p.steps) {
                                return true;
                            }
                        }
                    }
                    Union(UnionStep { plans }) => {
                        for p in plans {
                            if scan(&p.steps) {
                                return true;
                            }
                        }
                    }
                    Coalesce(CoalesceStep { plans }) => {
                        for p in plans {
                            if scan(&p.steps) {
                                return true;
                            }
                        }
                    }
                    Where(WhereStep { plan }) if scan(&plan.steps) => {
                        return true;
                    }
                    Repeat(RepeatStep { body, until, emit, .. }) => {
                        if scan(&body.steps) {
                            return true;
                        }
                        if let Some(p) = until {
                            if scan(&p.steps) {
                                return true;
                            }
                        }
                        if let EmitSpec::If(p) = emit {
                            if scan(&p.steps) {
                                return true;
                            }
                        }
                    }
                    Choose(ChooseStep { predicate, true_choice, false_choice, .. }) => {
                        if scan(&predicate.steps) {
                            return true;
                        }
                        if scan(&true_choice.steps) {
                            return true;
                        }
                        if let Some(fc) = false_choice {
                            if scan(&fc.steps) {
                                return true;
                            }
                        }
                    }
                    Local(LocalStep { plan }) if scan(&plan.steps) => {
                        return true;
                    }
                    _ => {}
                }
            }
            false
        }
        scan(&self.steps)
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
    Repeat(RepeatStep),
    Not(NotStep),
    And(AndStep),
    Or(OrStep),
    Sum(SumStep),
    Mean(MeanStep),
    Max(MaxStep),
    Min(MinStep),
    Unfold(UnfoldStep),
    As(AsStep),
    Select(SelectStep),
    Range(RangeStep),
    Skip(SkipStep),
    Tail(TailStep),
    Order(OrderStep),
    SimplePath(SimplePathStep),
    CyclicPath(CyclicPathStep),
    Choose(ChooseStep),
    Group(GroupStep),
    GroupCount(GroupCountStep),
    Id(IdStep),
    Label(LabelStep),
    Rank(RankStep),
    HasRank(HasRankStep),
    Constant(ConstantStep),
    Identity(IdentityStep),
    Local(LocalStep),
}

/// Specifies when a repeat step should emit intermediate results.
#[derive(Clone)]
pub enum EmitSpec {
    Never,
    Always,
    If(LogicalPlan),
}

/// Represents a logical `repeat` step — a variable-length looping construct.
#[derive(Clone)]
pub struct RepeatStep {
    pub body: LogicalPlan,
    pub until: Option<LogicalPlan>,
    pub times: Option<u32>,
    pub emit: EmitSpec,
}

impl Optimizer for RepeatStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        changed |= optimizer_rule(&mut self.body)?;
        if let Some(ref mut until) = self.until {
            changed |= optimizer_rule(until)?;
        }
        if let EmitSpec::If(ref mut plan) = self.emit {
            changed |= optimizer_rule(plan)?;
        }
        Ok(changed)
    }
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

/// Negates a sub-traversal filter: passes the traverser if the sub-plan yields nothing.
#[derive(Clone)]
pub struct NotStep {
    pub plan: LogicalPlan,
}

impl Optimizer for NotStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        optimizer_rule(&mut self.plan)
    }
}

/// Passes the traverser if all sub-plans yield results (short-circuit on first failure).
#[derive(Clone)]
pub struct AndStep {
    pub plans: Vec<LogicalPlan>,
}

impl Optimizer for AndStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        for plan in self.plans.iter_mut() {
            changed |= optimizer_rule(plan)?;
        }
        Ok(changed)
    }
}

/// Passes the traverser if any sub-plan yields results (short-circuit on first success).
#[derive(Clone)]
pub struct OrStep {
    pub plans: Vec<LogicalPlan>,
}

impl Optimizer for OrStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        for plan in self.plans.iter_mut() {
            changed |= optimizer_rule(plan)?;
        }
        Ok(changed)
    }
}

/// Sums all numeric traverser values into a single scalar (Gremlin `sum()` step).
#[derive(Clone, Debug)]
pub struct SumStep {}

impl Optimizer for SumStep {}

/// Averages all numeric traverser values, always returning `Float64`.
#[derive(Clone, Debug)]
pub struct MeanStep {}

impl Optimizer for MeanStep {}

/// Finds the maximum numeric traverser value.
#[derive(Clone, Debug)]
pub struct MaxStep {}

impl Optimizer for MaxStep {}

/// Finds the minimum numeric traverser value.
#[derive(Clone, Debug)]
pub struct MinStep {}

impl Optimizer for MinStep {}

/// Unfolds a `GValue::List` into individual traversers (inverse of `fold()`).
#[derive(Clone, Debug)]
pub struct UnfoldStep {}

impl Optimizer for UnfoldStep {}

/// Labels the current traverser for later retrieval via `select()`.
#[derive(Clone, Debug)]
pub struct AsStep {
    pub labels: SmallVec<[SmolStr; STEP_LABEL_INLINE]>,
}

impl Optimizer for AsStep {}

/// Retrieves traversers previously labeled with `as()`.
#[derive(Clone, Debug)]
pub struct SelectStep {
    pub labels: SmallVec<[SmolStr; STEP_LABEL_INLINE]>,
}

impl Optimizer for SelectStep {}

/// Keeps traversers in the half-open range `[lo, hi)`.
#[derive(Clone, Debug)]
pub struct RangeStep {
    pub lo: u32,
    pub hi: u32,
}
impl Optimizer for RangeStep {}

/// Skips the first `n` traversers, emitting the rest.
#[derive(Clone, Debug)]
pub struct SkipStep {
    pub n: u32,
}
impl Optimizer for SkipStep {}

/// Collects all traversers and emits only the last `n`.
#[derive(Clone, Debug)]
pub struct TailStep {
    pub n: u32,
}
impl Optimizer for TailStep {}

/// Sorting direction.
#[derive(Clone, Debug, PartialEq, Eq, Copy)]
pub enum Order {
    Asc,
    Desc,
}

/// Specifies what to compare when sorting.
#[derive(Clone, Debug)]
pub enum OrderKeySpec {
    /// Compare by the traverser value itself.
    Value,
    /// Compare by a property value (resolved at build time).
    Property(SmolStr),
}

/// A single sort key with direction.
#[derive(Clone, Debug)]
pub struct OrderKey {
    pub spec: OrderKeySpec,
    pub order: Order,
}

/// Sorts traversers using the given key specifications.
#[derive(Clone, Debug)]
pub struct OrderStep {
    pub keys: SmallVec<[OrderKey; ORDER_KEY_INLINE]>,
}
impl Optimizer for OrderStep {}

/// Filters out traversers whose path contains duplicate vertices (keeps simple paths).
#[derive(Clone, Debug)]
pub struct SimplePathStep {}
impl Optimizer for SimplePathStep {}

/// Filters out traversers whose path does NOT contain duplicates (keeps cyclic paths).
#[derive(Clone, Debug)]
pub struct CyclicPathStep {}
impl Optimizer for CyclicPathStep {}

/// Conditional branching: if predicate matches, take true_choice; else take false_choice (or pass-through).
#[derive(Clone)]
pub struct ChooseStep {
    pub predicate: LogicalPlan,
    pub true_choice: LogicalPlan,
    pub false_choice: Option<LogicalPlan>,
}
impl Optimizer for ChooseStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = optimizer_rule(&mut self.predicate)?;
        changed |= optimizer_rule(&mut self.true_choice)?;
        if let Some(ref mut fc) = self.false_choice {
            changed |= optimizer_rule(fc)?;
        }
        Ok(changed)
    }
}

/// Collects traversers into a map, grouped by key. If no key is specified, groups by value.
#[derive(Clone, Debug)]
pub struct GroupStep {
    pub key: Option<SmolStr>,
}
impl Optimizer for GroupStep {}

/// Collects traversers and counts occurrences per key. If no key is specified, counts by value.
#[derive(Clone, Debug)]
pub struct GroupCountStep {
    pub key: Option<SmolStr>,
}
impl Optimizer for GroupCountStep {}

/// Passes each traverser through unchanged (Gremlin `identity()` step).
#[derive(Clone, Debug)]
pub struct IdentityStep {}
impl Optimizer for IdentityStep {}

/// Replaces each traverser with the id of its element (Gremlin `id()` step).
#[derive(Clone, Debug)]
pub struct IdStep {}
impl Optimizer for IdStep {}

/// Replaces each traverser with the label of its element (Gremlin `label()` step).
#[derive(Clone, Debug)]
pub struct LabelStep {}
impl Optimizer for LabelStep {}

/// Replaces each traverser with the rank of its element. Edge-only — rank is the
/// structural multi-edge discriminator, vertices have no rank.
#[derive(Clone, Debug)]
pub struct RankStep {}
impl Optimizer for RankStep {}

/// Filters traversers by the rank of the edge they carry. Edge-only — a vertex
/// traverser never matches.
#[derive(Clone, Debug)]
pub struct HasRankStep {
    pub pred: PrimitivePredicate,
}
impl Optimizer for HasRankStep {}

/// Replaces each traverser with a fixed constant value (Gremlin `constant()` step).
#[derive(Clone, Debug)]
pub struct ConstantStep {
    pub value: Primitive,
}
impl Optimizer for ConstantStep {}

/// Executes a sub-traversal locally on each traverser and emits every result
/// (Gremlin `local()` step).
#[derive(Clone)]
pub struct LocalStep {
    pub plan: LogicalPlan,
}

impl Optimizer for LocalStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        optimizer_rule(&mut self.plan)
    }
}

/// Implements the `Optimizer` trait for `LogicalStep`, allowing optimization rules to be applied to individual steps.
impl Optimizer for LogicalStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        let mut changed = false;
        match self {
            LogicalStep::Where(wh) => changed |= wh.optimize(optimizer_rule)?,
            LogicalStep::Union(u) => changed |= u.optimize(optimizer_rule)?,
            LogicalStep::Coalesce(c) => changed |= c.optimize(optimizer_rule)?,
            LogicalStep::Repeat(r) => changed |= r.optimize(optimizer_rule)?,
            LogicalStep::Not(n) => changed |= n.optimize(optimizer_rule)?,
            LogicalStep::And(a) => changed |= a.optimize(optimizer_rule)?,
            LogicalStep::Or(o) => changed |= o.optimize(optimizer_rule)?,
            LogicalStep::Choose(c) => changed |= c.optimize(optimizer_rule)?,
            LogicalStep::Local(l) => changed |= l.optimize(optimizer_rule)?,
            _ => {}
        }
        Ok(changed)
    }
}

/// Generalized filter on the other vertex in a `where(otherV()…)` clause.
///
/// Holds id, label, and property predicates extracted from a `where()` sub-plan.
/// `ids: None` = unconstrained; `ids: Some(empty)` = matches nothing (empty intersection).
#[derive(Clone)]
pub struct EndVertexFilter {
    pub ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The other vertex's label predicates, ANDed — same accumulation shape as
    /// `property_preds` (label has no structural lookup-key role to constrain it to a single
    /// value, unlike `ids`/edge `rank`, so there's no reason it can't just be a list).
    pub label_preds: Vec<PrimitivePredicate>,
    /// The other vertex's property predicates, ANDed.
    pub property_preds: Vec<(SmolStr, PrimitivePredicate)>,
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
    pub labels: SmallVec<[SmolStr; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for BothStep {}

/// Represents a logical `bothE` step, traversing both incoming and outgoing edges and returning the edges themselves.
#[derive(Clone)]
pub struct BothEStep {
    pub labels: SmallVec<[SmolStr; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The edge rank to filter by, folded in from a trailing `.has("rank", N)` (see
    /// `merge_end_vertex_filter`). `None` means no rank constraint is known at plan time.
    pub rank: Option<Rank>,
}

impl Optimizer for BothEStep {}

/// Represents a logical `hasLabel` step, filtering elements by their label IDs.
#[derive(Clone)]
pub struct HasLabelStep {
    pub pred: PrimitivePredicate,
}

/// Implements the `Optimizer` trait for `HasLabelStep`.
impl Optimizer for HasLabelStep {}

#[derive(Clone)]
pub struct HasPropertyStep {
    pub key: PropKey,
    pub pred: PrimitivePredicate,
}

impl Optimizer for HasPropertyStep {}

/// Represents a logical `in` step, traversing incoming edges and returning the source vertices.
#[derive(Clone)]
pub struct InStep {
    pub labels: SmallVec<[SmolStr; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

/// Implements the `Optimizer` trait for `InStep`.
impl Optimizer for InStep {}

#[derive(Clone)]
pub struct InEStep {
    pub labels: SmallVec<[SmolStr; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
    /// The edge rank to filter by, folded in from a trailing `.has("rank", N)` (see
    /// `merge_end_vertex_filter`). `None` means no rank constraint is known at plan time.
    pub rank: Option<Rank>,
}
impl Optimizer for InEStep {}

/// Represents a logical `out` step, traversing outgoing edges and returning the destination vertices.
#[derive(Clone)]
pub struct OutStep {
    pub labels: SmallVec<[SmolStr; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

/// Implements the `Optimizer` trait for `OutStep`.
impl Optimizer for OutStep {}

#[derive(Clone)]
pub struct OutEStep {
    pub labels: SmallVec<[SmolStr; 4]>,
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
    pub pred: PrimitivePredicate,
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
    pub plans: SmallVec<[LogicalPlan; PIPELINE_BATCH_INLINE]>,
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
    pub label: SmolStr,
    pub vertex_id: Option<VertexKey>,
    pub properties: HashMap<PropKey, Primitive>,
}

impl Optimizer for AddVStep {}

/// Represents a logical `addE` step, adding a new edge to the graph.
#[derive(Clone)]
pub struct AddEStep {
    pub label: SmolStr,
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
    pub keys: SmallVec<[String; 4]>,
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
    pub pred: PrimitivePredicate,
}

impl Optimizer for HasIdStep {}
