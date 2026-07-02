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
    planner::logical_step::{LogicalPlan, LogicalStep},
    types::{prop_key::ID, StoreError},
};

/// Priority for filter reordering: lower sorts earlier.
/// `None` = not a reorderable filter step — acts as a barrier between runs.
fn filter_priority(step: &LogicalStep) -> Option<u8> {
    match step {
        LogicalStep::HasId(_) => Some(0),
        LogicalStep::HasProperty(hp) if hp.key == ID => Some(0),
        LogicalStep::HasLabel(_) => Some(1),
        LogicalStep::HasRank(_) => Some(2),
        LogicalStep::EndVertexFilter(_) => Some(3),
        LogicalStep::HasProperty(_) => Some(4), // any other key
        LogicalStep::Where(_) => Some(5),
        _ => None,
    }
}

/// Reorder adjacent filter steps into priority order:
///
/// `hasId = has("id"..) > hasLabel > hasRank > EndVertexFilter > has(not id..) > where()`
///
/// Each maximal contiguous run of reorderable steps (everything with a priority above)
/// is stable-sorted in one pass.  Ties (same priority between different keys) keep their
/// original relative order.
pub fn reorder_filters(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut i = 0;
    while i < plan.steps.len() {
        if filter_priority(&plan.steps[i]).is_none() {
            i += 1;
            continue;
        }
        // Find the end of this contiguous run of reorderable steps.
        let mut j = i + 1;
        while j < plan.steps.len() && filter_priority(&plan.steps[j]).is_some() {
            j += 1;
        }
        // Stable-sort [i, j) by priority.
        let run = &mut plan.steps[i..j];
        let before: Vec<u8> = run.iter().map(|s| filter_priority(s).unwrap()).collect();
        run.sort_by_key(|s| filter_priority(s).unwrap());
        let after: Vec<u8> = run.iter().map(|s| filter_priority(s).unwrap()).collect();
        if before != after {
            changed = true;
        }
        i = j;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::{
        planner::logical_step::{
            EndVertexFilter, HasIdStep, HasLabelStep, HasPropertyStep, HasRankStep, VStep, WhereStep,
        },
        types::{gvalue::Primitive, keys::VertexKey},
    };
    use smallvec::smallvec;

    fn v_all() -> LogicalStep {
        LogicalStep::V(VStep { ids: smallvec![] })
    }

    use crate::types::gvalue::PrimitivePredicate;

    fn has_prop(key: &str, value: Primitive) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), pred: PrimitivePredicate::Eq(value) })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        let pred = PrimitivePredicate::Within(ids.into_iter().map(Primitive::Int64).collect());
        LogicalStep::HasId(HasIdStep { pred })
    }

    fn has_label(labels: Vec<&str>) -> LogicalStep {
        let pred = PrimitivePredicate::Within(labels.into_iter().map(|l| Primitive::String(SmolStr::new(l))).collect());
        LogicalStep::HasLabel(HasLabelStep { pred })
    }

    fn whr(sub_steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Where(WhereStep { plan: LogicalPlan { steps: sub_steps } })
    }

    #[test]
    fn test_has_prop_then_has_id_swapped() {
        let mut plan = LogicalPlan {
            steps: vec![v_all(), has_prop("name", Primitive::String(SmolStr::new("marko"))), has_id(vec![1])],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_))); // hasId should be first
        assert!(matches!(plan.steps[2], LogicalStep::HasProperty(_))); // then hasProperty
    }

    #[test]
    fn test_has_label_then_has_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_label(vec!["10"]), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_))); // hasId should be first
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_))); // then hasLabel
    }

    #[test]
    fn test_has_label_then_has_prop_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_label(vec!["10"]), has_prop("id", Primitive::Int32(1))] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_))); // has("id",..) should be first
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_))); // then hasLabel
        if let LogicalStep::HasProperty(hp) = &plan.steps[1] {
            assert_eq!(hp.key.as_str(), ID);
        }
    }

    #[test]
    fn test_where_then_has_prop_swapped() {
        let mut plan = LogicalPlan {
            steps: vec![
                v_all(),
                whr(vec![has_id(vec![10])]),
                has_prop("name", Primitive::String(SmolStr::new("marko"))),
            ],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasProperty(_))); // hasProperty should be first
        assert!(matches!(plan.steps[2], LogicalStep::Where(_))); // then where
    }

    #[test]
    fn test_where_then_has_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), whr(vec![has_id(vec![10])]), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_))); // hasId should be first
        assert!(matches!(plan.steps[2], LogicalStep::Where(_))); // then where
    }

    #[test]
    fn test_where_then_has_label_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), whr(vec![has_id(vec![10])]), has_label(vec!["1"])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasLabel(_))); // hasLabel should be first
        assert!(matches!(plan.steps[2], LogicalStep::Where(_))); // then where
    }

    #[test]
    fn test_no_swap_needed_unchanged() {
        let mut plan = LogicalPlan {
            steps: vec![v_all(), has_id(vec![1]), has_prop("name", Primitive::String(SmolStr::new("marko")))],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed); // Already in preferred order
        assert_eq!(plan.steps.len(), 3);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_multiple_swaps_in_one_pass() {
        // Initial: V().HasProp(name).HasLabel().HasId()
        // Expected: V().HasId().HasLabel().HasProp(name)
        let mut plan = LogicalPlan {
            steps: vec![
                v_all(),
                has_prop("name", Primitive::String(SmolStr::new("marko"))),
                has_label(vec!["10"]),
                has_id(vec![1]),
            ],
        };
        while reorder_filters(&mut plan).unwrap() {}
        assert_eq!(plan.steps.len(), 4);
        assert!(matches!(plan.steps[0], LogicalStep::V(_)));
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_no_filters_unchanged() {
        let mut plan = LogicalPlan { steps: vec![v_all()] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 1);
    }

    fn has_rank(pred: PrimitivePredicate) -> LogicalStep {
        LogicalStep::HasRank(HasRankStep { pred })
    }

    fn evf(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::EndVertexFilter(EndVertexFilter {
            ids: Some(ids.into_iter().collect()),
            label_preds: vec![],
            property_preds: vec![],
        })
    }

    fn has_rank_eq(v: u16) -> LogicalStep {
        has_rank(PrimitivePredicate::Eq(Primitive::UInt16(v)))
    }

    #[test]
    fn test_where_then_has_rank_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), whr(vec![has_id(vec![10])]), has_rank_eq(5)] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasRank(_)));
        assert!(matches!(plan.steps[2], LogicalStep::Where(_)));
    }

    #[test]
    fn test_has_label_then_has_rank_no_swap() {
        // hasLabel before hasRank is already the preferred order — no swap.
        let mut plan = LogicalPlan { steps: vec![v_all(), has_label(vec!["10"]), has_rank_eq(5)] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasRank(_)));
    }

    #[test]
    fn test_has_rank_then_has_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_rank_eq(5), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasRank(_)));
    }

    #[test]
    fn test_has_prop_then_has_rank_swapped() {
        let mut plan = LogicalPlan {
            steps: vec![v_all(), has_prop("name", Primitive::String(SmolStr::new("marko"))), has_rank_eq(5)],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasRank(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_full_order_hasid_hasrank_haslabel_where() {
        // Final: hasLabel > hasRank > has > where  (no hasId in this plan)
        let mut plan = LogicalPlan {
            steps: vec![
                v_all(),
                has_prop("name", Primitive::String(SmolStr::new("marko"))),
                has_label(vec!["10"]),
                has_rank_eq(5),
                whr(vec![has_id(vec![10])]),
            ],
        };
        while reorder_filters(&mut plan).unwrap() {}
        assert!(matches!(plan.steps[1], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasRank(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasProperty(_)));
        assert!(matches!(plan.steps[4], LogicalStep::Where(_)));
    }

    #[test]
    fn test_has_prop_then_evf_swapped() {
        let mut plan = LogicalPlan {
            steps: vec![v_all(), has_prop("name", Primitive::String(SmolStr::new("marko"))), evf(vec![1])],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::EndVertexFilter(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasProperty(_)));
    }

    #[test]
    fn test_where_then_evf_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), whr(vec![has_id(vec![10])]), evf(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::EndVertexFilter(_)));
        assert!(matches!(plan.steps[2], LogicalStep::Where(_)));
    }

    #[test]
    fn test_has_rank_then_evf_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), evf(vec![1]), has_rank_eq(5)] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasRank(_)));
        assert!(matches!(plan.steps[2], LogicalStep::EndVertexFilter(_)));
    }

    #[test]
    fn test_evf_then_has_id_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), evf(vec![1]), has_id(vec![2])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::EndVertexFilter(_)));
    }

    #[test]
    fn test_has_label_then_evf_no_swap() {
        // hasLabel before EndVertexFilter is already correct (label more selective).
        let mut plan = LogicalPlan { steps: vec![v_all(), has_label(vec!["10"]), evf(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[2], LogicalStep::EndVertexFilter(_)));
    }

    #[test]
    fn test_evf_then_has_label_swapped() {
        let mut plan = LogicalPlan { steps: vec![v_all(), evf(vec![1]), has_label(vec!["person"])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        assert!(matches!(plan.steps[1], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[2], LogicalStep::EndVertexFilter(_)));
    }

    #[test]
    fn test_parity_reverse_order_all_six_kinds() {
        // All 6 reorderable kinds in reverse priority order — one pass should sort them.
        let mut plan = LogicalPlan {
            steps: vec![
                v_all(),
                whr(vec![has_id(vec![10])]),
                has_prop("name", Primitive::String(SmolStr::new("marko"))),
                has_rank_eq(5),
                evf(vec![1]),
                has_label(vec!["person"]),
                has_id(vec![2]),
            ],
        };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(changed);
        // After one pass: hasId > hasLabel > EndVertexFilter > hasRank > has > where
        assert!(matches!(plan.steps[1], LogicalStep::HasId(_)));
        assert!(matches!(plan.steps[2], LogicalStep::HasLabel(_)));
        assert!(matches!(plan.steps[3], LogicalStep::HasRank(_)));
        assert!(matches!(plan.steps[4], LogicalStep::EndVertexFilter(_)));
        assert!(matches!(plan.steps[5], LogicalStep::HasProperty(_)));
        assert!(matches!(plan.steps[6], LogicalStep::Where(_)));
        // Second pass: no changes.
        assert!(!reorder_filters(&mut plan).unwrap());
    }

    #[test]
    fn test_single_filter_unchanged() {
        let mut plan = LogicalPlan { steps: vec![v_all(), has_id(vec![1])] };
        let changed = reorder_filters(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }
}
