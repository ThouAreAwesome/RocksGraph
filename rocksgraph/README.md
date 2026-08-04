# RocksGraph

**An embeddable property graph database with Gremlin traversals and vector search.**
Open a graph with one line of code, traverse it by relationship, and search it by
vector similarity — no server, no cluster, no JVM.

RocksGraph is built for the places where a full graph database server is overkill:
local development, embedded applications, CI pipelines, desktop apps, and single-machine
production deployments. It uses RocksDB for persistent storage and offers a pragmatic Gremlin-style traversal API,
with most core traversal primitives implemented and additional steps being added over time.

> **Status:** Beta (v0.1.0). Under active development. Preparing for release on crates.io.

## Overview

RocksGraph translates [Gremlin](https://tinkerpop.apache.org/gremlin.html)-style traversal queries into a logical IR, optimizes them, and executes them through a pull-based Volcano pipeline against RocksDB. The codebase is organized as a clean, layered stack separating the user-facing session API, query planning, optimization, and execution.

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
| `store` | internal | RocksDB storage layer (`OptimisticTransactionDB`) |
| `schema` | `pub` | Label/property-key registry; `Auto` vs `Strict` schema modes (see [Schema Modes](#schema-modes)) |

## Value Types

All user-facing query inputs and outputs use types from `gremlin::value`, re-exported at the crate root:

| Type | Description |
|------|-------------|
| `Value` | Scalar or composite result: `Null`, `Bool`, `Int32`, `Int64`, `UInt16`, `Float32`, `Float64`, `String`, `Uuid`, `Vertex`, `Edge`, `Property`, `List`, `Map`, `Path`, `FloatVector` |
| `Predicate` | Filter condition: `Predicate::Eq`, `Within`, `Without`, `Gt`, `Gte`, `Lt`, `Lte`, `Between`, `Ne` |
| `Vertex` | Materialized vertex: `id`, `label` (decoded string name), `properties` |
| `Edge` | Materialized edge: `out_v`, `in_v`, `label` (decoded string name), `rank` (`u16`, see `Value::UInt16`), `properties` |
| `Property` | Key-value property element returned by `.properties()` |
| `Map` | Ordered key-value map returned by `.group()` or `.group_count()` |
| `Path` | Sequence of values with per-step labels returned by `.path()` |
| `FloatVector` | Dense f32 vector stored as a vertex/edge property (`Value::FloatVector(Vec<f32>)`). Used with `.nearest()` and `.similarity()` traversal steps. |

Predicate constructors are free functions: `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `between`, `within`, `without`.

### Reserved keys: `id`, `label`, `rank`

`"id"`, `"label"`, and `"rank"` are reserved — `.has()`, `.values()`, and `.properties()`
all reject them. Access them exclusively through dedicated steps, both for filtering and
for extraction:

```rust
// id — HasIdStep / IdStep
.hasId([1, 2, 3])          // filter: Eq (single) or Within (multiple)
.hasId(gt(2i64))           // filter: any Predicate works (vertex ids are ordered i64)
.id()                      // extract: Value::Int64 (vertex) / Value::String (edge)

// label — HasLabelStep / LabelStep
.hasLabel(["person"])      // filter: Eq/Within
.hasLabel(ne("person"))    // filter: eq/ne/within/without (no gt/lt/between — label
                            // names aren't meaningfully ordered)
.label()                   // extract: Value::String, decoded from the schema registry

// rank — HasRankStep / RankStep (edge-only; vertices have no rank)
.hasRank([5u16, 10u16])   // filter: Eq (single), Within (multiple), or any Predicate
.hasRank(ne(5u16))        // filter: eq/ne/within/without/gt/lt/between
.rank()                    // extract: Value::UInt16

// negation for hasId()/hasLabel() goes through not(), same as any other filter:
.not(__().hasId([1, 2]))   // "every vertex except 1 and 2"
```


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
| `in(labels)` | `.r#in([label, ...])` — `in` is a Rust keyword, hence the raw identifier |
| `both(labels)` | `.both([label, ...])` |
| `outE(labels)` | `.outE([label, ...])` |
| `inE(labels)` | `.inE([label, ...])` |
| `bothE(labels)` | `.bothE([label, ...])` |
| `inV()` | `.inV()` |
| `outV()` | `.outV()` |
| `otherV()` | `.otherV()` |

### Filtering

| Step | Method | Notes |
|------|--------|-------|
| `has(key, value)` | `.has(key, pred)` | `key` is a user property name (`&str`) — `"id"`/`"label"`/`"rank"` are rejected, use the steps below |
| `hasLabel(labels)` | `.hasLabel([label, ...])` / `.hasLabel(pred)` | accepts a list (Eq/Within) or any `Predicate` except range predicates |
| `hasId(ids)` | `.hasId([id, ...])` / `.hasId(pred)` | accepts a list (Eq/Within) or any `Predicate` |
| `hasRank(pred)` | `.hasRank(pred)` | edge-only; vertices never match |
| `is(pred)` | `.is(pred)` | filter the current scalar value |
| `where(traversal)` | `.r#where(__().xxx())` | sub-traversal filter |
| `not(traversal)` | `.not(__().xxx())` | negation filter |
| `and(traversals)` | `.and([__().xxx(), __().yyy()])` | passes if every sub-traversal yields a result |
| `or(traversals)` | `.or([__().xxx(), __().yyy()])` | passes if any sub-traversal yields a result |
| `choose(traversal)` | `.choose(pred, true, false?)` | conditional branching |
| `limit(n)` | `.limit(n)` | |
| `range(lo, hi)` | `.range(lo, hi)` | pagination into the result stream |
| `skip(n)` | `.skip(n)` | skip first N results |
| `tail(n)` | `.tail(n)` | keep last N results |
| `dedup()` | `.dedup()` | |
| `order()` | `.order()` | ascending sort on the current value |
| `order().by(key)` | `.order().by("key")` / `.order_by("key", dir)` | sort by a resolved property value; chain `.by(k1).by(k2)` for multi-key tie-breaking |

### Extraction & Aggregation

| Step | Method | Notes |
|------|--------|-------|
| `values(keys)` | `.values([key, ...])` | `key` is a user property name (`&str`) — `"id"`/`"label"`/`"rank"` are rejected, use the steps below |
| `properties(keys)` | `.properties(["key", ...])` | returns `Property` elements; `"id"`/`"label"`/`"rank"` are rejected |
| `id()` | `.id()` | extracts the element id as a scalar (`Int64` for vertices, `String` for edges) |
| `label()` | `.label()` | extracts the element label as a `String` |
| `rank()` | `.rank()` | extracts the edge rank as `UInt16`; errors on a vertex traverser |
| `select(label)` | `.select(label)` | extract a labelled value from the path history |
| `count()` | `.count()` | |
| `sum()` | `.sum()` | numeric sum |
| `mean()` | `.mean()` | numeric mean |
| `max()` | `.max()` | numeric maximum |
| `min()` | `.min()` | numeric minimum |
| `fold()` | `.fold()` | collects all results into a single `Value::List` |
| `unfold()` | `.unfold()` | flattens a list back into individual traversers |
| `group()` | `.group()` | keyed list aggregation into a `Map` |
| `groupCount()` | `.group_count()` | keyed count aggregation into a `Map` |
| `path()` | `.path()` | returns `Value::Path` with per-step labels |
| `withProperties(keys)` | `.withProperties(["key", ...])` | configures which properties are included when a vertex/edge is materialized in the result — `[]` fetches all, named keys fetch only those, omitting the call returns id + label only. Not a mutation; available on both `ReadTraversal` and `WriteTraversal` since either can terminate in a materialized read-back |

### Mutation (WriteTraversal only)

| Step | Method |
|------|--------|
| `addV(label)` | `.addV(label)` |
| `addE(label)` | `.addE(label)` |
| `from(vertex_id)` | `.from(vertex_id)` — optional; if omitted, the upstream traverser's vertex is used as the out-vertex |
| `to(vertex_id)` | `.to(vertex_id)` — optional; if omitted, the upstream traverser's vertex is used as the in-vertex |
| `property(key, value)` | `.property(key, value)` — `"id"` sets the vertex/edge id |
| `drop()` | `.drop()` — drops whatever the traverser carries: a vertex, an edge, or (after `.properties([..])`) a single property key |

### Composition

| Step | Method | Notes |
|------|--------|-------|
| `as(label)` | `.as_(\"label\")` | labels the current traverser for later `select()` |
| `identity()` | `.identity()` | pass-through — emits the traverser unchanged |
| `constant(value)` | `.constant(v)` | replaces every traverser with a fixed value |
| `local(traversal)` | `.local(__().xxx())` | runs the sub-traversal on each traverser and emits all results |
| `repeat(traversal)` | `.repeat(__().xxx())` | loop body |
| `until(traversal)` | `.until(__().xxx())` | loop termination condition |
| `emit()` / `emit_if(traversal)` | `.emit()` / `.emit_if(__().xxx())` | emit intermediate results during repetition |
| `union(traversals)` | `.union([__().xxx(), __().yyy()])` | merges all result streams |
| `coalesce(traversals)` | `.coalesce([__().xxx(), __().yyy()])` | first non-empty branch wins |

### Vector Search

| Step | Method | Notes |
|------|--------|-------|
| `nearest(prop, query, k)` | `.nearest("emb", vec![0.1, 0.9], 5)` | Returns the `k` nearest vertices/edges by cosine similarity on property `prop`, ordered by descending similarity. |
| `similarity(prop, query)` | `.similarity("emb", vec![0.1, 0.9])` | Emits the cosine similarity score (`Float32`) between each traverser's vector property and `query`. |
| `neighbors(prop, k)` | `.neighbors("emb", 5)` | From the current vertex, traverses to its `k` nearest neighbors in vector space. |

### Terminal Operations

| Operation | ReadTraversal | WriteTraversal | Returns |
|-----------|:-------------:|:--------------:|---------|
| `next()` | ✓ | ✓ | `Result<Option<Value>, StoreError>` |
| `to_list()` | ✓ | ✓ | `Result<Vec<Value>, StoreError>` |
| `iter()` | ✓ | ✓ | `Result<BuiltTraversal, StoreError>` — lazy `Iterator<Item = Result<Value, StoreError>>` |
| `explain()` | ✓ | ✓ | `Result<String, StoreError>` — pretty-printed physical plan tree |

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
use rocksgraph::{Graph, TraversalBuilder, Value, __};

let graph = Graph::open("./path/to/db")?;
let mut snap = graph.read();

// Count neighbors of vertex 1 via "knows" edges
let count = snap.g().V([1]).out(["knows"]).count().next()?.unwrap();
assert_eq!(count, Value::Int64(3));

// Fetch property values
let name = snap.g().V([1]).values(["name"]).next()?.unwrap();
assert_eq!(name, Value::String("marko".into()));

// Fetch vertex id and label (decoded to its string name) via the dedicated steps
let id = snap.g().V([1]).id().next()?.unwrap();
let label = snap.g().V([1]).label().next()?.unwrap();

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

### Inspecting query plans

`explain()` builds the physical plan and returns a pretty-printed tree showing
exactly which operators the engine will execute — including optimizer rule
effects like index lookup folding and filter reordering:

```rust
let plan = snap.g().V([1]).out(["knows"]).hasLabel(["person"]).count().explain()?;
println!("{}", plan);
// PhysicalPlan
//   └─ VStep(ids=[1])
//   └─ InOutStep(direction=OUT, labels=[1])
//   └─ HasLabelStep(vertex_pred=Eq(Int32(2)))
//   └─ CountStep()

// See how the optimizer folded hasId into a VStep index seek:
let plan = snap.g().V([]).hasId([1]).outE(["knows"]).where(__().otherV().hasId([2])).explain()?;
println!("{}", plan);
// PhysicalPlan
//   └─ VStep(ids=[1])
//   └─ GetEStep(labels=[...], end_vertex_ids=[2], rank=Some(0))
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

#### Creating edges from a traversal (variable source/target)

`.from()` / `.to()` are only needed when you want a *literal* endpoint. Omit either one
and `addE()` uses the upstream traverser's vertex instead, creating one edge per upstream
traverser — useful for "connect every result of a traversal to a fixed vertex" patterns:

```rust
use rocksgraph::{Graph, TraversalBuilder, StoreError};

let graph = Graph::open("./path/to/db")?;
let mut tx = graph.begin();
# tx.g().addV("person").property("id", 1i64).property("name", "alice").next()?;
# tx.g().addV("person").property("id", 2i64).property("name", "bob").next()?;
# tx.g().addV("person").property("id", 3i64).property("name", "carol").next()?;
# tx.g().addE("knows").from(1).to(2).next()?;
# tx.g().addE("knows").from(1).to(3).next()?;

// For every vertex `1` knows, create a "friends" edge from that vertex to vertex 1 —
// no `.from()` needed; the current traverser (each "knows" target) becomes the out-vertex.
let edges = tx.g().V([1]).out(["knows"]).addE("friends").to(1).property("since", "2020").to_list()?;
assert_eq!(edges.len(), 2); // one edge per upstream traverser

tx.commit()?;
```

`addE()` requires at least one of `.from()` / `.to()` — calling it with neither (and no
upstream vertex producer) returns `StoreError::TraversalError`.

### Deleting elements

`drop()` deletes whatever the traverser carries — a vertex, an edge, or (after `.properties([..])`)
a single property — and is a no-op if the traversal matched nothing:

```rust
use rocksgraph::{Graph, StoreError, TraversalBuilder};

let graph = Graph::open("./path/to/db")?;
let mut tx = graph.begin();

// Drop a single property; other properties on the same vertex/edge are untouched.
tx.g().V([1]).properties(["age"]).drop().next()?;

// Drop an edge.
tx.g().V([1]).outE(["knows"]).drop().next()?;

// A vertex with incident edges can't be dropped directly — drop its edges first.
match tx.g().V([1]).drop().next() {
    Err(StoreError::IncidentEdges) => { /* drop remaining edges, then retry */ }
    other => { other?; }
}

tx.commit()?;
```

### Predicate filtering

`Predicate` has constructors for `eq`, `ne`, `gt`, `gte`, `lt`, `lte`, `between`, `within`, and
`without`:

- **User properties** (`has(key, pred)` where `key` is a `&str`, or `is(pred)` after `values()`):
  every `Predicate` variant is supported.
- **`hasId()`**: every `Predicate` variant is supported (vertex ids are ordered `i64`; edge
  ids are opaque strings, so `gt`/`gte`/`lt`/`lte`/`between` never match an edge but don't
  error either).
- **`hasLabel()`**: `eq`, `ne`, `within`, and `without` are supported; range predicates (`gt`,
  `gte`, `lt`, `lte`, `between`) return `StoreError::UnsupportedOperation` since labels have no
  ordering.

```rust
use rocksgraph::{between, gt, TraversalBuilder};

// Scalar filter after values() — a plain scalar is shorthand for Predicate::Eq
let marko_age = snap.g().V([1]).values(["age"]).is(29i32).to_list()?;

// has() with a plain scalar — also shorthand for Predicate::Eq
let by_name = snap.g().V([]).has("name", "alice").to_list()?;

// Range and membership predicates work on properties and ids
let adults = snap.g().V([]).has("age", gt(18i32)).to_list()?;
let by_age_range = snap.g().V([]).has("age", between(20i32, 30i32)).to_list()?;

// A fixed-size array of values is shorthand for Eq (single)/Within (multiple) —
// hasId()/hasLabel() collapse it the same way has()/is() collapse a bare scalar to Eq
let result = snap.g().V([]).hasId([1, 2, 3]).count().next()?.unwrap();
```

#### Performance of `within` with large lists

- **`V([]).hasId([...])`** — the optimizer folds `HasIdStep` with `Eq` or `Within`
  into `VStep(ids=[...])`, which fetches all vertices in a single batch call
  (`get_vertices`).  Over a read-only snapshot (`graph.read()`) this is a single
  RocksDB `multi_get` round-trip; inside a transaction (`graph.transact()`) it
  is still one batch call but resolves as one point lookup per id under the
  hood, since RocksDB transactional reads have no multi-get equivalent here.

- **`hasId([...])` separated from `V([])` by other filters** — `has()` /
  `hasLabel()` / `where()` steps between `V([])` and `hasId()` get reordered
  and folded automatically, so write order among filters doesn't matter. Only
  a graph-navigation step in between (`out()`, `in()`, `otherV()`, etc.) blocks
  the fold — in that case `hasId` falls back to evaluating the `Within`
  predicate O(n) per traverser.

- **`has("prop", within([...]))`** — property-based `Within` is evaluated
  in-memory O(n) per traverser.  Large lists here incur per-element
  comparison cost.  For ID-based filtering, prefer `hasId()` with an
  explicit ID list over `has("id", within([...]))`.

### Idempotent upserts with coalesce

`coalesce()` only evaluates its branches once per *incoming* traverser — it needs a seed step
ahead of it to have anything to run against. A bare `tx.g().coalesce([...])` with nothing
upstream gets zero traversers and silently does nothing (returns `None`), even if branch 2 would
otherwise create something. `.V([id])` alone isn't a safe seed either: it filters out missing
ids, so if `id` doesn't exist yet it *also* emits zero traversers. `.count()` always emits
exactly one traverser (a count of `0` or `1`) regardless of whether `id` exists, which is what
reliably drives `coalesce()` in the "may or may not exist yet" case:

```rust
use rocksgraph::{Graph, TraversalBuilder, StoreError, __};

let graph = Graph::open("./path/to/db")?;
let mut tx = graph.begin();

// Upsert vertex: return existing name or create new
tx.g()
    .V([42])
    .count()                                          // seed: always exactly one traverser
    .coalesce([
        __().V([42]).values(["name"]),                // branch 1: vertex exists → emit name
        __().addV("person")                           // branch 2: create it
            .property("id", 42i64)
            .property("name", "charlie"),
    ])
    .next()?;

// Upsert edge: check for existing or create. No `.count()` seed needed here — vertex 42 now
// exists (created above, visible via read-your-writes), so `.V([42])` alone already emits one
// traverser.
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

`__()` creates a context-free traversal used as an argument to `where`,
`coalesce`, `union`, `repeat`, `not`, `choose`, and `until`.  The type is
`#[doc(hidden)]` because it's an internal implementation detail — you never
write it by hand.  Import `__` from the crate root:

```rust
use rocksgraph::__;
```

If you see `GraphTraversal` in a compiler error, that's the hidden type
behind `__()`.  The error message is referencing the internal type name, but
your code should only ever interact with it through `__()` — the same way you
pass `|x| x + 1` without naming the closure type.

```rust
// where: filter edges whose other endpoint matches a condition
snap.g().V([1]).outE(["knows"]).r#where(__().otherV().hasLabel(["person"])).count().next()?;

// union: merge results from multiple branches
snap.g().V([1]).union([__().outE(["knows"]), __().outE(["created"])]).count().next()?;

// coalesce: first non-empty branch (needs a `.count()` seed — see "Idempotent upserts" above)
tx.g().V([id]).count().coalesce([__().V([id]).values(["name"]), __().addV("person").property("name", "x")]).next()?;

// repeat: loop body
snap.g().V([1]).repeat(__().out(["knows"])).times(3).explain()?;
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
  and property key must be declared up front via `Graph::open_schema()`, or the write
  fails with `StoreError::SchemaViolation`. (Note: Property key definitions are global/graph-wide; a property key like `"name"` has a single, uniform `DataType` definition effective across the entire graph, rather than being scoped to specific vertex or edge labels.)

```rust
use rocksgraph::{schema::{DataType, GraphOptions, SchemaMode}, Graph, StoreError};

let options = GraphOptions { mode: SchemaMode::Strict, ..Default::default() };
let graph = Graph::open_with_options("./path/to/db", options)?;

// Declare the schema before any write reaches the engine.
let mut mgmt = graph.open_schema();
mgmt.add_vertex_label("person")
    .add_property_key("name", DataType::String);
mgmt.commit()?;

let mut tx = graph.begin();
tx.g().addV("person").property("id", 1i64).property("name", "alice").next()?; // Ok
tx.commit()?;

let mut tx = graph.begin();
let err = tx.g().addV("ghost").property("id", 2i64).next().unwrap_err(); // undeclared label
assert!(matches!(err, StoreError::SchemaViolation(_)));
```

`SchemaSession::commit()` is atomic and CAS-checked against concurrent schema changes: either
every staged label/key in the batch is applied, or none are. See the [`SchemaSession`
rustdoc](src/schema/management.rs) for the full guarantees, and `set_edge_mode` /
`set_schema_mode` for changing graph-wide options (e.g. enabling multi-edges) after creation.

## Bulk Loading

For large initial datasets (millions to billions of edges), use `SstBulkLoader` instead
of the transactional write path.  It generates sorted RocksDB SST files offline and
ingests them atomically — bypassing WAL, memtable pressure, and OCC overhead entirely.

**Measured:** 69 M edges, 4.85 M vertices → ~300K edges/s, ~1.2 GB peak RAM.

```rust
use rocksgraph::{BulkEdge, BulkLoadStats, BulkVertex, Graph};
use std::collections::HashMap;

// Open graph and get a BulkLoader session for atomic ingestion
let graph = Graph::open("path/to/db")?;
let mut loader = graph.open_bulk_loader()?;

let vertices = vec![
    BulkVertex { id: 1, label: "Person".into(), props: HashMap::new() },
    BulkVertex { id: 2, label: "Person".into(), props: HashMap::new() },
];
let edges = vec![
    BulkEdge { src: 1, dst: 2, label: "Knows".into(), props: HashMap::new(), rank: None },
];

loader.load_vertices(vertices)?;
loader.load_edges(edges)?;
let stats: BulkLoadStats = loader.commit()?;

println!("{} vertices, {} edges loaded", stats.vertices_written, stats.edges_written);
// Graph is now ready for reads and traversals
```

**Key properties:**
- `EdgeMode::Single` (default) — rank is always 0; duplicate `(src, label, dst)` → error.
- `EdgeMode::Multi` — supports auto-assigned ranks (`BulkEdge::rank = None`) and explicit
  ranks (`BulkEdge::rank = Some(r)`); `Rank::MAX` (65535) is reserved as sentinel.
- `SchemaMode::Strict` — validates all labels and property keys against `BulkSchema` before
  writing any SST file.
- Crash-safe: a `BULK_LOAD_IN_PROGRESS` marker in the schema CF is auto-cleared on
  `Graph::open` if ingest succeeded, or returns `StoreError::IncompleteLoad` if it didn't.
- Temporary files go in `work_dir` and are cleaned up automatically (RAII guard).

See [`docs/design_bulkload_sst_ingest.md`](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/design_bulkload_sst_ingest.md) for the
full pipeline, memory budget allocation, and format details.

## Development

**Prerequisites:** Rust toolchain 1.80+ (stable), [`just`](https://github.com/casey/just)

The Minimum Supported Rust Version (MSRV) is 1.80. It is bumped
conservatively — only when a dependency or a desired language feature
requires it. The `rust-version` field in `Cargo.toml` tracks the
current floor.

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
| `bench_write.sh` | Bulk-load a SNAP edge-list file into a new database via SST ingestion |
| `bench_read.sh` | Run the read traversal benchmark (Q1–Q9, prints throughput + latency) |
| `bench_integrity.sh` | Verify degree-CF consistency against a full adjacency scan |
| `instruments_read.sh` | Profile read benchmark with macOS Instruments |
| `instruments_write.sh` | Profile write benchmark with macOS Instruments |

```bash
# Build release binaries
just build-release

# Bulk-load the LiveJournal dataset (requires bench_data/soc-LiveJournal1-shuffled.txt)
./scripts/bench_write.sh

# Run read benchmark against the loaded database
./scripts/bench_read.sh

# Verify structural integrity of the loaded database
./scripts/bench_integrity.sh
```

[Current benchmark records](BENCHMARKS.md)

## Safety

RocksGraph's own code contains 5 `unsafe` blocks, all confined to the RocksDB
store layer (`src/store/rocks/`) and all performing the same operation:
`std::mem::transmute` to erase RocksDB transaction and snapshot lifetimes to
`'static`.  This is necessary because `rocksdb::Transaction` and
`rocksdb::Snapshot` borrow the database handle, but our structs own both the
transaction and the `Arc<OptimisticTransactionDB>` — and Rust's borrow checker
can't see that they're heap-allocated together.

The invariant upholding these transmutes is documented in the module-level
comments of `transaction.rs` and `snapshot.rs`: the transaction / snapshot
field is declared *before* the `Arc<DB>` field in every struct, so the DB
handle is dropped first — guaranteeing the borrowed transaction/snapshot never
outlives it.  Every callsite carries a `// SAFETY:` comment referencing this
invariant, and `#![warn(clippy::undocumented_unsafe_blocks)]` (enforced as an
error via `just full-check`'s `--deny warnings`) keeps it that way for any
`unsafe` block added in the future.

The RocksDB dependency (`rust-rocksdb`) wraps a C++ library via FFI and is
widely audited.

## Known Limitations

- **Embedded only:** no server/client mode; queries are executed in-process.
- **Single-threaded per query:** each volcano pipeline runs single-threaded; multiple sessions can run concurrently against a shared `Graph`.
- **Schema ID space limits:** up to `i32::MAX` (~2.1 billion) distinct vertex labels and edge labels (independent namespaces), and 32767 property keys per graph — registering past that fails with `StoreError::SchemaExhausted`. (Label IDs are stored as `i32`; property-key IDs remain `u16`.)
- **Not a TinkerPop driver:** RocksGraph is an embedded library with a Gremlin-style traversal API; it does not implement the Gremlin Server protocol and is not a drop-in replacement for TinkerPop-based systems. See [docs/design_principles.md](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/design_principles.md).
- **Embedded only (no distribution):** RocksGraph targets single-machine deployments. Distributed or server-client operation is not on the roadmap.

## Operations

### Backup & Restore

RocksDB stores all data in the directory passed to `Graph::open()` (or
`Graph::open_with_options()`). To back up:

1. Close the graph: `graph.close()?;` — this is best-effort if other `Graph` clones or open
   sessions still hold a reference; see [`Graph::close`](src/api.rs) for the exact semantics.
2. Copy the entire directory to your backup location.
3. To restore, point `Graph::open()` at a copy of that directory.

This is a cold backup: no writes should be in flight while you copy the directory. For a live
backup without stopping writes, use RocksDB's `Checkpoint` API directly via the raw RocksDB
handle — not yet wrapped by RocksGraph (see [Roadmap](#roadmap)).

### Upgrade & Migration Policy

RocksGraph is pre-1.0 (`0.x.y`). Per semver, **a minor version bump (`0.x` → `0.(x+1)`) may
change the on-disk format** with no schema-version check or automated migration path between
releases. Back up your data directory before upgrading the `rocksgraph` dependency on a project
with existing on-disk data, and validate the upgrade against a copy first.

Once the crate reaches 1.0, on-disk format compatibility will be covered by semver: breaking
format changes will require a major version bump and a documented migration path.

## Roadmap

### Vector Search

- [x] **v0.1** — `FloatVector` property type, brute-force exact KNN (`nearest`, `similarity`), Python bindings
- [ ] **v0.2** — HNSW index via `usearch`; `VectorIndex` trait; WAL + crash-consistent snapshots; per-index memory limits; RYOW isolation (see [design doc](https://github.com/ThouAreAwesome/RocksGraph/blob/main/docs/vector-search/design_vector_v0.2_implementation_plan.md))
- [ ] **v0.3** — Edge vector indexes; `change_vector_index_algorithm`; auto-rebuild on schema change

### Developer Experience

- [ ] Publish as a public crate on crates.io
- [ ] GitHub Pages rustdoc site

## License

RocksGraph is dual-licensed under the terms of both the [MIT License](LICENSE-MIT) and the [Apache License (Version 2.0)](LICENSE-APACHE).

See [LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MIT](LICENSE-MIT) for details.

Copyright © 2026 Austin Han.
