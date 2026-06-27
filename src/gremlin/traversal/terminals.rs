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

//! Terminal traversal types: [`ReadTraversal`] and [`WriteTraversal`].

use super::*;
use std::collections::HashMap;

// ── ReadTraversal ─────────────────────────────────────────────────────────

/// A read-only traversal bound to a [`ReadSession`](crate::api::ReadSession) context.
pub struct ReadTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
    pub(crate) error: Option<StoreError>,
    pending_repeat: Option<RepeatBuilder>,
    prop_keys: Option<Vec<SmolStr>>,
}

impl<'s> ReadTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx, error: None, pending_repeat: None, prop_keys: None }
    }

    /// Configure property fetching for this traversal.
    ///
    /// With no arguments (`withProperties([])`), all properties are fetched (matching
    /// the pre-0.2.0 behavior). With a list of keys, only those named properties are
    /// returned. Without this call, elements are returned with id + label only.
    ///
    /// Takes `&'a str` rather than `impl Into<SmolStr>` so the empty case infers
    /// without a type annotation — see `TraversalBuilder::out` for why.
    #[allow(non_snake_case)]
    pub fn withProperties<'a>(mut self, keys: impl IntoIterator<Item = &'a str>) -> Self {
        self.prop_keys = Some(keys.into_iter().map(SmolStr::from).collect());
        self
    }

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        GraphTraversal { plan: self.plan, error: None, pending_repeat: None }.build(self.ctx, self.prop_keys)
    }

    /// Execute and return the first result (`tryNext()` in Gremlin).
    pub fn next(self) -> Result<Option<Value>, StoreError> {
        self.iter()?.next().transpose()
    }

    /// Execute and collect all results (`toList()` in Gremlin).
    pub fn to_list(self) -> Result<Vec<Value>, StoreError> {
        self.iter()?.collect()
    }

    /// Build the physical plan and return a pretty-printed explanation tree.
    pub fn explain(self) -> Result<String, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        let mut logical = self.plan;
        crate::planner::apply_rules(&mut logical)?;
        let schema_lock = self.ctx.schema();
        let plan = crate::engine::volcano::builder::PhysicalPlanBuilder.build(&logical, &schema_lock)?;
        Ok(crate::engine::volcano::builder::render_explain(&plan.explain(), 0, ""))
    }
}

#[allow(private_interfaces)]
impl PlanAppender for ReadTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
    fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder> {
        &mut self.pending_repeat
    }
}

// ── WriteTraversal ────────────────────────────────────────────────────────

/// A read-write traversal bound to a [`TxSession`](crate::api::TxSession) context.
pub struct WriteTraversal<'s> {
    plan: LogicalPlan,
    ctx: &'s mut dyn GraphCtx,
    pub(crate) error: Option<StoreError>,
    pending_repeat: Option<RepeatBuilder>,
    prop_keys: Option<Vec<SmolStr>>,
}

impl<'s> WriteTraversal<'s> {
    pub(crate) fn new(ctx: &'s mut dyn GraphCtx) -> Self {
        Self { plan: LogicalPlan { steps: vec![] }, ctx, error: None, pending_repeat: None, prop_keys: None }
    }

    /// Configure property fetching for this traversal (see [`ReadTraversal::withProperties`]).
    #[allow(non_snake_case)]
    pub fn withProperties<'a>(mut self, keys: impl IntoIterator<Item = &'a str>) -> Self {
        self.prop_keys = Some(keys.into_iter().map(SmolStr::from).collect());
        self
    }

    // ── Concrete mutating methods ─────────────────────────────────────────

    #[allow(non_snake_case)]
    pub fn addV(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::AddV(AddVStep {
            label: label.into(),
            vertex_id: None,
            properties: HashMap::new(),
        }));
        self
    }

    #[allow(non_snake_case)]
    pub fn addE(mut self, label: impl Into<SmolStr>) -> Self {
        self.push_step(LogicalStep::AddE(AddEStep {
            label: label.into(),
            out_v_id: None,
            in_v_id: None,
            properties: HashMap::new(),
            rank: None,
        }));
        self
    }

    pub fn from(mut self, vertex_id: i64) -> Self {
        self.push_step(LogicalStep::From(FromStep { vertex_id }));
        self
    }

    pub fn to(mut self, vertex_id: i64) -> Self {
        self.push_step(LogicalStep::To(ToStep { vertex_id }));
        self
    }

    pub fn property(mut self, key: impl Into<SmolStr>, value: impl Into<Value>) -> Self {
        let key_smol = key.into();
        if key_smol == LABEL {
            self.record_error(StoreError::SchemaViolation(
                "Cannot manually set or update the reserved property 'label'. Vertex and edge labels must be specified when creating elements via addV()/addE().".to_string()
            ));
            return self;
        }
        let val = value.into();
        if let Some(prim) = value_to_primitive(val.clone()) {
            self.push_step(LogicalStep::Property(PropertyStep { prop_key: key_smol, prop_value: prim }));
        } else {
            self.record_error(StoreError::UnexpectedDataType(format!(
                "property() expects a scalar primitive value, got complex type: {:?}",
                val
            )));
        }
        self
    }

    pub fn drop(mut self) -> Self {
        self.push_step(LogicalStep::Drop(DropStep {}));
        self
    }

    // ── Terminal ops ──────────────────────────────────────────────────────

    /// Build the physical plan and return a lazy iterator over all results.
    pub fn iter(self) -> Result<BuiltTraversal<'s>, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        GraphTraversal { plan: self.plan, error: None, pending_repeat: None }.build(self.ctx, self.prop_keys)
    }

    /// Execute and return the first result.
    pub fn next(self) -> Result<Option<Value>, StoreError> {
        self.iter()?.next().transpose()
    }

    /// Execute and collect all results.
    pub fn to_list(self) -> Result<Vec<Value>, StoreError> {
        self.iter()?.collect()
    }

    /// Build the physical plan and return a pretty-printed explanation tree.
    pub fn explain(self) -> Result<String, StoreError> {
        if let Some(err) = self.error {
            return Err(err);
        }
        if self.pending_repeat.is_some() {
            return Err(StoreError::TraversalError(
                "repeat() requires at least one stop condition — call .times(n) or .until(cond).".to_string(),
            ));
        }
        let mut logical = self.plan;
        crate::planner::apply_rules(&mut logical)?;
        let schema_lock = self.ctx.schema();
        let plan = crate::engine::volcano::builder::PhysicalPlanBuilder.build(&logical, &schema_lock)?;
        Ok(crate::engine::volcano::builder::render_explain(&plan.explain(), 0, ""))
    }
}

#[allow(private_interfaces)]
impl PlanAppender for WriteTraversal<'_> {
    fn plan_mut(&mut self) -> &mut LogicalPlan {
        &mut self.plan
    }
    fn record_error(&mut self, err: StoreError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }
    fn pending_repeat_mut(&mut self) -> &mut Option<RepeatBuilder> {
        &mut self.pending_repeat
    }
}
