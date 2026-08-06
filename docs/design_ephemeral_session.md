# Design: EphemeralSession — Disposable In-Memory Overlay for Graph Computations

Status: proposed
Created: 2026-08-06

---

## 1. Problem Statement

RocksGraph currently provides two session types:

- `ReadSession` — snapshot reads, no mutations
- `TxnSession` — transactional reads + mutations, with commit/rollback and OCC conflict detection

Neither supports a pattern often needed for graph computation: **temporary, disposable mutations used as intermediate state within a single session.** Examples:

- Connected components: label propagation writes component IDs as temporary vertex properties
- Bidirectional Dijkstra: stores forward/backward distance frontiers as temporary properties
- Subgraph extraction: builds an in-memory subgraph for local analysis, then discards it
- Iterative ranking: stores per-vertex scores that are overwritten each iteration

Using `TxnSession` is wrong: each mutation writes to RocksDB, each iteration commits, and the intermediate state persists until explicitly deleted. Using plain `HashMap` structures loses the traversal API, property model, and integration with the Gremlin query engine.

**We need a third session type that combines write capability with disposable semantics — an ephemeral scratch pad.**

> **Scope note:** This session is designed for Gremlin-driven subgraph manipulation, what-if
> scenarios, and moderate-scale iterative algorithms. It is NOT intended as a replacement for
> native column-oriented / CSR-based graph analytics engines (PageRank over 100M vertices, etc.).
> For high-performance batch OLAP, a future `GraphComputer` abstraction using dedicated data
> structures would be more appropriate. EphemeralSession provides the Gremlin ergonomics; it
> does not match the throughput of bulk array operations.

---

## 2. Design: EphemeralSession / EphemeralGraph

### 2.1 API Surface

```rust
graph.read()      → ReadSession        (snapshot reads, no writes)
graph.begin()     → TxnSession         (transactions, commit/rollback)
graph.ephemeral() → EphemeralSession   (disposable overlay, drop on close, no commit)
```

> **Naming rationale:** `graph.ephemeral()` explicitly signals that mutations are not durable.
> `graph.write()` was rejected because it suggests persistent writes in database ergonomics
> ("read/write" implies durability). Alternatives considered: `scratch()`, `sandbox()`, `overlay()`.

### 2.2 Semantics

| Property | ReadSession | TxnSession | EphemeralSession |
|----------|-------------|-----------|------------------|
| Read source | Snapshot (frozen at open) | Snapshot (frozen at begin) | Snapshot (frozen at open) |
| Reads consistent? | ✅ Same snapshot throughout | ✅ Same snapshot throughout | ✅ Same snapshot throughout |
| Mutations allowed? | ❌ | ✅ | ✅ |
| Mutations durable? | N/A | ✅ WAL + OCC | ❌ HashMap only; discarded on drop |
| `commit()` | N/A | ✅ | ❌ |
| `drop()` | Releases snapshot | Rollback if not committed | Discards all overlay state |
| Schema mutations? | ❌ | ✅ (staged, committed atomically) | ⚠️ Auto-registered in ephemeral schema (see §3.3) |
| Vector index writes? | ❌ | ✅ (WAL + in-memory) | ⚠️ Overlay only (see §5.2) |
| Gremlin steps | Read-only | Read + Write | Read + Write (same as TxnSession) |
| Concurrent safety | Lock-free reads | OCC conflict detection | Single-threaded |

### 2.3 Usage Example

```rust
fn connected_components(graph: &Graph) -> HashMap<VertexKey, VertexKey> {
    let mut scratch = graph.ephemeral();

    // Phase 1: Initialize — copy vertices, set temporary "component" property.
    // "component" is not declared in the persistent schema; it auto-registers
    // in the session's ephemeral schema on first write.
    for v in graph.read().g().V([]).id().to_list()? {
        scratch.g().addV(v.label()).property("id", v.id())
            .property("component", v.id()).next()?;
    }

    // Phase 2: Label propagation (iterative, mutation-heavy)
    loop {
        let mut changed = false;
        for v in graph.read().g().V([]).to_list()? {
            let mut min_component = scratch.g().V(v.id()).values("component").next()?;
            for neighbor in scratch.g().V(v.id()).out([]).id().to_list()? {
                let nc = scratch.g().V(neighbor).values("component").next()?;
                min_component = min_component.min(nc);
            }
            let my = scratch.g().V(v.id()).values("component").next()?;
            if min_component < my {
                scratch.g().V(v.id()).property("component", min_component).next()?;
                changed = true;
            }
        }
        if !changed { break; }
    }

    // Phase 3: Extract result, discard ephemeral session
    let mut result = HashMap::new();
    for v in scratch.g().V([]).id().to_list()? {
        let comp = scratch.g().V(v).values("component").next()?;
        result.insert(v, comp);
    }
    // scratch dropped here — all mutations + ephemeral schema discarded
    result
}
```

> **Performance caveat:** The example above iterates all vertices via Gremlin traversal pipelines
> for readability. For large graphs, batch vertex iteration via `scan_vertices` (available through
> the `GraphCtx` trait) is significantly faster than building a traversal per vertex.

### 2.4 Python API

```python
scratch = graph.ephemeral()
scratch.g().addV("person").property("id", 1).property("component", 1).next()
# ... algorithm logic ...
# scratch out of scope — all mutations discarded
```

---

## 3. Internal Implementation: EphemeralGraph

The internal naming mirrors the existing convention: `ReadSession` wraps `LogicalSnapshot`,
`TxnSession` wraps `LogicalGraph`, and `EphemeralSession` wraps `EphemeralGraph`.

### 3.1 Struct

```rust
pub(crate) struct EphemeralGraph {
    // ── Read path (identical to LogicalSnapshot) ──
    store: Snapshot,                                    // frozen point-in-time
    schema: Arc<RwLock<Schema>>,
    schema_cache: TxnSchemaCache,
    vector_indexes: Arc<RwLock<VectorIndexMap>>,
    execution_options: ExecutionOptions,

    // ── Overlay caches (identical to LogicalGraph) ──
    vertices: HashMap<VertexKey, Vertex>,
    edges: HashMap<CanonicalEdgeKey, Edge>,
    vertex_degree: HashMap<VertexKey, (u32, u32, LabelId)>,

    // ── Dirty tracking (identical to LogicalGraph) ──
    dirty: HashMap<CanonicalKey, Existence>,

    // ── Ephemeral schema (EphemeralSession-specific) ──
    /// Session-local property-key registry.  Any unknown property key written
    /// via `.property()` or `set_property()` is **automatically registered**
    /// here (like auto-schema mode for labels).  No explicit
    /// `add_property_key()` call is required.
    ///
    /// Separated from the global Schema so algorithm-internal properties
    /// (e.g. "component", "distance", "visited") never leak into persistent
    /// storage and never collide with committed property-key IDs.
    ephemeral_schema: HashMap<SmolStr, u16>,
    next_ephemeral_id: u16,                             // monotonic allocator

    // ── Adjacency index: delta-only overlay edges ──
    /// Contains ONLY edges added within this EphemeralSession.  Committed
    /// edges are read from the Snapshot on each `get_adjacent_edges` call.
    out_adj: HashMap<VertexKey, Vec<CanonicalEdgeKey>>,
    in_adj: HashMap<VertexKey, Vec<CanonicalEdgeKey>>,
}
```

### 3.2 Key differences from LogicalGraph

| Aspect | LogicalGraph | EphemeralGraph |
|--------|-------------|------------|
| Store type | `Transaction` (read + write) | `Snapshot` (read-only) |
| Schema | `staged_schema: StagedSchema` (explicit DDL, atomic commit) | `ephemeral_schema: HashMap` (auto-register on write, session-local) |
| Property key resolution | Global schema only | Auto-register unknown keys in ephemeral schema; resolve ephemeral first, fall through to global |
| Vector WAL | `vector_pending_ops` — flushed on commit | None — vector writes are overlay-only |
| `commit()` / `rollback()` | ✅ | ❌ |
| `drop_vertex` behavior | Rejects if incident edges exist | Rejects if incident edges exist (parity with LogicalGraph) |
| Adjacency index | None (linear overlay scan) | Delta-only `out_adj`/`in_adj` for overlay edges |
| Cache clearing across `g()` calls | `clear_caches()` resets step-level caches; mutation state preserved | Same — step caches cleared, overlay state + ephemeral schema preserved |

### 3.3 Ephemeral schema: auto-registration on write

**Problem:** EphemeralSession algorithms write temporary properties like `"component"`, `"distance"`, `"visited"`. These property keys are NOT declared in the persistent schema — the user never called `add_property_key("component", DataType::Int64)`.

**Solution:** Follow the same auto-registration semantics as auto-schema mode for labels. When a property write encounters an unknown key:

```
1. Look up in ephemeral_schema → if found, use ephemeral ID
2. Look up in global schema_cache     → if found, use committed ID
3. Not found → auto-register: allocate next_ephemeral_id, insert into ephemeral_schema, use new ID
```

Key properties:

- **No explicit `add_property_key()` required.** The user writes `scratch.g().V(1).property("component", 5)` and `"component"` is automatically registered. This mirrors how `TxnSession` in auto-schema mode auto-creates vertex labels on first `addV("newLabel")`.
- **Ephemeral IDs allocated from the upper half of the `u16` range** (`32768..=65535`). Committed property-key IDs are capped at `MAX_PROP_KEYS = (1 << 15) - 1 = 32767` (see `schema/definition.rs`), so the upper half is permanently free and collision is structurally impossible regardless of how many keys the persistent schema accumulates.
- **Strict mode:** The global schema enforces declared vertex/edge labels. Property keys written in EphemeralSession are auto-registered in the ephemeral schema, not the persistent schema — so Strict mode's label enforcement is unaffected.
- **On drop:** `ephemeral_schema` is dropped — zero impact on the persistent database. No RocksDB writes, no schema version bumps.

This is functionally identical to how `TxnSession`'s auto-schema mode handles unknown vertex labels: transparent auto-registration within session scope, discarded if not committed.

### 3.4 Adjacency index: delta-only design

The adjacency index tracks **only overlay edges** (those added via `add_edge` within this session).
Committed edges are read from the Snapshot on each call — they are NOT cached in the index.

This avoids two correctness issues:
- **Pagination:** If `get_adjacent_edges` uses a `limit`, a partial read of committed edges would
  leave the index incomplete. Delta-only avoids this — the Snapshot is re-queried each time.
- **Tombstone tracking:** If a committed edge is later dropped via `drop_edge`, the tombstone is
  stored in `dirty` and filtered during the Snapshot merge phase.

```rust
fn get_adjacent_edges(&mut self, vertex: VertexKey, direction: Direction, ...) {
    // Phase 1: Committed edges from Snapshot
    let (committed, cursor) = self.store.get_adjacent_edges(vertex, direction, ...)?;
    let mut result = filter_committed(committed, &self.dirty);  // remove tombstoned

    // Phase 2: Overlay edges from delta index
    let overlay_keys = match direction {
        Direction::OUT => self.out_adj.get(&vertex).cloned().unwrap_or_default(),
        Direction::IN  => self.in_adj.get(&vertex).cloned().unwrap_or_default(),
        Direction::BOTH => { /* concat + dedup */ }
    };
    for cek in overlay_keys {
        if self.dirty.get(&CanonicalKey::Edge(cek)) != Some(&Existence::Tombstone) {
            result.push(edge_key_for_direction(&self.edges[&cek], direction));
        }
    }

    Ok((result, cursor))
}
```

### 3.5 Code Reuse

| Component | Source | Lines |
|-----------|--------|-------|
| `GraphCtx` impl (read methods) | Copied from `LogicalSnapshot`, with dirty-map + ephemeral-schema checks | ~100 |
| Mutation methods | Copied from `LogicalGraph`, stripped of RocksDB writes | ~80 |
| `get_adjacent_edges` | New: Snapshot read + delta-only overlay merge | ~40 |
| Ephemeral schema + auto-registration | New: `HashMap<SmolStr, u16>` + auto-register-on-write logic | ~25 |
| Adjacency index maintenance | New: `add_edge`/`drop_edge` update `out_adj`/`in_adj` | ~30 |
| Traversal integration | Zero changes — `EphemeralSession::g()` returns `WriteTraversal<'_>` via `&mut dyn GraphCtx`, identical to `TxnSession::g()` | 0 |
| **Total new code** | | **~275 lines** |

---

## 4. Performance Analysis

### 4.1 Time Complexity

| Operation | LogicalGraph (current) | EphemeralGraph (with delta index) |
|-----------|----------------------|------------------------------|
| `add_vertex` | O(1) | O(1) |
| `add_edge` | O(1) | O(1) × 3 (edges + out_adj + in_adj) |
| `get_vertex` | O(1) | O(1) + O(1) dirty check |
| `get_adjacent_edges` | O(committed) + O(\|all overlay edges\|) | O(committed from Snapshot) + O(overlay edges incident to vertex) |
| `set_property` | O(1) | O(1) + auto-register in ephemeral_schema if new key (amortized O(1)) |
| `drop_vertex` | O(1) dirty flag (rejects if incident edges exist) | O(1) dirty flag (rejects if incident edges exist — parity with LogicalGraph) |
| `drop_edge` | O(1) dirty flag | O(degree_overlay) for Vec::retain in out_adj/in_adj |

**Key difference:** `get_adjacent_edges` changes from O(total overlay edges) to O(incident overlay edges). For a session with 1000 temporary edges across 100 vertices (avg degree 10), this is a 100× speedup per adjacency query.

### 4.2 Space Complexity

| Component | Per-edge overhead | Example: 1000 edges |
|-----------|-------------------|---------------------|
| `edges: HashMap<CEK, Edge>` | ~100 bytes/edge | ~100 KB |
| `out_adj: HashMap<VK, Vec<CEK>>` | 24 bytes/edge | ~24 KB |
| `in_adj: HashMap<VK, Vec<CEK>>` | 24 bytes/edge | ~24 KB |
| `ephemeral_schema: HashMap<SmolStr, u16>` | ~30 bytes/key | ~120 bytes (4 keys) |
| **Total overhead vs LogicalGraph** | **~48 bytes/edge** | **~48 KB + 120 bytes** |

For a session with 10K temporary edges: ~480 KB overhead. Negligible.

### 4.3 RocksDB I/O

| Scenario | RocksDB reads |
|----------|--------------|
| Vertex queried, unmodified | 1 Snapshot read per first access |
| Vertex tombstoned | 0 reads (dirty flag short-circuit) |
| Vertex added in overlay (New) | 0 reads (no committed data exists) |
| Committed edges queried | 1 Snapshot read per `get_adjacent_edges` call |
| Overlay edges queried | 0 reads (from delta index) |

The snapshot is re-queried on each `get_adjacent_edges` for committed edges. For algorithms
that query the same vertex's adjacency repeatedly within a tight loop, the Snapshot's
internal RocksDB block cache provides automatic caching at the storage layer.

---

## 5. Design Decisions

### 5.1 Decided

| Decision | Rationale |
|----------|-----------|
| Session name: `graph.ephemeral()` | "Ephemeral" explicitly signals non-durability; avoids `write()` confusion |
| Internal name: `EphemeralGraph` | Mirrors the existing `LogicalGraph` / `LogicalSnapshot` convention; symmetric with `EphemeralSession` |
| Auto-registration of temporary properties | Follows auto-schema semantics — user never calls `add_property_key()` for algorithm-internal keys |
| Ephemeral ID range: `32768..=65535` | Committed IDs are capped at `MAX_PROP_KEYS = 32767` (15-bit), so the upper half of `u16` is permanently free — collision is structurally impossible |
| Snapshot capture at `ephemeral()` call | Consistent with `ReadSession` — frozen point-in-time |
| Delta-only adjacency index | Avoids pagination correctness bugs; keeps index memory bounded to overlay size |
| `EphemeralGraph` implements `GraphCtx` | Reuses `WriteTraversal<'_>` unchanged via dynamic dispatch — zero integration code |
| No `commit()`, no `rollback()`, no promotion to `TxnSession` | By design — mutations are always disposable. The "what-if → commit" pattern is handled by extracting results from the ephemeral session and writing them via a new `TxnSession`; adding a promotion path would break the disposable invariant and require `EphemeralGraph` to hold a reference back to the persistent store |
| `drop_vertex` parity with LogicalGraph | Rejects if incident edges exist (no cascade-delete) |
| Single-threaded only (v1) | No concurrent access needed for ephemeral sessions |

### 5.2 To Be Determined

| Open question | Options | Tradeoff |
|--------------|---------|----------|
| **Vector search in EphemeralSession** | Support `nearest()`/`similarity()` on snapshot vectors only, or merge ephemeral-added vectors? | Snapshot-only is simpler; merge requires overlay RYOW for vector writes |
| **Adjacency index back-port to LogicalGraph** | Yes (separate PR) or No | Yes: benefits TxnSession too. No: changes commit path, higher risk |
| **Python context manager** | `with graph.ephemeral() as s:` or manual `.close()` | Context manager matches TxnSession ergonomics |
| **Future `GraphComputer`** | Use EphemeralGraph internally, or use dedicated CSR/array structures | EphemeralGraph simpler to implement; CSR faster for bulk analytics |

---

## 6. Implementation Plan

| Phase | Scope | Est. lines | Risk |
|-------|-------|------------|------|
| 1 | `EphemeralGraph` struct + `GraphCtx` impl + ephemeral schema with auto-registration | ~225 | Low — additive |
| 2 | Delta-only adjacency index | ~30 | Low — encapsulated in EphemeralGraph |
| 3 | `graph.ephemeral()` → `EphemeralSession` public API | ~15 | Low — follows existing session pattern |
| 4 | Integration tests (connected components, subgraph extraction) | ~80 | None |
| 5 | Python bindings (`graph.ephemeral()` → `EphemeralSession`) | ~30 | Low |
| 6 | (future) `GraphComputer` + `VertexProgram` trait | ~500 | Medium |
