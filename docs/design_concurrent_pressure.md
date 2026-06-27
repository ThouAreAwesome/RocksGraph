# Design: concurrent transaction pressure testing

## Problem

RocksGraph uses Optimistic Concurrency Control (OCC) at commit time.  The existing
test suite verifies this in two narrow ways, neither exercising real concurrency:

- **Deterministic two-handle interleaving** — two transaction handles on *one* thread,
  test chooses the exact commit order.  Good for "does X always conflict with Y",
  but proves nothing about multi-threaded races.
- **Schema-only stress test** — 8 threads racing to auto-register labels/keys.  Covers
  schema registration well, but **not** contention on ordinary vertex/edge data writes.

What's missing: a real-thread test for data-write contention, plus a sustained-load
benchmark mode for throughput/latency measurement.

## Goals & non-goals

- **Goals:** Add correctness tests for hotspot contention (no lost updates) and disjoint
  writes (zero conflicts); extend `bench_write` with a hotspot mode for sustained load
  measurement.
- **Non-goals:** Mixed read/write isolation testing; `loom`-style exhaustive interleaving;
  exponential backoff beyond simple jitter.

## Design

### 1. Data-write contention tests — `src/concurrency_tests.rs`

A new test file written against the **public API** (`Graph`, `TraversalBuilder`,
`Value`, `StoreError`, `__`) — same surface `bench_write.rs` uses.  Declared in `lib.rs`:

```rust
#[cfg(test)]
mod concurrency_tests;
```

#### Hotspot contention — no lost updates

- `N` threads share one vertex with an integer counter property.
- Each thread loops: read counter, write `current + 1`, retry on `StoreError::Conflict`
  with jittered delay.  Only `Conflict` is retried — `LockError` means a poisoned
  `RwLock` (another thread panicked), which never clears.
- `Barrier` releases all threads at once to maximize true overlapping commits.
- **Invariant**: each thread returns its own committed-increment count; the watchdog's
  supervisor thread sums all results via an `mpsc` channel.  That sum must exactly
  equal the counter's final database value.

#### Disjoint writes — zero conflicts

- `N` threads, each assigned a disjoint ID range, creating vertices and connecting
  edges — no shared keys.
- Pre-declared schema via `Graph::open_management()` — avoids spurious conflicts
  from Auto-mode label/property-key registration.
- **Invariant**: zero `StoreError::Conflict` across all threads.  Single-threaded
  verification confirms every vertex/edge exists with expected properties.

### 2. `bench_write` hotspot mode — sustained load

Extend `bin/bench_write.rs` with `--mode hotspot|disjoint`:

- Hotspot mode targets a small fixed set of vertex/edge IDs repeatedly.
- Reuses existing `hdrhistogram`, retry loop, `--parallelism`, percentile reporting.
- Uses larger retry budget (50 attempts, jittered 1-10ms delay) tuned for high
  contention — avoids survivor-bias in latency histograms.

`bench_read.rs` is unaffected — concurrent reads never conflict in OCC.

## Constraints / invariants

- Only `StoreError::Conflict` is retried; any other error is a hard failure.
- Disjoint-writes test must pre-declare schema before spawning threads to avoid
  Auto-mode registration conflicts on shared schema metadata keys.
- Wrapped in `mpsc::channel` + `recv_timeout` watchdog — a deadlock fails fast
  instead of hanging CI.

## Implementation plan

1. Add `src/concurrency_tests.rs`; declare `#[cfg(test)] mod concurrency_tests;`
   in `src/lib.rs`.
2. Implement `hotspot_contention_preserves_all_updates`.
3. Implement `disjoint_writes_never_conflict`.
4. Tune thread/iteration counts for low-single-digit-second test suite impact.
5. Add `--mode`/`--hotspot-keys` to `bin/bench_write.rs`.

## Files changed

| File | Change |
|------|--------|
| `src/lib.rs` | `#[cfg(test)] mod concurrency_tests;` |
| `src/concurrency_tests.rs` | New file, both contention tests |
| `src/bin/bench_write.rs` | `--mode`/`--hotspot-keys` flags, hotspot key-selection |
| `src/schema/tests.rs` | Additional complex-distinct-schemas test |
