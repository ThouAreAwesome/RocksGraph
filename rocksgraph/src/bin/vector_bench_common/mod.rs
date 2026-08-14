// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Shared synthetic vector generator for the vector-index-overhead benchmarks
//! (`bench_vector_bulk_load`, `bench_write_occ --vector-dim`). See
//! `docs/design/vector-search/design_vector_benchmarks.md` for why this
//! generates i.i.d. Gaussian, L2-normalized vectors rather than real
//! embeddings: dimension and shape are what matter for measuring RocksGraph's
//! own mechanism cost, not semantic content.

use rand::Rng;

/// Draws `dim` i.i.d. standard-normal components (Box-Muller) and L2-normalizes
/// the result to unit length — the standard construction for a uniformly
/// distributed point on the unit hypersphere, matching the output convention
/// of real embedding models (sentence-transformers, OpenAI, BERT-derived).
pub fn random_normal_vector(dim: usize) -> Vec<f32> {
    let mut rng = rand::thread_rng();
    let mut vec = Vec::with_capacity(dim);
    let mut i = 0;
    while i < dim {
        let u1: f32 = rng.gen_range(0.000001f32..1.0);
        let u2: f32 = rng.gen_range(0.0f32..1.0);
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * std::f32::consts::PI * u2;
        let z0 = r * theta.cos();
        let z1 = r * theta.sin();
        vec.push(z0);
        if i + 1 < dim {
            vec.push(z1);
        }
        i += 2;
    }

    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut vec {
            *x /= norm;
        }
    }
    vec
}
