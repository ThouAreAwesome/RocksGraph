# Vector index: optimization backlog

Candidates identified while auditing vector-index build/update performance
(2026-08). One item (OLTP concurrent-write relaxation) has been implemented;
the rest are recorded here for future prioritization.

## Done

### OLTP concurrent-write relaxation

`logical.rs`'s post-commit "apply vector mutations" loop used to take each
index's `Arc<RwLock<Box<dyn VectorIndex>>>` with `.write()` per op, fully
serializing concurrent OLTP transactions that touched the same vector index —
even though `UsearchHnswIndex`/`BruteForceIndex` had already been made safe
for concurrent `&self` access (resize_lock + per-key upsert_locks for HNSW;
an inner `RwLock` for BruteForce). The outer lock was strictly more
conservative than necessary.

Fixed by:
- Changing `VectorIndex::set_last_replayed_timestamp` from `&mut self` to
  `&self` (backed by `AtomicU64` in both implementations) — the one method
  in the post-commit path that still required exclusive access.
- Switching the apply-loop's `arc.write()` to `arc.read()`.

Regression test: `test_concurrent_oltp_commits_same_vector_index` in
`rocksgraph/src/graph/tests/vector.rs` (named with "concurrent" so `just
stress-test` picks it up automatically).

Benchmark (`bench_write_occ`, `soc-LiveJournal1-10k.txt`, dim=384,
8 physical cores):

| parallelism | before (edges/s) | after (edges/s) |
|---|---|---|
| 1 | 800  | 823  |
| 4 | 863  | 2963 |
| 8 | 849  | 3953 |

Before the fix, throughput was flat regardless of thread count (the outer
lock was the bottleneck) and p50 latency grew ~7x from 1→8 threads as writers
queued on it. After the fix, throughput scales close to 5x at 8 threads and
p50 latency stays roughly flat. (For reference, the same sweep with no vector
property at all — pure edge/vertex OCC writes — scales from 35k to 78k
edges/s across the same thread range, so some of the remaining sub-linear
scaling above is ordinary OCC/RocksDB contention unrelated to the vector
index.)

## Backlog

### 1. Parallelize `IndexManager::rebuild()`'s property-value read/decode phase

`api.rs`'s `rebuild()` already parallelizes the HNSW insert phase via rayon,
but the property-value read/resolve phase immediately before it (`snap.get_value`
per vertex, decoding `Primitive::FloatVector`) is still explicitly sequential —
see the comment above that loop. Now that insert is ~4-5x faster, this
sequential phase is a proportionally larger share of total rebuild time than
it used to be. RocksDB snapshot reads are safe to parallelize; wrapping this
loop in the same rayon pattern already used for insert should be a small,
low-risk, mechanical change.

### 2. More aggressive reactive capacity growth during rebuild

`grow_capacity_and_add` (hnsw.rs) doubles capacity on each reactive growth
event, and each event briefly serializes every thread behind `resize_lock`'s
write side. A rebuild whose upfront RocksDB-estimate-based reservation
undershoots pays for `log2(N/1000)` separate growth events. Growing more
aggressively on the first reactive trip (e.g. jump to a larger multiple of
current size rather than a strict doubling) would reduce the number of these
serializing events for large rebuilds starting from a bad estimate.

### 3. Group the OLTP apply-loop by index

The post-commit "apply vector mutations" loop in `logical.rs` still does one
index lookup + one lock acquisition per individual op. `group_vector_ops`
already exists and is used for the pre-commit capacity check and the
checkpoint-trigger accounting in the same `commit()` — reusing it for the
apply loop itself would let a transaction that touches multiple vectors on
the same index take that index's lock once instead of once per op. Smaller
win than the items above; free once written since the grouping machinery
already exists.

### 4. F16 vs F32 build-speed comparison

`bench_vector_bulk_load --quantization` already exists but this comparison
has never actually been run. F16 is known to roughly halve memory at <0.1%
recall cost; whether it also meaningfully speeds up index *construction*
(not just memory footprint) is unmeasured and would inform whether
lower-precision builds are worth defaulting to more aggressively, and how
much headroom there is before something like RaBitQ becomes worth the
complexity (see below).

### 5. RaBitQ

Discussed and deliberately deferred — real potential speedup on the
distance-computation side, but a large, separate v0.4-scope project (needs
an async training pipeline and a warm-up fallback before the index is
trained). Not recommended as a near-term next step; revisit after (4).

### 6. Native batch-insert API / keyspace-partitioned scanning

Lower priority, needs investigation before committing effort:
- Whether usearch exposes a true batch-insert path with lower per-call
  overhead than individually-parallelized `add()` calls (unconfirmed either
  way — not yet checked against the current usearch version).
- Partitioning `rebuild()`'s vertex-keyspace scan itself across threads,
  rather than only parallelizing within each 10k-row chunk. Likely
  diminishing returns once (1) is done, since chunk-level parallelism
  already captures most of the available concurrency.
