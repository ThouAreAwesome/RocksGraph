# RocksGraph for Python

[![CI](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml/badge.svg)](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/rocksgraph.svg)](https://pypi.org/project/rocksgraph/)
[![Python 3.9+](https://img.shields.io/badge/python-3.9+-blue.svg)](https://pypi.org/project/rocksgraph/)
[![License: Apache 2.0 / MIT](https://img.shields.io/badge/License-Apache_2.0_|_MIT-blue.svg)](../../LICENSE)

**RocksGraph** is an embeddable, ACID-compliant property graph database with Gremlin query language and integrated HNSW vector search, compiled directly to native code via PyO3.

It runs in-process inside your Python application without external server daemons, JVM runtimes, or network overhead.

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

```bash
pip install rocksgraph
```

---

## 20-Second Quickstart

```python
from rocksgraph import Graph, Vector

# 1. Open a persistent embedded database
graph = Graph("./my_graph_db")

# 2. Write graph vertices, edges, and vector embeddings in an ACID transaction
with graph.begin() as txn:
    txn.g().addV("person").property("id", 1).property("name", "Alice").property("emb", Vector([0.9, 0.1, 0.0])).next()
    txn.g().addV("person").property("id", 2).property("name", "Bob").property("emb", Vector([0.1, 0.9, 0.0])).next()
    txn.g().addE("knows").from_(1).to(2).property("since", 2022).next()

# 3. Query via point-in-time snapshot
snap = graph.read()

# Graph traversal: Who does Alice know?
friends = snap.g().V(1).out("knows").values("name").to_list()
print("Friends of Alice:", friends)  # ['Bob']

# Vector Search: Find closest person to query embedding.
# No index declared here, so this runs an exact brute-force scan, not HNSW —
# see the Getting Started guide for when/how to declare a vector index.
nearest = snap.g().V().nearest("emb", Vector([1.0, 0.0, 0.0]), 1).values("name").to_list()
print("Nearest neighbor:", nearest)  # ['Alice']

# Clean shutdown
graph.close()
```

---

## Pythonic Architecture & Idioms

### 1. Context Manager Lifecycle
`TxnSession` and `SchemaSession` implement Python's context manager protocol (`with` statement):
- Automatically commits on normal block exit.
- Automatically rolls back and aborts staged changes if an exception is raised.

```python
# Auto-rolls back on exception without corrupting database state
try:
    with graph.begin() as txn:
        txn.g().addV("account").property("id", 42).property("balance", 100).next()
        raise RuntimeError("Something failed!")
except RuntimeError:
    pass  # Transaction was cleanly aborted
```

### 2. First-Class Type Wrappers
- **`Vector(list[float])`**: Continuous dense vector embedding wrapper for similarity queries and property assignments.
- **`Int64(int)`**: Explicit 64-bit integer type marker for vertex IDs.
- **`DistanceMetric` / `Quantization`**: Enums for configuring HNSW indexes (`DistanceMetric.Cosine`, `DistanceMetric.DotProduct`, `DistanceMetric.L2`, `Quantization.F16`, `Quantization.F32`).

### 3. Session Model
```
Graph(path)
  ├── .read()             ──► ReadSession     (Lock-free point-in-time snapshot reads)
  ├── .begin()            ──► TxnSession      (ACID transactional writes with RYOW)
  ├── .open_schema()      ──► SchemaSession   (Atomic DDL — labels, types, vector indexes)
  ├── .open_bulk_loader() ──► BulkLoader      (High-throughput offline SST file ingestion)
  └── .index_manager()    ──► IndexManager    (Vector index maintenance — rebuild, save)
```

### 4. Exception Hierarchy
All RocksGraph exceptions inherit from `rocksgraph.StoreError`:
- **`TransactionError`**: OCC optimistic commit conflict (retry recommended).
- **`SchemaError`**: Strict schema validation violations or undeclared properties.
- **`IntegrityError`**: Duplicate entity or key conflicts.
- **`QueryError`**: Invalid traversal syntax or query planner constraints.
- **`VectorError`**: Vector dimension mismatch or index configuration error.
- **`StorageError`**: Low-level storage and I/O failures.

---

## Topic Guides & Documentation

Full documentation and guides are available in the repository docs and GitHub Wiki:

| Guide | Description |
| :--- | :--- |
| 🚀 [**Getting Started**](../../docs/guides/getting_started.md) | 5-minute end-to-end walkthrough in Rust & Python. |
| 📐 [**Data Model & Types**](../../docs/guides/data_model.md) | Graph primitives, property types, identifier policies, and reserved keys. |
| 🔍 [**Vector Search Deep Dive**](../../docs/guides/vector_search.md) | HNSW parameters, quantization (`F16`), memory limits, and query primitives (`nearest`, `similarity`, `neighbors`). |
| 🗺️ [**Gremlin Step Reference**](../../docs/guides/step_reference.md) | Comprehensive step-by-step reference for all traversal steps and type transitions. |
| 📋 [**Schema Management & DDL**](../../docs/guides/schema_management.md) | Strict vs Auto schema modes, `SchemaSession`, and dynamic vector index management. |
| 🔒 [**Transactions & Concurrency**](../../docs/guides/concurrency_and_tx.md) | OCC conflict handling, Snapshot Isolation, and session lifecycles. |
| ⚡ [**Bulk Loading & SST Ingest**](../../docs/guides/bulk_loading.md) | High-throughput offline SST file generation and instant atomic DB loading. |
| 🏎️ [**Performance Tuning**](../../docs/guides/performance.md) | Batching strategies, memory sizing formulas, and query optimization patterns. |

For the Rust crate, see the [main repository README](../../rocksgraph/README.md).

---

## License

Dual-licensed under Apache 2.0 and MIT licenses.
