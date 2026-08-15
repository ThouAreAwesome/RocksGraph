# Design: Vector Index Overhead Benchmarks

Status: proposal

## Problem

RocksGraph has no benchmark that isolates the cost of vector indexing. The two
binaries that *do* touch vectors, `oltp_load_ldbc.rs` and
`bulk_load_ldbc_typed.rs`, declare a vector index unconditionally as part of
their schema (`ldbc_common/mod.rs:161`) — there's no baseline run without the
index to diff against, and neither binary times the vector-specific phases
separately from the rest of the load. `docs/guides/benchmarks.md`'s three
documented benchmarks (`bench_write`, `bench_write_occ`, `bench_read`) carry no
vector property at all.

This matters concretely right now: this session made three changes to the
per-commit checkpoint-trigger path — switching `mutation_count` to
`Ordering::Relaxed`, collapsing two redundant per-index `HashMap` grouping
passes into one, and replacing that grouping's container with an
inline-scanned `SmallVec` — all justified by reasoning about atomics,
allocation, and lock contention, none justified by a measurement. We have no
number showing any of it moved the needle, and no way to catch a future
regression in this path other than re-deriving the same reasoning by hand.

## Goals & non-goals

**Goals:**
- Measure bulk-load ingest cost separately from vector-index-build cost, so
  "loading N vertices" and "building the HNSW index for N vertices" are two
  numbers, not one.
- Measure OLTP (`TxnSession`) write throughput/latency with vs. without a
  vector-indexed property attached, across a concurrency sweep, so the
  overhead is a delta against an already-published non-vector baseline
  rather than an absolute number with nothing to compare it to.
- Measure the background-checkpoint trigger path specifically
  (`checkpoint_mutation_threshold` on vs. off), to put a number on the
  per-commit accounting cost this session optimized three times over.
- Sweep vector dimension and dataset scale, since both are load-bearing for
  how the overhead grows, not just a single point measurement.
- Produce numbers directly comparable to the existing `benchmarks.md` tables
  (same topology, same tiers, same environment block) rather than a
  freestanding report that can't be cross-referenced.

**Non-goals:**
- Not an ANN recall/quality benchmark. RocksGraph calls into `usearch` for
  insert/search; whether HNSW finds good neighbors is `usearch`'s concern
  (covered by `design_ann_algorithm_and_library.md`), not something this
  benchmark re-validates.
- Not covering edge vector indexes — not implemented yet (vertex only,
  matching current feature scope per `design_vector_search.md` §5a).
- Not covering the Gremlin Server protocol or any distributed scenario.

## Dataset

Two independent choices: what **graph topology** to load, and what **vector
data** to attach to each vertex. They don't need to come from the same
source.

### Graph topology: reuse LiveJournal, don't introduce a new dataset

Considered three options:

| Option | Pro | Con |
|---|---|---|
| **Real ANN dataset** (SIFT1M — 128-dim/1M vectors, GloVe, GIST1M) | Genuine embedding manifold structure; standard in ANN literature | Ships vectors only, no graph — would need a synthetic graph glued on anyway; fixes dimension to whatever the dataset provides, can't sweep it; new external download, no existing environment/tooling in this repo uses it |
| **LDBC synthetic generator** (`scripts/generate_synthetic_ldbc.py`) | Already wired end-to-end into `bulk_load_ldbc_typed.rs`/`oltp_load_ldbc.rs`; embeddings already baked into the CSV | Its embedding dimension is 16 (`VECTOR_DIM` in `ldbc_common/mod.rs:23`) — far below any real embedding model, would understate real cost; not yet benchmarked at a scale large enough to show overhead trends |
| **LiveJournal** (`soc-LiveJournal1`, existing `benchmarks.md` dataset) | Already downloaded/documented for this exact environment (Apple M3, 16 GB, NVMe); already has published 10 M/69 M-edge baselines for bulk-load, OCC-write, and read — a vector run over the *same* topology gives a direct delta against numbers that already exist; vertex counts already known (3.16 M at the 10 M-edge tier, 4,847,571 at full scale) | No node attributes of its own — vectors have to be synthesized regardless |

Recommendation: **LiveJournal**, reusing the same edge-list files and tiers
already in `benchmarks.md` (10 M/69 M edges). The deciding factor is that
"cost of vector indexing" is only meaningful as a *delta* — an absolute
"building a 384-dim index over 4.8 M vertices took N seconds" number is much
less useful than "the same load that took X seconds without a vector index
took X + N seconds with one," and the second framing only works if both runs
share topology, scale, and machine with an already-published baseline. Every
other option requires either a new topology (real ANN dataset) or a scale we
haven't benchmarked before (LDBC synthetic), which forces a first-time
absolute measurement with nothing to diff against on day one.

### Vector data: synthetic, not real embeddings — and not the existing dim=16 generator

Since this benchmark measures RocksGraph's *mechanism* cost (WAL writes,
per-commit checkpoint accounting, snapshot save/load, incremental HNSW insert
calls) rather than HNSW's search quality, the semantic content of the vectors
doesn't matter — only their **dimension** (drives distance-computation and
memory cost) and **shape** (should look like real embedding-model output, not
be pathologically degenerate). Synthetic i.i.d. random vectors are standard
practice for infrastructure/ingest benchmarks for exactly this reason (real
datasets are reserved for recall benchmarks, which is explicitly out of
scope here).

Generation: draw each component i.i.d. from a standard normal distribution,
then L2-normalize to unit length. Normalizing matters even though we're not
measuring recall — real embedding models (sentence-transformers, OpenAI,
BERT-derived) conventionally emit unit-norm vectors for cosine similarity,
and an unnormalized `uniform(-1, 1)` vector (the existing LDBC generator's
approach) has different per-dimension magnitude statistics that don't match
production shape.

Dimension sweep: 128, 384, 768, 1536 — chosen to bracket real production
embedding sizes (128 as a small/legacy floor, 384 for
`sentence-transformers/all-MiniLM-L6-v2`, a very common default, 768 for
BERT-base-derived models, 1536 for `text-embedding-3-small`) rather than the
existing LDBC generator's dim=16, which is far below anything a real
deployment would use and would understate the benchmark's whole point.

**Deferred, not required:** SIFT1M as an optional later cross-check, to
confirm build/query times aren't wildly out of line with published
ann-benchmarks numbers for the same `usearch` HNSW parameters. Left out of
the initial plan to keep it dependency-free and fully scriptable — nothing
here should require a manual download to reproduce.

## Design

### A. Bulk-load + index-build cost

`BulkLoader::commit()` already has a clean phase seam: SST ingest finishes,
then declared vertex vector indexes are rebuilt
(`bulk/loader.rs:767-773`, `index_manager().rebuild(...)`). Wrap just that
call in a timer to get "ingest" and "index build" as two separate numbers
instead of one combined total.

New binary `bench_vector_bulk_load.rs` (or a `--with-vector-index <dim>` flag
added to the existing `bulk_load_ldbc_typed.rs` — leaning toward a dedicated
binary, since reusing LiveJournal means dropping the LDBC-specific CSV
parsing entirely):

```text
bench_vector_bulk_load --data-dir <path> --db-path <path> --dim <128|384|768|1536> [--quantization f32|f16]
```

Reports, per run:
- SST-ingest-only wall time (matches `bench_write`'s existing metric — should
  be ~unchanged from the no-vector baseline, since ingest doesn't touch the
  index).
- Index-rebuild-only wall time.
- Total wall time.
- Peak resident memory during rebuild (the in-memory HNSW graph).
- On-disk snapshot file size (`vector_idx_<prop>.snapshot`).

Run at the existing 10 M/69 M-edge tiers, sweeping dimension and quantization
(F32 vs. F16 — quantifies the memory/build-time tradeoff `vector_search.md`
§5 documents but doesn't measure).

### B. OLTP write throughput/latency with vs. without vector indexing

Extend `bench_write_occ.rs`'s existing upsert pattern (hdrhistogram +
configurable parallelism, matching its current structure) with a
`--vector-dim <N>` flag: when set, each vertex upsert also carries a
generated unit-norm vector property tied to a declared index; when unset,
behaves exactly as today (the existing no-vector baseline, already
published).

Two things this isolates that (A) can't, since they're OLTP-path-only:
- **Per-commit WAL + incremental-insert cost**: run with `--vector-dim` on
  vs. off, same dataset/parallelism, diff throughput (ops/sec) and p50/p99
  latency (hdrhistogram, same convention as the existing benchmark).
- **Checkpoint-trigger accounting cost**: with `--vector-dim` on, run once
  with `checkpoint_mutation_threshold` unset (no background checkpointing —
  isolates the `mutation_count` fetch_add + grouping cost with the
  `spawn_gate` CAS/thread-spawn path never taken) and once with it set to a
  realistic threshold (exercises the full trigger path). The delta between
  these two — not between vector-on and vector-off — is the number that
  validates this session's optimization work.

Sweep parallelism (1/4/16/64 threads) in addition to the dimension sweep from
(A): the `checkpoint_states` `RwLock` and per-index `mutation_count` atomic
are exactly the kind of shared state that degrades under contention, and a
single-threaded run wouldn't show that even if it exists.

### C. Results

New "### Vector Index Overhead" section in `docs/guides/benchmarks.md`,
placed after the existing Read section, same format as the existing
sections (environment block already shared, since this reuses the same
LiveJournal tiers and machine — no need to repeat it). Each table row is a
`(dimension, quantization, scale)` combination; each OLTP table additionally
carries a `(parallelism, checkpoint on/off)` axis.

## Files changed

| File | Change |
|---|---|
| `rocksgraph/src/bin/bench_vector_bulk_load.rs` | New. Scenario A. |
| `rocksgraph/src/bin/bench_write_occ.rs` | Add `--vector-dim` / `--checkpoint-threshold` flags for scenario B; existing no-flag behavior unchanged. |
| `docs/guides/benchmarks.md` | New "Vector Index Overhead" section (scenario C). |

## Implementation plan

1. `bench_vector_bulk_load.rs`: LiveJournal edge-list ingest via `BulkLoader`, synthetic unit-norm vector generator, phase-separated timing. Verify against the existing `bench_write` numbers with `--dim 0`/no-vector run — ingest-only time should match the published baseline within noise, confirming the phase split doesn't itself add overhead.
2. `bench_write_occ.rs`: add the two flags, generator reused from step 1. Verify the no-flag path is byte-for-byte the same benchmark as today (same throughput within noise) before trusting the vector-on numbers.
3. Run the full sweep (dimension × scale × quantization for A; dimension × parallelism × checkpoint-on/off for B) and record results.
4. Write up `benchmarks.md`'s new section.
5. Optional follow-up (not blocking): SIFT1M cross-check for (A)'s build-time numbers.

## Test plan

No correctness test surface here (these are `#[bin]` binaries, not library
code) — verification is the sanity checks in implementation steps 1–2 above
(no-vector path matches the existing published baseline), plus a small-scale
smoke run (10 K vertices) before committing to a full 10 M/69 M run, to catch
a broken flag or generator before spending the wall-clock time.

## Out of scope

- Recall/quality measurement (non-goal, see above).
- Edge vector indexes (not implemented).
- Multi-index-per-commit overhead specifically — `test_independent_per_index_triggers`
  and this session's `group_vector_ops` work already cover that in the unit
  test suite; this benchmark's OLTP scenario uses a single vector-indexed
  property per vertex, matching the common case the optimization work
  targeted.
