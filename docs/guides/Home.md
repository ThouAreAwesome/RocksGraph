# Welcome to the RocksGraph Wiki! 🚀

RocksGraph is an embeddable, ACID-compliant property graph database featuring Gremlin-style traversals and integrated HNSW vector search. 

This wiki contains all the official guides, tutorials, and references to help you get the most out of RocksGraph in both **Rust** and **Python**.

---

## 📚 User Guides

Whether you're just getting started or looking to optimize an existing workload, we have you covered:

- 🚀 **[Getting Started](getting_started.md)** — Installation, connecting, and your first queries.
- 🏗️ **[Data Model & Types](data_model.md)** — Vertices, edges, properties, and supported primitives.
- 🗺️ **[Gremlin Step Reference](step_reference.md)** — Complete list of supported traversal steps (`has`, `outE`, `order`, etc.).
- 📐 **[Schema Management](schema_management.md)** — Strict vs Auto schema modes, index creation, and type enforcement.
- 🎯 **[Vector Search Deep Dive](vector_search.md)** — Using HNSW indexes to find similar embeddings directly within graph traversals.
- 🔒 **[Transactions & Concurrency](concurrency_and_tx.md)** — Read/write isolation, OCC, and retries.
- ⚡ **[Bulk Loading & SST Ingest](bulk_loading.md)** — How to bypass transactions for massive initial data imports.
- 🏎️ **[Performance Tuning](performance.md)** — Tips for batching, query optimization, and tuning execution options.
- 📊 **[Benchmarks](benchmarks.md)** — Measured write and read throughput/latency across dataset scales.

---

## 🔗 Developer Resources

- 💻 **[GitHub Repository](https://github.com/ThouAreAwesome/RocksGraph)**
- 📦 **[Rust Crate (crates.io)](https://crates.io/crates/rocksgraph)**
- 📖 **[Rust API Docs (docs.rs)](https://docs.rs/rocksgraph)**
- 🐍 **[Python Package (PyPI)](https://pypi.org/project/rocksgraph/)**

*(Use the sidebar on the right to easily navigate between pages!)*
