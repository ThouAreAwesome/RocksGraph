# Data Model & Type System

**Target:** RocksGraph v0.2.0+

RocksGraph is built upon the **Labeled Property Graph (LPG)** data model, extended with first-class **dense vector embeddings** for semantic similarity search.

This guide outlines the core graph primitives, identifier semantics, system capacities and constraints, supported data types, and reserved property rules.

---

## 1. The Property Graph Mental Model

A graph in RocksGraph consists of two fundamental topological entities: **Vertices** (nodes) and **Edges** (relationships), both of which can store arbitrarily typed property key-value pairs.

```
  ┌─────────────────────────┐               ┌─────────────────────────┐
  │   Vertex (id=1, person) │               │   Vertex (id=2, person) │
  ├─────────────────────────┤  ──knows──►   ├─────────────────────────┤
  │ name: "Alice"           │   since: 2021 │ name: "Bob"             │
  │ age: 30                 │   weight: 0.9 │ age: 32                 │
  │ emb: [0.95, 0.10, 0.05] │   rank: 0     │ emb: [0.10, 0.90, 0.20] │
  └─────────────────────────┘               └─────────────────────────┘
```

### Vertices (Nodes)
- **Primary Identity (`id`)**: A unique 64-bit signed integer (`i64` in Rust, `int` in Python, range: $-2^{63}$ to $2^{63}-1$). Every vertex is uniquely identified across the entire graph by this integer.
- **Label (`label`)**: A string defining the entity type (e.g. `"person"`, `"product"`, `"document"`).
- **Properties**: A key-value map containing strongly typed attributes (strings, integers, floats, booleans, UUIDs, binary payloads, or dense float vector embeddings).

### Edges (Relationships)
- **Directed Topology**: Every edge is strictly directed from an outgoing source vertex (`out_v`) to an incoming destination vertex (`in_v`). Traversal operations can navigate forward (`out`), backward (`in`), or bi-directionally (`both`).
- **Label (`label`)**: Identifies the relationship type (e.g. `"knows"`, `"purchased"`, `"created"`).
- **Multiplicity & Ranks**:
  - `EdgeMode::Single`: At most one edge of a given label can exist between a specific pair of vertices. Duplicate edge writes return `StoreError::DuplicateEdge` (`IntegrityError` in Python).
  - `EdgeMode::Multi`: Multiple parallel edges of the same label can exist between the same pair of vertices, distinguished by a 16-bit unsigned integer `rank`. Usable explicit rank range is `0` to `65,534` (`0x0000` to `0xFFFE`), default `0`. Rank `65,535` (`0xFFFF` / `u16::MAX`) is a reserved sentinel used internally for auto-assigning incrementing ranks.
  - **One-Way Ratchet**: Once set to `EdgeMode::Multi`, downgrading the database to `EdgeMode::Single` is strictly disallowed and rejected with `StoreError::SchemaConflict` (`SchemaError` in Python).
- **ID Collision Behavior**:
  - Adding a vertex with an existing primary key ID returns `StoreError::DuplicateVertex` (`IntegrityError` in Python).
  - Adding an edge with identical `(src, label, dst)` in `Single` mode, or identical `(src, label, dst, rank)` in `Multi` mode, returns `StoreError::DuplicateEdge` (`IntegrityError` in Python).
- **Canonical Edge Identifier (`id`)**: 
  - Every edge has a unique 30-character URL-safe string identifier (e.g. `"AAAAAAAAAAEAAAADAAAAAAAAAAIAAA"`) that encodes its source vertex, label, destination vertex, and rank.
  - Lookup by edge ID is supported via `g.E(["..."])` and `.hasId("...")`.

### Vector Embeddings
- **Vertex Embeddings**: Dense continuous float vectors (`Value::FloatVector(Vec<f32>)` in Rust, `rocksgraph.Vector` in Python — not a bare `list`) attached directly to vertices as properties. Vertices can be indexed in HNSW for sub-millisecond approximate nearest neighbor (ANN) search.
- **Edge Vector Attachments**: Edge vector properties can be stored as raw vector values. Note: In v0.2, vector indexes only support vertices (`VectorEntityType::Vertex`); edge vector indexing is planned for v0.3.

---

## 2. System Limits & Capacities

RocksGraph defines the following user-facing limits and capacities:

| Dimension | Supported Range / Capacity | Description |
| :--- | :--- | :--- |
| **Vertex ID (`id`)** | $-9,223,372,036,854,775,808$ to $9,223,372,036,854,775,807$ | 64-bit signed integer (`i64` / `int`). |
| **Edge ID (`id`)** | 30-character string | Globally unique identifier encoding edge endpoints, label, and rank. |
| **Edge Rank (`rank`)** | `0` to `65,534` (sentinel: `65,535`) | Usable discriminator range for parallel edges. `65,535` is reserved for auto-assignment. |
| **Total Distinct Labels** | Up to $2,147,483,647$ (~2.1 billion) | Total unique vertex and edge label types database-wide. |
| **Total Property Keys** | Up to $32,767$ | Total unique property keys database-wide across all entity types. |
| **Properties per Element** | Up to $4,095$ properties | Maximum property count per individual vertex or edge record. |
| **Max String Length** | Up to $65,535$ bytes (~64 KB) | Maximum length for any single UTF-8 string property. |
| **Max Binary Payload** | Up to $65,535$ bytes (~64 KB) | Maximum size for any single binary (`bytes`) property. |
| **Vector Dimensions** | Any $D \ge 1$ (typically 128 to 3072) | Number of float dimensions in a vector embedding. |

---

## 3. Supported Data Types

RocksGraph provides a strongly typed property engine (`Value` / `Primitive`) supporting scalars, binary blobs, and dense numeric vectors:

| Data Type | Rust Type | Python Type | Description |
| :--- | :--- | :--- | :--- |
| **Int32** | `i32` | `int` | 32-bit signed integer |
| **Int64** | `i64` | `int` / `rocksgraph.Int64` | 64-bit signed integer |
| **UInt16** | `u16` | `int` | 16-bit unsigned integer (used for edge ranks) |
| **Float32** | `f32` | `float` | 32-bit single-precision floating point |
| **Float64** | `f64` | `float` | 64-bit double-precision floating point |
| **String** | `String` / `&str` / `SmolStr` | `str` | UTF-8 encoded text string ($\le 65,535$ bytes) |
| **Boolean** | `bool` | `bool` | `true` or `false` |
| **Bytes** | `Vec<u8>` / `&[u8]` | `bytes` | Arbitrary binary payload ($\le 65,535$ bytes) |
| **Uuid** | `u128` | `uuid.UUID` | 128-bit Universally Unique Identifier |
| **FloatVector** | `Value::FloatVector(Vec<f32>)` | `rocksgraph.Vector` | Dense continuous float vector for similarity search — writing or reading a bare Python `list` raises `ValueError` |

### Working with Data Types

#### 🦀 Rust
```rust
use rocksgraph::{Graph, StoreError, Value};

fn create_rich_vertex(graph: &Graph) -> Result<(), StoreError> {
    let mut txn = graph.begin();

    txn.g()
        .addV("article")
        .property("id", 101i64)
        .property("title", "Modern Graph Architectures")
        .property("views", 4250i64)
        .property("score", 9.85f64)
        .property("is_published", true)
        .property("raw_payload", vec![0xDE, 0xAD, 0xBE, 0xEF])
        .property("embedding", Value::FloatVector(vec![0.12, 0.45, 0.78, 0.91]))
        .next()?;

    txn.commit()?;
    Ok(())
}
```

#### 🐍 Python
```python
from rocksgraph import Graph, Int64, Vector

def create_rich_vertex(graph: Graph):
    with graph.begin() as txn:
        txn.g().addV("article") \
            .property("id", 101) \
            .property("title", "Modern Graph Architectures") \
            .property("views", Int64(4250)) \
            .property("score", 9.85) \
            .property("is_published", True) \
            .property("raw_payload", b"\xde\xad\xbe\xef") \
            .property("embedding", Vector([0.12, 0.45, 0.78, 0.91])) \
            .next()
```

---

## 4. Reserved Keys & The Disjoint Access Model

RocksGraph treats three names — `"id"`, `"label"`, and `"rank"` — as **reserved structural attributes**:

| Reserved Name | Applicable Entity | Type | Structural Meaning | Dedicated Step |
| :--- | :--- | :--- | :--- | :--- |
| `"id"` | Vertex & Edge | `Int64` (Vertex) / `String` (Edge) | Primary entity identifier | `.id()` / `.hasId(...)` |
| `"label"` | Vertex & Edge | `String` | Entity type discriminator | `.label()` / `.hasLabel(...)` |
| `"rank"` | Edge only | `UInt16` (`u16`) | Multi-edge parallel discriminator | `.rank()` / `.hasRank(...)` |

### The Disjoint Model Rules:
1. **Dedicated Step Access Only**: `"id"`, `"label"`, and `"rank"` cannot be accessed through generic property steps (`.values()`, `.properties()`, `.has()`). They must be accessed via dedicated steps:
   - Use `.id()` / `.hasId(...)` for identifiers.
   - Use `.label()` / `.hasLabel(...)` for labels.
   - Use `.rank()` / `.hasRank(...)` for edge ranks.
2. **Generic Property Access Rejected**: Calling `.values(["id"])`, `.properties(["label"])`, or `.has("rank", 0)` is rejected during query planning.
3. **Write Path Rules**:
   - `addV(label)` and `addE(label)` take the label as an explicit parameter.
   - `.property("id", N)` and `.property("rank", R)` immediately following `addV` or `addE` set the structural ID and rank. Setting `"label"` as a regular property is invalid.

---

## 5. Property Retrieval & the `withProperties()` Hint

By default, RocksGraph optimizes query latency and I/O bandwidth by returning **only structural identifiers (`id` and `label`)** with zero property I/O overhead:

#### 🦀 Rust
```rust
// Default: Returns Vertex with id and label; properties map is empty (0 property I/O reads)
let vertices = snap.g().V([]).to_list()?;

// Fetch ALL properties explicitly ([] = all convention)
let vertices = snap.g().withProperties([]).V([]).to_list()?;

// Fetch ONLY specific named properties
let vertices = snap.g().withProperties(["name", "age"]).V([]).to_list()?;
```

#### 🐍 Python
`withProperties()` takes variadic arguments in Python, not a list — a bare call means "all", not `withProperties([])`:
```python
# Default: id and label only
vertices = snap.g().V().to_list()

# Fetch ALL properties: call withProperties() with no arguments
vertices = snap.g().withProperties().V().to_list()

# Fetch ONLY specific named properties: pass keys as separate arguments, not a list
vertices = snap.g().withProperties("name", "age").V().to_list()
```

### Why Defaulting to No Properties Matters:
In graph traversals (e.g. `g.V(1).out("knows").out("knows").count()`), loading full property records for every intermediate vertex would incur unnecessary I/O and memory overhead — that's why you must opt in with `withProperties()` when you actually need the data.

---

## 6. Data Modeling Best Practices

### Pattern 1: Dense Contiguous Integer Primary Keys
Assign dense, sequential 64-bit integer IDs (`i64`) to vertices. Sequential IDs maximize storage cache efficiency and optimize edge index traversal.

### Pattern 2: Model Relationships as First-Class Edges
Whenever you need to traverse, filter, or hop between entities, represent the relationship as a directed graph **Edge** with a meaningful label rather than embedding foreign ID arrays in a JSON property.

### Pattern 3: Choose Edge Multiplicity Intentionally
- Use **`EdgeMode::Single`** for simple relational graphs (e.g., `knows`, `belongs_to`) to prevent accidental duplicate edges and save storage.
- Use **`EdgeMode::Multi`** only when multiple parallel edges of the same label between the same pair of nodes are required, using `rank` (0 to 65,534) as the discriminator. Note that enabling Multi mode is an irreversible one-way operation.

---

## 7. Data Modeling Anti-Patterns

### ❌ Anti-Pattern 1: Embedding Foreign Key Lists in String / Binary Properties
Storing related IDs in a delimited string or binary payload prevents index-accelerated traversal.

### ❌ Anti-Pattern 2: Sparse Random Hashing for Vertex IDs
Hashing strings to arbitrary 64-bit random values (e.g. `hash(uuid)`) scatters keys uniformly across the entire integer space, reducing index locality and storage compression efficiency.

**What to do instead**: derive the `i64` primary key from a dense, sequential allocator your application controls (e.g. an auto-increment counter, or a per-shard counter if writes are distributed). If you have multiple concurrent writers without a shared counter, prefer a coarsely-monotonic scheme (e.g. timestamp-prefixed IDs) over a pure hash — it's still not perfectly sequential, but preserves far more locality than scattering across the full 64-bit space.

> [!WARNING]
> **RocksGraph has no secondary property index.** Storing a natural key (UUID, username) as a regular property and looking it up with `.has("external_id", ...)` is *not* an indexed lookup — it's a full scan of every vertex the traversal reaches, same as any other `.has()` call. Only `.hasId()` is a point lookup. If you need fast lookup by a natural key, either maintain the natural-key → `i64` mapping yourself outside RocksGraph (e.g. an in-process map or a small side store you load at startup), or, where the natural key already has a numeric/time-ordered component (a Snowflake ID, a ULID, an existing auto-increment ID from another system), derive the `i64` primary key directly from that component so no separate mapping is needed.

---

## Related Topics

- [Getting Started](getting_started.md) — 5-minute practical onboarding.
- [Schema Management](schema_management.md) — Declaring labels, property types, and strict validation.
- [Gremlin Step Reference](step_reference.md) — Step catalog for navigating and querying graph entities.
- [Performance Tuning](performance.md) — Optimization rules and memory sizing.
