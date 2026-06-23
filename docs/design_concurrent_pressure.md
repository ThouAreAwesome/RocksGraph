# Design: Concurrent Transaction Pressure Testing

## Problem

RocksGraph uses Optimistic Concurrency Control (OCC) at commit time: each transaction stages
changes in an in-memory overlay, and `commit()` fails with `StoreError::Conflict` if another
transaction committed to an overlapping key first. The existing test suite verifies this in two
narrow ways — neither of which exercises real concurrency under load:

- **Deterministic two-handle interleaving** ([graph.rs:2376](../src/graph.rs#L2376) `run_conflict`/
  `run_no_conflict`, [transaction.rs:579](../src/store/rocks/transaction.rs#L579)
  `test_read_write_conflict`) — two transaction handles on *one* thread, with the test choosing
  the exact commit order. Good for "does X always conflict with Y," but proves nothing about
  what happens when many threads race for real.
- **One real-thread stress test, schema-only** ([schema/tests.rs:385](../src/schema/tests.rs#L385)
  `test_concurrent_auto_mode_writes_do_not_starve_schema_lock`) — 8 threads racing to
  auto-register the same labels/keys, retrying on conflict, wrapped in an `mpsc`/`recv_timeout`
  watchdog. This already covers "concurrent schema registration doesn't deadlock or starve" well.
  It does **not** cover contention on ordinary vertex/edge *data* writes — the case most likely to
  matter to a caller running real concurrent traffic.

What's missing is a real-thread test for **data-write contention** (not schema), plus a way to
generate **sustained concurrent load** for throughput/latency measurement rather than a fixed
8×60-iteration smoke test.

## Non-goals (deferred)

- **Mixed read/write isolation testing** (verifying `LogicalSnapshot` repeatable-read holds while
  writers mutate the graph) is a different problem — it tests snapshot isolation semantics, not
  OCC conflict handling — and deserves its own design. Not in scope here.
- **Concurrent schema-update *contention*** is already covered by the existing integration test
  [`test_concurrent_auto_mode_writes_do_not_starve_schema_lock`](../src/schema/tests.rs#L385)
  (8 threads racing to register the *same* handful of labels/keys). No new test was planned for
  that. During implementation, a second schema test was added alongside it —
  `test_concurrent_auto_mode_complex_distinct_schemas` (12 threads × 40 iterations, each
  registering *fully unique* labels/keys per iteration — 1 vertex label, 1 edge label, and 2
  property keys — asserting all ~1920 entries land in the final schema). That's a genuinely
  different angle — registration *volume* with near-zero
  per-key contention, rather than many threads fighting over one key — so it's accepted as a
  worthwhile addition to the schema-side coverage, not a duplicate of the existing test.
- **Jittered backoff**: While a fully featured exponential backoff system is deferred, a simple
  random jitter delay (e.g. `thread::sleep(Duration::from_millis(rng.gen_range(1..10)))`) is adopted
  in the hotspot test to break lockstep scheduling collisions induced by the `Barrier` releases.
- **`loom`-style exhaustive interleaving exploration.** Real-thread tests are probabilistic, not
  exhaustive; `loom` would model-check every interleaving instead. That's a genuinely different
  testing style (and a new dependency) — worth a future design doc if real-thread tests turn out
  to miss a rare race, not bundled into this one.

## Design

Two deliverables, both built entirely from patterns and dependencies already in the codebase —
no new crates, no new test-execution mechanism.

### 1. New data-write contention tests — `src/concurrency_tests.rs`

A new sibling test file, mirroring the existing
[`gremlin::multi_edge_tests`](../src/gremlin/multi_edge_tests.rs) precedent (a thematically
distinct integration-style suite gets its own file rather than being squeezed into `graph.rs`'s
existing inline `mod tests`, which is already 3000+ lines). Declared in `lib.rs` alongside the
other top-level modules:

```rust
// src/lib.rs
#[cfg(test)]
mod concurrency_tests;
```

Written entirely against the **public API** (`Graph`, `TraversalBuilder`, `Value`, `StoreError`,
`__`) — the same surface `bench_write.rs`/`bench_read.rs` already use as external callers — so it
exercises the system exactly as a real client would, with no crate-internal shortcuts.

#### 1a. Hotspot contention — no lost updates

*Goal*: maximize the conflict rate and prove OCC never loses an update under contention.

- `N` threads share one vertex (`id: 0`) with an integer counter property.
- Each thread loops `ITERATIONS` times: read the current counter, write back `current + 1`,
  retry on `StoreError::Conflict` with a randomized jitter delay (e.g. 1-10ms) to break lockstep
  scheduling collisions induced by the `Barrier`, giving up and failing the test if a single
  increment exhausts its retry budget. **Only `Conflict` is retried** — `StoreError::LockError`
  is deliberately *not* in the retry set: its one call site
  ([`schema/management.rs:135`](../src/schema/management.rs#L135)) only fires when
  `RwLock::write()` returns `Err`, which in `std::sync::RwLock` means the lock is *poisoned*
  (another thread already panicked while holding it) — not contention, and not reachable from
  this test's plain `.property()` writes anyway (no `SchemaManagement` session involved). If it
  ever did fire, retrying would just burn the retry budget on a condition that never clears;
  treating it (and anything besides `Conflict`) as an immediate hard failure is both more correct
  and surfaces the real bug instead of masking it as "exhausted retries."
- A `std::sync::Barrier` releases all threads at once before their first attempt — std-only, one
  line, and it directly serves the stated goal (maximize true overlapping commits) instead of
  leaving thread start order to OS scheduling jitter.
- **Invariant**: each thread's closure returns its own count of successfully committed
  increments (no shared `AtomicUsize` needed); the watchdog's supervisor thread sums all
  `JoinHandle` results and sends the total through the same `mpsc` channel used for completion.
  That sum must exactly equal the counter's final database value — any mismatch means an update
  was lost (a commit that should have conflicted didn't) or double-counted.
- Wrapped in the same `mpsc::channel` + `recv_timeout` watchdog as the schema-lock test, so a
  deadlock regression fails fast with a clear message instead of hanging CI.

#### 1b. Disjoint writes — zero conflicts, full integrity

*Goal*: prove the write path is safe under parallel load when there's no logical reason to
conflict, and that nothing gets silently dropped or corrupted.

- `N` threads, each assigned a disjoint ID range (thread `i` owns `[i*1000, (i+1)*1000)`), each
  creating vertices and connecting them with edges — no shared keys at all.
- **Pre-declared schema**: declare every label/property key the threads will use via
  `Graph::open_management()` before spawning them — exactly the same reason
  [`bench_write.rs`](../src/bin/bench_write.rs#L116-L134) does this already: in Auto mode, every
  worker's *first* transaction would otherwise race to auto-register the same handful of
  labels/keys, which touch one shared schema metadata key regardless of how disjoint the actual
  vertex/edge IDs are. That's a real `StoreError::Conflict`, just not the kind this test is
  trying to catch — without pre-declaring, the "zero conflicts" invariant below would be false
  for a reason that has nothing to do with data-write disjointness. (1a's hotspot test doesn't
  *need* this — it already expects and retries through conflicts — but doing it there too removes
  one source of wasted retries on the first iteration.)
- **Invariant**: zero `StoreError::Conflict` across all threads (any conflict here indicates a
  key-collision bug, e.g. two "disjoint" ranges accidentally overlapping, or false-positive
  conflict detection). After joining, a single-threaded verification pass confirms every vertex
  and edge exists with the expected properties and endpoints.

### 2. `bench_write` hotspot mode — sustained load, not just a smoke test

The unit tests above prove correctness at a fixed, small scale (enough threads/iterations to
exercise the race, bounded so the test suite stays fast). They are not a load test. For
throughput/latency under *sustained* contention, extend the existing
[`bin/bench_write.rs`](../src/bin/bench_write.rs) rather than building a second harness:

- Add a `--mode hotspot|disjoint` flag (default `disjoint`, today's existing behavior —
  upserting from the input edge-list file with effectively-low collision odds).
- In `hotspot` mode, instead of reading `(src, dst)` pairs from the input file, every worker
  repeatedly targets the *same* small fixed set of vertex/edge IDs (configurable count, e.g.
  `--hotspot-keys 10`) for `--iterations` rounds.
- Reuses 100% of the existing machinery: per-thread `hdrhistogram::Histogram`, the
  `MAX_RETRIES`/`RETRY_DELAY_MS` retry loop, the `--parallelism` thread count, the merged
  percentile report at the end. Only the key-selection logic changes.
- This is the tool to answer "what's commit latency at p99 when 32 threads are hammering the same
  10 vertices," which the unit tests above are deliberately too small-scale to answer.
- The existing `MAX_RETRIES`/`RETRY_DELAY_MS` (3 attempts, flat 1ms) are tuned for disjoint
  mode's low conflict rate and are kept as-is there. Hotspot mode uses its own larger budget
  (50 attempts) with a jittered 1-10ms delay — confirmed by running it locally with only 10
  hotspot keys at `--parallelism 8`: with the disjoint-mode budget, several operations were
  dropped outright ("Failed to upsert edge ... after 3 retries") instead of just running slower,
  which would have silently understated both the mutation count and the latency histogram
  (survivor bias — only the luckier ops get recorded).

`bench_read.rs` is unaffected — concurrent reads never conflict in this OCC model (conflicts are
a commit-time, write-write phenomenon), so a "hotspot" mode has nothing to add there.

## Implementation Plan

1. Add `src/concurrency_tests.rs`; declare it `#[cfg(test)] mod concurrency_tests;`
   in `src/lib.rs`.
2. Implement `hotspot_contention_preserves_all_updates`: pre-declare schema, shared-vertex
   counter increment race, `Barrier`-synchronized start, jittered retry on `Conflict` only (hard
   `unwrap`/fail on any other error), each worker closure returning its own successful-commit
   count, summed by the watchdog's supervisor thread and sent through the `mpsc` channel. Assert
   the sum equals the counter's final database value.
3. Implement `disjoint_writes_never_conflict`: pre-declare schema via `open_management()`,
   per-thread disjoint ID ranges, assert zero `StoreError::Conflict` across all threads, then
   verify every created vertex/edge in a single-threaded pass.
4. Run `cargo test` and `just full-check`; tune thread count / iteration count so the two new
   tests add low-single-digit seconds to the suite, not minutes.
5. (Separate, smaller change) Add `--mode`/`--hotspot-keys` to `bin/bench_write.rs`; manually spot-check
   a hotspot run locally (not part of `cargo test` — it's a standalone load-generation binary, same as today).

## Affected Files

- `src/lib.rs` — new `#[cfg(test)] mod concurrency_tests;` declaration
- `src/concurrency_tests.rs` — new file, both contention tests
- `src/bin/bench_write.rs` — `--mode`/`--hotspot-keys`/`--iterations` flags, hotspot key-selection
  branch, and a hotspot-specific retry budget/jitter (see §2 note below)
- `src/schema/tests.rs` — additional `test_concurrent_auto_mode_complex_distinct_schemas`
  (accepted addition beyond the original plan; see Non-goals)
