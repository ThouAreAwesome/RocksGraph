# Design: Configuring Multi-Edges with User-Specified Ranks

## 1. Problem Statement

RocksGraph supports multiple edges of the same label between the same pair of vertices using a structural `rank` field (a `u16` unsigned integer) inside the `EdgeKey`:

```rust
pub struct EdgeKey {
    pub primary_id: VertexKey,
    pub direction: Direction,
    pub label_id: LabelId,
    pub secondary_id: VertexKey,
    pub rank: Rank, // <-- Structural discriminator for multigraphs
}
```

Currently, this capability is unconstrained and lacks usability features:
1. **No constraint validation**: There is no way to enforce a **Simple Graph (Single-Edge)** constraint where a pair of vertices has at most one edge of a given label.
2. **Explicit Rank Provision**: In order to write multiple edges, users need a clean API mechanism to specify structural `rank` values.

---

## 2. Refined Design Goals

1. **Configurable Edge Constraint, at the graph level**: Allow users to specify whether the
   *whole graph* operates in **Single-Edge Mode** (simple graph) or **Multi-Edge Mode**
   (multigraph). Unlike JanusGraph's per-edge-label `Multiplicity`, RocksGraph deliberately
   does not support mixing modes within one graph — see "Why graph-level, not per-label"
   below.
2. **Schema Integration**: Store this configuration inside the schema registry so it is enforced consistently.
3. **Explicit Rank Provision**: Provide a clean API mechanism in the Gremlin traversal builder for users to specify custom `rank` values.
4. **No Auto-Rank**: If the user does not provide a rank:
   - In Single-Edge Mode: `rank` is strictly `DEFAULT_RANK` (0).
   - In Multi-Edge Mode: `rank` defaults to `DEFAULT_RANK` (0). If a second edge is added between the same pair of vertices, the user must explicitly define a different rank to avoid a duplicate edge error.

### Why graph-level, not per-label

JanusGraph attaches `Multiplicity` to each edge label individually. RocksGraph instead
makes this a single graph-wide setting (`EdgeMode`, below), for two reasons:

- **Most graphs are uniform in practice.** A graph is either modeling a simple-relationship
  domain (org charts, type hierarchies) or one that inherently needs parallel edges (event
  logs, transaction graphs, time-series relationships). Per-label mixing is rarely the
  actual requirement, and when it is, the "default rank 0, explicit rank for the rest"
  rule (goal 4) already gives single-edge-shaped usage for free within a multi-edge graph
  — callers who never pass a second edge of the same label never notice multi-edge mode is
  enabled.
- **One less axis of schema state to declare, version, and reason about.** A per-label
  `EdgeConfig` map needs its own entry in every schema declaration, persistence, and
  conflict-checking path (see `design_auto_schema.md` §0, §5). A single graph-wide field
  is one value, set once, that every `add_edge` call consults — no per-label lookup, no
  "what if this label was never declared" edge case.

---

## 3. Detailed Design

### 1. Edge Mode in Schema

Whether the graph allows multiple edges per `(src, label, dst)` triple is a single,
graph-wide setting — not a per-label one. We introduce an `EdgeMode` enum on `Schema`
(see `design_auto_schema.md` §0 for how it sits alongside `SchemaMode`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EdgeMode {
    /// At most one edge of a given label can exist between a vertex pair (Simple Graph).
    #[default]
    Single,
    /// Multiple edges of a given label are allowed between a vertex pair (Multigraph),
    /// distinguished by `rank`.
    Multi,
}

pub struct Schema {
    pub mode: SchemaMode,    // resolve_*/declare_* enforcement — design_auto_schema.md §0
    pub edge_mode: EdgeMode, // this design
    // ...
}
```

`edge_mode` has a single value for the whole `Schema`, not a `HashMap<LabelId, _>` — every
edge label is governed by the same setting. It is set once when the graph is created
(`GraphOptions`, bootstrapping a fresh database only) and changed thereafter only through
`SchemaManagement::set_edge_mode(...)` (`design_auto_schema.md` §4), so it participates in
the same version/CAS guarantee as every other schema change, and is persisted/read the same
way as `SchemaMode` — every process opening this graph sees the same `edge_mode`.

**`edge_mode` can only move `Single` → `Multi`, never back.** `commit()` rejects a staged
`set_edge_mode(EdgeMode::Single)` once the persisted value is already `Multi`
(`StoreError::SchemaConflict`). This isn't a temporary restriction — there is no supported
path from `Multi` back to `Single`, because doing so would require validating that every
existing `(src, label, dst)` triple still has at most one edge, which this design does not
attempt. A multi-edge graph that needs to become a simple graph again requires an explicit
data migration, not a schema call.

---

### 2. Specifying Ranks in Gremlin Traversal

When adding an edge, the user specifies the rank via the standard `.property("rank", value)` step:

```rust
// Explicit rank definition
g.V(1).addE(KNOWS_LABEL_ID).to(2)
    .property("rank", 5u16)
    .property("weight", 0.5f64)
    .next()?;
```

When building the execution plan, the optimizer/builder:
1. Detects the `"rank"` key inside `PropertyStep` or during folding.
2. Parses the value into a `u16` (`Rank`).
3. Binds it as the structural `rank` in `LogicalStep::AddE` (making it `Some(rank)`).
4. Removes `"rank"` from the edge's property map (since it is structural and stored inside the edge key index, saving space).

---

### 3. Write-Time Constraint Enforcement

When a transaction attempts to insert an edge via `ctx.add_edge(&edge_key)`:
1. The transaction reads `schema.edge_mode` — one graph-wide value, not a per-label lookup
   (today's `self.schema.read().unwrap().edge_config(ek.label_id)` in
   [`src/graph.rs`](../src/graph.rs) becomes `self.schema.read().unwrap().edge_mode`).
2. **If `EdgeMode::Single`**:
   - The engine enforces `rank == DEFAULT_RANK` (0).
   - If the user passes a non-zero rank, the engine returns an error (`StoreError::UnexpectedDataType` or similar).
   - If an edge with `DEFAULT_RANK` already exists, the engine returns `StoreError::DuplicateEdge`.
3. **If `EdgeMode::Multi`**:
   - The edge is written with the specified `rank` (defaulting to `DEFAULT_RANK` if not specified).
   - If another edge is inserted with the same rank (e.g., if the user did not specify a different rank for the second edge), it fails with `StoreError::DuplicateEdge`.

This applies uniformly to every edge label in the graph — there is no per-label override.
