// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{
    planner::logical_step::{LogicalPlan, LogicalStep},
    types::StoreError,
};

/// Optimizes `V().nearest(...)` by deleting the unbounded `VStep` entirely.
///
/// Because `NearestStep` queries a vector index to find the top K matches,
/// evaluating an unbounded `VStep` first would force the execution engine to
/// buffer the entire graph in memory before calling the index. By deleting `VStep`,
/// `NearestStep` becomes the entry point and queries the index directly in O(1) memory.
pub fn merge_v_into_nearest(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut i = 0;

    while i < plan.steps.len().saturating_sub(1) {
        let is_match = matches!(&plan.steps[i], LogicalStep::V(v) if v.ids.is_empty())
            && matches!(&plan.steps[i + 1], LogicalStep::Nearest(_));

        if is_match {
            plan.steps.remove(i);
            if let LogicalStep::Nearest(ref mut s) = plan.steps[i] {
                s.is_root = true;
            }
            changed = true;
        } else {
            i += 1;
        }
    }

    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::logical_step::{CountStep, NearestLogicalStep, VStep};
    use smallvec::smallvec;

    fn v_empty() -> LogicalStep {
        LogicalStep::V(VStep { ids: Default::default() })
    }

    fn v_ids(id: i64) -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![id] })
    }

    fn nearest(is_root: bool) -> LogicalStep {
        LogicalStep::Nearest(NearestLogicalStep {
            prop_key: "emb".to_string(),
            query_vec: vec![1.0, 0.0],
            k: 5,
            ef_search: None,
            metric_override: None,
            is_root,
        })
    }

    #[test]
    fn test_v_empty_then_nearest_is_merged() {
        let mut plan = LogicalPlan { steps: vec![v_empty(), nearest(false)] };
        let changed = merge_v_into_nearest(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1, "VStep must be deleted");
        assert!(matches!(&plan.steps[0], LogicalStep::Nearest(n) if n.is_root), "Nearest must become root");
    }

    #[test]
    fn test_v_with_ids_then_nearest_is_not_merged() {
        // V([1]) is a bounded id lookup, not an unbounded scan — must not be deleted.
        let mut plan = LogicalPlan { steps: vec![v_ids(1), nearest(false)] };
        let changed = merge_v_into_nearest(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(&plan.steps[1], LogicalStep::Nearest(n) if !n.is_root));
    }

    #[test]
    fn test_bare_nearest_with_no_preceding_v_is_untouched() {
        // Nothing precedes Nearest at all — there's no VStep to delete, so is_root
        // must stay false. (The builder-level validation is what rejects this shape;
        // this optimizer pass correctly leaves it alone rather than guessing.)
        let mut plan = LogicalPlan { steps: vec![nearest(false)] };
        let changed = merge_v_into_nearest(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 1);
        assert!(matches!(&plan.steps[0], LogicalStep::Nearest(n) if !n.is_root));
    }

    #[test]
    fn test_v_empty_then_other_step_is_not_merged() {
        let mut plan = LogicalPlan { steps: vec![v_empty(), LogicalStep::Count(CountStep {})] };
        let changed = merge_v_into_nearest(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_empty_plan_does_not_panic() {
        let mut plan = LogicalPlan { steps: vec![] };
        let changed = merge_v_into_nearest(&mut plan).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_integration_via_apply_rules() {
        // `apply_rules` reports whether its *last* fixed-point iteration changed
        // anything, which is always false at convergence — assert on the resulting
        // plan shape instead of the return value (same pattern as the sibling
        // `merge_haslabel_into_edge` optimizer's integration test).
        let mut plan = LogicalPlan { steps: vec![v_empty(), nearest(false)] };
        let changed = crate::planner::apply_rules(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
        assert!(matches!(&plan.steps[0], LogicalStep::Nearest(n) if n.is_root));
    }
}
