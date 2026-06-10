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
    planner::logical_step::{FromStep, LogicalPlan, LogicalStep, ToStep},
    types::error::StoreError,
};

pub fn merge_adde_from(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;
    while j < plan.steps.len() {
        let (vid, is_from) = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::AddE(_ae), LogicalStep::From(FromStep { vertex_id })) => (Some(vertex_id), true),
            (LogicalStep::AddE(_ae), LogicalStep::To(ToStep { vertex_id })) => (Some(vertex_id), false),
            _ => (None, false),
        };

        if let Some(&id) = vid {
            let LogicalStep::AddE(ae) = &mut plan.steps[i] else {
                unreachable!("should never reach here since we have checked the pattern already")
            };
            if is_from {
                if ae.out_v_id.is_some() {
                    return Err(StoreError::UnsupportedOperation("cannot assign vertex id several time".into()));
                }
                ae.out_v_id = Some(id);
            } else {
                if ae.in_v_id.is_some() {
                    return Err(StoreError::UnsupportedOperation("cannot assign vertex id several time".into()));
                }
                ae.in_v_id = Some(id);
            }

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
