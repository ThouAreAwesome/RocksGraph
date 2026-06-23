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
        logical_step::{HasPropertyStep, LogicalPlan, LogicalStep},
        optimizer::primitive_to_rank,
    },
    types::{
        keys::{Rank, VertexKey},
        prop_key::{ID, RANK},
        Primitive, StoreError,
    },
};
use smallvec::{smallvec, SmallVec};

/// What a single end-vertex/rank filter contributed, applied to the anchor step in one shot.
enum Merge {
    EndVertexIds(SmallVec<[VertexKey; 4]>),
    Rank(Rank),
}

/// An optimizer rule that merges an `EndVertexFilter` / `HasId` / `HasProperty("id", …)` step,
/// or a `HasProperty("rank", …)` step, into a preceding edge traversal step
/// (`OutE`, `InE`, `BothE`, `Out`, `In`, `Both`).
///
/// This allows the edge traversal step to directly filter by the end vertex ID and/or edge
/// rank, pushing down predicates and potentially reducing the number of edges processed (or,
/// once both are known, turning the traversal into a single point lookup — see `GetEStep`).
/// Rank only applies to the edge-emitting steps (`OutE`/`InE`/`BothE`) since `Out`/`In`/`Both`
/// discard the edge before a caller could ever filter on its rank.
pub fn merge_end_vertex_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;
    while j < plan.steps.len() {
        let merge = match (&plan.steps[i], &plan.steps[j]) {
            (LogicalStep::OutE(_), LogicalStep::EndVertexFilter(ef))
            | (LogicalStep::InE(_), LogicalStep::EndVertexFilter(ef))
            | (LogicalStep::BothE(_), LogicalStep::EndVertexFilter(ef)) => Some(Merge::EndVertexIds(ef.ids.clone())),
            (LogicalStep::Out(_), LogicalStep::HasId(ef))
            | (LogicalStep::In(_), LogicalStep::HasId(ef))
            | (LogicalStep::Both(_), LogicalStep::HasId(ef)) => Some(Merge::EndVertexIds(ef.ids.clone())),
            (LogicalStep::Out(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
            | (LogicalStep::In(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
            | (LogicalStep::Both(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
                if ID == *key =>
            {
                match value {
                    Primitive::Int32(id) => Some(Merge::EndVertexIds(smallvec![*id as i64])),
                    Primitive::Int64(id) => Some(Merge::EndVertexIds(smallvec![*id])),
                    _ => return Err(StoreError::UnexpectedDataType("only i32 and i64 can be vertex id".into())),
                }
            }
            (LogicalStep::OutE(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
            | (LogicalStep::InE(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
            | (LogicalStep::BothE(_), LogicalStep::HasProperty(HasPropertyStep { key, value }))
                if RANK == *key =>
            {
                Some(Merge::Rank(primitive_to_rank(value)?))
            }
            _ => None,
        };
        if let Some(merge) = merge {
            match (&mut plan.steps[i], merge) {
                (LogicalStep::OutE(oute), Merge::EndVertexIds(idv)) => oute.end_vertex_ids = Some(idv),
                (LogicalStep::InE(ine), Merge::EndVertexIds(idv)) => ine.end_vertex_ids = Some(idv),
                (LogicalStep::BothE(bothe), Merge::EndVertexIds(idv)) => bothe.end_vertex_ids = Some(idv),
                (LogicalStep::Out(out), Merge::EndVertexIds(idv)) => out.end_vertex_ids = Some(idv),
                (LogicalStep::In(in_), Merge::EndVertexIds(idv)) => in_.end_vertex_ids = Some(idv),
                (LogicalStep::Both(both), Merge::EndVertexIds(idv)) => both.end_vertex_ids = Some(idv),
                (LogicalStep::OutE(oute), Merge::Rank(r)) => oute.rank = Some(r),
                (LogicalStep::InE(ine), Merge::Rank(r)) => ine.rank = Some(r),
                (LogicalStep::BothE(bothe), Merge::Rank(r)) => bothe.rank = Some(r),
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
        LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
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

    fn in_e() -> LogicalStep {
        use crate::planner::logical_step::InEStep;
        LogicalStep::InE(InEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn both_e() -> LogicalStep {
        use crate::planner::logical_step::BothEStep;
        LogicalStep::BothE(BothEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn out_step() -> LogicalStep {
        use crate::planner::logical_step::OutStep;
        LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })
    }

    fn in_step() -> LogicalStep {
        use crate::planner::logical_step::InStep;
        LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None })
    }

    fn both_step() -> LogicalStep {
        use crate::planner::logical_step::BothStep;
        LogicalStep::Both(BothStep { labels: smallvec![], end_vertex_ids: None })
    }

    fn has_id(ids: Vec<VertexKey>) -> LogicalStep {
        use crate::planner::logical_step::HasIdStep;
        LogicalStep::HasId(HasIdStep { ids: ids.into_iter().collect() })
    }

    fn has_prop_id(id: i32) -> LogicalStep {
        use crate::planner::logical_step::HasPropertyStep;
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("id"), value: crate::types::Primitive::Int32(id) })
    }

    #[test]
    fn test_in_e_evf_merged() {
        let mut plan = LogicalPlan { steps: vec![in_e(), evf(vec![5])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::InE(ine) = &plan.steps[0] {
            assert_eq!(ine.end_vertex_ids.as_deref(), Some(&[5i64][..]));
        } else {
            panic!("expected InE");
        }
    }

    #[test]
    fn test_both_e_evf_merged() {
        let mut plan = LogicalPlan { steps: vec![both_e(), evf(vec![7])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::BothE(be) = &plan.steps[0] {
            assert_eq!(be.end_vertex_ids.as_deref(), Some(&[7i64][..]));
        } else {
            panic!("expected BothE");
        }
    }

    #[test]
    fn test_out_has_id_merged() {
        let mut plan = LogicalPlan { steps: vec![out_step(), has_id(vec![3])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::Out(out) = &plan.steps[0] {
            assert_eq!(out.end_vertex_ids.as_deref(), Some(&[3i64][..]));
        } else {
            panic!("expected Out");
        }
    }

    #[test]
    fn test_in_has_id_merged() {
        let mut plan = LogicalPlan { steps: vec![in_step(), has_id(vec![4])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::In(in_s) = &plan.steps[0] {
            assert_eq!(in_s.end_vertex_ids.as_deref(), Some(&[4i64][..]));
        } else {
            panic!("expected In");
        }
    }

    #[test]
    fn test_both_has_id_merged() {
        let mut plan = LogicalPlan { steps: vec![both_step(), has_id(vec![9])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::Both(both) = &plan.steps[0] {
            assert_eq!(both.end_vertex_ids.as_deref(), Some(&[9i64][..]));
        } else {
            panic!("expected Both");
        }
    }

    #[test]
    fn test_out_has_property_id_merged() {
        let mut plan = LogicalPlan { steps: vec![out_step(), has_prop_id(11)] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::Out(out) = &plan.steps[0] {
            assert_eq!(out.end_vertex_ids.as_deref(), Some(&[11i64][..]));
        } else {
            panic!("expected Out");
        }
    }

    #[test]
    fn test_in_has_property_id_merged() {
        let mut plan = LogicalPlan { steps: vec![in_step(), has_prop_id(22)] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::In(in_s) = &plan.steps[0] {
            assert_eq!(in_s.end_vertex_ids.as_deref(), Some(&[22i64][..]));
        } else {
            panic!("expected In");
        }
    }

    #[test]
    fn test_both_has_property_id_merged() {
        let mut plan = LogicalPlan { steps: vec![both_step(), has_prop_id(33)] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::Both(both) = &plan.steps[0] {
            assert_eq!(both.end_vertex_ids.as_deref(), Some(&[33i64][..]));
        } else {
            panic!("expected Both");
        }
    }

    #[test]
    fn test_out_has_property_bad_type_errors() {
        use crate::planner::logical_step::HasPropertyStep;
        use smol_str::SmolStr;
        let bad_prop = LogicalStep::HasProperty(HasPropertyStep {
            key: SmolStr::new("id"),
            value: crate::types::Primitive::String(SmolStr::new("oops")),
        });
        let mut plan = LogicalPlan { steps: vec![out_step(), bad_prop] };
        let res = merge_end_vertex_filter(&mut plan);
        assert!(res.is_err(), "non-integer id type should return error");
    }

    fn has_rank(value: crate::types::Primitive) -> LogicalStep {
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("rank"), value })
    }

    #[test]
    fn test_out_e_rank_merged() {
        let mut plan = LogicalPlan { steps: vec![out_e(), has_rank(crate::types::Primitive::Int32(5))] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.rank, Some(5));
        } else {
            panic!("expected OutE");
        }
    }

    #[test]
    fn test_in_e_rank_merged() {
        let mut plan = LogicalPlan { steps: vec![in_e(), has_rank(crate::types::Primitive::Int64(9))] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        if let LogicalStep::InE(ine) = &plan.steps[0] {
            assert_eq!(ine.rank, Some(9));
        } else {
            panic!("expected InE");
        }
    }

    #[test]
    fn test_both_e_rank_merged() {
        let mut plan = LogicalPlan { steps: vec![both_e(), has_rank(crate::types::Primitive::Int32(3))] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        if let LogicalStep::BothE(be) = &plan.steps[0] {
            assert_eq!(be.rank, Some(3));
        } else {
            panic!("expected BothE");
        }
    }

    // OutE().EVF([1]).has("rank",5) -> OutE(end_vertex_ids=[1], rank=5) — both filters fold into the same step.
    #[test]
    fn test_out_e_evf_and_rank_both_merged() {
        let mut plan = LogicalPlan { steps: vec![out_e(), evf(vec![1]), has_rank(crate::types::Primitive::Int32(5))] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.end_vertex_ids.as_deref(), Some(&[1][..]));
            assert_eq!(oute.rank, Some(5));
        } else {
            panic!("expected OutE");
        }
    }

    // Order shouldn't matter: OutE().has("rank",5).EVF([1]) merges the same way.
    #[test]
    fn test_out_e_rank_and_evf_both_merged_reversed_order() {
        let mut plan = LogicalPlan { steps: vec![out_e(), has_rank(crate::types::Primitive::Int32(5)), evf(vec![1])] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.end_vertex_ids.as_deref(), Some(&[1][..]));
            assert_eq!(oute.rank, Some(5));
        } else {
            panic!("expected OutE");
        }
    }

    #[test]
    fn test_out_rank_not_merged() {
        // Out (vertex-emitting) has no rank field — has("rank", N) after it is left alone.
        let mut plan = LogicalPlan { steps: vec![out_step(), has_rank(crate::types::Primitive::Int32(5))] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_out_e_rank_bad_type_errors() {
        use smol_str::SmolStr;
        let mut plan =
            LogicalPlan { steps: vec![out_e(), has_rank(crate::types::Primitive::String(SmolStr::new("oops")))] };
        let res = merge_end_vertex_filter(&mut plan);
        assert!(res.is_err(), "non-integer rank type should return error");
    }
}
