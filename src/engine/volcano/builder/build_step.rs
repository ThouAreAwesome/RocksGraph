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

//! The `build_step` method of [`PhysicalPlanBuilder`] — the single place in the
//! codebase that maps [`LogicalStep`] variants to volcano physical operators.
//!
//! Also houses the private schema-resolution helpers used by that method.
//!
//! [`PhysicalPlanBuilder`]: super::PhysicalPlanBuilder
//! [`LogicalStep`]: crate::planner::logical_step::LogicalStep

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::{
    engine::volcano::steps,
    planner::logical_step::LogicalStep,
    schema::{DataType, EdgeMode, Schema, SchemaMode},
    types::{
        error::StoreError,
        gvalue::Primitive,
        keys::Rank,
        prop_key::{ID, LABEL, RANK},
        Direction, LabelId,
    },
};

use super::PhysicalPlanBuilder;

// ── Schema-resolution helpers ────────────────────────────────────────────────

pub(super) fn primitive_data_type(val: &crate::types::gvalue::Primitive) -> DataType {
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

pub(super) fn resolve_read_edge_label(name: &str, schema: &Schema) -> Result<Option<LabelId>, StoreError> {
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
/// (post-warmup, rare) path where the label is genuinely new.
pub(super) fn resolve_write_vertex_label(
    name: &str,
    schema_lock: &std::sync::RwLock<Schema>,
) -> Result<LabelId, StoreError> {
    if let Some(id) = schema_lock.read().unwrap().vertex_label_id(name) {
        return Ok(id);
    }
    schema_lock.write().unwrap().resolve_vertex_label(name)
}

/// Same rationale as [`resolve_write_vertex_label`], for edge labels.
pub(super) fn resolve_write_edge_label(
    name: &str,
    schema_lock: &std::sync::RwLock<Schema>,
) -> Result<LabelId, StoreError> {
    if let Some(id) = schema_lock.read().unwrap().edge_label_id(name) {
        return Ok(id);
    }
    schema_lock.write().unwrap().resolve_edge_label(name)
}

/// Same rationale as [`resolve_write_vertex_label`], for property keys.
pub(super) fn resolve_write_prop_key(
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

// ── build_step ───────────────────────────────────────────────────────────────

impl PhysicalPlanBuilder {
    /// Compile a single [`LogicalStep`] into a chain link of physical steps.
    pub(super) fn build_step(
        &mut self,
        step: &LogicalStep,
        upstream: Option<steps::traits::StepRef>,
        schema_lock: &std::sync::RwLock<Schema>,
        track_path: bool,
    ) -> Result<Option<steps::traits::StepRef>, StoreError> {
        use crate::engine::volcano::steps::traits::{BufferedStep, GremlinStep, StepRef};
        use smallvec::SmallVec;

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
                        return Err(StoreError::TraversalError(format!("{} must have an upstream", $name)));
                    }
                }
                Ok(Some(phys as StepRef))
            }};
        }

        // A label + end-vertex combination is specific enough to do a point lookup
        // (`GetEStep`) instead of an adjacency scan (`InOutStep`/`BothStep`) — but only when
        // the rank to look up is actually known.
        macro_rules! get_e_or_scan {
            ($s:expr, $label_ids:expr, $direction:expr, $rank:expr, $output_edges:expr, $scan:expr, $edge_mode:ident, $name:literal) => {{
                if let Some(end_ids) = &$s.end_vertex_ids {
                    let rank_safe = $rank.is_some() || $edge_mode == EdgeMode::Single;
                    if !$label_ids.is_empty() && !end_ids.is_empty() && rank_safe {
                        return wire_required!(
                            BufferedStep::new(steps::get_e::GetEStep::new(
                                $label_ids.clone(),
                                end_ids.clone(),
                                $direction,
                                $rank,
                                $output_edges,
                                track_path,
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
        // Captured once here so the `get_e_or_scan!` macro below can check
        // edge_mode without re-acquiring the RwLock inside the macro body.
        let edge_mode = schema.edge_mode;

        match step {
            LogicalStep::Both(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step = steps::both::BothStep::new(
                    label_ids.clone(),
                    s.end_vertex_ids.clone(),
                    None::<Rank>,
                    false,
                    track_path,
                );
                get_e_or_scan!(s, label_ids, None, None::<Rank>, false, scan_step, edge_mode, "BothStep")
            }
            LogicalStep::BothE(s) => {
                let label_ids = s
                    .labels
                    .iter()
                    .map(|l| resolve_read_edge_label(l, &schema).map(|id_opt| id_opt.unwrap_or(u16::MAX)))
                    .collect::<Result<SmallVec<[LabelId; 4]>, _>>()?;
                let scan_step =
                    steps::both::BothStep::new(label_ids.clone(), s.end_vertex_ids.clone(), s.rank, true, track_path);
                get_e_or_scan!(s, label_ids, None, s.rank, true, scan_step, edge_mode, "BothEStep")
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
                    track_path,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::IN), None::<Rank>, false, scan_step, edge_mode, "InStep")
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
                    track_path,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::IN), s.rank, true, scan_step, edge_mode, "InEStep")
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
                    track_path,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::OUT), None::<Rank>, false, scan_step, edge_mode, "OutStep")
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
                    track_path,
                );
                get_e_or_scan!(s, label_ids, Some(Direction::OUT), s.rank, true, scan_step, edge_mode, "OutEStep")
            }
            LogicalStep::InV(_) => {
                wire_required!(
                    BufferedStep::new(steps::in_v_out_v::InVOutVStep::new(Direction::IN, track_path)),
                    upstream,
                    "InVStep"
                )
            }
            LogicalStep::OtherV(_) => {
                wire_required!(BufferedStep::new(steps::other_v::OtherVStep::new(track_path)), upstream, "OtherVStep")
            }
            LogicalStep::OutV(_) => {
                wire_required!(
                    BufferedStep::new(steps::in_v_out_v::InVOutVStep::new(Direction::OUT, track_path)),
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
                    BufferedStep::new(steps::values::ValuesStep::new(resolved_keys, false, track_path)),
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
                    BufferedStep::new(steps::values::ValuesStep::new(resolved_keys, true, track_path)),
                    upstream,
                    "ValuesStep"
                )
            }
            LogicalStep::Where(s) => {
                if s.plan.steps.is_empty() {
                    return Err(StoreError::TraversalError("WhereStep must have a non-empty sub-plan.".to_string()));
                }
                drop(schema);
                let physical_plan = self.build_steps(&s.plan, schema_lock, track_path)?;
                wire_required!(BufferedStep::new(steps::r#where::WhereStep::new(physical_plan)), upstream, "WhereStep")
            }
            LogicalStep::Union(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::TraversalError(
                        "UnionStep must have at least one child traversal.".to_string(),
                    ));
                }
                drop(schema);
                let physical_plans =
                    s.plans.iter().map(|p| self.build_steps(p, schema_lock, track_path)).collect::<Result<_, _>>()?;
                wire_required!(BufferedStep::new(steps::union::UnionStep::new(physical_plans)), upstream, "UnionStep")
            }
            LogicalStep::AddV(s) => {
                let Some(vertex_id) = s.vertex_id else {
                    return Err(StoreError::TraversalError(
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
                    return Err(StoreError::TraversalError(
                        "AddEStep cannot be built without an out-vertex ID. A preceding `from(...)` step is required \
                         and should have been folded by the optimizer."
                            .to_string(),
                    ));
                };
                let Some(in_v_id) = s.in_v_id else {
                    return Err(StoreError::TraversalError(
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
                    BufferedStep::new(steps::add_e::AddEStep::new(label_id, out_v_id, in_v_id, resolved_props, s.rank)),
                    None::<StepRef>
                )
            }
            LogicalStep::Property(s) => {
                if s.prop_key == ID || s.prop_key == LABEL || s.prop_key == RANK {
                    return Err(StoreError::SchemaViolation(format!(
                        "Unfolded or misplaced reserved property key: '{}'. Writes to reserved keys must immediately \
                         follow the creating step (addV/addE) to be optimized.",
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
                    return Err(StoreError::TraversalError(
                        "CoalesceStep must have at least one child traversal.".to_string(),
                    ));
                }
                drop(schema);
                let physical_plans =
                    s.plans.iter().map(|p| self.build_steps(p, schema_lock, track_path)).collect::<Result<_, _>>()?;
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
                    return Err(StoreError::TraversalError(
                        "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
                    ));
                }
                drop(schema);
                let body = self.build_steps(&s.body, schema_lock, track_path)?;
                let until = s.until.as_ref().map(|p| self.build_steps(p, schema_lock, track_path)).transpose()?;
                let emit = match &s.emit {
                    crate::planner::logical_step::EmitSpec::Never => steps::repeat::PhysicalEmitMode::Never,
                    crate::planner::logical_step::EmitSpec::Always => steps::repeat::PhysicalEmitMode::Always,
                    crate::planner::logical_step::EmitSpec::If(plan) => {
                        steps::repeat::PhysicalEmitMode::If(self.build_steps(plan, schema_lock, track_path)?)
                    }
                };
                wire_required!(
                    BufferedStep::new(steps::repeat::RepeatStep::new(body, until, s.times, emit)),
                    upstream,
                    "RepeatStep"
                )
            }
            LogicalStep::Not(s) => {
                if s.plan.steps.is_empty() {
                    return Err(StoreError::TraversalError("NotStep must have a non-empty sub-plan.".to_string()));
                }
                drop(schema);
                let physical_plan = self.build_steps(&s.plan, schema_lock, track_path)?;
                wire_required!(BufferedStep::new(steps::not::NotStep::new(physical_plan)), upstream, "NotStep")
            }
            LogicalStep::And(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::TraversalError(
                        "AndStep must have at least one child traversal.".to_string(),
                    ));
                }
                drop(schema);
                let physical_plans =
                    s.plans.iter().map(|p| self.build_steps(p, schema_lock, track_path)).collect::<Result<_, _>>()?;
                wire_required!(BufferedStep::new(steps::and_or::AndStep::new(physical_plans)), upstream, "AndStep")
            }
            LogicalStep::Or(s) => {
                if s.plans.is_empty() {
                    return Err(StoreError::TraversalError(
                        "OrStep must have at least one child traversal.".to_string(),
                    ));
                }
                drop(schema);
                let physical_plans =
                    s.plans.iter().map(|p| self.build_steps(p, schema_lock, track_path)).collect::<Result<_, _>>()?;
                wire_required!(BufferedStep::new(steps::and_or::OrStep::new(physical_plans)), upstream, "OrStep")
            }
            LogicalStep::Sum(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::numeric_reducers::SumStep::default()), upstream, "SumStep")
            }
            LogicalStep::Mean(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::numeric_reducers::MeanStep::default()), upstream, "MeanStep")
            }
            LogicalStep::Max(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::numeric_reducers::MaxStep::default()), upstream, "MaxStep")
            }
            LogicalStep::Min(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::numeric_reducers::MinStep::default()), upstream, "MinStep")
            }
            LogicalStep::Unfold(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::unfold::UnfoldStep::new(track_path)), upstream, "UnfoldStep")
            }
            LogicalStep::As(s) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::as_select::AsStep::new(s.labels.clone())), upstream, "AsStep")
            }
            LogicalStep::Select(s) => {
                drop(schema);
                wire_required!(
                    BufferedStep::new(steps::as_select::SelectStep::new(s.labels.clone())),
                    upstream,
                    "SelectStep"
                )
            }
            LogicalStep::Range(s) => {
                drop(schema);
                wire_required!(
                    BufferedStep::new(steps::range_skip_tail::RangeStep::new(s.lo, s.hi)),
                    upstream,
                    "RangeStep"
                )
            }
            LogicalStep::Skip(s) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::range_skip_tail::SkipStep::new(s.n)), upstream, "SkipStep")
            }
            LogicalStep::Tail(s) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::range_skip_tail::TailStep::new(s.n)), upstream, "TailStep")
            }
            LogicalStep::Order(s) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::order::OrderStep::new(s.keys.clone())), upstream, "OrderStep")
            }
            LogicalStep::SimplePath(_) => {
                drop(schema);
                wire_required!(
                    BufferedStep::new(steps::simple_cyclic_path::SimplePathStep::default()),
                    upstream,
                    "SimplePathStep"
                )
            }
            LogicalStep::CyclicPath(_) => {
                drop(schema);
                wire_required!(
                    BufferedStep::new(steps::simple_cyclic_path::CyclicPathStep::default()),
                    upstream,
                    "CyclicPathStep"
                )
            }
            LogicalStep::Group(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::group::GroupStep::default()), upstream, "GroupStep")
            }
            LogicalStep::GroupCount(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::group::GroupCountStep::default()), upstream, "GroupCountStep")
            }
            LogicalStep::Choose(s) => {
                drop(schema);
                let predicate = self.build(&s.predicate, schema_lock)?;
                let true_choice = self.build(&s.true_choice, schema_lock)?;
                let false_choice =
                    if let Some(ref fc) = s.false_choice { Some(self.build(fc, schema_lock)?) } else { None };
                wire_required!(
                    BufferedStep::new(steps::choose::ChooseStep::new(predicate, true_choice, false_choice)),
                    upstream,
                    "ChooseStep"
                )
            }
            LogicalStep::Id(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::id_step::IdStep::default()), upstream, "IdStep")
            }
            LogicalStep::Identity(_) => {
                drop(schema);
                wire_required!(BufferedStep::new(steps::identity::IdentityStep::default()), upstream, "IdentityStep")
            }
            LogicalStep::Constant(s) => {
                drop(schema);
                wire_required!(
                    BufferedStep::new(steps::constant::ConstantStep::new(s.value.clone())),
                    upstream,
                    "ConstantStep"
                )
            }
            LogicalStep::Label(_) => {
                wire_required!(BufferedStep::new(steps::label_step::LabelStep::default()), upstream, "LabelStep")
            }
            LogicalStep::Local(s) => {
                drop(schema);
                let physical_plan = self.build_steps(&s.plan, schema_lock, track_path)?;
                wire_required!(
                    BufferedStep::new(steps::local::LocalStep::new(physical_plan)),
                    upstream,
                    "LocalStep"
                )
            }
            LogicalStep::From(_) | LogicalStep::To(_) => Err(StoreError::UnsupportedOperation(
                "From/To steps are optimizer-internal and should be eliminated before physical build.".to_string(),
            )),
        }
    }
}
