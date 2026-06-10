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

pub mod logical_step;

mod optimizer;

use crate::{
    planner::{
        logical_step::{LogicalPlan, Optimizer, OptimizerRule},
        optimizer::{
            extract_end_vertex_filter, merge_adde_ids, merge_addv_id, merge_end_vertex_filter, merge_v_id_filter,
        },
    },
    types::StoreError,
};
/// Rewrites a `LogicalPlan` into a more efficient equivalent before physical planning.
pub fn apply_rules(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    // all the optimizers we want to apply to the logical plan.
    let optimizers: Vec<OptimizerRule> = vec![
        extract_end_vertex_filter::extract_end_vertex_filter,
        merge_v_id_filter::merget_v_id_filter,
        merge_end_vertex_filter::merge_end_vertex_filter,
        merge_addv_id::merge_addv_id,
        merge_adde_ids::merge_adde_from,
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
    use smol_str::SmolStr;

    use crate::{
        planner::logical_step::{BothEStep, HasIdStep, LogicalStep, OtherVStep, OutEStep, OutVStep, VStep, WhereStep},
        types::{keys::VertexKey, Primitive},
    };
    use smallvec::smallvec;
    use std::collections::HashMap;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() })
    }

    fn out_e() -> LogicalStep {
        LogicalStep::OutE(OutEStep { label_ids: smallvec![], end_vertex_ids: None })
    }

    fn out_e_label() -> LogicalStep {
        LogicalStep::OutE(OutEStep { label_ids: smallvec![1], end_vertex_ids: None })
    }

    fn other_v() -> LogicalStep {
        LogicalStep::OtherV(OtherVStep {})
    }

    fn out_v() -> LogicalStep {
        LogicalStep::OutV(OutVStep {})
    }

    fn both_e() -> LogicalStep {
        LogicalStep::BothE(BothEStep { label_ids: smallvec![], end_vertex_ids: None })
    }

    fn whr(steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Where(WhereStep { plan: LogicalPlan { steps: steps.into_iter().collect() } })
    }

    fn has_id_prop(id: i32) -> LogicalStep {
        use crate::{planner::logical_step::HasPropertyStep, types::gvalue::Primitive};
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), value: Primitive::Int32(id) })
    }

    fn prop(key: SmolStr, value: Primitive) -> LogicalStep {
        LogicalStep::Property(crate::planner::logical_step::PropertyStep { prop_key: key, prop_value: value })
    }

    // V().has("id",1).has("id",2).outE().otherV().hasId(4)
    #[test]
    fn test_v_has_id_prop_twice_merge_into_v() {
        let steps = vec![v_all(), has_id_prop(1), has_id_prop(2), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 5);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[1i64], "second has(\"id\") should win");
        } else {
            panic!("expected VStep at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[3], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[4], LogicalStep::HasId(_)));
    }

    #[test]
    fn test_v_has_id_merged_into_v() {
        let steps = vec![v_all(), has_id_prop(1), has_id_prop(2), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 5);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[1i64], "second has(\"id\") should win");
        } else {
            panic!("expected VStep at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[3], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[4], LogicalStep::HasId(_)));
    }

    // V().hasId(1).outE().where(otherV().hasId(2))
    #[test]
    fn test_v_has_id_where_otherv_has_id() {
        let steps = vec![v_all(), has_id_prop(1), out_e_label(), whr(vec![other_v(), has_id(vec![2])])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[1i64], "has(\"id\",1) should be folded into V");
        } else {
            panic!("expected VStep at step 0");
        }
        if let LogicalStep::OutE(oute) = &plan.steps[1] {
            assert_eq!(&oute.label_ids[..], &[1u16], "has(\"id\",1) should be folded into V");
            assert_eq!(
                oute.end_vertex_ids.as_deref(),
                Some(&[2i64][..]),
                "where(otherV().hasId(2)) should be folded into OutE"
            );
        } else {
            panic!("expected VStep at step 0");
        }
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
            assert_eq!(&v.ids[..], &[1i64], "has(\"id\",1) should be folded into V");
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

    // addV().property("id", 1)
    // merge_addv_id folds property("id", 1) into AddV(vertex_id=1).
    #[test]
    fn test_add_v_id_prop_merged() {
        use crate::planner::logical_step::AddVStep;
        let steps = vec![
            LogicalStep::AddV(AddVStep { label_id: 1, vertex_id: None, properties: HashMap::new() }),
            prop(SmolStr::new("id"), Primitive::Int32(321)),
        ];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddV(add_v) = &plan.steps[0] {
            assert_eq!(add_v.vertex_id, Some(321), "property(\"id\") should be merged into addV");
        } else {
            panic!("expected AddVStep at step 0");
        }
    }

    // addV().property("id", 1).property("id", 2) which is unsupported
    // merge_addv_id folds property("id", 1) into AddV(vertex_id=1).
    #[test]
    fn test_add_v_id_prop_duplicate() {
        use crate::planner::logical_step::AddVStep;
        let steps = vec![
            LogicalStep::AddV(AddVStep { label_id: 1, vertex_id: None, properties: HashMap::new() }),
            prop(SmolStr::new("id"), Primitive::Int64(321)),
            prop(SmolStr::new("id"), Primitive::Int32(21)),
        ];
        let mut plan = LogicalPlan { steps };
        let res = apply_rules(&mut plan);
        assert!(res.is_err());
    }

    #[test]
    fn test_adde_from_merged() {
        use crate::planner::logical_step::{AddEStep, FromStep, ToStep};
        let steps = vec![
            LogicalStep::AddE(AddEStep { label_id: 1, out_v_id: None, in_v_id: None, properties: HashMap::new() }),
            LogicalStep::From(FromStep { vertex_id: 12 }),
            LogicalStep::To(ToStep { vertex_id: 13 }),
        ];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::AddE(add_e) = &plan.steps[0] {
            assert_eq!(add_e.out_v_id, Some(12), "property(\"id\") should be merged into addE");
            assert_eq!(add_e.in_v_id, Some(13), "property(\"id\") should be merged into addE");
        } else {
            panic!("expected AddVStep at step 0");
        }
    }
}
