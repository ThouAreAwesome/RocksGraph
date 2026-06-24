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
    planner::logical_step::{EndVertexFilter, LogicalPlan, LogicalStep},
    types::{prop_key::ID, StoreError},
};

/// An optimizer rule that extracts `where(__.otherV().hasId(…))` or `where(__.otherV().has("id", …))`
/// patterns and replaces them with an `EndVertexFilter` step.
///
/// This allows subsequent optimization rules to merge the `EndVertexFilter` directly into edge traversal steps.
pub fn extract_end_vertex_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    for step in plan.steps.iter_mut() {
        if let LogicalStep::Where(wh) = step {
            match wh.plan.steps.as_slice() {
                [LogicalStep::OtherV(_), LogicalStep::HasId(hi)] => {
                    if let Some(ids) = super::extract_ids_from_predicate(&hi.pred)? {
                        *step = LogicalStep::EndVertexFilter(EndVertexFilter { ids });
                        changed = true;
                    }
                }
                [LogicalStep::OtherV(_), LogicalStep::HasProperty(hp)] if hp.key.as_str() == ID => {
                    if let Some(ids) = super::extract_ids_from_predicate(&hp.pred)? {
                        *step = LogicalStep::EndVertexFilter(EndVertexFilter { ids });
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }
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
            assert_eq!(&evf.ids[..], &[1, 2, 3]);
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
            assert_eq!(&evf.ids[..], &[1, 2, 3]);
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
            assert_eq!(&evf.ids[..], &[123]);
        } else {
            panic!("expected EndVertexFilter");
        }
    }

    #[test]
    fn test_v_where_other_v_has_unextracted() {
        let steps = vec![v_ids(vec![1, 2]), whr_has()];
        let mut plan = LogicalPlan { steps };
        let opt = extract_end_vertex_filter(&mut plan).unwrap();
        assert!(!opt, "plan should not be changed");
        assert_eq!(plan.steps.len(), 2);
        assert!(matches!(&plan.steps[1], LogicalStep::Where(_)), "plan should not be changed")
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
            assert_eq!(&evf.ids[..], &[999i64]);
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
    fn test_where_with_extra_steps_not_extracted() {
        // where(otherV().hasId(1).hasLabel(2)) — 3-step sub-plan, should not match
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
        assert!(!opt, "plan should not be changed");
        assert!(matches!(&plan.steps[0], LogicalStep::Where(_)));
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
