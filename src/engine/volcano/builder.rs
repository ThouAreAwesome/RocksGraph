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

//! Compiles a [`LogicalPlan`] into an executable [`PhysicalPlan`] for the
//! volcano engine.
//!
//! [`PhysicalPlanBuilder::build`] walks the logical steps in order and calls
//! [`build_step`] for each one. `build_step` owns the only place in the codebase
//! that maps logical step variants to volcano physical operators — keeping
//! [`planner::logical_step`] free of any engine-specific imports.
//!
//! A [`PhysicalPlan`] is a [`VecSourceStep`] (the injection point) wired to a
//! `tail` [`StepRef`]. Callers inject traversers via [`PhysicalPlan::inject`]
//! and pull results one at a time with [`PhysicalPlan::next`].
//!
//! [`LogicalPlan`]: crate::planner::logical_step::LogicalPlan
//! [`planner::logical_step`]: crate::planner::logical_step
//! [`build_step`]: PhysicalPlanBuilder::build_step
//! [`VecSourceStep`]: crate::engine::volcano::steps::vec_source::VecSourceStep

use std::{fmt, rc::Rc};

use smallvec::SmallVec;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::{
            traits::{BufferedStep, GremlinStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
    planner::logical_step::{LogicalPlan, LogicalStep},
    types::{error::StoreError, Direction},
};

#[derive(Clone)]
pub struct PhysicalPlan {
    pub source: Rc<BufferedStep<VecSourceStep>>,
    pub tail: StepRef,
}

impl fmt::Debug for PhysicalPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut chain = Vec::new();
        let mut current = Some(self.tail.clone());

        while let Some(step) = current {
            // Once GremlinStep: Debug is added, we can format the step chain.
            chain.push(format!("{:?}", step));
            // Traverse upstream towards the source
            current = step.upper();
        }
        // Volcano is a pull-based engine (tail is the root); reverse to show Source -> Result flow.
        chain.reverse();

        write!(f, "PhysicalPlan({})", chain.join(" -> "))
    }
}

impl PhysicalPlan {
    pub fn inject(&self, items: SmallVec<[Rc<Traverser>; 4]>) {
        self.source.inner.borrow_mut().core.inject(items);
    }

    pub fn next(&self, ctx: &mut dyn GraphCtx) -> Result<Option<Rc<Traverser>>, StoreError> {
        self.tail.next(ctx)
    }

    pub fn reset(&self) {
        self.tail.reset();
    }
}

#[derive(Default)]
pub struct PhysicalPlanBuilder;

impl PhysicalPlanBuilder {
    pub fn build(&mut self, plan: &LogicalPlan) -> Result<PhysicalPlan, StoreError> {
        let source = BufferedStep::new(VecSourceStep::empty());

        if plan.steps.is_empty() {
            let tail: StepRef = source.clone();
            return Ok(PhysicalPlan { source, tail });
        }

        let mut upstream: Option<StepRef> = Some(source.clone());
        for step in &plan.steps {
            upstream = self.build_step(step, upstream)?;
        }

        Ok(PhysicalPlan { source, tail: upstream.expect("plan must have at least one step") })
    }

    fn build_step(&mut self, step: &LogicalStep, upstream: Option<StepRef>) -> Result<Option<StepRef>, StoreError> {
        use crate::engine::volcano::steps;

        macro_rules! wire {
            ($phys:expr, $up:expr) => {{
                let phys = $phys;
                if let Some(up) = $up {
                    phys.add_upper(up);
                }
                Ok(Some(phys as StepRef))
            }};
        }
        macro_rules! wire_required {
            ($phys:expr, $up:expr, $name:literal) => {{
                let phys = $phys;
                match $up {
                    Some(up) => phys.add_upper(up),
                    None => {
                        return Err(StoreError::RuntimeError(format!("{} must have an upstream", $name)));
                    }
                }
                Ok(Some(phys as StepRef))
            }};
        }

        match step {
            LogicalStep::Both(s) => {
                wire_required!(
                    BufferedStep::new(steps::both::BothStep::new(s.label_ids.clone(), s.end_vertex_ids.clone())),
                    upstream,
                    "BothStep"
                )
            }
            LogicalStep::BothE(s) => {
                wire_required!(
                    BufferedStep::new(steps::both_e::BothEStep::new(s.label_ids.clone(), s.end_vertex_ids.clone())),
                    upstream,
                    "BothEStep"
                )
            }
            LogicalStep::V(s) => {
                if s.ids.is_empty() {
                    return Err(StoreError::RuntimeError(
                        "VStep cannot be built with empty IDs. A `g.V()` traversal must be followed by an optimizable \
                         filter like `hasId()` or `has('id', ...)` that the optimizer can fold into the V-step."
                            .to_string(),
                    ));
                }
                wire!(BufferedStep::new(steps::v::VStep::new(s.ids.clone())), None::<StepRef>)
            }
            LogicalStep::Count(_) => {
                wire_required!(BufferedStep::new(steps::count::CountStep::default()), upstream, "CountStep")
            }
            LogicalStep::HasLabel(s) => {
                wire_required!(
                    BufferedStep::new(steps::has_label::HasLabelStep::new(s.label_ids.clone())),
                    upstream,
                    "HasLabelStep"
                )
            }
            LogicalStep::HasProperty(s) => wire_required!(
                BufferedStep::new(steps::has_property::HasPropertyStep::new(s.key.clone(), s.value.clone())),
                upstream,
                "HasPropertyStep"
            ),
            LogicalStep::In(s) => {
                wire_required!(
                    BufferedStep::new(steps::in_out::InOutStep::new(
                        s.label_ids.clone(),
                        Direction::IN,
                        s.end_vertex_ids.clone()
                    )),
                    upstream,
                    "InStep"
                )
            }
            LogicalStep::InE(s) => {
                if let Some(end_ids) = &s.end_vertex_ids {
                    if !s.label_ids.is_empty() && !end_ids.is_empty() {
                        return wire_required!(
                            BufferedStep::new(steps::get_e::GetEStep::new(
                                s.label_ids.clone(),
                                end_ids.clone(),
                                Direction::IN
                            )),
                            upstream,
                            "GetOutEStep"
                        );
                    }
                }
                wire_required!(
                    BufferedStep::new(steps::in_e_out_e::InEOutEStep::new(
                        s.label_ids.clone(),
                        Direction::IN,
                        s.end_vertex_ids.clone()
                    )),
                    upstream,
                    "InEStep"
                )
            }
            LogicalStep::Out(s) => {
                wire_required!(
                    BufferedStep::new(steps::in_out::InOutStep::new(
                        s.label_ids.clone(),
                        Direction::OUT,
                        s.end_vertex_ids.clone()
                    )),
                    upstream,
                    "OutStep"
                )
            }
            LogicalStep::OutE(s) => {
                if let Some(end_ids) = &s.end_vertex_ids {
                    if !s.label_ids.is_empty() && !end_ids.is_empty() {
                        return wire_required!(
                            BufferedStep::new(steps::get_e::GetEStep::new(
                                s.label_ids.clone(),
                                end_ids.clone(),
                                Direction::OUT
                            )),
                            upstream,
                            "GetOutEStep"
                        );
                    }
                }
                wire_required!(
                    BufferedStep::new(steps::in_e_out_e::InEOutEStep::new(
                        s.label_ids.clone(),
                        Direction::OUT,
                        s.end_vertex_ids.clone()
                    )),
                    upstream,
                    "OutEStep"
                )
            }
            LogicalStep::InV(_) => {
                wire_required!(
                    BufferedStep::new(steps::in_v_out_v::InVOutVStep::new(Direction::IN)),
                    upstream,
                    "InVStep"
                )
            }
            LogicalStep::OtherV(_) => {
                wire_required!(BufferedStep::new(steps::other_v::OtherVStep::default()), upstream, "OtherVStep")
            }
            LogicalStep::OutV(_) => {
                wire_required!(
                    BufferedStep::new(steps::in_v_out_v::InVOutVStep::new(Direction::OUT)),
                    upstream,
                    "OutVStep"
                )
            }
            LogicalStep::ScalarFilter(s) => {
                wire_required!(
                    BufferedStep::new(steps::scalar_filter::ScalarFilterStep::new(s.value.clone())),
                    upstream,
                    "ScalarFilterStep"
                )
            }
            LogicalStep::Values(s) => {
                wire_required!(
                    BufferedStep::new(steps::values::ValuesStep::new(s.property_keys.clone(), false)),
                    upstream,
                    "ValuesStep"
                )
            }
            LogicalStep::Properties(s) => {
                wire_required!(
                    BufferedStep::new(steps::values::ValuesStep::new(s.property_keys.clone(), true)),
                    upstream,
                    "ValuesStep"
                )
            }
            LogicalStep::Where(s) => {
                if s.plan.steps.is_empty() {
                    return Err(StoreError::RuntimeError("WhereStep must have a non-empty sub-plan.".to_string()));
                }
                let physical_plan = self.build(&s.plan)?;
                wire_required!(BufferedStep::new(steps::r#where::WhereStep::new(physical_plan)), upstream, "WhereStep")
            }
            LogicalStep::Union(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::RuntimeError(
                        "UnionStep must have at least one child traversal.".to_string(),
                    ));
                }
                let physical_plans = s.plans.iter().map(|p| self.build(p)).collect::<Result<_, _>>()?;
                wire_required!(BufferedStep::new(steps::union::UnionStep::new(physical_plans)), upstream, "UnionStep")
            }
            LogicalStep::AddV(s) => {
                let Some(vertex_id) = s.vertex_id else {
                    return Err(StoreError::RuntimeError(
                        "AddVStep cannot be built without a vertex ID. A preceding `property('id', ...)` step is required \
                         and should have been folded by the optimizer."
                            .to_string(),
                    ));
                };
                wire!(
                    BufferedStep::new(steps::add_v::AddVStep::new(s.label_id, vertex_id, s.properties.clone())),
                    None::<StepRef>
                )
            }
            LogicalStep::AddE(s) => {
                let Some(out_v_id) = s.out_v_id else {
                    return Err(StoreError::RuntimeError(
                        "AddEStep cannot be built without an out-vertex ID. A preceding `from(...)` step is required \
                         and should have been folded by the optimizer."
                            .to_string(),
                    ));
                };
                let Some(in_v_id) = s.in_v_id else {
                    return Err(StoreError::RuntimeError(
                        "AddEStep cannot be built without an in-vertex ID. A preceding `to(...)` step is required \
                         and should have been folded by the optimizer."
                            .to_string(),
                    ));
                };
                wire!(
                    BufferedStep::new(steps::add_e::AddEStep::new(s.label_id, out_v_id, in_v_id, s.properties.clone())),
                    None::<StepRef>
                )
            }
            LogicalStep::Property(s) => wire_required!(
                BufferedStep::new(steps::property::PropertyStep::new(s.prop_key.clone(), s.prop_value.clone())),
                upstream,
                "PropertyStep"
            ),
            LogicalStep::Limit(s) => {
                wire_required!(BufferedStep::new(steps::limit::LimitStep::new(s.limit)), upstream, "LimitStep")
            }
            LogicalStep::HasId(s) => {
                wire_required!(BufferedStep::new(steps::has_id::HasIdStep::new(s.ids.clone())), upstream, "HasIdStep")
            }
            LogicalStep::Coalesce(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::RuntimeError(
                        "CoalesceStep must have at least one child traversal.".to_string(),
                    ));
                }
                let physical_plans = s.plans.iter().map(|p| self.build(p)).collect::<Result<_, _>>()?;
                wire_required!(
                    BufferedStep::new(steps::coalesce::CoalesceStep::new(physical_plans)),
                    upstream,
                    "CoalesceStep"
                )
            }
            LogicalStep::EndVertexFilter(s) => {
                wire_required!(
                    BufferedStep::new(steps::end_vertex_filter::EndVertexFilter::new(s.ids.clone())),
                    upstream,
                    "EndVertexFilterStep"
                )
            }
            LogicalStep::Drop(_) => {
                wire_required!(BufferedStep::new(steps::drop::DropStep::default()), upstream, "DropStep")
            }
            _ => unreachable!("unreachable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use smallvec::smallvec;
    use std::rc::Rc;

    use super::PhysicalPlanBuilder;
    use crate::{
        engine::{context::NoopCtx, traverser::Traverser},
        planner::logical_step::{CountStep, LogicalPlan, LogicalStep, ScalarFilterStep, WhereStep},
        types::gvalue::{GValue, Primitive},
    };

    fn gvalue(value: i64) -> GValue {
        GValue::Scalar(Primitive::Int64(value))
    }

    fn traverser(value: i64) -> Rc<Traverser> {
        Traverser::new_rc(gvalue(value))
    }

    #[test]
    fn test_simple_filter_plan() {
        let plan =
            LogicalPlan { steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep { value: Primitive::Int64(2) })] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan).unwrap();

        physical_plan.inject(smallvec![traverser(1), traverser(2), traverser(3)]);

        let mut ctx = NoopCtx;
        let result = physical_plan.next(&mut ctx).expect("store error").expect("Expected one result");
        assert_eq!(result.as_ref().value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).expect("store error").is_none());
    }

    #[test]
    fn test_plan_reuse_with_reset() {
        let plan = LogicalPlan { steps: vec![LogicalStep::Count(CountStep {})] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan).unwrap();

        physical_plan.inject(smallvec![traverser(1), traverser(2), traverser(3)]);
        let mut ctx = NoopCtx;
        let result1 = physical_plan.next(&mut ctx).unwrap().unwrap();
        assert_eq!(result1.as_ref().value, gvalue(3));
        assert!(physical_plan.next(&mut ctx).unwrap().is_none());

        physical_plan.reset();
        physical_plan.inject(smallvec![traverser(1), traverser(2)]);
        let result2 = physical_plan.next(&mut ctx).unwrap().unwrap();
        assert_eq!(result2.as_ref().value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).unwrap().is_none());
    }

    #[test]
    fn test_where_step_plan() {
        let sub_plan =
            LogicalPlan { steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep { value: Primitive::Int64(2) })] };
        let plan = LogicalPlan { steps: vec![LogicalStep::Where(WhereStep { plan: sub_plan })] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan).unwrap();

        physical_plan.inject(smallvec![traverser(1), traverser(2), traverser(3)]);

        let mut ctx = NoopCtx;
        let result = physical_plan.next(&mut ctx).expect("store error").expect("Expected one result");
        assert_eq!(result.as_ref().value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).expect("store error").is_none());
    }

    #[cfg(test)]
    mod debug_print {
        use super::*;
        use crate::{
            planner::{
                apply_rules,
                logical_step::{
                    CoalesceStep, CountStep, HasIdStep, HasPropertyStep, InEStep, InStep, LogicalPlan, LogicalStep,
                    OtherVStep, OutEStep, OutStep, PropertiesStep, UnionStep, VStep, WhereStep,
                },
            },
            types::{gvalue::Primitive, prop_key::ID},
        };

        fn assert_plan_contains_in_order(steps: Vec<LogicalStep>, expected_step_names: &[&str]) {
            let mut plan = LogicalPlan { steps };
            apply_rules(&mut plan).expect("Optimizer rules failed");
            let mut builder: PhysicalPlanBuilder = Default::default();
            let physical_plan = builder.build(&plan).unwrap();
            let debug_str = format!("{:?}", physical_plan);

            let mut last_pos = 0;
            for step_name in expected_step_names {
                if let Some(pos) = debug_str[last_pos..].find(step_name) {
                    last_pos += pos + step_name.len(); // Start next search after this one
                } else {
                    panic!("Did not find '{}' in order in plan string: {}", step_name, debug_str);
                }
            }
        }

        #[test]
        fn test_print_v_hasid_properties() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Properties(PropertiesStep { property_keys: smallvec![] }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "ValuesStep"]);
        }

        #[test]
        fn test_print_v_hasid_out_properties() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Out(OutStep { label_ids: smallvec![], end_vertex_ids: None }),
                LogicalStep::Properties(PropertiesStep { property_keys: smallvec![] }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InOutStep", "ValuesStep"]);
        }

        #[test]
        fn test_print_v_hasid_oute_count() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { label_ids: smallvec![], end_vertex_ids: None }),
                LogicalStep::Count(CountStep {}),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InEOutEStep", "CountStep"]);
        }

        #[test]
        fn test_print_v_hasid_oute_where_otherv_hasid() {
            let where_plan = LogicalPlan {
                steps: vec![LogicalStep::OtherV(OtherVStep {}), LogicalStep::HasId(HasIdStep { ids: smallvec![2] })],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { label_ids: smallvec![], end_vertex_ids: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InEOutEStep"]);
        }

        #[test]
        fn test_print_v_hasid_oute_label_where_otherv_hasid() {
            let where_plan = LogicalPlan {
                steps: vec![LogicalStep::OtherV(OtherVStep {}), LogicalStep::HasId(HasIdStep { ids: smallvec![2] })],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { label_ids: smallvec![123], end_vertex_ids: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "GetEStep"]);
        }

        #[test]
        fn test_print_v_hasid_ine_label_where_otherv_hasid() {
            let where_plan = LogicalPlan {
                steps: vec![LogicalStep::OtherV(OtherVStep {}), LogicalStep::HasId(HasIdStep { ids: smallvec![2] })],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::InE(InEStep { label_ids: smallvec![456], end_vertex_ids: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "GetEStep"]);
        }

        #[test]
        fn test_print_v_hasprop_id_ine_otherv_hasid() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasProperty(HasPropertyStep { key: ID, value: Primitive::Int64(1) }),
                LogicalStep::InE(InEStep { label_ids: smallvec![], end_vertex_ids: None }),
                LogicalStep::OtherV(OtherVStep {}),
                LogicalStep::HasId(HasIdStep { ids: smallvec![2] }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InEOutEStep", "OtherVStep", "HasIdStep"]);
        }

        #[test]
        fn test_print_union_and_coalesce() {
            let out_plan =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { label_ids: smallvec![], end_vertex_ids: None })] };
            let in_plan =
                LogicalPlan { steps: vec![LogicalStep::In(InStep { label_ids: smallvec![], end_vertex_ids: None })] };

            let union_steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Union(UnionStep { plans: smallvec![out_plan.clone(), in_plan.clone()] }),
            ];
            assert_plan_contains_in_order(union_steps, &["VStep", "UnionStep", "InOutStep", "InOutStep"]);

            let coalesce_steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Coalesce(CoalesceStep { plans: vec![out_plan, in_plan] }),
            ];
            assert_plan_contains_in_order(coalesce_steps, &["VStep", "CoalesceStep", "InOutStep", "InOutStep"]);
        }
    }
}
