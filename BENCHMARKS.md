# Benchmarks

Results are recorded here for each major version. All benchmarks run against the
[SNAP soc-LiveJournal1](https://snap.stanford.edu/data/soc-LiveJournal1.html) dataset
(1 M edges, shuffled). See [`scripts/prepare_bench_data.sh`](scripts/prepare_bench_data.sh)
for dataset preparation and [`src/bin/`](src/bin/) for the benchmark binaries.

Benchmark binaries use the public `Graph` / `ReadSession` / `TxSession` API:

```rust
// Read benchmark: one ReadSession per thread, reused across all queries
let mut snap = graph.read();
snap.g().V([]).hasId([src]).outE([label]).where(__().otherV().hasId([dst])).count().next()?;

// Write benchmark: one TxSession per edge (with OCC retry on conflict)
let mut tx = graph.begin();
tx.g().V([src]).coalesce([__().V([src]).values(["id"]), __().addV(label).property(...)]).next()?;
tx.commit()?;
```

---

## v0.1.0

### Environment

| | |
|-|-|
| **Binary** | `target/release/bench_read` / `bench_write` (`just build-release`) |
| **Dataset** | soc-LiveJournal1, 1 M edges, shuffled (`bench_data/soc-LiveJournal1-shuffled.txt`) |
| **Data dir** | `data/rocksGraph_shuffled` |
| **Parallelism** | 3 concurrent workers |
| **Machine** | Apple M3, 16G, SSD 256G |
| **OS** | macOS 15.4.1 |

---

### Write: Insert Vertex and Edge

The benchmark performs an atomic `TxSession` for every edge in the source dataset. Each transaction contains two vertex upserts and one edge upsert using Gremlin `coalesce` patterns to ensure idempotency. Conflicts (OCC) are retried up to 3 times with a 1 ms back-off.

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Upsert vertex | `g.V(id).coalesce(__.V(id).values('id'), __.addV(label).property(...))` | Idempotent upsert |
| Upsert edge | `g.V(src).coalesce(__.outE(label).where(otherV().hasId(dst)).values('label'), __.addE(label).from(src).to(dst).property(...))` | Idempotent upsert |

#### Results

Each mutation upserts source vertex, destination vertex, and the connecting edge.

| Query | Mutations/s | Total | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------------:|------:|--------:|--------:|--------:|--------:|---------:|
| Upsert (2V + 1E) | 83,333 | 1,000,000 | 32.511 | 40.319 | 50.047 | 112.063 | 63,307.8 |

---

### Read

One `ReadSession` is created per worker thread and reused for all queries in that thread's chunk (snapshot is pinned at session creation time).

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | Point lookup + property projection |
| Q2 | `g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | Edge traversal with end-vertex filter |
| Q3 | `g.V().hasId(id).bothE(label).values('weight','timestamp').count()` | Bidirectional edge scan + property projection |
| Q4 | `g.V().hasId(id).both(label).values('name','age').count()` | Bidirectional neighbor scan + property projection |
| Q5 | `g.V(id).out(label).both(label).count()` | 2-hop traversal (mixed directions) |

#### Results

| Query | Ops/s | Queries | Duration | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 667,045.92 | 1,000,000 | 1.50 s | 2.751 | 5.291 | 5.583 | 7.419 | 5,595.1 |
| Q2 | 369,685.32 | 1,000,000 | 2.71 s | 5.835 | 10.127 | 10.799 | 20.303 | 1,663.0 |
| Q3 | 180,996.04 | 1,000,000 | 5.52 s | 13.879 | 22.639 | 26.255 | 42.943 | 3,645.4 |
| Q4 | 124,975.85 | 1,000,000 | 8.00 s | 18.591 | 36.799 | 45.855 | 74.559 | 1,385.5 |
| Q5 | 72,109.28 | 1,000,000 | 13.87 s | 25.503 | 72.703 | 103.935 | 186.623 | 12,107.8 |
