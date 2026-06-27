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

#[cfg(test)]
use crate::types::gvalue::{Primitive, PrimitivePredicate};
use crate::{
    planner::{
        logical_step::{HasPropertyStep, LogicalPlan, LogicalStep},
        optimizer::primitive_to_rank,
    },
    types::{
        keys::{Rank, VertexKey},
        prop_key::ID,
        StoreError,
    },
};
use smallvec::SmallVec;

/// What a single end-vertex/rank filter contributed, applied to the anchor step in one shot.
enum Merge {
    EndVertexIds(SmallVec<[VertexKey; 4]>),
    Rank(Rank),
}

/// Extracts a rank value from a `HasRankStep` predicate to fold into a preceding
/// `OutE`/`InE`/`BothE` step. Only `Eq` is foldable — every other shape (`Gt`, `Between`,
/// `Within`, …) is left unfolded and returns `Ok(None)`.
fn extract_rank_from_predicate(pred: &crate::types::PrimitivePredicate) -> Result<Option<Rank>, StoreError> {
    use crate::types::gvalue::PrimitivePredicate;
    if let PrimitivePredicate::Eq(prim) = pred {
        return Ok(Some(primitive_to_rank(prim)?));
    }
    Ok(None)
}

/// An optimizer rule that merges an `EndVertexFilter` / `HasId` / `HasProperty("id", …)` step
/// or a `HasRank` step into a preceding edge traversal step
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
            | (LogicalStep::BothE(_), LogicalStep::EndVertexFilter(ef)) => {
                // Only merge ids — label/property predicates stay in a residual step.
                ef.ids.clone().map(Merge::EndVertexIds)
            }
            (LogicalStep::Out(_), LogicalStep::HasId(ef))
            | (LogicalStep::In(_), LogicalStep::HasId(ef))
            | (LogicalStep::Both(_), LogicalStep::HasId(ef)) => {
                super::extract_ids_from_predicate(&ef.pred)?.map(Merge::EndVertexIds)
            }
            (LogicalStep::Out(_), LogicalStep::HasProperty(HasPropertyStep { key, pred }))
            | (LogicalStep::In(_), LogicalStep::HasProperty(HasPropertyStep { key, pred }))
            | (LogicalStep::Both(_), LogicalStep::HasProperty(HasPropertyStep { key, pred }))
                if ID == *key =>
            {
                super::extract_ids_from_predicate(pred)?.map(Merge::EndVertexIds)
            }
            // Guarded by `rank.is_none()` — same precondition shape as `merge_haslabel_into_edge`'s
            // `labels.is_empty()` — so a second `hasRank()` on the same anchor is left unfolded
            // instead of silently overwriting the first (see regression tests below).
            (LogicalStep::OutE(oute), LogicalStep::HasRank(hr)) if oute.rank.is_none() => {
                extract_rank_from_predicate(&hr.pred)?.map(Merge::Rank)
            }
            (LogicalStep::InE(ine), LogicalStep::HasRank(hr)) if ine.rank.is_none() => {
                extract_rank_from_predicate(&hr.pred)?.map(Merge::Rank)
            }
            (LogicalStep::BothE(bothe), LogicalStep::HasRank(hr)) if bothe.rank.is_none() => {
                extract_rank_from_predicate(&hr.pred)?.map(Merge::Rank)
            }
            _ => None,
        };
        if let Some(merge) = merge {
            match (&mut plan.steps[i], merge) {
                (LogicalStep::OutE(oute), Merge::EndVertexIds(idv)) => intersect_ids(&mut oute.end_vertex_ids, idv),
                (LogicalStep::InE(ine), Merge::EndVertexIds(idv)) => intersect_ids(&mut ine.end_vertex_ids, idv),
                (LogicalStep::BothE(bothe), Merge::EndVertexIds(idv)) => intersect_ids(&mut bothe.end_vertex_ids, idv),
                (LogicalStep::Out(out), Merge::EndVertexIds(idv)) => intersect_ids(&mut out.end_vertex_ids, idv),
                (LogicalStep::In(in_), Merge::EndVertexIds(idv)) => intersect_ids(&mut in_.end_vertex_ids, idv),
                (LogicalStep::Both(both), Merge::EndVertexIds(idv)) => intersect_ids(&mut both.end_vertex_ids, idv),
                (LogicalStep::OutE(oute), Merge::Rank(r)) => oute.rank = Some(r),
                (LogicalStep::InE(ine), Merge::Rank(r)) => ine.rank = Some(r),
                (LogicalStep::BothE(bothe), Merge::Rank(r)) => bothe.rank = Some(r),
                _ => unreachable!(),
            }
            // If the consumed EndVertexFilter has non-id predicates, leave them as a residual.
            let has_residual = if let LogicalStep::EndVertexFilter(ef) = &plan.steps[j] {
                !ef.label_preds.is_empty() || !ef.property_preds.is_empty()
            } else {
                false
            };
            if has_residual {
                // Clear ids, keep label/property predicates.
                if let LogicalStep::EndVertexFilter(ef) = &mut plan.steps[j] {
                    ef.ids = None;
                }
            } else {
                plan.steps.remove(j);
            }
            plan_changed = true;
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

/// Intersect `ids` into `target`.  None = unconstrained, Some(empty) = matches nothing.
fn intersect_ids(target: &mut Option<SmallVec<[VertexKey; 4]>>, incoming: SmallVec<[VertexKey; 4]>) {
    match target {
        None => *target = Some(incoming),
        Some(ref mut existing) => existing.retain(|v| incoming.contains(v)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        planner::logical_step::{EndVertexFilter, HasRankStep, OutEStep, OutVStep, VStep},
        types::keys::VertexKey,
    };
    use smallvec::smallvec;

    fn out_e() -> LogicalStep {
        LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn evf(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::EndVertexFilter(EndVertexFilter {
            ids: Some(ids.into_iter().collect()),
            label_preds: vec![],
            property_preds: vec![],
        })
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
        let pred = PrimitivePredicate::Within(ids.into_iter().map(Primitive::Int64).collect());
        LogicalStep::HasId(HasIdStep { pred })
    }

    fn has_prop_id(id: i32) -> LogicalStep {
        use crate::planner::logical_step::HasPropertyStep;
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep {
            key: SmolStr::new("id"),
            pred: PrimitivePredicate::Eq(Primitive::Int32(id)),
        })
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
            pred: PrimitivePredicate::Eq(Primitive::String(SmolStr::new("oops"))),
        });
        let mut plan = LogicalPlan { steps: vec![out_step(), bad_prop] };
        let res = merge_end_vertex_filter(&mut plan);
        assert!(res.is_err(), "non-integer id type should return error");
    }

    fn has_rank(value: crate::types::Primitive) -> LogicalStep {
        LogicalStep::HasRank(HasRankStep { pred: PrimitivePredicate::Eq(value) })
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

    // OutE().HasRank(1).HasRank(2) — a second hasRank() on the same anchor must not silently
    // overwrite the first. Only the first folds; the second stays as a residual HasRank step
    // (which then correctly evaluates to always-false downstream, since every edge OutE now
    // emits already has rank=1, never rank=2 — the right outcome for an impossible conjunction).
    #[test]
    fn test_out_e_second_hasrank_not_merged() {
        let mut plan = LogicalPlan {
            steps: vec![
                out_e(),
                has_rank(crate::types::Primitive::Int32(1)),
                has_rank(crate::types::Primitive::Int32(2)),
            ],
        };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.rank, Some(1), "first hasRank() should fold");
        } else {
            panic!("expected OutE at step 0");
        }
        if let LogicalStep::HasRank(hr) = &plan.steps[1] {
            assert_eq!(hr.pred, PrimitivePredicate::Eq(crate::types::Primitive::Int32(2)));
        } else {
            panic!("expected residual HasRank at step 1, got something else");
        }
    }

    #[test]
    fn test_in_e_second_hasrank_not_merged() {
        let mut plan = LogicalPlan {
            steps: vec![
                in_e(),
                has_rank(crate::types::Primitive::Int32(1)),
                has_rank(crate::types::Primitive::Int32(2)),
            ],
        };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::InE(ine) = &plan.steps[0] {
            assert_eq!(ine.rank, Some(1), "first hasRank() should fold");
        } else {
            panic!("expected InE at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasRank(_)));
    }

    #[test]
    fn test_both_e_second_hasrank_not_merged() {
        let mut plan = LogicalPlan {
            steps: vec![
                both_e(),
                has_rank(crate::types::Primitive::Int32(1)),
                has_rank(crate::types::Primitive::Int32(2)),
            ],
        };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::BothE(bothe) = &plan.steps[0] {
            assert_eq!(bothe.rank, Some(1), "first hasRank() should fold");
        } else {
            panic!("expected BothE at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasRank(_)));
    }

    // Same value twice (redundant, not conflicting) — still only folds once; the second stays
    // as a residual that will correctly keep matching (rank==1 AND rank==1 is just rank==1).
    #[test]
    fn test_out_e_duplicate_same_value_hasrank_not_merged_but_consistent() {
        let mut plan = LogicalPlan {
            steps: vec![
                out_e(),
                has_rank(crate::types::Primitive::Int32(1)),
                has_rank(crate::types::Primitive::Int32(1)),
            ],
        };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.rank, Some(1));
        } else {
            panic!("expected OutE at step 0");
        }
        assert!(matches!(plan.steps[1], LogicalStep::HasRank(_)));
    }

    #[test]
    fn test_out_rank_not_merged() {
        // Out (vertex-emitting) has no rank field — HasRank after it is left alone.
        let mut plan = LogicalPlan { steps: vec![out_step(), has_rank(crate::types::Primitive::Int32(5))] };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_out_e_rank_bad_type_errors() {
        // String rank type in HasRank Eq predicate — primitive_to_rank returns error.
        use smol_str::SmolStr;
        let step = LogicalStep::HasRank(HasRankStep {
            pred: PrimitivePredicate::Eq(crate::types::Primitive::String(SmolStr::new("oops"))),
        });
        let mut plan = LogicalPlan { steps: vec![out_e(), step] };
        let res = merge_end_vertex_filter(&mut plan);
        assert!(res.is_err(), "non-integer rank type should return error from HasRank merge");
    }

    // `.has("rank", gt(100_000))` isn't a foldable shape (only Eq folds into OutE.rank) and its
    // literal is out of u16 range — but it's still a valid query that should just evaluate to
    // always-false via HasPropertyStep, not abort the whole optimizer pass.
    #[test]
    fn test_out_e_rank_out_of_range_non_eq_not_folded_no_error() {
        let mut plan = LogicalPlan {
            steps: vec![out_e(), has_rank_pred(PrimitivePredicate::Gt(crate::types::Primitive::Int64(100_000)))],
        };
        let changed = merge_end_vertex_filter(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::OutE(oute) = &plan.steps[0] {
            assert_eq!(oute.rank, None);
        } else {
            panic!("expected OutE");
        }
    }

    fn has_rank_pred(pred: PrimitivePredicate) -> LogicalStep {
        use smol_str::SmolStr;
        LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new("rank"), pred })
    }
}
