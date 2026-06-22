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
    planner::{
        logical_step::{FromStep, LogicalPlan, LogicalStep, PropertyStep, ToStep},
        optimizer::primitive_to_rank,
    },
    types::{error::StoreError, prop_key::RANK},
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

pub fn merge_adde_rank(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    // An optimizer rule that merges `property("rank", N)` step into an preceding `addE()` step.
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;
    while j < plan.steps.len() {
        let rank = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::AddE(_ae), LogicalStep::Property(PropertyStep { prop_key: key, prop_value: value }))
                if *key == RANK =>
            {
                Some(primitive_to_rank(value)?)
            }
            _ => None,
        };

        if let Some(r) = rank {
            let LogicalStep::AddE(ae) = &mut plan.steps[i] else {
                unreachable!("should never reach here since we have checked the pattern already");
            };
            if ae.rank.is_some() {
                return Err(StoreError::UnsupportedOperation("cannot assign edge rank several times".into()));
            }
            ae.rank = Some(r);
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

/// An optimizer rule that moves a `property("rank", N)` step forward, ahead of any other
/// `property(...)` step it's adjacent to.
///
/// `merge_adde_rank` only ever looks at the step immediately following `addE()`, so a plan
/// like `addE().property("name", "x").property("rank", 5)` would never get its rank merged —
/// the unrelated `property("name", ...)` sits between `addE()` and `property("rank", ...)`.
/// Repeatedly swapping an out-of-place rank property past its non-rank neighbor (driven by
/// `apply_rules`'s fixpoint loop) walks it all the way back to sit directly after `addE()`,
/// at which point `merge_adde_rank` can fold it in on a later pass.
pub fn reorder_rank_forward(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut i = 0;
    while i + 1 < plan.steps.len() {
        let should_swap = matches!(
            (&plan.steps[i], &plan.steps[i + 1]),
            (LogicalStep::Property(p0), LogicalStep::Property(p1)) if p0.prop_key != RANK && p1.prop_key == RANK
        );
        if should_swap {
            plan.steps.swap(i, i + 1);
            changed = true;
        }
        i += 1;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        planner::logical_step::{AddEStep, FromStep, PropertyStep, ToStep},
        types::Primitive,
    };
    use std::collections::HashMap;

    fn adde() -> LogicalStep {
        LogicalStep::AddE(AddEStep {
            label_id: 1,
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
            rank: None,
        })
    }

    fn from(id: i64) -> LogicalStep {
        LogicalStep::From(FromStep { vertex_id: id })
    }

    fn to(id: i64) -> LogicalStep {
        LogicalStep::To(ToStep { vertex_id: id })
    }

    #[test]
    fn test_from_and_to_merged() {
        let mut plan = LogicalPlan { steps: vec![adde(), from(10), to(20)] };
        let changed = merge_adde_from(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.out_v_id, Some(10));
            assert_eq!(ae.in_v_id, Some(20));
        } else {
            panic!("expected AddE");
        }
    }

    #[test]
    fn test_from_only_merged() {
        let mut plan = LogicalPlan { steps: vec![adde(), from(5)] };
        let changed = merge_adde_from(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.out_v_id, Some(5));
            assert_eq!(ae.in_v_id, None);
        } else {
            panic!("expected AddE");
        }
    }

    #[test]
    fn test_to_only_merged() {
        let mut plan = LogicalPlan { steps: vec![adde(), to(8)] };
        let changed = merge_adde_from(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.out_v_id, None);
            assert_eq!(ae.in_v_id, Some(8));
        } else {
            panic!("expected AddE");
        }
    }

    #[test]
    fn test_to_before_from_merged() {
        // addE().to(20).from(10) — reversed order is legal
        let mut plan = LogicalPlan { steps: vec![adde(), to(20), from(10)] };
        let changed = merge_adde_from(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.out_v_id, Some(10));
            assert_eq!(ae.in_v_id, Some(20));
        } else {
            panic!("expected AddE");
        }
    }

    #[test]
    fn test_duplicate_from_errors() {
        let mut plan = LogicalPlan { steps: vec![adde(), from(1), from(2)] };
        let res = merge_adde_from(&mut plan);
        assert!(res.is_err(), "duplicate from() should return error");
    }

    #[test]
    fn test_duplicate_to_errors() {
        let mut plan = LogicalPlan { steps: vec![adde(), to(1), to(2)] };
        let res = merge_adde_from(&mut plan);
        assert!(res.is_err(), "duplicate to() should return error");
    }

    #[test]
    fn test_no_from_or_to_unchanged() {
        use crate::planner::logical_step::PropertyStep;
        use smol_str::SmolStr;
        let prop = LogicalStep::Property(PropertyStep {
            prop_key: SmolStr::new("weight"),
            prop_value: crate::types::Primitive::Int32(1),
        });
        let mut plan = LogicalPlan { steps: vec![adde(), prop] };
        let changed = merge_adde_from(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_rank_merged_into_adde() {
        use smol_str::SmolStr;
        let mut plan = LogicalPlan {
            steps: vec![
                adde(),
                LogicalStep::Property(PropertyStep { prop_key: SmolStr::new("rank"), prop_value: Primitive::Int32(5) }),
            ],
        };
        let changed = merge_adde_rank(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.rank, Some(5));
        } else {
            panic!("expected AddE");
        }
    }

    #[test]
    fn test_non_rank_property_not_merged() {
        use smol_str::SmolStr;
        let mut plan = LogicalPlan {
            steps: vec![
                adde(),
                LogicalStep::Property(PropertyStep {
                    prop_key: SmolStr::new("weight"),
                    prop_value: Primitive::Int32(5),
                }),
            ],
        };
        let changed = merge_adde_rank(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    fn prop_step(key: &str, value: Primitive) -> LogicalStep {
        use smol_str::SmolStr;
        LogicalStep::Property(PropertyStep { prop_key: SmolStr::new(key), prop_value: value })
    }

    #[test]
    fn test_rank_reordered_ahead_of_other_property() {
        // addE().property("name", "x").property("rank", 5) -> addE().property("rank", 5).property("name", "x")
        let mut plan = LogicalPlan {
            steps: vec![
                adde(),
                prop_step("name", Primitive::String("x".into())),
                prop_step("rank", Primitive::Int32(5)),
            ],
        };
        let changed = reorder_rank_forward(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::Property(ref p) if p.prop_key == "rank"));
        assert!(matches!(plan.steps[2], LogicalStep::Property(ref p) if p.prop_key == "name"));
    }

    #[test]
    fn test_rank_already_first_unchanged() {
        let mut plan = LogicalPlan {
            steps: vec![
                adde(),
                prop_step("rank", Primitive::Int32(5)),
                prop_step("name", Primitive::String("x".into())),
            ],
        };
        let changed = reorder_rank_forward(&mut plan).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_adde_to_property_rank_property_merged_after_full_apply_rules() {
        // addE().to(v).property("name","x").property("rank",5) should fully fold into
        // AddE(in_v_id=v, rank=5).property("name","x") once reorder + merge both run to a fixpoint.
        use crate::planner::{apply_rules, logical_step::ToStep};
        let mut plan = LogicalPlan {
            steps: vec![
                adde(),
                LogicalStep::To(ToStep { vertex_id: 13 }),
                prop_step("name", Primitive::String("x".into())),
                prop_step("rank", Primitive::Int32(5)),
            ],
        };
        apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::AddE(ae) = &plan.steps[0] {
            assert_eq!(ae.in_v_id, Some(13));
            assert_eq!(ae.rank, Some(5));
        } else {
            panic!("expected AddE at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::Property(ref p) if p.prop_key == "name"));
    }
}
