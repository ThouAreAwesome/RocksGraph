// Physical step: order()

use crate::types::PIPELINE_BATCH_INLINE;
use std::rc::Rc;
use smallvec::{smallvec, SmallVec};
use crate::{
    engine::{context::GraphCtx, traverser::Traverser, volcano::steps::traits::{CoreStep, StepRef}},
    planner::logical_step::{Order, OrderKey, OrderKeySpec},
    types::{error::StoreError, gvalue::{GValue, Primitive}, ORDER_KEY_INLINE},
};

/// Sorts all upstream traversers and emits them in order.
#[derive(Debug)]
pub struct OrderStep {
    upstream: Option<StepRef>,
    keys: SmallVec<[OrderKey; ORDER_KEY_INLINE]>,
    buffer: Vec<(Rc<Traverser>, SmallVec<[Primitive; ORDER_KEY_INLINE]>)>,
    cursor: usize,
    drained: bool,
}

impl OrderStep {
    pub fn new(keys: SmallVec<[OrderKey; ORDER_KEY_INLINE]>) -> Self {
        Self { upstream: None, keys, buffer: Vec::new(), cursor: 0, drained: false }
    }
}

/// Extract a comparison key from a traverser value. Returns None for non-comparable values.
fn extract_key(value: &GValue, spec: &OrderKeySpec) -> Option<Primitive> {
    match spec {
        OrderKeySpec::Value => match value {
            GValue::Scalar(p) => Some(p.clone()),
            GValue::Vertex(v) => Some(Primitive::Int64(*v)),
            _ => None,
        },
        OrderKeySpec::Property(_prop_name) => {
            // Property-based ordering requires schema resolution at build time.
            // For now, treat the value as-is.
            extract_key(value, &OrderKeySpec::Value)
        }
    }
}

impl CoreStep for OrderStep {
    fn add_upper(&mut self, upstream: StepRef) { self.upstream = Some(upstream); }
    fn reset(&mut self) { self.buffer.clear(); self.cursor = 0; self.drained = false; if let Some(u) = &self.upstream { u.reset(); } }
    fn upper(&self) -> Option<StepRef> { self.upstream.clone() }

    fn produce(&mut self, ctx: &mut dyn GraphCtx) -> Result<Option<SmallVec<[Rc<Traverser>; PIPELINE_BATCH_INLINE]>>, StoreError> {
        if !self.drained {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            while let Some(t) = upstream.next(ctx)? {
                let key_values: SmallVec<[Primitive; ORDER_KEY_INLINE]> = self.keys.iter().map(|k| {
                    extract_key(&t.value, &k.spec).unwrap_or(Primitive::Null)
                }).collect();
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
}
