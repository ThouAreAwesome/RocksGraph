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
    planner::logical_step::{FromStep, LogicalPlan, LogicalStep, ToStep},
    types::StoreError,
};

pub fn merge_adde_from(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    // An optimizer rule that merges `from()` and `to()` steps into an preceding `addE()` step.
    //
    // This simplifies the plan by consolidating edge creation information directly into the `addE` step,
    // making it more efficient for physical planning.
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
