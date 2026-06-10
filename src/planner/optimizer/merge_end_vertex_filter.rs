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
    planner::logical_step::{HasPropertyStep, LogicalPlan, LogicalStep},
    types::{prop_key::ID, Primitive, StoreError},
};
use smallvec::smallvec;

pub fn merge_end_vertex_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;
    while j < plan.steps.len() {
        let ids = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::OutE(_), LogicalStep::EndVertexFilter(ef))
            | (LogicalStep::InE(_), LogicalStep::EndVertexFilter(ef))
            | (LogicalStep::BothE(_), LogicalStep::EndVertexFilter(ef)) => Some(ef.ids.clone()),
            (LogicalStep::Out(_), LogicalStep::HasId(ef))
            | (LogicalStep::In(_), LogicalStep::HasId(ef))
            | (LogicalStep::Both(_), LogicalStep::HasId(ef)) => Some(ef.ids.clone()),
            (LogicalStep::Out(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
            | (LogicalStep::In(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
            | (LogicalStep::Both(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
                if ID == *key =>
            {
                match value {
                    Primitive::Int32(id) => Some(smallvec![*id as i64]),
                    Primitive::Int64(id) => Some(smallvec![*id]),
                    _ => return Err(StoreError::UnexpectedDataType("only i32 and i64 can be vertex id".into())),
                }
            }
            _ => None,
        };
        if let Some(idv) = ids {
            match &mut plan.steps[i] {
                LogicalStep::OutE(oute) => {
                    oute.end_vertex_ids = Some(idv);
                }
                LogicalStep::InE(ine) => {
                    ine.end_vertex_ids = Some(idv);
                }
                LogicalStep::BothE(bothe) => {
                    bothe.end_vertex_ids = Some(idv);
                }
                LogicalStep::Out(out) => {
                    out.end_vertex_ids = Some(idv);
                }
                LogicalStep::In(in_) => {
                    in_.end_vertex_ids = Some(idv);
                }
                LogicalStep::Both(both) => {
                    both.end_vertex_ids = Some(idv);
                }

                _ => unreachable!("should never reach here since we have checked the pattern already"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        planner::logical_step::{EndVertexFilter, OutEStep, OutVStep, VStep},
        types::keys::VertexKey,
    };
    use smallvec::smallvec;

    fn out_e() -> LogicalStep {
        LogicalStep::OutE(OutEStep { label_ids: smallvec![], end_vertex_ids: None })
    }

    fn evf(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::EndVertexFilter(EndVertexFilter { ids: ids.into_iter().collect() })
    }

    fn v(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids: ids.into_iter().collect() })
    }

    fn out_v() -> LogicalStep {
        LogicalStep::OutV(OutVStep {})
    }

    // OutE().EndVertexFilter([1,2]) → OutE(end_vertex_ids=[1,2]), EVF removed
    #[test]
    fn test_out_e_evf_merged() {
        let mut plan = LogicalPlan { steps: vec![out_e(), evf(vec![1, 2])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.end_vertex_ids.as_deref(), Some(&[1, 2][..]));
        } else {
            panic!("expected OutE at step 0");
        }
    }

    // V(s).OutE().EVF(d).OutV() → V(s).OutE(ev=d).OutV(), step count drops from 4 to 3
    #[test]
    fn test_out_e_evf_merged_in_context() {
        let mut plan = LogicalPlan { steps: vec![v(vec![10]), out_e(), evf(vec![20]), out_v()] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        if let LogicalStep::OutE(oute) = &plan.steps[1] {
            assert_eq!(oute.end_vertex_ids.as_deref(), Some(&[20][..]));
        } else {
            panic!("expected OutE at step 1");
        }
        assert!(matches!(plan.steps[2], LogicalStep::OutV(_)));
    }

    // OutE().EVF([1]).OutV().OutE().EVF([2]) → OutE(ev=[1]).OutV().OutE(ev=[2]), both pairs merged
    #[test]
    fn test_two_out_e_evf_pairs_both_merged() {
        let mut plan = LogicalPlan { steps: vec![out_e(), evf(vec![1]), out_v(), out_e(), evf(vec![2])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.end_vertex_ids.as_deref(), Some(&[1][..]));
        } else {
            panic!("expected OutE at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::OutV(_)));
        if let LogicalStep::OutE(oute) = &plan.steps[2] {
            assert_eq!(oute.end_vertex_ids.as_deref(), Some(&[2][..]));
        } else {
            panic!("expected OutE at step 2");
        }
    }

    // OutE().OutV() — no EVF present, plan unchanged, end_vertex_ids stays None
    #[test]
    fn test_out_e_without_evf_unchanged() {
        let mut plan = LogicalPlan { steps: vec![out_e(), out_v()] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.end_vertex_ids, None);
        } else {
            panic!("expected OutE at step 0");
        }
    }

    // EVF not preceded by OutE — EVF is preserved as-is, plan unchanged
    #[test]
    fn test_evf_without_out_e_preserved() {
        let mut plan = LogicalPlan { steps: vec![v(vec![1]), evf(vec![2])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::EndVertexFilter(_)));
    }
}
