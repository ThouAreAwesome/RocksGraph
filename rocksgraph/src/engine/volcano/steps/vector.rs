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
    keys::CanonicalKey,
};
use parking_lot::RwLock;
use smallvec::smallvec;
use smol_str::SmolStr;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::schema::Schema;
use crate::vector::DistanceMetric;
use crate::vector::EntityKey;
use crate::vector::VectorEntityType;

#[derive(Debug)]
pub struct NearestStep {
    upstream: Option<StepRef>,
    prop_key: SmolStr,
    query_vec: Vec<f32>,
    k: usize,
    ef_search: Option<usize>,
    metric_override: Option<DistanceMetric>,
    buffer: Vec<Rc<Traverser>>,
    cursor: usize,
    drained: bool,
    prop_key_cache: HashMap<SmolStr, u16>,
}

impl NearestStep {
    pub fn new(
        prop_key: String,
        query_vec: Vec<f32>,
        k: usize,
        ef_search: Option<usize>,
        metric_override: Option<DistanceMetric>,
    ) -> Self {
        Self {
            upstream: None,
            prop_key: SmolStr::from(prop_key),
            query_vec,
            k,
            ef_search,
            metric_override,
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
            if self.k == 0 {
                self.drained = true;
                return Ok(None);
            }
            let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
            let mut used_index = false;

            // Drain the first traverser to infer entity type for index selection.
            // If the upstream stream is empty (`first` is None), `inferred_entity_type` defaults
            // to Vertex; the candidate collection loop will simply find 0 items and cleanly produce None.
            // TODO(mixed-streams): entity type is inferred from the first traverser only.
            // A mixed vertex/edge stream would need per-element dispatch to select the right index.
            let first = upstream.next(ctx)?;
            let inferred_entity_type = first
                .as_ref()
                .map(|t| match &t.value {
                    GValue::Vertex(_) => VectorEntityType::Vertex,
                    GValue::Edge(_) => VectorEntityType::Edge,
                    _ => VectorEntityType::Vertex,
                })
                .unwrap_or(VectorEntityType::Vertex);

            // ── HNSW path ──────────────────────────────────────────────
            let hnsw_index = ctx.vector_indexes().and_then(|indexes| {
                let guard = indexes.read();
                guard.get(&(inferred_entity_type, self.prop_key.clone())).cloned()
            });

            if let Some(index) = hnsw_index {
                // TODO(perf/collect-all): We unconditionally drain the entire upstream into
                // `allowed` before searching. For an unfiltered .V([]).nearest(...) on a 100M
                // vertex graph this can consume hundreds of MB. Two future optimizations:
                //   1. When the planner can prove the upstream is a full entity scan with no
                //      filters, skip collection entirely and post-filter index results only
                //      against RYOW pending ops.
                //   2. When upstream is a bounded id-set (.V([1, 2, 3])), keep the current
                //      approach — the allowed set is small and the index results are filtered
                //      down to it.
                // Both require planner cooperation to signal "unfiltered scan" vs "id-set".
                let mut allowed: HashMap<EntityKey, Rc<Traverser>> = HashMap::new();
                if let Some(ref t) = first {
                    match &t.value {
                        GValue::Vertex(vk) => {
                            allowed.insert(EntityKey::Vertex(*vk), Rc::clone(t));
                        }
                        GValue::Edge(ek) => {
                            allowed.insert(EntityKey::Edge(ek.canonical_edge_key()), Rc::clone(t));
                        }
                        _ => {}
                    }
                }
                while let Some(t) = upstream.next(ctx)? {
                    match &t.value {
                        GValue::Vertex(vk) => {
                            allowed.insert(EntityKey::Vertex(*vk), Rc::clone(&t));
                        }
                        GValue::Edge(ek) => {
                            allowed.insert(EntityKey::Edge(ek.canonical_edge_key()), Rc::clone(&t));
                        }
                        _ => {}
                    }
                }

                // TODO(perf): ef_search is fixed at step-construction time (or uses the index default).
                // A future improvement could choose ef_search adaptively — e.g. scale it with k,
                // or expose a per-query recall target that the step translates to an ef_search value.
                let (results, index_metric) = {
                    let guard = index.read();
                    let results = guard
                        .search(&self.query_vec, self.k, self.ef_search)
                        .map_err(|e| StoreError::UnsupportedOperation(format!("vector search: {e}")))?;
                    let m = guard.metric();
                    (results, m)
                };
                let metric = self.metric_override.unwrap_or(index_metric);
                let mut candidates: HashMap<EntityKey, (Rc<Traverser>, f32)> = HashMap::new();
                for (ek, dist) in results {
                    if let Some(t) = allowed.get(&ek) {
                        let sim = if self.metric_override.is_some() && self.metric_override != Some(index_metric) {
                            if let Some(v) = resolve_vector(t, ctx, prop_id) {
                                crate::vector::metric_sim(metric, &v, &self.query_vec)
                            } else {
                                crate::vector::dist_to_sim(index_metric, dist)
                            }
                        } else {
                            crate::vector::dist_to_sim(index_metric, dist)
                        };
                        candidates.insert(ek, (Rc::clone(t), sim));
                    }
                }

                // ── RYOW merge ──────────────────────────────────────────
                let pending = ctx.vector_pending_ops();
                if !pending.is_empty() {
                    for op in pending {
                        match op {
                            crate::vector::PendingVectorOp::Removed { key, prop_name, .. } => {
                                if prop_name == &self.prop_key {
                                    candidates.remove(key);
                                }
                            }
                            crate::vector::PendingVectorOp::Inserted { key, prop_name, vector, .. } => {
                                if prop_name == &self.prop_key {
                                    if let Some(t) = allowed.get(key) {
                                        let sim = crate::vector::metric_sim(metric, vector, &self.query_vec);
                                        candidates.insert(key.clone(), (Rc::clone(t), sim));
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
                let metric = self.metric_override.unwrap_or_default();
                let mut candidates: Vec<(Rc<Traverser>, f32)> = Vec::new();
                // Include the first traverser consumed for entity-type inference.
                if let Some(t) = first {
                    if let Some(v) = resolve_vector(&t, ctx, prop_id) {
                        candidates.push((t, crate::vector::metric_sim(metric, &v, &self.query_vec)));
                    }
                }
                while let Some(t) = upstream.next(ctx)? {
                    if let Some(v) = resolve_vector(&t, ctx, prop_id) {
                        candidates.push((t, crate::vector::metric_sim(metric, &v, &self.query_vec)));
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
    metric: DistanceMetric,
    prop_key_cache: HashMap<SmolStr, u16>,
}
impl SimilarityStep {
    pub fn new(prop_key: String, query_vec: Vec<f32>, metric: DistanceMetric) -> Self {
        Self { upstream: None, prop_key: SmolStr::from(prop_key), query_vec, metric, prop_key_cache: HashMap::new() }
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
        let metric = self.metric;
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };
        let mut batch = smallvec::SmallVec::new();
        while batch.len() < PIPELINE_PRODUCE_SIZE {
            match upstream.next(ctx)? {
                Some(t) => {
                    if let Some(v) = resolve_vector(&t, ctx, prop_id) {
                        let sim = crate::vector::metric_sim(metric, &v, &self.query_vec);
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

#[derive(Debug)]
pub struct NeighborsStep {
    upstream: Option<StepRef>,
    /// Property on the incoming traverser to read as the query vector.
    source_prop: SmolStr,
    /// Property name of the declared vector index to search.
    target_prop: SmolStr,
    /// Which entity type's index to search.
    entity_type: VectorEntityType,
    k: usize,
    ef_search: Option<usize>,
    source_prop_cache: HashMap<SmolStr, u16>,
    buffer: Vec<Rc<Traverser>>,
    cursor: usize,
}

impl NeighborsStep {
    pub fn new(
        source_prop: String,
        target_prop: String,
        k: usize,
        entity_type: VectorEntityType,
        ef_search: Option<usize>,
    ) -> Self {
        Self {
            upstream: None,
            source_prop: SmolStr::from(source_prop),
            target_prop: SmolStr::from(target_prop),
            entity_type,
            k,
            ef_search,
            source_prop_cache: HashMap::new(),
            buffer: Vec::new(),
            cursor: 0,
        }
    }
}

impl CoreStep for NeighborsStep {
    fn add_upper(&mut self, upstream: StepRef) {
        self.upstream = Some(upstream);
    }

    fn reset(&mut self) {
        if let Some(u) = &self.upstream {
            u.reset();
        }
        self.buffer.clear();
        self.cursor = 0;
    }

    fn upper(&self) -> Option<StepRef> {
        self.upstream.clone()
    }

    fn produce(
        &mut self,
        ctx: &mut dyn GraphCtx,
    ) -> Result<Option<smallvec::SmallVec<[Rc<Traverser>; PIPELINE_PRODUCE_SIZE]>>, StoreError> {
        if self.k == 0 {
            return Ok(None);
        }
        let prop_id = resolve_prop_key_id(&ctx.schema(), &mut self.source_prop_cache, &self.source_prop);
        let Some(upstream) = self.upstream.as_ref() else { return Ok(None) };

        loop {
            // Return buffered results before fetching the next upstream traverser.
            if self.cursor < self.buffer.len() {
                let t = Rc::clone(&self.buffer[self.cursor]);
                self.cursor += 1;
                return Ok(Some(smallvec![t]));
            }

            self.buffer.clear();
            self.cursor = 0;

            let Some(t) = upstream.next(ctx)? else { return Ok(None) };

            let Some(query_vec) = resolve_vector(&t, ctx, prop_id) else {
                continue; // traverser has no embedding for source_prop — skip silently
            };

            let hnsw_index = ctx.vector_indexes().and_then(|indexes| {
                let guard = indexes.read();
                guard.get(&(self.entity_type, self.target_prop.clone())).cloned()
            });

            let Some(index) = hnsw_index else {
                // TODO(accuracy): brute-force fallback would require scanning all entities,
                // which GraphCtx does not currently expose. Require an index for now.
                return Err(StoreError::UnsupportedOperation(
                    "neighbors() requires a configured HNSW vector index; \
                     brute-force fallback over the full graph is not yet implemented"
                        .into(),
                ));
            };

            let (results, metric) = {
                let guard = index.read();
                let r = guard
                    .search(&query_vec, self.k, self.ef_search)
                    .map_err(|e| StoreError::UnsupportedOperation(format!("vector search: {e}")))?;
                let m = guard.metric();
                (r, m)
            };

            let mut candidates: Vec<(EntityKey, f32)> =
                results.into_iter().map(|(ek, dist)| (ek, crate::vector::dist_to_sim(metric, dist))).collect();

            // RYOW merge: apply uncommitted writes from the current transaction.
            let pending = ctx.vector_pending_ops();
            if !pending.is_empty() {
                for op in pending {
                    match op {
                        crate::vector::PendingVectorOp::Removed { key, prop_name, .. } => {
                            if prop_name == &self.target_prop {
                                candidates.retain(|(k, _)| k != key);
                            }
                        }
                        crate::vector::PendingVectorOp::Inserted { key, prop_name, vector, .. } => {
                            if prop_name == &self.target_prop {
                                let sim = crate::vector::metric_sim(metric, vector, &query_vec);
                                // Overwrite: update the similarity if this entity was already
                                // returned by HNSW but has a newer uncommitted vector.
                                if let Some(pos) = candidates.iter().position(|(k, _)| k == key) {
                                    candidates[pos] = (key.clone(), sim);
                                } else {
                                    candidates.push((key.clone(), sim));
                                }
                            }
                        }
                    }
                }
            }

            candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            self.buffer = candidates
                .into_iter()
                .take(self.k)
                .map(|(ek, _)| {
                    let val = match ek {
                        EntityKey::Vertex(vk) => GValue::Vertex(vk),
                        EntityKey::Edge(cek) => GValue::Edge(cek.out_key()),
                    };
                    Rc::new(Traverser::new(val))
                })
                .collect();
            // Loop back to return from the now-filled buffer.
        }
    }

    fn explain(&self) -> ExplainNode {
        ExplainNode::new("NeighborsStep")
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
