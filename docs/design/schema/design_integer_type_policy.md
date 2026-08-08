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

This policy covers two genuinely different axes, kept in separate sections below:

- **Public API policy** — what types traversal parameters and return values use.
  Governed by "prefer `Int32`/`Int64`; use `UInt16` only when nothing else fits."
- **Internal encoding** — types used purely inside the storage/protocol boundary
  (on-disk keys, RocksDB's own API). Governed by the external format, not by the
  public API rule — these never reach a caller and are not "fixed" to `i64`.

## Decision: public API (traversal parameters and return values)

| Use case | Type | Why |
|----------|:---:|-----|
| **Builder configuration: limit, times, range, skip, tail** | `i64` | Matches the project's value type `Primitive::Int64` — widest range, consistent with output types, no unsigned/negative ambiguity.  `limit(-1)` simply wraps to a very large number (unbounded), identical in behaviour to `limit(u32::MAX)`. |
| **Accumulators (count, sum, min/max)** | `i64` | Output is `Primitive::Int64`; every accumulator (`count.rs`, `numeric_reducers.rs`) uses `i64` exclusively — no `u64` anywhere in this path. |
| **Edge `rank`** | `u16` (`Primitive::UInt16`) | The one `UInt16` that reaches the public API. `Rank` is structurally `u16` end-to-end: on-disk encoding, `GetEStep` key construction, and `.property("rank", 5u16)` / `.values(["rank"])` / `Edge::rank` all use it with no widening. There is no better option — `Int32`/`Int64` would just add a pointless cast at every boundary for a value that is never arithmetic, only an identity/ordering disambiguator. |

`Value`/`Primitive` intentionally expose only these three integer variants — no `I8`/`U8`/`U32`/`U64`/`I16` sprawl. Anything else (label names, property keys) crosses the public API as `String`/`SmolStr`, never as a raw numeric ID.

## Internal encoding (never user-facing)

These types are dictated by an external format (on-disk layout or RocksDB's own API), not by the public API rule above. They never appear in a `Value`/`Predicate` or cross the `TraversalBuilder` boundary — `LabelId` and `PropKeyId` are always resolved to/from `String` before reaching the caller.

| Use case | Type | Why |
|----------|:---:|-----|
| **Collection indexing** | `usize` | Required by Rust; never user-facing. |
| **RocksDB scan limits** (`get_adjacent_edges`, `scan_edges`) | `u32` | RocksDB's native limit type; consumed at the boundary with a `i64 → u32` cast. |
| **Property/edge keys, label IDs** | `u16` / `i32` | Fixed-width on-disk encoding, resolved to/from `String` before reaching the public API. |
| **Schema version** | `u64` | Internal versioning counter, unrelated to query parameters. |

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

## Cast policy

```rust
// Physical step: config → indexing
if self.current_idx >= self.limit as usize { ... }

// RocksDB boundary: i64 → u32
let db_limit: u32 = limit.max(0).min(u32::MAX as i64) as u32;
```
