# RocksGraph

[![CI](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml/badge.svg)](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/rocksgraph.svg)](https://crates.io/crates/rocksgraph)
[![docs.rs](https://docs.rs/rocksgraph/badge.svg)](https://docs.rs/rocksgraph)
[![PyPI](https://img.shields.io/pypi/v/rocksgraph.svg)](https://pypi.org/project/rocksgraph/)
[![License: Apache 2.0 / MIT](https://img.shields.io/badge/License-Apache_2.0_|_MIT-blue.svg)](LICENSE-APACHE)

**RocksGraph** is an embeddable, ACID-compliant property graph database with Gremlin query language and integrated HNSW vector search. Open a database with one line of code, traverse relationships, and query semantic similarity—**no external servers, no network overhead, no JVM**.

```
  ┌──────────────────────────────────────────────────────────┐
  │                 Gremlin Traversal Engine                 │
  │  • Lazy streaming engine   • Multi-hop path traversals   │
  ├──────────────────────────────────────────────────────────┤
  │                 Graph Consistency Layer                  │
  │  • Snapshot ReadSession    • ACID TxnSession (OCC / RYOW)│
  ├────────────────────────────┬─────────────────────────────┤
  │     Graph Data Storage     │     Vector Index Engine     │
  │  • Vertices, Edges, Props  │  • In-Memory HNSW Graph     │
  │  • Schema & Type Metadata  │  • Quantization algorithms  │
  │                            └─────────────────────────────┤
  │  • Write-Ahead Log (WAL) — crash recovery & consistency  │
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

Comprehensive guides are available in the [`docs/guides/`](https://github.com/ThouAreAwesome/RocksGraph/tree/main/docs/guides/) directory and the [GitHub Wiki](https://github.com/ThouAreAwesome/RocksGraph/wiki):

| Guide                                                                                                    | Description                                                                                                        |
| :------------------------------------------------------------------------------------------------------- | :----------------------------------------------------------------------------------------------------------------- |
| 🚀 [**Getting Started**](https://github.com/ThouAreAwesome/RocksGraph/wiki/getting_started)               | 5-minute end-to-end walkthrough in Rust & Python.                                                                  |
| 📐 [**Data Model & Types**](https://github.com/ThouAreAwesome/RocksGraph/wiki/data_model)                 | Graph primitives, property types, identifier policies, and reserved keys.                                          |
| 🔍 [**Vector Search Deep Dive**](https://github.com/ThouAreAwesome/RocksGraph/wiki/vector_search)         | HNSW parameters, quantization (`F16`), memory limits, and query primitives (`nearest`, `similarity`, `neighbors`). |
| 🗺️ [**Gremlin Step Reference**](https://github.com/ThouAreAwesome/RocksGraph/wiki/step_reference)         | Comprehensive step-by-step reference for all traversal steps and type transitions.                                 |
| 📋 [**Schema Management & DDL**](https://github.com/ThouAreAwesome/RocksGraph/wiki/schema_management)     | Strict vs Auto schema modes, `SchemaSession`, and dynamic vector index management.                                 |
| 🔒 [**Transactions & Concurrency**](https://github.com/ThouAreAwesome/RocksGraph/wiki/concurrency_and_tx) | OCC conflict handling, Snapshot Isolation, and session lifecycles.                                                 |
| ⚡ [**Bulk Loading & SST Ingest**](https://github.com/ThouAreAwesome/RocksGraph/wiki/bulk_loading)        | High-throughput offline SST file generation and instant atomic DB loading.                                         |
| 🏎️ [**Performance Tuning**](https://github.com/ThouAreAwesome/RocksGraph/wiki/performance)                | Batching strategies, memory sizing formulas, and query optimization patterns.                                      |
| 📊 [**Benchmarks**](https://github.com/ThouAreAwesome/RocksGraph/wiki/benchmarks)                         | Measured write (bulk load, transactional OCC) and read throughput/latency across dataset scales.                   |

For Python developers, see the dedicated [Python Storefront](https://github.com/ThouAreAwesome/RocksGraph/blob/main/bindings/python/README.md).

---

## Architecture Overview

A Gremlin traversal is parsed into a logical plan, optimized (index-seek folding, filter reordering), and lowered into a physical plan executed by a streaming, pull-based iterator engine. That engine reads from two co-located backends: on-disk graph storage (vertices, edges, adjacency index) and an in-memory HNSW vector index, so a single traversal pipeline can mix edge navigation with nearest-neighbor lookups without crossing a process or network boundary.

For the full internal design — query planner rules, storage layout, WAL/vector-index lifecycle — see [`docs/design/architecture/`](https://github.com/ThouAreAwesome/RocksGraph/tree/main/docs/design/architecture/).

---

## Project Status

**Maturity**: RocksGraph is pre-1.0 software, currently at v0.2.3. The core engine — ACID transactions, Gremlin traversal, integrated HNSW vector search — is functional and covered by an extensive test suite: unit tests, property-based (`proptest`) round-trip tests for the bulk loader and the bytecode wire format, and fuzz testing on the wire-format decoder. It's a young project, though: the public API isn't frozen yet, and the on-disk format — while unchanged in practice since v0.1.0 — isn't formally guaranteed stable until 1.0.0. Good fit for side projects, prototypes, and anywhere you control the blast radius of a bad upgrade. Not yet the right choice if you need a storage-format stability guarantee today.

| Version | Stability                                                                             |
| ------- | ------------------------------------------------------------------------------------- |
| 0.2.x   | API may change. On-disk format may change. Not for production data you can't rebuild. |
| 0.3.x   | (planned) API stable. On-disk format stable.                                          |
| 1.0.0   | (planned) Full backward compatibility for both API and storage.                       |

**Maintenance**: Actively maintained. Issues responded to within a week. Releases when there's something worth shipping, not on a schedule. If that changes, it'll be reflected here.

**Roadmap** (directional, not a fixed schedule):
- v0.2.x (current): the API keeps growing based on real usage — more Gremlin traversal steps, additional vector search capabilities, and LLM/framework integrations may all land here before anything is locked down.
- Toward 0.3.x: once that feedback has shaped the surface, freeze the public API and the on-disk format.
- Toward 1.0.0: formalize the on-disk format guarantee with a documented migration policy for any future breaking change.

---

## License

Dual-licensed under either:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
