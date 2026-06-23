# RocksGraph

A Gremlin-inspired property graph query engine written in Rust, backed by RocksDB.
RocksGraph takes the traversal model from TinkerPop but makes deliberate departures
where the standard's design choices create unnecessary complexity or overhead.
See [docs/design_principles.md](docs/design_principles.md) for the full rationale.

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
gremlin              Rust DSL; accumulates LogicalSteps into a LogicalPlan
    │
    ▼
planner              LogicalPlan optimizer (index-seek folding, filter reordering, …)
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
| `api` | `pub` | `Graph`, `ReadSession`, `TxSession` — the only types users import directly |
| `gremlin` | internal | Fluent step builders; converts method chains into a `LogicalPlan` |
| `planner` | internal | Engine-agnostic `LogicalPlan` IR + optimizer rules |
| `engine::volcano` | internal | Pull-based Volcano iterator execution engine |
| `graph` | internal | Query-scoped in-memory overlay over a `GraphStore` transaction |
| `store` | internal | Pluggable storage backend abstraction; RocksDB implementation |
| `schema` | `pub` | Label/property-key registry; `Auto` vs `Strict` schema modes (see [Schema Modes](#schema-modes)) |

## Value Types

All user-facing query inputs and outputs use types from `gremlin::value`, re-exported at the crate root:

| Type | Description |
|------|-------------|
| `Value` | Scalar or composite result: `Null`, `Bool`, `Int32`, `Int64`, `UInt16`, `Float32`, `Float64`, `String`, `Uuid`, `Vertex`, `Edge`, `Property`, `List`, `Map`, `Path` |
| `Key` | Property key selector: `Key::Id` (vertex/edge id), `Key::Label` (label, decoded to its string name), `Key::Property(SmolStr)` (user property). String literals convert to `Key::Property` via `From<&str>`. |
| `Predicate` | Filter condition: `Predicate::Eq`, `Within`, `Without`, `Gt`, `Gte`, `Lt`, `Lte`, `Between`, `Ne` |
| `Vertex` | Materialized vertex: `id`, `label` (decoded string name), `properties` |
| `Edge` | Materialized edge: `out_v`, `in_v`, `label` (decoded string name), `rank` (`u16`, see `Value::UInt16`), `properties` |
| `Property` | Key-value property element returned by `.properties()` |
| `Map` | Ordered key-value map returned by `.group()` etc. |
| `Path` | Sequence of values with per-step labels returned by `.path()` |

Predicate constructors are free functions: `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `between`, `within`, `without`.

### Key routing

`Key` controls how steps like `.has()` and `.values()` are dispatched:

```rust
// Key::Id  → HasIdStep (vertex id index lookup)
.has(Key::Id, 1i64)
.hasId([1, 2, 3])          // shorthand for the same
.values([Key::Id])         // returns the vertex id as Value::Int64

// Key::Label → HasLabelStep (label filter)
.has(Key::Label, "person")
.hasLabel(["person"])      // shorthand
.values([Key::Label])      // returns the label's string name as Value::String

// Key::Property("name") → property-bag lookup
.has("name", "marko")      // string literals convert to Key::Property automatically
.values(["name"])
```

Note: `.has("id", N)` routes through `Key::Property("id")` — a different code path from `.has(Key::Id, N)` / `.hasId(N)`, but the optimizer folds `V([]).has("id", N)` into a vertex id index seek, so the end result is the same.

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
let name = snap.g().V([1]).out(["knows"]).values(["name"]).next()?.unwrap();

// Write path
let mut tx = graph.begin();
tx.g().addV("person").property("id", 1i64).property("name", "alice").next()?;
tx.g().V([1]).out(["knows"]).count().next()?; // read-your-writes within the same tx
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

Vertex labels, edge labels, and property keys are plain strings (e.g. `"person"`, `"knows"`) at
the traversal API. The `schema` module interns them to compact numeric IDs internally — see
[Schema Modes](#schema-modes) for how and when that registration happens.

### Traversal

| Step | Method |
|------|--------|
| `V(ids)` | `.V([id, ...])` |
| `out(labels)` | `.out([label, ...])` |
| `in_(labels)` | `.in_([label, ...])` |
| `both(labels)` | `.both([label, ...])` |
| `outE(labels)` | `.outE([label, ...])` |
| `inE(labels)` | `.inE([label, ...])` |
| `bothE(labels)` | `.bothE([label, ...])` |
| `inV()` | `.inV()` |
| `outV()` | `.outV()` |
| `otherV()` | `.otherV()` |

### Filtering

| Step | Method |
|------|--------|
| `has(key, value)` | `.has(key, pred)` — `key` is `Key::Id`, `Key::Label`, or a `&str` |
| `hasLabel(labels)` | `.hasLabel([label, ...])` |
| `hasId(ids)` | `.hasId([id, ...])` |
| `is(pred)` | `.is(pred)` — filter the current scalar value |
| `where(traversal)` | `.r#where(__().xxx())` |
| `limit(n)` | `.limit(n)` |
| `dedup()` | `.dedup()` |

### Extraction & Aggregation

| Step | Method | Notes |
|------|--------|-------|
| `values(keys)` | `.values([key, ...])` | `key` may be `Key::Id`, `Key::Label`, or `&str` |
| `properties(keys)` | `.properties(["key", ...])` | returns `Property` elements; id/label excluded |
| `count()` | `.count()` | |
| `fold()` | `.fold()` | collects all results into a single `Value::List` |
| `path()` | `.path()` | returns `Value::Path` with per-step labels |

### Mutation (WriteTraversal only)

| Step | Method |
|------|--------|
| `addV(label)` | `.addV(label)` |
| `addE(label)` | `.addE(label)` |
| `from(vertex_id)` | `.from(vertex_id)` |
| `to(vertex_id)` | `.to(vertex_id)` |
| `property(key, value)` | `.property(key, value)` — `"id"` sets the vertex/edge id |
| `drop()` | `.drop()` |

### Composition

| Step | Method | Notes |
|------|--------|-------|
| `union(traversals)` | `.union([__().xxx(), __().yyy()])` | merges all result streams |
| `coalesce(traversals)` | `.coalesce([__().xxx(), __().yyy()])` | first non-empty branch wins |

### Terminal Operations

| Operation | ReadTraversal | WriteTraversal | Returns |
|-----------|:-------------:|:--------------:|---------|
| `next()` | ✓ | ✓ | `Result<Option<Value>, StoreError>` |
| `to_list()` | ✓ | ✓ | `Result<Vec<Value>, StoreError>` |
| `iter()` | ✓ | ✓ | `Result<BuiltTraversal, StoreError>` — lazy `Iterator<Item = Result<Value, StoreError>>` |

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
use rocksgraph::{Graph, Key, TraversalBuilder, Value, __};

let graph = Graph::open("./path/to/db")?;
let mut snap = graph.read();

// Count neighbors of vertex 1 via "knows" edges
let count = snap.g().V([1]).out(["knows"]).count().next()?.unwrap();
assert_eq!(count, Value::Int64(3));

// Fetch property values
let name = snap.g().V([1]).values(["name"]).next()?.unwrap();
assert_eq!(name, Value::String("marko".into()));

// Fetch vertex id and label (decoded to its string name) alongside a property
let results = snap.g()
    .V([1])
    .values([Key::Id, Key::Label, "name".into()])
    .to_list()?;

// Sub-traversal filter: outgoing "knows" edges whose other endpoint is vertex 2
let ct = snap.g()
    .V([1])
    .outE(["knows"])
    .r#where(__().otherV().hasId([2]))
    .count()
    .next()?.unwrap();

// Lazy iteration
for result in snap.g().V([]).out(["knows"]).iter()? {
    let value = result?;
    // process each Value::Vertex(...)
}
```

### Write transactions

```rust
use rocksgraph::{Graph, TraversalBuilder, StoreError};

let graph = Graph::open("./path/to/db")?;
let mut tx = graph.begin();

// Add vertices — "id" is the reserved property key for the vertex id.
// "person" and "knows" register automatically on first use (SchemaMode::Auto,
// the default) — see "Schema Modes" below for the alternative, explicit-declaration mode.
tx.g().addV("person").property("id", 1i64).property("name", "alice").property("age", 30i32).next()?;
tx.g().addV("person").property("id", 2i64).property("name", "bob").property("age", 25i32).next()?;

// Add an edge
tx.g().addE("knows").from(1).to(2).property("weight", 0.9f64).next()?;

tx.commit()?;
```

### Predicate filtering

```rust
use rocksgraph::{gt, within, TraversalBuilder, Value};

// Scalar filter after values()
let older = snap.g().V([]).values(["age"]).is(gt(30i32)).to_list()?;

// has() with explicit predicates
let result = snap.g().V([]).has("age", gt(25i32)).values(["name"]).to_list()?;

// within() for multi-value membership
let result = snap.g().V([]).has("name", within(["alice", "bob"])).count().next()?.unwrap();
```

### Idempotent upserts with coalesce

```rust
use rocksgraph::{Graph, TraversalBuilder, StoreError, __};

let graph = Graph::open("./path/to/db")?;
let mut tx = graph.begin();

// Upsert vertex: return existing id or create new
tx.g()
    .coalesce([
        __().V([42]).values(["name"]),              // branch 1: vertex exists → emit name
        __().addV("person")                          // branch 2: create it
            .property("id", 42i64)
            .property("name", "charlie"),
    ])
    .next()?;

// Upsert edge: check for existing or create
tx.g()
    .V([42])
    .coalesce([
        __().outE(["knows"]).r#where(__().otherV().hasId([99])),
        __().addE("knows").from(42).to(99).property("weight", 0.5f64),
    ])
    .next()?;

tx.commit()?;
```

### Anonymous sub-traversals with `__()`

`__()` creates a context-free traversal used as an argument to `where`, `coalesce`, and `union`. The type is `#[doc(hidden)]`; you never need to name it:

```rust
// where: filter edges whose other endpoint matches a condition
snap.g().V([1]).outE(["knows"]).r#where(__().otherV().hasLabel(["person"])).count().next()?;

// union: merge results from multiple branches
snap.g().V([1]).union([__().outE(["knows"]), __().outE(["created"])]).count().next()?;

// coalesce: first non-empty branch
tx.g().coalesce([__().V([id]).values(["name"]), __().addV("person").property("name", "x")]).next()?;
```

### Multiple queries per session

`g()` returns a temporary traversal that borrows the session for exactly one statement. Once the statement ends, the borrow is released and you can call `g()` again:

```rust
let mut tx = graph.begin();

// Each call to g() is an independent query against the same transaction.
tx.g().addV("person").property("id", 1i64).property("name", "alice").next()?;
tx.g().addV("person").property("id", 2i64).property("name", "bob").next()?;
// alice and bob are both visible here (read-your-writes)
let count = tx.g().V([]).count().next()?.unwrap();

tx.commit()?; // both writes flushed atomically
```

## Schema Modes

Vertex labels, edge labels, and property keys are interned to compact numeric IDs internally
by the `schema` module. How that registration happens is controlled by `SchemaMode`, set via
`Graph::open_with_options` (it sticks for the lifetime of the on-disk database — reopening an
existing database ignores the options passed and uses whatever was persisted):

- **`SchemaMode::Auto`** (the default, used by `Graph::open`) — a label or property key is
  registered the first time a traversal uses it. This is the mode every example above uses;
  there is nothing extra to do.
- **`SchemaMode::Strict`** — nothing is registered implicitly. Every vertex label, edge label,
  and property key must be declared up front via `Graph::open_management()`, or the write
  fails with `StoreError::SchemaViolation`.

```rust
use rocksgraph::{schema::{DataType, GraphOptions, SchemaMode}, Graph, StoreError};

let options = GraphOptions { mode: SchemaMode::Strict, ..Default::default() };
let graph = Graph::open_with_options("./path/to/db", options)?;

// Declare the schema before any write reaches the engine.
let mut mgmt = graph.open_management();
mgmt.make_vertex_label("person").make();
mgmt.make_property_key("name", DataType::String).make();
mgmt.commit()?;

let mut tx = graph.begin();
tx.g().addV("person").property("id", 1i64).property("name", "alice").next()?; // Ok
tx.commit()?;

let mut tx = graph.begin();
let err = tx.g().addV("ghost").property("id", 2i64).next().unwrap_err(); // undeclared label
assert!(matches!(err, StoreError::SchemaViolation(_)));
```

`SchemaManagement::commit()` is atomic and CAS-checked against concurrent schema changes: either
every staged label/key in the batch is applied, or none are. See the [`SchemaManagement`
rustdoc](src/schema/management.rs) for the full guarantees, and `set_edge_mode` /
`set_schema_mode` for changing graph-wide options (e.g. enabling multi-edges) after creation.

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
- **Schema ID space limits:** up to 4096 distinct vertex labels and 4096 distinct edge labels (independent namespaces), and ~65k property keys per graph — registering past that fails with `StoreError::SchemaExhausted`.
- **Not TinkerPop-compatible:** RocksGraph is Gremlin-inspired but intentionally departs from the standard. See [docs/design_principles.md](docs/design_principles.md).
- **No distributed backend:** placeholder exists but is not implemented.

## Roadmap

### Engine & Query

- [ ] Improve TinkerPop Gremlin step coverage (lambdas, side-effects, additional aggregation steps)
- [ ] Support bulk-load mode: offline SST file generation + direct RocksDB SST ingestion for high-throughput initial loads
- [x] `ReadSession` / `ReadTraversal` — read-only snapshot path with no OCC overhead
- [x] `next(), to_list(), iter()` on `ReadTraversal` and `WriteTraversal`
- [x] Support strict schema mode (see [Schema Modes](#schema-modes))
- [ ] Range predicates in `HasPropertyStep` (`Gt`, `Lt`, `Between`, etc.)

### Storage & Distribution

- [ ] Support distributed key-value backend (e.g. FoundationDB)
- [ ] Server-client mode (gRPC or WebSocket)

### Developer Experience

- [ ] Publish as a public crate on crates.io
- [ ] GitHub Pages rustdoc site

## License

RocksGraph is free software: you can redistribute it and/or modify it under the terms of the
[GNU General Public License v2.0](https://www.gnu.org/licenses/old-licenses/gpl-2.0.html)
or (at your option) any later version.

Copyright © 2026 Austin Han.
