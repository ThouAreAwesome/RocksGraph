# Integer type policy

Status: implemented

## Problem

Physical steps mix three classes of integers inconsistently:

1. **Value types** (`Primitive::Int32`, `Primitive::Int64`, `Primitive::UInt16`) —
   the fixed-width types that appear in traversers and Gremlin output.
2. **User-facing configuration parameters** (`limit`, `times`, `range` bounds, `skip`/`tail`
   counts) — these come from the builder API and are stored in logical/physical steps.
3. **Runtime indexing** (`current_idx`, `cursor`, `buffer.len()`) — must be `usize`.

Before this policy some parameters used `u32` (`limit`, `times`) while others used
`u64` (`range`, `skip`, `tail`).  There was no principled rule.

## Decision

| Use case | Type | Why |
|----------|:---:|-----|
| **Builder configuration: limit, times, range, skip, tail** | `i64` | Matches the project's value type `Primitive::Int64` — widest range, consistent with output types, no unsigned/negative ambiguity.  `limit(-1)` simply wraps to a very large number (unbounded), identical in behaviour to `limit(u32::MAX)`. |
| **Accumulators (count, sum)** | `i64` / `u64` | Output is `Primitive::Int64`; `i64` for all accumulators. |
| **Collection indexing** | `usize` | Required by Rust; never user-facing. |
| **RocksDB scan limits** (`get_adjacent_edges`, `scan_edges`) | `u32` | RocksDB's native limit type; consumed at the boundary with a `i64 → u32` cast. |
| **Property/edge keys, label IDs** | `u16` / `i32` | Fixed-width on-disk encoding. |

## Changes

| Layer | Before | After |
|-------|--------|-------|
| `limit(n)` | `u32` | `i64` |
| `range(lo, hi)` | `u64` | `i64` |
| `skip(n)` | `u64` | `i64` |
| `tail(n)` | `u64` | `i64` |
| `times(n)` | `u32` | `i64` |

All three layers (builder → logical step → physical step) use `i64`.  The internal
`index`/`skipped`/`cursor` fields use `usize` with an `as usize` cast at the
comparison site (cost-free widening on 64-bit platforms).

RocksDB edge-scan limits remain `u32` — cast from `i64` at the boundary in
`get_adjacent_edges`/`scan_edges`.  Schema version remains `u64` (internal versioning
counter, unrelated to query parameters).

## Cast policy

```rust
// Physical step: config → indexing
if self.current_idx >= self.limit as usize { ... }

// RocksDB boundary: i64 → u32
let db_limit: u32 = limit.max(0).min(u32::MAX as i64) as u32;
```
