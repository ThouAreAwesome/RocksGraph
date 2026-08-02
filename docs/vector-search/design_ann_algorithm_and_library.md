# Design: ANN Algorithm and Library Selection

Status: proposal — informs `design_vector_search.md` §7 and §12.

---

## Table of Contents

- [Design: ANN Algorithm and Library Selection](#design-ann-algorithm-and-library-selection)
  - [Table of Contents](#table-of-contents)
  - [1. Scope](#1-scope)
  - [2. IVF vs HNSW — algorithm comparison](#2-ivf-vs-hnsw--algorithm-comparison)
    - [2a. How each algorithm works](#2a-how-each-algorithm-works)
      - [HNSW (Hierarchical Navigable Small World)](#hnsw-hierarchical-navigable-small-world)
      - [IVF (Inverted File Index)](#ivf-inverted-file-index)
    - [2b. Build time and bulk-load cost](#2b-build-time-and-bulk-load-cost)
    - [2c. Query latency and recall](#2c-query-latency-and-recall)
    - [2d. Memory usage](#2d-memory-usage)
    - [2e. Incremental update support](#2e-incremental-update-support)
    - [2f. Deletion handling](#2f-deletion-handling)
    - [2g. Parameter sensitivity](#2g-parameter-sensitivity)
    - [2h. Filter / predicate integration](#2h-filter--predicate-integration)
    - [2i. Scalability beyond RAM](#2i-scalability-beyond-ram)
    - [2j. Workload fit: OLTP vs OLAP](#2j-workload-fit-oltp-vs-olap)
    - [2k. Comparison matrix](#2k-comparison-matrix)
    - [2l. Decision: HNSW only](#2l-decision-hnsw-only)
  - [3. Open-source library comparison](#3-open-source-library-comparison)
    - [3a. Candidate libraries](#3a-candidate-libraries)
    - [3b. Comparison: language and FFI](#3b-comparison-language-and-ffi)
    - [3c. Comparison: algorithms and metrics](#3c-comparison-algorithms-and-metrics)
    - [3d. Comparison: incremental updates and deletion](#3d-comparison-incremental-updates-and-deletion)
    - [3e. Comparison: thread safety](#3e-comparison-thread-safety)
    - [3f. Comparison: serialization and persistence](#3f-comparison-serialization-and-persistence)
    - [3g. Comparison: custom entity keys](#3g-comparison-custom-entity-keys)
    - [3h. Comparison: SIMD and performance](#3h-comparison-simd-and-performance)
    - [3i. Comparison: license and maintenance](#3i-comparison-license-and-maintenance)
    - [3j. Comparison: compile-time weight and dependency surface](#3j-comparison-compile-time-weight-and-dependency-surface)
    - [3k. Library comparison matrix](#3k-library-comparison-matrix)
    - [3l. Decision: usearch for v0.2 HNSW](#3l-decision-usearch-for-v02-hnsw)
  - [4. Integration design for usearch](#4-integration-design-for-usearch)
    - [4a. Mapping EntityKey to usearch label](#4a-mapping-entitykey-to-usearch-label)
    - [4b. Snapshot serialization](#4b-snapshot-serialization)
    - [4c. RwLock wrapper](#4c-rwlock-wrapper)
  - [5. Risks and mitigations](#5-risks-and-mitigations)
  - [6. Future library slots](#6-future-library-slots)

---

## 1. Scope

`design_vector_search.md` §7 gives a two-sentence summary of algorithm selection
(BruteForce v0.1, HNSW v0.2). This document gives the full reasoning behind that
decision — including why IVF was evaluated and rejected — covering every dimension
relevant to an embedded OLTP graph database. It then compares the most mature
open-source ANN libraries and selects the implementation crate for v0.2.

---

## 2. IVF vs HNSW — algorithm comparison

### 2a. How each algorithm works

#### HNSW (Hierarchical Navigable Small World)

HNSW builds a multi-layer proximity graph over the vector space. Each node
(vector) exists at layer 0, and with exponentially decreasing probability at
higher layers. Search starts at the entry point on the topmost layer, greedily
walks toward the query by following the nearest neighbor link at each step,
descends to the next layer when no closer neighbor exists, and terminates at
layer 0 with a candidate set of `ef_search` elements. The top-`k` are returned.

```
Layer 2:   *           *                   (few nodes, long-range links)
Layer 1:   * - * - *   * - * - *
Layer 0:   * * * * * * * * * * * * * * *  (all nodes, short-range links)
```

Insertion assigns a random maximum layer (exponential distribution with
parameter `1 / ln(M)`), then connects the new node to its `M` nearest
neighbors at each layer from top to bottom. Neighbor lists are capped at
`M` (layer 0: `2M`) and pruned by a heuristic that preserves diversity.

#### IVF (Inverted File Index)

IVF first clusters the vector space into `nlist` Voronoi cells using k-means.
Each vector is assigned to its nearest centroid and stored in that centroid's
inverted list. At search time, the `nprobe` closest centroids to the query are
identified by scanning all `nlist` centroids (cheap: `nlist` is typically
256–4096 centroids, not N vectors), then the `nprobe` lists are exhaustively
scanned for nearest neighbors.

```
Centroids (nlist=256):   c0, c1, c2, ..., c255
List for c7:             [v_23, v_891, v_1204, ...]   (vectors in Voronoi cell 7)
Search: find top nprobe centroids → scan their lists → merge and top-k
```

IVFFlat stores raw float vectors. IVF + PQ (product quantization) or
IVF + SQ (scalar quantization) compress the per-list vectors to reduce memory.

---

### 2b. Build time and bulk-load cost

**HNSW** has O(N · M · log N) build complexity. Each insertion touches
approximately M · log N nodes (greedy descent + neighbor connection), each
requiring a distance calculation. For 1M × 1536-dim vectors:

- ~800 distance calculations per insert
- Each distance calculation: ~6144 floating-point operations
- With AVX2 (8 floats/cycle at ~3 GHz): ~0.25 μs per distance calc
- Per insert: ~200 μs → **1M inserts ≈ 3–30 min** depending on M, hardware,
  and memory layout

There is no separate "training" phase. Insertions can begin immediately.

**IVF** build has two phases:

1. **Training**: k-means over a representative sample (typically 10–100× nlist
   vectors). For nlist=256: train on 25,600–256,000 samples. Fast (seconds to
   minutes) but requires representative data before any vectors can be indexed.

2. **Population**: assign each vector to its nearest centroid and append it to
   the inverted list. O(N · nlist) centroid scans at add time, but each centroid
   is just 1536 floats, not a graph neighbor — much cheaper than HNSW's insert.
   For 1M vectors: ~90 seconds with nlist=256 and AVX2.

**IVF bulk load is 2–10× faster than HNSW for batch workloads.** But IVF
requires a two-phase workflow: train first, then populate. HNSW requires neither.

---

### 2c. Query latency and recall

At comparable recall targets:

| Scale                  | HNSW (ef=50, M=16) | IVF (nprobe=16, nlist=1024) |
| ---------------------- | :----------------: | :-------------------------: |
| 100K, k=10, 1536-dim   |      0.5–1 ms      |           1–3 ms            |
| 1M, k=10, 1536-dim     |       1–3 ms       |           2–6 ms            |
| 10M, k=10, 1536-dim    |       2–5 ms       |           5–15 ms           |
| Recall at those params |       96–99%       |           88–95%            |

HNSW consistently achieves higher recall at lower latency because it
navigates directly to the answer region via graph links, while IVF must
scan entire Voronoi cells even when many vectors in the cell are far from
the query.

To reach 99% recall with IVF requires nprobe ≈ 64–128 (vs nprobe=16
for 90%), which proportionally increases latency. HNSW recall degrades
gracefully with smaller ef_search and can be tuned per-query without
affecting index structure.

**HNSW wins on latency and recall at equal parameter counts.** IVF can
match recall only by scanning more lists, pushing latency well above HNSW.

---

### 2d. Memory usage

**HNSW**:
- Raw vectors (always stored): N × D × 4 bytes
- Adjacency lists: N × M × 8 bytes (layer 0: N × 2M × 8 bytes)
- Average layers per node ≈ 1 + 1/ln(M) ≈ 1.28 for M=16
- Total adjacency: N × 2M × 8 × 1.28 ≈ N × 330 bytes for M=16
- **At 1M × 1536 dims, M=16: 6.0 GB vectors + 0.3 GB adjacency = 6.3 GB**

**IVFFlat**:
- Raw vectors: N × D × 4 bytes (same as HNSW)
- Centroids: nlist × D × 4 bytes (negligible: 256 × 1536 × 4 = 1.5 MB)
- Inverted list metadata: N × 8 bytes (centroid assignment + offset)
- **At 1M × 1536 dims: 6.0 GB vectors + 8 MB metadata ≈ 6.0 GB**

IVF saves ~300 MB compared to HNSW at 1M scale. The difference is modest.

With compression (IVF+PQ or IVF+SQ8), IVF can store vectors in 8 bits
instead of 32 bits, reducing 6 GB to ~1.5 GB at 3–5% recall cost. HNSW
with RaBitQ achieves similar compression ratios. At 10M+ vectors, compression
makes a larger absolute difference, but both algorithms support it.

**IVF is marginally more memory-efficient for raw vectors; both compress
similarly.**

---

### 2e. Incremental update support

**HNSW** supports incremental inserts natively. Adding a new node takes
2–10 ms and does not disturb existing graph structure beyond the connections
added to the new node's `M` neighbors. The graph remains navigable as long
as nodes are well-connected. There is no need to rebuild or retrain.

**IVF** requires that the k-means centroids are computed before any vectors
can be indexed. If new data arrives that does not fit the existing centroid
distribution (concept drift), search recall degrades silently — the query
finds the nearest centroid, but its inverted list no longer contains the
true nearest neighbors because they were inserted near a distant centroid.

The standard mitigation is to periodically retrain (re-run k-means on a
fresh sample) and reassign all vectors to new centroids. This is an O(N × nlist)
operation over the entire dataset — it is disruptive to a live system and
requires quiescing queries during the reassignment or maintaining two index
generations simultaneously.

Without retraining:
- If vectors are distributed similarly to the training set: recall stays
  high indefinitely.
- If the data distribution shifts (e.g., a new embedding model, a new
  document domain): recall silently degrades below the expected target.

**HNSW is correct by construction for any incremental workload. IVF requires
retraining to remain correct under distribution shift.**

---

### 2f. Deletion handling

**HNSW**: Deletions are difficult because the deleted node may be a required
link in the graph — removing it could disconnect neighbors that had no
direct connection to each other. The most common strategy is soft deletion
(tombstone): mark the node dead, skip it during search. The graph remains
navigable but queries must skip tombstoned nodes, incurring extra distance
calculations. Recall degrades slowly as tombstone fraction grows. Beyond
a threshold (typically 20–30%), a rebuild is required to remove dead nodes
and reconnect the graph.

**IVF**: Deletions are trivial. The inverted list for the centroid is a
plain list; the entry is removed (or marked deleted and compacted lazily).
No graph connectivity is affected. There is no tombstone accumulation
problem. Recall is unaffected by deletions.

**IVF handles deletions better than HNSW.** For a graph database where
vertex/edge deletion is common (social graph unfriends, document withdrawals),
HNSW's tombstone accumulation needs active management (`rebuild_threshold`
in the design). IVF does not.

---

### 2g. Parameter sensitivity

**HNSW**:
- `M` (max neighbors per node): set once at index creation. Higher M →
  better recall, more memory, slower insert. M=16 is standard. Changing M
  requires a rebuild.
- `ef_construction` (insert candidates): set once. Higher → better recall,
  slower build. ef_construction=200 is standard.
- `ef_search` (query candidates): tunable per-query, no rebuild needed.
  Can be decreased for latency, increased for recall. This is HNSW's
  main tuning knob in production.
- Total parameters to tune for initial deployment: **1** (ef_search).

**IVF**:
- `nlist` (number of centroids): set once at training time. Affects recall
  and search latency. Rule of thumb: `nlist ≈ sqrt(N)`. Changing nlist
  requires retraining + full reassignment.
- `nprobe` (centroids to search at query time): tunable per-query.
  Higher → better recall, higher latency. Unlike ef_search, nprobe has a
  non-linear effect: going from nprobe=8 to nprobe=16 often doubles
  latency but adds only 2–5% recall; going from nprobe=64 to nprobe=128
  adds almost nothing.
- Training sample size and k-means convergence: user must decide how much
  data to sample and how many k-means iterations to run. Under-sampling
  leads to poor centroids. Over-iterating is wasteful.
- Total parameters to tune correctly: **3–4**.

**HNSW is significantly easier to tune correctly on first deployment.**

---

### 2h. Filter / predicate integration

Both algorithms return a list of approximate nearest neighbors from the full
index. Post-filter (`design_vector_api.md` §6) works identically for
both: ANN on all vectors → apply predicates → return filtered subset.

For true pre-filter (v0.3), the approaches differ:

**HNSW pre-filter** requires passing an eligibility bitset to the search
function so it can skip ineligible nodes during graph traversal. Several
HNSW implementations (usearch, faiss HNSW) support this via a filter callback.
The graph traversal still explores the normal neighborhood; ineligible nodes
are skipped rather than followed. This works well when the filter selectivity
is moderate (> 10% of nodes are eligible).

**IVF pre-filter** is straightforward: at centroid scan time, only scan
inverted lists for centroids that have eligible vectors. But this requires
an auxiliary structure mapping eligible keys to their centroid assignments —
non-trivial to maintain incrementally. Alternatively, scan the eligible set
itself (brute-force on the subset), which degrades to exact KNN if the subset
is large.

**Neither has a clear winner for pre-filter.** HNSW filter callbacks are
better-supported in major libraries; IVF subset scan is conceptually simpler
but harder to integrate with the inverted list structure.

---

### 2i. Scalability beyond RAM

**HNSW**: The full graph must fit in RAM. Partial disk-based HNSW exists
(DiskANN from Microsoft uses a disk-resident graph with SSDs), but requires
a custom on-disk format and SSD random-access patterns. No pure-Rust
implementation of disk-based HNSW is mature today.

**IVF**: Inverted lists can be stored on disk and read sequentially at
search time (the `nprobe` lists scanned). Faiss's `IndexIVF` + `IOWriter`
supports disk-resident inverted lists. Because IVF reads sequential runs
(one inverted list = contiguous bytes), it maps well to RocksDB range reads.
This makes IVF a natural fit for datasets too large to hold in RAM.

**IVF scales past RAM; HNSW does not without a dedicated disk-aware design.**
For RocksGraph's in-memory scope, this is addressed by DiskANN (v0.5), which uses
an SSD-resident graph format rather than IVF's centroid-partitioned inverted lists.

---

### 2j. Workload fit: OLTP vs OLAP

RocksGraph is an embedded OLTP graph database. Its write pattern is
single-vertex or single-edge mutations per transaction, not bulk imports.

| Characteristic                   | RocksGraph workload            |         HNSW fit          |           IVF fit            |
| -------------------------------- | ------------------------------ | :-----------------------: | :--------------------------: |
| Single-item inserts (OLTP)       | Dominant                       |         ✅ native          |     ❌ no training phase      |
| Bulk import (OLAP)               | Occasional (data migrations)   |         ✅ (slow)          |           ✅ (fast)           |
| Frequent deletions               | Varies by application          |       ⚠️ tombstones        |          ✅ trivial           |
| Concept drift / schema evolution | New embedding models over time |      ✅ no retraining      |    ❌ requires retraining     |
| Low query latency                | Core requirement               |         ✅ 1–3 ms          |           ⚠️ 2–6 ms           |
| High recall                      | Core requirement               |         ✅ 96–99%          |           ⚠️ 88–95%           |
| Simple deployment                | Core requirement (embedded)    | ✅ no training data needed | ❌ need representative sample |

---

### 2k. Comparison matrix

| Aspect                             |              HNSW              |              IVFFlat               |
| ---------------------------------- | :----------------------------: | :--------------------------------: |
| **Build speed (1M vectors)**       |            3–30 min            |             30–90 sec              |
| **Build phase requirements**       |   None — insert immediately    | Train on representative data first |
| **Query latency (1M, k=10)**       |             1–3 ms             |               2–6 ms               |
| **Recall at standard params**      |             96–99%             |               88–95%               |
| **Memory overhead vs raw vectors** |     +5% (adjacency lists)      |        <1% (centroid table)        |
| **Incremental inserts**            |       ✅ native, O(log N)       |         ❌ centroids drift          |
| **Deletions**                      | ⚠️ tombstones, periodic rebuild |       ✅ trivial list removal       |
| **Parameter count to tune**        |         1 (ef_search)          |   3–4 (nlist, nprobe, training)    |
| **Pre-filter ANN**                 |  ✅ filter callback supported   |           ⚠️ subset scan            |
| **Post-filter ANN**                |               ✅                |                 ✅                  |
| **Beyond-RAM scalability**         |   ❌ (DiskANN variant needed)   |       ✅ disk-resident lists        |
| **OLTP fit**                       |          ✅ excellent           |               ❌ poor               |
| **OLAP / bulk fit**                |       ⚠️ slow but correct       |            ✅ excellent             |
| **Distribution shift safety**      |        ✅ no degradation        |        ❌ silent recall drop        |

---

### 2l. Decision: HNSW only

**HNSW is chosen as the only ANN algorithm.**

RocksGraph's write pattern is inherently OLTP — individual vertex and edge
mutations per transaction. IVF cannot start indexing without a training phase
on representative data, has no safe path for incremental inserts under
distribution shift, and cannot be deployed correctly without tuning nlist to
the dataset size at open time. These are unacceptable constraints for a
zero-configuration embedded database.

HNSW requires zero training, handles incremental inserts natively, and achieves
the best recall at the lowest latency. Its weaknesses (higher deletion cost,
higher build time for bulk imports) are manageable: the tombstone rebuild
threshold is configurable, and bulk imports ship via `BulkLoader` (v0.3).

**IVF is not on the roadmap.** Its three stated use cases are addressed by other
features without introducing a second ANN library (usearch has no IVF; adding
faiss-lite reintroduces the build complexity that disqualified faiss in §3j):

- **Large static corpus bulk imports**: handled by `BulkLoader` SST ingest (v0.3)
- **Datasets too large for RAM**: handled by RaBitQ (190 MB at 1M × 1536, v0.4)
  and eventual DiskANN (v0.5)
- **Deletion churn**: HNSW's tombstone rebuild threshold and background rebuild
  (v0.3) manage deletion cost without restructuring the algorithm

The §2 analysis above is retained as the explanation of why IVF was rejected.

---

## 3. Open-source library comparison

### 3a. Candidate libraries

The following libraries are evaluated for implementing the HNSW `VectorIndex`
trait in RocksGraph. Only libraries with active maintenance (commit within
18 months) and production-grade usage are included.

| Library              | Language                   | Primary maintainer                              | Stars (approx) |
| -------------------- | -------------------------- | ----------------------------------------------- | :------------: |
| **faiss**            | C++ (Python/Rust bindings) | Meta AI                                         |      35K       |
| **hnswlib**          | C++ (Python/Rust bindings) | Malkov & Yashunin (original HNSW paper authors) |      4.5K      |
| **usearch**          | C++ / Rust / Python        | Unum Cloud                                      |      2.5K      |
| **instant-distance** | Pure Rust                  | Instant Domain Search                           |      0.4K      |
| **hora**             | Pure Rust                  | aylei                                           |      2.7K      |
| **nmslib**           | C++ (Python bindings)      | Naidan & Boytsov                                |      3.4K      |
| **annoy**            | C++ (Python bindings)      | Spotify                                         |      13K       |

Libraries eliminated from comparison:

- **ScaNN** (Google) — C++ only, no Rust bindings, Apache 2.0 but tightly
  coupled to TensorFlow ecosystem; not viable for pure Rust crate.
- **DiskANN** (Microsoft) — disk-resident SSD-optimized HNSW; complexity
  incompatible with RocksGraph's in-memory + snapshot model.
- **pgvector** — Postgres extension; HNSW logic is entangled with PostgreSQL's
  buffer pool page format; not extractable as a library.
- **Vespa HNSW** — Java; JVM overhead unacceptable for Rust embedding.
- **qdrant's hnsw-rs** — internal HNSW used by Qdrant; not published as a
  standalone crate on crates.io.

---

### 3b. Comparison: language and FFI

| Library              | Integration path                               |        Compilation overhead         |           Unsafe risk           |
| -------------------- | ---------------------------------------------- | :---------------------------------: | :-----------------------------: |
| **faiss**            | `faiss-rs` (sys crate + bindgen)               | Large: libc++, OpenBLAS/MKL, LAPACK | High: C++ ABI, exception safety |
| **hnswlib**          | `hnswlib-rs` (bindgen headers)                 |       Medium: C++17, no BLAS        |         Medium: C++ ABI         |
| **usearch**          | `usearch` crate (napi/pyo3-style safe wrapper) | Small: C++ core, thin Rust wrapper  |  Low: published safe Rust API   |
| **instant-distance** | Direct Cargo dependency                        |         Minimal: pure Rust          |              None               |
| **hora**             | Direct Cargo dependency                        |         Minimal: pure Rust          |              None               |
| **nmslib**           | No Rust bindings; manual FFI required          |                 N/A                 |            Very high            |
| **annoy**            | `annoy-rs` (bindgen)                           |               Medium                |             Medium              |

For RocksGraph (a Rust crate published to crates.io), a C++ dependency means:
- Downstream users must have a C++ toolchain installed
- `cargo build` pulls in a C++ build via `cc` or `cmake`
- Windows MSVC / ARM64 cross-compile paths require additional configuration
- CI must install `clang`, `libclang-dev`, and potentially BLAS headers

Pure-Rust libraries eliminate all of this. Among libraries with C++ cores,
usearch has the best-maintained safe Rust wrapper and the simplest C++ dependency
graph (no BLAS, no Eigen for basic HNSW).

---

### 3c. Comparison: algorithms and metrics

| Library              |      HNSW      |  IVF  |  PQ   | Brute-force |      Cosine       |  L2   | Inner product |   Custom metric   |
| -------------------- | :------------: | :---: | :---: | :---------: | :---------------: | :---: | :-----------: | :---------------: |
| **faiss**            |       ✅        |   ✅   |   ✅   |      ✅      | via normalization |   ✅   |       ✅       |         ❌         |
| **hnswlib**          |       ✅        |   ❌   |   ❌   |      ❌      |         ✅         |   ✅   |       ✅       | ✅ (user-supplied) |
| **usearch**          |       ✅        |   ❌   |   ❌   |      ❌      |         ✅         |   ✅   |       ✅       | ✅ (Rust closure)  |
| **instant-distance** |       ✅        |   ❌   |   ❌   |      ❌      |         ✅         |   ✅   |       ❌       |  ✅ (trait impl)   |
| **hora**             |       ✅        |   ✅   |   ❌   |      ✅      |         ✅         |   ✅   |       ✅       |         ❌         |
| **nmslib**           |       ✅        |   ❌   |   ❌   |      ❌      |         ✅         |   ✅   |       ✅       |         ✅         |
| **annoy**            | ❌ (tree-based) |   ❌   |   ❌   |      ❌      |         ✅         |   ✅   |       ✅       |         ❌         |

For RocksGraph v0.2, the required algorithms are HNSW with Cosine, L2, and
Inner Product metrics. Custom metrics are a nice-to-have for user-defined
distance functions (e.g., Hamming for binary embeddings). All candidates except
annoy support the required metrics.

---

### 3d. Comparison: incremental updates and deletion

| Library              | Incremental insert |             Online deletion              | Deletion strategy                        |
| -------------------- | :----------------: | :--------------------------------------: | ---------------------------------------- |
| **faiss**            |  ✅ (`add` method)  | ⚠️ `remove_ids` (slow for IVF, O(N) scan) | Full list scan; use IDMap wrapper        |
| **hnswlib**          |         ✅          |             ✅ `mark_deleted`             | Soft delete (tombstone); no compaction   |
| **usearch**          |         ✅          |                ✅ `remove`                | Soft delete with optional compaction API |
| **instant-distance** |    ❌ build-once    |                    ❌                     | N/A (immutable after build)              |
| **hora**             |         ✅          |                    ✅                     | Soft delete (tombstone)                  |
| **nmslib**           |         ✅          |                    ❌                     | No deletion support                      |
| **annoy**            |    ❌ build-once    |                    ❌                     | N/A (immutable after build)              |

**instant-distance and annoy are eliminated**: both require a build-then-freeze
workflow incompatible with RocksGraph's OLTP insert pattern.

**nmslib is eliminated**: no deletion support means vertex drops and edge
drops would corrupt the index.

faiss's `remove_ids` on `IndexHNSW` is implemented as a full graph scan with
tombstoning, no better than hnswlib. On `IndexIVF`, deletion is a list scan —
acceptable for IVF but still requires a subsequent `compact` to recover space.

---

### 3e. Comparison: thread safety

| Library              |           Concurrent reads            |                         Concurrent writes                          | Concurrent read + write |
| -------------------- | :-----------------------------------: | :----------------------------------------------------------------: | :---------------------: |
| **faiss**            | ✅ (internally thread-safe for search) |                 ❌ (not safe without external lock)                 |            ❌            |
| **hnswlib**          |                   ✅                   |                                 ❌                                  |            ❌            |
| **usearch**          |                   ✅                   | ✅ (lock-free insertions via atomic CAS — unused in v0.2, see note) |            ✅            |
| **instant-distance** |                   ✅                   |                          N/A (immutable)                           |           N/A           |
| **hora**             |                   ✅                   |                                 ❌                                  |            ❌            |
| **nmslib**           |                   ✅                   |                                 ❌                                  |            ❌            |

`design_vector_concurrency.md` §3 wraps each index in `Arc<RwLock<>>`. This means
the library's own thread safety model is less critical — reads and writes are
externally serialized. **In v0.2, usearch's lock-free insert capability is
deliberately unused**: the `RwLock` write lock is held for every `insert` and
`remove`, serializing all mutations. This is Option A from the concurrency
comparison table (`design_vector_concurrency.md` §4). usearch's lock-free path is
reserved as a future upgrade (Option B) if write-lock contention becomes
measurable at high concurrent write rates — it can be enabled without changing
the `VectorIndex` trait or WAL design.

---

### 3f. Comparison: serialization and persistence

| Library              |    Snapshot save    | Snapshot load  | Format                             | Custom metadata  |
| -------------------- | :-----------------: | :------------: | ---------------------------------- | :--------------: |
| **faiss**            |   ✅ `write_index`   | ✅ `read_index` | Binary (faiss internal format)     |  ❌ (must wrap)   |
| **hnswlib**          |    ✅ `saveIndex`    | ✅ `loadIndex`  | Binary (hnswlib format)            |        ❌         |
| **usearch**          |      ✅ `save`       |    ✅ `load`    | Binary (usearch format, versioned) |        ❌         |
| **instant-distance** | ✅ (`serde` feature) |       ✅        | JSON or bincode (via serde)        | ✅ (serde fields) |
| **hora**             |      ✅ `dump`       |    ✅ `load`    | Binary (hora format)               |        ❌         |

All remaining candidates support serialization. The critical requirement for
RocksGraph is embedding `last_replayed_timestamp` in the snapshot header (see
`design_vector_wal.md` §8). Libraries that expose raw serialization bytes
(faiss, hnswlib, usearch) can be wrapped: write the library's bytes + a header
with `last_replayed_timestamp`. Libraries with opaque file-based save/load
(most of them) can be wrapped by saving to a temp file, reading back the bytes,
prepending the header, and writing to the final snapshot path.

instant-distance's serde integration makes it easiest to add custom metadata
fields. usearch has a versioned format that handles schema evolution — forward
compatibility is built in.

---

### 3g. Comparison: custom entity keys

RocksGraph's `VectorIndex` trait uses `EntityKey` as the external identifier
(`Vertex(i64)` or `Edge(EdgeKey)`). ANN libraries use either:

- **Integer labels** (u64 / i64): most common. Requires a side-table mapping
  `EntityKey → library_label` and `library_label → EntityKey`.
- **String labels**: some libraries (usearch) support string labels natively,
  but at extra overhead.
- **Direct index (usize)**: libraries like instant-distance return indices into
  an input array, requiring the caller to maintain a parallel Vec of EntityKeys.

| Library     | Key type            | EntityKey mapping strategy                                                  |
| ----------- | ------------------- | --------------------------------------------------------------------------- |
| **faiss**   | `i64` label         | `HashMap<EntityKey, i64>` + reverse `Vec<EntityKey>` indexed by internal id |
| **hnswlib** | `size_t` label      | Same as faiss                                                               |
| **usearch** | `u64` or string key | `HashMap<EntityKey, u64>` with hashed key                                   |
| **hora**    | `usize`             | `Vec<EntityKey>` indexed by internal usize                                  |

For `EntityKey::Vertex(i64)`, the i64 is used directly as the u64 label — a
bit-reinterpret cast, no mapping needed. For `EntityKey::Edge(CanonicalEdgeKey)`,
`CanonicalEdgeKey` is 22 bytes — too wide for a u64. Labels are assigned from a
monotonic counter and both directions of the mapping are stored in the
`vector_edge_labels` RocksDB CF:

- Forward key `[prop_key_id][0x00][edge_key]` → label u64 (insert/remove path)
- Reverse key `[prop_key_id][0x01][label_be]` → edge_key bytes (search result path)

This avoids hashing (no collision risk), avoids in-memory map growth (the old HashMap
approach reached ~6 GB at 100M edges), and requires no snapshot serialization.
See `design_hnsw_impl.md` §4 for the full CF key format and rationale.

---

### 3h. Comparison: SIMD and performance

| Library              |                  AVX2                  | AVX-512 |        NEON (ARM)         | f32 throughput (cosine, 1536-dim, AVX2) |
| -------------------- | :------------------------------------: | :-----: | :-----------------------: | :-------------------------------------: |
| **faiss**            |            ✅ (auto-detect)             |    ✅    |             ✅             |              ~0.20 μs/pair              |
| **hnswlib**          |            ✅ (compile flag)            |    ❌    |             ❌             |              ~0.25 μs/pair              |
| **usearch**          |          ✅ (runtime dispatch)          |    ✅    | ✅ (Apple M-series tested) |              ~0.18 μs/pair              |
| **instant-distance** | ❌ (generic Rust loop, auto-vectorized) |    ❌    |             ❌             |           ~0.40–0.60 μs/pair            |
| **hora**             |            ❌ (generic Rust)            |    ❌    |             ❌             |           ~0.40–0.60 μs/pair            |

faiss and usearch have hand-written SIMD kernels for every major metric. At
1536 dims and 1M vectors, the 2–3× gap between library SIMD and compiler
auto-vectorization translates to 1–2 ms query latency difference (150 distance
calls at 0.25 μs each vs 0.50 μs each → 38 ms vs 75 ms for brute-force, but
for HNSW the savings compound per-layer).

For RocksGraph targeting the performance expectations in §11 (1–3 ms for 1M,
k=10), instant-distance and hora cannot reliably hit that target without
custom SIMD. faiss and usearch can.

---

### 3i. Comparison: license and maintenance

| Library              | License    |         Last commit (approx)          | Breaking changes policy           |
| -------------------- | ---------- | :-----------------------------------: | --------------------------------- |
| **faiss**            | MIT        |      Active (Meta AI maintains)       | Occasional ABI breaks; versioned  |
| **hnswlib**          | Apache 2.0 |                 2024                  | Stable; original authors maintain |
| **usearch**          | Apache 2.0 | Active (Unum Cloud, commercial users) | Semantic versioning, changelog    |
| **instant-distance** | Apache 2.0 |                 2023                  | Small project; fewer guarantees   |
| **hora**             | Apache 2.0 |             2022 (stale)              | Last release 2022                 |

**hora is eliminated**: last commit was 2022; no evidence of active maintenance
or bug fixes. Using a stale pure-Rust crate as core infrastructure is a risk
not justified by its marginal convenience.

**instant-distance** is small and was last updated in 2023. Its immutable
build model would require us to rebuild from scratch after every insert anyway
(not viable), so it was already eliminated in §3d.

**faiss**, **hnswlib**, and **usearch** are all actively maintained with
commercial users depending on them.

---

### 3j. Comparison: compile-time weight and dependency surface

RocksGraph is an embedded library published to crates.io. Users run
`cargo add rocksgraph`. Heavy build dependencies increase:

- First-build time for downstream users
- Cross-compilation complexity (Windows, ARM, musl)
- Binary size

| Library              | Build deps                          | `cargo build` cold time (approx) | Additional sys libs              |
| -------------------- | ----------------------------------- | :------------------------------: | -------------------------------- |
| **faiss**            | libc++, OpenBLAS or MKL, LAPACK     |             5–15 min             | `libopenblas-dev`, `libblas-dev` |
| **hnswlib**          | C++17 STL only                      |            30–60 sec             | None beyond C++ compiler         |
| **usearch**          | C++ core only (header-only simsimd) |            30–90 sec             | None                             |
| **instant-distance** | Pure Rust                           |             5–10 sec             | None                             |
| **hora**             | Pure Rust + SIMD crate              |            10–20 sec             | None                             |

faiss is eliminated on build complexity alone: requiring OpenBLAS as a system
dependency would break the zero-configuration embedded DB promise. Users on
macOS arm64, musl Linux, or Windows MSVC would face non-trivial setup.
faiss-lite (HNSW only) avoids BLAS, but `faiss-rs` on crates.io bundles the
full faiss at time of writing.

---

### 3k. Library comparison matrix

Scoring: ✅ = excellent, ⚠️ = acceptable, ❌ = disqualifying

| Criterion                      |  Weight  |          faiss           |     hnswlib      |   usearch    | instant-distance | hora  |
| ------------------------------ | :------: | :----------------------: | :--------------: | :----------: | :--------------: | :---: |
| Pure Rust or minimal C++ dep   |   High   |            ❌             |        ⚠️         |      ⚠️       |        ✅         |   ✅   |
| Incremental inserts            | Critical |            ✅             |        ✅         |      ✅       |        ❌         |   ✅   |
| Deletion support               | Critical |            ⚠️             |        ✅         |      ✅       |        ❌         |   ✅   |
| SIMD performance (AVX2, NEON)  |   High   |            ✅             |        ⚠️         |      ✅       |        ❌         |   ❌   |
| Serialization + metadata       |   High   |            ✅             |        ✅         |      ✅       |        ✅         |   ✅   |
| Active maintenance             |   High   |            ✅             |        ✅         |      ✅       |        ⚠️         |   ❌   |
| Cross-platform build (Win/ARM) |   High   |            ❌             |        ⚠️         |      ✅       |        ✅         |   ✅   |
| Safe Rust API                  |  Medium  |            ❌             |        ❌         |      ✅       |        ✅         |   ✅   |
| Custom metric support          |   Low    |            ❌             |        ✅         |      ✅       |        ✅         |   ❌   |
| **Disqualifiers**              |          | Build complexity, unsafe | Unsafe, ARM SIMD |      —       | Immutable index  | Stale |
| **Verdict**                    |          |            ❌             |        ⚠️         | **✅ chosen** |        ❌         |   ❌   |

---

### 3l. Decision: usearch for v0.2 HNSW

**usearch is chosen as the HNSW implementation for v0.2.**

Rationale:

1. **Performance**: Hand-written SIMD (AVX2, AVX-512, NEON via simsimd) achieves
   ~0.18 μs/pair cosine distance. This is the fastest per-distance-calculation
   performance of any candidate, enabling RocksGraph to hit the 1–3 ms target
   at 1M × 1536 dims.

2. **Cross-platform**: usearch explicitly targets and tests Linux x86_64,
   Linux arm64, macOS x86_64, macOS arm64 (Apple Silicon), and Windows x86_64.
   The C++ core is header-only for the HNSW component (simsimd is also
   header-only). No system library installation needed.

3. **Safe Rust API**: the `usearch` crate on crates.io exposes a safe Rust
   wrapper around the C++ core. No raw `unsafe` blocks required in RocksGraph
   code.

4. **Incremental insert + deletion**: both are supported and tested in
   production at Unum Cloud (USearch is the backing index for USearch DB).
   Deletion uses soft-delete (tombstone) with a separate compaction step,
   matching the tombstone rebuild model in §7b of `design_vector_search.md`.

5. **Active maintenance**: commercial users depend on it; API stability is
   maintained with semantic versioning.

6. **Future lock-free upgrade**: usearch supports concurrent lock-free inserts
   via atomic CAS internally. Once RocksGraph's write throughput outgrows the
   `Arc<RwLock<>>` serialization (`design_vector_concurrency.md` §3), the wrapper can be promoted to allow
   concurrent writes without changing the `VectorIndex` trait or WAL design.

**hnswlib is the fallback** if usearch integration proves problematic.
hnswlib is the reference implementation from the original paper authors, widely
deployed, and has working Rust FFI bindings. It lacks ARM NEON optimization
and requires a C++ compiler, but is otherwise correct and battle-tested.


---

## 4. Integration design for usearch

### 4a. Mapping EntityKey to usearch label

usearch uses `u64` labels. For RocksGraph's `EntityKey`:

- **Vertices**: direct bit-reinterpret cast (`vertex_id as u64`). Bijective on all i64
  values including negatives. No side-table or CF lookup needed — the reverse cast
  recovers the ID exactly.
- **Edges**: `CanonicalEdgeKey` is 22 bytes — too wide for a u64. Labels are assigned
  from a monotonic counter. Both directions of the mapping are stored in the
  `vector_edge_labels` RocksDB Column Family (not in RAM), so the mapping survives
  across restarts without deserializing a snapshot block and has no in-memory
  overhead regardless of edge count.

```rust
pub struct UsearchHnswIndex {
    inner:           usearch::Index,
    db:              Arc<DB>,        // for vector_edge_labels CF lookups
    prop_key_id:     u16,            // CF key prefix
    entity_type:     VectorEntityType,
    next_edge_label: u64,            // monotonic counter; persisted in snapshot header
    last_replayed_timestamp: u64,
}
```

For vertex-only indexes, `db` is held but never accessed for label lookups. On insert,
the CF is consulted for upsert detection (edge indexes) or `usearch.contains(label)` for
vertex indexes. See `design_hnsw_impl.md` §4 for the full CF key format and implementation.

### 4b. Snapshot serialization

usearch's `save(path)` writes the index to a file. RocksGraph wraps it with a
fixed header (magic, format_version, `last_replayed_timestamp`, dimension, metric,
algorithm, tombstone_count, next_edge_label, usearch_payload_len) followed by the
usearch binary block and a CRC-32C trailer. No label-map serialization: the
`vector_edge_labels` CF is always current and requires no snapshot encoding step.

```rust
impl VectorIndex for UsearchHnswIndex {
    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<()> {
        let tmp_usearch = path.with_extension("usearch.tmp");
        self.inner.save(tmp_usearch.to_str().unwrap())?;
        let usearch_bytes = std::fs::read(&tmp_usearch)?;
        std::fs::remove_file(&tmp_usearch)?;

        // Write final snapshot atomically: header + usearch bytes + CRC-32C.
        // No label map block — mappings live in vector_edge_labels CF.
        let tmp_path = path.with_extension("tmp");
        write_snapshot_header(&tmp_path, last_replayed_timestamp,
                              self.next_edge_label, self.dimension,
                              self.metric, self.tombstone_count, &usearch_bytes)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

pub fn load_vector_index(path: &Path, config: &VectorIndexConfig,
                         db: &Arc<DB>) -> Result<Box<dyn VectorIndex>> {
    // read header, verify magic and CRC-32C
    // extract last_replayed_timestamp, next_edge_label
    // extract usearch bytes → temp file → usearch::Index::load(tmp)
    // no label map deserialization
    Ok(Box::new(UsearchHnswIndex { inner, db: db.clone(), .. }))
}
```

Full byte layout: `design_hnsw_impl.md` §8a (format_version = 2, 48-byte header + trailer).

### 4c. RwLock wrapper

No changes to the `Arc<RwLock<Box<dyn VectorIndex>>>` model. usearch's
internal lock-free inserts are unused in v0.2 — the `RwLock` write lock
is held during `insert`/`remove` as designed in `design_vector_concurrency.md` §3. If write contention
becomes measurable, the wrapper can be promoted to acquire no lock for
inserts (relying on usearch's own atomics), while retaining the read lock
for searches.

---

## 5. Risks and mitigations

| Risk                                                               | Likelihood | Impact | Mitigation                                                                                                                         |
| ------------------------------------------------------------------ | :--------: | :----: | ---------------------------------------------------------------------------------------------------------------------------------- |
| usearch C++ build fails on exotic target (musl, WASM)              |    Low     | Medium | Fall back to hnswlib (also C++) or instant-distance (pure Rust, build-time only — rebuild each open)                               |
| usearch snapshot format changes incompatibly across crate versions |    Low     |  High  | Pin usearch version; store format_version in snapshot header; migration path: rebuild from props CF (Strategy B)                   |
| Edge label collision                                               | Eliminated |   —    | Resolved by monotonic u64 counter + `vector_edge_labels` CF instead of hashing; no collision possible |
| HNSW tombstone accumulation (high-churn graph)                     |   Medium   | Medium | `rebuild_threshold` = 30% triggers rebuild on `Graph::open`; background rebuild in v0.3                                            |
| usearch memory usage higher than expected at 10M+ vectors          |    Low     | Medium | RaBitQ (v0.4) reduces to ~190 MB at 1M × 1536; DiskANN (v0.5) for true disk-resident scale |

---

## 6. Future library slots

| Version  | Algorithm          | Candidate library             | Rationale                                                                    |
| -------- | ------------------ | ----------------------------- | ---------------------------------------------------------------------------- |
| **v0.4** | RaBitQ compression | Custom Rust impl or RaBitQ-rs | Compression layer over HNSW; no established Rust crate today                 |
| **v0.5** | DiskANN            | spann-rs or custom            | Disk-resident index for graphs that exceed RAM; requires SSD-friendly design |
