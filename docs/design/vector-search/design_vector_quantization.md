# Design: Vector Quantization — Memory and Build-Speed Optimization

Status: proposal.

> **Revised 2026-08-16**: §3c originally described RaBitQ as requiring an
> SVD-computed rotation matrix, async background training, and a warm-up
> fallback index — modeled on how PQ/OPQ-style *learned* rotations work.
> Checked against the actual algorithm (Gao & Long, SIGMOD 2024): RaBitQ's
> rotation is fixed and data-independent (a Johnson-Lindenstrauss transform),
> not learned from a data sample. There is no training phase, no SVD, no
> warm-up state machine. §3c below is rewritten accordingly — it's
> substantially simpler than the original version of this document described.
> Also added: §3d, addressing whether a RaBitQ layer could speed up index
> *building*/*updating*, not just reduce memory — the answer is yes, with a
> real mechanism and a real risk, both explained there.

> **Revised 2026-08-16 (recall investigation)**: v1 was implemented and
> measured raw (no re-rank) recall around 40% on a synthetic 768-dim
> benchmark — far below both the literature's cited 60-80% and this
> project's usual 90% bar. Before accepting "RaBitQ is just lossy," this was
> run down empirically rather than assumed:
> - **The estimator formula and rotation are not the cause.** Numerically
>   calibrated the closure's distance formula against the actual RaBitQ
>   paper's reference implementation (verified via source, not just
>   description — see §3c), and separately A/B'd the FWHT+sign-flip rotation
>   against a properly Gram-Schmidt-orthogonalized dense rotation on
>   identical data: both gave statistically identical correlation with true
>   similarity (0.835 vs 0.836) and recall (40.4% vs 40.6%). Neither the
>   math nor the rotation implementation is the bottleneck.
> - **Construction-time quantization is a real, sourced concern, but on its
>   own it's not confirmed to be sufficient.** Production HNSW+RaBitQ
>   implementations (the RaBitQ paper authors' own library, per multiple
>   independent sources) build the graph using full-precision vectors and
>   only quantize the final stored/searched representation — RocksGraph's v1
>   does the opposite (§3d). Reported recall for that approach is ~97% at
>   k=10, a gap from our ~40% too large to attribute to formula/rotation
>   alone. **But**: fed the quantized estimator a *perfect* candidate pool
>   directly — literally the true top-N by exact similarity, simulating what
>   flawless full-precision graph construction would hand to the final
>   ranking step — and recall still only reached 40-58% for any realistic
>   pool size (100+), converging back to the ~40% brute-force baseline by
>   pool_size=200. **The final ranking step, using the quantized estimator
>   alone, is a bottleneck independent of candidate discovery quality.**
>   Fixing construction is likely necessary but not sufficient by itself —
>   see §3d for the revised framing and the two most likely reconciling
>   explanations (real embeddings vs. this synthetic i.i.d. benchmark; and
>   production systems likely combining construction fixes *with* some form
>   of exact final-step comparison, not treating them as alternatives).
> - **Re-ranking (§6) is the one lever with direct, unconditional evidence.**
>   Simulated overfetch+rerank directly against RocksGraph's own estimator:
>   recall climbed 40%→55%→72%→89%→100% at 1x/2x/4x/10x/50x overfetch,
>   smoothly and with no ceiling — this doesn't depend on fixing construction
>   or on assumptions about real-embedding structure. Milvus's own published
>   production numbers for RaBitQ refinement show a similar shape: ~4% QPS
>   cost to go from 76% to 94.7% recall. Given the candidate-pool result
>   above, re-ranking should be treated as the primary path to a usable v1,
>   not a fallback co-equal with the construction-time investigation.

---

## Table of Contents

- [Design: Vector Quantization — Memory and Build-Speed Optimization](#design-vector-quantization--memory-and-build-speed-optimization)
  - [Table of Contents](#table-of-contents)
  - [1. The memory problem](#1-the-memory-problem)
  - [2. Architectural philosophy: Decouple API from memory](#2-architectural-philosophy-decouple-api-from-memory)
  - [3. Three options — optimized by default, exact on request](#3-three-options--optimized-by-default-exact-on-request)
    - [3a. f32 — full-precision float (v0.1, opt-in)](#3a-f32--full-precision-float-v01-opt-in)
    - [3b. f16 — half-precision float (v0.2, **default**)](#3b-f16--half-precision-float-v02-default)
    - [3c. RaBitQ — binary projection with fixed random rotation (v0.4)](#3c-rabitq--binary-projection-with-fixed-random-rotation-v04)
    - [3d. Does RaBitQ also speed up index building/updating, not just memory?](#3d-does-rabitq-also-speed-up-index-buildingupdating-not-just-memory)
  - [4. Integration with the index lifecycle](#4-integration-with-the-index-lifecycle)
    - [4a. WAL replay](#4a-wal-replay)
    - [4b. Rebuild](#4b-rebuild)
    - [4c. Snapshot](#4c-snapshot)
    - [4d. Changing quantization](#4d-changing-quantization)
  - [5. User-facing API](#5-user-facing-api)
  - [6. Internal re-ranking — deferred out of v1, to simplify the first landing](#6-internal-re-ranking--deferred-out-of-v1-to-simplify-the-first-landing)
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

Memory isn't the only thing quantization can improve. §3d makes the case that
the same bitwise distance computation which shrinks memory can also speed up
the distance-calculation-dominated cost of index *building* — HNSW insertion
is internally a graph search, exactly like a query.

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
| **RaBitQ** | Binary projection, fixed random rotation |  190 MB (theoretical) / see caveat below |      ~40% raw, measured (v1 — re-rank deferred, §6)      |  Opt-in  | v0.4  |

**Memory caveat (2026-08-16)**: the 190 MB figure is bits-only (1M × 1536 ÷
8), and doesn't include HNSW's own graph/connectivity metadata. Sourced
production numbers for HNSW+RaBitQ specifically report **~6.4× total memory
reduction, not 32×** — HNSW's per-node graph overhead (edge lists, layer
assignments) doesn't shrink with vector quantization, so it becomes a larger
fraction of total memory as the vector payload shrinks. The 32×-style figure
is closer to what IVF+RaBitQ achieves (~26.3× reported), where quantization
gains aren't diluted by per-node graph metadata the same way. This table's
"190 MB" number should be treated as a lower bound on the vector payload
alone, not RocksGraph's actual per-index memory footprint once graph
overhead is included — needs remeasuring against a real built index, not
computed from the bits-per-vector formula alone.

**Recall caveat (2026-08-16)**: the "~40% raw" figure is measured against
RocksGraph's own recall test (synthetic, 768-dim, no query-record
correlation) — see §3d for why this is likely explained by construction-time
quantization rather than an inherent property of RaBitQ, and §6 for how much
re-ranking recovers empirically.

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

### 3c. RaBitQ — binary projection with fixed random rotation (v0.4)

**How it works**: a `RaBitQIndex` wrapper composes with usearch the same way
`UsearchHnswIndex` does today — it is not a separate FFI dependency, just a
new code path inside the existing vector module. The core transform is fixed
and data-independent (Gao & Long, SIGMOD 2024; extended/proved asymptotically
optimal in SIGMOD 2025):

1. **Rotation matrix `P`** (D×D, orthogonal): generated **once per index**,
   deterministically from a seed stored in the index config — e.g. via a
   randomized Hadamard transform, which is `O(D log D)` to apply and avoids
   materializing a full D×D matrix for large D. `P` does not depend on the
   data. It exists before the first vector is ever inserted.
2. **Per-vector, on arrival**: rotate `Px`, take the sign of each component as
   one bit, and compute a rescaling factor `t` from `x` and `Px` alone (no
   other vectors involved). This is pure per-vector arithmetic — same shape
   as the existing F16 conversion, streaming, no batching required.

```
insert(v):
    rotated = P @ v                          // O(D log D) via Hadamard transform
    bits    = pack_bits(sign(rotated))       // D bits, sign-quantized
    t       = rescale_factor(v, rotated)     // one f32, from v alone
    buf     = bits ++ le_bytes(t)            // D bits + 32 bits, one buffer
    inner.add(label, buf)                    // usearch, native B1 storage, dimensions = D + 32
```

**Storage — usearch's native `B1` scalar kind, `t` embedded in the padded
vector buffer, not an external side-table.** Checked the `usearch` 2.26 Rust
crate directly: it exposes `ScalarKind::B1` (bit-packed storage, `b1x8`
container) *and* `MetricFunction::B1X8Metric` for a user-supplied distance
closure — but **the closure signature is
`Fn(*const b1x8, *const b1x8) -> Distance`, two raw pointers to vector data
and nothing else** (found during implementation, verified against
`usearch/rust/lib.rs`). No label, no index, no way to identify which vectors
are being compared. That kills the originally-designed approach: an external
array (`rescale_factors[label]`) keyed by label can't be looked up from
inside a closure that never receives a label.

The fix: `t` has to travel *inside* the bytes the closure already receives.
The index is created with `dimensions = D + 32` (round `D` up to a multiple
of 8 first), and every vector's buffer is `D` sign bits followed by `t`'s raw
`f32` bytes. Both closure arguments are pointers to a full buffer of that
shape, so each side reads its own trailing 4 bytes to recover its own `t` —
no lookup, no label, no external structure. usearch treats vectors as opaque
byte blobs sized by `dimensions`/`scalar_kind` (confirmed against
`index_dense.hpp` — it never interprets bit contents itself, only the
registered metric function does), so this padding is transparent to its own
graph/capacity accounting.

This removes the in-memory side-array (`Vec<f32>`/`HashMap<u64, f32>`) the
original version of this section proposed, and with it, the extra
snapshot-trailer persistence for that array (§4c) — `t` is now part of the
same payload usearch's own `save_to_buffer`/`load_from_buffer` already
covers. Net simpler than the side-array design, not just a fix for it.

**Open question — can the query really stay full precision?** The original
draft of this section assumed asymmetric distance computation: the query
kept as exact floats, only stored vectors quantized, matching PQ and RaBitQ's
own reference design. That's now in question. `usearch`'s `search<T>()` is
generic per scalar kind, and the `b1x8`-typed search path is what routes
through `B1X8Metric` — meaning the query vector likely also has to arrive
pre-packed as a `b1x8` buffer to reach the custom closure at all, not as a
plain `f32` slice. If so, true asymmetric (full-precision-query vs.
quantized-database) distance may not be reachable through usearch's typed
API as directly as first assumed, and RaBitQ would fall back to symmetric
quantized-vs-quantized comparison, which the literature (§ discussed
separately) shows has higher error than the asymmetric form. `Index` holds
exactly one `metric_fn` for its whole lifetime (confirmed — it's a single
field, not per-call), so the same closure handles both insertion's internal
comparisons and external query search; a possible direction is tagging each
buffer (query-side vs. stored-side) so the one closure can interpret them
differently, but that's unvalidated, not a designed solution — flagging as
open rather than asserting a fix that hasn't been prototyped.

**The reconstruction formula depends on the distance metric — this has to be
specified, not assumed.** RaBitQ's estimator reconstructs an approximate
inner product `⟨q, o⟩` from the bit code plus the per-vector rescale factor
`t`. What that reconstructed value *means*, and what the `B1X8Metric` closure
must do with it, differs by `DistanceMetric`:

- **Cosine**: correct only if both `q` and `o` are unit-normalized *before*
  rotation/quantization — then the reconstructed inner product directly
  approximates cosine similarity. The f16/f32 paths get this for free because
  usearch's native `Cos` metric kernel normalizes internally; RaBitQ's custom
  closure has no such built-in step, so normalization has to happen
  explicitly at quantize time (both `insert` and `search`) whenever `Cosine`
  is configured.
- **DotProduct**: the reconstructed inner product is used as-is — this is
  precisely what `t` exists to calibrate, since dot product (unlike cosine)
  is magnitude-sensitive.
- **Euclidean/L2**: **not** recoverable from the inner-product estimator
  alone. `‖q−o‖² = ‖q‖² + ‖o‖² − 2⟨q,o⟩` — a correct RaBitQ-for-L2 path needs
  `‖o‖²` (unquantized) available alongside the bit code and folded into the
  closure's distance formula. There is no way to make the cosine/dot-product
  formula do double duty for L2.

So **the `B1X8Metric` closure must branch on `config.metric`** — one formula
cannot be correct for all three simultaneously, and the index-creation path
needs to either implement all three or explicitly restrict RaBitQ to a
supported subset and reject the rest at `add_vector_index()` time.

> **Found during implementation review (2026-08-16)**: the v1 code as
> written does not branch on metric at all. `create_rabitq_metric()` always
> computes the same `(1 − t_a·t_b·(D − 2·hamming)).max(0.0)` formula, and both
> `UsearchHnswIndex::new()` and `load_vector_index()` install it via
> `inner.change_metric::<b1x8>(...)` unconditionally whenever quantization is
> RaBitQ — silently overwriting whatever `metric_to_usearch(config.metric)`
> set two lines earlier. A RaBitQ index configured with `Euclidean` or
> `DotProduct` today ranks by the cosine-shaped formula anyway, with no error
> or warning. Even the one metric the formula resembles (`Cosine`) is only
> correct with pre-normalized inputs, and the current `insert`/`search` code
> does not normalize. This is very likely a significant contributor to the
> near-random recall measured during review (2–27% across repeated runs of
> the same fixed-seed test, against literature's 60–80% and this project's
> usual 90%+ bar) — it needs to be fixed as part of landing v1, not filed as
> a follow-up. Whether v1 supports all three metrics or scopes down to one
> (e.g. `DotProduct` only, since it needs neither normalization nor a
> separate norm term — the simplest correct case) is an open scope decision,
> not just a bug fix.

**No training, no SVD, no warm-up fallback index.** The entire state-machine
this section previously described (`TrainingPhase`, a fallback F16 index
serving queries during a background-SVD warm-up window, crash-recovery logic
for a training sample) does not apply to RaBitQ as actually specified — that
machinery models a *learned* rotation (closer to OPQ), which RaBitQ is not.
Vectors can be quantized and inserted immediately, from the very first one.

**Internal re-ranking is deferred out of v1** — see §6. v1 returns the raw
quantized result directly; the quantization penalty is not hidden from the
user yet.

**Recall loss — now measured, not just cited from literature (2026-08-16)**:
RocksGraph's own `test_nearest_hnsw_rabitq_recall_vs_exact`
(`graph/tests/vector.rs`) measures ~33% average recall against the project's
90% `RECALL_THRESHOLD`, and a standalone estimator-only diagnostic (bypassing
usearch/HNSW entirely) measured ~40% on a synthetic 768-dim dataset — both
far below the "2-5% raw loss" (i.e. ~95-98% recall) figure this section
originally cited from literature. Root-caused, not just observed: verified
numerically that neither the rotation (FWHT+sign-flip vs. a properly
orthogonalized dense matrix: 0.835 vs 0.836 correlation, no meaningful
difference) nor the distance-estimator formula is the cause. §3d's finding —
that v1 quantizes during graph construction, which production
implementations explicitly avoid — is the leading explanation. Until that's
resolved (or ruled out as infeasible under RocksGraph's usearch-based
architecture — see §3d), **do not cite the 2-5%/95-98% literature figure as
what v1 delivers.** §6 shows re-ranking recovers most of this gap regardless
of root cause: 40%→89% at 10x overfetch, measured against RocksGraph's own
implementation.  
**Training**: none.  
**Memory**: ~190 MB bit-packed vectors at 1M × 1536-dim, live from the first
insert — no fallback index, no transient 3 GB F16 warm-up footprint.

---

### 3d. Does RaBitQ also speed up index building/updating, not just memory?

Revised 2026-08-16: this section originally framed "should construction use
RaBitQ bits too, not just final storage" as a speculative, optional,
downstream question. It isn't anymore — it's now the leading, sourced
explanation for v1's recall gap, and the answer from how production systems
actually do it is the *opposite* of what v1 currently implements.

**The mechanism (why this matters for recall, not just speed)**: HNSW
insertion *is* a search. Adding a vector means greedily walking the graph to
find its `M` nearest neighbors at each layer — the exact same
graph-traversal operation a query performs, just triggered by `insert()`
instead of `search()`. Per `design_ann_algorithm_and_library.md` §2b, that's
roughly 800 distance calculations per insert at 1536 dimensions. Whatever
distance function is used for *those* internal comparisons determines which
neighbors actually get wired into the graph — and unlike a query with a bad
`ef_search` (which just returns a slightly-worse answer, fixable by
re-querying), a bad neighbor chosen *during construction* is baked into the
graph permanently. Approximation error at insert time doesn't cost recall on
one operation, it compounds across every future query through that region of
the graph, until the index is rebuilt.

**What production RaBitQ+HNSW implementations actually do — sourced, not
theoretical**: researched this rather than continuing to treat it as an open
question. The RaBitQ paper authors' own reference library, and multiple
independent sources describing it, agree: **HNSW graph construction uses
full-precision vectors; RaBitQ compression is applied only to the final
stored/searched representation.** Quote: *"HNSWRaBitQ follows the RaBitQ
Library approach of building the graph with uncompressed vectors and only
applying RaBitQ compression during search."* This is explicitly contrasted
with HNSWPQ, which *does* build the graph with compressed vectors and is
reported to have measurably worse graph quality as a direct result — i.e.
this isn't a minor implementation detail, the reference implementation
treats "don't quantize during construction" as load-bearing. Reported
recall for the correct (full-precision-construction) approach: **97.2% at
k=10** — essentially matching F32 exact search, and dramatically higher than
RocksGraph's own measured ~40% raw.

**RocksGraph v1 does the opposite of this, and it's very likely part of why
recall is so low.** `UsearchHnswIndex::insert()` quantizes the vector into a
`b1x8` buffer *before* calling `usearch::add()` (§3c). usearch registers
exactly one `metric_fn` per `Index` for its whole lifetime — the same
closure handles both usearch's own internal graph-construction comparisons
during `add()` and external query-time `search()`. There is currently no
path in v1 where graph construction sees full-precision distances at all.

**Revised again, same day — construction-time quantization is real but not
proven sufficient on its own.** Directly tested whether "good candidate
discovery, noisy final ranking" gets close to the 97.2% figure: fed the
quantized estimator a *perfect* candidate pool — literally the true top-N
by exact similarity, i.e. best case for what flawless full-precision
construction would hand to the final selection step — and let the same
quantized asymmetric estimator pick the top-`k` from just that pool.

| Candidate pool size | Recall |
| -------------------: | -----: |
| 10 (= true top-k itself) | 100% (trivial, sanity check) |
| 25  | 57.8% |
| 50  | 47.0% |
| 100 | 42.8% |
| 200 | 41.0% |
| 500 | 40.6% |
| 2000 (full dataset, no restriction) | 40.4% |

For any realistically-sized candidate pool (100+, a normal `ef_search`
range), recall converges right back to the ~40% brute-force baseline. **The
final ranking step is a real, independent bottleneck — fixing candidate
discovery (construction quality) alone would not be expected to close the
gap to 97.2%, at least not on this benchmark's data.** Two most likely
reconciling explanations, neither confirmed:
- This benchmark uses fully independent, unrelated random query/dataset
  vectors at dim=768, where true top-10 similarities sit within ~0.03-0.13
  of the median (§3c) — an unusually flat, noise-sensitive landscape. Real
  embeddings with genuine semantic clusters would give true near-neighbors
  much more separation from the noise floor, letting the same estimator
  quality translate to much higher recall. Untested without real embedding
  data.
- Production 97.2%-recall implementations plausibly don't rely on the
  quantized estimator for the *final* decision either — "full-precision
  construction" and "exact re-rank" may not be alternative strategies, but
  two parts of the same recipe (cheap quantized bits for coarse graph
  navigation, something closer to exact for the final top-`k` selection).
  If so, the construction fix and §6's re-ranking aren't competing options —
  both may be required together.

**Practical consequence**: don't treat the construction-time fix as
sufficient by itself even if the usearch-feasibility question below turns
out favorable. Re-ranking (§6) has direct, unconditional evidence behind it
(40%→89% at 10x overfetch, measured, no dependency on construction or on
real-embedding assumptions) — it should be the primary path to a usable v1,
with the construction-time fix as a complementary, secondary effort for
graph/build-speed quality, not the thing recall depends on.

**The open question this reframes, and it's a harder one than before: is
"full-precision construction, quantized storage" achievable at all through
RocksGraph's usearch-as-black-box architecture?** The reference library that
achieves 97.2% recall has its own custom HNSW implementation with full
control over the construction algorithm — it can freely use one distance
function while building and a different encoding for what's persisted.
RocksGraph deliberately reuses usearch's mature C++ engine rather than
implementing HNSW from scratch (an earlier, deliberate architectural
decision this project made). usearch's public API doesn't obviously expose
"use metric A for graph construction, store as encoding B" — it has one
index, one scalar kind, one registered metric. Candidate directions, none
yet validated:
- Build with a temporary `ScalarKind::F32` usearch index, then find a way to
  transplant just the learned graph edges into a `B1`-configured index —
  unclear whether usearch exposes graph structure at that level, or whether
  its on-disk/in-memory format even separates "graph connectivity" from
  "stored vector data" cleanly enough to do this.
- Maintain two indexes simultaneously (F32 for insert-time graph decisions,
  B1 for what's actually kept/searched) — defeats a meaningful part of the
  memory win during the window both exist, and doubles insert-path work; may
  still net out positive if the F32 copy can be dropped once construction
  for a segment/batch completes, but that's a real design in its own right,
  not a one-line change.
- Accept that RocksGraph's specific composition of usearch can't reach the
  reference implementation's construction-time behavior, and rely on
  re-ranking (§6) instead — re-ranking corrects the final answer regardless
  of how the graph was built, so it's insulated from this limitation
  entirely, and per the candidate-pool result above it's likely necessary
  either way, not just a fallback.

**What this means practically**: given the candidate-pool finding above,
the construction-time fix is no longer the thing recall depends on — that's
re-ranking (§6), which has direct, unconditional evidence behind it. The
usearch feasibility question is still worth a quick, concrete answer (does
usearch expose anything close to graph-edge introspection or
transplantation), since fixing construction likely still helps graph quality
and build speed on top of re-ranking (§8's original build-speed motivation)
— but it's no longer gating whether RaBitQ v1 can reach acceptable recall.
That gate is re-ranking. This is no longer accurately described as
"downstream of RaBitQ shipping" either way — construction quality and
re-ranking are both real, sourced parts of getting RaBitQ to a usable state,
with re-ranking being the one with evidence it works unconditionally.

**Sources for the production-implementation claims above** (2026-08-16):
[RaBitQ: 1-Bit Vector Quantization (Part 3)](https://medium.com/@dnotitia/rabitq-1-bit-vector-quantization-part-3-92bbc1dbe8eb)
(HNSWRaBitQ construction approach, 6.4×/97.2% figures);
[Bring Vector Compression to the Extreme — Milvus](https://milvus.io/blog/bring-vector-compression-to-the-extreme-how-milvus-serves-3%C3%97-more-queries-with-rabitq.md)
(76%→94.7% recall via refinement at ~4% QPS cost, cited in §6).

---

## 4. Integration with the index lifecycle

### 4a. WAL replay

WAL entries contain the raw `FloatVector` (full-precision). During replay,
`RaBitQIndex` (or the F16/F32 path, which needs no wrapper — §3a/§3b) encodes
the vector on the fly before passing it to usearch. The WAL format does not
change — quantization is an in-memory index optimization, not a storage
format change.

### 4b. Rebuild

`rebuild_vector_index()` scans the props CF for `FloatVector` values and
re-inserts them. Each insert goes through the quantization step. For RaBitQ,
the rotation matrix is deterministic from the index's stored seed — rebuild
regenerates the identical matrix, it does not retrain or resample anything.

### 4c. Snapshot

The HNSW snapshot stores the index's internal vectors in their quantized
form. For RaBitQ, the per-vector rescale factor `t` no longer needs separate
handling here — since §3c embeds `t` inside each vector's own padded buffer,
it's already part of the payload usearch's own `save_to_buffer`/
`load_from_buffer` covers, with no extra work. The trailer only needs to
carry the rotation seed (or the matrix itself, if not regenerated
deterministically), with a magic number, size, and CRC-32C. On `load`, the
index checks for the trailer; if absent (old snapshot before RaBitQ was
enabled), it falls back to unquantized f32 search and logs a warning.

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

# v0.4 — RaBitQ (opt-in, minimum memory). No training-sample size to
# configure — the rotation is fixed, not learned. An optional seed makes
# the rotation reproducible across rebuilds of the same index (also fine
# to omit and let RocksGraph pick one, stored in the index config either way).
g.add_vector_index(VectorIndexConfig(...,
    quantization=Quantization.RaBitQ(seed=None),
))
```

`VectorIndexStats` (defined in `design_vector_api.md §6e`) gains a `quantization`
field for introspection:

```python
stats = g.vector_index_stats(entity_type=VectorEntityType.VERTEX, property="embedding")
print(stats.quantization)   # Quantization.F16
```

---

## 6. Internal re-ranking — deferred out of v1, to simplify the first landing

**Decision (2026-08-16): RaBitQ v1 does not overfetch or re-rank.** A
`nearest`/`similarity`/`neighbors` query against a RaBitQ index returns
usearch's raw top-`k` results directly, computed by the `B1X8Metric` closure
from §3c — the same shape F16/F32 already have today, no special-casing.
This is a deliberate scope cut to simplify the first implementation, not an
accuracy claim: v1's recall is whatever the raw quantized estimator gives —
now measured at ~33-40%, not the "2-5% loss" literature figure originally
cited here (§3c) — with no query-time recovery.

This defers, rather than resolves, the two open questions the previous
version of this section raised:
- **How would a step know a target index is quantized**, to decide whether
  to overfetch at all — moot for v1 (nothing overfetches), but still an open
  question for whenever re-ranking is picked up.
- **What overfetch factor, and where re-ranking would live** — the
  `GraphCtx::get_value()` mechanism described below is still the answer to
  "how would it fetch exact vectors if it needed to," reusing exactly what
  `NearestStep::produce()`'s existing `resolve_vector()` helper already does
  for `metric_override` (`vector.rs:79`) — but it's not wired up for v1.

**If/when re-ranking is picked up later**, the shape it would take: overfetch
`k × overfetch_factor` candidates from `VectorIndex::search()` (no signature
change, just a larger internal `k`), fetch each candidate's exact `f32`
vector via `ctx.get_value()` in the Volcano step layer — not a callback
threaded into `VectorIndex`, and not the `Graph`/`api.rs` session layer,
since `VectorIndex` deliberately holds no RocksDB reference today (keeps it
rebuildable/ephemeral, swappable as `Box<dyn VectorIndex>`) and `GraphCtx` is
already what makes step code work identically across `ReadSession` and
`TxnSession` — then compute exact similarity and truncate to `k`.

**No longer just a literature claim — measured directly (2026-08-16)**:
simulated overfetch+rerank against RocksGraph's own raw estimator on the
same 768-dim benchmark that measures ~40% raw recall:

| Overfetch factor | Candidates (k=10) | Recall |
| ----------------: | -----------------: | -----: |
| 1x (baseline, no rerank) | 10  | 40.4%  |
| 2x                       | 20  | 55.0%  |
| 4x                       | 40  | 72.4%  |
| 10x                      | 100 | 89.4%  |
| 50x                      | 500 | 100.0% |

Recall climbs smoothly with overfetch factor, no ceiling observed — this
project's 90% bar needs roughly 10x overfetch on this (synthetic, harder
than realistic) benchmark. **Cost**: overfetching doesn't touch memory (the
resident index still stores only quantized bits; overfetch only affects how
many `ctx.get_value()` RocksDB lookups happen per query) and the added
compute is smaller than what a single HNSW operation already does internally
(§3d: ~800 float distance calculations per insert/search; 10x overfetch at
k=10 is 100 extra exact re-rank computations). Milvus's own published
production numbers for RaBitQ refinement corroborate this is cheap in
practice: ~4% QPS cost (898→864 QPS) to go from 76% to 94.7% recall. Treat
10x as this benchmark's specific number, not a universal constant — real
embeddings with genuine cluster structure would plausibly need less
overfetch, since their true near-neighbors aren't buried as close to the
similarity noise floor as this synthetic i.i.d. dataset's are.

---

## 7. Interaction with other features

| Feature            |                             f16                             |                   RaBitQ                   |
| ------------------ | :---------------------------------------------------------: | :----------------------------------------: |
| `similarity` |          Works as-is (usearch returns f32 scores)           | Works as-is (v1: raw `B1X8Metric` estimator score, not re-ranked — §6) |
| `neighbors`        | Source vector stored f32 in graph, query encoded on the fly |                    Same                    |
| Filtered ANN       |                       No interaction                        |               No interaction               |
| `withEfSearch`     |                       No interaction                        |               No interaction               |
| Bulk load          |           `VectorIndex.insert()` handles encoding           | Same — `VectorIndex.insert()` rotates + quantizes on arrival, no separate phase |

---

## 8. Implementation plan

| Phase | What                                                                                                                                                                                   |   Effort   | Depends on                                            |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | :--------: | ------------------------------------------------------ |
| v0.1  | f32 — no quantization (existing code path)                                                                                                                                            |     —      | HNSW index impl                                       |
| v0.2  | f16 quantization — default — pass `ScalarKind::F16` to usearch construction                                                                                                           |  ~5 lines  | HNSW index impl                                       |
| v0.4  | RaBitQ v1 — rotation (Hadamard transform), per-vector quantize + rescale factor `t` + norm embedded in the padded vector buffer (§3c), `B1X8Metric` closure implementing the estimator branched by `config.metric` (§3c). No overfetch/re-ranking (§6, deferred). No async/training machinery. **Implemented; recall measured ~33-40% raw, well below the 90% bar — not yet viable standalone (§3d, §6).** | ~150-250 lines | f16 shipped, `VectorIndex` trait stable |
| ~~v0.4+~~ done | ~~Query-typing open question~~ — resolved during implementation: a `CURRENT_QUERY` thread-local carries the exact rotated query into the closure via a dummy-buffer `search()` call, giving genuine asymmetric (float-query vs. quantized-database) distance. Verified working. | — | RaBitQ v1 |
| **v0.4 (primary path to usable recall)** | Internal re-ranking (§6): trigger mechanism (how a step knows an index is quantized), overfetch, `GraphCtx`-based exact re-rank in the Volcano step layer. Directly measured, unconditional evidence this closes the gap (§3d/§6: 40%→89% at 10x overfetch) — no longer a "deferred, maybe later" item; it's what v1's recall actually depends on. | separate phase, not yet estimated | RaBitQ v1 shipped |
| v0.4+ (secondary — graph/build-speed quality, not a recall dependency) | Investigate whether "full-precision construction, quantized storage" is achievable through usearch's public API (§3d), and implement if so. Candidate-pool testing showed this alone would **not** be expected to reach the 97.2% reference figure without re-ranking too (§3d) — so this is no longer gating recall, but still plausibly worth it for graph quality and the build-speed motivation (§1, §3d's original mechanism). | investigation first (small), implementation unknown until that lands | RaBitQ v1 + re-ranking shipped |
