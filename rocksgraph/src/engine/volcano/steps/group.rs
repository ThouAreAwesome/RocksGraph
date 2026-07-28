// SPDX-License-Identifier: MIT OR Apache-2.0

// Physical steps: group(), groupCount()

use crate::engine::volcano::steps::traits::ExplainNode;
use crate::types::PIPELINE_PRODUCE_SIZE;
use crate::{
    engine::{
        context::GraphCtx,
        traverser::Traverser,
        volcano::steps::traits::{CoreStep, StepRef},
    },
    types::{error::StoreError, gvalue::GValue, CanonicalKey},
};
use smallvec::{smallvec, SmallVec};
use std::rc::Rc;

/// Collects all traversers and groups them into a Map.
/// If `key` is set, groups by the named property value instead of the traverser value.
#[derive(Debug)]
pub struct GroupStep {
    upstream: Option<StepRef>,
    done: bool,
    key: Option<u16>, // property key ID for group().by("key")
}

impl GroupStep {
    pub fn new(key: Option<u16>) -> Self {
        Self { upstream: None, done: false, key }
    }
}

impl CoreStep for GroupStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        self.done = false;
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
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut groups: Vec<(GValue, Vec<GValue>)> = Vec::new();
        while let Some(t) = upstream.next(ctx)? {
            let key = if let Some(prop_key_id) = self.key {
                let canonical_key = match &t.value {
                    GValue::Vertex(vt) => Some(CanonicalKey::Vertex(*vt)),
                    GValue::Edge(eg) => Some(CanonicalKey::Edge(eg.canonical_edge_key())),
                    _ => None,
                };
                match canonical_key.and_then(|ck| ctx.get_value(&ck, prop_key_id).transpose()) {
                    Some(Ok(prim)) => GValue::Scalar(prim),
                    _ => continue,
                }
            } else {
                t.value.clone()
            };
            if let Some((_, list)) = groups.iter_mut().find(|(k, _)| k == &key) {
                list.push(t.value.clone());
            } else {
                groups.push((key, vec![t.value.clone()]));
            }
        }
        self.done = true;
        let map = GValue::Map(groups.into_iter().map(|(k, v)| (k, GValue::List(v))).collect());
        Ok(Some(smallvec![Traverser::new_rc(map)]))
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("GroupStep")
    }
}

/// Collects all traversers and counts occurrences per value.
/// If `key` is set, counts by the named property value instead of the traverser value.
#[derive(Debug)]
pub struct GroupCountStep {
    upstream: Option<StepRef>,
    done: bool,
    key: Option<u16>,
}

impl GroupCountStep {
    pub fn new(key: Option<u16>) -> Self {
        Self { upstream: None, done: false, key }
    }
}

impl CoreStep for GroupCountStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        self.done = false;
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
        if self.done {
            return Ok(None);
        }
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut counts: Vec<(GValue, i64)> = Vec::new();
        while let Some(t) = upstream.next(ctx)? {
            let key = if let Some(prop_key_id) = self.key {
                let canonical_key = match &t.value {
                    GValue::Vertex(vt) => Some(CanonicalKey::Vertex(*vt)),
                    GValue::Edge(eg) => Some(CanonicalKey::Edge(eg.canonical_edge_key())),
                    _ => None,
                };
                match canonical_key.and_then(|ck| ctx.get_value(&ck, prop_key_id).transpose()) {
                    Some(Ok(prim)) => GValue::Scalar(prim),
                    _ => continue,
                }
            } else {
                t.value.clone()
            };
            if let Some((_, cnt)) = counts.iter_mut().find(|(k, _)| k == &key) {
                *cnt += 1;
            } else {
                counts.push((key, 1));
            }
        }
        self.done = true;
        use crate::types::gvalue::Primitive;
        let map = GValue::Map(counts.into_iter().map(|(k, v)| (k, GValue::Scalar(Primitive::Int64(v)))).collect());
        Ok(Some(smallvec![Traverser::new_rc(map)]))
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("GroupCountStep")
    }
}
