# Benchmarks

Results are recorded here for each major version. Benchmarks run against the
[SNAP soc-LiveJournal1](https://snap.stanford.edu/data/soc-LiveJournal1.html) dataset
(full 69 M edges, shuffled, or sub-sampled slices). See [`scripts/`](scripts/) for
dataset preparation and [`src/bin/`](src/bin/) for the benchmark binaries.

Benchmark binaries use the public `Graph` / `ReadSession` / `SstBulkLoader` API:

```rust
// Read benchmark: one ReadSession per thread, reused across all queries
let mut snap = graph.read();
snap.g().V([]).hasId([src]).bothE([label]).values(["weight","timestamp"]).count().next()?;

// Write benchmark: SST bulk load (streaming, O(1) memory)
SstBulkLoader::new(db_path, work_dir)
    .load_initial(schema, vertices, edges, GraphOptions::default(), &RocksOptions::default())?;
```

---

## v0.1.0 (2026-07)

### Environment

| | |
|-|-|
| **Binary** | `target/release/bench_read` / `bench_write` (`cargo run --release`) |
| **Machine** | Apple M2, 16 GB, NVMe SSD |
| **OS** | macOS 15.4 |
| **Rust** | 1.95.0 |
| **RocksDB** | 10.4.2 (via `rocksdb` crate 0.24) |
| **RocksOptions** | write_buffer=128 MiB, block_cache=1 GiB (shared), format_version=6 |

---

### Write: SST Bulk Load (soc-LiveJournal1, full shuffled, 69 M edges)

`SstBulkLoader` streams vertices and edges through `ExternalSorter`, writes sorted SST
files, and ingests them atomically — bypassing WAL, memtable pressure, and OCC entirely.
Followed by a full compaction pass to move L0 SSTs into deeper levels.

| | |
|-|-|
| **Dataset** | soc-LiveJournal1, **68,993,773 edges** (full shuffled), `soc-LiveJournal1-shuffled.txt` |
| **Vertices** | 4,847,571 |
| **Data dir** | `data/rocksGraph-shuffled` |
| **EdgeMode** | `Single` (rank=0 for all edges) |
| **SST files** | 53 (ingested atomically via `IngestExternalFile`) |

#### Phase breakdown

| Phase | Description | Duration |
|-------|-------------|----------|
| 1a | Stream 4.85M vertices → `vertex_sorter` + `label_sorter` → `SortedLabelFile` | 4.4 s |
| 1b | Stream 69M edges → 4 annotation/degree sorters | 95.6 s |
| 2a | Write vertex SSTs | 1.3 s |
| 2b | Write degree SSTs (three-way merge) | 11.6 s |
| 2c | Annotate + write `edges_out` SSTs | 57.1 s |
| 2d | Annotate + write `edges_in` SSTs | 59.4 s |
| 3 | `IngestExternalFile` (53 SSTs, atomic) | 0.2 s |
| — | Post-ingest compaction (L0 → L2) | 30.2 s |
| **Total** | | **259.8 s** |

#### Summary

| Metric | Value |
|--------|-------|
| **Throughput** | **265,536 edges/s** (end-to-end incl. file parse + compaction) |
| **Elapsed** | 259.8 s |
| **Peak memory** | ~1.2 GB (sorter buffers + `SortedLabelFile`; no per-vertex/edge maps) |

---

### Write: Transactional OCC — incremental writes to an existing DB (1 M edges)

`SstBulkLoader` only works on an empty database.  For **incremental writes** to a live
database — appending new vertices and edges after the initial bulk load — use `TxSession`.
Each transaction upserts source vertex, destination vertex, and the connecting edge using
Gremlin `coalesce` patterns (idempotent). OCC conflicts are retried with randomised
back-off.

| | |
|-|-|
| **Dataset** | soc-LiveJournal1, 1 M edges, shuffled (`soc-LiveJournal1-1M.txt`) |
| **Data dir** | `data/rocksGraph-1M` |
| **Parallelism** | 3 concurrent workers |

#### Results

| Query | Mutations/s | Total | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------------:|------:|--------:|--------:|--------:|--------:|---------:|
| Upsert (2V + 1E) | 90,909 | 1,000,000 | 33.2 | 37.9 | 39.5 | 46.3 | 7,156 |

---

### Read (soc-LiveJournal1, full shuffled — 69 M edges, 4.85 M vertices)

One `ReadSession` is created per worker thread and reused for all queries in that thread's
chunk (snapshot pinned at session creation). Caches are cleared between queries via
`snap.clear_caches()` to simulate cold-start per-query access (this clears only the
session-local vertex/edge maps — the shared RocksDB block cache persists across queries).

| | |
|-|-|
| **Dataset** | soc-LiveJournal1, **68,993,773 edges**, 4,847,571 vertices (full shuffled) |
| **Data dir** | `data/rocksGraph-shuffled` (bulk-loaded, post-compaction) |
| **Parallelism** | 5 concurrent workers |
| **Query sample** | 10,000 random pairs per benchmark (`--queries 10000`) for Q1–Q7; Q8/Q9 (full scans) run once — a full scan deterministically covers the same dataset every time |

#### Query Definitions

Q2 and Q3 both traverse `outE(label)` over the same sampled source vertices — Q2 as an
unfiltered scan, Q3 as a point lookup for one specific `dst`. They intentionally run in
that order (scan first, point lookup second); see the cache note below.

| ID | Traversal | Sample used | Pattern |
|----|-----------|-------------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | vertex | Point lookup + 2 vertex property reads |
| Q2 | `g.V().hasId(id).outE(label).values('weight','timestamp').count()` | edge pair | Full out-edge scan + 2 edge property reads per edge |
| Q3 | `g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | edge pair | Out-edge point lookup (GetEStep) + 2 edge property reads |
| Q4 | `g.V().hasId(id).outE(label).values('weight','timestamp').limit(5).count()` | edge pair | Q2 with early termination at 5 results |
| Q5 | `g.V().hasId(id).out(label).values('name','age').count()` | vertex | Out-neighbor scan + 2 vertex property reads per neighbor |
| Q6 | `g.V(id).out(label).hasLabel(v_label).dedup().out(label).hasLabel(v_label).dedup().hasId(not(id)).count()` | vertex | 2-hop outbound traversal, label filter, dedup, self-exclusion (unrolled) |
| Q7 | `g.V(id).repeat(out(label).hasLabel(v_label).dedup()).times(2).hasId(not(id)).count()` | vertex | Same as Q6 via `repeat().times(2)` |
| Q8 | `g.V().count()` | — | Full vertex scan (4,847,571 vertices) |
| Q9 | `g.E([]).count()` | — | Full edge scan (68,993,773 edges) |

#### Results

| Query | Ops/s | Queries | Mean (μs) | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 87,058 | 10,000 | 56.1 | 8.1 | 153.9 | 187.3 | 263.2 | 2,689 |
| Q2 | 21,728 | 10,000 | 212.0 | 174.6 | 382.7 | 489.0 | 793.6 | 14,754 |
| Q3 | 411,487 | 10,000 | 11.0 | 9.0 | 17.7 | 22.9 | 37.6 | 127 |
| Q4 | 175,016 | 10,000 | 25.0 | 25.0 | 30.3 | 56.8 | 74.6 | 272 |
| Q5 | 11,271 | 10,000 | 397.6 | 194.7 | 843.3 | 1,288.2 | 2,521.1 | 78,774 |
| Q6 | 264.95 | 10,000 | 13,408 | 3,893 | 31,982 | 57,180 | 124,322 | 2,212,495 |
| Q7 | 268.99 | 10,000 | 13,010 | 3,199 | 31,441 | 56,525 | 130,417 | 2,206,204 |
| Q8 | 0.43 | 1 | 1,542,980 | 1,543,504 | 1,543,504 | 1,543,504 | 1,543,504 | 1,543,504 |
| Q9 | 0.0042 | 1 | 212,802,208 | 212,869,317 | 212,869,317 | 212,869,317 | 212,869,317 | 212,869,317 |

#### Result Count Distribution

Each query's `.count()` result is also recorded into its own histogram, to see directly
how much of the graph a query actually touched rather than inferring it from latency alone.

| Query | Mean | p50 | p90 | p95 | p99 | max |
|-------|-----:|----:|----:|----:|----:|----:|
| Q1 | 2.0 | 2 | 2 | 2 | 2 | 2 |
| Q2 | 203.5 | 90 | 448 | 728 | 1,356 | 40,607 |
| Q3 | 2.0 | 2 | 2 | 2 | 2 | 2 |
| Q4 | 4.9 | 5 | 5 | 5 | 5 | 5 |
| Q5 | 203.5 | 90 | 448 | 728 | 1,356 | 40,607 |
| Q6 | 5,352.8 | 1,428 | 14,767 | 25,631 | 49,023 | 316,415 |
| Q7 | 5,352.8 | 1,428 | 14,767 | 25,631 | 49,023 | 316,415 |
| Q8 | 4,847,616 | 4,849,663 | 4,849,663 | 4,849,663 | 4,849,663 | 4,849,663 |
| Q9 | 68,976,640 | 69,009,407 | 69,009,407 | 69,009,407 | 69,009,407 | 69,009,407 |

Q8/Q9 have only one sample, so all percentiles collapse to that sample; the value differs
slightly from the exact totals printed by `[Scan Result]` (4,847,571 / 68,993,773) because
`hdrhistogram`'s 3-significant-figure bucketing rounds large single values — not a real
count discrepancy.

#### Notes

- **`block_cache_size` reduced to 1 GiB** (from 4 GiB in earlier runs, see
  [store.rs](src/store/rocks/store.rs)): still comfortably covers the ~900 MB working set
  (vertices ~243 MB + edges_out ~300 MB + edges_in ~300 MB + vertex_degree ~58 MB), so
  Q1/Q4's numbers are essentially unchanged from the 4 GiB run — 1 GiB is a more
  conservative default that doesn't assume a large-memory host.
- **Q2 vs Q3 execution order is deliberate and explains their entire cost asymmetry**: both
  traverse the same sampled source vertices' out-edges — Q2 as an unfiltered scan
  (`InOutStep`), Q3 as a point lookup for a specific `dst` (`GetEStep`, confirmed via
  `--explain`). The shared RocksDB block cache persists across the whole benchmark run
  (`clear_caches()` only clears session-local maps, not the block cache), and `edges_out`'s
  16 KB blocks are large enough to hold most or all of a vertex's out-edges in one block.
  So whichever of Q2/Q3 runs *first* pays the cold-cache cost of pulling each sampled
  vertex's edge block off SSD, and whichever runs *second* inherits that warmth for free.
  Running the scan (Q2) first and the point lookup (Q3) second — as ordered here — makes
  Q2 look expensive (mean 212.0 µs, p50 174.6 µs) and Q3 look extremely cheap (mean 11.0 µs,
  p50 9.0 µs): reversing the order was verified experimentally to flip these numbers almost
  exactly. Treat Q2's absolute latency as "cold scan cost" and Q3's as "warm point-lookup
  cost" rather than as a fair head-to-head comparison of the two access patterns.
- **Sampling is size-biased toward high-degree vertices**: `src`/`dst` come from randomly
  sampled *edges* in the file, not randomly sampled *vertices*. A random edge's endpoint has
  higher expected degree than a uniformly random vertex (the same effect behind the
  "friendship paradox") — this is visible directly in the result-count data: Q2/Q5's mean
  result count of 203.5 (≈102 out-edges, since 2 property values are emitted per edge)
  is ~6.4× higher than the dataset's true unweighted mean out-degree of 16.0 measured
  separately over all 4,308,452 vertices with outgoing edges. The comment in
  [bench_read.rs](src/bin/bench_read.rs) about the reservoir shuffle avoiding
  "bias from hub vertices that cluster at the start of sorted files" only addresses
  positional bias — it doesn't remove this size bias, which is inherent to sampling
  from an edge list. Worth keeping in mind when reading p50/mean as "typical query cost."
- **Q1 (87,058 ops/s, mean 56.1 µs, p50 8.1 µs)**: vertex point-lookup from `vertices` CF.
  Dataset comfortably fits in the 1 GiB block cache, so most lookups are cache hits; the
  mean is pulled well above p50 by a heavier tail (p99 263 µs, max 2.7 ms) — normal
  cache/scheduling jitter, not a data-dependent effect (result count is a constant 2).
- **Q2 (21,728 ops/s, mean 212.0 µs, p50 174.6 µs)**: full out-edge prefix scan, running
  cold (see cache-order note above). The heavy tail (p99 794 µs, max 14.8 ms) directly
  correlates with result count: p99 result count is 1,356 and max is 40,607, i.e. the
  slowest queries really are scanning tens of thousands of edges, not just hitting an
  unlucky cache miss.
- **Q3 (411,487 ops/s, mean 11.0 µs, p50 9.0 µs)**: `GetEStep` point lookup, running warm
  off Q2's just-populated cache (see cache-order note above). Result count is a constant 2
  (exactly one matching edge via `otherV().hasId(dst)`), and with a warm cache this is now
  the fastest edge-touching query in the suite — faster even than Q1, since `edges_out`'s
  larger 16 KB blocks were already resident from Q2's scan moments earlier.
- **Q4 (175,016 ops/s, mean 25.0 µs)**: `limit(5)` caps the flattened value stream at 5, so
  result count is essentially always 5 (mean 4.9 — only vertices with out-degree <3 return
  fewer). Latency is correspondingly flat and close to Q1's cost, since almost every query
  does the same bounded amount of work regardless of the source vertex's true degree.
- **Q5 (11,271 ops/s, mean 397.6 µs, p50 194.7 µs)**: out-neighbor scan plus a vertex
  property read per neighbor. Shares the exact same result-count distribution as Q2 (both
  reuse the same sampled `(src, dst)` pairs and both emit 2 values per neighbor/edge) —
  mean 203.5, max 40,607 — so the same size-biased-sampling caveat above applies, and the
  max latency of 78.8 ms directly tracks the same high-out-degree source vertices.
- **Q6/Q7 (265–269 ops/s, mean ~13.0–13.4 ms)**: the result-count histogram confirms the
  hub-vertex theory directly rather than by inference — max result count is **316,415**
  deduped 2-hop neighbors for a single query, with p99 at 49,023. That single query is
  almost certainly the same one hitting the max latency of ~2.21 s (Q6) / ~2.21 s (Q7).
  Q6 and Q7 report *identical* result-count distributions (mean 5,352.8, max 316,415),
  confirming the unrolled form and `repeat().times(2)` compute the same result set — the
  small Ops/s gap between them is sampling/scheduling noise, not `repeat()` overhead.
- **Q8 (0.43 ops/s, single 1.54 s scan)**: full scan of all 4,847,571 vertices, run once
  since a full scan covers the same fixed dataset deterministically every time.
- **Q9 (single scan, 68,993,773 edges, ~3.9 min)**: run once for the same reason as Q8.
  The per-query timer recorded ~212.9 s, while the benchmark's total wall-clock (used for
  the printed Ops/s) was 235.97 s — a ~23 s gap, consistent in magnitude with the ~30 s and
  ~33 s gaps seen in prior runs. That consistency across three independent runs suggests a
  fixed, reproducible cost (most likely in session/iterator setup for a full-CF scan)
  rather than noise — still worth profiling separately if Q9's absolute number matters for
  future comparisons.

---

### Read (soc-LiveJournal1, 1 M edges — transactional DB)

| | |
|-|-|
| **Dataset** | soc-LiveJournal1, 1 M edges, shuffled (`soc-LiveJournal1-1M.txt`) |
| **Data dir** | `data/rocksGraph-1M` (OCC transactional writes) |
| **Parallelism** | 5 concurrent workers |
| **Query sample** | 1,000,000 per benchmark (full file) |

#### Results

| Query | Ops/s | Queries | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 855,088 | 1,000,000 | 4.5 | 9.8 | 10.7 | 12.3 | 5,001 |
| Q2¹ | 442,240 | 1,000,000 | 8.6 | 18.5 | 19.8 | 33.3 | 29,164 |
| Q3¹ | 338,916 | 1,000,000 | 11.3 | 23.2 | 28.8 | 49.2 | 1,710 |
| Q4¹ | 394,466 | 1,000,000 | 10.4 | 20.7 | 23.4 | 29.9 | 1,761 |
| Q5¹ | 237,932 | 1,000,000 | 15.2 | 32.8 | 42.6 | 74.7 | 3,207 |
| Q6 | 233,569 | 1,000,000 | 14.7 | 33.1 | 43.7 | 79.3 | 2,476 |
| Q7 | 230,234 | 1,000,000 | 13.8 | 32.1 | 43.1 | 79.8 | 37,323 |
| Q8 | 2.3 | 5 | 286,786 | 408,945 | 408,945 | 408,945 | 408,945 |
| Q9 | 2.1 | 5 | 348,914 | 451,150 | 451,150 | 451,150 | 451,150 |

¹ These queries used `bothE`/`both` in the original run; current code uses `outE`/`out`.

#### Notes

- All data fits in the 256 MB block cache for the 1 M database → near-RAM latency throughout.
- **OCC write throughput at 90.9 K/s**: upsert-heavy workload (2 vertex coalesces + 1 edge
  write per input edge). p50 at 33 µs is bounded by RocksDB write + OCC validation.
  This is the correct path for incremental writes to an existing database.
  For the initial load of a new database, use `SstBulkLoader` (265 K edges/s, 69 M edges).
