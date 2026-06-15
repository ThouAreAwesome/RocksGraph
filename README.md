# RocksGraph

A Gremlin-compatible property graph query engine written in Rust, backed by RocksDB.

> **Status:** Early-stage (v0.1.0). Not yet production-ready or published to crates.io.

## Overview

RocksGraph translates a subset of [Gremlin](https://tinkerpop.apache.org/gremlin.html) traversal queries into a logical IR, optimizes them, and executes them against a persistent RocksDB store. It is designed as a clean, layered architecture separating the user-facing session API, query planning, optimization, and execution.

## Architecture

```
User code
    │  Graph::open / graph.read() / graph.begin()
    ▼
api                  User-facing session layer (Graph, ReadSession, TxSession)
    │  session.g() → ReadTraversal / WriteTraversal
    ▼
gremlin::traversal   Fluent step builder; accumulates LogicalSteps into an AST
    │
    ▼
planner              Gremlin AST → LogicalPlan (engine-agnostic IR) + optimizer
    │
    ▼
engine::volcano      Pull-based Volcano iterator pipeline
    │
    ▼
graph                Query-scoped overlay (OCC dirty tracking, read-your-writes)
    │
    ▼
store / RocksDB      OptimisticTransactionDB persistence
```

| Module | Visibility | Role |
|--------|-----------|------|
| `api` | `pub` | `Graph`, `ReadSession`, `TxSession` — the only types users import |
| `gremlin` | `pub(crate)` | Fluent step builder; converts method chains into `LogicalPlan` AST |
| `planner` | `pub(crate)` | Gremlin AST → engine-agnostic `LogicalPlan` IR + optimizer |
| `engine::volcano` | `pub(crate)` | Pull-based Volcano iterator execution engine |
| `graph` | `pub(crate)` | Query-scoped in-memory overlay over a `GraphStore` transaction |
| `store` | `pub(crate)` | Pluggable storage backend abstraction; RocksDB implementation |
| `schema` | `pub` | Label-ID ↔ label-string bidirectional mapping (planned) |
| `types` | `pub` | Shared value types: `GValue`, `Primitive`, keys |

## Session Model

Every interaction with the graph goes through a session. Sessions are obtained from a `Graph` handle:

```
Graph::open(path)
  ├── .read()   → ReadSession    read-only snapshot; no commit needed
  └── .begin()  → TxSession      OCC read-write transaction
                    ├── .commit()   atomically flush mutations
                    └── .rollback() discard (also called automatically on drop)
```

Each session exposes a single method `g()` that returns a blank traversal. All Gremlin step methods live on the traversal, not on the session:

```rust
// Read path
let mut snap = graph.read();
let names = snap.g().V([1]).out([KNOWS]).values(["name"]).next()?.unwrap();

// Write path
let mut tx = graph.begin();
tx.g().addV(PERSON).property("name", "alice").next()?;
tx.g().V([1]).out([KNOWS]).count().next()?; // read-your-writes within the same tx
tx.commit()?;
```

`Graph` is cheap to clone (wraps an `Arc` internally), safe to share across threads. Sessions are single-threaded — create one per thread or per request.

### OCC conflict handling

RocksGraph uses **Optimistic Concurrency Control** via RocksDB's `OptimisticTransactionDB`. `commit()` returns `StoreError::Conflict` if a concurrent transaction modified an overlapping key. The caller must retry from scratch with a new `TxSession`:

```rust
loop {
    let mut tx = graph.begin();
    // ... build mutations ...
    match tx.commit() {
        Ok(_) => break,
        Err(StoreError::Conflict) => continue, // retry
        Err(e) => return Err(e),
    }
}
```

**Key invariants enforced by the transaction layer:**
- Bidirectional edge indexing: both OUT and IN indices are written on commit
- Dangling edge prevention: edge endpoints are verified to exist before insertion
- Degree validation: vertices with incident edges cannot be dropped
- Tombstone visibility: deleted elements are invisible to later reads within the same transaction

## Supported Gremlin Steps

Labels and edge labels are represented as `u16` integer IDs. String-to-ID mapping is provided by the `schema` module (in progress).

### Traversal

| Step | Method |
|------|--------|
| `V(ids)` | `.V([id, ...])` |
| `out(labels)` | `.out([label_id, ...])` |
| `in_(labels)` | `.in_([label_id, ...])` |
| `both(labels)` | `.both([label_id, ...])` |
| `outE(labels)` | `.outE([label_id, ...])` |
| `inE(labels)` | `.inE([label_id, ...])` |
| `bothE(labels)` | `.bothE([label_id, ...])` |
| `inV()` | `.inV()` |
| `outV()` | `.outV()` |
| `otherV()` | `.otherV()` |

### Filtering

| Step | Method |
|------|--------|
| `has(key, value)` | `.has(key, value)` |
| `hasLabel(labels)` | `.hasLabel([label_id, ...])` |
| `hasId(ids)` | `.hasId([id, ...])` |
| `where(traversal)` | `.r#where(&mut __().xxx())` |
| `limit(n)` | `.limit(n)` |

### Aggregation & Extraction

| Step | Method |
|------|--------|
| `count()` | `.count()` |
| `values(keys)` | `.values(["key", ...])` |
| `properties(keys)` | `.properties(["key", ...])` |
| `dedup()` | `.dedup()` |
| `path()` | `.path()` |

### Mutation (WriteTraversal only)

| Step | Method |
|------|--------|
| `addV(label)` | `.addV(label_id)` |
| `addE(label)` | `.addE(label_id)` |
| `from(vertex_id)` | `.from(vertex_id)` |
| `to(vertex_id)` | `.to(vertex_id)` |
| `property(key, value)` | `.property(key, value)` |
| `drop()` | `.drop()` |

### Composition

| Step | Method | Notes |
|------|--------|-------|
| `union(traversals)` | `.union([__().xxx(), __().yyy()])` | merges all result streams |
| `coalesce(traversals)` | `.coalesce([__().xxx(), __().yyy()])` | first non-empty branch wins |

### Terminal Operations

| Operation | ReadTraversal | WriteTraversal | Returns |
|-----------|:-------------:|:--------------:|---------|
| `next()` | ✓ | ✓ | `Result<Option<GValue>, StoreError>` |
| `toList()` | ✓ | ✓ | `Result<Option<GValue>, StoreError>` |


## Usage

### Opening a graph

```rust
use rocksgraph::Graph;

// Open an existing database or create a new one on disk:
let graph = Graph::open("./path/to/db")?;
// or a temporary directory for tests:
let graph = Graph::open(tempfile::tempdir()?.path())?;
```

`Graph` is `Clone` — clone it freely to share across threads.

### Read queries

```rust
use rocksgraph::{Graph, TraversalBuilder, GValue, Primitive, __};

const KNOWS: u16 = 3;

let graph = Graph::open("./path/to/db")?;
let mut snap = graph.read();

// Count neighbors of vertex 1 via KNOWS edges
let count = snap.g().V([1]).out([KNOWS]).count().next()?.unwrap();

// Fetch property values for multiple vertices
let values = snap.g()
    .V([]).hasId([1, 2, 3])
    .values(["name", "age"])
    .next()?.unwrap();

// Sub-traversal filter: outgoing KNOWS edges whose other endpoint is vertex 2
let ct = snap.g()
    .V([1])
    .outE([KNOWS])
    .r#where(__().otherV().hasId([2]))
    .count()
    .next()?.unwrap();
```

### Write transactions

```rust
use rocksgraph::{Graph, TraversalBuilder, StoreError, __};

const PERSON: u16 = 1;
const KNOWS: u16  = 3;

let graph = Graph::open("./path/to/db")?;
let mut tx = graph.begin();

// Add vertices
tx.g().addV(PERSON).property("id", 1i64).property("name", "alice").property("age", 30i64).next()?;
tx.g().addV(PERSON).property("id", 2i64).property("name", "bob").property("age", 25i64).next()?;

// Add an edge
tx.g().addE(KNOWS).from(1).to(2).property("weight", 0.9f64).next()?;

tx.commit()?;
```

### Idempotent upserts with coalesce

```rust
use rocksgraph::{Graph, TraversalBuilder, GValue, Primitive, StoreError, __};

const PERSON: u16 = 1;
const KNOWS: u16  = 3;

let graph = Graph::open("./path/to/db")?;

// Upsert vertex: read existing or create new
let mut tx = graph.begin();
tx.g()
    .V([98])
    .coalesce([
        __().V([42]).values(["id"]),          // branch 1: vertex exists → emit id
        __().addV(PERSON)                        // branch 2: create it
            .property("id", 98i64)
            .property("name", "charlie")
            .property("age", 40i64),
    ])
    .next()?;

// Upsert edge: check for existing or create
tx.g()
    .V([98])
    .coalesce([
        __().outE([KNOWS]).r#where(__().otherV().hasId([99])).values(["weight"]),
        __().addE(KNOWS).from(42).to(99).property("weight", 0.5f64),
    ])
    .next()?;

tx.commit()?;
```

### Anonymous sub-traversals with `__()`

`__()` creates a context-free traversal used as an argument to `where`, `coalesce`, and `union`. The type (`GraphTraversal`) is doc-hidden; you never need to name it:

```rust
// where: filter edges whose other endpoint matches a condition
snap.g().V([1]).outE([EDGE]).r#where(__().otherV().hasLabel([LABEL])).count().next()?;

// union: merge results from multiple branches
snap.g().V([1]).union([__().outE([A]), __().outE([B])]).count().next()?;

// coalesce: first non-empty branch
tx.g().V([id]).coalesce([__().values(["name"]), __().addV(PERSON).property("name", "x")]).next()?;
```

### Multiple queries per session

`g()` returns a temporary traversal that borrows the session for exactly one statement. Once the statement ends, the borrow is released and you can call `g()` again:

```rust
let mut tx = graph.begin();

// Each call to g() is an independent query against the same transaction.
tx.g().addV(PERSON).property("id", 1i64).property("name", "alice").next()?;
tx.g().addV(PERSON).property("id", 2i64).property("name", "bob").next()?;
// alice and bob are both visible here (read-your-writes)
let count = tx.g().V([]).count().next()?.unwrap();

tx.commit()?; // both writes flushed atomically
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

```bash
# Build release binaries
just build-release

# Run read benchmark (adjust paths as needed)
./scripts/bench_read.sh

# Run write benchmark
./scripts/bench_write.sh
```

[Current benchmark records](BENCHMARKS.md)

## Known Limitations

- **Embedded only:** no server/client mode; queries are executed in-process.
- **Single-threaded per query:** each volcano pipeline runs single-threaded; multiple sessions can run concurrently against a shared `Graph`.
- **Integer label IDs:** labels are `u16` integers; string-to-ID mapping via the `schema` module is not yet fully implemented.
- **No full TinkerPop compliance:** lambdas, side effects, multi-path tracking, and many aggregate steps are not supported.
- **No distributed backend:** placeholder exists but is not implemented.

## Roadmap

### Engine & Query

- [ ] Improve TinkerPop Gremlin step coverage (lambdas, side-effects, path tracking, additional aggregation steps)
- [ ] Support bulk-load mode: offline SST file generation + direct RocksDB SST ingestion for high-throughput initial loads
- [x] `ReadSession` / `ReadTraversal` — read-only snapshot path with no OCC overhead
- [ ] Support strict schema mode
- [ ] `to_list()` on `ReadTraversal`

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
