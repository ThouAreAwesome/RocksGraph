# Getting Started with RocksGraph

**Target:** RocksGraph v0.2.0+

RocksGraph is an embeddable, ACID-compliant property graph database with Gremlin query language and integrated HNSW vector search. It runs in-process inside your application without external server daemons, JVM runtimes, or network overhead.

This guide walks you through installing RocksGraph, opening a database, creating vertices and edges with vector embeddings, running graph traversals, and querying nearest neighbors.

---

## 1. Installation

#### 🦀 Rust
Add `rocksgraph` to your `Cargo.toml`:
```toml
[dependencies]
rocksgraph = "0.2"
```
Or run:
```bash
cargo add rocksgraph
```

#### 🐍 Python
Install `rocksgraph` from PyPI (Python 3.9+):
```bash
pip install rocksgraph
```

---

## 2. Opening a Graph Database

RocksGraph opens an embedded database directly from a local directory path.

> [!TIP]
> **Thread-Safety & Cloning**: `Graph` is cheap to clone and fully thread-safe (`Send + Sync`). You can clone `graph` handles across multiple threads freely.
>
> **Lifecycle & Vector Persistence**: `Graph` has no automatic persistence in `Drop`. To durably snapshot in-memory HNSW vector indexes to disk and flush resources cleanly, always invoke `graph.close()` (or `graph.index_manager().save_all()`) before application termination.

#### 🦀 Rust
```rust
use rocksgraph::Graph;

// Open a persistent embedded database
let graph = Graph::open("/tmp/my_graph_db")?;

// ... use graph ...

// Clean shutdown: persists all vector indexes to disk
graph.close()?;
```

#### 🐍 Python
```python
from rocksgraph import Graph

# Open a persistent embedded database
graph = Graph("/tmp/my_graph_db")

# ... use graph ...

# Clean shutdown: persists all vector indexes to disk
graph.close()
```

---

## 3. Writing Data: Vertices, Edges & Embeddings

All graph writes in RocksGraph execute inside an ACID transaction (`TxnSession`). Transactions provide Snapshot Isolation with Optimistic Concurrency Control (OCC) and **Read-Your-Own-Writes (RYOW)**.

> [!NOTE]
> **Auto-Rollback on Drop**: `TxnSession` rolls back automatically if dropped without calling `.commit()` — see [Transactions & Concurrency](concurrency_and_tx.md#3-writing-data-txnsession--read-your-own-writes-ryow) for details.

Let's insert two people (`Alice` and `Bob`), a `knows` relationship, and attach vector embeddings for semantic search:

#### 🦀 Rust
```rust
use rocksgraph::{Graph, StoreError, Value};

fn write_sample_data(graph: &Graph) -> Result<(), StoreError> {
    let mut txn = graph.begin();

    // 1. Add Vertex 1 (Alice) with properties and a 3D embedding
    txn.g()
        .addV("person")
        .property("id", 1i64)
        .property("name", "Alice")
        .property("age", 30i32)
        .property("emb", Value::FloatVector(vec![0.95, 0.10, 0.05]))
        .next()?;

    // 2. Add Vertex 2 (Bob)
    txn.g()
        .addV("person")
        .property("id", 2i64)
        .property("name", "Bob")
        .property("age", 32i32)
        .property("emb", Value::FloatVector(vec![0.10, 0.90, 0.20]))
        .next()?;

    // 3. Add a directed edge: Alice -> knows -> Bob
    txn.g()
        .addE("knows")
        .from(1i64)
        .to(2i64)
        .property("since", 2021i32)
        .property("weight", 0.85f64)
        .next()?;

    // Commit all changes atomically
    txn.commit()?;
    Ok(())
}
```

#### 🐍 Python
```python
from rocksgraph import Graph, Vector

def write_sample_data(graph: Graph):
    # Using context manager auto-commits on clean exit or rolls back on exception
    with graph.begin() as txn:
        # 1. Add Vertex 1 (Alice)
        txn.g().addV("person") \
            .property("id", 1) \
            .property("name", "Alice") \
            .property("age", 30) \
            .property("emb", Vector([0.95, 0.10, 0.05])) \
            .next()

        # 2. Add Vertex 2 (Bob)
        txn.g().addV("person") \
            .property("id", 2) \
            .property("name", "Bob") \
            .property("age", 32) \
            .property("emb", Vector([0.10, 0.90, 0.20])) \
            .next()

        # 3. Add directed edge: Alice -> knows -> Bob
        txn.g().addE("knows") \
            .from_(1) \
            .to(2) \
            .property("since", 2021) \
            .property("weight", 0.85) \
            .next()
```

---

## 4. Querying the Graph: Graph Traversal

Read queries run against a point-in-time snapshot (`ReadSession`), guaranteeing zero locks and non-blocking multi-threaded concurrency.

> [!NOTE]
> **Rust vs. Python step arguments**: throughout these guides, Rust source/filter steps take an explicit array/slice — `.V([1])`, `.out(["knows"])`, `.V([])` for "all" — because Rust needs a concrete type for the argument. Python steps take plain variadic arguments instead — `.V(1)`, `.out("knows")`, bare `.V()` for "all" — since Python's `*args` makes the brackets unnecessary. Same steps, same semantics, different calling convention per language.

### Example A: Find people Alice knows

#### 🦀 Rust
```rust
let mut snap = graph.read();

// Start at Alice (id=1), traverse outgoing "knows" edges, extract name
let friends = snap
    .g()
    .V([1])
    .out(["knows"])
    .values(["name"])
    .to_list()?;

println!("Alice knows: {:?}", friends);
// Output: Alice knows: [String("Bob")]
```

#### 🐍 Python
```python
snap = graph.read()

# Start at Alice (id=1), traverse outgoing "knows" edges, extract name
friends = (
    snap.g()
    .V(1)
    .out("knows")
    .values("name")
    .to_list()
)

print("Alice knows:", friends)
# Output: Alice knows: ['Bob']
```

---

## 5. Vector Search: Nearest Neighbors

RocksGraph embeds an in-memory HNSW index for fast approximate nearest neighbor (ANN) search. Vector search is fully integrated with graph traversals:

> [!IMPORTANT]
> `.nearest()` is an **entry-point step** that seeds a traversal stream directly from the vector index. It must immediately follow `g.V([])`. You can then chain standard Gremlin filtering and navigation steps on the returned candidate stream.

> [!NOTE]
> Example B below never declares a vector index (no `graph.open_schema()` call) — in Auto mode, `.property("emb", ...)` only registers the property's *type*, not an HNSW index. Without a declared index, `.nearest()` silently falls back to an exact brute-force scan rather than erroring — correct results, but O(N), not the sub-linear ANN search the paragraph above describes. That's invisible at 2 vertices; at real scale it isn't. Declare an index before relying on `.nearest()` for performance — see [Vector Search Deep Dive](vector_search.md#2-declaring-vector-indexes).

### Example B: Find top-1 closest vertex to query vector `[1.0, 0.0, 0.0]`

#### 🦀 Rust
```rust
let mut snap = graph.read();

let query_vec = vec![1.0f32, 0.0, 0.0];

let nearest_people = snap
    .g()
    .V([])
    .nearest("emb", query_vec, 1)
    .hasLabel("person")
    .values(["name"])
    .to_list()?;

println!("Nearest person: {:?}", nearest_people);
// Output: Nearest person: [String("Alice")]
```

#### 🐍 Python
```python
snap = graph.read()

query_vec = [1.0, 0.0, 0.0]

nearest_people = (
    snap.g()
    .V()
    .nearest("emb", Vector(query_vec), 1)
    .hasLabel("person")
    .values("name")
    .to_list()
)

print("Nearest person:", nearest_people)
# Output: Nearest person: ['Alice']
```

---

## 6. Complete Runnable Examples

Here are standalone, copy-pasteable programs demonstrating the complete lifecycle:

#### 🦀 Rust (`src/main.rs`)
```rust
use rocksgraph::{Graph, Value};
use tempfile::tempdir;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create a database in a temporary directory
    let dir = tempdir()?;
    let graph = Graph::open(dir.path())?;

    // 2. Insert graph elements within a transaction
    {
        let mut txn = graph.begin();
        txn.g().addV("person").property("id", 1i64).property("name", "Alice")
            .property("emb", Value::FloatVector(vec![0.9, 0.1, 0.0])).next()?;
        txn.g().addV("person").property("id", 2i64).property("name", "Bob")
            .property("emb", Value::FloatVector(vec![0.1, 0.9, 0.0])).next()?;
        txn.g().addE("knows").from(1i64).to(2i64).property("weight", 0.9f64).next()?;
        txn.commit()?;
    }

    // 3. Query via snapshot
    let mut snap = graph.read();

    // Traversal: Who does Alice know?
    let friends = snap.g().V([1]).out(["knows"]).values(["name"]).to_list()?;
    println!("Friends of Alice: {:?}", friends);

    // Vector Search: Find closest to [1.0, 0.0, 0.0]
    let nearest = snap.g().V([]).nearest("emb", vec![1.0f32, 0.0, 0.0], 1)
        .values(["name"]).to_list()?;
    println!("Nearest neighbor: {:?}", nearest);

    // 4. Clean shutdown: persist in-memory vector index snapshots to disk
    graph.close()?;

    Ok(())
}
```

#### 🐍 Python (`main.py`)
```python
from rocksgraph import Graph, Vector
import tempfile

def main():
    # 1. Create a database in a temporary directory
    dir_path = tempfile.mkdtemp()
    graph = Graph(dir_path)

    # 2. Insert graph elements within a transaction
    with graph.begin() as txn:
        txn.g().addV("person").property("id", 1).property("name", "Alice") \
            .property("emb", Vector([0.9, 0.1, 0.0])).next()
        txn.g().addV("person").property("id", 2).property("name", "Bob") \
            .property("emb", Vector([0.1, 0.9, 0.0])).next()
        txn.g().addE("knows").from_(1).to(2).property("weight", 0.9).next()

    # 3. Query via snapshot
    snap = graph.read()

    # Traversal: Who does Alice know?
    friends = snap.g().V(1).out("knows").values("name").to_list()
    print("Friends of Alice:", friends)

    # Vector Search: Find closest to [1.0, 0.0, 0.0]
    nearest = snap.g().V().nearest("emb", Vector([1.0, 0.0, 0.0]), 1) \
        .values("name").to_list()
    print("Nearest neighbor:", nearest)

    # 4. Clean shutdown: persist in-memory vector index snapshots to disk
    graph.close()

if __name__ == "__main__":
    main()
```

---

## Next Steps

Explore the focused topic guides for deep dives into specific subsystems:

- [Data Model & Type System](data_model.md) — Properties, types, identifiers, and labels.
- [Gremlin Step Reference](step_reference.md) — Comprehensive catalog of all traversal steps and type transitions.
- [Vector Search Deep Dive](vector_search.md) — HNSW tuning, memory boundaries, distance metrics, and quantization.
- [Schema Management](schema_management.md) — Strict vs Auto schema modes, `SchemaSession`, and runtime DDL.
- [Transactions & Concurrency](concurrency_and_tx.md) — OCC conflict handling, isolation, and session lifecycles.
- [Bulk Loading](bulk_loading.md) — High-throughput offline SST file ingestion.
- [Performance Tuning](performance.md) — Batch sizing, vector memory calculations, and query optimization.
