use crate::planner::logical_step::{LogicalPlan, LogicalStep, OtherVStep};
use crate::types::StoreError;

/// Normalise `inV()` / `outV()` to `otherV()` after edge-emitting steps so the
/// `extract_end_vertex_filter` optimizer can recognise the pattern.
///
/// Only two conversions are semantically safe:
/// - `outE().inV()` → `outE().otherV()`  (both reach the destination)
/// - `inE().outV()` → `inE().otherV()`   (both reach the source)
///
/// `bothE()` is deliberately left alone — `inV`/`outV` can point back to the
/// current vertex for half the edges, which `otherV` never does.
pub fn normalize_inv_outv(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut last_edge: Option<EdgeDir> = None;

    for step in &mut plan.steps {
        match step {
            LogicalStep::OutE(_) => last_edge = Some(EdgeDir::OutE),
            LogicalStep::InE(_) => last_edge = Some(EdgeDir::InE),

            LogicalStep::InV(_) if last_edge == Some(EdgeDir::OutE) => {
                *step = LogicalStep::OtherV(OtherVStep {});
                changed = true;
            }
            LogicalStep::OutV(_) if last_edge == Some(EdgeDir::InE) => {
                *step = LogicalStep::OtherV(OtherVStep {});
                changed = true;
            }

            LogicalStep::Where(wh) if last_edge.is_some() => {
                if let Some(first) = wh.plan.steps.first() {
                    let should_convert = match last_edge {
                        Some(EdgeDir::OutE) => matches!(first, LogicalStep::InV(_)),
                        Some(EdgeDir::InE) => matches!(first, LogicalStep::OutV(_)),
                        None => false,
                    };
                    if should_convert {
                        wh.plan.steps[0] = LogicalStep::OtherV(OtherVStep {});
                        changed = true;
                    }
                }
            }

            LogicalStep::HasLabel(_)
            | LogicalStep::HasId(_)
            | LogicalStep::HasProperty(_)
            | LogicalStep::HasRank(_)
            | LogicalStep::EndVertexFilter(_)
            | LogicalStep::ScalarFilter(_)
            | LogicalStep::SimplePath(_)
            | LogicalStep::CyclicPath(_)
            | LogicalStep::Dedup(_)
            | LogicalStep::Limit(_)
            | LogicalStep::Range(_)
            | LogicalStep::Skip(_)
            | LogicalStep::Tail(_)
            | LogicalStep::OtherV(_)
            | LogicalStep::Path(_)
            | LogicalStep::Where(_)
            | LogicalStep::Identity(_) => {}

            _ => last_edge = None,
        }
    }
    Ok(changed)
}

#[derive(PartialEq)]
enum EdgeDir {
    OutE,
    InE,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::logical_step::{HasIdStep, InEStep, InVStep, OutEStep, OutVStep, WhereStep};
    use crate::types::gvalue::{Primitive, PrimitivePredicate};
    use smallvec::smallvec;

    #[test]
    fn test_oute_inv_becomes_otherv() {
        let mut plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::InV(InVStep {}),
            ],
        };
        assert!(normalize_inv_outv(&mut plan).unwrap());
        assert!(matches!(plan.steps[1], LogicalStep::OtherV(_)));
    }

    #[test]
    fn test_oute_outv_unchanged() {
        let mut plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::OutV(OutVStep {}),
            ],
        };
        assert!(!normalize_inv_outv(&mut plan).unwrap());
        assert!(matches!(plan.steps[1], LogicalStep::OutV(_)));
    }

    #[test]
    fn test_ine_outv_becomes_otherv() {
        let mut plan = LogicalPlan {
            steps: vec![
                LogicalStep::InE(InEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::OutV(OutVStep {}),
            ],
        };
        assert!(normalize_inv_outv(&mut plan).unwrap());
        assert!(matches!(plan.steps[1], LogicalStep::OtherV(_)));
    }

    #[test]
    fn test_filter_between_edge_and_inv_allows_conversion() {
        let mut plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::HasLabel(crate::planner::logical_step::HasLabelStep {
                    pred: PrimitivePredicate::Eq(Primitive::String("person".into())),
                }),
                LogicalStep::InV(InVStep {}),
            ],
        };
        assert!(normalize_inv_outv(&mut plan).unwrap());
        assert!(matches!(plan.steps[2], LogicalStep::OtherV(_)));
    }

    #[test]
    fn test_where_inv_becomes_otherv() {
        let wh = LogicalPlan {
            steps: vec![
                LogicalStep::InV(InVStep {}),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
            ],
        };
        let mut plan = LogicalPlan {
            steps: vec![
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: wh }),
            ],
        };
        assert!(normalize_inv_outv(&mut plan).unwrap());
        if let LogicalStep::Where(wh2) = &plan.steps[1] {
            assert!(matches!(wh2.plan.steps[0], LogicalStep::OtherV(_)));
        }
    }

    #[test]
    fn test_inv_without_edge_step_unchanged() {
        let mut plan = LogicalPlan { steps: vec![LogicalStep::InV(InVStep {})] };
        assert!(!normalize_inv_outv(&mut plan).unwrap());
    }
}
