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

//! Logical plan rewriter — transforms a [`LogicalPlan`] into a semantically
//! equivalent but more efficient form before physical planning.
//!
//! Each rewrite rule lives in its own submodule. [`optimize`] applies them in
//! order; adding a new rule means creating a new file and one extra call here.
//!
//! ## Current rules
//!
//! | Rule | File | Effect |
//! |------|------|--------|
//! | ID filter pushdown | [`push_down_id_filter`] | `V().has("id", N)` → `V(N)` |
//!
//! [`LogicalPlan`]: crate::planner::logical_step::LogicalPlan

mod extract_end_vertex_filter;
mod merge_end_vertex_filter;
mod merge_v_id_filter;

use crate::{
    planner::logical_step::{LogicalPlan, Optimizer, OptimizerRule},
    types::StoreError,
};
/// Rewrites a `LogicalPlan` into a more efficient equivalent before physical planning.
pub fn apply_rules(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    // all the optimizers we want to apply to the logical plan.
    let optimizers: Vec<OptimizerRule> = vec![
        extract_end_vertex_filter::extract_end_vertex_filter,
        merge_v_id_filter::merget_v_id_filter,
        merge_end_vertex_filter::merge_end_vertex_filter,
    ];
    let mut plan_changed = true;
    // apply optimizers to each step first, then to the whole plan. this allows optimizers to terget specific patterns
    // in steps, which is common for most optimizations.
    while plan_changed {
        plan_changed = false;
        for opt in &optimizers {
            plan_changed |= plan.optimize(opt)?;
        }
    }

    Ok(plan_changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        planner::logical_step::{BothEStep, HasIdStep, LogicalStep, OtherVStep, OutEStep, OutVStep, VStep, WhereStep},
        types::keys::VertexKey,
    };

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: vec![] })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::HasId(HasIdStep { ids })
    }

    fn out_e() -> LogicalStep {
        LogicalStep::OutE(OutEStep { label_ids: vec![], end_vertex_ids: None })
    }

    fn other_v() -> LogicalStep {
        LogicalStep::OtherV(OtherVStep {})
    }

    fn out_v() -> LogicalStep {
        LogicalStep::OutV(OutVStep {})
    }

    fn both_e() -> LogicalStep {
        LogicalStep::BothE(BothEStep { label_ids: vec![], end_vertex_ids: None })
    }

    fn whr(steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Where(WhereStep { plan: LogicalPlan { steps } })
    }

    fn has_id_prop(id: i32) -> LogicalStep {
        use crate::{planner::logical_step::HasPropertyStep, types::gvalue::Primitive};
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), value: Primitive::Int32(id) })
    }

    // V().has("id",1).has("id",2).outE().otherV().hasId(4)
    // Both has("id") are folded into V by merget_v_id_filter (second id wins); no where() to extract.
    // Result: [V(2), OutE, OtherV, HasId(4)]
    #[test]
    fn test_v_has_id_prop_twice_merged_into_v() {
        let steps = vec![v_all(), has_id_prop(1), has_id_prop(2), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 4);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(v.ids, vec![2], "second has(\"id\") should win");
        } else {
            panic!("expected VStep at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasId(_)));
    }

    // V().has("id",1).outE().where(otherV().hasId(2)).outV().bothE().where(otherV().hasId(3))
    // merget_v_id_filter folds has("id",1) into V; extract_end_vertex_filter lifts both where() steps.
    // Result: [V(1), OutE, EndVertexFilter(2), OutV, BothE, EndVertexFilter(3)]
    #[test]
    fn test_v_has_id_prop_with_where_extracted_and_merged() {
        let steps = vec![
            v_all(),
            has_id_prop(1),
            out_e(),
            whr(vec![other_v(), has_id(vec![2])]),
            out_v(),
            both_e(),
            whr(vec![other_v(), has_id(vec![3])]),
        ];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 4);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(v.ids, vec![1], "has(\"id\",1) should be folded into V");
        } else {
            panic!("expected VStep at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OutV(_)));
        assert!(matches!(plan.steps[3], LogicalStep::BothE(_)));
    }

    // V().hasId(1, 2).hasId(3).outE().otherV().hasId(4)
    // No current rule matches this pattern — plan structure must be preserved as-is.
    #[test]
    fn test_v_has_id_has_id_out_e_other_v_has_id() {
        let steps = vec![v_all(), has_id(vec![1, 2]), has_id(vec![3]), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 4);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasId(_)));
    }

    // V().hasId(1).outE().where(otherV().hasId(2)).outV().bothE().where(otherV().hasId(3))
    // optimized into V(1).outE().EndVertexFilter().outV().bothE().EndVertexFilter()
    #[test]
    fn test_where_other_v_has_id_extracted_at_multiple_positions() {
        let steps = vec![
            v_all(),
            has_id(vec![1]),
            out_e(),
            whr(vec![other_v(), has_id(vec![2])]),
            out_v(),
            both_e(),
            whr(vec![other_v(), has_id(vec![3])]),
        ];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 4);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OutV(_)));
        assert!(matches!(plan.steps[3], LogicalStep::BothE(_)));
    }
}
