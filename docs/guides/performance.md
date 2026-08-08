# Performance Tuning & Best Practices

**Target:** RocksGraph v0.2.0+

This guide provides practical recommendations for maximizing write throughput, reducing query latency, and optimizing memory allocation in RocksGraph.

---

## 1. Write Throughput Optimization

### Rule 1: Batch Writes — Sized to Your Contention
Committing each mutation in its own `TxnSession` adds per-commit overhead, and batching amortizes it — but a bigger transaction also means a bigger OCC conflict window and a costlier retry if it collides. See [Transactions & Concurrency](concurrency_and_tx.md#5-what-actually-conflicts-the-occ-conflict-matrix) for the trade-off, and [§6 there](concurrency_and_tx.md#6-transaction-best-practices) for how to size batches for your workload — there's no universal number, and no RocksGraph-specific benchmark backing one.

```python
# ✅ Low contention: batch mutations for throughput
with graph.begin() as txn:
    for item in disjoint_batch:
        txn.g().addV("item").property("id", item.id).next()
```

### Rule 2: Use `BulkLoader` for Initial Imports
For initial dataset loading ($> 100,000$ entities), bypass the transactional write path completely and use [`BulkLoader`](bulk_loading.md). `BulkLoader` creates sorted storage files directly on disk, bypassing write-ahead logging and OCC entirely — substantially higher throughput for bulk imports than incremental transactional writes. See [`BENCHMARKS.md`](https://github.com/ThouAreAwesome/RocksGraph/blob/main/rocksgraph/BENCHMARKS.md) for measured figures; the write-path benchmarks there were run at different dataset scales (1M vs 69M edges), so don't treat them as a controlled comparison or derive a specific multiplier from them.

---

## 2. Query Traversal Optimization

### Rule 1: Filter Each Hop When You Reach It
`.limit(n)` already stops as soon as `n` items pass a preceding `.has()` — no need to reorder them for that, and doing so isn't equivalent: it caps the *unfiltered* candidate count first, which can return fewer or different results than "the first `n` that match."

For multi-hop traversals, apply `.has()` on a hop's properties as soon as you reach that hop, before navigating further away from it — this keeps the fan-out small for every later hop, and isn't just faster: the property may not even exist on a later hop's entity type (e.g. `"age"` belongs to a person, not a product they bought).

```python
# ✅ Filter friends by age right after out("knows"), before expanding purchases
snap.g().V(1).out("knows").has("age", P.gt(30)).out("bought").limit(5).to_list()
```

### Rule 2: Specify Edge Labels in Traversal Steps
Always pass explicit edge labels to `.out()`, `.in()`, or `.both()` when navigating graph topology to avoid scanning irrelevant relationship types:

```rust
// ❌ Slower: Scans all relationship types incident to Vertex 1
snap.g().V([1]).out([])...

// ✅ Faster: Reads only "knows" adjacency records
snap.g().V([1]).out(["knows"])...
```

---

## 3. Vector Search Optimization

### Choosing HNSW Index Parameters

| Scenario | Recommended Parameters | Trade-Off |
| :--- | :--- | :--- |
| **Balanced / General Purpose** | $M=16$, $ef\_construction=128$ | Fast build time, low memory, $>95\%$ recall |
| **High Recall / Precision** | $M=32$, $ef\_construction=256$ | Higher memory & build time, $>99\%$ recall |
| **Memory Constrained** | $M=12$, $ef\_construction=64$, `F16` | Minimum RAM, $<50\%$ memory footprint |

### F16 Quantization by Default
RocksGraph defaults to `Quantization::F16`, halving vector memory consumption (e.g. from $\approx 6\text{ KB}$ in F32 down to $\approx 3\text{ KB}$ per 1536-dimensional vector) with negligible recall impact ($<0.5\%$). Full `F32` precision can be opted into if exact float representations are required:

#### 🦀 Rust
```rust
let config = VectorIndexConfig::new(
    "embedding",
    VectorEntityType::Vertex,
    1536,
    DistanceMetric::Cosine,
    AnnAlgorithm::Hnsw(HnswConfig::default()),
).with_quantization(Quantization::F16);

schema.add_vector_index(config);
```

#### 🐍 Python
```python
from rocksgraph import VectorEntityType, Quantization

schema.add_vector_index(
    entity_type=VectorEntityType.Vertex,
    property="embedding",
    dimension=1536,
    quantization=Quantization.F16,
)
```

---

## 4. Memory Calculations & Sizing Guide

To prevent memory pressure or out-of-memory (OOM) situations, calculate your HNSW memory budget using this sizing formula:

$$\text{RAM (bytes)} \approx N \times \left( \text{dim} \times \text{bytes\_per\_dim} + 2 \times M \times 8 \times 1.28 \text{ bytes} \right)$$

> [!NOTE]
> The $1.28\times$ factor on adjacency links ($2 \times M \times 8$) accounts for the multi-layer HNSW graph hierarchy ($\approx 1 + 1/\ln(M)$ average layers per node for $M=16$).

### Reference Memory Table for 1,000,000 Vectors

| Dimensions | Precision | $M$ Parameter | Approx. RAM |
| :--- | :--- | :--- | :--- |
| **384 (MiniLM)** | F16 (2 bytes) | 16 | **1.10 GB** |
| **384 (MiniLM)** | F32 (4 bytes) | 16 | **1.86 GB** |
| **768 (BGE / Bert)** | F16 (2 bytes) | 16 | **1.86 GB** |
| **768 (BGE / Bert)** | F32 (4 bytes) | 16 | **3.40 GB** |
| **1536 (OpenAI)** | F16 (2 bytes) | 16 | **3.40 GB** |
| **1536 (OpenAI)** | F32 (4 bytes) | 16 | **6.47 GB** |

---

## 5. Concurrency & Scaling Recommendations

1. **Read Scaling**: `ReadSession` is lock-free and scales linearly across CPU cores. For web APIs, maintain a single global `Graph` instance and spawn a lightweight `ReadSession` per incoming HTTP request.
2. **Short Transaction Lifecycles**: Transactions hold snapshot state in memory; commit transactions promptly after mutation to minimize conflict aborts.

---

## Related Topics

- [Vector Search Deep Dive](vector_search.md) — Vector parameters and query primitives.
- [Bulk Loading](bulk_loading.md) — Offline SST import engine.
- [Transactions & Concurrency](concurrency_and_tx.md) — OCC transaction lifecycle.
