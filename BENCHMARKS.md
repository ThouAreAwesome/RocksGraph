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

## v0.1.0 (2026-07)

### Environment

| | |
|-|-|
| **Binary** | `target/release/bench_read` / `bench_write` (`cargo run --release`) |
| **Dataset** | soc-LiveJournal1, 1 M edges, shuffled (`bench_data/soc-LiveJournal1-1M.txt`) |
| **Data dir** | `data/rocksGraph-1M` |
| **Parallelism** | 3 (write) / 5 (read) concurrent workers |
| **Machine** | Apple M3, 16GB, SSD 256GB |
| **OS** | macOS 15.4 |
| **Rust** | 1.95.0 |
| **RocksDB** | 10.4.2 (via `rocksdb` crate 0.24) |
| **RocksOptions** | write_buffer=128 MiB, block_cache=256 MiB (shared), format_version=6 |

---

### Write: Insert Vertex and Edge

Each transaction upserts source vertex, destination vertex, and the connecting edge using
Gremlin `coalesce` patterns (idempotent). OCC conflicts are retried up to 5 times with
randomised back-off.

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Upsert vertex | `g.V(id).coalesce(__.V(id).values('id'), __.addV(label).property(...))` | Idempotent upsert |
| Upsert edge | `g.V(src).coalesce(__.outE(label).where(otherV().hasId(dst)).label(), __.addE(label).from(src).to(dst).property(...))` | Idempotent upsert |

#### Results

| Query | Mutations/s | Total | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------------:|------:|--------:|--------:|--------:|--------:|---------:|
| Upsert (2V + 1E) | 69444 | 10,000,000 | 41.087 | 47.391 | 50.335 | 91.967 | 31916.031 |

---

### Read

One `ReadSession` is created per worker thread and reused for all queries in that thread's
chunk (snapshot pinned at session creation). Caches are cleared between queries via
`snap.clear_caches()` to simulate cold-start per-query access.

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | Point lookup + property projection |
| Q2 | `g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | Edge point-lookup (GetEStep) with property projection |
| Q3 | `g.V().hasId(id).bothE(label).values('weight','timestamp').count()` | Bidirectional edge scan + property projection |
| Q4 | `g.V().hasId(id).both(label).values('name','age').count()` | Bidirectional neighbor scan + property projection |
| Q5 | `g.V(id).out(label).both(label).count()` | 2-hop traversal (mixed directions) |
| Q6 | `g.V(id).out(label).both(label).hasLabel(v_label).count()` | 2-hop traversal with endpoint label filter |
| Q7 | `g.V().count()` | Full vertex scan (1,093,302 vertices) |
| Q8 | `g.E().count()` | Full edge scan (1,000,000 edges) |

#### Results

| Query | Ops/s | Queries | Duration | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 981,852 | 10,000,000 | 1.02 s | 4.167 | 8.423 | 9.423 | 10.671 | 1,395.7 |
| Q2 | 469,460 | 10,000,000 | 2.13 s | 8.671 | 16.543 | 17.423 | 25.375 | 1,421.3 |
| Q3 | 353,006 | 10,000,000 | 2.83 s | 11.295 | 19.631 | 24.175 | 36.255 | 1,445.9 |
| Q4 | 240,695 | 10,000,000 | 4.15 s | 15.255 | 32.255 | 41.727 | 72.959 | 1,577.0 |
| Q5 | 170,747 | 10,000,000 | 5.86 s | 17.967 | 49.599 | 71.551 | 136.575 | 6,819.8 |
| Q6 | 159,922 | 10,000,000 | 6.25 s | 19.215 | 52.735 | 75.839 | 144.895 | 19,284.0 |
| Q7 | 2.59 | 5 | 1.93 s | 268,042 | 329,515 | 329,515 | 329,515 | 329,515 |
| Q8 | 2.24 | 5 | 2.23 s | 333,971 | 420,479 | 420,479 | 420,479 | 420,479 |

#### Notes

- **Q5–Q6 max latency (6–19 ms)**: the LiveJournal graph follows a power-law degree
  distribution — a small fraction of hub vertices have thousands of edges, causing large
  per-query variance on multi-hop traversals. This is an intrinsic property of the dataset,
  not a storage bottleneck.
- **Q7/Q8 full scans (~270–334 ms)**: data is uncompacted after bulk load (L0 files only).
  Running a force compaction after ingestion would reorganise SSTs for sequential access
  and roughly halve full-scan latency. Run `scripts/bench_integrity.sh` to verify
  degree-counter correctness before compacting.
