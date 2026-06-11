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
    planner::logical_step::{LogicalPlan, LogicalStep, PropertyStep},
    types::{error::StoreError, prop_key::ID, Primitive},
};

pub fn merge_addv_id(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    // An optimizer rule that merges a `property("id", N)` step into an preceding `addV()` step.
    //
    // This allows the `addV` step to directly specify the vertex ID, simplifying the plan
    // and potentially enabling more direct physical planning.
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
