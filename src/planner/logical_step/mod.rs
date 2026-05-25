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

use crate::types::{gvalue::Primitive, keys::VertexKey, prop_key::PropKey, LabelId};
use std::collections::HashMap;
#[derive(Clone)]
pub struct LogicalPlan {
    pub steps: Vec<LogicalStep>,
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
    Where(WhereStep),
    Union(UnionStep),
    AddV(AddVStep),
    AddE(AddEStep),
    Property(PropertyStep),
    V(VStep),
    Limit(LimitStep),
    HasId(HasIdStep),
}

#[derive(Clone)]
pub struct CountStep {}

#[derive(Clone)]
pub struct BothStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct BothEStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct HasLabelStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct HasPropertyStep {
    pub key: PropKey,
    pub value: Primitive,
}

#[derive(Clone)]
pub struct InStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct InEStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct OutStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct OutEStep {
    pub label_ids: Vec<LabelId>,
}

#[derive(Clone)]
pub struct InVStep {}

#[derive(Clone)]
pub struct OtherVStep {}

#[derive(Clone)]
pub struct OutVStep {}

#[derive(Clone)]
pub struct ScalarFilterStep {
    pub value: Primitive,
}

#[derive(Clone)]
pub struct ValuesStep {
    pub property_keys: Vec<PropKey>,
}

#[derive(Clone)]
pub struct WhereStep {
    pub plan: LogicalPlan,
}

#[derive(Clone)]
pub struct UnionStep {
    pub plans: Vec<LogicalPlan>,
}

#[derive(Clone)]
pub struct AddVStep {
    pub label_id: LabelId,
    pub vertex_id: VertexKey,
    pub properties: HashMap<PropKey, Primitive>,
}

#[derive(Clone)]
pub struct AddEStep {
    pub label_id: LabelId,
    pub out_v_id: VertexKey,
    pub in_v_id: VertexKey,
    pub properties: HashMap<PropKey, Primitive>,
}

#[derive(Clone)]
pub struct PropertyStep {
    pub prop_key: PropKey,
    pub prop_value: Primitive,
}

#[derive(Clone)]
pub struct VStep {
    pub ids: Vec<VertexKey>,
}

#[derive(Clone)]
pub struct LimitStep {
    pub limit: u32,
}

#[derive(Clone)]
pub struct HasIdStep {
    pub ids: Vec<VertexKey>,
}
