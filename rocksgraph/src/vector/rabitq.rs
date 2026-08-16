#![allow(dead_code)]
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::cell::RefCell;

thread_local! {
    /// Thread-local storage for passing the exact, rotated query vector
    /// to the usearch symmetric distance metric closure.
    pub static CURRENT_QUERY: RefCell<Option<Vec<f32>>> = const { RefCell::new(None) };
}

pub struct RaBitQTransform {
    metric: crate::vector::traits::DistanceMetric,
    dim: usize,
    pad_dim: usize,
    /// Random sign vector of size pad_dim
    signs: Vec<f32>,
}

fn next_power_of_two(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    n -= 1;
    n |= n >> 1;
    n |= n >> 2;
    n |= n >> 4;
    n |= n >> 8;
    n |= n >> 16;
    n |= n >> 32;
    n + 1
}

/// In-place Fast Walsh-Hadamard Transform
fn fwht(a: &mut [f32]) {
    let mut h = 1;
    while h < a.len() {
        for i in (0..a.len()).step_by(h * 2) {
            for j in i..i + h {
                let x = a[j];
                let y = a[j + h];
                a[j] = x + y;
                a[j + h] = x - y;
            }
        }
        h *= 2;
    }
}

impl RaBitQTransform {
    pub fn new(dim: usize, seed: u64, metric: crate::vector::traits::DistanceMetric) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let pad_dim = next_power_of_two(dim);
        let mut signs = Vec::with_capacity(pad_dim);
        for _ in 0..pad_dim {
            signs.push(if rng.gen_bool(0.5) { 1.0 } else { -1.0 });
        }

        Self { dim, pad_dim, signs, metric }
    }

    pub fn pad_dim(&self) -> usize {
        self.pad_dim
    }

    pub fn rotate(&self, v: &[f32]) -> Vec<f32> {
        assert_eq!(v.len(), self.dim);
        let mut padded = vec![0.0; self.pad_dim];
        for i in 0..self.dim {
            padded[i] = v[i] * self.signs[i];
        }
        fwht(&mut padded);
        let scale = 1.0 / (self.pad_dim as f32).sqrt();
        for x in &mut padded {
            *x *= scale;
        }
        padded
    }
    pub fn transform_and_pack(&self, v: &[f32]) -> Vec<u8> {
        let mut rotated = self.rotate(v);
        
        let t = if self.metric == crate::vector::traits::DistanceMetric::Cosine {
            let norm = rotated.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                rotated.iter_mut().for_each(|x| *x /= norm);
            }
            // L2 norm of normalized vector is 1.0, so t is L1 norm / dim
            rotated.iter().map(|x| x.abs()).sum::<f32>() / (rotated.len() as f32)
        } else {
            rescale_factor(v, &rotated)
        };
        
        let norm = if self.metric == crate::vector::traits::DistanceMetric::Euclidean {
            v.iter().map(|&x| x * x).sum::<f32>()
        } else {
            1.0
        };
        
        pack_bits_with_t_and_norm(&rotated, t, norm)
    }
}

pub fn rescale_factor(_original: &[f32], rotated: &[f32]) -> f32 {
    let mut l1_norm = 0.0;
    for &x in rotated {
        l1_norm += x.abs();
    }
    let dim = rotated.len() as f32;
    l1_norm / dim
}

pub fn pack_bits_with_t_and_norm(rotated: &[f32], t: f32, norm: f32) -> Vec<u8> {
    let dim = rotated.len();
    let bits_len = dim.div_ceil(8);
    let mut buf = vec![0u8; bits_len + 8];

    for i in 0..dim {
        if rotated[i] > 0.0 {
            buf[i / 8] |= 1 << (i % 8);
        }
    }

    buf[bits_len..bits_len + 4].copy_from_slice(&t.to_le_bytes());
    buf[bits_len + 4..bits_len + 8].copy_from_slice(&norm.to_le_bytes());
    buf
}

pub fn create_rabitq_metric(
    pad_dim: usize,
    metric: crate::vector::traits::DistanceMetric,
) -> Box<dyn Fn(*const usearch::b1x8, *const usearch::b1x8) -> usearch::Distance + Send + Sync> {
    let bits_len = pad_dim.div_ceil(8);
    Box::new(move |a: *const usearch::b1x8, b: *const usearch::b1x8| -> usearch::Distance {
        // SAFETY: The a and b pointers are provided by usearch and point to valid bit buffers.
        unsafe {
            let b_ptr = b as *const u8;
            let b_slice = std::slice::from_raw_parts(b_ptr, bits_len + 8);

            let distance = CURRENT_QUERY.with(|query_opt| {
                if let Some(q_rotated) = &*query_opt.borrow() {
                    let a_ptr = a as *const u8;
                    let a_slice = std::slice::from_raw_parts(a_ptr, bits_len + 8);
                    let is_a_dummy = a_slice.iter().all(|&x| x == 0);
                    let node_slice = if is_a_dummy { b_slice } else { a_slice };

                    let mut dot = 0.0;
                    let mut q_sq_norm = 0.0;
                    for i in 0..pad_dim {
                        let bit = (node_slice[i / 8] >> (i % 8)) & 1;
                        let sign = if bit == 1 { 1.0 } else { -1.0 };
                        let qi = q_rotated[i];
                        dot += sign * qi;
                        q_sq_norm += qi * qi;
                    }

                    let mut t_bytes = [0u8; 4];
                    t_bytes.copy_from_slice(&node_slice[bits_len..bits_len + 4]);
                    let t_node = f32::from_le_bytes(t_bytes);
                    let t_node = if t_node == 0.0 { 1.0 } else { t_node };

                    let mut norm_bytes = [0u8; 4];
                    norm_bytes.copy_from_slice(&node_slice[bits_len + 4..bits_len + 8]);
                    let norm_node = f32::from_le_bytes(norm_bytes);

                    // Inner product estimator: c * sum(sign(x) * q)
                    let mut ip = dot * t_node;

                    match metric {
                        crate::vector::traits::DistanceMetric::Euclidean => {
                            (q_sq_norm + norm_node - 2.0 * ip).max(0.0) as usearch::Distance
                        }
                        crate::vector::traits::DistanceMetric::Cosine
                        | crate::vector::traits::DistanceMetric::DotProduct => {
                            // For DotProduct, usearch IP metric natively expects `1.0 - ip`.
                            // For Cosine, q and node are unit-normalized, so ip is cosine similarity.
                            if q_sq_norm > 0.0 && metric != crate::vector::traits::DistanceMetric::Cosine {
                                ip /= q_sq_norm.sqrt();
                            }
                            (1.0 - ip).max(0.0) as usearch::Distance
                        }
                    }
                } else {
                    let a_ptr = a as *const u8;
                    let b_ptr = b as *const u8;
                    let mut hamming = 0;
                    let a_slice = std::slice::from_raw_parts(a_ptr, bits_len + 8);
                    let b_slice = std::slice::from_raw_parts(b_ptr, bits_len + 8);
                    let mut i = 0;
                    while i + 8 <= bits_len {
                        let mut a_chunk = [0u8; 8];
                        let mut b_chunk = [0u8; 8];
                        a_chunk.copy_from_slice(&a_slice[i..i + 8]);
                        b_chunk.copy_from_slice(&b_slice[i..i + 8]);
                        let a_u64 = u64::from_le_bytes(a_chunk);
                        let b_u64 = u64::from_le_bytes(b_chunk);
                        hamming += (a_u64 ^ b_u64).count_ones();
                        i += 8;
                    }
                    while i < bits_len {
                        hamming += (a_slice[i] ^ b_slice[i]).count_ones();
                        i += 1;
                    }
                    let mut t_a_bytes = [0u8; 4];
                    t_a_bytes.copy_from_slice(&a_slice[bits_len..bits_len + 4]);
                    let t_a = f32::from_le_bytes(t_a_bytes);

                    let mut t_b_bytes = [0u8; 4];
                    t_b_bytes.copy_from_slice(&b_slice[bits_len..bits_len + 4]);
                    let t_b = f32::from_le_bytes(t_b_bytes);

                    let mut norm_a_bytes = [0u8; 4];
                    norm_a_bytes.copy_from_slice(&a_slice[bits_len + 4..bits_len + 8]);
                    let norm_a = f32::from_le_bytes(norm_a_bytes);

                    let mut norm_b_bytes = [0u8; 4];
                    norm_b_bytes.copy_from_slice(&b_slice[bits_len + 4..bits_len + 8]);
                    let norm_b = f32::from_le_bytes(norm_b_bytes);

                    let ip = t_a * t_b * (pad_dim as f32 - 2.0 * hamming as f32);

                    match metric {
                        crate::vector::traits::DistanceMetric::Euclidean => {
                            (norm_a + norm_b - 2.0 * ip).max(0.0) as usearch::Distance
                        }
                        crate::vector::traits::DistanceMetric::Cosine
                        | crate::vector::traits::DistanceMetric::DotProduct => {
                            (1.0 - ip).max(0.0) as usearch::Distance
                        }
                    }
                }
            });
            distance
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rabitq_transform_and_pack() {
        let dim = 1024; // Already power of 2
        let transform = RaBitQTransform::new(dim, 42, crate::vector::traits::DistanceMetric::Euclidean);

        let mut v = vec![0.0; dim];
        v[0] = 1.0;
        v[10] = -1.0;

        let rotated = transform.rotate(&v);
        assert_eq!(rotated.len(), dim);

        let t = rescale_factor(&v, &rotated);
        assert!(t > 0.0);

        let packed = pack_bits_with_t_and_norm(&rotated, t, 1.0);

        // Size should be: dim / 8 + 4
        let expected_len = dim.div_ceil(8) + 8;
        assert_eq!(packed.len(), expected_len);

        // Recover t from the trailing bytes
        let mut t_bytes = [0u8; 4];
        t_bytes.copy_from_slice(&packed[expected_len - 4..]);
        let recovered_t = f32::from_le_bytes(t_bytes);
        assert_eq!(recovered_t, t);
    }
}
