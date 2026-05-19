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
//! `tail` [`ConsumerIter`]. Callers inject traversers via [`PhysicalPlan::inject`]
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
            traits::{ConsumerIter, GremlinStep, Step},
            vec_source::VecSourceStep,
        },
    },
    planner::logical_step::{LogicalPlan, LogicalStep},
};

#[derive(Clone)]
pub struct PhysicalPlan {
    pub source: Rc<VecSourceStep>,
    pub tail: ConsumerIter,
}

impl PhysicalPlan {
    pub fn inject(&self, items: VecDeque<Traverser>) {
        self.source.inject(items);
    }

    pub fn next(&self, ctx: &mut dyn GraphCtx) -> Option<Traverser> {
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
        let source = VecSourceStep::empty();
        let mut upstream = Some(Step::subscribe(&source));

        if plan.steps.is_empty() {
            return PhysicalPlan { source: source.clone(), tail: Step::subscribe(&source) };
        }

        for step in &plan.steps {
            upstream = self.build_step(step, upstream);
        }

        PhysicalPlan { source, tail: upstream.expect("Plan must have at least the source step") }
    }

    fn build_step(&mut self, step: &LogicalStep, upstream: Option<ConsumerIter>) -> Option<ConsumerIter> {
        use crate::engine::volcano::steps;

        match step {
            LogicalStep::Both(s) => {
                let phys = steps::both::BothStep::new(s.label_ids.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::BothE(s) => {
                let phys = steps::both_e::BothEStep::new(s.label_ids.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::V(s) => {
                let phys = steps::v::VStep::new(s.ids.clone());
                Some(Step::subscribe(&phys))
            }
            LogicalStep::Count(_) => {
                let phys = steps::count::CountStep::new();
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::HasLabel(s) => {
                let phys = steps::has_label::HasLabelStep::new(s.label_ids.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::HasProperty(s) => {
                let phys = steps::has_property::HasPropertyStep::new(s.key.clone(), s.value.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::In(s) => {
                let phys = steps::r#in::InStep::new(s.label_ids.clone());
                match upstream {
                    Some(up) => phys.add_upper(up),
                    None => panic!("InStep must have an upstream."),
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::InE(s) => {
                let phys = steps::in_e::InEStep::new(s.label_ids.clone());
                match upstream {
                    Some(up) => phys.add_upper(up),
                    None => panic!("InEStep must have an upstream."),
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::Out(s) => {
                let phys = steps::out::OutStep::new(s.label_ids.clone());
                match upstream {
                    Some(up) => phys.add_upper(up),
                    None => panic!("OutStep must have an upstream."),
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::OutE(s) => {
                let phys = steps::out_e::OutEStep::new(s.label_ids.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::InV(_) => {
                let phys = steps::in_v::InVStep::new();
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::OtherV(_) => {
                let phys = steps::other_v::OtherVStep::new();
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::OutV(_) => {
                let phys = steps::out_v::OutVStep::new();
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::ScalarFilter(s) => {
                let phys = steps::scalar_filter::ScalarFilterStep::new(s.value.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::Values(s) => {
                let phys = steps::values::ValuesStep::new(s.property_keys.clone());
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::Where(s) => {
                let physical_plan = self.build(&s.plan);
                let phys = steps::where_step::WhereStep::new(physical_plan);
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::Union(s) => {
                let physical_plans = s.plans.iter().map(|p| self.build(p)).collect();
                let phys = steps::union::UnionStep::new(physical_plans);
                if let Some(up) = upstream {
                    phys.add_upper(up);
                }
                Some(Step::subscribe(&phys))
            }
            LogicalStep::AddV(s) => {
                let phys = steps::add_v::AddVStep::new(s.label_id, s.vertex_id, s.properties.clone());
                Some(Step::subscribe(&phys))
            }
            LogicalStep::AddE(s) => {
                let phys = steps::add_e::AddEStep::new(s.label_id, s.out_v_id, s.in_v_id, s.properties.clone());
                Some(Step::subscribe(&phys))
            }
            LogicalStep::Property(s) => {
                let phys = steps::property::PropertyStep::new(s.prop_key.clone(), s.prop_value.clone());
                match upstream {
                    Some(up) => phys.add_upper(up),
                    None => panic!("PropertyStep must have an upstream."),
                }
                Some(Step::subscribe(&phys))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::PhysicalPlanBuilder;
    use crate::{
        engine::{context::NoopCtx, traverser::Traverser},
        planner::logical_step::{CountStep, LogicalPlan, LogicalStep, ScalarFilterStep, WhereStep},
        types::gvalue::{GValue, Primitive},
    };

    fn gvalue(value: i32) -> GValue {
        GValue::Scalar(Primitive::Int32(value))
    }

    fn traverser(value: i32) -> Traverser {
        Traverser::new(gvalue(value))
    }

    #[test]
    fn test_simple_filter_plan() {
        let plan =
            LogicalPlan { steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep { value: Primitive::Int32(2) })] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan);

        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2), traverser(3)]));

        let mut ctx = NoopCtx;
        let result = physical_plan.next(&mut ctx).expect("Expected one result");
        assert_eq!(result.value, gvalue(2));
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
        assert_eq!(result1.value, gvalue(3));
        assert!(physical_plan.next(&mut ctx).is_none());

        physical_plan.reset();
        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2)]));
        let result2 = physical_plan.next(&mut ctx).unwrap();
        assert_eq!(result2.value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).is_none());
    }

    #[test]
    fn test_where_step_plan() {
        let sub_plan =
            LogicalPlan { steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep { value: Primitive::Int32(2) })] };
        let plan = LogicalPlan { steps: vec![LogicalStep::Where(WhereStep { plan: sub_plan })] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let physical_plan = builder.build(&plan);

        physical_plan.inject(VecDeque::from(vec![traverser(1), traverser(2), traverser(3)]));

        let mut ctx = NoopCtx;
        let result = physical_plan.next(&mut ctx).expect("Expected one result");
        assert_eq!(result.value, gvalue(2));
        assert!(physical_plan.next(&mut ctx).is_none());
    }
}
