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

use crate::{
    planner::logical_step::{EndVertexFilter, HasIdStep, HasPropertyStep, LogicalPlan, LogicalStep},
    types::{prop_key::ID, Primitive, StoreError},
};

pub(super) fn extract_end_vertex_filter(plan: &mut LogicalPlan) -> Result<bool, StoreError> {
    let mut changed = false;
    for step in plan.steps.iter_mut() {
        if let LogicalStep::Where(wh) = step {
            match wh.plan.steps.as_slice() {
                [LogicalStep::OtherV(_), LogicalStep::HasId(HasIdStep { ids })] => {
                    *step = LogicalStep::EndVertexFilter(EndVertexFilter { ids: ids.to_vec() });
                    changed = true;
                }
                [LogicalStep::OtherV(_), LogicalStep::HasProperty(HasPropertyStep { key, value })]
                    if key.as_str() == ID =>
                {
                    match *value {
                        Primitive::Int64(vl) => {
                            *step = LogicalStep::EndVertexFilter(EndVertexFilter { ids: vec![vl] });
                            changed = true;
                        }
                        Primitive::Int32(vl) => {
                            *step = LogicalStep::EndVertexFilter(EndVertexFilter { ids: vec![vl as i64] });
                            changed = true;
                        }
                        _ => return Err(StoreError::UnexpectedDataType("expect i32 or i64 type for vertex id".into())),
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
        planner::logical_step::{OtherVStep, VStep, WhereStep},
        types::keys::VertexKey,
    };

    fn v_ids(ids: Vec<VertexKey>) -> LogicalStep {
        LogicalStep::V(VStep { ids })
    }

    fn whr_all() -> LogicalStep {
        LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![LogicalStep::OtherV(OtherVStep {}), LogicalStep::HasId(HasIdStep { ids: vec![1, 2, 3] })],
            },
        })
    }

    fn whr_has_pro() -> LogicalStep {
        LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep { key: ID, value: Primitive::Int32(123) }),
                ],
            },
        })
    }

    fn whr_has() -> LogicalStep {
        LogicalStep::Where(WhereStep {
            plan: LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep { key: "name".into(), value: Primitive::Int32(123) }),
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
            assert_eq!(evf.ids, vec![1, 2, 3]);
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
            assert_eq!(evf.ids, vec![1, 2, 3]);
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
            assert_eq!(evf.ids, vec![123]);
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
}
