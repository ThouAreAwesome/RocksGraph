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

use crate::types::PIPELINE_BATCH_INLINE;
use smallvec::SmallVec;
use smol_str::SmolStr;

use crate::{
    planner::logical_step::{EndVertexFilter, LogicalPlan, LogicalStep, WhereStep},
    types::{prop_key::ID, PrimitivePredicate, StoreError},
};

/// Intersect `incoming` into `target`.  None = unconstrained; Some = current list.
fn intersect_option_ids(
    target: &mut Option<SmallVec<[i64; PIPELINE_BATCH_INLINE]>>,
    incoming: SmallVec<[i64; PIPELINE_BATCH_INLINE]>,
) {
    match target {
        None => *target = Some(incoming),
        Some(ref mut existing) => existing.retain(|v| incoming.contains(v)),
    }
}

/// An optimizer rule that extracts filter predicates from `where(otherV()…)` sub-plans
/// into a generalized `EndVertexFilter`.
///
/// Any linear chain of filter steps after `OtherV` — `HasId`, `HasProperty(id)`,
/// `HasLabel`, `HasProperty(other)` — is extracted.  The first non-filter step
/// (e.g. `Out`, `And`, `Or`) stops extraction and leaves the entire `where()`
/// untouched.
///
/// This allows subsequent optimization rules to merge the `EndVertexFilter`'s `ids`
/// directly into edge traversal steps, while label and property predicates run
/// through the fused `EndVertexFilterStep` at the physical level.
pub fn extract_end_vertex_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    let mut new_steps = Vec::with_capacity(plan.steps.len());

    for step in std::mem::take(&mut plan.steps) {
        if let LogicalStep::Where(wh) = step {
            let mut sub = wh.plan;
            if sub.steps.len() < 2 || !matches!(sub.steps.first(), Some(LogicalStep::OtherV(_))) {
                new_steps.push(LogicalStep::Where(WhereStep { plan: sub }));
                continue;
            }
            // Reorder the sub-plan so id filter comes first.
            crate::planner::optimizer::reorder_filter::reorder_filters(&mut sub)?;

            let mut ids: Option<SmallVec<[i64; PIPELINE_BATCH_INLINE]>> = None;
            let mut label_preds: Vec<PrimitivePredicate> = Vec::new();
            let mut property_preds: Vec<(SmolStr, PrimitivePredicate)> = Vec::new();
            let mut all_filters = true;

            for s in &sub.steps[1..] {
                match s {
                    LogicalStep::HasId(hi) => {
                        if let Some(found) = super::extract_ids_from_predicate(&hi.pred)? {
                            intersect_option_ids(&mut ids, found);
                        }
                    }
                    LogicalStep::HasProperty(hp) if hp.key.as_str() == ID => {
                        if let Some(found) = super::extract_ids_from_predicate(&hp.pred)? {
                            intersect_option_ids(&mut ids, found);
                        }
                    }
                    LogicalStep::HasLabel(hl) => {
                        // Label has no structural lookup-key role (unlike `ids`/edge `rank`),
                        // so multiple label predicates on the same chain just accumulate —
                        // ANDed, same as property_preds.
                        label_preds.push(hl.pred.clone());
                    }
                    LogicalStep::HasProperty(hp) => {
                        property_preds.push((hp.key.clone(), hp.pred.clone()));
                    }
                    _ => {
                        all_filters = false;
                        break;
                    }
                }
            }

            if all_filters {
                new_steps.push(LogicalStep::EndVertexFilter(EndVertexFilter { ids, label_preds, property_preds }));
                changed = true;
            } else {
                new_steps.push(LogicalStep::Where(WhereStep { plan: sub }));
            }
        } else {
            new_steps.push(step);
        }
    }

    plan.steps = new_steps;
    Ok(changed)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::{
        planner::logical_step::{HasIdStep, HasPropertyStep, OtherVStep, VStep, WhereStep},
        types::{
            gvalue::{Primitive, PrimitivePredicate},
            keys::VertexKey,
        },
    };

    fn v_ids(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids: ids.into_iter().collect() })
    }

    fn whr_all() -> LogicalStep {
        let pred = PrimitivePredicate::Within(vec![Primitive::Int64(1), Primitive::Int64(2), Primitive::Int64(3)]);
        LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![LogicalStep::OtherV(OtherVStep {}), LogicalStep::HasId(HasIdStep { pred })],
            },
        })
    }

    fn whr_has_pro() -> LogicalStep {
        LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep {
                        key: ID,
                        pred: PrimitivePredicate::Eq(Primitive::Int32(123)),
                    }),
                ],
            },
        })
    }

    fn whr_has() -> LogicalStep {
        LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep {
                        key: "name".into(),
                        pred: PrimitivePredicate::Eq(Primitive::Int32(123)),
                    }),
                ],
            },
        })
    }

    // fn has(key: &str, value: Primitive) -> LogicalStep {
    //     LogicalStep::HasProperty(HasPropertyStep { key: SmolStr::new(key), value })
    // }

    #[test]
    fn test_where_other_v_has_id_extracted() {
        let steps = vec![whr_all()];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[0] {
            assert_eq!(evf.ids.as_deref().unwrap(), &[1, 2, 3]);
        } else {
            panic!("expected EndVertexFilter");
        }
    }
    #[test]
    fn test_v_where_other_v_has_id_extracted() {
        let steps = vec![v_ids(vec![1, 2]), whr_all()];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[1] {
            assert_eq!(evf.ids.as_deref().unwrap(), &[1, 2, 3]);
        } else {
            panic!("expected EndVertexFilter");
        }
    }

    #[test]
    fn test_v_where_other_v_has_property_extracted() {
        let steps = vec![v_ids(vec![1, 2]), whr_has_pro()];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        assert_eq!(plan.steps.len(), 2);
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[1] {
            assert_eq!(evf.ids.as_deref().unwrap(), &[123]);
        } else {
            panic!("expected EndVertexFilter");
        }
    }

    #[test]
    fn test_v_where_other_v_has_property_extracted_to_endvertex() {
        let steps = vec![v_ids(vec![1, 2]), whr_has()];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "property-only where() should extract into EndVertexFilter");
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(&plan.steps[1], LogicalStep::EndVertexFilter(_)));
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[1] {
            assert_eq!(evf.property_preds.len(), 1);
            assert_eq!(evf.property_preds[0].0.as_str(), "name");
        }
    }

    #[test]
    fn test_where_other_v_has_property_int64_extracted() {
        let steps = vec![LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep {
                        key: ID,
                        pred: PrimitivePredicate::Eq(Primitive::Int64(999)),
                    }),
                ],
            },
        })];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed");
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[0] {
            assert_eq!(evf.ids.as_deref().unwrap(), &[999i64]);
        } else {
            panic!("expected EndVertexFilter");
        }
    }

    #[test]
    fn test_where_other_v_has_property_bad_type_errors() {
        let steps = vec![LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep {
                        key: ID,
                        pred: PrimitivePredicate::Eq(Primitive::String(smol_str::SmolStr::new("bad"))),
                    }),
                ],
            },
        })];
        let mut plan = LogicalPlan { steps };
        let res = extract_end_vertex_filter(&mut plan);
        assert!(res.is_err(), "non-integer id should return error");
    }

    #[test]
    fn test_where_with_extra_steps_full_extraction() {
        // where(otherV().hasId(1).hasLabel("2")) — all filters extracted into one EndVertexFilter.
        use crate::planner::logical_step::HasLabelStep;
        let steps = vec![LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
                    LogicalStep::HasLabel(HasLabelStep {
                        pred: PrimitivePredicate::Eq(Primitive::String(smol_str::SmolStr::new("2"))),
                    }),
                ],
            },
        })];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed — all filters extracted");
        assert_eq!(plan.steps.len(), 1);
        assert!(matches!(&plan.steps[0], LogicalStep::EndVertexFilter(_)));
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[0] {
            assert_eq!(evf.ids.as_deref().unwrap(), &[1]);
            assert_eq!(evf.label_preds.len(), 1);
        }
    }

    // where(otherV().hasLabel("a").hasLabel("b")) — a second hasLabel() in the same chain must
    // accumulate (ANDed), not be silently dropped or block extraction of the rest of the chain.
    #[test]
    fn test_where_second_haslabel_in_same_chain_both_accumulate() {
        use crate::planner::logical_step::HasLabelStep;
        let steps = vec![LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasLabel(HasLabelStep {
                        pred: PrimitivePredicate::Eq(Primitive::String(smol_str::SmolStr::new("a"))),
                    }),
                    LogicalStep::HasLabel(HasLabelStep {
                        pred: PrimitivePredicate::Eq(Primitive::String(smol_str::SmolStr::new("b"))),
                    }),
                ],
            },
        })];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed — both hasLabel() predicates extract");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[0] {
            assert_eq!(evf.label_preds.len(), 2);
        } else {
            panic!("expected EndVertexFilter");
        }
    }

    // where(otherV().hasId(1).hasLabel("a").hasLabel("b")) — the leading id filter and both
    // label predicates all extract together into the same EndVertexFilter.
    #[test]
    fn test_where_id_and_two_haslabel_all_extracted() {
        use crate::planner::logical_step::HasLabelStep;
        let steps = vec![LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
                    LogicalStep::HasLabel(HasLabelStep {
                        pred: PrimitivePredicate::Eq(Primitive::String(smol_str::SmolStr::new("a"))),
                    }),
                    LogicalStep::HasLabel(HasLabelStep {
                        pred: PrimitivePredicate::Eq(Primitive::String(smol_str::SmolStr::new("b"))),
                    }),
                ],
            },
        })];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt, "plan should be changed — id and both labels all extract");
        assert_eq!(plan.steps.len(), 1);
        if let LogicalStep::EndVertexFilter(evf) = &plan.steps[0] {
            assert_eq!(evf.ids.as_deref().unwrap(), &[1]);
            assert_eq!(evf.label_preds.len(), 2);
        } else {
            panic!("expected EndVertexFilter");
        }
    }

    #[test]
    fn test_multiple_where_steps_all_extracted() {
        let steps = vec![whr_all(), whr_has_pro()];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(opt);
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(plan.steps[0], LogicalStep::EndVertexFilter(_)));
        assert!(matches!(plan.steps[1], LogicalStep::EndVertexFilter(_)));
    }
}
