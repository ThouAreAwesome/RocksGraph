// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{
    vector::{AnnAlgorithm, DistanceMetric, Quantization, VectorEntityType, VectorIndexConfig},
    Graph, Value,
};

pub(super) fn lcg_vectors(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed;
    let mut next = || {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        ((s >> 33) as f32) / ((1u32 << 31) as f32) - 0.5
    };
    (0..n).map(|_| (0..dim).map(|_| next()).collect()).collect()
}

pub(super) fn exact_top_k(vectors: &[Vec<f32>], query: &[f32], k: usize, metric: DistanceMetric) -> Vec<i64> {
    let mut v: Vec<(usize, f32)> =
        vectors.iter().enumerate().map(|(i, vv)| (i, crate::vector::metric_sim(metric, vv, query))).collect();
    v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    v.into_iter().take(k).map(|(i, _)| (i + 1) as i64).collect()
}

pub(super) fn recall(result_ids: &[i64], exact_ids: &[i64], k: usize) -> f32 {
    use std::collections::BTreeSet;
    let rs: BTreeSet<_> = result_ids.iter().take(k).collect();
    let es: BTreeSet<_> = exact_ids.iter().take(k).collect();
    rs.intersection(&es).count() as f32 / k as f32
}

pub(super) fn build_vector_graph(
    path: &std::path::Path,
    vectors: &[Vec<f32>],
    metric: DistanceMetric,
    quantization: Quantization,
    with_hnsw: bool,
) {
    let g = Graph::open(path).unwrap();
    if with_hnsw {
        let mut s = g.open_schema();
        s.add_vector_index(VectorIndexConfig {
            property: "emb".into(),
            entity_type: VectorEntityType::Vertex,
            dimension: if vectors.is_empty() { 1 } else { vectors[0].len() },
            metric,
            algorithm: AnnAlgorithm::Hnsw(Default::default()),
            quantization,
        });
        s.commit().unwrap();
    }
    let mut t = g.begin();
    for (i, vec) in vectors.iter().enumerate() {
        t.g()
            .addV("doc")
            .property("id", (i + 1) as i64)
            .property("emb", Value::FloatVector(vec.clone()))
            .next()
            .unwrap();
    }
    t.commit().unwrap();
    if with_hnsw {
        g.index_manager().rebuild(VectorEntityType::Vertex, "emb").unwrap();
    }
    g.close().unwrap();
}
