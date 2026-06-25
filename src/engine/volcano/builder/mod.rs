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

//! Compiles a [`LogicalPlan`] into an executable [`PhysicalPlan`] for the
//! volcano engine.
//!
//! [`PhysicalPlanBuilder::build`] walks the logical steps in order and calls
//! [`build_step`] for each one.  The step-by-step mapping lives in
//! [`build_step`](mod@build_step) — the only place in the codebase that maps
//! logical step variants to volcano physical operators, keeping
//! [`planner::logical_step`] free of any engine-specific imports.
//!
//! A [`PhysicalPlan`] is a [`VecSourceStep`] (the injection point) wired to a
//! `tail` [`StepRef`].  Callers inject traversers via [`PhysicalPlan::inject`]
//! and pull results one at a time with [`PhysicalPlan::next`].
//!
//! [`LogicalPlan`]: crate::planner::logical_step::LogicalPlan
//! [`planner::logical_step`]: crate::planner::logical_step
//! [`VecSourceStep`]: crate::engine::volcano::steps::vec_source::VecSourceStep
//! [`StepRef`]: crate::engine::volcano::steps::traits::StepRef

mod build_step;

use std::{fmt, rc::Rc};

use smallvec::SmallVec;

use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::{
            traits::{BufferedStep, StepRef},
            vec_source::VecSourceStep,
        },
    },
    planner::logical_step::LogicalPlan,
    schema::Schema,
    types::error::StoreError,
};

// ── PhysicalPlan ──────────────────────────────────────────────────────────────

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
            chain.push(format!("{:?}", step));
            current = step.upper();
        }
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

// ── PhysicalPlanBuilder ───────────────────────────────────────────────────────

#[derive(Default)]
pub struct PhysicalPlanBuilder;

impl PhysicalPlanBuilder {
    /// Compiles a top-level [`LogicalPlan`] (or a self-contained sub-plan being compiled in
    /// isolation, e.g. in tests). Computes `track_path` once, comprehensively, from the plan's
    /// own shape — see [`LogicalPlan::has_path_consumer`].
    ///
    /// Internal recursive compilation of nested sub-plans (`where`/`union`/`coalesce`/`repeat`/
    /// etc., in [`build_step`](mod@build_step)) must call [`build_steps`](Self::build_steps)
    /// directly with the *inherited* `track_path` value instead of this method — recomputing it
    /// from just the sub-plan's shape would miss a path consumer that lives outside the
    /// sub-plan (e.g. a `repeat()` body has no `as()`/`select()`/`path()` of its own, but its
    /// output is what a `.path()` after the loop walks).
    pub fn build(
        &mut self,
        plan: &LogicalPlan,
        schema_lock: &std::sync::RwLock<Schema>,
    ) -> Result<PhysicalPlan, StoreError> {
        let track_path = plan.has_path_consumer();
        self.build_steps(plan, schema_lock, track_path)
    }

    fn build_steps(
        &mut self,
        plan: &LogicalPlan,
        schema_lock: &std::sync::RwLock<Schema>,
        track_path: bool,
    ) -> Result<PhysicalPlan, StoreError> {
        let source = BufferedStep::new(VecSourceStep::empty());

        if plan.steps.is_empty() {
            let tail: StepRef = source.clone();
            return Ok(PhysicalPlan { source, tail });
        }

        let mut upstream: Option<StepRef> = Some(source.clone());
        for step in &plan.steps {
            upstream = self.build_step(step, upstream, schema_lock, track_path)?;
        }

        Ok(PhysicalPlan { source, tail: upstream.expect("plan must have at least one step") })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{context::NoopCtx, traverser::Traverser},
        planner::logical_step::{CountStep, LogicalPlan, LogicalStep, ScalarFilterStep, WhereStep},
        schema::Schema,
        types::gvalue::{GValue, Primitive, PrimitivePredicate},
    };
    use smallvec::smallvec;
    use std::rc::Rc;

    fn gvalue(value: i64) -> GValue {
        GValue::Scalar(Primitive::Int64(value))
    }

    fn traverser(value: i64) -> Rc<Traverser> {
        Traverser::new_rc(gvalue(value))
    }

    #[test]
    fn test_simple_filter_plan() {
        let plan = LogicalPlan {
            steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep {
                pred: PrimitivePredicate::Eq(Primitive::Int64(2)),
            })],
        };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let schema_lock = std::sync::RwLock::new(Schema::default());
        let physical_plan = builder.build(&plan, &schema_lock).unwrap();

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
        let schema_lock = std::sync::RwLock::new(Schema::default());
        let physical_plan = builder.build(&plan, &schema_lock).unwrap();

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
        let sub_plan = LogicalPlan {
            steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep {
                pred: PrimitivePredicate::Eq(Primitive::Int64(2)),
            })],
        };
        let plan = LogicalPlan { steps: vec![LogicalStep::Where(WhereStep { plan: sub_plan })] };

        let mut builder: PhysicalPlanBuilder = Default::default();
        let schema_lock = std::sync::RwLock::new(Schema::default());
        let physical_plan = builder.build(&plan, &schema_lock).unwrap();

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
                    CoalesceStep, CountStep, EmitSpec, HasIdStep, HasPropertyStep, InEStep, InStep, LogicalPlan,
                    LogicalStep, OtherVStep, OutEStep, OutStep, PropertiesStep, RepeatStep, UnionStep, VStep,
                    WhereStep,
                },
            },
            types::{
                gvalue::{Primitive, PrimitivePredicate},
                prop_key::ID,
            },
        };

        fn assert_plan_contains_in_order(steps: Vec<LogicalStep>, expected_step_names: &[&str]) {
            let mut plan = LogicalPlan { steps };
            apply_rules(&mut plan).expect("Optimizer rules failed");
            let mut builder: PhysicalPlanBuilder = Default::default();
            let schema_lock = std::sync::RwLock::new(Schema::default());
            let physical_plan = builder.build(&plan, &schema_lock).unwrap();
            let debug_str = format!("{:?}", physical_plan);

            let mut last_pos = 0;
            for step_name in expected_step_names {
                if let Some(pos) = debug_str[last_pos..].find(step_name) {
                    last_pos += pos + step_name.len();
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
                LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::Properties(PropertiesStep { property_keys: smallvec![] }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InOutStep", "ValuesStep"]);
        }

        #[test]
        fn test_print_v_hasid_oute_count() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Count(CountStep {}),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InOutStep", "CountStep"]);
        }

        #[test]
        fn test_print_v_hasid_oute_where_otherv_hasid() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InOutStep"]);
        }

        #[test]
        fn test_print_v_hasid_oute_label_where_otherv_hasid() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "GetEStep"]);
        }

        #[test]
        fn test_print_v_hasid_ine_label_where_otherv_hasid() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::InE(InEStep { labels: smallvec!["456".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "GetEStep"]);
        }

        #[test]
        fn test_print_v_hasprop_id_ine_otherv_hasid() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasProperty(HasPropertyStep {
                    key: ID,
                    pred: PrimitivePredicate::Eq(Primitive::Int64(1)),
                }),
                LogicalStep::InE(InEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::OtherV(OtherVStep {}),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "InOutStep", "OtherVStep", "HasIdStep"]);
        }

        #[test]
        fn test_print_union_and_coalesce() {
            let out_plan =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let in_plan =
                LogicalPlan { steps: vec![LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None })] };

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

        #[test]
        fn test_print_repeat_with_times() {
            let body =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Repeat(RepeatStep { body, until: None, times: Some(3), emit: EmitSpec::Never }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "RepeatStep", "InOutStep"]);
        }
    }
}
