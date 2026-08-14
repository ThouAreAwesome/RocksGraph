# Design: Vector Index Checkpoint Triggering — Trigger-and-Spawn Background Save

Status: §1–13 implemented (`Graph::commit`, per-index `checkpoint_states` map,
`IndexOptions::default_checkpoint_mutation_threshold` / `PerIndexOptions::checkpoint_mutation_threshold`,
Rust + Python) — addresses `TODO.md` §1 P0 "Background / Periodic Checkpointing
(Online Snapshotting)". The graph-wide `GraphOptions::checkpoint_mutation_threshold`
field from §1–12 has been removed per the §13h migration (it never shipped in a
release, so no compat shim was needed); checkpoint thresholds are now configured
exclusively through `IndexOptions`. Builds on the existing snapshot/WAL-GC mechanism in
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
- [13. Extension: per-index checkpoint triggering](#13-extension-per-index-checkpoint-triggering)
  - [13a. Why this is lower-risk than it looked at first](#13a-why-this-is-lower-risk-than-it-looked-at-first)
  - [13b. Configuration surface](#13b-configuration-surface)
  - [13c. Per-index checkpoint state](#13c-per-index-checkpoint-state)
  - [13d. Trigger logic in commit()](#13d-trigger-logic-in-commit)
  - [13e. Coordinating with close()](#13e-coordinating-with-close)
  - [13f. Reused vs. new machinery](#13f-reused-vs-new-machinery)
  - [13g. Python bindings](#13g-python-bindings)
  - [13h. Migration — no backward-compat shim](#13h-migration--no-backward-compat-shim)
  - [13i. Testing strategy](#13i-testing-strategy)
  - [13j. Implementation checklist](#13j-implementation-checklist)
  - [13k. Complexity & effort estimate](#13k-complexity--effort-estimate)

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

---

## 13. Extension: per-index checkpoint triggering

Status: proposal. Extends §1–12 (implemented) rather than replacing it —
everything below reuses the trigger-and-spawn mechanics, the `Mutex<()>`
guard pattern, and the `save()`/WAL-GC primitives already shipped.

### 13a. Why this is lower-risk than it looked at first

The shipped design (§1–12) is graph-wide: one counter, one guard, one
`save_all()` call covering every declared index. The natural worry with
"make it per-index instead" is that it multiplies the concurrency surface
that took several review rounds to get right the first time (a real,
demonstrated cost — see §8/§11's history of this doc, and the two bugs found
during implementation review: the `close()`-poisons-the-guard deadlock and
the TOCTOU thread-spawn storm).

But checked directly against the storage layer, the hard part is already
done:

- Each declared index is a fully independent `usearch::Index` in memory
  (`UsearchHnswIndex { inner: Index, ... }`, `vector/hnsw.rs`) — no shared
  state between indexes.
- Each index already has its own on-disk snapshot file
  (`vector_snapshot_path` → `vector_idx_{property}.snapshot`, distinct per
  `(entity_type, property)`).
- **`IndexManager::save(entity_type, property)` — the single-index
  checkpoint — already exists and is already used** (`api.rs:406`). This
  isn't new functionality; it's the same primitive `save_all()` already
  calls in a loop.
- `gc_vector_wal` (`vector/wal.rs:285`) already computes its deletion range
  **per index independently** — it loops over every declared index, reads
  that index's own `last_replayed_timestamp` as its cutoff, and deletes only
  within that index's own `[prop_key_id][entity_type]` key-prefix range
  within the shared `CF_VECTOR_WAL`. Calling it after saving only one index
  is already safe today — the other indexes' entries are simply
  re-scanned against their unchanged (already-covered) cutoff, which is a
  no-op, not a correctness risk.
- `CF_VECTOR_WAL` has no `prefix_extractor` configured (`store.rs:287`,
  plain `Options::default()`), so its per-index prefix-scoped GC scans
  behave like the cheap, flat-scaling `vertices`-CF seeks established
  elsewhere in this project's benchmarking work, not like the pathological
  `edges_out` case — no seek-cost risk analogous to that one lurking here.

So this extension is entirely about replicating the *triggering* layer
(counter + guard + spawn) per index and pointing it at `save()` instead of
`save_all()` — not about building new storage or WAL-truncation mechanics.

### 13b. Configuration surface

Mirror the existing `default_limit` / `PerIndexOptions.memory_limit` pattern
exactly, rather than inventing a new shape:

```rust
pub struct IndexOptions {
    pub default_limit: Option<VectorIndexLimit>,
    pub default_checkpoint_mutation_threshold: Option<u64>,  // new
    pub per_index: Vec<PerIndexOptions>,
}

pub struct PerIndexOptions {
    pub entity_type: VectorEntityType,
    pub property: SmolStr,
    pub memory_limit: Option<VectorIndexLimit>,
    pub checkpoint_mutation_threshold: Option<u64>,  // new — overrides the default when set
}
```

Resolution order for a given `(entity_type, property)`: per-index override →
`IndexOptions::default_checkpoint_mutation_threshold` → disabled.
`GraphOptions::checkpoint_mutation_threshold` (§7) is retired by this
change — see §13h.

### 13c. Per-index checkpoint state

Add a second map alongside `vector_indexes`, not merged into
`VectorIndexMap` itself (keeps that type's shape — and every existing call
site that already matches on it — untouched):

```rust
pub(crate) struct PerIndexCheckpointState {
    mutation_count: AtomicU64,
    in_progress: parking_lot::Mutex<()>,
    spawn_gate: AtomicBool,
    threshold: u64,  // resolved per §13b at the time the entry is created
}

pub(crate) type CheckpointStateMap =
    Arc<RwLock<HashMap<(VectorEntityType, SmolStr), Arc<PerIndexCheckpointState>>>>;
```

Entries are created **lazily**, on first touch, via the entry API when a
commit's pending ops first reference a given `(entity_type, property)` —
not proactively populated when a vector index is declared. This sidesteps
needing to keep this map in sync with `vector_indexes` on every
`add_vector_index`/`drop_vector_index` schema change: a dropped index just
leaves a harmless, tiny, orphaned entry (a few atomics) rather than a
correctness problem, since nothing ever reads it again once its index is
gone. (A cleanup pass on schema-drop is a reasonable follow-up, not a
blocker — see §13j.)

### 13d. Trigger logic in commit()

Today, `commit()` sums `self.vector_pending_ops.len()` into one counter.
This changes to grouping first:

```rust
let mut by_index: HashMap<(VectorEntityType, SmolStr), u64> = HashMap::new();
for op in &self.vector_pending_ops {
    let (entity_type, prop_name) = match op {
        PendingVectorOp::Inserted { key, prop_name, .. }
        | PendingVectorOp::Removed { key, prop_name, .. } => {
            // No `EntityKey` → `VectorEntityType` conversion exists yet (checked —
            // `EntityKey` is defined in `vector/brute_force.rs`, no such `From` impl
            // today); this is a small new match to write, not something to assume
            // is already there: `EntityKey::Vertex(_) => VectorEntityType::Vertex`,
            // `EntityKey::Edge(_) => VectorEntityType::Edge`.
            (entity_type_of(key), prop_name.clone())
        }
    };
    *by_index.entry((entity_type, prop_name)).or_default() += 1;
}

for ((entity_type, prop_name), count) in by_index {
    let state = get_or_create(&self.checkpoint_states, entity_type, prop_name.clone());
    let prior = state.mutation_count.fetch_add(count, Ordering::AcqRel);
    if state.threshold > 0
        && prior + count >= state.threshold
        && state.spawn_gate.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    {
        let graph = self.graph_handle.clone().unwrap();
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            struct ResetGateOnDrop(Arc<PerIndexCheckpointState>);
            impl Drop for ResetGateOnDrop {
                fn drop(&mut self) { self.0.spawn_gate.store(false, Ordering::Release); }
            }
            let _reset_gate = ResetGateOnDrop(Arc::clone(&state));
            if let Some(_guard) = state.in_progress.try_lock() {
                state.mutation_count.store(0, Ordering::Release);
                let _ = graph.index_manager().save(entity_type, &prop_name);
            }
        });
    }
}
```

Note: `commit()`'s existing pre-flight capacity-check block (§11, "Apply
committed vector mutations") already builds a similar per-`(entity_type,
prop_name)` grouping but currently **hardcodes `VectorEntityType::Vertex`**
rather than deriving it from the op's `EntityKey` — a pre-existing
simplification, harmless today since edge indexes aren't supported yet
(§5, P4 backlog), but worth deriving correctly in this new code rather than
copying that shortcut forward, so this stays correct the moment edge index
support lands.

### 13e. Coordinating with close()

The single biggest correctness risk in this extension, and the one that
deserves the most review attention: `close()` currently acquires **one**
`in_progress.lock()` before its own `save_all()`. With N independent
per-index guards, it must wait for **all** of them, not just the one it
happens to check first — otherwise a background per-index checkpoint could
still be mid-flight, writing to a snapshot file `close()`'s own
`save_all()` is about to also write, reproducing the exact file-write race
§4/§5d were built to prevent, just at per-index granularity instead of
graph-wide.

**Correction (post-implementation review):** the code sketch originally here
used `checkpoint_states.read()`, dropped before locking the individual
guards:

```rust
// WRONG — see explanation below
let states = self.checkpoint_states.read();
let _guards: Vec<_> = states.values().map(|s| s.in_progress.lock()).collect();
drop(states);
self.index_manager().save_all()
```

This under-guards: per-index state is created *lazily*, on first touch
(§13c), not proactively at schema-declaration time. A concurrent commit on
a sibling `Graph` clone that's the *first-ever* mutation to an
already-declared index in this process inserts a brand-new
`PerIndexCheckpointState` into the map — and if that happens after
`close()`'s `read()` snapshot but before `close()` finishes, the new
state's guard is never collected. If that same commit's op count also
crosses threshold, its spawned background thread's `try_lock()` on that
guard succeeds (since `close()` never took it), and its `save()` call can
run concurrently with `close()`'s own `save_all()` writing the *same*
snapshot file — since neither `save()` nor `save_all()` synchronize with
each other except through these guards. That's a real, if narrow, instance
of the exact file-write race this section calls out, not the
already-accepted "schema change mid-iteration" case (which is about the
*set of declared indexes* changing, a different thing from a *known*
index's per-process checkpoint state being created for the first time).

Fixed by holding the map lock as a **write** lock through guard collection,
closing the window entirely — no new entry can be inserted (the `commit()`
lookup/insert path in §13c also goes through this same lock) until
`close()` has already captured every existing state's guard:

```rust
pub fn close(self) -> Result<(), StoreError> {
    let states = self.checkpoint_states.write();
    let state_arcs: Vec<_> = states.values().cloned().collect();
    let _guards: Vec<_> = state_arcs.iter().map(|s| s.in_progress.lock()).collect();
    drop(states);
    self.index_manager().save_all()
}
```

No deadlock risk: the background checkpoint thread never touches
`checkpoint_states` (it already holds its own `Arc<PerIndexCheckpointState>`
captured at spawn time), so it can't be waiting on the write lock while
`close()` waits on its `in_progress` guard. Concurrent commits on sibling
clones simply block on the map lock until `close()` finishes collecting
guards, then proceed normally.

This narrows, but doesn't fully eliminate, the race: a sibling clone can
still commit and spawn a background save for an *already-known* index
concurrently with `close()`'s `save_all()` call itself (after the write
lock is released) — an inherent consequence of sibling clones staying live
and mutable during another clone's `close()`, not something guard
collection can prevent without a much larger global write-barrier. In that
residual case `save_snapshot_file`'s CRC-32C (`persistence.rs`) still
guarantees corruption is *detected* on next load rather than silently
accepted, and the WAL remains the source of truth either way — worst case
is a wasted/corrupted snapshot forcing a full WAL replay on next open, not
lost data. Accepted as a known limitation rather than engineered away here.

### 13f. Reused vs. new machinery

To be explicit about what's actually new work here versus what's a direct
copy of already-reviewed, already-tested code:

| Piece | Status |
|---|---|
| `Mutex<()>` RAII guard pattern | Reused verbatim (was the fix for the `close()` deadlock in §5d) |
| CAS-based `spawn_gate: AtomicBool` | Reused verbatim (was the fix for the thread-spawn storm) |
| `ResetGateOnDrop` panic-safety wrapper | Reused verbatim |
| `save()` / `gc_vector_wal` | Fully existing, unmodified |
| Per-`(entity_type, property)` state map | New |
| Grouping `vector_pending_ops` by index before triggering | New |
| `close()` waiting on N guards instead of 1 | New (small, but the one place a subtle bug could hide) |

### 13g. Python bindings

`IndexOptions`/`PerIndexOptions` already have a working Python-to-Rust
plumbing path — `_builder.py`'s `Graph.__init__` already builds an
`index_dict` with a `per_index_overrides` list, and `bindings/python/src/lib.rs`
already parses each override entry's `memory_limit_bytes` field
(`lib.rs:395-422`). Adding `checkpoint_mutation_threshold` to that same
per-override dict, and `default_checkpoint_mutation_threshold` alongside
the existing `default_memory_limit` at the `IndexOptions` level, follows
the identical parsing pattern already in place — no new mechanism, same
shape as the memory-limit fields sitting right next to it.

### 13h. Migration — no backward-compat shim

`GraphOptions::checkpoint_mutation_threshold` hasn't shipped in a release —
it was added this session, on this branch. There's no external caller to
protect, so no seed/fallback logic is needed. Plan is a clean replacement,
not a compatibility layer:

- Remove `GraphOptions::checkpoint_mutation_threshold` and its builder
  method (`schema/definition.rs`).
- Move that exact field, doc comment, and default (`None`) to
  `IndexOptions::default_checkpoint_mutation_threshold` (§13b).
- Update the one call site in `Graph::open_with_options` (`api.rs`) that
  currently reads `options.checkpoint_mutation_threshold` to instead read
  it off `options.index.default_checkpoint_mutation_threshold` when
  constructing the initial `CheckpointState`/`CheckpointStateMap`.
- Python: rename `GraphOptions(checkpoint_mutation_threshold=...)` to the
  equivalent `IndexOptions(default_checkpoint_mutation_threshold=...)`
  parameter, update `_builder.py`, `__init__.pyi`, and `lib.rs`'s parsing
  accordingly — same three files touched when the field was first added,
  just relocated.
- Update `docs/guides/vector_search.md` §8's examples (written last turn)
  to construct `IndexOptions` instead of `GraphOptions` directly for the
  threshold — everything else in that section stays accurate as-is.

This also simplifies §13d/§13k slightly: no dual-resolution logic between
a graph-wide field and an index-level default, just the single per-index
resolution order from §13b (per-index override → `IndexOptions` default →
disabled).

### 13i. Testing strategy

- Two indexes, two different thresholds, confirm each triggers
  independently and a low-volume index's commits never trigger the
  other's save (direct test of the grouping logic in §13d).
- `close()` with two simulated in-flight per-index checkpoints
  (mirroring `test_close_waits_for_checkpoint`'s barrier-based pattern,
  doubled), confirming it waits for both before proceeding.
- Reuse the exact empirical verification approach already used to validate
  §1–12: temporary instrumentation counters, run under concurrent load,
  confirm no thread-spawn storm per index (the same 40-concurrent-commits
  test from this doc's implementation review, repeated per index).
- Regression test mirroring `test_graph_close_with_clones`, but with
  multiple declared indexes, confirming no per-index guard poisoning
  survives a `close()` call on one clone.

### 13j. Implementation checklist

- [x] `IndexOptions::default_checkpoint_mutation_threshold`,
      `PerIndexOptions::checkpoint_mutation_threshold` (+ builder methods,
      matching `with_memory_limit`'s pattern)
- [x] `PerIndexCheckpointState` + `CheckpointStateMap`, lazy entry creation
- [x] Group `vector_pending_ops` by `(entity_type, property)` in `commit()`,
      deriving `entity_type` from `EntityKey` rather than hardcoding Vertex
- [x] Per-index trigger logic (§13d), reusing the CAS gate + RAII reset
      pattern verbatim
- [x] `close()` waits on all per-index guards (§13e) — highest-priority
      review item. Initial implementation under-guarded (`read()`-then-drop
      missed states created lazily by concurrent commits); fixed to hold
      the map lock as `write()` through guard collection — see §13e's
      post-implementation correction.
- [x] §13h migration: remove `GraphOptions::checkpoint_mutation_threshold`,
      move it to `IndexOptions::default_checkpoint_mutation_threshold`
      (Rust, Python, and the `vector_search.md` §8 examples). The field was
      initially left in place, unused (dead — `Graph::open_with_options`
      never read it, and the Python `_builder.py` wrapper didn't even
      forward it to the Rust layer), silently no-opping the documented
      `checkpoint_mutation_threshold=` example in both languages until
      found in review and removed.
- [x] Python: extend the existing per-index-override dict parsing (§13g)
- [x] Tests per §13i (`test_checkpoint_trigger_logic`,
      `test_background_checkpoint_execution`, `test_close_waits_for_checkpoint`,
      `test_graph_close_with_two_clones`, `test_independent_per_index_triggers`
      in `graph/tests/vector.rs`)
- [x] Regression test for the `close()`-vs-lazy-state-creation race fixed
      above: `test_new_index_state_blocked_during_close_guard_collection`
      in `graph/tests/vector.rs` verifies the write-lock mechanism actually
      blocks a concurrent first-touch commit from creating a new
      `PerIndexCheckpointState` entry while guards are being collected.
- [ ] Optional: cleanup pass for orphaned checkpoint-state entries on
      `drop_vector_index` (not correctness-critical, deferred by default)

### 13k. Complexity & effort estimate

**Low-to-moderate — smaller than §1–12 was**, because the two hardest
problems (getting the guard semantics right, and proving the storage layer
supports independent per-index saves) are already solved and being
reused, not reinvented:

- **Configuration + plumbing** (§13b, §13g) — *low effort*, direct copy of
  the existing `memory_limit` pattern at every layer (Rust struct, builder
  method, Python dict parsing).
- **Per-index state + grouping in `commit()`** (§13c, §13d) — *low-to-moderate*.
  Mechanically small, but this is where a new class of bug *could* hide if
  the lazy-entry creation isn't handled carefully under concurrent access
  (two commits racing to create the same map entry) — worth an explicit
  `entry().or_insert_with(...)`-style atomic get-or-create rather than a
  check-then-insert.
- **`close()` change** (§13e) — *low effort* to write, but the
  highest-priority item for review, since it's the one place this design
  could reintroduce the exact class of bug §5d/§8 already fixed once, now
  at a different granularity.
- **Testing** (§13i) — *moderate*, but every test pattern needed already
  exists and gets duplicated/parameterized rather than designed from
  scratch — real effort, low risk of missing a scenario.
- **No changes** to `save()`, `gc_vector_wal`, snapshot format, or
  `CF_VECTOR_WAL` — confirmed in §13a that none of this needs to move.

Rough shape: similar file count to §1–12 (`schema/definition.rs`,
`vector/traits.rs`, `graph/logical.rs`, `api.rs`, Python bindings, tests),
most of it structurally mechanical once §13c/§13d's core pattern is
written once and then it's "the same thing, keyed by index."
