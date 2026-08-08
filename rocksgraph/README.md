# RocksGraph

[![CI](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml/badge.svg)](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/rocksgraph.svg)](https://crates.io/crates/rocksgraph)
[![docs.rs](https://docs.rs/rocksgraph/badge.svg)](https://docs.rs/rocksgraph)
[![PyPI](https://img.shields.io/pypi/v/rocksgraph.svg)](https://pypi.org/project/rocksgraph/)
[![License: Apache 2.0 / MIT](https://img.shields.io/badge/License-Apache_2.0_|_MIT-blue.svg)](LICENSE)

**RocksGraph** is an embeddable, ACID-compliant property graph database with Gremlin query language and integrated HNSW vector search. Open a database with one line of code, traverse relationships, and query semantic similarity—**no external servers, no network overhead, no JVM**.

```
  ┌──────────────────────────────────────────────────────────┐
  │                 Gremlin Traversal Engine                 │
  │  • Lazy streaming engine   • Multi-hop path traversals   │
  ├──────────────────────────────────────────────────────────┤
  │              Logical / Snapshot Graph Layer              │
  │  • Lock-free ReadSession   • ACID TxnSession (OCC / RYOW)│
  ├────────────────────────────┬─────────────────────────────┤
  │     Graph Data Storage     │     Vector Index Engine     │
  │  • Vertices, Edges, Props  │  • In-Memory HNSW Graph     │
  │  • Compact adjacency lists │  • F16 / F32 quantization   │
  │                            └─────────────────────────────┤
  │  • Write-Ahead Log (WAL)                                 │
  └──────────────────────────────────────────────────────────┘
```

---

## Key Features

- **In-Process & Zero-Config**: Runs directly within your Rust or Python process. Zero daemon management or cluster orchestration.
- **Unified Graph + Vector Search**: Combine relationship traversal and vector similarity search in a single declarative query pipeline.
- **ACID Transactions**: Snapshot Isolation with Optimistic Concurrency Control (OCC), Write-Ahead Logging (WAL), and Read-Your-Own-Writes (RYOW).
- **Streaming Query Engine**: Stream-based, lazy-iterator query processing with early `.limit()` termination and index pushdown.
- **High-Throughput Ingestion**: Dedicated `BulkLoader` for direct offline storage file generation and instant atomic DB ingestion.
- **Polyglot**: First-class Rust native crate and high-performance Python bindings via PyO3.

---

## 30-Second Quickstart

### 🦀 Rust

Add `rocksgraph` to your `Cargo.toml`:
```toml
[dependencies]
rocksgraph = "0.2"
```

```rust
use rocksgraph::{Graph, StoreError, Value};

fn main() -> Result<(), StoreError> {
    // 1. Open an embedded database
    let graph = Graph::open("./my_graph_db")?;

    // 2. Insert graph data with vector embeddings in an ACID transaction
    {
        let mut txn = graph.begin();
        txn.g().addV("person").property("id", 1i64).property("name", "Alice")
            .property("emb", Value::FloatVector(vec![0.9, 0.1, 0.0])).next()?;
        txn.g().addV("person").property("id", 2i64).property("name", "Bob")
            .property("emb", Value::FloatVector(vec![0.1, 0.9, 0.0])).next()?;
        txn.g().addE("knows").from(1i64).to(2i64).property("since", 2022i32).next()?;
        txn.commit()?;
    }

    // 3. Query via point-in-time snapshot
    let mut snap = graph.read();

    // Traversal: Find people Alice knows
    let friends = snap.g().V([1]).out(["knows"]).values(["name"]).to_list()?;
    println!("Alice knows: {friends:?}"); // ["Bob"]

    // Vector Search: Find closest person to query vector.
    // No index declared here, so this runs an exact brute-force scan, not HNSW —
    // see the Getting Started guide for when/how to declare a vector index.
    let nearest = snap.g().V([]).nearest("emb", vec![1.0f32, 0.0, 0.0], 1)
        .values(["name"]).to_list()?;
    println!("Nearest neighbor: {nearest:?}"); // ["Alice"]

    // Clean shutdown: persists all vector indexes to disk
    graph.close()?;

    Ok(())
}
```

### 🐍 Python

Install via pip:
```bash
pip install rocksgraph
```

```python
from rocksgraph import Graph, Vector

# 1. Open an embedded database
graph = Graph("./my_graph_db")

# 2. Insert graph data in an ACID transaction
with graph.begin() as txn:
    txn.g().addV("person").property("id", 1).property("name", "Alice") \
        .property("emb", Vector([0.9, 0.1, 0.0])).next()
    txn.g().addV("person").property("id", 2).property("name", "Bob") \
        .property("emb", Vector([0.1, 0.9, 0.0])).next()
    txn.g().addE("knows").from_(1).to(2).property("since", 2022).next()

# 3. Query via snapshot
snap = graph.read()

# Traversal: Find people Alice knows
friends = snap.g().V(1).out("knows").values("name").to_list()
print("Alice knows:", friends) # ['Bob']

# Vector Search: Find closest person.
# No index declared here, so this runs an exact brute-force scan, not HNSW —
# see the Getting Started guide for when/how to declare a vector index.
nearest = snap.g().V().nearest("emb", [1.0, 0.0, 0.0], 1) \
    .values("name").to_list()
print("Nearest neighbor:", nearest) # ['Alice']

# Clean shutdown
graph.close()
```

---

## User Documentation & Topic Guides

Comprehensive guides are available in the [`docs/guides/`](docs/guides/) directory and the [GitHub Wiki](https://github.com/ThouAreAwesome/RocksGraph/wiki):

| Guide | Description |
| :--- | :--- |
| 🚀 [**Getting Started**](docs/guides/getting_started.md) | 5-minute end-to-end walkthrough in Rust & Python. |
| 📐 [**Data Model & Types**](docs/guides/data_model.md) | Graph primitives, property types, identifier policies, and reserved keys. |
| 🔍 [**Vector Search Deep Dive**](docs/guides/vector_search.md) | HNSW parameters, quantization (`F16`), memory limits, and query primitives (`nearest`, `similarity`, `neighbors`). |
| 🗺️ [**Gremlin Step Reference**](docs/guides/step_reference.md) | Comprehensive step-by-step reference for all traversal steps and type transitions. |
| 📋 [**Schema Management & DDL**](docs/guides/schema_management.md) | Strict vs Auto schema modes, `SchemaSession`, and dynamic vector index management. |
| 🔒 [**Transactions & Concurrency**](docs/guides/concurrency_and_tx.md) | OCC conflict handling, Snapshot Isolation, and session lifecycles. |
| ⚡ [**Bulk Loading & SST Ingest**](docs/guides/bulk_loading.md) | High-throughput offline SST file generation and instant atomic DB loading. |
| 🏎️ [**Performance Tuning**](docs/guides/performance.md) | Batching strategies, memory sizing formulas, and query optimization patterns. |

For Python developers, see the dedicated [Python Storefront](bindings/python/README.md).

---

## Architecture Overview

A Gremlin traversal is parsed into a logical plan, optimized (index-seek folding, filter reordering), and lowered into a physical plan executed by a streaming, pull-based iterator engine. That engine reads from two co-located backends: on-disk graph storage (vertices, edges, adjacency index) and an in-memory HNSW vector index, so a single traversal pipeline can mix edge navigation with nearest-neighbor lookups without crossing a process or network boundary.

For the full internal design — query planner rules, storage layout, WAL/vector-index lifecycle — see [`docs/design/architecture/`](../docs/design/architecture/).

---

## License

Dual-licensed under either:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
