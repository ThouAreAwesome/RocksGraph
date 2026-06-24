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
    schema::{DataType, EdgeMode, Schema, SchemaMode},
    types::{
        error::StoreError,
        gvalue::Primitive,
        keys::Rank,
        prop_key::{ID, LABEL, RANK},
        Direction, LabelId,
    },
};
use smol_str::SmolStr;
use std::collections::HashMap;

fn primitive_data_type(val: &crate::types::gvalue::Primitive) -> DataType {
    use crate::types::gvalue::Primitive;
    match val {
        Primitive::Bool(_) => DataType::Bool,
        Primitive::Int32(_) => DataType::Int32,
        Primitive::Int64(_) => DataType::Int64,
        Primitive::UInt16(_) => DataType::UInt16,
        Primitive::Float32(_) => DataType::Float32,
        Primitive::Float64(_) => DataType::Float64,
        Primitive::String(_) => DataType::String,
        Primitive::Uuid(_) => DataType::Uuid,
        Primitive::Null => DataType::String,
    }
}

fn resolve_read_edge_label(name: &str, schema: &Schema) -> Result<Option<LabelId>, StoreError> {
    if let Some(id) = schema.edge_label_id(name) {
        Ok(Some(id))
    } else {
        match schema.mode {
            SchemaMode::Strict => Err(StoreError::SchemaViolation(format!("Undeclared edge label: '{}'", name))),
            SchemaMode::Auto => Ok(None),
        }
    }
}

/// Resolve a vertex label for a write step, taking the `Schema` write lock only on the
/// (post-warmup, rare) path where the label is genuinely new. The common case — the label
/// was already registered by an earlier write — is satisfied entirely under a read lock.
///
/// This matters under concurrency: every `addV`/`property(...)` previously took the write
/// lock unconditionally, even when nothing was going to change. With several writer threads
/// doing that simultaneously, the write-preferring `RwLock` starves readers badly enough to
/// look like a hang (see the regression test below).
fn resolve_write_vertex_label(name: &str, schema_lock: &std::sync::RwLock<Schema>) -> Result<LabelId, StoreError> {
    if let Some(id) = schema_lock.read().unwrap().vertex_label_id(name) {
        return Ok(id);
    }
    schema_lock.write().unwrap().resolve_vertex_label(name)
}

/// Same rationale as [`resolve_write_vertex_label`], for edge labels.
fn resolve_write_edge_label(name: &str, schema_lock: &std::sync::RwLock<Schema>) -> Result<LabelId, StoreError> {
    if let Some(id) = schema_lock.read().unwrap().edge_label_id(name) {
        return Ok(id);
    }
    schema_lock.write().unwrap().resolve_edge_label(name)
}

/// Same rationale as [`resolve_write_vertex_label`], for property keys. The type-mismatch
/// check against an already-registered key is itself read-only, so the common case (key
/// already exists, type matches) never escalates to the write lock at all.
fn resolve_write_prop_key(
    name: &str,
    inferred_type: DataType,
    schema_lock: &std::sync::RwLock<Schema>,
) -> Result<u16, StoreError> {
    {
        let schema = schema_lock.read().unwrap();
        if let Some(id) = schema.prop_key_id(name) {
            if let Some(cfg) = schema.prop_key_types.get(&id) {
                if cfg.data_type != inferred_type {
                    return Err(StoreError::SchemaViolation(format!(
                        "Property key '{}' is already defined with type {:?}, but requested {:?}",
                        name, cfg.data_type, inferred_type
                    )));
                }
                return Ok(id);
            }
        }
    }
    schema_lock.write().unwrap().resolve_prop_key(name, inferred_type)
}

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
    pub fn build(
        &mut self,
        plan: &LogicalPlan,
        schema_lock: &std::sync::RwLock<Schema>,
    ) -> Result<PhysicalPlan, StoreError> {
        let source = BufferedStep::new(VecSourceStep::empty());

        if plan.steps.is_empty() {
            let tail: StepRef = source.clone();
            return Ok(PhysicalPlan { source, tail });
        }

        let mut upstream: Option<StepRef> = Some(source.clone());
        for step in &plan.steps {
            upstream = self.build_step(step, upstream, schema_lock)?;
        }

        Ok(PhysicalPlan { source, tail: upstream.expect("plan must have at least one step") })
    }

    fn build_step(
        &mut self,
        step: &LogicalStep,
        upstream: Option<StepRef>,
        schema_lock: &std::sync::RwLock<Schema>,
    ) -> Result<Option<StepRef>, StoreError> {
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

        // A label + end-vertex combination is specific enough to do a point lookup
        // (`GetEStep`) instead of an adjacency scan (`InOutStep`/`BothStep`) — but only when
        // the rank to look up is actually known. If it isn't, `GetEStep` would have to guess
        // `DEFAULT_RANK`, which is only correct when every label involved is single-edge;
        // for a multi-edge label, the real edge could sit at any rank, and guessing 0 would
        // silently miss it. So an unknown rank is only safe when every label in `label_ids`
        // is confirmed single-edge via `Schema`; otherwise fall back to the scan, which
        // already returns every rank correctly regardless of edge mode.
        macro_rules! get_e_or_scan {
            ($s:expr, $label_ids:expr, $direction:expr, $rank:expr, $output_edges:expr, $scan:expr, $name:literal) => {{
                if let Some(end_ids) = &$s.end_vertex_ids {
                    let schema_read = schema_lock.read().unwrap();
                    let rank_safe = $rank.is_some() || schema_read.edge_mode == EdgeMode::Single;
                    if !$label_ids.is_empty() && !end_ids.is_empty() && rank_safe {
                        return wire_required!(
                            BufferedStep::new(steps::get_e::GetEStep::new(
                                $label_ids.clone(),
                                end_ids.clone(),
                                $direction,
                                $rank,
                                $output_edges
                            )),
                            upstream,
                            $name
                        );
                    }
                }
                wire_required!(BufferedStep::new($scan), upstream, $name)
            }};
        }

        let schema = schema_lock.read().unwrap();

        match step {
            LogicalStep::Both(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step =
                    steps::both::BothStep::new(label_ids.clone(), s.end_vertex_ids.clone(), None::<Rank>, false);
                get_e_or_scan!(s, label_ids, None, None::<Rank>, false, scan_step, "BothStep")
            }
            LogicalStep::BothE(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step = steps::both::BothStep::new(label_ids.clone(), s.end_vertex_ids.clone(), s.rank, true);
                get_e_or_scan!(s, label_ids, None, s.rank, true, scan_step, "BothEStep")
            }
            LogicalStep::V(s) => {
                wire!(BufferedStep::new(steps::v::VStep::new(s.ids.clone())), None::<StepRef>)
            }
            LogicalStep::E(s) => {
                wire!(BufferedStep::new(steps::e::EStep::new(s.keys.clone())), None::<StepRef>)
            }
            LogicalStep::Count(_) => {
                wire_required!(BufferedStep::new(steps::count::CountStep::default()), upstream, "CountStep")
            }
            LogicalStep::HasLabel(s) => {
                if schema.mode == SchemaMode::Strict {
                    for v in s.pred.values() {
                        if let Primitive::String(name) = v {
                            if schema.vertex_label_id(name).is_none() && schema.edge_label_id(name).is_none() {
                                return Err(StoreError::SchemaViolation(format!("Undeclared label: '{}'", name)));
                            }
                        }
                    }
                }
                // Resolve each label name to its interned id once here — separately per
                // namespace, since vertex and edge labels are independent id spaces — so
                // `HasLabelStep::produce()` can compare raw ids with no schema lookup needed.
                let vertex_pred = s.pred.clone().map(|v| match v {
                    Primitive::String(name) => Primitive::Int32(
                        schema
                            .vertex_label_id(&name)
                            .map(|id| id as i32)
                            .unwrap_or(steps::has_label::UNRESOLVED_LABEL_ID),
                    ),
                    other => other,
                });
                let edge_pred = s.pred.clone().map(|v| match v {
                    Primitive::String(name) => Primitive::Int32(
                        schema
                            .edge_label_id(&name)
                            .map(|id| id as i32)
                            .unwrap_or(steps::has_label::UNRESOLVED_LABEL_ID),
                    ),
                    other => other,
                });
                wire_required!(
                    BufferedStep::new(steps::has_label::HasLabelStep::new(vertex_pred, edge_pred)),
                    upstream,
                    "HasLabelStep"
                )
            }
            LogicalStep::HasProperty(s) => {
                let prop_key_id = if let Some(id) = schema.prop_key_id(&s.key) {
                    id
                } else {
                    match schema.mode {
                        SchemaMode::Strict => {
                            return Err(StoreError::SchemaViolation(format!("Undeclared property key: '{}'", s.key)))
                        }
                        SchemaMode::Auto => u16::MAX,
                    }
                };
                wire_required!(
                    BufferedStep::new(steps::has_property::HasPropertyStep::new(prop_key_id, s.pred.clone())),
                    upstream,
                    "HasPropertyStep"
                )
            }
            LogicalStep::In(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step = steps::in_out::InOutStep::new(
                    label_ids.clone(),
                    Direction::IN,
                    s.end_vertex_ids.clone(),
                    None::<Rank>,
                    false,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::IN), None::<Rank>, false, scan_step, "InStep")
            }
            LogicalStep::InE(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step = steps::in_out::InOutStep::new(
                    label_ids.clone(),
                    Direction::IN,
                    s.end_vertex_ids.clone(),
                    s.rank,
                    true,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::IN), s.rank, true, scan_step, "InEStep")
            }
            LogicalStep::Out(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step = steps::in_out::InOutStep::new(
                    label_ids.clone(),
                    Direction::OUT,
                    s.end_vertex_ids.clone(),
                    None::<Rank>,
                    false,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::OUT), None::<Rank>, false, scan_step, "OutStep")
            }
            LogicalStep::OutE(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step = steps::in_out::InOutStep::new(
                    label_ids.clone(),
                    Direction::OUT,
                    s.end_vertex_ids.clone(),
                    s.rank,
                    true,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::OUT), s.rank, true, scan_step, "OutEStep")
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
                    BufferedStep::new(steps::scalar_filter::ScalarFilterStep::new(s.pred.clone())),
                    upstream,
                    "ScalarFilterStep"
                )
            }
            LogicalStep::Values(s) => {
                let resolved_keys = s
                    .property_keys
                    .iter()
                    .map(|k| {
                        if let Some(id) = schema.prop_key_id(k) {
                            Ok((k.clone(), id))
                        } else {
                            match schema.mode {
                                SchemaMode::Strict => {
                                    Err(StoreError::SchemaViolation(format!("Undeclared property key: '{}'", k)))
                                }
                                SchemaMode::Auto => Ok((k.clone(), u16::MAX)),
                            }
                        }
                    })
                    .collect::<Result<SmallVec<[(SmolStr, u16); 4]>, _>>()?;
                wire_required!(
                    BufferedStep::new(steps::values::ValuesStep::new(resolved_keys, false)),
                    upstream,
                    "ValuesStep"
                )
            }
            LogicalStep::Properties(s) => {
                let resolved_keys = s
                    .property_keys
                    .iter()
                    .map(|k| {
                        if let Some(id) = schema.prop_key_id(k) {
                            Ok((k.clone(), id))
                        } else {
                            match schema.mode {
                                SchemaMode::Strict => {
                                    Err(StoreError::SchemaViolation(format!("Undeclared property key: '{}'", k)))
                                }
                                SchemaMode::Auto => Ok((k.clone(), u16::MAX)),
                            }
                        }
                    })
                    .collect::<Result<SmallVec<[(SmolStr, u16); 4]>, _>>()?;
                wire_required!(
                    BufferedStep::new(steps::values::ValuesStep::new(resolved_keys, true)),
                    upstream,
                    "ValuesStep"
                )
            }
            LogicalStep::Where(s) => {
                if s.plan.steps.is_empty() {
                    return Err(StoreError::RuntimeError("WhereStep must have a non-empty sub-plan.".to_string()));
                }
                drop(schema);
                let physical_plan = self.build(&s.plan, schema_lock)?;
                wire_required!(BufferedStep::new(steps::r#where::WhereStep::new(physical_plan)), upstream, "WhereStep")
            }
            LogicalStep::Union(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::RuntimeError(
                        "UnionStep must have at least one child traversal.".to_string(),
                    ));
                }
                drop(schema);
                let physical_plans = s.plans.iter().map(|p| self.build(p, schema_lock)).collect::<Result<_, _>>()?;
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
                drop(schema);
                let label_id = resolve_write_vertex_label(&s.label, schema_lock)?;
                let mut resolved_props = HashMap::new();
                for (k, v) in &s.properties {
                    let inferred_type = primitive_data_type(v);
                    let id = resolve_write_prop_key(k, inferred_type, schema_lock)?;
                    resolved_props.insert(id, v.clone());
                }
                wire!(
                    BufferedStep::new(steps::add_v::AddVStep::new(label_id, vertex_id, resolved_props)),
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
                drop(schema);
                let label_id = resolve_write_edge_label(&s.label, schema_lock)?;
                let mut resolved_props = HashMap::new();
                for (k, v) in &s.properties {
                    let inferred_type = primitive_data_type(v);
                    let id = resolve_write_prop_key(k, inferred_type, schema_lock)?;
                    resolved_props.insert(id, v.clone());
                }
                wire!(
                    BufferedStep::new(
                        steps::add_e::AddEStep::new(label_id, out_v_id, in_v_id, resolved_props, s.rank,)
                    ),
                    None::<StepRef>
                )
            }
            LogicalStep::Property(s) => {
                if s.prop_key == ID || s.prop_key == LABEL || s.prop_key == RANK {
                    return Err(StoreError::SchemaViolation(format!(
                        "Unfolded or misplaced reserved property key: '{}'. Writes to reserved keys must immediately follow the creating step (addV/addE) to be optimized.",
                        s.prop_key
                    )));
                }
                drop(schema);
                let inferred_type = primitive_data_type(&s.prop_value);
                let id = resolve_write_prop_key(&s.prop_key, inferred_type, schema_lock)?;
                wire_required!(
                    BufferedStep::new(steps::property::PropertyStep::new(id, s.prop_value.clone())),
                    upstream,
                    "PropertyStep"
                )
            }
            LogicalStep::Limit(s) => {
                wire_required!(BufferedStep::new(steps::limit::LimitStep::new(s.limit)), upstream, "LimitStep")
            }
            LogicalStep::HasId(s) => {
                wire_required!(BufferedStep::new(steps::has_id::HasIdStep::new(s.pred.clone())), upstream, "HasIdStep")
            }
            LogicalStep::Coalesce(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::RuntimeError(
                        "CoalesceStep must have at least one child traversal.".to_string(),
                    ));
                }
                drop(schema);
                let physical_plans = s.plans.iter().map(|p| self.build(p, schema_lock)).collect::<Result<_, _>>()?;
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
            LogicalStep::Path(_) => {
                wire_required!(BufferedStep::new(steps::path::PathStep::new()), upstream, "PathStep")
            }
            LogicalStep::Drop(_) => {
                wire_required!(BufferedStep::new(steps::drop::DropStep::default()), upstream, "DropStep")
            }
            LogicalStep::Dedup(_) => {
                wire_required!(BufferedStep::new(steps::dedup::DedupStep::default()), upstream, "DedupStep")
            }
            LogicalStep::Fold(_) => {
                wire_required!(BufferedStep::new(steps::fold::FoldStep::default()), upstream, "FoldStep")
            }
            LogicalStep::Repeat(s) => {
                if s.until.is_none() && s.times.is_none() {
                    return Err(StoreError::RuntimeError(
                        "RepeatStep must have at least one stop condition: .times(n) or .until(cond).".to_string(),
                    ));
                }
                drop(schema);
                let body = self.build(&s.body, schema_lock)?;
                let until = s.until.as_ref().map(|p| self.build(p, schema_lock)).transpose()?;
                let emit = match &s.emit {
                    crate::planner::logical_step::EmitSpec::Never => steps::repeat::PhysicalEmitMode::Never,
                    crate::planner::logical_step::EmitSpec::Always => steps::repeat::PhysicalEmitMode::Always,
                    crate::planner::logical_step::EmitSpec::If(plan) => {
                        steps::repeat::PhysicalEmitMode::If(self.build(plan, schema_lock)?)
                    }
                };
                wire_required!(
                    BufferedStep::new(steps::repeat::RepeatStep::new(body, until, s.times, emit)),
                    upstream,
                    "RepeatStep"
                )
            }
            _ => unreachable!("unreachable"),
        }
    }
}

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
                    CoalesceStep, CountStep, EmitSpec, HasIdStep, HasPropertyStep, InEStep, InStep,
                    LogicalPlan, LogicalStep,
                    OtherVStep, OutEStep, OutStep, PropertiesStep, RepeatStep, UnionStep, VStep,
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
            let body = LogicalPlan {
                steps: vec![LogicalStep::Out(OutStep { labels: smallvec![], end_vertex_ids: None })],
            };
            let steps = vec![
                LogicalStep::V(VStep { ids: smallvec![1] }),
                LogicalStep::Repeat(RepeatStep {
                    body,
                    until: None,
                    times: Some(3),
                    emit: EmitSpec::Never,
                }),
            ];
            assert_plan_contains_in_order(steps, &["VStep", "RepeatStep", "InOutStep"]);
        }
    }
}
