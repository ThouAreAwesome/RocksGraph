# Benchmarks

Results are recorded here for each major version. All benchmarks run against the
[SNAP soc-LiveJournal1](https://snap.stanford.edu/data/soc-LiveJournal1.html) dataset
(1 M edges, shuffled). See [`scripts/prepare_bench_data.sh`](scripts/prepare_bench_data.sh)
for dataset preparation and [`src/bin/`](src/bin/) for the benchmark binaries.

Benchmark binaries use the public `Graph` / `ReadSession` / `TxSession` API:

```rust
// Read benchmark: one ReadSession per thread, reused across all queries
let mut snap = graph.read();
snap.g().V([]).hasId([src]).bothE([label]).values(["weight","timestamp"]).count().next()?;

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
| **Parallelism** | 5 concurrent workers (read); 3 concurrent workers (write) |
| **Machine** | Apple M3, 16 GB, SSD |
| **OS** | macOS 15.4 |
| **Rust** | 1.95.0 |
| **RocksDB** | 10.4.2 (via `rocksdb` crate 0.24) |
| **RocksOptions** | write_buffer=128 MiB, block_cache=256 MiB (shared), format_version=6 |

---

### Write: Insert Vertex and Edge

Each transaction upserts source vertex, destination vertex, and the connecting edge using
Gremlin `coalesce` patterns (idempotent). OCC conflicts are retried with randomised
back-off.

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Upsert vertex | `g.V(id).coalesce(__.V(id).values('id'), __.addV(label).property(...))` | Idempotent upsert |
| Upsert edge | `g.V(src).coalesce(__.outE(label).where(otherV().hasId(dst)).label(), __.addE(label).from(src).to(dst).property(...))` | Idempotent upsert |

#### Results

| Query | Mutations/s | Total | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------------:|------:|--------:|--------:|--------:|--------:|---------:|
| Upsert (2V + 1E) | 90,909 | 1,000,000 | 33.2 | 37.9 | 39.5 | 46.3 | 7,156 |

---

### Read

One `ReadSession` is created per worker thread and reused for all queries in that thread's
chunk (snapshot pinned at session creation). Caches are cleared between queries via
`snap.clear_caches()` to simulate cold-start per-query access.


#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | Point lookup + 2 vertex property reads |
| Q2 | `g.V().hasId(id).bothE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | Incident edge scan + endpoint filter + 2 edge property reads |
| Q3 | `g.V().hasId(id).bothE(label).values('weight','timestamp').count()` | Full incident edge scan + 2 edge property reads (no filter) |
| Q4 | `g.V().hasId(id).bothE(label).values('weight','timestamp').limit(5).count()` | Q3 with early termination at 5 results |
| Q5 | `g.V().hasId(id).both(label).values('name','age').count()` | Neighbor vertex scan + 2 vertex property reads per neighbor |
| Q6 | `g.V(id).out(label).hasLabel(v_label).dedup().out(label).hasLabel(v_label).dedup().hasId(not(id)).count()` | 2-hop outbound traversal, label filter, dedup, self-exclusion (unrolled) |
| Q7 | `g.V(id).repeat(out(label).hasLabel(v_label).dedup()).times(2).hasId(not(id)).count()` | Same as Q6 via `repeat().times(2)` (tests loop-driver overhead) |
| Q8 | `g.V().count()` | Full vertex scan (1,093,302 vertices) |
| Q9 | `g.E([]).count()` | Full edge scan (1,000,000 edges) |

#### Results

| Query | Ops/s | Queries | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 855,088 | 1,000,000 | 4.5 | 9.8 | 10.7 | 12.3 | 5,001 |
| Q2 | 442,240 | 1,000,000 | 8.6 | 18.5 | 19.8 | 33.3 | 29,164 |
| Q3 | 338,916 | 1,000,000 | 11.3 | 23.2 | 28.8 | 49.2 | 1,710 |
| Q4 | 394,466 | 1,000,000 | 10.4 | 20.7 | 23.4 | 29.9 | 1,761 |
| Q5 | 237,932 | 1,000,000 | 15.2 | 32.8 | 42.6 | 74.7 | 3,207 |
| Q6 | 233,569 | 1,000,000 | 14.7 | 33.1 | 43.7 | 79.3 | 2,476 |
| Q7 | 230,234 | 1,000,000 | 13.8 | 32.1 | 43.1 | 79.8 | 37,323 |
| Q8 | 2.3 | 5 | 286,786 | 408,945 | 408,945 | 408,945 | 408,945 |
| Q9 | 2.1 | 5 | 348,914 | 451,150 | 451,150 | 451,150 | 451,150 |

#### Notes

- **Q1 at 855 K ops/s, p50 4.5 µs**: vertex point-lookup plus 2 property reads. The
  offset-index blob format and `PropertyMap` enable O(log P) single-key lookups without
  full property materialisation.
- **Q2 > Q3 throughput (442 K vs 339 K)**: Q2 includes a `where(otherV().hasId(dst))` filter
  that in practice matches only 1–2 edges per source vertex, limiting the property-read work.
  Q3 reads all incident edges unconditionally, so it processes more properties per query.
- **Q4 > Q3 throughput (394 K vs 339 K)**: `limit(5)` triggers early termination — most
  vertices have more than 5 incident edges, so Q4 saves significant property-read work.
- **Q6 ≈ Q7 throughput (234 K vs 230 K)**: `repeat().times(2)` imposes less than 2%
  overhead over the manually unrolled 2-hop traversal, confirming the loop driver is
  near-zero cost for small iteration counts.
- **Q8/Q9 at ~2 ops/s (287–349 ms/scan)**: full-DB scan of 1 M+ vertices / 1 M edges.
  All data fits in the 256 MB block cache so repeated scans are OS-cache-warm.
- **Power-law outliers**: Q2 max at 29 ms and Q7 max at 37 ms are single-query outliers
  caused by hub vertices with thousands of incident edges — expected for the LiveJournal
  social graph.
- **Write throughput at 90.9 K/s**: upsert-heavy workload (2 vertex coalesces + 1 edge
  write per input edge). p50 at 33 µs is bounded by RocksDB write + OCC validation.
