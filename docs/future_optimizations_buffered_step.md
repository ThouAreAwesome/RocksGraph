# Future optimization: BufferedStep pipeline overhead

Status: proposal only. Not scheduled.

## Current design

Every physical step is wrapped in `BufferedStep<T>` which adds:

```
inner: RefCell<BufferedInner<T>>    // dynamic borrow check
buffer: VecDeque<Rc<Traverser>>     // FIFO output queue
```

Every `next()` call does `self.inner.borrow_mut()`, which performs a `RefCell` runtime borrow
check even though the engine is strictly single-threaded (each step's `RefCell` is independent,
so re-entrancy cannot occur — but the check still runs).

## Proposed optimization: unsafe bypass for RefCell

The pipeline's call pattern is:

```
step_k.next()          → borrow_mut step_k
  → step_k.produce()   → borrow_mut step_k-1
    → step_k-1.next()  → borrow_mut step_k-1
      → ...
```

These are different `RefCell` instances — no aliasing possible. The borrow check is redundant.

Option: a compile-time flag (`cfg(feature = "unsafe-buffer")`) that replaces `RefCell` with `UnsafeCell`
and documents the invariant:

```rust
// SAFETY: each BufferedStep has its own UnsafeCell. The call chain is strictly
// parent → child → grandchild — never re-entrant on the same step. No two threads
// access the same step concurrently (the engine is single-threaded).
```

Gain: one branch elimination per `next()` call. On a 10-step pipeline with 10K traversers:
100K dynamic borrow checks eliminated. Benchmarked impact expected at 3—8% wall-clock
reduction for pure-pipeline workloads (no I/O).

Risk: `UnsafeCell` drops Rust's aliasing guarantees. A future code change that introduces
re-entrancy (e.g., a step calling its own `next()` during `produce()`) would become
undefined behavior silently.

## Pre-allocation of buffer capacity

Steps with known fan-out (e.g., `out()` with a high average degree) could benefit from
pre-allocated buffer capacity set at build time:

```rust
impl BufferedStep<T> {
    fn with_capacity(core: T, cap: usize) -> Self { ... }
}
```

Currently the `VecDeque` starts at capacity 0 and grows organically. For a step that
consistently produces 3—5 traversers, pre-allocating 8 slots avoids the initial
reallocation chain.

Gain: 1—2 heap allocations saved per step per pipeline invocation. Small but zero-risk.

## Rc elision when path tracking is absent

The buffer stores `Rc<Traverser>`. When no downstream step uses `as()`/`select()`/`path()`,
the `Rc` indirection and refcount are wasted:

- `Rc::new(Traverser { ... })` allocates on every value production
- `Rc::clone(&t)` bumps an atomic counter on every injection

If the optimizer could determine "no path consumer in the pipeline", steps could emit
plain `Traverser` (owned, no Rc) and the buffer type could switch to `VecDeque<Traverser>`:

```rust
struct BufferedStep<T> {
    inner: RefCell<BufferedInner<T>>,
    buffer: VecDeque<Traverser>,   // no Rc when path tracking is off
}
```

Gain: eliminates one heap allocation per traverser per step. For a 5-step pipeline with
10K traversers: 50K allocations removed. Benchmarked impact expected at 15—25% for
non-path workloads.

Risk: requires an optimizer pass to detect path usage and select the buffer type at
build time. Non-trivial implementation.

## Summary

| Idea | Effort | Impact | Risk |
|------|:---:|:---:|:---:|
| `UnsafeCell` for RefCell | 2h | 3—8% | Medium — unsafe |
| Pre-allocate buffer | 30m | <1% | None |
| Rc elision | 3d | 15—25% | High — optimizer pass |
