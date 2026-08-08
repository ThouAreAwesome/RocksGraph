# Design: BufferedStep pipeline overhead (future optimization)

> Status: proposal only. Not scheduled.

## Problem

Every physical step is wrapped in `BufferedStep<T>` which adds `RefCell<BufferedInner<T>>`
and `VecDeque<Rc<Traverser>>`.  The `RefCell` runtime borrow check runs on every `next()`
call even though the engine is strictly single-threaded and no re-entrancy can occur.

## Goals & non-goals

- **Goals:** Explore optimizations that reduce pipeline overhead — `RefCell` bypass,
  buffer pre-allocation, `Rc` elision.
- **Non-goals:** Implement any of these; evaluate trade-offs only.

## Design — three proposed optimizations

### 1. Unsafe bypass for RefCell

Replace `RefCell` with `UnsafeCell` behind a compile-time flag
(`cfg(feature = "unsafe-buffer")`):

```rust
// SAFETY: each BufferedStep has its own UnsafeCell. The call chain is strictly
// parent → child → grandchild — never re-entrant on the same step. No two threads
// access the same step concurrently (the engine is single-threaded).
```

**Gain:** one branch elimination per `next()` call.  On a 10-step pipeline with 10K
traversers: 100K dynamic borrow checks eliminated.  Expected 3–8% wall-clock reduction
for pure-pipeline workloads.

**Risk:** `UnsafeCell` drops Rust's aliasing guarantees.  A future re-entrant code
change would become undefined behaviour silently.

### 2. Pre-allocation of buffer capacity

```rust
impl BufferedStep<T> {
    fn with_capacity(core: T, cap: usize) -> Self { ... }
}
```

**Gain:** 1–2 heap allocations saved per step per pipeline invocation.  Zero risk.

### 3. Rc elision when path tracking is absent

When no downstream step uses `as()`/`select()`/`path()`, the `Rc` indirection is wasted.
Emit plain `Traverser` (owned, no `Rc`) and switch buffer to `VecDeque<Traverser>`.

**Gain:** one heap allocation eliminated per traverser per step.  For a 5-step pipeline
with 10K traversers: 50K allocations removed.  Expected 15–25% for non-path workloads.

**Risk:** requires an optimizer pass to detect path usage and select the buffer type at
build time.  Non-trivial implementation.

## Summary

| Idea | Effort | Impact | Risk |
|------|:---:|:---:|:---:|
| `UnsafeCell` for RefCell | 2h | 3–8% | Medium — unsafe |
| Pre-allocate buffer | 30m | <1% | None |
| Rc elision | 3d | 15–25% | High — optimizer pass |
