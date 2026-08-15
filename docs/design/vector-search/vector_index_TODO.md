# Vector index: optimization backlog

Candidates identified while auditing vector-index build/update performance
(2026-08). Three items (OLTP concurrent-write relaxation; a save()-vs-insert
race found via follow-up code review; and simplifying `rebuild()`'s vertex
scan) have been implemented; the rest are recorded here for future
prioritization.

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

### save()/save_all() vs. concurrent insert/remove race

Found via code review after the OLTP concurrent-write relaxation above:
`IndexManager::save`/`save_all` (`api.rs`) took `arc.read()` on the same
per-index lock that the apply loop now also takes `.read()` for — so a
background checkpoint could run fully concurrently with an in-flight insert
on another thread. Two confirmed, real issues:

1. **Segfault risk**: usearch's `save_to_buffer()` has no internal
   synchronization of its own (confirmed against usearch 2.26.0's C++
   source — `save_to_stream()` walks `vectors_lookup_`/`size()` without
   taking `slot_lookup_mutex_`), and `hnsw.rs`'s `save()` doesn't take
   `resize_lock` either. A save running concurrently with a
   capacity-triggered reallocation (`grow_capacity_and_add`) is an
   unsynchronized pointer/size race.
2. **Permanent data loss**: a racing thread's `set_last_replayed_timestamp`
   could advance the watermark past an insert still in flight on another
   thread. A snapshot taken in that window is stamped as covering a
   timestamp it doesn't actually contain, so WAL replay after a crash skips
   the lost entry for good.

Fixed by changing `arc.read()` → `arc.write()` at both call sites in
`api.rs`. `save()` only needs `&self`, so calling it through a write guard
is unaffected — what matters is that the guard can't be *acquired*
exclusively while any insert/remove holds the read side, which restores the
mutual exclusion the old outer write-lock used to provide by accident.

Regression test: `test_concurrent_oltp_commits_with_background_checkpoint`
(`graph/tests/vector.rs`) — sets a low `checkpoint_mutation_threshold` so
background saves fire during heavy concurrent inserts, the one case none of
the other concurrency tests exercised. Verified load-bearing: reverting the
fix reproduced the predicted SIGSEGV in 2/15 runs; with the fix, 15/15 clean.

No benchmark impact: none of the published or in-flight benchmark runs
configure a checkpoint threshold, so `save()` was never invoked during
those runs — this fix doesn't change any published number.

### Simplify `IndexManager::rebuild()`'s vertex scan + property decode

Original framing here assumed the sequential "resolve property values" loop
was doing N separate RocksDB point-lookups that could be parallelized with
rayon. Traced the actual data flow and that assumption was wrong:
`LogicalSnapshot::scan_vertices` already fetches full `Vertex` records
(property blob included) directly off the RocksDB iterator during the scan
itself (`store/rocks/snapshot.rs`'s raw `scan_vertices` decodes
`VertexValue` per row) — it just discards everything but the `VertexKey`
from its return value and caches the full `Vertex` internally. The
subsequent `snap.get_value(&k, prop_key_id)` calls were cache hits: a
HashMap lookup plus `Vertex::get_value()`, a pure `&self`, no-I/O,
no-interior-mutability decode (confirmed via `PropertyMap::get_value` in
`types/element.rs`). So there was no I/O to parallelize — the actual fix
was simpler than originally scoped here:

- Bypass `LogicalSnapshot`'s caching wrapper entirely (it exists for
  repeated-access traversal patterns; `rebuild()` visits every vertex
  exactly once, so the cache added cost with no benefit) and call the raw
  `Snapshot::scan_vertices` directly, which already returns `Vec<Vertex>`.
- Merge decode + insert into a single `rayon` pass instead of two phases.

`Vertex` has no interior mutability, so it's trivially `Send` — no new
locking, no trait/API changes needed. Confined to `rebuild()` in `api.rs`;
also dropped the now-unneeded `LogicalSnapshot::new(...)` construction,
which in turn left `IndexManager::execution_options` completely unused —
removed that field too (clippy `dead_code` caught it immediately). One
minor behavior change: the old code silently swallowed `get_value` errors
(`if let Ok(Some(...)) = ...`); `Vertex::get_value` can't fail once you
already have the `Vertex`, so that possibility is gone entirely — a small
correctness improvement, not a regression.

Verified: `just full-check` clean, full 899-test suite passes, the 9
rebuild-specific tests pass across 5 repeated runs, and 15 `just
stress-test` rounds clean (covers `test_rebuild_survives_multiple_capacity_growth_cycles`).

## Backlog

### 1. More aggressive reactive capacity growth during rebuild

`grow_capacity_and_add` (hnsw.rs) doubles capacity on each reactive growth
event, and each event briefly serializes every thread behind `resize_lock`'s
write side. A rebuild whose upfront RocksDB-estimate-based reservation
undershoots pays for `log2(N/1000)` separate growth events. Growing more
aggressively on the first reactive trip (e.g. jump to a larger multiple of
current size rather than a strict doubling) would reduce the number of these
serializing events for large rebuilds starting from a bad estimate.

### 2. Group the OLTP apply-loop by index

The post-commit "apply vector mutations" loop in `logical.rs` still does one
index lookup + one lock acquisition per individual op. `group_vector_ops`
already exists and is used for the pre-commit capacity check and the
checkpoint-trigger accounting in the same `commit()` — reusing it for the
apply loop itself would let a transaction that touches multiple vectors on
the same index take that index's lock once instead of once per op. Smaller
win than the items above; free once written since the grouping machinery
already exists.

### 3. F16 vs F32 build-speed comparison

`bench_vector_bulk_load --quantization` already exists but this comparison
has never actually been run. F16 is known to roughly halve memory at <0.1%
recall cost; whether it also meaningfully speeds up index *construction*
(not just memory footprint) is unmeasured and would inform whether
lower-precision builds are worth defaulting to more aggressively, and how
much headroom there is before something like RaBitQ becomes worth the
complexity (see below).

### 4. RaBitQ

Discussed and deliberately deferred — real potential speedup on the
distance-computation side, but a large, separate v0.4-scope project (needs
an async training pipeline and a warm-up fallback before the index is
trained). Not recommended as a near-term next step; revisit after (4).

### 5. Native batch-insert API / keyspace-partitioned scanning

Lower priority, needs investigation before committing effort:
- Whether usearch exposes a true batch-insert path with lower per-call
  overhead than individually-parallelized `add()` calls (unconfirmed either
  way — not yet checked against the current usearch version).
- Partitioning `rebuild()`'s vertex-keyspace scan itself across threads,
  rather than only parallelizing within each 10k-row chunk. Likely
  diminishing returns once (1) is done, since chunk-level parallelism
  already captures most of the available concurrency.
