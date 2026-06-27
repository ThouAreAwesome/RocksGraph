# Integer type policy for physical steps

Status: implemented

## Problem

Physical steps mix three classes of integers inconsistently:

1. **Fixed-width primitives** (`Primitive::Int32`, `Primitive::Int64`, `Primitive::UInt16`) —
   these are RocksGraph's value types. Output that flows into traverser values must use
   fixed-width types so the serialised form is identical on every platform.

2. **User-facing configuration parameters** (`limit`, `times`, `range` bounds, `skip`/`tail`
   counts) — these come from the Gremlin-idiom builder API and are stored in logical/physical
   steps. They should use a single consistent fixed-width type.

3. **Runtime indexing into Rust collections** (`current_idx`, `cursor`, `buffer.len()`) —
   these must be `usize` (Rust's platform word size) or the compiler rejects the code.

Before this audit, some steps used `u64` builder parameters (`range(lo: u64, hi: u64)`,
`skip(n: u64)`, `tail(n: u64)`) while `limit()` used `u32` and `times()` used `u32`.
There was no documented policy, and every new step picked a type ad-hoc.

## Decision

| Use case | Type | Why |
|----------|:---:|-----|
| **Accumulators (count, sum)** | `i64` / `u64` | Output is `Primitive::Int64`; large graphs can produce millions of traversers. `u64` for count, `i64` for sum (matches Primitive width). |
| **Builder configuration: limit, times, range bounds, skip/tail n** | `u32` | Same class of parameter — a bound on traverser count or iteration depth. 4 billion cap is more than any practical query crosses. Fixed 4-byte width on all platforms. |
| **Collection indexing** | `usize` | Required by Rust; never user-facing. |
| **Property/edge keys, label IDs** | `u16` / `i32` | Fixed-width on-disk encoding. |

## Changes

| Layer | File | Before | After |
|-------|------|--------|-------|
| Builder API | `gremlin/traversal/mod.rs` | `fn range(u64, u64)` | `fn range(u32, u32)` |
| | | `fn skip(u64)` | `fn skip(u32)` |
| | | `fn tail(u64)` | `fn tail(u32)` |
| Logical step | `planner/logical_step/mod.rs` | `RangeStep { lo: u64, hi: u64 }` | `u32` |
| | | `SkipStep { n: u64 }` | `u32` |
| | | `TailStep { n: u64 }` | `u32` |
| Physical step config | `engine/volcano/steps/range_skip_tail.rs` | `lo: u64, hi: u64` (RangeStep) | `u32` |
| | | `n: u64` (SkipStep, TailStep) | `u32` |
| Physical step tracking | same | `index: u64, skipped: u64, cursor: u64` | `usize` (collection indexing) |

## Unchanged (confirmed correct)

| Step | Field | Type | Rationale |
|------|-------|:---:|-----------|
| `CountStep` | `count` accumulator | `u64` | Outputs `Int64`; wraps via `as i64` |
| `SumStep` | `sum_int` accumulator | `i64` | Matches `Int64` output width; `wrapping_add` |
| `MeanStep` | `count` accumulator | `u64` | Same as CountStep |
| `MinStep` / `MaxStep` | `best_int` | `i64` | Matches `Int64` output |
| `LimitStep` | `limit` | `u32` | Already correct |
| `RepeatStep` | `times`, `iter_count` | `u32` | Already correct |
| `HasLabelStep` | `UNRESOLVED_LABEL_ID` | `i32` | Matches `LabelId` |
| All steps | `current_*_idx`, `buffer_idx`, `cursor` | `usize` | Required by Rust — collection indexing |

## Cast policy

Config fields (`u32`) are cast to `usize` at the point of indexing:

```rust
if self.current_idx >= self.limit as usize { ... }
```

The cast is a zero-cost widening on all 64-bit platforms (x86-64, ARM64, RISC-V64) and a
narrowing nop on 32-bit platforms (where `usize == u32`).  RocksGraph does not target
16-bit or 8-bit platforms.
