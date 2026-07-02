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

use std::{collections::HashMap, fmt, rc::Rc};

use smol_str::SmolStr;

use crate::types::PIPELINE_PRODUCE_SIZE;
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
    types::{error::StoreError, LabelId},
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
    pub fn inject(&self, items: SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>) {
        self.source.inner.borrow_mut().core.inject(items);
    }

    pub fn next(&self, ctx: &mut dyn GraphCtx) -> Result<Option<Rc<Traverser>>, StoreError> {
        self.tail.next(ctx)
    }

    pub fn reset(&self) {
        self.tail.reset();
    }

    /// Return a structured explain tree for rendering.  Walks the `upper()`
    /// backbone and stops at `self.source` via `Rc::ptr_eq` to exclude the
    /// injection point from the tree.
    pub fn explain(&self) -> crate::engine::volcano::steps::traits::ExplainNode {
        use crate::engine::volcano::steps::traits::ExplainNode;
        let mut nodes = Vec::new();
        let mut current = Some(self.tail.clone());
        while let Some(step) = current {
            if Rc::ptr_eq(&step, &(self.source.clone() as StepRef)) {
                break;
            }
            nodes.push(step.explain());
            current = step.upper();
        }
        nodes.reverse();
        ExplainNode::new("PhysicalPlan").with_children(nodes.into_iter().map(|n| (String::new(), n)).collect())
    }
}

/// Recursively render an [`ExplainNode`] tree into a string with tree-drawing
/// characters and indentation.
pub(crate) fn render_explain(
    node: &crate::engine::volcano::steps::traits::ExplainNode,
    depth: usize,
    prefix: &str,
) -> String {
    let indent = "  ".repeat(depth);
    let params_str = if node.params.is_empty() {
        String::new()
    } else {
        format!("({})", node.params.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join(", "))
    };
    let mut out = format!("{}{}{}{}\n", indent, prefix, node.name, params_str);
    for (label, child) in &node.children {
        let child_prefix = if label.is_empty() { "  └─ ".to_string() } else { format!("    {}: └─ ", label) };
        out.push_str(&render_explain(child, depth + 1, &child_prefix));
    }
    out
}

// ── PhysicalPlanBuilder ───────────────────────────────────────────────────────

#[derive(Default)]
pub struct PhysicalPlanBuilder {
    /// Per-build cache: label name → resolved LabelId.  Avoids repeated
    /// schema-lock acquisitions when the same label name appears across
    /// multiple LogicalSteps in one plan.
    pub(super) label_cache: HashMap<SmolStr, LabelId>,
    /// Per-build cache: property-key name → resolved u16 id.
    pub(super) prop_key_cache: HashMap<SmolStr, u16>,
}

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
            // degree_pushdown rewrites unfiltered OutE([]).Count → DegreeStep+SumStep
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec![], end_vertex_ids: None, rank: None }),
                LogicalStep::Count(CountStep {}),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "DegreeStep", "SumStep"]);
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

    /// Tests verifying that `explain()` correctly reflects optimizer rule
    /// transformations in the physical plan tree.
    mod explain_tests {
        use super::*;
        use crate::planner::{apply_rules, logical_step::*};
        use crate::types::gvalue::{Primitive, PrimitivePredicate};
        use crate::types::prop_key::ID;
        use smallvec::smallvec;

        fn explain_str(steps: Vec<LogicalStep>) -> String {
            let mut plan = LogicalPlan { steps };
            apply_rules(&mut plan).expect("Optimizer rules failed");
            let mut builder: PhysicalPlanBuilder = Default::default();
            let schema_lock = std::sync::RwLock::new(Schema::default());
            let physical_plan = builder.build(&plan, &schema_lock).unwrap();
            crate::engine::volcano::builder::render_explain(&physical_plan.explain(), 0, "")
        }

        #[track_caller]
        fn assert_names_in_order(explain: &str, names: &[&str]) {
            let mut last_pos = 0;
            for name in names {
                if let Some(pos) = explain[last_pos..].find(name) {
                    last_pos += pos + name.len();
                } else {
                    panic!("Did not find '{}' in order in explain output:\n{}", name, explain);
                }
            }
        }

        #[track_caller]
        fn assert_names_absent(explain: &str, names: &[&str]) {
            for name in names {
                if explain.contains(name) {
                    panic!("Found unexpected '{}' in explain output:\n{}", name, explain);
                }
            }
        }

        #[track_caller]
        fn assert_contains(haystack: &str, needle: &str) {
            if !haystack.contains(needle) {
                panic!("Expected '{}' not found in explain output:\n{}", needle, haystack);
            }
        }

        // ── Group 1: V + hasId folding ───────────────────────────────────

        #[test]
        fn v_hasid_folds_into_vstep() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep"]);
            assert_names_absent(&out, &["HasIdStep", "ScalarFilterStep"]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_has_prop_id_folds_into_vstep() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasProperty(HasPropertyStep {
                    key: ID.clone(),
                    pred: PrimitivePredicate::Eq(Primitive::Int64(1)),
                }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep"]);
            assert_names_absent(&out, &["HasPropertyStep", "ScalarFilterStep", "HasIdStep"]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_hasid_hasid_only_first_folds() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "HasIdStep"]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_hasprop_then_hasid_reorder_and_fold() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasProperty(HasPropertyStep {
                    key: "age".into(),
                    pred: PrimitivePredicate::Eq(Primitive::Int32(42)),
                }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "HasPropertyStep"]);
            assert_names_absent(&out, &[]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_hasid_empty_within_not_folded() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasId(HasIdStep {
                    pred: PrimitivePredicate::Within(vec![
                        Primitive::Int64(1),
                        Primitive::Int64(2),
                        Primitive::Int64(3),
                    ]),
                }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep"]);
            assert_names_absent(&out, &["HasIdStep", "ScalarFilterStep"]);
            assert_contains(&out, "ids=[1, 2, 3]");
        }

        #[test]
        fn v_explicit_ids_prevents_hasid_fold() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![42] }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "HasIdStep"]);
            assert_contains(&out, "ids=[42]");
        }

        // ── Group 2: label-based edge folding ─────────────────────────────

        #[test]
        fn oute_label_where_otherv_hasid_folds_to_gete() {
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
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → no GetEStep (InOutStep instead).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep"]);
            assert_names_absent(&out, &["WhereStep", "OtherVStep", "HasIdStep", "InOutStep"]);
            assert_contains(&out, "end_vertex_ids=[2]");
        }

        #[test]
        fn bothe_label_where_otherv_hasid_folds_to_gete() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::BothE(BothEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → GetEStep (id folded).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep"]);
            assert_names_absent(&out, &["WhereStep", "OtherVStep", "HasIdStep"]);
        }

        #[test]
        fn ine_label_where_otherv_hasid_folds_to_gete() {
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
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → GetEStep (id folded).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep"]);
            assert_names_absent(&out, &["WhereStep", "OtherVStep", "HasIdStep"]);
        }

        #[test]
        fn oute_label_where_otherv_hasprop_extracts_to_endvertexfilter() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasProperty(HasPropertyStep {
                        key: "age".into(),
                        pred: PrimitivePredicate::Eq(Primitive::Int32(30)),
                    }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep", "EndVertexFilterStep"]);
        }

        #[test]
        fn oute_has_rank_where_otherv_hasid_folds() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: Some(0) }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → GetEStep (id folded).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep"]);
            assert_names_absent(&out, &["WhereStep", "OtherVStep", "HasIdStep"]);
            assert_contains(&out, "rank=Some(0)");
        }

        #[test]
        fn oute_two_where_both_extracted() {
            let where1 = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let where2 = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(3)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where1 }),
                LogicalStep::Where(WhereStep { plan: where2 }),
            ];
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → GetEStep (id folded).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep"]);
            assert_names_absent(&out, &["WhereStep", "OtherVStep", "HasIdStep"]);
        }

        #[test]
        fn oute_no_label_where_otherv_hasid_no_fold() {
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
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep"]);
            assert_names_absent(&out, &["GetEStep", "WhereStep"]);
        }

        #[test]
        fn oute_no_where_stays_inout() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep"]);
            assert_names_absent(&out, &["GetEStep"]);
        }

        #[test]
        fn bothe_where_hasid_and_haslabel_partial_fold() {
            let where1 = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let where2 = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Eq(Primitive::Int32(1)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where1 }),
                LogicalStep::Where(WhereStep { plan: where2 }),
            ];
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → no GetEStep (InOutStep instead).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep"]);
            assert_names_absent(&out, &[]);
        }

        #[test]
        fn oute_label_has_edgeproperty_no_fold() {
            // Edge-property filter (not other-vertex) stays as HasPropertyStep.
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::HasProperty(HasPropertyStep {
                    key: "weight".into(),
                    pred: PrimitivePredicate::Gt(Primitive::Float64(0.5)),
                }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep", "HasPropertyStep"]);
        }

        #[test]
        fn oute_where_otherv_haslabel_partial_extraction() {
            // hasLabel on otherV extracts to EndVertexFilter but label predicates
            // stay as a residual WhereStep (can't fold into edge step labels).
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Eq(Primitive::Int32(1)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            let out = explain_str(steps);
            // hasLabel on the other vertex extracts to EndVertexFilterStep.
            // The WhereStep is eliminated; hasLabel becomes label_preds in EndVertexFilter.
            assert_names_in_order(&out, &["PhysicalPlan", "VStep"]);
            assert_contains(&out, "EndVertexFilterStep");
        }

        // ── Group 3: addV + property("id") folding ───────────────────────

        #[test]
        fn addv_property_id_folds() {
            // Post-optimization: id was folded into the AddVStep by merge_property_into_add.
            let steps = vec![LogicalStep::AddV(AddVStep {
                label: "person".into(),
                vertex_id: Some(1),
                properties: smallvec::smallvec![],
            })];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "AddVStep"]);
            assert_contains(&out, "id=1");
        }

        #[test]
        fn addv_property_name_then_id_folds() {
            // Post-optimization: id and non-id property both folded into AddVStep.
            let mut props = smallvec::smallvec![];
            props.push(("name".into(), Primitive::String("alice".into())));
            let steps =
                vec![LogicalStep::AddV(AddVStep { label: "person".into(), vertex_id: Some(1), properties: props })];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "AddVStep"]);
            assert_names_absent(&out, &["PropertyStep"]);
            assert_contains(&out, "id=1");
        }

        #[test]
        fn addv_no_id_property_no_fold() {
            let mut props = smallvec::smallvec![];
            props.push(("name".into(), Primitive::String("alice".into())));
            let steps =
                vec![LogicalStep::AddV(AddVStep { label: "person".into(), vertex_id: Some(42), properties: props })];
            let out = explain_str(steps);
            assert_contains(&out, "AddVStep");
        }

        // ── Group 4: addE + from/to/rank folding ─────────────────────────

        #[test]
        fn adde_from_to_merged() {
            // Post-optimization: from/to folded into AddEStep by merge_adde_ids.
            let steps = vec![LogicalStep::AddE(AddEStep {
                label: "knows".into(),
                out_v_id: Some(1),
                in_v_id: Some(2),
                properties: smallvec::smallvec![],
                rank: None,
            })];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "AddEStep"]);
            assert_contains(&out, "from=Some(1)");
            assert_contains(&out, "to=Some(2)");
        }

        #[test]
        fn adde_from_to_rank_all_merged() {
            // Post-optimization: from/to/rank all folded into AddEStep.
            let steps = vec![LogicalStep::AddE(AddEStep {
                label: "knows".into(),
                out_v_id: Some(1),
                in_v_id: Some(2),
                properties: smallvec::smallvec![],
                rank: Some(5),
            })];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "AddEStep"]);
            assert_names_absent(&out, &["PropertyStep"]);
            assert_contains(&out, "from=Some(1)");
            assert_contains(&out, "to=Some(2)");
            assert_contains(&out, "rank=5");
        }

        // ── Group 5: branching operators ──────────────────────────────────

        #[test]
        fn union_has_branch_children() {
            let out_plan =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let in_plan =
                LogicalPlan { steps: vec![LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None })] };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Union(UnionStep { plans: smallvec![out_plan, in_plan] }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "UnionStep"]);
            assert_contains(&out, "branch 0:");
            assert_contains(&out, "branch 1:");
        }

        #[test]
        fn coalesce_has_branch_children() {
            let out_plan =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let addv_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::AddV(AddVStep {
                        label: "person".into(),
                        vertex_id: None,
                        properties: smallvec::smallvec![],
                    }),
                    LogicalStep::Property(PropertyStep { prop_key: ID.clone(), prop_value: Primitive::Int64(99) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Coalesce(CoalesceStep { plans: vec![out_plan, addv_plan] }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "CoalesceStep"]);
            assert_contains(&out, "branch 0:");
            assert_contains(&out, "branch 1:");
        }

        #[test]
        fn where_has_sub_plan_child() {
            let sub = LogicalPlan {
                steps: vec![
                    LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![LogicalStep::V(VStep { ids: smallvec![1] }), LogicalStep::Where(WhereStep { plan: sub })];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "WhereStep"]);
            assert_contains(&out, "InOutStep");
        }

        #[test]
        fn not_has_sub_plan_child() {
            let sub =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let steps = vec![LogicalStep::V(VStep { ids: smallvec![1] }), LogicalStep::Not(NotStep { plan: sub })];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "NotStep"]);
            assert_contains(&out, "InOutStep");
        }

        #[test]
        fn choose_has_predicate_and_branches() {
            let pred_plan = LogicalPlan {
                steps: vec![LogicalStep::ScalarFilter(ScalarFilterStep {
                    pred: PrimitivePredicate::Eq(Primitive::Int64(1)),
                })],
            };
            let true_plan =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let false_plan =
                LogicalPlan { steps: vec![LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None })] };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Choose(ChooseStep {
                    predicate: pred_plan,
                    true_choice: true_plan,
                    false_choice: Some(false_plan),
                }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "ChooseStep"]);
            assert_contains(&out, "pred=");
        }

        #[test]
        fn repeat_times_shows_child() {
            let body =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Repeat(RepeatStep { body, until: None, times: Some(3), emit: EmitSpec::Never }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "RepeatStep"]);
            assert_contains(&out, "times=Some(3)");
            assert_contains(&out, "InOutStep");
        }

        #[test]
        fn repeat_until_shows_body_and_until_children() {
            let body =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let until = LogicalPlan {
                steps: vec![LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) })],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Repeat(RepeatStep { body, until: Some(until), times: None, emit: EmitSpec::Never }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "RepeatStep"]);
            assert_contains(&out, "body");
            assert_contains(&out, "until");
        }

        #[test]
        fn repeat_emit_if_shows_child() {
            let body =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let emit_if = LogicalPlan {
                steps: vec![LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(42)) })],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Repeat(RepeatStep { body, until: None, times: Some(1), emit: EmitSpec::If(emit_if) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "RepeatStep"]);
            assert_contains(&out, "emit=If");
            assert_contains(&out, "emit_if");
            assert_contains(&out, "HasIdStep");
        }

        // ── Group 6: multi-hop and combined rules ─────────────────────────

        #[test]
        fn v_hasid_out_haslabel_combined() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
                LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Eq(Primitive::Int32(1)) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep", "HasLabelStep"]);
            assert_names_absent(&out, &[]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_hasid_oute_where_hasid_inv_haslabel_combined() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
                LogicalStep::OutE(OutEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
                LogicalStep::InV(InVStep {}),
                LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Eq(Primitive::Int32(1)) }),
            ];
            let out = explain_str(steps);
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → no GetEStep (InOutStep instead).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep", "OtherV", "HasLabel"]);
            assert_names_absent(&out, &["HasIdStep", "WhereStep"]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_has_prop_id_out_out_haslabel_combined() {
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasProperty(HasPropertyStep {
                    key: ID.clone(),
                    pred: PrimitivePredicate::Eq(Primitive::Int64(1)),
                }),
                LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }),
                LogicalStep::HasLabel(HasLabelStep { pred: PrimitivePredicate::Eq(Primitive::Int32(1)) }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "InOutStep", "InOutStep", "HasLabelStep"]);
            assert_names_absent(&out, &["HasPropertyStep"]);
            assert_contains(&out, "ids=[1]");
        }

        #[test]
        fn v_hasid_bothe_where_hasid_and_hasprop_combined() {
            let where_plan = LogicalPlan {
                steps: vec![
                    LogicalStep::OtherV(OtherVStep {}),
                    LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(2)) }),
                    LogicalStep::HasProperty(HasPropertyStep {
                        key: "age".into(),
                        pred: PrimitivePredicate::Gt(Primitive::Int32(30)),
                    }),
                ],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![] }),
                LogicalStep::HasId(HasIdStep { pred: PrimitivePredicate::Eq(Primitive::Int64(1)) }),
                LogicalStep::BothE(BothEStep { labels: smallvec!["123".into()], end_vertex_ids: None, rank: None }),
                LogicalStep::Where(WhereStep { plan: where_plan }),
            ];
            let out = explain_str(steps);
            // Partial extraction: hasId extracted into EndVertexFilterStep, merged into BothE →
            // GetEStep instead of InOutStep.  Property filter stays as a smaller WhereStep.
            // Intersection of [2] and [3] from the two where() clauses is empty
            // → no GetEStep (InOutStep instead).
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "GetEStep"]);
            assert_names_absent(&out, &[]);
            assert_contains(&out, "ids=[1]");
        }

        // ── Group 7: regression ──────────────────────────────────────────

        #[test]
        fn empty_plan_renders_root() {
            let steps = vec![];
            let out = explain_str(steps);
            assert_contains(&out, "PhysicalPlan");
        }

        #[test]
        fn plan_with_many_steps_all_present() {
            // degree_pushdown converts the final Out([]).Count pair to DegreeStep+SumStep.
            // The other 9 Out([]) steps remain as InOutStep since they are not immediately
            // followed by Count.
            let mut steps = Vec::new();
            steps.push(LogicalStep::V(VStep { ids: smallvec![1] }));
            for _ in 0..10 {
                steps.push(LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None }));
            }
            steps.push(LogicalStep::Count(CountStep {}));
            let out = explain_str(steps);
            // VStep and InOutStep (from the first 9 Out steps) are still present.
            // The last Out+Count pair becomes DegreeStep+SumStep.
            for name in &["VStep", "InOutStep", "DegreeStep", "SumStep"] {
                assert_contains(&out, name);
            }
        }

        #[test]
        fn nested_branching_depth_correct() {
            let out_plan =
                LogicalPlan { steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })] };
            let in_plan =
                LogicalPlan { steps: vec![LogicalStep::In(InStep { labels: smallvec![], end_vertex_ids: None })] };
            let body =
                LogicalPlan { steps: vec![LogicalStep::Union(UnionStep { plans: smallvec![out_plan, in_plan] })] };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Repeat(RepeatStep { body, until: None, times: Some(2), emit: EmitSpec::Never }),
            ];
            let out = explain_str(steps);
            assert_names_in_order(&out, &["PhysicalPlan", "VStep", "RepeatStep"]);
            assert_contains(&out, "UnionStep");
        }

        #[test]
        fn step_with_no_params() {
            let steps = vec![LogicalStep::Count(CountStep {})];
            let out = explain_str(steps);
            assert_contains(&out, "CountStep");
        }

        #[test]
        fn step_with_params_renders_kv() {
            let steps = vec![LogicalStep::V(VStep { ids: smallvec![1, 2] })];
            let out = explain_str(steps);
            assert_contains(&out, "VStep(ids=[1, 2])");
        }
    }
}
