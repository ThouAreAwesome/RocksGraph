# Benchmarks

Results are recorded here for each major version. All benchmarks run against the
[SNAP soc-LiveJournal1](https://snap.stanford.edu/data/soc-LiveJournal1.html) dataset
(1 M edges, shuffled). See [`scripts/prepare_bench_data.sh`](scripts/prepare_bench_data.sh)
for dataset preparation and [`src/bin/`](src/bin/) for the benchmark binaries.

---

## v0.1.0

### Environment

| | |
|-|-|
| **Binary** | `target/release/bench_read` / `bench_write` (`just build-release`) |
| **Dataset** | soc-LiveJournal1, 1 M edges, shuffled (`bench_data/soc-LiveJournal1-shuffled.txt`) |
| **Data dir** | `data/rocksGraph_shuffled` |
| **Parallelism** | 3 concurrent workers |
| **Machine** | _(fill in: CPU, RAM, storage type)_ |
| **OS** | _(fill in)_ |

---

### Write: Insert Vertex and Edge

_(fill in results from `bench_write`)_

---

### Read

#### Query Definitions

| ID | Traversal | Pattern |
|----|-----------|---------|
| Q1 | `g.V().hasId(id).values('name','age').count()` | Point lookup + property projection |
| Q2 | `g.V().hasId(id).outE(label).where(otherV().hasId(dst)).values('weight','timestamp').count()` | Edge traversal with end-vertex filter |
| Q3 | `g.V().hasId(id).both(label).values('weight','timestamp').count()` | Bidirectional neighbour scan |
| Q4 | `g.V(id).out(label).out(label).count()` | 2-hop traversal |

#### Results

| Query | Ops/s | Queries | Duration | p50 (μs) | p90 (μs) | p95 (μs) | p99 (μs) | max (μs) |
|-------|------:|-------:|--------:|--------:|--------:|--------:|--------:|--------:|
| Q1 | 1,778,541 | 68,993,773 | 38.79 s | 1.042 | 1.125 | 1.125 | 1.208 | 11,673.6 |
| Q2 | 1,393,310 | 68,993,773 | 49.52 s | 1.500 | 1.583 | 1.584 | 1.625 | 21,495.8 |
| Q3 | 1,535,957 | 68,993,773 | 44.92 s | 1.291 | 1.375 | 1.416 | 1.500 | 12,763.1 |
| Q4 | 1,766,969 | 68,993,773 | 39.05 s | 1.042 | 1.084 | 1.125 | 1.167 | 10,059.8 |

