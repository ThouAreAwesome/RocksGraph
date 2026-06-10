// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
//
// This file is part of MultiGraph.
//
// Use of this software is governed by the Business Source License 1.1
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date (2030-01-01), in accordance with the Business Source
// License, use of this software will be governed by the Apache License 2.0.
//
// SPDX-License-Identifier: BUSL-1.1

//! Engine-agnostic logical IR — the intermediate representation shared by the
//! optimizer and all execution engines.
//!
//! A [`LogicalPlan`] is an ordered list of [`LogicalStep`]s. It carries only
//! *what* to compute, with no reference to any physical operator or execution
//! strategy. The volcano builder ([`engine::volcano::builder`]) is responsible
//! for compiling a `LogicalPlan` into a chain of physical steps.
//!
//! [`engine::volcano::builder`]: crate::engine::volcano::builder

use crate::types::{gvalue::Primitive, keys::VertexKey, prop_key::PropKey, LabelId, StoreError};
use smallvec::SmallVec;
use std::collections::HashMap;

// Reuse the same rewrite/optimize rule for both LogicalPlan and LogicalStep.
pub type OptimizerRule = fn(&mut LogicalPlan) -> Result<bool, StoreError>;

pub trait Optimizer {
    fn optimize(&mut self, _: &OptimizerRule) -> Result<bool, StoreError> {
        Ok(false)
    }
}

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
    Limit(LimitStep),
    HasId(HasIdStep),
    Coalesce(CoalesceStep),
    EndVertexFilter(EndVertexFilter),
    Drop(DropStep),
}

#[derive(Clone)]
pub struct DropStep {}

impl Optimizer for DropStep {}

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

#[derive(Clone)]
pub struct EndVertexFilter {
    pub ids: SmallVec<[VertexKey; 4]>,
}

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

#[derive(Clone)]
pub struct CountStep {}

impl Optimizer for CountStep {}
#[derive(Clone)]
pub struct BothStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for BothStep {}

#[derive(Clone)]
pub struct BothEStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for BothEStep {}

#[derive(Clone)]
pub struct HasLabelStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
}

impl Optimizer for HasLabelStep {}

#[derive(Clone)]
pub struct HasPropertyStep {
    pub key: PropKey,
    pub value: Primitive,
}

impl Optimizer for HasPropertyStep {}

#[derive(Clone)]
pub struct InStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for InStep {}

#[derive(Clone)]
pub struct InEStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}
impl Optimizer for InEStep {}

#[derive(Clone)]
pub struct OutStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for OutStep {}

#[derive(Clone)]
pub struct OutEStep {
    pub label_ids: SmallVec<[LabelId; 4]>,
    pub end_vertex_ids: Option<SmallVec<[VertexKey; 4]>>,
}

impl Optimizer for OutEStep {}

#[derive(Clone)]
pub struct InVStep {}

impl Optimizer for InVStep {}

#[derive(Clone)]
pub struct OtherVStep {}

impl Optimizer for OtherVStep {}

#[derive(Clone)]
pub struct OutVStep {}

impl Optimizer for OutVStep {}

#[derive(Clone)]
pub struct ScalarFilterStep {
    pub value: Primitive,
}

impl Optimizer for ScalarFilterStep {}

#[derive(Clone)]
pub struct ValuesStep {
    pub property_keys: SmallVec<[PropKey; 4]>,
}

impl Optimizer for ValuesStep {}

#[derive(Clone)]
pub struct PropertiesStep {
    pub property_keys: SmallVec<[PropKey; 4]>,
}
impl Optimizer for PropertiesStep {}

#[derive(Clone)]
pub struct WhereStep {
    pub plan: LogicalPlan,
}

impl Optimizer for WhereStep {
    fn optimize(&mut self, optimizer_rule: &OptimizerRule) -> Result<bool, StoreError> {
        optimizer_rule(&mut self.plan)
    }
}

#[derive(Clone)]
pub struct UnionStep {
    pub plans: SmallVec<[LogicalPlan; 0]>,
}

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
pub struct AddVStep {
    pub label_id: LabelId,
    pub vertex_id: Option<VertexKey>,
    pub properties: HashMap<PropKey, Primitive>,
}

impl Optimizer for AddVStep {}

#[derive(Clone)]
pub struct AddEStep {
    pub label_id: LabelId,
    pub out_v_id: Option<VertexKey>,
    pub in_v_id: Option<VertexKey>,
    pub properties: HashMap<PropKey, Primitive>,
}

impl Optimizer for AddEStep {}

#[derive(Clone)]
pub struct FromStep {
    pub vertex_id: VertexKey,
}

impl Optimizer for FromStep {}

#[derive(Clone)]
pub struct ToStep {
    pub vertex_id: VertexKey,
}

impl Optimizer for ToStep {}

#[derive(Clone)]
pub struct PropertyStep {
    pub prop_key: PropKey,
    pub prop_value: Primitive,
}

impl Optimizer for PropertyStep {}

#[derive(Clone)]
pub struct VStep {
    pub ids: SmallVec<[VertexKey; 4]>,
}

impl Optimizer for VStep {}

#[derive(Clone)]
pub struct LimitStep {
    pub limit: u32,
}

impl Optimizer for LimitStep {}

#[derive(Clone)]
pub struct HasIdStep {
    pub ids: SmallVec<[VertexKey; 4]>,
}

impl Optimizer for HasIdStep {}
