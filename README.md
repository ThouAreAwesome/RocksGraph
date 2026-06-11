# RocksGraph

A Gremlin-compatible property graph query engine written in Rust, backed by RocksDB.

> **Status:** Early-stage (v0.1.0). Not yet production-ready or published to crates.io.

## Overview

RocksGraph translates a subset of [Gremlin](https://tinkerpop.apache.org/gremlin.html) traversal queries into a logical IR, optimizes them, and executes them against a persistent RocksDB store. It is designed as a clean, layered architecture separating query planning, optimization, and execution.

## Architecture

```
Gremlin fluent API  (gremlin::traversal)
        │
        ▼
   Planner          (planner)          Gremlin AST → LogicalPlan (engine-agnostic IR)
        │
        ▼
   Optimizer        (planner::optimizer) LogicalPlan → optimized LogicalPlan
        │
        ▼
   Volcano Engine   (engine::volcano)  Pull-based iterator pipeline
        │
        ▼
   LogicalGraph     (graph)            Query-scoped overlay; OCC conflict detection
        │
        ▼
   GraphStore       (store)            RocksDB backend (OptimisticTransactionDB)
```

| Module | Role |
|--------|------|
| `gremlin` | Fluent query builder; converts Gremlin API calls into a `LogicalPlan` |
| `planner` | Gremlin AST → engine-agnostic `LogicalPlan` IR |
| `planner::optimizer` | Rewrites a `LogicalPlan` into a more efficient equivalent (fixpoint iteration) |
| `engine::volcano` | Pull-based volcano iterator execution engine |
| `graph` | Query-scoped in-memory overlay over a `GraphStore` transaction |
| `store` | Pluggable storage backend abstraction; RocksDB implementation |
| `schema` | Label-ID ↔ label-string bidirectional mapping (planned) |
| `types` | Shared value types: `GValue`, `Primitive`, keys |

## Supported Gremlin Steps

Labels and edge labels are represented as `u16` integer IDs mapped through a schema layer.

### Traversal

| Step | Method |
|------|--------|
| `V(ids)` | `g.V([id])` |
| `out(labels)` | `.out([label_id])` |
| `in_(labels)` | `.in_([label_id])` |
| `both(labels)` | `.both([label_id])` |
| `outE(labels)` | `.outE([label_id])` |
| `inE(labels)` | `.inE([label_id])` |
| `bothE(labels)` | `.bothE([label_id])` |
| `inV()` | `.inV()` |
| `outV()` | `.outV()` |
| `otherV()` | `.otherV()` |

### Filtering

| Step | Method |
|------|--------|
| `has(key, value)` | `.has(key, value)` |
| `hasLabel(labels)` | `.hasLabel([label_id])` |
| `hasId(ids)` | `.hasId([id])` |
| `is(value)` | `.is(value)` |
| `where(traversal)` | `.where_(&mut sub_traversal)` |
| `limit(n)` | `.limit(n)` |

### Aggregation & Extraction

| Step | Method |
|------|--------|
| `count()` | `.count()` |
| `values(keys)` | `.values([key])` |
| `properties(keys)` | `.properties([key])` |

### Mutation

| Step | Method |
|------|--------|
| `addV(label)` | `.addV(label_id)` |
| `addE(label)` | `.addE(label_id)` |
| `from(vertex_id)` | `.from(vertex_id)` |
| `to(vertex_id)` | `.to(vertex_id)` |
| `property(key, value)` | `.property(key, value)` |
| `drop()` | `.drop()` |

### Composition

| Step | Method |
|------|--------|
| `union(traversals)` | `.union(vec![&mut t1, &mut t2])` |
| `coalesce(traversals)` | `.coalesce(vec![&mut t1, &mut t2])` |

## Consistency Model

RocksGraph uses **Optimistic Concurrency Control (OCC)** via RocksDB's `OptimisticTransactionDB`. Each query operates against a `LogicalGraph`: an in-memory overlay that tracks all mutations (adds, modifies, deletions) using a dirty-state machine. The overlay is committed atomically when `graph.commit()` is called.

### Batching multiple queries in one transaction

Multiple traversals can be built and executed against the same `LogicalGraph` instance before committing. All mutations are staged in the overlay and flushed to RocksDB atomically as a single transaction. Reads within the same `LogicalGraph` see the uncommitted writes of earlier traversals in the same batch (read-your-writes).

```rust
let mut graph = LogicalGraph::<RocksStorage>::new(store.begin());

// Both traversals share the same transaction
graphTraversalSource()
    .addV(1).property("name", "alice")
    .build(&mut graph)?.next().transpose()?;

graphTraversalSource()
    .addV(1).property("name", "bob")
    .build(&mut graph)?.next().transpose()?;

graph.commit()?; // alice and bob are written atomically
```

### LogicalGraph reuse after commit

- **After a successful `commit()`**: the overlay is cleared and a fresh RocksDB transaction is started internally. The same `LogicalGraph` instance **can be reused** for the next batch of queries without creating a new one.
- **After a `StoreError::Conflict`**: the underlying transaction is replaced with a fresh one, but the in-memory overlay is **not** cleared — it still holds the mutations from the failed attempt. Create a new `LogicalGraph` via `LogicalGraph::new(store.begin())` for a clean retry.

**Key invariants enforced by `LogicalGraph`:**
- Bidirectional edge indexing: both OUT and IN indices are written on commit
- Dangling edge prevention: edge endpoints are verified to exist before insertion
- Degree validation: vertices with incident edges cannot be dropped
- Tombstone visibility: deleted elements are invisible to subsequent reads within the same transaction

## Usage

### Opening a store

```rust
use rocksgraph::gremlin::traversal::open_rocks_store;

let store = open_rocks_store::<&str>(None)?; // temp dir
// or:
let store = open_rocks_store(Some("/path/to/db"))?;
```

### Building and executing a query

```rust
use rocksgraph::{
    graph::LogicalGraph,
    gremlin::traversal::{graphTraversalSource, open_rocks_store},
    store::RocksStorage,
};

let store = open_rocks_store::<&str>(None)?;
let mut graph = LogicalGraph::<RocksStorage>::new(store.begin());

// Build a traversal: find all vertices adjacent to vertex 1 via label 3, count them
let mut traversal = graphTraversalSource()
    .V([1])
    .out([3])
    .count()
    .build(&mut graph)?;

for result in &mut traversal {
    println!("{:?}", result?);
}
```

### Mutations

```rust
use rocksgraph::{
    graph::LogicalGraph,
    gremlin::traversal::{graphTraversalSource, open_rocks_store},
    store::RocksStorage,
};

let store = open_rocks_store::<&str>(None)?;
let mut graph = LogicalGraph::<RocksStorage>::new(store.begin());

// Add a vertex (label_id = 1) with a name property
graphTraversalSource()
    .addV(1)
    .property("name", "alice")
    .build(&mut graph)?
    .next()
    .transpose()?;

// Add an edge (label_id = 3) from vertex 42 to vertex 99 with a weight property
graphTraversalSource()
    .addE(3)
    .from(42)
    .to(99)
    .property("weight", 0.5_f64)
    .build(&mut graph)?
    .next()
    .transpose()?;

graph.commit()?;


// Find an edge (label_id = 3) from vertex 42 to vertex 99 and update its weight
let mut graph = LogicalGraph::<RocksStorage>::new(store.begin());
graphTraversalSource()
    .V([])
    .hasId([42])
    .outE([3])
    .where(__().otherV().hasId([99]))
    .property("weight", 99.99_f64)
    .build(&mut graph)?
    .next()
    .transpose()?;
graph.commit()?;
```

## Development

**Prerequisites:** Rust toolchain (stable), [`just`](https://github.com/casey/just)

```bash
# List all commands
just

# Build
just build

# Run tests
just test

# Format + clippy (required before committing)
just full-check

# Auto-fix formatting
just full-write

# Generate and open rustdoc
just doc
```

### Benchmarks

Benchmark binaries live in `src/bin/`. The `scripts/` directory contains helpers:

| Script | Purpose |
|--------|---------|
| `prepare_bench_data.sh` | Download and sample 1M edges from the SNAP soc-LiveJournal1 dataset |
| `bench_read.sh` | Run the read traversal benchmark |
| `bench_write.sh` | Run the write benchmark |
| `instruments_read.sh` | Profile read benchmark with macOS Instruments |
| `instruments_write.sh` | Profile write benchmark with macOS Instruments |

[Current benchmark records](BENCHMARKS.md)

## Known Limitations

- **Embedded only:** no server/client mode; queries are executed in-process.
- **Single-threaded per query:** each volcano pipeline uses `Rc`/`RefCell` and runs single-threaded; multiple queries can run concurrently against a shared `GraphStore`.
- **Integer label IDs:** labels are `u16` integers; string-to-ID mapping via the `schema` module is not yet fully implemented.
- **No full TinkerPop compliance:** lambdas, side effects, multi-path tracking, and many aggregate steps are not supported.
- **No distributed backend:** the `store::distributed` placeholder exists but is not implemented.

## Roadmap

### Engine & Query

- [ ] Improve TinkerPop Gremlin step coverage (lambdas, side-effects, path tracking, additional aggregation steps)
- [ ] Support bulk-load mode: offline SST file generation + direct RocksDB SST ingestion for high-throughput initial loads
- [ ] `GraphSnapshot` trait for a read-only query path (no overlay, no OCC overhead)
- [ ] Support strict schema mode

### Storage & Distribution

- [ ] Support at least one distributed key-value backend (e.g., TiKV, FoundationDB)
- [ ] Server-client mode (gRPC or WebSocket)

### Developer Experience

- [ ] Publish as a public crate on crates.io
- [ ] GitHub Pages rustdoc site

## License

RocksGraph is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License v2.0](https://www.gnu.org/licenses/old-licenses/gpl-2.0.html)
or (at your option) any later version.

Copyright © 2026 Austin Han.
