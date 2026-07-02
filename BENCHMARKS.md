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
| **Parallelism** | 3 concurrent workers (write and read) |
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
| Q1 | `g.V().hasId(id).values('name','age').count()` | Point lookup + property projection |
| Q2 | `g.V().hasId(id).bothE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | Edge point-lookup (`GetEStep`) + property projection |
| Q3 | `g.V().hasId(id).bothE(label).values('weight','timestamp').count()` | Bidirectional edge scan + property projection |
| Q4 | `g.V().hasId(id).both(label).values('name','age').count()` | Bidirectional neighbor scan + property projection |
| Q5 | `g.V(id).out(label).both(label).count()` | 2-hop traversal (mixed directions) |
| Q6 | `g.V(id).out(label).both(label).hasLabel(v_label).count()` | 2-hop traversal with endpoint label filter |
| Q7 | `g.V().count()` | Full vertex scan (1,093,302 vertices) |
| Q8 | `g.E().count()` | Full edge scan (1,000,000 edges) |

#### Results

| Query | Ops/s | Queries | Duration | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 871,114 | 1,000,000 | 1.15 s | 3.38 | 3.75 | 3.96 | 6.04 | 262.7 |
| Q2 | 379,669 | 1,000,000 | 2.63 s | 7.25 | 7.92 | 8.46 | 14.42 | 1,159 |
| Q3 | 295,365 | 1,000,000 | 3.39 s | 9.22 | 12.26 | 13.80 | 19.14 | 373.5 |
| Q4 | 194,232 | 1,000,000 | 5.15 s | 12.71 | 22.88 | 28.80 | 47.97 | 792.1 |
| Q5 | 137,899 | 1,000,000 | 7.25 s | 14.38 | 36.35 | 51.94 | 91.01 | 2,103 |
| Q6 | 129,741 | 1,000,000 | 7.71 s | 15.34 | 38.88 | 55.68 | 98.37 | 27,083 |
| Q7 | 3.63 | 5 | 1.38 s | 228,983 | 301,203 | 301,203 | 301,203 | 301,203 |
| Q8 | 2.76 | 5 | 1.81 s | 311,427 | 439,353 | 439,353 | 439,353 | 439,353 |

#### RocksDB storage profile (`--features rocksdb-stats`)

| Metric | Value | Interpretation |
|--------|------:|----------------|
| Block cache capacity | 256 MB | Shared across all 4 data CFs |
| Data block hit rate | 99.7% (23,756,373 hits / 68,785 misses) | Working set fits in block cache |
| Bloom filter skip rate | 8.3% (972,034 useful / 10,763,484 full positive) | Low skip — dense working set; bloom filter checks are always true-positive |
| Bloom false-positive rate | <0.09% (9,612 / 10,763,484) | Filter accuracy is excellent |
| L0 files per CF | 1 each (~25–28 MB) | Data is uncompacted; all records in a single L0 SST file per CF |
| SST file read P50 | < 1 µs (vertices, edges_out, edges_in) | Reads served from OS page cache or block cache |
| Compaction | 0 bytes written | No compaction has run since data was loaded |

#### Notes

- **Q1 p50 at 3.4 µs**: sub-4 µs for a full vertex point-lookup plus two-property decode.  The v2
  offset-index blob format and `PropertyMap` enable O(log P) single-key lookups without full
  property materialization.
- **Write throughput at 90.9 K/s**: upsert-heavy workload (3 OCC operations per edge: 2 vertex
  coalesce + 1 edge write).  p50 latency at 33 µs is bounded by RocksDB write + OCC validation.
- **Q5–Q6 max at 2–27 ms**: the LiveJournal graph follows a power-law degree distribution.
  Hub vertices with thousands of out-edges cause multi-hop traversals to touch proportionally
  more data blocks.  Q6 max at 27 ms is a single outlier (one hub with an extremely high
  out-degree).
- **Bloom filter skip rate (8.3%)**: the 1 M working set fits in a single L0 SST file per CF.
  Most point-lookup keys are present (true-positive bloom), so useful bloom skips are low.
  Skip rate will improve as the dataset grows beyond the block cache and requires multi-level
  compaction.
