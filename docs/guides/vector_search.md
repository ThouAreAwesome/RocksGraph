# Vector Search Deep Dive

**Target:** RocksGraph v0.2.0+

RocksGraph provides integrated, in-process approximate nearest neighbor (ANN) vector search powered by Hierarchical Navigable Small World (HNSW) graphs.

Unlike separated architectures where vector databases and graph databases run in separate systems requiring complex distributed joins, RocksGraph embeds vector embeddings directly into graph properties and traversal streams.

> [!NOTE]
> Snippets below are excerpts, not full programs — they assume `graph`/`snap`/`schema` are already open as shown in [Getting Started](getting_started.md), and that relevant enums (`VectorEntityType`, `DistanceMetric`, `AnnAlgorithm`, `Quantization`, `Order`) are imported from `rocksgraph` where used.

## Table of Contents

- [1. Vector Index Configuration](#1-vector-index-configuration)
- [2. Declaring Vector Indexes](#2-declaring-vector-indexes)
- [3. Query Primitives](#3-query-primitives)
- [4. Query-Time Tuning Knobs](#4-query-time-tuning-knobs)
- [5. Memory Footprint & Quantization](#5-memory-footprint--quantization)
- [6. Vector Search Best Practices](#6-vector-search-best-practices)
- [7. Vector Search Anti-Patterns](#7-vector-search-anti-patterns)
- [8. Index Persistence & Crash Recovery](#8-index-persistence--crash-recovery)

---

## 1. Vector Index Configuration

Vector indexes are declared per property key using `SchemaSession`.

### Configuration Parameters

| Parameter | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| **`dimension`** | `usize` | *Required* | Vector embedding dimension (e.g., 384, 768, 1536). |
| **`metric`** | `DistanceMetric` | `Cosine` | Distance metric: `Cosine`, `DotProduct`, `Euclidean` (L2). |
| **`m`** | `usize` | `16` | Maximum outgoing links per node in the HNSW graph (higher = better recall, higher memory). |
| **`ef_construction`** | `usize` | `200` | Size of dynamic candidate list during index construction. |
| **`ef_search`** | `usize` | `50` | Size of dynamic candidate list during search (runtime tuning). |
| **`quantization`** | `Quantization` | `F16` | Vector storage format: `F16` (50% memory savings) or `F32` (full precision). |

---

## 2. Declaring Vector Indexes

#### 🦀 Rust
```rust
use rocksgraph::{
    schema::{
        AnnAlgorithm, DistanceMetric, HnswConfig, Quantization,
        VectorEntityType, VectorIndexConfig,
    },
    Graph, StoreError,
};

fn init_vector_index(graph: &Graph) -> Result<(), StoreError> {
    let mut schema = graph.open_schema();

    let hnsw_config = HnswConfig::default()
        .with_m(16)
        .with_ef_construction(200)
        .with_ef_search(50);

    let index_config = VectorIndexConfig::new(
        "emb",
        VectorEntityType::Vertex,
        384,
        DistanceMetric::Cosine,
        AnnAlgorithm::Hnsw(hnsw_config),
    ).with_quantization(Quantization::F16);

    schema.add_vector_index(index_config);
    schema.commit()?;

    Ok(())
}
```

#### 🐍 Python
```python
from rocksgraph import Graph, VectorEntityType, DistanceMetric, AnnAlgorithm, Quantization

def init_vector_index(graph: Graph):
    with graph.open_schema() as schema:
        schema.add_vector_index(
            entity_type=VectorEntityType.Vertex,
            property="emb",
            dimension=384,
            metric=DistanceMetric.Cosine,
            algorithm=AnnAlgorithm.Hnsw,
            m=16,
            ef_construction=200,
            ef_search=50,
            quantization=Quantization.F16,
        )
```

---

## 3. Query Primitives

RocksGraph exposes three vector query primitives within Gremlin traversals:

### 1. `.nearest(property, query_vector, k)` (ANN Traversal Seed)
> [!IMPORTANT]
> `.nearest()` is an **entry-point step** that queries the HNSW index and emits the top-$k$ nearest vertices into the traversal stream. It must immediately follow `g.V([])` (empty ids) — placing it anywhere else, including after other filtering/navigation steps, is rejected at query build time with `StoreError::UnsupportedOperation`, not a silent unaccelerated scan. To compute similarity against vertices already in the stream, use `.similarity()` instead.

#### 🦀 Rust
```rust
let mut snap = graph.read();
let query = vec![0.1f32, 0.8, 0.1];

let top_matches = snap
    .g()
    .V([])
    .nearest("emb", query, 5)
    .hasLabel("document")
    .values(["title"])
    .to_list()?;
```

#### 🐍 Python
```python
snap = graph.read()
query = [0.1, 0.8, 0.1]

top_matches = (
    snap.g()
    .V()
    .nearest("emb", query, 5)
    .hasLabel("document")
    .values("title")
    .to_list()
)
```

---

### 2. `.similarity(property, query_vector, metric)` (Compute Score & Sort)
Computes the similarity score between each candidate vertex currently in the traversal and a reference vector, **replacing each traverser with its scalar score** — the vertex itself is gone from the stream after this step, only the number remains. `.order()` alone sorts that score **ascending** (lowest first) — for similarity you almost always want the highest scores first, so sort descending explicitly.

> [!NOTE]
> `.similarity()` computes an **exact** score for every candidate it's given — it isn't a faster or slower version of `.nearest()`, it's a different guarantee. `.nearest()` searches the HNSW index and is **approximate**: the crate's own test suite targets ≥95% recall against brute-force ground truth, not 100%. Use `.similarity(...).order().by(Order.Desc).limit(k)` when you need the *exact* top-k **scores** (small candidate sets, verification/eval, or no index declared yet, as covered in this section); use `.nearest()` when approximate is acceptable and the candidate set is large — that's the common case, which is why brute-force scanning is the anti-pattern below. If you need the exact top-k **vertices** (not bare scores) via brute force, see the sub-traversal form in [§7](#7-vector-search-anti-patterns) — `.similarity()` used directly, as below, cannot give you that.

#### 🦀 Rust
```rust
use rocksgraph::{schema::DistanceMetric, Order};

let mut snap = graph.read();
let target_vector = vec![0.5f32, 0.5, 0.0];

// Similarity score of each of user 1's friends to target_vector, highest first.
// Note: the result is a list of scores (Vec<f32>), not the friends themselves —
// .similarity() replaces the traverser with its score.
let friend_scores = snap
    .g()
    .V([1])
    .out(["knows"])
    .similarity("emb", target_vector, DistanceMetric::Cosine)
    .order()
    .by(Order::Desc)
    .to_list()?;
```

#### 🐍 Python
```python
from rocksgraph import DistanceMetric, Order

snap = graph.read()
target_vector = [0.5, 0.5, 0.0]

# Result is a list of scores, not the friends themselves — see the note above.
friend_scores = (
    snap.g()
    .V(1)
    .out("knows")
    .similarity("emb", target_vector, DistanceMetric.Cosine)
    .order()
    .by(Order.Desc)
    .to_list()
)
```

---

### 3. `.neighbors(source_prop, target_prop, k, entity_type)` (Vertex-to-Vertex ANN)
Takes vertices currently in the traversal stream, reads each one's `source_prop` vector as the query, and searches the `target_prop` HNSW index for the $k$ nearest neighbors.

> [!NOTE]
> `source_prop` and `target_prop` are allowed to differ so you can search across two different vector properties (e.g. a `query_emb` property against a `document_emb` index) — but both should come from the **same embedding model**. RocksGraph only validates that the two vectors have matching *dimensions* (a mismatch is a clean `VectorError::DimensionMismatch`); it can't validate that they represent the same embedding space. Two same-dimension vectors from unrelated models will return a result with no error — just a meaningless one.

> [!NOTE]
> In v0.2, vector indexing and `neighbors()` support only `VectorEntityType::Vertex` (`VectorEntityType.Vertex` in Python). Edge vector indexing is planned for v0.3.

#### 🦀 Rust
```rust
use rocksgraph::schema::VectorEntityType;

let mut snap = graph.read();

// Start at document 42, find 3 semantically closest documents
let related = snap
    .g()
    .V([42])
    .neighbors("emb", "emb", 3, VectorEntityType::Vertex)
    .values(["title"])
    .to_list()?;
```

#### 🐍 Python
```python
from rocksgraph import VectorEntityType

snap = graph.read()

# Start at document 42, find 3 semantically closest documents
related = (
    snap.g()
    .V(42)
    .neighbors("emb", "emb", 3, VectorEntityType.Vertex)
    .values("title")
    .to_list()
)
```

---

## 4. Query-Time Tuning Knobs

You can override index defaults dynamically on individual queries (`with_ef_search` & `with_metric`):

- **`.with_ef_search(ef: usize)`**: Dynamically sets `ef_search` for `.nearest()` or `.neighbors()`.
  ```python
  # High recall query on high-stakes search
  snap.g().V().nearest("emb", query_vec, 10).with_ef_search(150).to_list()
  ```
- **`.with_metric(metric: DistanceMetric)`**: Overrides the distance metric on `.nearest()`.
  ```python
  from rocksgraph import DistanceMetric
  snap.g().V().nearest("emb", query_vec, 10).with_metric(DistanceMetric.Cosine).to_list()
  ```

---

## 5. Memory Footprint & Quantization

HNSW indexes reside entirely in memory for microsecond search latencies.

### Memory Estimation Formula
$$\text{Memory} \approx N \times \left( d \times S_q + 2 \times M \times 8 \times 1.28 \text{ bytes} \right)$$
Where:
- $N$ = Number of indexed vectors.
- $d$ = Vector dimensions (e.g., 384, 1536).
- $S_q$ = Bytes per scalar (`2` for `F16`, `4` for `F32`).
- $M$ = HNSW connectivity parameter (default `16`).
- $1.28$ = Multi-layer hierarchical graph overhead factor ($\approx 1 + 1/\ln(M)$ average layers per node for $M=16$).

### Example (1 Million 384-dimensional vectors with $M=16$):
- **With `F32` (4 bytes)**: $1\text{M} \times (384 \times 4 + 328) = 1\text{M} \times (1536 + 328) \approx \mathbf{1.86\text{ GB}}$ ($\approx 1.74\text{ GiB}$)
- **With `F16` (2 bytes)**: $1\text{M} \times (384 \times 2 + 328) = 1\text{M} \times (768 + 328) \approx \mathbf{1.10\text{ GB}}$ ($\approx 1.02\text{ GiB}$) (~41% reduction)

---

## 6. Vector Search Best Practices

### Pattern 1: Seed Traversal via Index Lookups (Pre-Filtering)
Always place `.nearest()` at the pipeline origin. This uses the HNSW index to fetch only the top candidate vertices, then navigates graph edges only from those candidates.

```python
# ✅ BEST PRACTICE: HNSW index seeds candidates, then traverses graph
snap.g().V() \
    .nearest("emb", query_vector, 20) \
    .hasLabel("product") \
    .out("bought_together") \
    .values("title") \
    .to_list()
```

### Pattern 2: Enable F16 Quantization for High-Dimensional Vectors
For embeddings with $\ge 768$ dimensions (e.g., BGE, OpenAI `text-embedding-3-small`), F16 cuts index memory by ~50% with negligible ($<0.5\%$) recall impact.

```python
# ✅ BEST PRACTICE: Use F16 quantization for large models
schema.add_vector_index(
    entity_type=VectorEntityType.Vertex,
    property="embedding",
    dimension=1536,
    quantization=Quantization.F16,
)
```

### Pattern 3: Dynamic Latency vs. Accuracy Tuning (`ef_search`)
Set `ef_search = 32` for low-latency interactive search ($<1\text{ms}$), or increase to `ef_search = 128`–`256` for batch analytical queries requiring $>99\%$ recall.

---

## 7. Vector Search Anti-Patterns

### ❌ Anti-Pattern 1: Brute-Force Vector Scans over Unindexed Properties
Using `.similarity()` over the full vertex set forces a linear $O(N)$ scan, computing an exact score for every vertex on CPU. This is only the right tool when you specifically need exact results (§3.2) — if approximate nearest neighbors are acceptable, which is the common case, declare an HNSW index and use `.nearest()` instead: same result shape (vertices, not scores), sub-linear cost, ~95%+ recall rather than exact.

#### 🦀 Rust
```rust
// ❌ ANTI-PATTERN: exact top-10 *vertices* via full scan, when approximate would do.
// The sub-traversal form of by() is required here to keep the vertex as the traverser —
// plain `.similarity(...).order().by(Order::Desc)` (§2) would replace it with a bare score.
snap.g().V([]).order().by((__().similarity("raw_emb", query_vec, DistanceMetric::Cosine), Order::Desc)).limit(10).to_list()?;

// ✅ BETTER (when exact isn't required): declare an HNSW index on "raw_emb" and use .nearest() — same shape, less work
snap.g().V([]).nearest("raw_emb", query_vec, 10).to_list()?;
```

#### 🐍 Python
```python
# ❌ ANTI-PATTERN: exact top-10 *vertices* via full scan, when approximate would do.
# The sub-traversal form of by() is required here to keep the vertex as the traverser —
# plain `.similarity(...).order().by(Order.Desc)` (§2) would replace it with a bare score.
snap.g().V().order().by(__.similarity("raw_emb", query_vec, DistanceMetric.Cosine), Order.Desc).limit(10).to_list()

# ✅ BETTER (when exact isn't required): declare an HNSW index on "raw_emb" and use .nearest() — same shape, less work
snap.g().V().nearest("raw_emb", query_vec, 10).to_list()
```

> [!WARNING]
> `.similarity(...).order().by(Order.Desc)` and `.order().by(__.similarity(...), Order.Desc)` (Rust: `.order().by((__().similarity(...), Order::Desc))`) are **not interchangeable**, and the difference isn't performance — both are $O(N)$ overall, one similarity computation per vertex either way, since `.similarity()` does a single point-lookup per traverser it receives rather than an independent scan. The difference is the *result*: the direct form discards the vertex and sorts bare scores (§2); the sub-traversal form keeps the vertex as the traverser and uses the score only as the sort key, matching what `.nearest()` returns. Pick based on what you need downstream — chain `.values(...)` or further graph steps after the sort → use the sub-traversal form; only need the numbers → use the direct form, which has less per-element overhead (no nested sub-plan dispatch) for the same asymptotic cost.

### ❌ Anti-Pattern 2: Unbounded In-Memory Index Sizing
Creating multiple unquantized F32 indexes with $M=64$ on memory-constrained servers without estimating RAM usage can cause out-of-memory crashes. Always calculate your memory budget with the formula in [§5](#5-memory-footprint--quantization) before provisioning.

---

## 8. Index Persistence & Crash Recovery

Vector index durability is guaranteed through an integrated write-ahead log (WAL) and disk-persisted index snapshots:

- **Atomic Durability on Commit**: Vector index updates are committed atomically within the exact same transaction as the graph topology, ensuring zero divergence between graph records and vector index state.
- **Snapshot Checkpointing**: Index snapshots — and the WAL trimming that goes with them — are written to disk in three ways:
  1. **Automatically**, in the background, once enough vector-property writes accumulate — see below.
  2. On a clean shutdown, via `graph.close()`.
  3. On demand, via an explicit `graph.index_manager().save_all()` call.
- **Clean Shutdown vs. Crash Recovery**:
  - **Clean Shutdown (`graph.close()`)**: Snapshots are saved immediately; subsequent `Graph::open` starts instantaneously.
  - **Unclean Shutdown / Crash**: If the process terminates without a snapshot since the last checkpoint, `Graph::open` recovers state by replaying all WAL mutations logged since then. For large, long-running write sessions with millions of vectors, replaying a long-uncheckpointed WAL can take several seconds to minutes — which is exactly what background checkpointing bounds.

### Automatic Background Checkpointing

Set `default_checkpoint_mutation_threshold` on `IndexOptions` to have RocksGraph checkpoint on its own once that many vector-property writes have accumulated across commits, on a background thread — you don't need to call `save_all()` yourself for this to happen. The threshold is tracked **per index**: each declared vector index gets its own mutation counter and triggers its own background save independently, so a hot index doesn't wait on a quiet one. Use `PerIndexOptions` to override the default for a specific index.

#### 🦀 Rust
```rust
use rocksgraph::{Graph, GraphOptions};
use rocksgraph::schema::IndexOptions;

let options = GraphOptions::default()
    .with_index(IndexOptions::default().with_default_checkpoint_mutation_threshold(50_000));
let graph = Graph::open_with_options(path, options)?;
```

#### 🐍 Python
```python
from rocksgraph import Graph, GraphOptions, IndexOptions

options = GraphOptions(index=IndexOptions(default_checkpoint_mutation_threshold=50_000))
graph = Graph.open_with_options(path, options=options)
```

`None` (the default, in both languages) disables automatic checkpointing — matching the older, fully-manual behavior described above. This doesn't replace `graph.close()`: still call it before process exit, since it's the only thing that guarantees a snapshot at the *exact* moment you stop, rather than at the last threshold crossing.

> [!TIP]
> **Operational Best Practice**: prefer `default_checkpoint_mutation_threshold` for long-running write workloads or batch import pipelines over calling `save_all()` yourself on a timer — it reacts to actual write volume rather than a guessed interval. To trigger a checkpoint outside of normal write volume (e.g. before a planned maintenance window), call `graph.index_manager().save_all()` explicitly. Either way, always call `graph.close()` before process exit.


