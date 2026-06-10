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

use crate::{
    planner::logical_step::{LogicalPlan, LogicalStep, PropertyStep},
    types::{error::StoreError, prop_key::ID, Primitive},
};

pub fn merge_addv_id(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;
    while j < plan.steps.len() {
        let vid = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::AddV(_av), LogicalStep::Property(PropertyStep { prop_key: key, prop_value: value })) => {
                if ID == *key {
                    match value {
                        Primitive::Int32(id) => Some(*id as i64),
                        Primitive::Int64(id) => Some(*id),
                        _ => return Err(StoreError::UnexpectedDataType("only i32 and i64 can be vertex id".into())),
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if vid.is_some() {
            let LogicalStep::AddV(av) = &mut plan.steps[i] else {
                unreachable!("should never reach here since we have checked the pattern already");
            };
            if av.vertex_id.is_some() {
                return Err(StoreError::UnsupportedOperation("cannot assign vertex id several time".into()));
            }
            av.vertex_id = vid;
            plan_changed = true;
            j += 1; // skip the merged step
        } else {
            i += 1;
            if i != j {
                plan.steps.swap(i, j);
            }
            j += 1;
        }
    }

    plan.steps.truncate(i + 1);
    Ok(plan_changed)
}
