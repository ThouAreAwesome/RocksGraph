# Design: multi-edges — `EdgeMode` and user-specified ranks

## Problem

RocksGraph supports multiple edges of the same label between the same vertex pair using
a structural `rank` field (`u16`) inside `EdgeKey`.  Currently this capability is
unconstrained:

1. No way to enforce a **simple graph** constraint (at most one edge per label per pair).
2. No clean API for users to specify structural `rank` values.

## Goals & non-goals

- **Goals:** Configurable `EdgeMode` (single/multi) at the graph level; schema-integrated
  enforcement; explicit rank provision via `.property("rank", N)`; no auto-rank generation.
- **Non-goals:** Per-label multiplicity (deliberately rejected — see below); auto-incrementing
  ranks; downgrading from `Multi` back to `Single`.

### Why graph-level, not per-label

- Most graphs are uniform in practice.  Per-label mixing is rarely the actual requirement.
- One less axis of schema state to declare, version, and reason about.  A single graph-wide
  field is one value, set once — no per-label lookup, no "what if this label was never
  declared" edge case.

## Design

### 1. EdgeMode in Schema

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EdgeMode {
    #[default]
    Single,  // at most one edge per (src, label, dst) triple
    Multi,   // multiple edges distinguished by rank
}

pub struct Schema {
    pub mode: SchemaMode,
    pub edge_mode: EdgeMode,  // graph-wide, not per-label
    // ...
}
```

`edge_mode` is set once at graph creation and changed only through
`SchemaManagement::set_edge_mode(...)`.  It can only move `Single` → `Multi`, never back
— downgrading would require validating every existing triple, which this design does not
attempt.

### 2. Specifying ranks via `.property("rank", N)`

```rust
g.V(1).addE("knows").to(2)
    .property("rank", 5u16)
    .property("weight", 0.5f64)
    .next()?;
```

The optimizer/builder folds `property("rank", N)` into `AddEStep`'s `rank` field,
removing it from the property map.

### 3. Write-time enforcement

- **`EdgeMode::Single`**: `rank == DEFAULT_RANK (0)` enforced; non-zero rank → error;
  duplicate edge at rank 0 → `StoreError::DuplicateEdge`.
- **`EdgeMode::Multi`**: writes with specified rank; duplicate rank → `DuplicateEdge`.

## Constraints / invariants

- `edge_mode` can only ratchet `Single` → `Multi`; running `set_edge_mode(Single)`
  once already `Multi` returns `StoreError::SchemaConflict`.
- No per-label override — every edge label obeys the same `edge_mode`.

## Files changed

| File | Change |
|------|--------|
| `src/schema/definition.rs` | `EdgeMode` enum; `Schema.edge_mode` field; CAS guard for single-direction ratchet |
| `src/schema/management.rs` | `set_edge_mode()` |
| `src/planner/optimizer/merge_adde_ids.rs` | Fold `property("rank", N)` → `AddEStep.rank` |
| `src/graph/logical.rs` | `add_edge()` reads `schema.edge_mode`; enforces rank constraint |
| `src/engine/volcano/builder/build_step.rs` | `Property("rank")` → `AddEStep.rank` folding |
| `src/gremlin/traversal/mod.rs` | `property("rank", N)` passes through to optimizer |
