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

//! Folds `hasLabel` / `has("label", …)` into a preceding `outE` / `inE` /
//! `bothE` step when the edge step carries no label restriction yet.
//!
//! `"label"` is a reserved key (`docs/design_reserved_keys.md`) — this fold is what lets
//! `.outE().has("label", N)` keep working; an unfolded `.has("label", …)` not adjacent to
//! an edge-traversal step is rejected by `reject_reserved_key` in `build_step.rs` instead
//! of reaching `HasPropertyStep`.
//!
//! ```text
//! outE().hasLabel("knows")          → outE("knows")
//! bothE().has("label", "knows")     → bothE("knows")
//! ```
//!
//! Only `Eq` and `Within` predicates are folded; range predicates (`Gt`,
//! `Lt`, …) and negative predicates (`Ne`, `Without`) are valid `HasLabel`
//! filters that cannot be represented as an edge-step allowlist, so they
//! are left in place.

use crate::types::SMALL_VECTOR_LENGTH;
use crate::{
    planner::logical_step::{HasPropertyStep, LogicalPlan, LogicalStep},
    types::{prop_key, Primitive, PrimitivePredicate, StoreError},
};
use smallvec::SmallVec;
use smol_str::SmolStr;

/// Extracts label names from a predicate, returning `None` when the
/// predicate shape cannot be folded into an edge-step allowlist
/// (e.g. `Ne`, `Gt`, `Without`, or `Eq` with a non-String literal).
fn extract_labels_from_predicate(
    pred: &PrimitivePredicate,
) -> Result<Option<SmallVec<[SmolStr; SMALL_VECTOR_LENGTH]>>, StoreError> {
    fn to_label(v: &Primitive) -> Result<SmolStr, StoreError> {
        match v {
            Primitive::String(s) => Ok(s.clone()),
            other => Err(StoreError::UnexpectedDataType(format!("expected string label, got {other:?}"))),
        }
    }

    match pred {
        PrimitivePredicate::Eq(v) => Ok(Some({
            let mut labels = SmallVec::new();
            labels.push(to_label(v)?);
            labels
        })),
        PrimitivePredicate::Within(vs) => {
            let mut labels = SmallVec::new();
            for v in vs {
                labels.push(to_label(v)?);
            }
            Ok(if labels.is_empty() { None } else { Some(labels) })
        }
        _ => Ok(None),
    }
}

/// Checks whether a `HasPropertyStep` targets the label pseudo-property.
fn is_label_property(hp: &HasPropertyStep) -> bool {
    hp.key.as_str() == prop_key::LABEL
}

/// See module-level documentation.
pub fn merge_haslabel_into_edge(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut plan_changed = false;
    let mut i = 0;
    let mut j = 1;

    while j < plan.steps.len() {
        let labels: Option<SmallVec<[SmolStr; SMALL_VECTOR_LENGTH]>> = match (&plan.steps[i], &plan.steps[j]) {
            // ── OutE + HasLabel ───────────────────────────────────────────
            (LogicalStep::OutE(out_e), LogicalStep::HasLabel(hl)) if out_e.labels.is_empty() => {
                extract_labels_from_predicate(&hl.pred)?
            }
            (LogicalStep::OutE(out_e), LogicalStep::HasProperty(hp))
                if out_e.labels.is_empty() && is_label_property(hp) =>
            {
                extract_labels_from_predicate(&hp.pred)?
            }
            // ── InE + HasLabel ────────────────────────────────────────────
            (LogicalStep::InE(in_e), LogicalStep::HasLabel(hl)) if in_e.labels.is_empty() => {
                extract_labels_from_predicate(&hl.pred)?
            }
            (LogicalStep::InE(in_e), LogicalStep::HasProperty(hp))
                if in_e.labels.is_empty() && is_label_property(hp) =>
            {
                extract_labels_from_predicate(&hp.pred)?
            }
            // ── BothE + HasLabel ──────────────────────────────────────────
            (LogicalStep::BothE(both_e), LogicalStep::HasLabel(hl)) if both_e.labels.is_empty() => {
                extract_labels_from_predicate(&hl.pred)?
            }
            (LogicalStep::BothE(both_e), LogicalStep::HasProperty(hp))
                if both_e.labels.is_empty() && is_label_property(hp) =>
            {
                extract_labels_from_predicate(&hp.pred)?
            }

            _ => None,
        };

        if let Some(labels) = labels {
            match &mut plan.steps[i] {
                LogicalStep::OutE(ref mut s) => s.labels = labels,
                LogicalStep::InE(ref mut s) => s.labels = labels,
                LogicalStep::BothE(ref mut s) => s.labels = labels,
                _ => unreachable!("should never reach here since we have checked the pattern already"),
            }
            plan.steps.remove(j);
            plan_changed = true;
        } else {
            i = j;
            j += 1;
        }
    }

    Ok(plan_changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::logical_step::{BothEStep, HasLabelStep, InEStep, OutEStep};
    use smallvec::smallvec;

    fn out_e_empty() -> LogicalStep {
        LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }
    fn in_e_empty() -> LogicalStep {
        LogicalStep::InE(InEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }
    fn both_e_empty() -> LogicalStep {
        LogicalStep::BothE(BothEStep { labels: smallvec![], end_vertex_ids: None, rank: None })
    }
    fn has_label_eq(name: &str) -> LogicalStep {
        LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Eq(Primitive::String(SmolStr::from(name))) })
    }
    fn has_label_within(names: &[&str]) -> LogicalStep {
        LogicalStep::HasLabel(HasLabelStep {
            pred: PrimitivePredicate::Within(names.iter().map(|n| Primitive::String(SmolStr::from(*n))).collect()),
        })
    }
    fn has_prop_label_eq(name: &str) -> LogicalStep {
        LogicalStep::HasProperty(HasPropertyStep {
            key: prop_key::LABEL,
            pred: PrimitivePredicate::Eq(Primitive::String(SmolStr::from(name))),
        })
    }

    fn edge_labels(step: &LogicalStep) -> &[SmolStr] {
        match step {
            LogicalStep::OutE(s) => &s.labels,
            LogicalStep::InE(s) => &s.labels,
            LogicalStep::BothE(s) => &s.labels,
            _ => panic!("not an edge step"),
        }
    }

    #[test]
    fn test_out_e_haslabel_folded() {
        let mut plan = LogicalPlan { steps: vec![out_e_empty(), has_label_eq("knows")] };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(edge_labels(&plan.steps[0]), &[SmolStr::from("knows")]);
    }

    #[test]
    fn test_in_e_haslabel_folded() {
        let mut plan = LogicalPlan { steps: vec![in_e_empty(), has_label_eq("created")] };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(edge_labels(&plan.steps[0]), &[SmolStr::from("created")]);
    }

    #[test]
    fn test_both_e_haslabel_folded() {
        let mut plan = LogicalPlan { steps: vec![both_e_empty(), has_label_eq("works_for")] };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(edge_labels(&plan.steps[0]), &[SmolStr::from("works_for")]);
    }

    #[test]
    fn test_haslabel_within_folded() {
        let mut plan = LogicalPlan { steps: vec![out_e_empty(), has_label_within(&["knows", "created"])] };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(edge_labels(&plan.steps[0]), &[SmolStr::from("knows"), SmolStr::from("created")]);
    }

    #[test]
    fn test_has_property_label_eq_folded() {
        let mut plan = LogicalPlan { steps: vec![out_e_empty(), has_prop_label_eq("knows")] };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(edge_labels(&plan.steps[0]), &[SmolStr::from("knows")]);
    }

    #[test]
    fn test_edge_already_has_labels_not_merged() {
        let mut plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(OutEStep {
                    labels: smallvec![SmolStr::from("knows")],
                    end_vertex_ids: None,
                    rank: None,
                }),
                has_label_eq("created"),
            ],
        };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_haslabel_ne_not_folded() {
        let mut plan = LogicalPlan {
            steps: vec![
                out_e_empty(),
                LogicalStep::HasLabel(HasLabelStep {
                    pred: PrimitivePredicate::Ne(Primitive::String(SmolStr::from("knows"))),
                }),
            ],
        };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_has_non_label_property_not_folded() {
        let mut plan = LogicalPlan {
            steps: vec![
                out_e_empty(),
                LogicalStep::HasProperty(HasPropertyStep {
                    key: SmolStr::from("weight"),
                    pred: PrimitivePredicate::Eq(Primitive::Float64(0.5)),
                }),
            ],
        };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_non_adjacent_haslabel_not_folded() {
        let mut plan = LogicalPlan {
            steps: vec![
                out_e_empty(),
                LogicalStep::Count(crate::planner::logical_step::CountStep {}),
                has_label_eq("knows"),
            ],
        };
        let changed = merge_haslabel_into_edge(&mut plan).unwrap();
        assert!(!changed);
        assert_eq!(plan.steps.len(), 3);
    }

    #[test]
    fn test_integration_via_apply_rules() {
        let mut plan = LogicalPlan { steps: vec![out_e_empty(), has_label_eq("knows")] };
        let changed = crate::planner::apply_rules(&mut plan).unwrap();
        assert!(changed || plan.steps.len() == 1);
        assert_eq!(plan.steps.len(), 1);
    }
}
