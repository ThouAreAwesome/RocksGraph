# Design: Vector Index Checkpoint Triggering — Trigger-and-Spawn Background Save

Status: proposal — addresses `TODO.md` §1 P0 "Background / Periodic Checkpointing
(Online Snapshotting)". Builds on the existing snapshot/WAL-GC mechanism in
`design_vector_wal.md` §9 and the per-index locking model in
`design_vector_concurrency.md` §3–4; does not change either.

---

## Table of Contents

- [1. Problem](#1-problem)
- [2. What already exists (no new mechanism needed here)](#2-what-already-exists-no-new-mechanism-needed-here)
- [3. Options considered](#3-options-considered)
  - [A — Application-driven](#a--application-driven)
  - [B — Persistent background thread](#b--persistent-background-thread)
  - [C — Inline on commit()](#c--inline-on-commit)
  - [D — Trigger-and-spawn (chosen)](#d--trigger-and-spawn-chosen)
- [4. Why not persisted (on-disk) checkpoint-in-progress metadata](#4-why-not-persisted-on-disk-checkpoint-in-progress-metadata)
- [5. Chosen design: trigger-and-spawn](#5-chosen-design-trigger-and-spawn)
  - [5a. Shared state — and the trap to avoid](#5a-shared-state--and-the-trap-to-avoid)
  - [5b. Plumbing: getting a checkpoint handle into commit()](#5b-plumbing-getting-a-checkpoint-handle-into-commit)
  - [5c. Trigger logic in commit()](#5c-trigger-logic-in-commit)
  - [5d. Coordinating with close()](#5d-coordinating-with-close)
  - [5e. WAL truncation](#5e-wal-truncation)
- [6. Blast radius during a checkpoint](#6-blast-radius-during-a-checkpoint)
- [7. Configuration surface](#7-configuration-surface)
- [8. Failure handling](#8-failure-handling)
- [9. Testing strategy](#9-testing-strategy)
- [10. Implementation checklist](#10-implementation-checklist)
- [11. Complexity & effort estimate](#11-complexity--effort-estimate)
- [12. Open questions](#12-open-questions)

---

## 1. Problem

Vector index snapshots are currently only written on `Graph::close()`, or
explicitly via `IndexManager::save_all()`/`save()`. There is no periodic or
volume-triggered checkpoint. A long-running server process that never calls
`close()` (SIGKILL, OOM, power loss) must replay the *entire* accumulated
`CF_VECTOR_WAL` on next open — unbounded by how long the process actually ran,
only by how much write volume it saw since the last explicit checkpoint.

## 2. What already exists (no new mechanism needed here)

`IndexManager::save_all()` (`api.rs:387`) already does everything a checkpoint
needs:

```rust
pub fn save_all(&self) -> Result<(), StoreError> {
    let map = vector_indexes.read();
    for ((entity_type, prop_name), arc) in map.iter() {
        let guard = arc.read();
        guard.save(&snap_path, guard.last_replayed_timestamp())?;
    }
    drop(map);
    gc_vector_wal(&store, &vector_indexes, &schema.read()).ok();
    Ok(())
}
```

Snapshot writing is already atomic (`save_snapshot_file` writes to `.tmp` then
`rename`s — see `design_hnsw_impl.md` §8a), and WAL trimming is already fused
into the same call (`gc_vector_wal`, gated per-index by that index's own
`last_replayed_timestamp`). **This document is entirely about *what calls
`save_all()` and when* — not about changing snapshotting or WAL GC
themselves.**

## 3. Options considered

### A — Application-driven
The embedding application spawns its own timer thread holding a cloned
`Graph`, calling `.index_manager().save_all()` on an interval. Works today,
zero library changes. Puts the durability guarantee in the app's hands —
useful as a documented pattern regardless of what else ships, but not a
substitute for a built-in default.

### B — Persistent background thread
`Graph::open` spawns a thread that sleeps/wakes on a timer and calls
`save_all()`, with `checkpoint_interval`/`checkpoint_mutation_threshold` in
`GraphOptions`. Gives time-based coverage even for idle processes. Cost: a
genuine architectural first for this library — `std::thread::spawn` currently
appears only in `rocksgraph/src/bin/` benchmark tools, never inside
`rocksgraph` itself — and needs full lifecycle handling (spawn, sleep, wake,
detect shutdown, join) tied correctly to `Graph::close()`/`Drop`.

### C — Inline on commit()
Track a mutation counter; when a commit crosses the threshold, that commit's
own thread calls `save_all()` synchronously before returning. Simplest to
reason about, but couples an unpredictable multi-millisecond-plus I/O stall
into the transactional hot path. Traced precisely in §6: the calling
transaction is blocked for the *entire* multi-index `save_all()` loop, and the
threshold is most likely to trip exactly when write volume — and thus
concurrent contention — is highest. Rejected: this directly undermines the
predictable-commit-latency property expected of an OLTP-facing API.

### D — Trigger-and-spawn (chosen)
Same mutation counter as C, but instead of running `save_all()` inline, the
triggering commit spawns a short-lived thread to do it and returns
immediately. No persistent thread exists between triggers — each spawned
thread does one checkpoint and exits. Gets B's core benefit (no foreground
transaction pays for the I/O) with far less lifecycle surface than B (no
sleep/wake loop, no persistent-thread shutdown coordination — see §5). Gap
relative to B: no time-based coverage for a long-running, low-traffic process
that never crosses the mutation threshold. (Mitigated by documenting A/a
simple timer alongside it, not by building B outright — see §12.)

## 4. Why not persisted (on-disk) checkpoint-in-progress metadata

Considered and rejected. `Graph::open()` already takes an exclusive RocksDB
file lock — a second process can never have the same database open
concurrently (verified empirically). So the only entity that can race two
`save_all()` calls against each other is two *threads inside the same
process* — an in-process problem, for which an in-memory primitive is the
correct tool. Persisted metadata would still need an in-memory guard around
its own check-then-act read/write (or it has the identical race one level
down), and adds a disk round-trip to the exact commit-path check we're trying
to keep cheap. The one legitimate use for persisted checkpoint metadata — "when
did this index last successfully checkpoint" — is already satisfied for free:
`SnapshotHeader.last_replayed_timestamp` is written into every snapshot file's
header on each successful save (`design_hnsw_impl.md` §8a).

## 5. Chosen design: trigger-and-spawn

### 5a. Shared state — and the trap to avoid

New shared state, owned by `Graph`:

```rust
struct CheckpointState {
    mutation_count: AtomicU64,
    in_progress: Mutex<()>,
    threshold: u64, // from GraphOptions; 0/disabled if not configured
}
```

**This must be `Arc<CheckpointState>` on `Graph`, cloned via `Arc::clone` in
`impl Clone for Graph`.** `Graph` already has a field that gets this wrong on
purpose for a different reason — `bulk_load_in_progress: AtomicBool` is
deliberately *not* shared across clones (`Clone` re-inits it to
`AtomicBool::new(false)` per clone, since bulk-load-in-progress is a
single-handle concern). `CheckpointState` needs the opposite: every clone must
coordinate through the *same* instance, or the mutex/counter silently stops
doing its job the moment two clones are in play — which is the common case,
since a checkpoint is triggered from inside a transaction and coordinated
against `close()` called from a possibly different clone.

### 5b. Plumbing: getting a checkpoint handle into commit()

Checked directly against the current code: there is currently **no path** from
`LogicalGraph::commit()` down to anything that can call
`IndexManager::save_all()`. `TxnSession` holds only `LogicalGraph`;
`LogicalGraph` holds `store: Transaction` (`store/rocks/transaction.rs`,
wrapping `Arc<OptimisticTransactionDB>` — the raw RocksDB handle, not
`Arc<RocksStorage>`); neither carries `IndexOptions` or anything else needed to
construct an `IndexManager`.

The natural fix follows the exact pattern already used for `schema`/
`vector_indexes`: `Graph::begin()` already does

```rust
LogicalGraph::new(self.store.begin(), Arc::clone(&self.schema), Arc::clone(&self.vector_indexes), self.execution_options)
```

Add one more argument — either a cloned `Graph` handle or a lighter
purpose-built struct bundling `Arc<CheckpointState>` plus enough to build an
`IndexManager` (weak `store`/`schema`/`vector_indexes`, `index_options`). A
full `Graph::clone()` is simplest: it lets the spawned thread call
`.index_manager().save_all()` verbatim, reusing existing code with zero new
methods on `IndexManager`. `LogicalGraph`/`TxnSession` already keep the
`schema`/`vector_indexes` `Arc`s alive strongly for the transaction's duration
regardless, so holding one more `Graph` clone alongside them changes no
existing lifetime/ownership behavior.

Only `TxnSession`/`LogicalGraph` need this — `ReadSession`/`LogicalSnapshot`
never touch `vector_pending_ops` and are out of scope.

### 5c. Trigger logic in commit()

Inserted right after the existing "apply committed vector mutations to
in-memory indexes" block (`graph/logical.rs`, ~line 1260):

```rust
if commit_result.is_ok() && !self.vector_pending_ops.is_empty() {
    let prior = self.checkpoint.mutation_count.fetch_add(self.vector_pending_ops.len() as u64, Ordering::AcqRel);
    if self.checkpoint.threshold > 0 && prior + self.vector_pending_ops.len() as u64 >= self.checkpoint.threshold {
        if let Ok(guard) = self.checkpoint.in_progress.try_lock() {
            self.checkpoint.mutation_count.store(0, Ordering::Release);
            let graph = self.graph_handle.clone();
            std::thread::spawn(move || {
                let _ = graph.index_manager().save_all();
                drop(guard); // held for the spawned thread's lifetime — see 5d
            });
        }
        // try_lock() failure = a checkpoint is already in flight; skip this
        // trigger, the counter keeps accumulating and the next commit retries.
    }
}
```

The `MutexGuard` must be moved into the spawned closure (not dropped
immediately after spawning) so it's held for the actual duration of the save —
this is what makes `close()`'s `.lock()` in §5d correctly wait for it.

### 5d. Coordinating with close()

`close()`'s current body is just `self.index_manager().save_all()`. This is
not sufficient on its own: if a background checkpoint is mid-flight when
`close()` runs, two `save_all()` calls could write to the same `.tmp` snapshot
path concurrently (`save_snapshot_file` uses a fixed derived temp path, not a
unique one per call). Fix:

```rust
pub fn close(self) -> Result<(), StoreError> {
    let _guard = self.checkpoint.in_progress.lock(); // blocks until any in-flight checkpoint finishes
    self.index_manager().save_all()
}
```

`try_lock()` (trigger path) vs `.lock()` (`close()` path) is the reason this
is a `Mutex<()>` and not a bare `AtomicBool`: the trigger path wants
skip-if-busy semantics (there's always a next trigger to retry), `close()`
wants block-until-available semantics (there is no "next trigger" after
`close()` returns — the graph is going away, so it cannot just skip). A bare
flag gives you the first for free but requires hand-rolled polling for the
second; `Mutex<()>` gives both natively.

### 5e. WAL truncation

No new design needed — see §2. Every path here (A/B/C/D, and `close()`) ends
up calling the same `save_all()`, which already fuses snapshot-write with
`gc_vector_wal`. Concurrent GC calls are safe without the mutex even discussed
above — deletion is `ts <= cutoff` per index, cutoffs come from each index's
own monotonically increasing `last_replayed_timestamp`, and new WAL writes
always get strictly newer timestamps, so overlapping GC runs are redundant but
never destructive. The mutex in §5c/5d exists solely to protect the snapshot
*file write*, not WAL GC.

## 6. Blast radius during a checkpoint

Traced against the actual lock granularity in `save_all()` (not just its doc
comment):

| Lock | Held for | Blocks |
|---|---|---|
| Outer `vector_indexes.read()` | The entire `save_all()` loop (sum of every index's save time) | Only `.write()` seekers on the index *map* — i.e. `add_vector_index`/`drop_vector_index` schema DDL. Does not block other transactions' commits (they also only take `.read()` on this map; reader-reader never blocks). |
| Per-index `arc.read()` (inside `save()`) | Just that one index's own save duration | Only `.write()` seekers on *that specific index* — a commit with pending ops on that one property. Other properties' commits, and all reads/searches (`nearest()`/`similarity()`, also `.read()`), are unaffected. |

So: ordinary non-vector writes and all reads are untouched by a checkpoint
regardless of which option is chosen. The only foreground impact is (a)
writers to whichever single vector-indexed property is currently mid-save,
for that property's own save duration, and (b) vector-index schema DDL, for
the checkpoint's full duration. Trigger-and-spawn does not change this
blast radius — it only removes the *triggering transaction itself* from
having to wait for it, which inline-C does not.

## 7. Configuration surface

Add to `GraphOptions`:

```rust
pub struct GraphOptions {
    ...
    /// Vector mutation count that triggers a background checkpoint
    /// (snapshot + WAL GC). `None`/`0` disables triggered checkpointing —
    /// only `Graph::close()` and explicit `IndexManager::save_all()` calls
    /// checkpoint.
    pub checkpoint_mutation_threshold: Option<u64>,
}
```

No time-based field in this design (that's option B's territory — see §12).

## 8. Failure handling

- `save_all()` returning `Err` inside the spawned thread: log and drop the
  error (matching `close()`'s existing best-effort tone elsewhere in
  `api.rs`, e.g. the `eprintln!` in `save_all`'s insert-failure path). The
  mutation counter was already reset before spawning (§5c) — a failed
  checkpoint means the *next* threshold crossing retries from scratch, not
  immediately, which is an acceptable trade for not adding retry-scheduling
  complexity to a background path. Worth flagging in review: an alternative is
  not resetting the counter until the spawned thread confirms success, so a
  failed checkpoint retries on the very next write instead of waiting a full
  threshold's worth of writes.
- Panic inside the spawned thread: an unhandled panic there does not propagate
  to the triggering commit (it already returned) or unwind the process — it
  terminates only that detached thread. The `MutexGuard` is dropped on unwind
  either way, so `in_progress` doesn't deadlock permanently.

## 9. Testing strategy

- **Trigger decision, synchronous**: unit-test the counter/threshold/
  `try_lock` logic directly without needing to observe a real background save
  — assert a checkpoint is *attempted* (guard acquired) at the right mutation
  count, and that a second trigger while the guard is held is correctly
  skipped.
- **End-to-end, async**: write past the threshold, poll (bounded timeout) for
  the snapshot file's mtime to update or for `last_replayed_timestamp` in the
  reloaded header to advance — this codebase already has a test in this shape
  (`test_rebuild_while_reads_active` in the vector test suite), so there's a
  direct precedent to follow rather than inventing a new pattern.
- **`close()` racing an in-flight checkpoint**: the trickiest one to write
  reliably — needs a way to reliably create a "checkpoint in flight" window to
  call `close()` against (e.g. a test-only injectable delay in the save path,
  or a large-enough synthetic index that its save duration is reliably longer
  than the test's own `close()` call). Assert `close()` blocks until the
  checkpoint finishes and neither corrupts the snapshot file nor panics.

## 10. Implementation checklist

- [ ] `CheckpointState` struct + `Arc<CheckpointState>` field on `Graph`,
      correct `Clone` impl (shared, not per-clone-fresh like
      `bulk_load_in_progress`)
- [ ] `checkpoint_mutation_threshold` in `GraphOptions`, threaded into
      `Graph::open_with_options`
- [ ] Thread a `Graph` clone (or equivalent handle) + `Arc<CheckpointState>`
      through `Graph::begin()` → `LogicalGraph::new()` → stored field
- [ ] Trigger logic in `LogicalGraph::commit()` (§5c)
- [ ] `close()` acquires `in_progress.lock()` before its own `save_all()`
      (§5d)
- [ ] Unit test: trigger/skip decision logic
- [ ] Integration test: checkpoint actually fires and produces a valid,
      loadable snapshot
- [ ] Integration test: `close()` during an in-flight background checkpoint
- [ ] Doc updates: `docs/design/vector-search/TODO.md` §1 (mark done, link
      here), `docs/design/vector-search/README.md` documents table

## 11. Complexity & effort estimate

**Overall: low-to-moderate — a small, well-scoped feature, not a
multi-week effort.** Breakdown:

- **Mechanical plumbing** (§5a, §5b, §7) — *low effort*. Follows an existing,
  already-proven pattern exactly (this is the same shape as how `schema`/
  `vector_indexes` Arcs already get threaded through `Graph::begin()` →
  `LogicalGraph::new()`). No new architecture, just one more field carried
  along the same path.
- **Trigger logic in `commit()`** (§5c) — *low-to-moderate effort*. The
  insertion point is already known precisely (right after the existing
  vector-mutation-apply block); the logic itself is a counter bump, a
  threshold compare, and a `try_lock`+spawn. Small, but concurrency code
  deserves a careful read in review even when short.
- **`close()` coordination** (§5d) — *low effort*, one line, but easy to
  forget — it's the piece that makes the whole design actually race-free
  rather than just "usually fine."
- **Testing** (§9) — *moderate effort*, the largest real chunk of the work.
  The synchronous trigger-decision test is straightforward; the end-to-end
  async test has a direct precedent to copy (`test_rebuild_while_reads_active`);
  the `close()`-races-in-flight-checkpoint test is the one piece that needs
  real thought to make deterministic rather than flaky.
- **No changes required** to snapshot format, WAL format, WAL GC, or the
  per-index `RwLock` locking model — all of §2's existing mechanism is reused
  as-is.

Rough shape: a single-digit number of files touched (`api.rs`, `graph/
logical.rs`, `engine/options.rs` or wherever `GraphOptions` lives, plus test
files), most of it mechanical, with the two spots demanding real attention
being the `commit()` trigger logic and the `close()`-vs-background-checkpoint
test. No new dependencies, no new column families, no wire-format changes.

## 12. Open questions

- Should a future `checkpoint_interval: Option<Duration>` (option B,
  time-based) be layered on top of this later for the idle-process gap noted
  in §3D? This design doesn't preclude it — `CheckpointState` and the
  `Mutex<()>` guard would be reused as-is by a timer thread calling the same
  trigger path, not replaced.
- Should a failed checkpoint retry on the very next write (don't reset the
  counter until success) rather than waiting a full threshold's worth of
  writes (§8)? Left as a review decision rather than settled here — affects
  worst-case WAL replay volume after a save failure, not correctness.
- Default value (if any) for `checkpoint_mutation_threshold` — the original
  TODO sketch suggested ~50,000 as an example; no benchmark backs that number
  yet.
