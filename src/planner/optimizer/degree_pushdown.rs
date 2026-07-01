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

//! Optimizer rule: `degree_pushdown` — replaces unfiltered `[edge-scan, Count]` pairs
//! with `[Degree, Sum]` using the O(1) `vertex_degree` CF counters.
//!
//! Three cascading rules:
//! - **Rule A**: `[unfiltered-edge-scan, Count]` → `[Degree(dir), Sum]`
//! - **Rule B**: `[Degree, Sum]` → `[Degree]` inside single-element contexts (local/where)
//! - **Rule C**: `Local([Degree(d)])` → `Degree(d)` (lift lone Degree out of local)

use crate::{
    planner::logical_step::{DegreeStep, LogicalPlan, LogicalStep, SumStep},
    types::{keys::DegreeDirection, StoreError},
};

/// Top-level optimizer entry point registered in `apply_rules`.
///
/// 1. Applies Rule A to the top-level plan.
/// 2. Recurses into `Local` and `Where` sub-plans, applying Rules A, B, and C.
pub fn degree_pushdown(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;

    // Rule A on the top-level plan
    changed |= apply_rule_a(plan);

    // Rules B + C applied to wrapper sub-plans
    let mut i = 0;
    while i < plan.steps.len() {
        let is_local = matches!(plan.steps[i], LogicalStep::Local(_));
        let is_where = matches!(plan.steps[i], LogicalStep::Where(_));

        if is_local {
            let LogicalStep::Local(ref mut local) = plan.steps[i] else { unreachable!() };
            changed |= degree_pushdown(&mut local.plan)?;
            changed |= apply_rule_b(&mut local.plan);

            // Rule C: if sub-plan is exactly [Degree(d)], lift it to outer plan.
            // Re-borrow immutably for the check.
            let lift = if let LogicalStep::Local(ref local_ref) = plan.steps[i] {
                if let [LogicalStep::Degree(ds)] = local_ref.plan.steps.as_slice() {
                    Some(ds.clone())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(ds) = lift {
                plan.steps[i] = LogicalStep::Degree(ds);
                changed = true;
            }
        } else if is_where {
            let LogicalStep::Where(ref mut wh) = plan.steps[i] else { unreachable!() };
            changed |= degree_pushdown(&mut wh.plan)?;
            changed |= apply_rule_b(&mut wh.plan);
            // Rule C does NOT apply to Where — it keeps its wrapper.
        }
        i += 1;
    }

    Ok(changed)
}

/// Rule A: scan `plan.steps` for consecutive `[unfiltered-edge-scan, Count]` pairs
/// and replace them with `[Degree(dir), Sum]`.
fn apply_rule_a(plan: &mut LogicalPlan) -> bool {
    let mut i = 0;
    let mut changed = false;
    while i + 1 < plan.steps.len() {
        if let Some(dir) = unfiltered_edge_scan_dir(&plan.steps[i]) {
            if matches!(plan.steps[i + 1], LogicalStep::Count(_)) {
                plan.steps.splice(
                    i..=i + 1,
                    [LogicalStep::Degree(DegreeStep { direction: dir }), LogicalStep::Sum(SumStep {})],
                );
                changed = true;
                i += 2; // skip the two newly inserted steps
                continue;
            }
        }
        i += 1;
    }
    changed
}

/// Rule B: inside a single-element context (local/where sub-plan), remove a `Sum`
/// that immediately follows a `Degree`.
///
/// Must only be called on sub-plans of `Local`/`Where`, never on the top-level plan.
fn apply_rule_b(plan: &mut LogicalPlan) -> bool {
    let mut i = 0;
    let mut changed = false;
    while i + 1 < plan.steps.len() {
        if matches!(plan.steps[i], LogicalStep::Degree(_)) && matches!(plan.steps[i + 1], LogicalStep::Sum(_)) {
            plan.steps.remove(i + 1); // remove Sum
            changed = true;
            // Stay at i: re-examine steps[i] and new steps[i+1]
        } else {
            i += 1;
        }
    }
    changed
}

/// Returns `Some(direction)` iff the step is a fully-unfiltered edge scan eligible for
/// Phase-1 degree pushdown:
/// - `labels` must be empty
/// - `end_vertex_ids` must be `None`
/// - `rank` must be `None` (E-steps only)
fn unfiltered_edge_scan_dir(step: &LogicalStep) -> Option<DegreeDirection> {
    match step {
        LogicalStep::Out(s) if s.labels.is_empty() && s.end_vertex_ids.is_none() => Some(DegreeDirection::Out),
        LogicalStep::OutE(s) if s.labels.is_empty() && s.end_vertex_ids.is_none() && s.rank.is_none() => {
            Some(DegreeDirection::Out)
        }
        LogicalStep::In(s) if s.labels.is_empty() && s.end_vertex_ids.is_none() => Some(DegreeDirection::In),
        LogicalStep::InE(s) if s.labels.is_empty() && s.end_vertex_ids.is_none() && s.rank.is_none() => {
            Some(DegreeDirection::In)
        }
        LogicalStep::Both(s) if s.labels.is_empty() && s.end_vertex_ids.is_none() => Some(DegreeDirection::Both),
        LogicalStep::BothE(s) if s.labels.is_empty() && s.end_vertex_ids.is_none() && s.rank.is_none() => {
            Some(DegreeDirection::Both)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::logical_step::{
        BothEStep, BothStep, CountStep, HasPropertyStep, InEStep, InStep, LocalStep, OutEStep, OutStep,
        ScalarFilterStep, WhereStep,
    };
    use crate::types::{gvalue::PrimitivePredicate, Primitive};
    use smallvec::smallvec;

    // ── Helper constructors ──────────────────────────────────────────────────

    fn out_unfiltered() -> LogicalStep {
        LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })
    }

    fn out_labeled() -> LogicalStep {
        LogicalStep::Out(OutStep { labels: smallvec!["knows".into()], end_vertex_ids: None })
    }

    fn out_with_dst() -> LogicalStep {
        LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: Some(smallvec![1]) })
    }

    fn out_e_unfiltered() -> LogicalStep {
        LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn out_e_with_rank() -> LogicalStep {
        LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: Some(0) })
    }

    fn in_unfiltered() -> LogicalStep {
        LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None })
    }

    fn in_e_unfiltered() -> LogicalStep {
        LogicalStep::InE(InEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn both_unfiltered() -> LogicalStep {
        LogicalStep::Both(BothStep { labels: smallvec![], end_vertex_ids: None })
    }

    fn both_e_unfiltered() -> LogicalStep {
        LogicalStep::BothE(BothEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }

    fn count() -> LogicalStep {
        LogicalStep::Count(CountStep {})
    }

    fn scalar_filter() -> LogicalStep {
        LogicalStep::ScalarFilter(ScalarFilterStep { pred: PrimitivePredicate::Gt(Primitive::Int64(2)) })
    }

    fn has_property() -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep {
            key: "age".into(),
            pred: PrimitivePredicate::Gt(Primitive::Int64(25)),
        })
    }

    fn plan(steps: Vec<LogicalStep>) -> LogicalPlan {
        LogicalPlan { steps }
    }

    fn local(steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Local(LocalStep { plan: plan(steps) })
    }

    fn whr(steps: Vec<LogicalStep>) -> LogicalStep {
        LogicalStep::Where(WhereStep { plan: plan(steps) })
    }

    fn degree_out() -> LogicalStep {
        LogicalStep::Degree(DegreeStep { direction: DegreeDirection::Out })
    }

    fn sum() -> LogicalStep {
        LogicalStep::Sum(SumStep {})
    }

    // Assert plan shape using pattern matching
    fn assert_degree(step: &LogicalStep, expected_dir: DegreeDirection) {
        match step {
            LogicalStep::Degree(ds) => assert_eq!(ds.direction, expected_dir, "DegreeStep direction mismatch"),
            other => panic!("Expected Degree step, got {:?}", std::mem::discriminant(other)),
        }
    }

    fn assert_sum(step: &LogicalStep) {
        assert!(matches!(step, LogicalStep::Sum(_)), "Expected Sum step");
    }

    fn assert_where_steps<F>(step: &LogicalStep, check: F)
    where
        F: FnOnce(&[LogicalStep]),
    {
        match step {
            LogicalStep::Where(w) => check(&w.plan.steps),
            other => panic!("Expected Where step, got {:?}", std::mem::discriminant(other)),
        }
    }

    // ── Rule A tests ─────────────────────────────────────────────────────────

    #[test]
    fn test_rule_a_out_unfiltered() {
        // [Out([]), Count] → [Degree(Out), Sum]
        let mut p = plan(vec![out_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::Out);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_rule_a_out_e_unfiltered() {
        // [OutE(labels=[], rank=None), Count] → [Degree(Out), Sum]
        let mut p = plan(vec![out_e_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::Out);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_rule_a_in_unfiltered() {
        // [In([]), Count] → [Degree(In), Sum]
        let mut p = plan(vec![in_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::In);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_rule_a_in_e_unfiltered() {
        // [InE(labels=[], rank=None), Count] → [Degree(In), Sum]
        let mut p = plan(vec![in_e_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::In);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_rule_a_both_unfiltered() {
        // [Both([]), Count] → [Degree(Both), Sum]
        let mut p = plan(vec![both_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::Both);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_rule_a_both_e_unfiltered() {
        // [BothE(labels=[], rank=None), Count] → [Degree(Both), Sum]
        let mut p = plan(vec![both_e_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::Both);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_rule_a_guard_labels_nonempty() {
        // [Out(labels=["knows"]), Count] → unchanged (labels non-empty)
        let mut p = plan(vec![out_labeled(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
        assert_eq!(p.steps.len(), 2);
        assert!(matches!(p.steps[0], LogicalStep::Out(_)));
        assert!(matches!(p.steps[1], LogicalStep::Count(_)));
    }

    #[test]
    fn test_rule_a_guard_dst_set() {
        // [Out(dst=Some([1])), Count] → unchanged (dst filter set)
        let mut p = plan(vec![out_with_dst(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
        assert_eq!(p.steps.len(), 2);
    }

    #[test]
    fn test_rule_a_guard_rank_set() {
        // [OutE(rank=Some(0)), Count] → unchanged (rank set)
        let mut p = plan(vec![out_e_with_rank(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
        assert_eq!(p.steps.len(), 2);
    }

    #[test]
    fn test_rule_a_guard_count_not_adjacent_has_property() {
        // [Out([]), HasProperty, Count] → unchanged (Count not adjacent to Out)
        let mut p = plan(vec![out_unfiltered(), has_property(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
        assert_eq!(p.steps.len(), 3);
    }

    #[test]
    fn test_rule_a_two_pairs() {
        // [Out([]), Count, Out([]), Count] → [Degree(Out), Sum, Degree(Out), Sum]
        let mut p = plan(vec![out_unfiltered(), count(), out_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 4);
        assert_degree(&p.steps[0], DegreeDirection::Out);
        assert_sum(&p.steps[1]);
        assert_degree(&p.steps[2], DegreeDirection::Out);
        assert_sum(&p.steps[3]);
    }

    // ── Rule B tests (via Local/Where wrappers) ───────────────────────────────

    #[test]
    fn test_rule_b_removes_sum_after_degree_in_local() {
        // Local([Degree(Out), Sum]):
        // - Rule B fires: removes Sum → Local([Degree(Out)])
        // - Rule C fires: lifts lone Degree → Degree(Out) in outer plan
        // Both happen in a single degree_pushdown call.
        let mut p = plan(vec![local(vec![degree_out(), sum()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 1, "Local+Sum+B+C cascade should yield one outer step");
        assert_degree(&p.steps[0], DegreeDirection::Out);
    }

    #[test]
    fn test_rule_b_removes_sum_mid_plan_in_local() {
        // Local([Degree(Out), Sum, ScalarFilter(gt(2))]) → Local([Degree(Out), ScalarFilter(gt(2))])
        let mut p = plan(vec![local(vec![degree_out(), sum(), scalar_filter()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        match &p.steps[0] {
            LogicalStep::Local(l) => {
                assert_eq!(l.plan.steps.len(), 2);
                assert_degree(&l.plan.steps[0], DegreeDirection::Out);
                assert!(matches!(l.plan.steps[1], LogicalStep::ScalarFilter(_)));
            }
            other => panic!("Expected Local, got {:?}", std::mem::discriminant(other)),
        }
    }

    #[test]
    fn test_rule_b_removes_sum_in_where() {
        // Where([Degree(Out), Sum]) → Where([Degree(Out)])
        let mut p = plan(vec![whr(vec![degree_out(), sum()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_where_steps(&p.steps[0], |steps| {
            assert_eq!(steps.len(), 1);
            assert_degree(&steps[0], DegreeDirection::Out);
        });
    }

    #[test]
    fn test_rule_b_does_not_fire_at_top_level() {
        // [Degree(Out), Sum] at top level → unchanged (Rule B only in single-element context)
        let mut p = plan(vec![degree_out(), sum()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed, "Rule B must not fire at top level");
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::Out);
        assert_sum(&p.steps[1]);
    }

    // ── Rule C tests ──────────────────────────────────────────────────────────

    #[test]
    fn test_rule_c_lifts_lone_degree_from_local() {
        // Local([Degree(Out)]) → Degree(Out)
        let mut p = plan(vec![local(vec![degree_out()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 1);
        assert_degree(&p.steps[0], DegreeDirection::Out);
    }

    #[test]
    fn test_rule_c_does_not_fire_when_local_has_two_steps() {
        // Local([Degree(Out), ScalarFilter]) — no Rule C (length 2)
        let mut p = plan(vec![local(vec![degree_out(), scalar_filter()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed, "Rule C must not fire when Local has >1 step");
        assert!(matches!(p.steps[0], LogicalStep::Local(_)));
    }

    #[test]
    fn test_rule_c_does_not_fire_for_non_degree_single_step_local() {
        // Local([Count]) — no Rule C (not Degree)
        let mut p = plan(vec![local(vec![count()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
        assert!(matches!(p.steps[0], LogicalStep::Local(_)));
    }

    #[test]
    fn test_rule_c_does_not_lift_from_where() {
        // Where([Degree(Out)]) — Rule C does NOT apply to Where
        let mut p = plan(vec![whr(vec![degree_out()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed, "Rule C must not lift Degree out of Where");
        assert!(matches!(p.steps[0], LogicalStep::Where(_)));
    }

    // ── Full cascade tests ─────────────────────────────────────────────────────

    #[test]
    fn test_cascade_out_count_at_top_level() {
        // [Out([]), Count] → [Degree(Out), Sum]
        let mut p = plan(vec![out_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_eq!(p.steps.len(), 2);
        assert_degree(&p.steps[0], DegreeDirection::Out);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_cascade_local_out_count_full_abc() {
        // Local([Out([]), Count]) → Degree(Out)  (A + B + C)
        let mut p = plan(vec![local(vec![out_unfiltered(), count()])]);
        // Run multiple passes like apply_rules does
        let mut total_changed = false;
        loop {
            let changed = degree_pushdown(&mut p).unwrap();
            if !changed {
                break;
            }
            total_changed = true;
        }
        assert!(total_changed);
        assert_eq!(p.steps.len(), 1);
        assert_degree(&p.steps[0], DegreeDirection::Out);
    }

    #[test]
    fn test_cascade_local_both_count_full_abc() {
        // Local([Both([]), Count]) → Degree(Both)  (A + B + C)
        let mut p = plan(vec![local(vec![both_unfiltered(), count()])]);
        let mut total_changed = false;
        loop {
            let changed = degree_pushdown(&mut p).unwrap();
            if !changed {
                break;
            }
            total_changed = true;
        }
        assert!(total_changed);
        assert_eq!(p.steps.len(), 1);
        assert_degree(&p.steps[0], DegreeDirection::Both);
    }

    #[test]
    fn test_cascade_where_out_count_a_plus_b() {
        // Where([Out([]), Count]) → Where([Degree(Out)])  (A + B, no C for Where)
        let mut p = plan(vec![whr(vec![out_unfiltered(), count()])]);
        let mut total_changed = false;
        loop {
            let changed = degree_pushdown(&mut p).unwrap();
            if !changed {
                break;
            }
            total_changed = true;
        }
        assert!(total_changed);
        assert_eq!(p.steps.len(), 1);
        assert_where_steps(&p.steps[0], |steps| {
            assert_eq!(steps.len(), 1);
            assert_degree(&steps[0], DegreeDirection::Out);
        });
    }

    #[test]
    fn test_cascade_where_out_count_scalar_filter() {
        // Where([Out([]), Count, ScalarFilter(gt(2))]) → Where([Degree(Out), ScalarFilter(gt(2))])
        let mut p = plan(vec![whr(vec![out_unfiltered(), count(), scalar_filter()])]);
        let mut total_changed = false;
        loop {
            let changed = degree_pushdown(&mut p).unwrap();
            if !changed {
                break;
            }
            total_changed = true;
        }
        assert!(total_changed);
        assert_where_steps(&p.steps[0], |steps| {
            assert_eq!(steps.len(), 2);
            assert_degree(&steps[0], DegreeDirection::Out);
            assert!(matches!(steps[1], LogicalStep::ScalarFilter(_)));
        });
    }

    #[test]
    fn test_cascade_labeled_out_unchanged() {
        // [Out(["knows"]), Count] → unchanged
        let mut p = plan(vec![out_labeled(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_cascade_local_labeled_out_unchanged() {
        // Local([Out(["knows"]), Count]) → unchanged
        let mut p = plan(vec![local(vec![out_labeled(), count()])]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_cascade_out_has_property_count_unchanged() {
        // [Out([]), HasProperty, Count] → unchanged (Count not adjacent to Out)
        let mut p = plan(vec![out_unfiltered(), has_property(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(!changed);
    }

    #[test]
    fn test_in_direction() {
        // [In([]), Count] → [Degree(In), Sum]
        let mut p = plan(vec![in_unfiltered(), count()]);
        let changed = degree_pushdown(&mut p).unwrap();
        assert!(changed);
        assert_degree(&p.steps[0], DegreeDirection::In);
        assert_sum(&p.steps[1]);
    }

    #[test]
    fn test_local_in_count_full_cascade() {
        // Local([In([]), Count]) → Degree(In) via A+B+C
        let mut p = plan(vec![local(vec![in_unfiltered(), count()])]);
        let mut total_changed = false;
        loop {
            let changed = degree_pushdown(&mut p).unwrap();
            if !changed {
                break;
            }
            total_changed = true;
        }
        assert!(total_changed);
        assert_degree(&p.steps[0], DegreeDirection::In);
    }

    #[test]
    fn test_local_in_e_count_full_cascade() {
        // Local([InE([]), Count]) → Degree(In) via A+B+C
        let mut p = plan(vec![local(vec![in_e_unfiltered(), count()])]);
        let mut total_changed = false;
        loop {
            let changed = degree_pushdown(&mut p).unwrap();
            if !changed {
                break;
            }
            total_changed = true;
        }
        assert!(total_changed);
        assert_degree(&p.steps[0], DegreeDirection::In);
    }
}
