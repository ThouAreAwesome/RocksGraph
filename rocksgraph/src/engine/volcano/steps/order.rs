// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

// Physical step: order()

use crate::engine::volcano::builder::PhysicalPlan;
use crate::engine::volcano::steps::traits::ExplainNode;
use crate::types::PIPELINE_PRODUCE_SIZE;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    planner::logical_step::Order,
    schema::Schema,
    types::{
        error::StoreError,
        gvalue::{GValue, Primitive},
        keys::CanonicalKey,
        ORDER_KEY_INLINE,
    },
};
use parking_lot::RwLock;
use smallvec::{smallvec, SmallVec};
use smol_str::SmolStr;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Physical specification of what to compare when sorting.
#[derive(Debug)]
pub enum PhysicalOrderKeySpec {
    /// Compare by the traverser value itself.
    Value,
    /// Compare by a property value (resolved at runtime via cache).
    Property(SmolStr),
    /// Compare by the output of an anonymous sub-traversal.
    Traversal(PhysicalPlan),
}

/// A physical sort key with direction.
#[derive(Debug)]
pub struct PhysicalOrderKey {
    pub spec: PhysicalOrderKeySpec,
    pub order: Order,
}

/// Sorts all upstream traversers and emits them in order.
#[derive(Debug)]
pub struct OrderStep {
    upstream: Option<StepRef>,
    keys: SmallVec<[PhysicalOrderKey; ORDER_KEY_INLINE]>,
    buffer: Vec<(Rc<Traverser>, SmallVec<[Primitive; ORDER_KEY_INLINE]>)>,
    cursor: usize,
    drained: bool,
    /// Lazily resolved property-key ids, populated on first use per name.
    prop_key_cache: HashMap<SmolStr, u16>,
}

impl OrderStep {
    pub fn new(keys: SmallVec<[PhysicalOrderKey; ORDER_KEY_INLINE]>) -> Self {
        Self { upstream: None, keys, buffer: Vec::new(), cursor: 0, drained: false, prop_key_cache: HashMap::new() }
    }
}

/// Resolve a property name to its `u16` prop_key_id, caching the result.
fn resolve_prop_key_id(schema: &Arc<RwLock<Schema>>, cache: &mut HashMap<SmolStr, u16>, name: &SmolStr) -> Option<u16> {
    if let Some(&id) = cache.get(name) {
        return Some(id);
    }
    let guard = schema.read();
    let id = guard.prop_key_id(name)?;
    cache.insert(name.clone(), id);
    Some(id)
}

/// Extract a comparison key from a traverser, resolving property lookups
/// or executing sub-traversals where needed.
fn extract_order_key(
    prop_key_cache: &mut HashMap<SmolStr, u16>,
    ctx: &mut dyn GraphCtx,
    traverser: &Rc<Traverser>,
    spec: &PhysicalOrderKeySpec,
) -> Result<Option<Primitive>, StoreError> {
    let value = &traverser.value;
    match spec {
        PhysicalOrderKeySpec::Value => match value {
            GValue::Scalar(p) => Ok(Some(p.clone())),
            GValue::Vertex(v) => Ok(Some(Primitive::Int64(*v))),
            _ => Ok(None),
        },
        PhysicalOrderKeySpec::Property(prop_name) => {
            let schema = ctx.schema();
            let Some(prop_id) = resolve_prop_key_id(&schema, prop_key_cache, prop_name) else {
                return Ok(None);
            };
            let canonical_key = match value {
                GValue::Vertex(vk) => CanonicalKey::Vertex(*vk),
                GValue::Edge(ek) => CanonicalKey::Edge(ek.canonical_edge_key()),
                GValue::Scalar(_) => return Ok(extract_key_fallback(value)),
                _ => return Ok(None),
            };
            Ok(ctx.get_value(&canonical_key, prop_id).ok().flatten())
        }
        PhysicalOrderKeySpec::Traversal(sub_plan) => {
            sub_plan.reset();
            sub_plan.inject(smallvec![Rc::clone(traverser)]);
            if let Some(res) = sub_plan.next(ctx)? {
                match &res.value {
                    GValue::Scalar(p) => Ok(Some(p.clone())),
                    GValue::Vertex(v) => Ok(Some(Primitive::Int64(*v))),
                    _ => Ok(None),
                }
            } else {
                Ok(None)
            }
        }
    }
}

/// Fallback extraction for non-vertex/non-edge values.
fn extract_key_fallback(value: &GValue) -> Option<Primitive> {
    match value {
        GValue::Scalar(p) => Some(p.clone()),
        GValue::Vertex(v) => Some(Primitive::Int64(*v)),
        _ => None,
    }
}

impl CoreStep for OrderStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.drained = false;
        for k in &mut self.keys {
            if let PhysicalOrderKeySpec::Traversal(sub_plan) = &mut k.spec {
                sub_plan.reset();
            }
        }
        if let Some(u) = &self.upstream {
            u.reset();
        }
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if !self.drained {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let cache = &mut self.prop_key_cache;
            while let Some(t) = upstream.next(ctx)? {
                let mut key_values = SmallVec::new();
                for k in &self.keys {
                    let key = extract_order_key(cache, ctx, &t, &k.spec)?.unwrap_or(Primitive::Null);
                    key_values.push(key);
                }
                self.buffer.push((t, key_values));
            }
            self.drained = true;
            // Sort: compare key tuples lexicographically
            self.buffer.sort_by(|a, b| {
                for i in 0..self.keys.len() {
                    let ord = Primitive::partial_cmp(&a.1[i], &b.1[i]).unwrap_or(std::cmp::Ordering::Equal);
                    if ord != std::cmp::Ordering::Equal {
                        return if self.keys[i].order == Order::Asc { ord } else { ord.reverse() };
                    }
                }
                std::cmp::Ordering::Equal
            });
            self.cursor = 0;
        }
        if self.cursor < self.buffer.len() {
            let t = Rc::clone(&self.buffer[self.cursor].0);
            self.cursor += 1;
            Ok(Some(smallvec![t]))
        } else {
            Ok(None)
        }
    }

    fn explain(&self) -> ExplainNode {
        let mut node = ExplainNode::new("OrderStep");
        let mut children = Vec::new();
        for (i, k) in self.keys.iter().enumerate() {
            if let PhysicalOrderKeySpec::Traversal(sub_plan) = &k.spec {
                children.push((format!("key[{}]", i), sub_plan.explain()));
            }
        }
        if !children.is_empty() {
            node = node.with_children(children);
        }
        node
    }
}
