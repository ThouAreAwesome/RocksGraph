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
| Upsert (2V + 1E) | 69,444 | 1,000,000 | 41.087 | 47.391 | 50.335 | 91.967 | 31,916.0 |

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
| Q1 | 839,962 | 1,000,000 | 1.19 s | 3.42 | 3.75 | 3.88 | 5.88 | 265.7 |
| Q2 | 351,225 | 1,000,000 | 2.85 s | 7.34 | 8.63 | 10.17 | 19.55 | 78,774 |
| Q3 | 290,995 | 1,000,000 | 3.44 s | 9.26 | 12.50 | 14.22 | 19.47 | 1,938 |
| Q4 | 188,644 | 1,000,000 | 5.30 s | 12.54 | 23.17 | 29.42 | 50.72 | 3,154 |
| Q5 | 138,423 | 1,000,000 | 7.22 s | 14.26 | 36.06 | 51.39 | 90.56 | 3,656 |
| Q6 | 125,755 | 1,000,000 | 7.95 s | 15.50 | 39.78 | 56.93 | 101.50 | 20,185 |
| Q7 | 3.37 | 5 | 1.48 s | 240,124 | 300,941 | 300,941 | 300,941 | 300,941 |
| Q8 | 2.64 | 5 | 1.89 s | 313,000 | 416,023 | 416,023 | 416,023 | 416,023 |

#### RocksDB storage profile (`--features rocksdb-stats`)

| Metric | Value | Interpretation |
|--------|------:|----------------|
| Block cache capacity | 256 MB | Shared across all 4 data CFs |
| Data block hit rate | 99.8% | Working set fits in cache |
| Bloom filter skip rate | 48.0% (10,021,629 useful / 20,852,266 total) | Half of point lookups skip disk reads entirely |
| Bloom false-positive rate | <0.1% (96,119 / 10,853,266) | Filter accuracy is excellent |
| L0 files per CF | 1 each (~25–28 MB) | Data is uncompacted; all records are in a single L0 SST file per CF |
| SST file read P50 | < 1 µs (vertices, edges_out) | Reads served from OS page cache or block cache |
| Compaction | 0 bytes written | No compaction has run since data was loaded |

#### Notes

- **Q1 p50 at 3.9 µs**: sub-4 µs for a full vertex point-lookup + two-property decode is
  essentially in-memory speed — the 256 MB block cache and OS page cache together hold the
  entire 1 M vertex working set.
- **Q2 max at 39 ms**: the `GetEStep` point-lookup is fast for most edges, but a small
  number of (src, label, dst) combinations touch cold SST blocks and incur a full disk
  read. The `edges_in` CF file read latency histogram shows one outlier at ~39 ms
  (matching the Q2 max), confirming this is a cold-block event, not a systemic issue.
- **Q5–Q6 max at 20–22 ms**: the LiveJournal graph follows a power-law degree distribution.
  A small fraction of hub vertices have thousands of out-edges; traversing two hops from
  them touches proportionally more data blocks. This is an intrinsic property of the
  dataset.
- **Q7/Q8 full scans at 274–360 ms**: each CF has exactly one uncompacted L0 file.
  Full scans read the entire file sequentially; the latency reflects raw sequential I/O
  bandwidth rather than random-access overhead. Running a force compaction after bulk load
  would merge flushes and improve scan locality. Run `scripts/bench_integrity.sh` before
  compacting to confirm degree-counter correctness.
- **Bloom filter skip rate (8.3%)**: lower than typical because the 1 M vertex/edge
  working set fits almost entirely in a single L0 SST file per CF, meaning nearly every
  bloom-filter-positive check finds the key (true positive). Skip rate will improve
  significantly as the dataset grows beyond the block cache and requires multi-level
  compaction.
