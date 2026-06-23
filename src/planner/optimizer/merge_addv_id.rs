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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::logical_step::{AddVStep, PropertyStep};
    use smol_str::SmolStr;
    use std::collections::HashMap;

    fn addv() -> LogicalStep {
        LogicalStep::AddV(AddVStep { label: "1".into(), vertex_id: None, properties: HashMap::new() })
    }

    fn prop(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::Property(PropertyStep { prop_key: SmolStr::new(key), prop_value: value })
    }

    #[test]
    fn test_id_int32_merged_into_addv() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("id", Primitive::Int32(7))] };
        let changed = merge_addv_id(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(7));
        } else {
            panic!("expected AddV");
        }
    }

    #[test]
    fn test_id_int64_merged_into_addv() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("id", Primitive::Int64(1_000_000_000))] };
        let changed = merge_addv_id(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(1_000_000_000));
        } else {
            panic!("expected AddV");
        }
    }

    #[test]
    fn test_non_id_property_not_merged() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("name", Primitive::Int32(5))] };
        let changed = merge_addv_id(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::AddV(_)));
        assert!(matches!(plan.steps[1], LogicalStep::Property(_)));
    }

    #[test]
    fn test_non_id_property_before_id_property_not_merged() {
        // addV().property("name", "x").property("id", 3)
        // Once i advances past AddV to the name-prop, the (AddV, Property("id")) pattern
        // never fires for the id-prop — the plan is returned unchanged.
        let mut plan = LogicalPlan {
            steps: vec![addv(), prop("name", Primitive::String(SmolStr::new("x"))), prop("id", Primitive::Int32(3))],
        };
        let changed = merge_addv_id(&mut plan).unwrap();
        assert!(!changed, "id property not immediately after addV should not be merged");
        assert_eq!(plan.steps.len(), 3);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, None, "vertex_id should remain unset");
        } else {
            panic!("expected AddV");
        }
    }

    #[test]
    fn test_id_prop_bad_type_errors() {
        let mut plan = LogicalPlan { steps: vec![addv(), prop("id", Primitive::String(SmolStr::new("bad")))] };
        let res = merge_addv_id(&mut plan);
        assert!(res.is_err(), "non-integer id type should return error");
    }

    #[test]
    fn test_duplicate_id_property_errors() {
        let mut plan =
            LogicalPlan { steps: vec![addv(), prop("id", Primitive::Int32(1)), prop("id", Primitive::Int32(2))] };
        let res = merge_addv_id(&mut plan);
        assert!(res.is_err(), "duplicate id assignment should return error");
    }

    #[test]
    fn test_trailing_non_id_properties_preserved() {
        let mut plan = LogicalPlan {
            steps: vec![
                addv(),
                prop("id", Primitive::Int32(5)),
                prop("name", Primitive::String(SmolStr::new("alice"))),
                prop("age", Primitive::Int32(30)),
            ],
        };
        let changed = merge_addv_id(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        if let LogicalStep::AddV(av) = &plan.steps[0] {
            assert_eq!(av.vertex_id, Some(5));
        } else {
            panic!("expected AddV");
        }
        assert!(matches!(plan.steps[1], LogicalStep::Property(_)));
        assert!(matches!(plan.steps[2], LogicalStep::Property(_)));
    }
}
