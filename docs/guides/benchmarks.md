# Benchmarks

Results are recorded here for each major version. Benchmarks run against the
[SNAP soc-LiveJournal1](https://snap.stanford.edu/data/soc-LiveJournal1.html) dataset
(full 69 M edges, shuffled, or sub-sampled slices). See
[`rocksgraph/src/bin/`](https://github.com/ThouAreAwesome/RocksGraph/tree/main/rocksgraph/src/bin)
for the benchmark binaries:

- `bench_write` — bulk-load (SST ingest) throughput
- `bench_write_occ` — transactional OCC (OLTP) write throughput
- `bench_read` — read query latency/throughput

```rust
// Read benchmark: one ReadSession per thread, reused across all queries
let mut snap = graph.read();
snap.g().V([]).hasId([src]).outE([label]).values(["weight","timestamp"]).count().next()?;

// Bulk-load benchmark: streaming ingest (O(1) memory)
let mut loader = graph.open_bulk_loader()?;
loader.load_vertices(vertices)?;
loader.load_edges(edges)?;
loader.commit()?;

// Transactional OCC benchmark: idempotent upsert via coalesce(), retried on conflict
let mut txn = graph.begin();
txn.g().V([id]).fold().coalesce([
    __().unfold(),
    __().addV(label).property("id", id) /* ... */,
]).next()?;
txn.commit()?;
```

---

## v0.2.2 (2026-08)

### Environment

| | |
|-|-|
| **Binary** | `target/release/bench_read` / `bench_write` / `bench_write_occ` (`cargo run --release`) |
| **Machine** | Apple M3, 16 GB, NVMe SSD |
| **OS** | macOS 15.4.1 |
| **Rust** | 1.95.0 |
| **RocksDB** | 10.4.2 (via `rocksdb` crate 0.24) |
| **RocksOptions** | write_buffer=128 MiB, block_cache=1 GiB (shared), format_version=6 |

Data scale is grouped into three tiers: **1 M** and **10 M** edges for OLTP
(transactional) writes, **10 M** and **69 M** (full dataset) for bulk-load writes, and
all three for reads — giving a bulk-vs-OLTP comparison at the 10 M tier and a read
scaling picture across all three sizes.

---

### Write: Bulk Load (SST ingest)

`BulkLoader` streams vertices and edges through `ExternalSorter`, writes sorted SST
files, and ingests them atomically via `IngestExternalFile` — bypassing WAL, memtable
pressure, and OCC entirely. Followed by an explicit full compaction pass
(`compact_range_cf`) that moves L0 SSTs into deeper levels for fast subsequent reads.

#### 10 M edges (`soc-LiveJournal1-10M.txt` → `data/rocksGraph-10M`)

| | |
|-|-|
| **Vertices** | 3,157,969 |
| **SST files** | 11 (ingested atomically) |

| Phase | Description | Duration |
|-------|-------------|---------:|
| 1a | Stream 3.16 M vertices → `vertex_sorter` + `label_sorter` | 3.0 s |
| 1b | Stream 10 M edges → annotation/degree sorters | 13.4 s |
| 2a | Write vertex SSTs | 0.6 s |
| 2b | Write degree SSTs (three-way merge) | 1.6 s |
| 2c | Annotate + write `edges_out` SSTs | 7.1 s |
| 2d | Annotate + write `edges_in` SSTs | 6.7 s |
| 3 | `IngestExternalFile` (11 SSTs, atomic) | ~0.0 s |
| — | Post-ingest compaction (L0 → deeper levels) | 4.3 s |
| **Total** | | **36.72 s** |

**Throughput: 272,347 edges/s** (end-to-end incl. file parse + compaction).

#### 69 M edges (`soc-LiveJournal1-shuffled.txt` → `data/rocksGraph-shuffled`, full dataset)

| | |
|-|-|
| **Vertices** | 4,847,571 |
| **SST files** | 53 (ingested atomically) |

| Phase | Description | Duration |
|-------|-------------|---------:|
| 1a | Stream 4.85 M vertices → `vertex_sorter` + `label_sorter` | 4.4 s |
| 1b | Stream 69 M edges → annotation/degree sorters | 95.6 s |
| 2a | Write vertex SSTs | 1.3 s |
| 2b | Write degree SSTs (three-way merge) | 11.6 s |
| 2c | Annotate + write `edges_out` SSTs | 57.1 s |
| 2d | Annotate + write `edges_in` SSTs | 59.4 s |
| 3 | `IngestExternalFile` (53 SSTs, atomic) | 0.2 s |
| — | Post-ingest compaction (L0 → deeper levels) | 30.2 s |
| **Total** | | **259.8 s** |

**Throughput: 265,536 edges/s** (end-to-end incl. file parse + compaction). Peak
memory ~1.2 GB (sorter buffers + `SortedLabelFile`; no per-vertex/edge maps).

---

### Write: Transactional OCC (OLTP)

`bench_write_occ` upserts each edge-list line as source vertex + destination vertex +
connecting edge via idempotent Gremlin `coalesce()` patterns inside a `TxnSession`,
retrying on `StoreError::Conflict`. This is the incremental-write path used to append
data to an already-populated database (bulk load only works on an empty database).
Each edge produces 3 mutations (2 vertex upserts + 1 edge upsert).

| Dataset | Parallelism | Edges/s | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|---------|------------:|--------:|--------:|--------:|--------:|--------:|--------:|
| 1 M edges | 3 | 74,558 | 39.3 | 44.0 | 45.8 | 60.2 | 20,267 |
| 10 M edges | 3 | 67,189 | 42.7 | 48.7 | 51.4 | 81.5 | 38,797 |

---

### Read

One `ReadSession` is created per worker thread and reused for all queries in that
thread's chunk (snapshot pinned at session creation). Query parameters (`src`/`dst`
vertex IDs) are sampled from the dataset's edge list; sampling is size-biased toward
higher-degree vertices, since a random edge's endpoint has higher expected degree than
a uniformly random vertex.

**Sample sizes**: the 1 M dataset uses its full 1,000,000-line file. The 10 M and 69 M
datasets use a random sample of 300,000 queries rather than the full file or a
1,000,000-query sample — at this dataset size, sampling a very large number of
*distinct* vertices spread across the full keyspace was found to trigger severe,
non-linear slowdowns unrelated to per-query cost (observed via profiling to be
allocator/cache overhead from touching many distinct storage blocks under sustained
random access, not a data or hardware issue). 300,000 samples was verified to run
cleanly and completes in reasonable time while remaining far larger than a
back-of-envelope sample.

**Q6/Q7 use `.limit(10000)`**: both are 2-hop `dedup()` traversals, and hub vertices in
this graph can produce very large fan-outs. An explicit limit bounds worst-case cost —
matching this project's own recommended pattern for multi-hop traversals — and only
affects the extreme tail of the distribution; at 10 M scale the natural (unbounded)
result count only reaches ~2,600 at p99, well under the 10,000 cap.

**Q2 vs Q3 ordering**: both traverse the same sampled source vertices' out-edges — Q2
as an unfiltered scan (`InOutStep`), Q3 as a point lookup for a specific `dst`
(`GetEStep`). They run in that order intentionally: whichever runs first pays the cold
cache cost of pulling each vertex's edge block off SSD, and the other inherits that
warmth. Treat Q2 as "cold scan cost" and Q3 as "warm point-lookup cost" rather than a
fair head-to-head comparison.

**Q8/Q9** (full vertex/edge scans) always run exactly once regardless of sample size,
since a full scan deterministically covers the same dataset every time.

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | Point lookup + 2 vertex property reads |
| Q2 | `g.V().hasId(id).outE(label).values('weight','timestamp').count()` | Full out-edge scan + 2 edge property reads per edge |
| Q3 | `g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | Out-edge point lookup (`GetEStep`) + 2 edge property reads |
| Q4 | `g.V().hasId(id).outE(label).values('weight','timestamp').limit(5).count()` | Q2 with early termination at 5 results |
| Q5 | `g.V().hasId(id).out(label).values('name','age').count()` | Out-neighbor scan + 2 vertex property reads per neighbor |
| Q6 | `g.V(id).out(label).hasLabel(v_label).dedup().out(label).hasLabel(v_label).dedup().hasId(not(id)).limit(10000).count()` | 2-hop outbound traversal, label filter, dedup, self-exclusion, capped (unrolled) |
| Q7 | `g.V(id).repeat(out(label).hasLabel(v_label).dedup()).times(2).hasId(not(id)).limit(10000).count()` | Same as Q6 via `repeat().times(2)` |
| Q8 | `g.V().count()` | Full vertex scan |
| Q9 | `g.E([]).count()` | Full edge scan |

#### Results — 1 M edges (`data/rocksGraph-1M`, bulk-loaded, 1,000,000 queries, parallelism 5)

| Query | Ops/s | Mean (μs) | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 687,219 | 7.1 | 4.8 | 10.3 | 16.9 | 30.7 | 25,002 |
| Q2 | 371,214 | 13.3 | 9.8 | 19.2 | 30.0 | 50.3 | 19,399 |
| Q3 | 455,053 | 10.7 | 8.5 | 15.0 | 25.0 | 40.0 | 1,065 |
| Q4 | 363,290 | 13.6 | 10.7 | 21.7 | 31.0 | 48.1 | 1,093 |
| Q5 | 224,821 | 22.0 | 15.2 | 34.4 | 52.3 | 104.6 | 61,014 |
| Q6 | 173,819 | 28.4 | 19.1 | 48.5 | 66.7 | 131.7 | 47,448 |
| Q7 | 179,301 | 27.6 | 18.9 | 48.1 | 65.9 | 127.8 | 3,518 |
| Q8 | 2.19 | 342,229 | — | — | — | — | — |
| Q9 | 1.82 | 434,242 | — | — | — | — | — |

#### Results — 10 M edges (`data/rocksGraph-10M`, bulk-loaded, 300,000 sampled queries, parallelism 5)

| Query | Ops/s | Mean (μs) | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 655,472 | 7.2 | 5.3 | 11.2 | 18.0 | 30.7 | 1,438 |
| Q2 | 216,278 | 22.4 | 15.8 | 36.7 | 50.2 | 85.4 | 3,631 |
| Q3 | 444,982 | 10.8 | 8.8 | 15.7 | 25.1 | 37.1 | 221 |
| Q4 | 286,122 | 17.0 | 13.5 | 28.9 | 34.4 | 59.1 | 656 |
| Q5 | 60,283 | 80.9 | 39.4 | 158.6 | 244.5 | 481.0 | 32,047 |
| Q6 | 17,341 | 281.8 | 100.5 | 666.6 | 1,144.8 | 2,635.8 | 15,958 |
| Q7 | 16,017 | 306.6 | 107.9 | 722.4 | 1,243.1 | 2,873.3 | 25,281 |
| Q8 | 0.69 | 958,136 | — | — | — | — | — |
| Q9 | 0.15 | 4,821,352 | — | — | — | — | — |

#### Results — 69 M edges (`data/rocksGraph-shuffled`, bulk-loaded, full dataset, 300,000 sampled queries, parallelism 5)

| Query | Ops/s | Mean (μs) | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 514,935 | 9.4 | 5.2 | 10.3 | 18.8 | 121.3 | 1,968 |
| Q2 | 28,764 | 171.8 | 143.7 | 308.0 | 404.0 | 664.1 | 24,543 |
| Q3 | 220,400 | 22.2 | 16.3 | 30.7 | 47.8 | 149.9 | 15,081 |
| Q4 | 132,226 | 37.3 | 34.4 | 50.2 | 73.8 | 100.4 | 2,000 |
| Q5 | 10,352 | 478.3 | 221.4 | 973.3 | 1,509.4 | 2,959.4 | 91,292 |
| Q6 | 632 | 7,855.7 | 4,210.7 | 16,638.0 | 23,380.0 | 60,325.9 | 364,642 |
| Q7 | 603 | 8,235.5 | 4,337.7 | 17,678.3 | 24,936.4 | 62,914.6 | 325,583 |
| Q8 | 0.40 | 1,729,626 | — | — | — | — | — |
| Q9 | 0.00 (254.8 s) | 231,592,690 | — | — | — | — | — |

Q8/Q9's single-sample latency is reported as mean only — percentiles collapse to the
same value with `n=1`.

### Vector Index Overhead

The vector index benchmarks measure the mechanism cost of vector indexing (WAL writes, per-commit checkpoint accounting, incremental HNSW insert calls) separately from the rest of the RocksGraph system. 

The vectors used are synthetic L2-normalized unit vectors drawn from a standard normal distribution. The topology reused is the LiveJournal edge list.

#### Bulk Load and Index Build

The following table isolates the SST ingest wall time from the vector index build wall time using `bench_vector_bulk_load`:

| Scale | Dimension | Quantization | Ingest Time (s) | Index Build Time (s) | Total Time (s) | Throughput (edges/s) |
|---|---|---|---|---|---|---|
| 10M edges / 3.16M vertices | 128 | F32 | | | | |
| 10M edges / 3.16M vertices | 384 | F32 | | | | |
| 10M edges / 3.16M vertices | 768 | F32 | | | | |
| 10M edges / 3.16M vertices | 1536 | F32 | | | | |
| 10M edges / 3.16M vertices | 384 | F16 | | | | |

#### Transactional OCC Write with Vector Indexing

The following table measures the throughput and latency overhead of maintaining a vector index synchronously during a transactional OCC write workload (`bench_write_occ`), isolating the cost of the per-commit checkpoint trigger accounting:

| Scale | Dimension | Parallelism | Checkpoint Threshold | Throughput (edges/s) | p50 Latency (μs) | p99 Latency (μs) |
|---|---|---|---|---|---|---|
| 10M edges | 0 (baseline) | 3 | None | | | |
| 10M edges | 384 | 3 | None | | | |
| 10M edges | 384 | 3 | 1000 | | | |
| 10M edges | 384 | 16 | None | | | |
| 10M edges | 384 | 16 | 1000 | | | |
