# Design: Vector Quantization — Memory Optimization

Status: proposal.

---

## Table of Contents

- [Design: Vector Quantization — Memory Optimization](#design-vector-quantization--memory-optimization)
  - [Table of Contents](#table-of-contents)
  - [1. The memory problem](#1-the-memory-problem)
  - [2. Architectural philosophy: Decouple API from memory](#2-architectural-philosophy-decouple-api-from-memory)
  - [3. Three options — optimized by default, exact on request](#3-three-options--optimized-by-default-exact-on-request)
    - [3a. f32 — full-precision float (v0.1, opt-in)](#3a-f32--full-precision-float-v01-opt-in)
    - [3b. f16 — half-precision float (v0.2, **default**)](#3b-f16--half-precision-float-v02-default)
    - [3c. RaBitQ — binary projection (v0.4)](#3c-rabitq--binary-projection-v04)
  - [4. Integration with the index lifecycle](#4-integration-with-the-index-lifecycle)
    - [4a. WAL replay](#4a-wal-replay)
    - [4b. Rebuild](#4b-rebuild)
    - [4c. Snapshot](#4c-snapshot)
    - [4d. Changing quantization](#4d-changing-quantization)
  - [5. User-facing API](#5-user-facing-api)
  - [6. Internal re-ranking — the database owns the raw vectors](#6-internal-re-ranking--the-database-owns-the-raw-vectors)
  - [7. Interaction with other features](#7-interaction-with-other-features)
  - [8. Implementation plan](#8-implementation-plan)

---

## 1. The memory problem

Raw float32 vectors dominate HNSW memory at any scale:

| Vectors × Dim | float32 | % of total |
| ------------- | ------- | ---------: |
| 100K × 1536   | 600 MB  |        93% |
| 1M × 1536     | 6.0 GB  |        95% |
| 10M × 1536    | 60 GB   |        97% |

RocksGraph targets embedded deployment — laptops, small cloud VMs, edge devices.
A 1M-vector corpus shouldn't require a 16 GB machine. Quantization trades a
small recall loss for a massive memory reduction.

The graph store (RocksDB props CF) always stores full-precision `FloatVector`.
Quantization applies only to the in-memory ANN index. If quantization settings
change, the original data is intact for a rebuild.

---

## 2. Architectural philosophy: Decouple API from memory

Before evaluating quantization techniques, it is critical to establish a strict
boundary between the **user interface** and the **internal memory engine**.

**Public API is strictly f32**: the wire protocol, Python/Node.js SDKs, and
RocksDB storage (the `props` CF) only ever accept and return standard 32-bit
floats (`f32`).
- **Universality**: `f32` is the standard floating-point array type across all
  languages (`Float32Array` in JS, `numpy.float32` in Python). Most ML models
  naturally output `f32`.
- **Decoupling**: by forcing the user to submit standard floats, the database can
  transparently upgrade its internal quantization (like switching to RaBitQ) without
  ever breaking the user's application code. The user says "here are my floats,
  search them fast", and RocksGraph handles the internal compression.

**Why f8 was abandoned**: previous designs considered an 8-bit scalar quantization
tier. This was dropped because it sits in an awkward "Pareto valley": it requires
complex metadata tracking (min/max bounds) and handles outliers poorly, while only
offering a 4× compression ratio. The combination of `f16` (free 2× compression) and
`RaBitQ` (massive 32× compression) covers the needs of all users without the
maintenance burden of scalar bounds checking.

---

## 3. Three options — optimized by default, exact on request

| Tier       | Technique                              | Memory (1M × 1536) |         Recall loss vs exact          | Default? | Ships |
| ---------- | -------------------------------------- | :----------------: | :-----------------------------------: | :------: | :---: |
| **f32**    | Full-precision float, no quantization  |       6.0 GB       | 0% (HNSW approximation still applies) |  Opt-in  | v0.1  |
| **f16**    | Half-precision float, usearch built-in |       3.0 GB       |           <0.1% additional            | **Yes**  | v0.2  |
| **RaBitQ** | Binary projection with rotation matrix |       190 MB       |       2–5% (0.5% with re-rank)        |  Opt-in  | v0.4  |

f16 is the default because it halves memory at negligible additional recall cost —
HNSW's inherent ~2–3% approximation dominates regardless of scalar precision.
Users who need the last 0.1% of recall (legal/compliance, scientific benchmarks)
opt into f32. Users with memory constraints opt into RaBitQ.

### 3a. f32 — full-precision float (v0.1, opt-in)

**How it works**: usearch stores vectors as `ScalarKind::F32` (the default).
No quantization code is involved. The graph store already holds f32 vectors;
the ANN index mirrors them exactly.

```rust
VectorIndexConfig {
    quantization: Quantization::F32,  // explicit opt-in
}
// or omit quantization entirely — F16 is the default
```

**Memory**: 1536 × 4 bytes = 6,144 bytes per vector. 1M vectors → 6.0 GB.  
**Distance**: usearch's f32 distance kernel. Baseline performance.  
**Recall**: HNSW approximation (~97–99% at typical M/ef_search). No quantization penalty.

### 3b. f16 — half-precision float (v0.2, **default**)

**How it works**: usearch natively supports `ScalarKind::F16`. When configured,
it stores all vectors as IEEE 754 half-precision floats and uses SIMD-accelerated
f16 distance functions internally. No code in RocksGraph changes beyond passing
a configuration value.

```rust
VectorIndexConfig {
    quantization: Quantization::F16,
    // all other fields unchanged
}
```

**Memory**: 1536 × 2 bytes = 3,072 bytes per vector. 1M vectors → 3.0 GB.  
**Distance**: usearch's f16 SIMD kernel, ~2× faster than f32 on x86_64 (half the memory bandwidth).  
**Recall loss**: < 0.1%. f16 has 10 bits of mantissa — enough for cosine similarity
on normalized embeddings where values are in [−1, 1].  
**Training**: none. Quantization is pointwise.

### 3c. RaBitQ — binary projection (v0.4)

**How it works**: the `QuantizedIndex` middleware wraps any `VectorIndex`
implementation. A rotation matrix `R` (orthogonal, D×D) aligns the vector space.
Each vector is encoded as `sign(R × v)`, packing 32 dimensions into one `u32`.
The index stores bit-packed `Vec<u32>` and uses hardware `popcnt` for distance.

```
┌──────────────────────────────────────────┐
│  QuantizedIndex                          │
│                                          │
│  R: f32[D×D]  ── rotation matrix        │
│                                          │
│  insert(v):                              │
│    compressed = sign(R @ v)  // [D] bits │
│    packed     = pack_32(compressed)      │
│    inner.insert(key, packed)             │
│                                          │
│  search(q):                              │
│    compressed = sign(R @ q)              │
│    packed     = pack_32(compressed)      │
│    inner.search(packed, k × overfetch)   │  ← overfetch
│    candidates = re_rank(raw_vectors, q)  │  ← exact cosine
│    return top_k(candidates, k)           │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│  HNSW (usearch, ScalarKind::F32)         │
│  stores bit-packed u32 vectors as f32    │
│  distance = hardware popcnt              │
└──────────────────────────────────────────┘
```

> **Implementation note**: usearch's `ScalarKind::B1` is pure binary quantization
> applied by usearch internally — it is NOT the same as RaBitQ's bit-packed-after-rotation
> encoding. `QuantizedIndex` passes pre-quantized bit-packed data to usearch using
> `ScalarKind::F32` as the storage type (treating the packed bits as opaque bytes),
> and implements the `popcnt`-based distance function externally. The exact usearch
> API for custom distance functions must be verified against the locked crate version.

**Critical: no blocking SVD.** SVD on a 100K × 1536 matrix is O(N·D²) —
~2.3 × 10¹¹ operations, 30+ seconds single-threaded. Blocking the 100,000th
`TxnSession::commit()` on linear algebra is unacceptable for OLTP.

Instead, training runs asynchronously:

1. **Warm-up phase**: vectors are inserted into the fallback index (f16 HNSW) as
   normal. Queries use the fallback index. No separate training buffer is kept in
   RAM — the fallback index IS the warm-up accumulator, and the props CF holds the
   durable copy of every vector.
2. **Threshold reached**: when `fallback_index.size() >= training_sample_size`,
   a background thread is spawned. It scans the props CF to gather the training
   sample and runs SVD. The fallback index continues serving queries.
3. **Training completes**: the rotation matrix is stored atomically in `__meta` CF.
4. **Atomic upgrade**: on the next `Graph::open` (or immediate swap), the quantized
   HNSW replaces the fallback index.
5. **Small dataset safety**: if the dataset never reaches `training_sample_size`,
   the fallback index (f16) serves queries indefinitely — there is no warm-up trap.

```rust
struct RaBitQState {
    phase:          TrainingPhase,
    fallback_index: Box<dyn VectorIndex>,  // f16 HNSW, serves queries during warm-up
    matrix:         Option<Box<[f32]>>,
}

enum TrainingPhase {
    Collecting,  // fallback_index.size() < training_sample_size
    Training,    // SVD running in background thread; fallback still serves
    Trained,     // rotation matrix ready, awaiting index rebuild
}
```

**Crash recovery**: the training buffer was eliminated because it was purely in RAM
and would be lost on crash. After a crash during `Collecting`:
- The fallback index restores from its snapshot + WAL replay.
- `fallback_index.size()` tells us how many vectors have been indexed.
- If the count is still below `training_sample_size`, training restarts automatically
  without any lost progress — all vectors are already in the fallback index.
- If the count now exceeds the threshold (because WAL replay added more), the
  background training thread is spawned immediately on restart.

Training data is always read from the props CF at training time, not from a
transient buffer. A crash can never cause training to restart from fewer than the
vectors already durably committed.

**Internal re-ranking** — the database hides the quantization penalty from the user.
See §6.

**Recall loss**: raw HNSW via popcnt: 2–5%. After internal re-ranking against raw
f32 vectors: **< 0.5%**.  
**Training**: asynchronous SVD. Never blocks a transaction.  
**Memory**: ~190 MB (bit-packed vectors) + fallback index during warm-up (~3 GB with
f16). Once training completes and the fallback is dropped, only 190 MB is live.

---

## 4. Integration with the index lifecycle

### 4a. WAL replay

WAL entries contain the raw `FloatVector` (full-precision). During replay,
the quantized middleware encodes the vector on the fly before passing to
the inner index. The WAL format does not change — quantization is a
memory optimization, not a storage format change.

### 4b. Rebuild

`rebuild_vector_index()` scans the props CF for `FloatVector` values and
re-inserts them. Each insert goes through the quantization middleware.
The rotation matrix is re-trained from scratch during rebuild (for RaBitQ)
or recomputed from the first N inserts.

### 4c. Snapshot

The HNSW snapshot stores the index's internal vectors in their quantized
form. The rotation matrix is appended as a trailer with a magic number,
size, and CRC-32C. On `load`, the middleware checks for the trailer; if
absent (old snapshot before quantization was enabled), it falls back to
unquantized f32 search and logs a warning.

> **Coordination required**: the base snapshot format is defined in
> `design_hnsw_impl.md §8a`. When RaBitQ is implemented, `format_version`
> must be bumped and the trailer byte layout must be added to that document.
> The trailer is invisible to `load_vector_index` for non-RaBitQ indexes
> (they read only the fixed header and CRC-32C).

### 4d. Changing quantization

Quantization cannot be changed in-place — the internal ANN graph is built
with quantized distances that don't compare meaningfully across encodings.
The user drops the index, re-declares with the new quantization, and
rebuilds:

```python
g.drop_vector_index(entity_type=VectorEntityType.VERTEX, property="embedding")
g.add_vector_index(VectorIndexConfig(
    entity_type        = VectorEntityType.VERTEX,
    property           = "embedding",
    dimension          = 1536,
    metric             = DistanceMetric.COSINE,
    quantization       = Quantization.F16,   # upgrade or change here
))
g.rebuild_vector_index(entity_type=VectorEntityType.VERTEX, property="embedding")
```

---

## 5. User-facing API

The `quantization: Quantization` field is added to `VectorIndexConfig`
(defined in `design_vector_api.md §6a`). Default value: `Quantization::F16`.

```python
from rocksgraph import Quantization, VectorIndexConfig, VectorEntityType, DistanceMetric

# v0.2 — f16 (default, quantization field optional)
g.add_vector_index(VectorIndexConfig(
    entity_type = VectorEntityType.VERTEX,
    property    = "embedding",
    dimension   = 1536,
    metric      = DistanceMetric.COSINE,
    # quantization omitted → defaults to Quantization.F16
))

# v0.1 — f32 (opt-in, maximum recall)
g.add_vector_index(VectorIndexConfig(..., quantization=Quantization.F32))

# v0.4 — RaBitQ (opt-in, minimum memory)
g.add_vector_index(VectorIndexConfig(...,
    quantization=Quantization.RaBitQ(training_sample_size=100_000),
))
```

`VectorIndexStats` (defined in `design_vector_api.md §6e`) gains a `quantization`
field for introspection:

```python
stats = g.vector_index_stats(entity_type=VectorEntityType.VERTEX, property="embedding")
print(stats.quantization)   # Quantization.F16
```

---

## 6. Internal re-ranking — the database owns the raw vectors

Quantized search trades accuracy for memory. But RocksGraph stores the original,
exact `f32` vectors durably in the `props` CF — the database has access to the
ground truth data. It must **never** leak the quantization penalty to the user.
Re-ranking is strictly internal.

The execution pipeline for a `nearest` step on a quantized index:

1. **Overfetch (memory)**: query the quantized HNSW graph for `k × overfetch_factor`
   candidates using the fast hardware distance metric (e.g., `popcnt` for RaBitQ).
2. **Retrieve (disk/cache)**: fetch the exact 32-bit floats for these candidates from
   the RocksDB `props` CF. Recently-accessed vectors are in the block cache; cold
   vertices incur one point lookup per candidate (~10–50 μs each).
3. **Re-rank**: compute exact cosine similarity using the raw `f32` vectors, sort the
   candidates. Re-ranking cost: `k × overfetch × dim` FLOPs — at `k=10, overfetch=10,
   dim=1536`: ~1.5 × 10⁵ FLOPs × ~10 ns/FLOP ≈ 1.5 ms. Return true top `k`.

**Overfetch factor** is auto-tuned by the optimizer: 10 for RaBitQ, 1 for f16 and
f32 (no re-ranking needed). The user never configures this — it is an optimizer
decision visible only in `VectorIndexStats.last_overfetch_factor`.

**Recall after re-ranking**: f16 ~100%, RaBitQ ~99.5%. The quantization penalty is
eliminated at query time.

---

## 7. Interaction with other features

| Feature            |                             f16                             |                   RaBitQ                   |
| ------------------ | :---------------------------------------------------------: | :----------------------------------------: |
| `similarity` |          Works as-is (usearch returns f32 scores)           | Works as-is (popcnt → exact re-rank score) |
| `neighbors`        | Source vector stored f32 in graph, query encoded on the fly |                    Same                    |
| Filtered ANN       |                       No interaction                        |               No interaction               |
| `withEfSearch`     |                       No interaction                        |               No interaction               |
| Bulk load          |           `VectorIndex.insert()` handles encoding           | Goes into fallback index; count check triggers training when threshold hit |

---

## 8. Implementation plan

| Phase | What                                                                                                             |   Effort   | Depends on                              |
| ----- | ---------------------------------------------------------------------------------------------------------------- | :--------: | --------------------------------------- |
| v0.1  | f32 — no quantization (existing code path)                                                                       |     —      | HNSW index impl                         |
| v0.2  | f16 quantization — default — pass `ScalarKind::F16` to usearch construction                                      |  ~5 lines  | HNSW index impl                         |
| v0.4  | RaBitQ — `QuantizedIndex` middleware, async SVD training, popcnt distance, internal re-ranking, snapshot trailer | ~400 lines | f16 shipped, `VectorIndex` trait stable |
