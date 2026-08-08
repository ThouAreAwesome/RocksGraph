# Schema Management in RocksGraph

**Target:** RocksGraph v0.2.0+

RocksGraph supports flexible schema evolution through two operational modes: **Auto Mode** (dynamic schema inference) and **Strict Mode** (explicit schema declaration and strict enforcement).

This guide covers schema declaration, system limits, atomic schema updates using `SchemaSession`, property key types, label definitions, and runtime vector index management.

> [!NOTE]
> Snippets below are excerpts, not full programs — they assume `graph`/`schema` are already open as shown in [Getting Started](getting_started.md), and that relevant enums (`DataType`, `SchemaMode`, `EdgeMode`, `VectorEntityType`, `DistanceMetric`, `AnnAlgorithm`, `Quantization`) are imported from `rocksgraph` where used.

---

## 1. Auto Mode vs. Strict Mode

| Feature | Auto Mode (Default) | Strict Mode |
| :--- | :--- | :--- |
| **New Labels** | Auto-registered on first insert | Rejected if undeclared (`StoreError::SchemaViolation`) |
| **New Property Keys** | Auto-registered; type inferred from first value | Rejected if undeclared (`StoreError::SchemaViolation`) |
| **Concurrent First-Time Writes** | Can trigger OCC schema catalog conflicts | Zero catalog contention; parallel writes succeed |
| **Type Inconsistency** | Disallowed (type mismatch errors out) | Disallowed (type mismatch errors out) |
| **Reserved Names** | `"id"`, `"label"`, `"rank"` reserved | `"id"`, `"label"`, `"rank"` reserved |
| **Best For** | Rapid prototyping, exploratory graphs | High-concurrency production deployments, ETL |

---

## 2. System Schema Limits

RocksGraph defines the following user-facing schema capacities:

| Schema Dimension | Limit / Range | Description |
| :--- | :--- | :--- |
| **Max Distinct Labels** | Up to $2,147,483,647$ (~2.1 billion) | Total combined vertex and edge labels database-wide. |
| **Max Distinct Property Keys** | Up to $32,767$ | Total distinct property keys database-wide across all entity types. |
| **Properties per Element** | Up to $4,095$ | Maximum property count per individual vertex or edge record. |
| **Rank Range** | `0` to `65,534` (sentinel: `65,535`) | Usable discriminator range for parallel edges in `EdgeMode::Multi`. |

---

## 3. Supported Data Types in Schema

When declaring property keys in `SchemaSession`, specify one of the following `DataType` variants:

| `DataType` Enum | Rust Representation | Python Equivalent | Description |
| :--- | :--- | :--- | :--- |
| `DataType::Bool` | `bool` | `DataType.Bool` | Boolean (`true` or `false`) |
| `DataType::Int32` | `i32` | `DataType.Int32` | 32-bit signed integer |
| `DataType::Int64` | `i64` | `DataType.Int64` | 64-bit signed integer |
| `DataType::UInt16` | `u16` | `DataType.UInt16` | 16-bit unsigned integer (edge ranks) |
| `DataType::Float32` | `f32` | `DataType.Float32` | 32-bit single-precision floating point |
| `DataType::Float64` | `f64` | `DataType.Float64` | 64-bit double-precision floating point |
| `DataType::String` | `String` / `SmolStr` | `DataType.String` | UTF-8 text string ($\le 65,535$ bytes) |
| `DataType::Bytes` | `Vec<u8>` | `DataType.Bytes` | Raw binary payload ($\le 65,535$ bytes) |
| `DataType::Uuid` | `u128` | `DataType.Uuid` | 128-bit UUID identifier |
| `DataType::FloatVector` | `Vec<f32>` | `DataType.FloatVector` | Dense float vector for vector search |

> [!NOTE]
> `DataType::Null` represents the absence of a value or an unset property and **cannot** be registered as the declared data type of a property key.

---

## 4. Declaring Schema with `SchemaSession`

Schema modifications execute atomically via `SchemaSession`. When modifying schema in code:

#### 🦀 Rust
```rust
use rocksgraph::{
    schema::{DataType, EdgeMode, SchemaMode},
    Graph, StoreError,
};

fn configure_strict_schema(graph: &Graph) -> Result<(), StoreError> {
    let mut schema = graph.open_schema();

    // 1. Enable Strict Mode
    schema.set_schema_mode(SchemaMode::Strict);

    // 2. Configure Edge Multiplicity across the database
    schema.set_edge_mode(EdgeMode::Single);

    // 3. Declare Vertex Labels
    schema.add_vertex_label("person");
    schema.add_vertex_label("organization");

    // 4. Declare Edge Labels
    schema.add_edge_label("knows");
    schema.add_edge_label("works_for");

    // 5. Declare Global Property Keys with Data Types
    schema.add_property_key("name", DataType::String);
    schema.add_property_key("age", DataType::Int32);
    schema.add_property_key("salary", DataType::Float64);
    schema.add_property_key("embedding", DataType::FloatVector);

    // 6. Commit schema changes atomically
    schema.commit()?;

    Ok(())
}
```

#### 🐍 Python
```python
from rocksgraph import Graph, DataType, SchemaMode, EdgeMode

def configure_strict_schema(graph: Graph):
    with graph.open_schema() as schema:
        # 1. Enable Strict Mode
        schema.set_schema_mode(SchemaMode.Strict)

        # 2. Configure Edge Multiplicity
        schema.set_edge_mode(EdgeMode.Single)

        # 3. Declare Vertex Labels
        schema.add_vertex_label("person")
        schema.add_vertex_label("organization")

        # 4. Declare Edge Labels
        schema.add_edge_label("knows")
        schema.add_edge_label("works_for")

        # 5. Declare Global Property Keys with Data Types
        schema.add_property_key("name", DataType.String)
        schema.add_property_key("age", DataType.Int32)
        schema.add_property_key("salary", DataType.Float64)
        schema.add_property_key("embedding", DataType.FloatVector)
```

---

## 5. Schema Design Rules & Principles

RocksGraph's schema system adheres to five core rules:

1. **Global Property Keys**: A property key (e.g., `"name"`) has a single, global type shared by all vertex and edge labels across the database. Setting `"name"` as `String` on a vertex prevents using `"name"` as `Int32` on an edge.
2. **Reserved Names**: `"id"`, `"label"`, and `"rank"` are reserved system names and cannot be used as custom property keys.
3. **Graph-Wide Edge Multiplicity & One-Way Ratchet**: Edge multiplicity (`Single` or `Multi`) is set at the graph level via `set_edge_mode`. In `Multi` mode, parallel edges between the same source and destination with the same label are disambiguated using `rank` (0 to 65,534). **Crucially, transitioning from `Multi` back to `Single` is an illegal downgrade** and is rejected with `StoreError::SchemaConflict` (`SchemaError` in Python).
4. **Unconstrained Connections**: In RocksGraph, any edge label can connect any vertex label (e.g., `"works_for"` can connect `person -> organization` or `person -> person`).
5. **Vector Index Entity Scope**: In v0.2, vector indexes only support vertices (`VectorEntityType::Vertex`). Edge vector indexing is deferred to v0.3; passing `VectorEntityType::Edge` to `add_vector_index` (or `drop_vector_index`) returns `StoreError::UnsupportedOperation("edge vector indexes are not yet supported (v0.3)")`.

---

## 6. Declaring Vector Indexes

Vector indexes can be declared during schema initialization or added to an existing graph at runtime (currently supported on vertex properties):

#### 🦀 Rust
```rust
use rocksgraph::{
    schema::{
        AnnAlgorithm, DistanceMetric, HnswConfig, Quantization,
        SchemaSession, VectorEntityType, VectorIndexConfig,
    },
    Graph, StoreError,
};

fn declare_vector_indexes(graph: &Graph) -> Result<(), StoreError> {
    let mut schema = graph.open_schema();

    // Configure HNSW algorithm parameters
    let hnsw = AnnAlgorithm::Hnsw(
        HnswConfig::default()
            .with_m(16)
            .with_ef_construction(200)
            .with_ef_search(50),
    );

    // Create Index Config
    let config = VectorIndexConfig::new(
        "bio_embedding",
        VectorEntityType::Vertex,
        384, // dimension
        DistanceMetric::Cosine,
        hnsw,
    ).with_quantization(Quantization::F16);

    schema.add_vector_index(config);
    schema.commit()?;

    Ok(())
}
```

#### 🐍 Python
```python
from rocksgraph import Graph, VectorEntityType, DistanceMetric, AnnAlgorithm, Quantization

def declare_vector_indexes(graph: Graph):
    with graph.open_schema() as schema:
        # Add 384-dimensional cosine HNSW index with F16 quantization
        schema.add_vector_index(
            entity_type=VectorEntityType.Vertex,
            property="bio_embedding",
            dimension=384,
            metric=DistanceMetric.Cosine,
            algorithm=AnnAlgorithm.Hnsw,
            m=16,
            ef_construction=200,
            ef_search=50,
            quantization=Quantization.F16,
        )
```

---

## 7. Dropping Vector Indexes

To remove a vector index and reclaim memory:

#### 🦀 Rust
```rust
let mut schema = graph.open_schema();
schema.drop_vector_index(VectorEntityType::Vertex, "bio_embedding");
schema.commit()?;
```

#### 🐍 Python
```python
from rocksgraph import VectorEntityType

with graph.open_schema() as schema:
    schema.drop_vector_index(VectorEntityType.Vertex, "bio_embedding")
```

---

## 8. Schema Error Handling

When using `Strict` mode, operations attempting to insert undeclared properties or mismatched data types will trigger errors:

#### 🦀 Rust
```rust
let mut txn = graph.begin();
let result = txn.g()
    .addV("unknown_label")
    .property("id", 1i64)
    .next();

match result {
    Err(StoreError::SchemaViolation(msg)) => {
        println!("Rejected undeclared label: {}", msg);
    }
    _ => {}
}
```

#### 🐍 Python
```python
from rocksgraph import SchemaError

try:
    with graph.begin() as txn:
        txn.g().addV("unknown_label").property("id", 1).next()
except SchemaError as e:
    print(f"Rejected undeclared label: {e}")
```

---

## 9. Schema Best Practices

### Pattern 1: Enforce Strict Mode in Production
Enable `SchemaMode::Strict` in production pipelines to catch misspelled property keys (e.g. `"frist_name"` vs `"first_name"`) and type mismatches immediately at write time.

### Pattern 2: Atomic Schema Migrations
Group all label additions, property declarations, and index configurations into a single `SchemaSession` block and commit once.

```python
# ✅ BEST PRACTICE: Single atomic DDL transaction
with graph.open_schema() as schema:
    schema.set_schema_mode(SchemaMode.Strict)
    schema.add_vertex_label("user")
    schema.add_property_key("email", DataType.String)
    schema.add_property_key("signup_timestamp", DataType.Int64)
```

---

## 10. Schema Anti-Patterns

### ❌ Anti-Pattern 1: Opening `SchemaSession` in Per-Entity Loops
Schema modifications write catalog records and acquire catalog locks. Never open a `SchemaSession` inside a per-entity ingestion loop.

```python
# ❌ ANTI-PATTERN: Opens schema session per entity
for doc in documents:
    with graph.open_schema() as s:
        s.add_property_key(doc.key, DataType.String)
    with graph.begin() as txn:
        txn.g().addV("doc").property(doc.key, doc.val).next()

# ✅ CORRECT: Declare schema once at application startup
with graph.open_schema() as s:
    for key in known_keys:
        s.add_property_key(key, DataType.String)
```

### ❌ Anti-Pattern 2: Relying on Auto Mode for Strict Pipelines
Auto Mode infers types from the first written record. If a document enters with an integer `"zip_code": 90210`, the property key becomes typed as `Int32`, subsequently rejecting legitimate alphanumeric string zip codes like `"SW1A 1AA"`.

```python
# ❌ RISK in Auto Mode:
# First insert: property("zip", 90210) -> sets "zip" to Int32
# Second insert: property("zip", "90210-1234") -> SchemaViolation: expected Int32, got String!

# ✅ CORRECT: Explicitly declare String in SchemaSession
schema.add_property_key("zip", DataType.String)
```

### ❌ Anti-Pattern 3: Concurrent Ingestion into Undeclared Auto Mode Schemas
In `SchemaMode::Auto`, the first transaction to insert an unseen label or property key dynamically registers it in the internal schema catalog. If multiple concurrent worker threads insert new labels/keys simultaneously, their transactions will race on the schema catalog and fail with OCC `StoreError::Conflict` (`TransactionError` in Python) upon commit.

```python
# ❌ ANTI-PATTERN: Parallel workers racing on first-time auto-registration
# Worker 1 & Worker 2 both try to auto-create "event_type" simultaneously -> OCC Conflict!

# ✅ CORRECT: Pre-declare all labels and property keys in Strict Mode before starting workers
with graph.open_schema() as s:
    s.set_schema_mode(SchemaMode.Strict)
    s.add_vertex_label("event")
    s.add_property_key("event_type", DataType.String)
```

---

## Related Topics

- [Data Model & Type System](data_model.md) — Supported types, capacities, identifiers, and labels.
- [Bulk Loading](bulk_loading.md) — High-throughput ingestion with declared schemas.
- [Transactions & Concurrency](concurrency_and_tx.md) — ACID isolation and session types.

