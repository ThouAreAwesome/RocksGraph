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

use std::{collections::VecDeque, rc::Rc};

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
};

#[derive(Clone)]
pub struct PhysicalPlan {
    pub source: Rc<BufferedStep<VecSourceStep>>,
    pub tail: StepRef,
}

impl PhysicalPlan {
    pub fn inject(&self, items: VecDeque<Rc<Traverser>>) {
        self.source.inner.borrow_mut().core.inject(items);
    }

    pub fn next(&self, ctx: &mut dyn GraphCtx) -> Option<Rc<Traverser>> {
        self.tail.next(ctx)
    }

    pub fn reset(&self) {
        self.tail.reset();
    }
}

#[derive(Default)]
pub struct PhysicalPlanBuilder;

impl PhysicalPlanBuilder {
    pub fn build(&mut self, plan: &LogicalPlan) -> PhysicalPlan {
        let source = BufferedStep::new(VecSourceStep::empty());

        if plan.steps.is_empty() {
            let tail: StepRef = source.clone();
            return PhysicalPlan { source, tail };
        }

        let mut upstream: Option<StepRef> = Some(source.clone());
        for step in &plan.steps {
            upstream = self.build_step(step, upstream);
        }

        PhysicalPlan { source, tail: upstream.expect("plan must have at least one step") }
    }

    fn build_step(&mut self, step: &LogicalStep, upstream: Option<StepRef>) -> Option<StepRef> {
        use crate::engine::volcano::steps;

        macro_rules! wire {
            ($phys:expr, $up:expr) => {{
                let phys = $phys;
                if let Some(up) = $up {
                    phys.add_upper(up);
                }
                Some(phys as StepRef)
            }};
        }
        macro_rules! wire_required {
            ($phys:expr, $up:expr, $name:literal) => {{
                let phys = $phys;
                match $up {
                    Some(up) => phys.add_upper(up),
                    None => panic!(concat!($name, " must have an upstream")),
                }
                Some(phys as StepRef)
            }};
        }

        match step {
            LogicalStep::Both(s) => {
                wire!(BufferedStep::new(steps::both::BothStep::new(s.label_ids.clone())), upstream)
            }
            LogicalStep::BothE(s) => {
                wire!(BufferedStep::new(steps::both_e::BothEStep::new(s.label_ids.clone())), upstream)
            }
            LogicalStep::V(s) => {
                wire!(BufferedStep::new(steps::v::VStep::new(s.ids.clone())), None::<StepRef>)
            }
            LogicalStep::Count(_) => {
                wire_required!(BufferedStep::new(steps::count::CountStep::new()), upstream, "CountStep")
            }
            LogicalStep::HasLabel(s) => {
                wire!(BufferedStep::new(steps::has_label::HasLabelStep::new(s.label_ids.clone())), upstream)
            }
            LogicalStep::HasProperty(s) => wire!(
                BufferedStep::new(steps::has_property::HasPropertyStep::new(s.key.clone(), s.value.clone())),
                upstream
            ),
            LogicalStep::In(s) => {
                wire_required!(BufferedStep::new(steps::r#in::InStep::new(s.label_ids.clone())), upstream, "InStep")
            }
            LogicalStep::InE(s) => {
                wire_required!(BufferedStep::new(steps::in_e::InEStep::new(s.label_ids.clone())), upstream, "InEStep")
            }
            LogicalStep::Out(s) => {
                wire_required!(BufferedStep::new(steps::out::OutStep::new(s.label_ids.clone())), upstream, "OutStep")
            }
            LogicalStep::OutE(s) => {
                wire!(BufferedStep::new(steps::out_e::OutEStep::new(s.label_ids.clone())), upstream)
            }
            LogicalStep::InV(_) => {
                wire!(BufferedStep::new(steps::in_v::InVStep::new()), upstream)
            }
            LogicalStep::OtherV(_) => {
                wire!(BufferedStep::new(steps::other_v::OtherVStep::new()), upstream)
            }
            LogicalStep::OutV(_) => {
                wire!(BufferedStep::new(steps::out_v::OutVStep::new()), upstream)
            }
            LogicalStep::ScalarFilter(s) => {
                wire!(BufferedStep::new(steps::scalar_filter::ScalarFilterStep::new(s.value.clone())), upstream)
            }
            LogicalStep::Values(s) => {
                wire!(BufferedStep::new(steps::values::ValuesStep::new(s.property_keys.clone())), upstream)
            }
            LogicalStep::Where(s) => {
                let physical_plan = self.build(&s.plan);
                wire!(BufferedStep::new(steps::r#where::WhereStep::new(physical_plan)), upstream)
            }
            LogicalStep::Union(s) => {
                let physical_plans = s.plans.iter().map(|p| self.build(p)).collect();
                wire!(BufferedStep::new(steps::union::UnionStep::new(physical_plans)), upstream)
            }
            LogicalStep::AddV(s) => {
                wire!(
                    BufferedStep::new(steps::add_v::AddVStep::new(s.label_id, s.vertex_id, s.properties.clone())),
                    None::<StepRef>
                )
            }
            LogicalStep::AddE(s) => {
                wire!(
                    BufferedStep::new(steps::add_e::AddEStep::new(s.label_id, s.out_v_id, s.in_v_id, s.properties.clone())),
                    None::<StepRef>
                )
            }
            LogicalStep::Property(s) => wire_required!(
                BufferedStep::new(steps::property::PropertyStep::new(s.prop_key.clone(), s.prop_value.clone())),
                upstream,
                "PropertyStep"
            ),
            LogicalStep::Limit(s) => {
                wire!(BufferedStep::new(steps::limit::LimitStep::new(s.limit)), upstream)
            }
            LogicalStep::HasId(s) => {
                wire!(BufferedStep::new(steps::has_id::HasIdStep::new(s.ids.clone())), upstream)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

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
        let physical_plan = builder.build(&plan);

        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2), traverser(3)]));

        let mut ctx = NoopCtx;
        let result = physical_plan.next(&mut ctx).expect("Expected one result");
        assert_eq!(result.as_ref().value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).is_none());
    }

    #[test]
    fn test_plan_reuse_with_reset() {
        let plan = LogicalPlan { steps: vec![LogicalStep::Count(CountStep {})] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan);

        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2), traverser(3)]));
        let mut ctx = NoopCtx;
        let result1 = physical_plan.next(&mut ctx).unwrap();
        assert_eq!(result1.as_ref().value, gvalue(3));
        assert!(physical_plan.next(&mut ctx).is_none());

        physical_plan.reset();
        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2)]));
        let result2 = physical_plan.next(&mut ctx).unwrap();
        assert_eq!(result2.as_ref().value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).is_none());
    }

    #[test]
    fn test_where_step_plan() {
        let sub_plan =
            LogicalPlan { steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep { value: Primitive::Int64(2) })] };
        let plan = LogicalPlan { steps: vec![LogicalStep::Where(WhereStep { plan: sub_plan })] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan);

        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2), traverser(3)]));

        let mut ctx = NoopCtx;
        let result = physical_plan.next(&mut ctx).expect("Expected one result");
        assert_eq!(result.as_ref().value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).is_none());
    }
}
