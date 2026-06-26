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
| **Parallelism** | 3 (write) / 5 (read) concurrent workers |
| **Machine** | Apple M3, 16G, SSD 256G |
| **OS** | macOS 15.4.1 |
| **Rust** | 1.95.0 |

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
| Upsert (2V + 1E) | 100,000 | 1,000,000 | 30.927 | 35.903 | 38.143 | 56.031 | 13,901.8 |

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
| Q6 | `g.V(id).out(label).both(label).hasLabel(v_label).count()` | 2-hop traversal with endpoint label filter |
| Q7 | `g.V().count()` | Full vertex scan |
| Q8 | `g.E().count()` | Full edge scan |

#### Results

| Query | Ops/s | Queries | Duration | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 922,655.80 | 1,000,000 | 1.08 s | 3.417 | 7.295 | 9.839 | 17.055 | 39,026.7 |
| Q2 | 524,982.39 | 1,000,000 | 1.90 s | 7.875 | 13.671 | 19.471 | 29.711 | 4,333.6 |
| Q3 | 277,940.98 | 1,000,000 | 3.60 s | 14.711 | 26.127 | 35.391 | 55.519 | 24,821.8 |
| Q4 | 182,428.97 | 1,000,000 | 5.48 s | 20.335 | 43.519 | 57.375 | 101.887 | 23,150.6 |
| Q5 | 117,041.44 | 1,000,000 | 8.54 s | 27.471 | 74.431 | 102.335 | 188.671 | 14,442.5 |
| Q6 | 110,856.19 | 1,000,000 | 9.02 s | 28.879 | 78.527 | 108.031 | 197.759 | 6,545.4 |
| Q7 | 3.39 | 5 | 1.48 s | 246,939.6 | 304,087.0 | 304,087.0 | 304,087.0 | 304,087.0 |
| Q8 | 2.53 | 5 | 1.97 s | 333,185.0 | 427,819.0 | 427,819.0 | 427,819.0 | 427,819.0 |
