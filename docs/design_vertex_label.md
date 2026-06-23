# Design: Vertex Label in Storage Key

## Problem

Vertex label currently lives in the RocksDB **value**, not the key. This has two costs:

1. `hasLabel()` requires a value read per vertex — no key-level filtering
2. At materialization, `label_id` is only available after `get_all_props()`, so
   `GValue::Vertex` cannot carry the label in-pipeline without an extra point-read

Edge label by contrast is part of the composite key `(src, edge_label, dst, rank)` —
always free in the pipeline. Vertex label should be treated consistently.

## How Mainstream Graph Databases Handle Vertex and Edge Labels

### Edge label — universally structural

Every major graph database treats edge label as part of the edge's composite identity.
Without it you cannot distinguish `(Alice, KNOWS, Bob)` from `(Alice, MANAGES, Bob)`.

| Database | Edge key |
|---|---|
| Neo4j | `(src_id, relationship_type, dst_id)` — type is non-optional |
| TinkerPop / Gremlin | `(src, label, dst, rank)` — same as RocksGraph today |
| TigerGraph | `(edge_type, src_type+id, dst_type+id)` |
| NebulaGraph | `(src_vid, edge_type, rank, dst_vid)` |
| HugeGraph | `(src_label+id, edge_label, dst_label+id)` |
| ArangoDB | edge lives in a named collection — collection name is the type |

### Vertex label — much more varied

| Database | Label in storage key? | Multiple labels? | Required? | Notes |
|---|---|---|---|---|
| Neo4j | No — node ID is the key | Yes, any number | No | Label is optional metadata; add/remove after creation |
| TinkerPop standard | No — user ID is the key | No — exactly one | Has default `"vertex"` | `g.V(id)` needs no label |
| NebulaGraph | No — VID is the key | Multiple "tags" | No | Tags are independent of vertex identity |
| JanusGraph | No — vertex ID is the key | No | Yes | Label stored in vertex value |
| TigerGraph | Yes — `vertex_type + primary_key` | No | Required | Reuse of IDs across types is allowed |
| HugeGraph | Yes — `label_id + vertex_id` | No | Required | Enables prefix scan by type; neighbor label stored in edge record |
| ArangoDB | Yes — vertex lives in a named collection | No | Required | Collection + `_key` = full identity |

### Key takeaway

Edge label is **structurally load-bearing** in all systems — removing it would break
multi-relation graphs. Vertex label is **semantically useful but not structurally
necessary**: the vertex ID alone is globally unique. The design choice is whether to
pay the storage cost of embedding label in the key in exchange for free in-pipeline
access and faster `hasLabel()` scans.

RocksGraph currently matches Neo4j / TinkerPop / JanusGraph: vertex ID is the key,
label lives in the value. Option B below would move it toward HugeGraph / TigerGraph /
ArangoDB, trading a schema migration for consistent, zero-cost label access.

## Options

### Option A — Keep label in value, make it optional at API boundary

- Change `Value::Vertex.label` (decoded string, formerly numeric `label_id`) to `Option<SmolStr>`
- `None` when `PropHint::None` is used (see `design_with_properties.md`)
- Minimal store change; `hasLabel()` stays a value-side filter
- Best as a short-term step while Option B is planned

### Option B — Promote label to storage key (HugeGraph approach)

RocksDB vertex key becomes `label_id (u16) + vertex_id (i64)` instead of `vertex_id` alone.

**Consequences:**
- `VertexKey` changes from `type VertexKey = i64` to a struct `{ label_id: u16, id: i64 }`
- `hasLabel()` becomes a native RocksDB prefix scan — no value read needed
- Adjacency edge records must store the neighbor's `(label_id + vertex_id)` so that
  `VertexKey` is fully populated after an `out()`/`in()` scan without extra reads
- `GValue::Vertex(VertexKey)` carries `label_id` for free throughout the pipeline

**Store schema changes required:**
- Vertex CF key: `label_id (2B) + vertex_id (8B)` instead of `vertex_id (8B)`
- Edge adjacency records: store `(src_label, src_id, edge_label, dst_label, dst_id, rank)` in both `edges_out` and `edges_in` CFs

**Migration:** existing data must be re-encoded; not backwards compatible.

### Option C — Drop vertex labels entirely

Treat label as a regular user property (`has("type", "Person")`).
- Simplest — removes `label_id` from `Vertex`, `VertexKey`, and step implementations
- Loses semantic distinction and future storage optimization
- Not recommended unless vertex-type queries are unimportant in the target workload

## Recommendation

Do **Option A** now as part of the `withProperties()` work (low effort, unblocks that feature).
Plan **Option B** as a follow-on store refactor when `hasLabel()` query performance becomes a bottleneck.

## Affected Files

- `src/types/keys.rs` — `VertexKey` type
- `src/types/gvalue.rs` — `GValue::Vertex` variant
- `src/gremlin/value.rs` — `Value::Vertex` / `Vertex` struct
- `src/gremlin/traversal.rs` — `materialize()`
- `src/store/rocks/encoding.rs` — vertex and edge CF key encoding
- `src/store/rocks/snapshot.rs`, `transaction.rs` — all vertex/edge read/write paths
- `src/engine/context.rs` — `GraphCtx` trait, `get_adjacent_vertices`
- `src/graph.rs` — `LogicalGraph` / `LogicalSnapshot`
