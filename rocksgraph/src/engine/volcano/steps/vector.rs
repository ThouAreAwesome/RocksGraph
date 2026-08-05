// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::engine::volcano::steps::traits::ExplainNode;
use crate::engine::{
    context::GraphCtx,
    traverser::Traverser,
    volcano::steps::traits::{CoreStep, StepRef},
};
use crate::types::PIPELINE_PRODUCE_SIZE;
use crate::types::{
    error::StoreError,
    gvalue::{GValue, Primitive},
    keys::{CanonicalKey, VertexKey},
};
use parking_lot::RwLock;
use smallvec::smallvec;
use smol_str::SmolStr;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::schema::Schema;
use crate::vector::EntityKey;
use crate::vector::VectorEntityType;

#[derive(Debug)]
pub struct NearestStep {
    upstream: Option<StepRef>,
    prop_key: SmolStr,
    query_vec: Vec<f32>,
    k: usize,
    ef_search: Option<usize>,
    buffer: Vec<Rc<Traverser>>,
    cursor: usize,
    drained: bool,
    prop_key_cache: HashMap<SmolStr, u16>,
}

impl NearestStep {
    pub fn new(prop_key: String, query_vec: Vec<f32>, k: usize, ef_search: Option<usize>) -> Self {
        Self {
            upstream: None,
            prop_key: SmolStr::from(prop_key),
            query_vec,
            k,
            ef_search,
            buffer: Vec::new(),
            cursor: 0,
            drained: false,
            prop_key_cache: HashMap::new(),
        }
    }
}

fn resolve_prop_key_id(schema: &Arc<RwLock<Schema>>, cache: &mut HashMap<SmolStr, u16>, name: &SmolStr) -> Option<u16> {
    if let Some(&id) = cache.get(name) {
        return Some(id);
    }
    let guard = schema.read();
    let id = guard.prop_key_id(name)?;
    cache.insert(name.clone(), id);
    Some(id)
}

/// Extract a FloatVector from a traverser: either directly if the value is
/// already a FloatVector, or by looking up the named property from a Vertex/Edge.
fn resolve_vector(t: &Traverser, ctx: &mut dyn GraphCtx, prop_id: Option<u16>) -> Option<Vec<f32>> {
    match &t.value {
        GValue::FloatVector(v) => Some(v.clone()),
        GValue::Scalar(Primitive::FloatVector(v)) => Some(v.clone()),
        GValue::Property(p) => match &p.value {
            Primitive::FloatVector(v) => Some(v.clone()),
            _ => None,
        },
        GValue::Vertex(vk) => {
            let pid = prop_id?;
            match ctx.get_value(&CanonicalKey::Vertex(*vk), pid).ok()? {
                Some(Primitive::FloatVector(v)) => Some(v),
                _ => None,
            }
        }
        GValue::Edge(ek) => {
            let pid = prop_id?;
            match ctx.get_value(&CanonicalKey::Edge(ek.canonical_edge_key()), pid).ok()? {
                Some(Primitive::FloatVector(v)) => Some(v),
                _ => None,
            }
        }
        _ => None,
    }
}

impl CoreStep for NearestStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
        if let Some(u) = &self.upstream {
            u.reset();
        }
        self.buffer.clear();
        self.cursor = 0;
        self.drained = false;
    }
    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }
    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<smallvec::SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let prop_id = resolve_prop_key_id(&ctx.schema(), &mut self.prop_key_cache, &self.prop_key);

        if !self.drained {
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let mut used_index = false;

            // ── HNSW path ──────────────────────────────────────────────
            // Check availability first without holding the guard across
            // the upstream drain (which needs &mut ctx).
            let hnsw_index = ctx.vector_indexes().and_then(|indexes| {
                let guard = indexes.read();
                guard.get(&(VectorEntityType::Vertex, self.prop_key.clone())).cloned()
            });

            if let Some(index) = hnsw_index {
                // Drain upstream to collect the set of vertex IDs the
                // caller filtered on, keeping the original traverser so
                // path history and sacks are preserved.
                let mut allowed: HashMap<VertexKey, Rc<Traverser>> = HashMap::new();
                while let Some(t) = upstream.next(ctx)? {
                    if let GValue::Vertex(vk) = &t.value {
                        allowed.insert(*vk, t);
                    }
                }

                let results = index
                    .read()
                    .search(&self.query_vec, self.k, self.ef_search)
                    .map_err(|e| StoreError::UnsupportedOperation(format!("vector search: {e}")))?;

                let mut candidates: HashMap<VertexKey, (Rc<Traverser>, f32)> = HashMap::new();
                for (ek, dist) in results {
                    if let EntityKey::Vertex(vk) = ek {
                        if let Some(t) = allowed.get(&vk) {
                            let sim = 1.0 - dist;
                            candidates.insert(vk, (Rc::clone(t), sim));
                        }
                    }
                }

                // ── RYOW merge ──────────────────────────────────────────
                // Merge uncommitted pending vector ops from the current
                // transaction so the write is visible within the same session.
                let pending = ctx.vector_pending_ops();
                if !pending.is_empty() {
                    for op in pending {
                        match op {
                            crate::vector::PendingVectorOp::Removed { key, prop_name, .. } => {
                                if prop_name == &self.prop_key {
                                    if let EntityKey::Vertex(vk) = key {
                                        candidates.remove(vk);
                                    }
                                }
                            }
                            crate::vector::PendingVectorOp::Inserted { key, prop_name, vector, .. } => {
                                if prop_name == &self.prop_key {
                                    if let EntityKey::Vertex(vk) = key {
                                        if let Some(t) = allowed.get(vk) {
                                            let sim = crate::vector::cosine_sim(vector, &self.query_vec);
                                            candidates.insert(*vk, (Rc::clone(t), sim));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let mut sorted: Vec<(Rc<Traverser>, f32)> = candidates.into_values().collect();
                sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                self.buffer = sorted.into_iter().take(self.k).map(|(t, _)| t).collect();
                used_index = true;
            }

            // ── Brute-force fallback ───────────────────────────────────
            if !used_index {
                let mut candidates: Vec<(Rc<Traverser>, f32)> = Vec::new();
                while let Some(t) = upstream.next(ctx)? {
                    let vec = resolve_vector(&t, ctx, prop_id);
                    if let Some(v) = vec {
                        candidates.push((t, crate::vector::cosine_sim(&v, &self.query_vec)));
                    }
                }
                candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                self.buffer = candidates.into_iter().take(self.k).map(|(t, _)| t).collect();
            }
            self.cursor = 0;
            self.drained = true;
        }
        if self.cursor < self.buffer.len() {
            let t = Rc::clone(&self.buffer[self.cursor]);
            self.cursor += 1;
            Ok(Some(smallvec![t]))
        } else {
            Ok(None)
        }
    }
    fn explain(&self) -> ExplainNode {
        ExplainNode::new("NearestStep")
    }
}

#[derive(Debug)]
pub struct SimilarityStep {
    upstream: Option<StepRef>,
    prop_key: SmolStr,
    query_vec: Vec<f32>,
    prop_key_cache: HashMap<SmolStr, u16>,
}
impl SimilarityStep {
    pub fn new(prop_key: String, query_vec: Vec<f32>) -> Self {
        Self { upstream: None, prop_key: SmolStr::from(prop_key), query_vec, prop_key_cache: HashMap::new() }
    }
}
impl CoreStep for SimilarityStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }
    fn reset(&mut self) {
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
    ) -> Result<Option<smallvec::SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        let prop_id = resolve_prop_key_id(&ctx.schema(), &mut self.prop_key_cache, &self.prop_key);
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = smallvec::SmallVec::new();
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            match upstream.next(ctx)? {
                Some(t) => {
                    if let Some(v) = resolve_vector(&t, ctx, prop_id) {
                        let sim = crate::vector::cosine_sim(&v, &self.query_vec);
                        batch.push(Rc::new(Traverser::new(GValue::Scalar(Primitive::Float32(sim)))));
                    }
                }
                None => break,
            }
        }
        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }
    fn explain(&self) -> ExplainNode {
        ExplainNode::new("SimilarityStep")
    }
}

#[cfg(test)]
mod vector_e2e_tests {
    use crate::engine::traverser::Traverser;
    use crate::types::gvalue::{GValue, Primitive};

    #[test]
    fn test_floatvector_hash_dedup() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let a = GValue::FloatVector(vec![1.0, 2.0]);
        let b = GValue::FloatVector(vec![1.0, 2.0]);
        assert_eq!(a, b, "FloatVector equality must be bitwise");
        let mut ha = DefaultHasher::new();
        a.hash(&mut ha);
        let mut hb = DefaultHasher::new();
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());
        // NaN == NaN
        let na = GValue::FloatVector(vec![f32::NAN]);
        let nb = GValue::FloatVector(vec![f32::NAN]);
        assert_eq!(na, nb, "NaN == NaN for FloatVector");
    }

    #[test]
    fn test_cosine_sim() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        assert!((crate::vector::cosine_sim(&a, &b) - 0.0).abs() < 1e-6);
        assert!((crate::vector::cosine_sim(&a, &a) - 1.0).abs() < 1e-6);
        assert!((crate::vector::cosine_sim(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_floatvector_traverser_roundtrip() {
        let t = Traverser::new(GValue::FloatVector(vec![0.1, 0.2, 0.3]));
        match &t.value {
            GValue::FloatVector(v) => assert_eq!(v, &vec![0.1, 0.2, 0.3]),
            _ => panic!("Expected FloatVector"),
        }
    }

    #[test]
    fn test_primitive_floatvector_prop_codec_roundtrip() {
        use crate::types::prop_codec::{decode_prop_by_key, encode_props};
        use std::collections::HashMap;
        let mut props = HashMap::new();
        props.insert(1u16, Primitive::FloatVector(vec![1.0, 2.0, 3.0]));
        let blob = encode_props(&props);
        assert!(!blob.is_empty(), "FloatVector must encode to non-empty blob");
        match decode_prop_by_key(&blob, 1) {
            Some(Primitive::FloatVector(v)) => assert_eq!(v, vec![1.0, 2.0, 3.0]),
            other => panic!("Expected FloatVector([1.0, 2.0, 3.0]), got {:?}", other),
        }
    }
}
