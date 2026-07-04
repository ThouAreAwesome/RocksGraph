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
| **RocksOptions** | write_buffer=128 MiB, block_cache=4 GiB (shared), format_version=6 |

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
`snap.clear_caches()` to simulate cold-start per-query access.

| | |
|-|-|
| **Dataset** | soc-LiveJournal1, **68,993,773 edges** (full shuffled) |
| **Data dir** | `data/rocksGraph-shuffled` (bulk-loaded, post-compaction) |
| **Parallelism** | 5 concurrent workers |
| **Query sample** | 10,000 random pairs per benchmark (`--queries 10000`) |

#### Query Definitions

| ID | Traversal | Sample used | Pattern |
|----|-----------|-------------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | vertex | Point lookup + 2 vertex property reads |
| Q2 | `g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | edge pair | Out-edge point lookup (GetEStep) + 2 edge property reads |
| Q3 | `g.V().hasId(id).outE(label).values('weight','timestamp').count()` | edge pair | Full out-edge scan + 2 edge property reads per edge |
| Q4 | `g.V().hasId(id).outE(label).values('weight','timestamp').limit(5).count()` | edge pair | Q3 with early termination at 5 results |
| Q5 | `g.V().hasId(id).out(label).values('name','age').count()` | vertex | Out-neighbor scan + 2 vertex property reads per neighbor |
| Q6 | `g.V(id).out(label).hasLabel(v_label).dedup().out(label).hasLabel(v_label).dedup().hasId(not(id)).count()` | vertex | 2-hop outbound traversal, label filter, dedup, self-exclusion (unrolled) |
| Q7 | `g.V(id).repeat(out(label).hasLabel(v_label).dedup()).times(2).hasId(not(id)).count()` | vertex | Same as Q6 via `repeat().times(2)` |
| Q8 | `g.V().count()` | — | Full vertex scan (4,847,571 vertices) |
| Q9 | `g.E([]).count()` | — | Full edge scan (pending) |

#### Results

| Query | Ops/s | Queries | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 4,987 | 10,000 | 7.8 | 153 | 185 | 260 | 3,310 |
| Q2 | 4,981 | 10,000 | 158 | 942 | 1,154 | 1,725 | 11,026 |
| Q3 | 4,986 | 10,000 | 74 | 1,008 | 1,341 | 2,423 | 19,071 |
| Q4 | 4,980 | 10,000 | 41 | 966 | 1,243 | 2,294 | 4,952 |
| Q5 | 4,980 | 10,000 | 428 | 1,478 | 2,055 | 3,609 | 68,813 |
| Q6 | 200 | 10,000 | 7,569 | 40,993 | 69,599 | 154,927 | 2,449,474 |
| Q7 | 192 | 10,000 | 7,344 | 41,779 | 72,810 | 171,442 | 2,736,783 |
| Q8 | 0.45 | 5 | 1,386,217 | 1,644,167 | 1,644,167 | 1,644,167 | 1,644,167 |
| Q9 | — | — | — | — | — | — | — (pending) |

#### Notes

- **Q1–Q5 throughput capped at ~5 K ops/s**: with 5 workers × 2,000 queries each and most
  queries completing in <500 µs, the benchmark finishes in ~2 s. True sustained throughput
  (longer warm-up, larger sample) would be higher; re-run with `--queries 100000` for a
  more stable measurement.
- **Q1 p50 7.8 µs**: vertex point-lookup from `vertices` CF (~243 MB, 4 KB blocks).
  The CF mostly fits in the 256 MB block cache → near-RAM latency.
- **Q2 p50 158 µs vs Q1 p50 7.8 µs**: Q2 uses GetEStep (single out-edge point lookup in
  `edges_out` CF, ~300 MB, 16 KB blocks). The larger edge CF doesn't fully fit in the
  256 MB shared block cache → many queries pay SSD latency (~150 µs on NVMe).
  Increasing `block_cache_size` to 1 GiB would bring Q2 close to Q1.
- **Q3 p50 74 µs < Q2 p50 158 µs**: Q3 scans all out-edges but without the endpoint
  filter of Q2. GetEStep (Q2) does a single point read while Q3 does a prefix scan —
  prefix scans benefit from sequential prefetch, reducing average latency.
- **Q4 p50 41 µs**: `limit(5)` terminates the edge scan after 5 results — most vertices
  have many more than 5 out-edges, so Q4 reads far less data than Q3.
- **Q5 p50 428 µs**: out-neighbor scan reads edges then vertex properties for each
  neighbor. LiveJournal has average out-degree ~14, so Q5 pays ~14 vertex CF reads
  per query (many cache misses).
- **Q6/Q7 at ~200 ops/s, p50 ~7.5 ms**: 2-hop traversal over the full 69 M edge graph.
  Power-law hubs can have 10 K+ out-edges at hop 1, producing up to 10 K² candidate
  neighbors at hop 2. Max latency >2 s for the highest-degree hubs.
- **Q6 ≈ Q7 (200 vs 192 ops/s)**: `repeat().times(2)` overhead is negligible (<5%)
  compared to the actual traversal work.
- **Q8 at 0.45 ops/s (1.4 s/scan)**: full scan of 4.85 M vertices. The vertex CF
  (~243 MB) is slightly smaller than the 256 MB block cache; repeated scans warm up
  quickly but the first scan pays cold-read cost.

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
