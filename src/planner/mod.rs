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

//! Logical-to-physical plan translation, optimizer rules, and filter reordering.
//!
//! The planner translates a Gremlin-idiom AST into a logical plan IR, then
//! applies optimizer rules (id-filter fusion, edge-step fusion, filter reorder)
//! before handing off to the Volcano engine for physical plan construction.
pub mod logical_step;

pub(crate) mod optimizer;

use crate::{
    planner::{
        logical_step::{LogicalPlan, Optimizer, OptimizerRule},
        optimizer::{
            extract_end_vertex_filter, merge_adde_ids, merge_addv_id, merge_end_vertex_filter,
            merge_haslabel_into_edge, merge_v_id_filter, reorder_filter,
        },
    },
    types::StoreError,
};

/// Rewrites a `LogicalPlan` into a more efficient equivalent before physical planning.
pub fn apply_rules(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    const OPTIMIZERS: &[OptimizerRule] = &[
        reorder_filter::reorder_filters,
        merge_v_id_filter::merge_v_id_filter,
        merge_addv_id::merge_addv_id,
        merge_adde_ids::merge_adde_from,
        merge_adde_ids::reorder_rank_forward,
        merge_adde_ids::merge_adde_rank,
        extract_end_vertex_filter::extract_end_vertex_filter,
        merge_end_vertex_filter::merge_end_vertex_filter,
        merge_haslabel_into_edge::merge_haslabel_into_edge,
    ];
    let mut plan_changed = true;
    while plan_changed {
        plan_changed = false;
        for opt in OPTIMIZERS {
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
        planner::optimizer::extract_ids_from_predicate,
        types::{keys::VertexKey, Primitive, PrimitivePredicate},
    };
    use smallvec::smallvec;
    use std::collections::HashMap;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        let pred = PrimitivePredicate::Within(ids.into_iter().map(Primitive::Int64).collect());
        LogicalStep::HasId(HasIdStep { pred })
    }

    fn out_e() -> LogicalStep {
        LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn out_e_label() -> LogicalStep {
        LogicalStep::OutE(OutEStep { labels: smallvec!["1".into()], end_vertex_ids: None, rank: None })
    }

    fn other_v() -> LogicalStep {
        LogicalStep::OtherV(OtherVStep {})
    }

    fn out_v() -> LogicalStep {
        LogicalStep::OutV(OutVStep {})
    }

    fn both_e() -> LogicalStep {
        LogicalStep::BothE(BothEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn whr(steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Where(WhereStep { plan: LogicalPlan { steps: steps.into_iter().collect() } })
    }

    fn has_id_prop(id: i32) -> LogicalStep {
        use crate::planner::logical_step::HasPropertyStep;
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep {
            key: SmolStr::new("id"),
            pred: PrimitivePredicate::Eq(Primitive::Int32(id)),
        })
    }

    fn prop(key: SmolStr, value: Primitive) -> LogicalStep {
        LogicalStep::Property(crate::planner::logical_step::PropertyStep { prop_key: key, prop_value: value })
    }

    // V().has("id",1).has("id",2).outE().otherV().hasId(4)
    // First has("id") is folded into V; second has("id") stays (V already has ids set).
    // The trailing hasId(4) after otherV() is not affected by merge_v_id_filter (not preceded by V).
    #[test]
    fn test_v_has_id_prop_twice_first_wins() {
        let steps = vec![v_all(), has_id_prop(1), has_id_prop(2), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 5);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[1i64], "first has(\"id\") should be folded into V");
        } else {
            panic!("expected VStep at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[3], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[4], LogicalStep::HasId(_)));
    }

    // V().hasId(1).outE().otherV().hasId(4)
    // hasId(1) is folded into V; trailing hasId(4) after otherV() is preserved.
    #[test]
    fn test_v_has_id_merged_into_v() {
        let steps = vec![v_all(), has_id(vec![1]), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 4);
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[1i64], "hasId(1) should be folded into V");
        } else {
            panic!("expected VStep at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasId(_)));
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
            assert_eq!(&oute.labels[..], &[smol_str::SmolStr::new("1")], "has(\"id\",1) should be folded into V");
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
    // merget_v_id_filter folds has("id",1) into V; extract_end_vertex_filter lifts both where() steps;
    // merge_end_vertex_filter pushes them into OutE and BothE.
    // Result: [V(1), OutE(ev=2), OutV, BothE(ev=3)]
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
        if let LogicalStep::OutE(oute) = &plan.steps[1] {
            assert_eq!(
                oute.end_vertex_ids.as_deref(),
                Some(&[2i64][..]),
                "end vertex filter should be merged into OutE"
            );
        } else {
            panic!("expected OutE at step 1");
        }
        assert!(matches!(plan.steps[2], LogicalStep::OutV(_)));
        if let LogicalStep::BothE(bothe) = &plan.steps[3] {
            assert_eq!(
                bothe.end_vertex_ids.as_deref(),
                Some(&[3i64][..]),
                "end vertex filter should be merged into BothE"
            );
        } else {
            panic!("expected BothE at step 3");
        }
    }

    // V().hasId(1, 2).hasId(3).outE().otherV().hasId(4)
    // No current rule matches this pattern — plan structure must be preserved as-is.
    #[test]
    fn test_v_has_id_has_id_out_e_other_v_has_id() {
        let steps = vec![v_all(), has_id(vec![1, 2]), has_id(vec![3]), out_e(), other_v(), has_id(vec![4])];
        let mut plan = LogicalPlan { steps };
        let _ = apply_rules(&mut plan).unwrap();
        assert_eq!(plan.steps.len(), 5);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        if let LogicalStep::HasId(has_id) = &plan.steps[1] {
            let ids = extract_ids_from_predicate(&has_id.pred).unwrap().unwrap();
            assert_eq!(&ids[..], &[3i64], "hasId(3) should be preserved");
        } else {
            panic!("expected HasId at step 1");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::OutE(_)));
        assert!(matches!(plan.steps[3], LogicalStep::OtherV(_)));
        assert!(matches!(plan.steps[4], LogicalStep::HasId(_)));
    }

    // V().hasId(1).outE().where(otherV().hasId(2)).outV().bothE().where(otherV().hasId(3))
    // optimized into V(1).outE(ev=2).outV().bothE(ev=3)
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
        if let LogicalStep::V(v) = &plan.steps[0] {
            assert_eq!(&v.ids[..], &[1i64], "hasId(1) should be folded into V");
        } else {
            panic!("expected VStep at step 0");
        }
        if let LogicalStep::OutE(oute) = &plan.steps[1] {
            assert_eq!(
                oute.end_vertex_ids.as_deref(),
                Some(&[2i64][..]),
                "end vertex filter should be merged into OutE"
            );
        } else {
            panic!("expected OutE at step 1");
        }
        assert!(matches!(plan.steps[2], LogicalStep::OutV(_)));
        if let LogicalStep::BothE(bothe) = &plan.steps[3] {
            assert_eq!(
                bothe.end_vertex_ids.as_deref(),
                Some(&[3i64][..]),
                "end vertex filter should be merged into BothE"
            );
        } else {
            panic!("expected BothE at step 3");
        }
    }

    // addV().property("id", 1)
    // merge_addv_id folds property("id", 1) into AddV(vertex_id=1).
    #[test]
    fn test_add_v_id_prop_merged() {
        use crate::planner::logical_step::AddVStep;
        let steps = vec![
            LogicalStep::AddV(AddVStep { label: "1".into(), vertex_id: None, properties: HashMap::new() }),
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
            LogicalStep::AddV(AddVStep { label: "1".into(), vertex_id: None, properties: HashMap::new() }),
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
            LogicalStep::AddE(AddEStep {
                label: "1".into(),
                out_v_id: None,
                in_v_id: None,
                properties: HashMap::new(),
                rank: None,
            }),
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

    #[test]
    fn test_logical_step_optimizer_coverage() {
        use crate::planner::logical_step::*;
        use smallvec::smallvec;

        let dummy_rule: OptimizerRule = |_plan| Ok(false);

        let mut steps = vec![
            LogicalStep::Both(BothStep { labels: smallvec![], end_vertex_ids: None }),
            LogicalStep::BothE(BothEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
            LogicalStep::Count(CountStep {}),
            LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Within(vec![]) }),
            LogicalStep::HasProperty(HasPropertyStep {
                key: SmolStr::new("key"),
                pred: PrimitivePredicate::Eq(Primitive::Int32(0)),
            }),
            LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None }),
            LogicalStep::InE(InEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
            LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }),
            LogicalStep::InV(InVStep {}),
            LogicalStep::OtherV(OtherVStep {}),
            LogicalStep::OutV(OutVStep {}),
            LogicalStep::ScalarFilter(ScalarFilterStep { pred: PrimitivePredicate::Eq(Primitive::Int32(0)) }),
            LogicalStep::Values(ValuesStep { property_keys: smallvec![] }),
            LogicalStep::Properties(PropertiesStep { property_keys: smallvec![] }),
            LogicalStep::From(FromStep { vertex_id: 0 }),
            LogicalStep::To(ToStep { vertex_id: 0 }),
            LogicalStep::Limit(LimitStep { limit: 0 }),
            LogicalStep::EndVertexFilter(EndVertexFilter {
                ids: Some(smallvec![]),
                label_preds: vec![],
                property_preds: vec![],
            }),
            LogicalStep::Drop(DropStep {}),
            LogicalStep::Path(PathStep {}),
            LogicalStep::Dedup(DedupStep {}),
            LogicalStep::Fold(FoldStep {}),
        ];

        for step in steps.iter_mut() {
            let res = step.optimize(&dummy_rule);
            assert!(res.is_ok());
        }

        // Call optimize on a step struct instance directly to cover default Optimizer trait implementation
        let mut drop_step = DropStep {};
        let res = drop_step.optimize(&dummy_rule);
        assert!(res.is_ok());
    }
}
