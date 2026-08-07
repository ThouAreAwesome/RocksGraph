# RocksGraph

[![CI](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml/badge.svg)](https://github.com/ThouAreAwesome/RocksGraph/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/rocksgraph.svg)](https://pypi.org/project/rocksgraph/)

**An embeddable property graph database with Gremlin traversals and vector search.**
Open a graph with one line of code, traverse it by relationship, and search it by
vector similarity — no server, no cluster, no JVM.

```bash
pip install rocksgraph
```

**Early stage, production-curious.** Beta (v0.2.0).

**What's solid:**
- Gremlin-style traversal API with query optimizer
- Property graph model (vertices, edges, typed properties, labels)
- ACID transactions (OCC, auto-rollback on exception)
- HNSW vector search (810+ tests, WAL crash recovery, RYOW isolation)

**What's not:**
- No distributed or cluster mode (and won't have one)
- No SQL/GQL query language — use the traversal API
- Not yet fuzzed or Jepsen-tested

**Who should use it:**
- Building a local-first Python application that needs graph traversal + vector similarity
- Running RAG on edge devices or embedded systems
- Want ACID without managing a database server

**Who shouldn't:**
- Need horizontal scaling → use Neo4j or Dgraph
- Need a SQL or GQL query language → use PostgreSQL + pgvector or SurrealDB

**Maintenance:** Actively maintained. Issues responded to within a week. Releases when there's something worth shipping. If I stop, I'll say so here.

## Quickstart

```python
from rocksgraph import Graph, Int64, Vector
import tempfile

graph = Graph(tempfile.mkdtemp())

# ── Write — ordinary properties + embedding in one pass ─
with graph.begin() as txn:
    txn.g().addV("person").property("id", 1).property("name", "Alice").property("emb", Vector([1.0, 0.0, 0.0])).next()
    txn.g().addV("person").property("id", 2).property("name", "Bob").property("emb", Vector([0.0, 1.0, 0.0])).next()
    txn.g().addE("knows").from_(1).to(2).next()

snap = graph.read()

# ── Graph traversal ────────────────────────────────────
snap.g().V(1).out("knows").values("name").to_list()       # ["Bob"]

# ── Vector search on the same vertices ─────────────────
snap.g().V().nearest("emb", Vector([0.9, 0.1, 0.0]), 1).to_list()  # [Vertex(id=1, label="person", ...)]
```

## Session Model

```
Graph(path)
  ├─ .begin()            → TxnSession      (OCC read-write transaction)
  ├─ .read()             → ReadSession    (pinned snapshot, immutable)
  ├─ .open_schema()      → SchemaSession  (DDL — declare labels, indexes)
  ├─ .open_bulk_loader() → BulkLoader   (high-throughput batch ingest)
  └─ .index_manager()    → IndexManager  (rebuild / save vector indexes)

session.g() → GraphTraversal  (immutable step builder)
  .next()     → single result or None
  .to_list()  → list of all results
  .to_set()   → set of all results (elements must be hashable)
  .iterate()  → None — execute for side-effects, discard results
```

`Graph` is cheap to clone internally; create one `Graph` instance and share it.
Sessions are single-threaded — create one per thread.

`TxnSession` supports the context manager protocol — auto-commits on success,
auto-rolls-back on any exception:

```python
with graph.begin() as txn:
    txn.g().addV("person").property("id", 1).property("name", "Alice").next()
# committed automatically

with graph.begin() as txn:
    txn.g().addV("person").property("id", 2).next()
    raise ValueError("oops")   # rolled back automatically; exception still propagates
```

`.g()` returns a fresh `GraphTraversal` per call — each method returns a **new** traversal object and nothing executes until `.next()`, `.to_list()`, `.to_set()`, or `.iterate()`:

```python
snap = graph.read()
snap.g().V().count().to_list()       # query 1 → [2]
snap.g().V(1).out("knows").to_list() # query 2, independent, same snapshot
```

## Usage

Examples assume `from rocksgraph import Graph, P, __, Int64, T, Direction, Order` and `graph = Graph(path)`. See [Step Reference](#step-reference) for the full step catalogue.

### Property filtering with predicates

```python
snap = graph.read()

# Age > 30 → list of name strings
snap.g().V().has("age", P.gt(Int64(30))).values("name").to_list()

# Age between 20 and 40 (inclusive lo, exclusive hi)
snap.g().V().has("age", P.between(Int64(20), Int64(40))).values("name").to_list()

# Name is Alice → list of Vertex objects
snap.g().V().has("name", "Alice").to_list()

# Property existence → count as Int64
snap.g().V().has("email").count().to_list()
```

### Edge creation

```python
with graph.begin() as txn:
    # next() returns a Vertex object; from() / to() extract its .id automatically
    alice = txn.g().addV("person").property("id", 1).property("name", "Alice").next()
    bob   = txn.g().addV("person").property("id", 2).property("name", "Bob").next()
    txn.g().addE("knows").from_(alice).to(bob).property("since", Int64(2020)).next()
```

### Sub-traversal filtering

```python
# Vertices that have at least one "knows" edge pointing to vertex 2
snap = graph.read()
snap.g().V().where(__.out("knows").hasId(2)).values("name").to_list()
```

### Selective property loading

`withProperties()` controls which properties are populated on the returned `Vertex`/`Edge`
objects. Without it, `.properties` is always `{}` — use `.values()` / `.properties()` steps instead.

```python
snap = graph.read()

# Default — .properties is empty; use .values() to read
v = snap.g().V(1).next()
v.properties  # {}
snap.g().V(1).values("name").to_list()  # ["Alice"]

# Load specific properties into the object
v = snap.g().withProperties("name", "age").V(1).next()
v.properties  # {"name": "Alice", "age": 30}

# Load all properties
v = snap.g().withProperties().V(1).next()
```

### Ordering with enums

```python
snap = graph.read()

# Sort ascending / descending using Order enum
snap.g().V().order().by("age", Order.asc).values("name").to_list()
snap.g().V().order().by("age", Order.desc).values("name").to_list()

# Multi-key sort
snap.g().V().order().by("city", Order.asc).by("name", Order.asc).values("name").to_list()
```

### Side-effect traversals with `iterate()`

```python
# drop() discards the removed elements — use iterate() to avoid materialising them
with graph.begin() as txn:
    txn.g().V().hasLabel("temp").drop().iterate()
```

### Coalesce (upsert)

```python
with graph.begin() as txn:
    txn.g().V().has("email", "a@b.com").fold().coalesce(
        __.unfold(),
        __.addV("user").property("id", 99).property("email", "a@b.com")
    ).next()
```

### Repeat (loop)

```python
# 2-hop neighbours via "link" edges
snap = graph.read()
snap.g().V(1).repeat(__.out("link")).times(2).values("name").to_list()
```

### Transactions

```python
# Context manager — recommended
with graph.begin() as txn:
    txn.g().addV("person").property("id", 1).property("name", "Alice").next()
# auto-committed

# Manual commit / rollback
txn = graph.begin()
txn.g().addV("person").property("id", 2).property("name", "Bob").next()
txn.rollback()  # discard

graph.read().g().V().hasLabel("person").count().to_list()  # [1] — only Alice
```

### Vector search

```python
from rocksgraph import Graph, DataType, VectorEntityType, DistanceMetric, Vector
import tempfile

graph = Graph(tempfile.mkdtemp())

# 1. Declare the vector index (only needed once; persisted with the database)
with graph.open_schema() as s:
    s.add_property_key("emb", DataType.FloatVector)
    s.add_vector_index(
        property="emb",
        dimension=3,
        entity_type=VectorEntityType.Vertex,
        metric=DistanceMetric.Cosine,
    )

# 2. Insert vertices with embeddings
with graph.begin() as txn:
    txn.g().addV("doc").property("id", 1).property("emb", Vector([1.0, 0.0, 0.0])).next()
    txn.g().addV("doc").property("id", 2).property("emb", Vector([0.0, 1.0, 0.0])).next()
    txn.g().addV("doc").property("id", 3).property("emb", Vector([0.0, 0.0, 1.0])).next()

# 3. Top-k nearest neighbours and point similarity score
snap = graph.read()
results = snap.g().V().nearest("emb", Vector([0.9, 0.1, 0.0]), 2).to_list()
# → [Vertex(id=1, ...), Vertex(id=2, ...)]
score = snap.g().V(1).similarity("emb", Vector([0.9, 0.1, 0.0]), DistanceMetric.Cosine).next()
# → 0.994...
```

## Data Model

`next()` and `to_list()` return typed Python objects. Properties are **single-valued** — one value per key.

### `Vertex`

```python
v = snap.g().V(1).next()
v.id          # int
v.label       # str
v.properties  # dict — always {} unless withProperties() was used

v["id"]       # dict-style access
"id" in v     # True
```

Vertices are **hashable** by `id`.

### `Edge`

```python
e = snap.g().V(1).outE("knows").next()
e.src    # int — source vertex id
e.dst    # int — destination vertex id
e.label  # str
e.rank   # int — default 0
e.properties  # dict — always {} unless withProperties() was used

e["src"]  # dict-style access
```

Edges are **hashable** by `(src, dst, label, rank)`.

### `Property`

```python
props = snap.g().V(1).properties("name").to_list()
p = props[0]
p.key    # str — "name"
p.value  # Any — "Alice"

p["key"]  # dict-style access
```

### `Path`

`path()` returns `{"objects": [Vertex|Edge|...], "labels": [[str]]}`.

## Type System

Python `int` and `float` auto-convert to `Int64` / `Float64`. Use typed wrappers for precision control:

| Wrapper | Rust equivalent | Python input |
|---------|----------------|-------------|
| `Int32(42)` | `i32` | `int` |
| `Int64(42)` | `i64` | `int` |
| `UInt16(5)` | `u16` | `int` |
| `Float32(3.14)` | `f32` | `float` |
| `Float64(1e300)` | `f64` | `float` |
| `Uuid("550e8400-...")` | `Uuid` | `str` |
| `Vector([1.0, 0.5, ...])` | `FloatVector(Vec<f32>)` | `list[float]` |
| raw `int` / `float` | → `Int64` / `Float64` | auto |

## Enums & Predicates

```python
from rocksgraph import T, Direction, Order, P
```

| Enum | Values | Typical use |
|------|--------|-------------|
| `T` | `T.id`, `T.label`, `T.key`, `T.value` | `order().by(T.id)` |
| `Direction` | `Direction.OUT`, `Direction.IN`, `Direction.BOTH` | `degree(Direction.OUT)` |
| `Order` | `Order.asc`, `Order.desc` | `order().by("age", Order.asc)` |

**Predicates** — used in `.has(key, P.pred)`, `.hasId(P.pred)`, `.is_(P.pred)`:

| Predicate | Meaning |
|-----------|---------|
| `P.eq(v)` / `P.neq(v)` | equal / not equal |
| `P.gt(v)` / `P.gte(v)` | greater than / greater than or equal |
| `P.lt(v)` / `P.lte(v)` | less than / less than or equal |
| `P.between(lo, hi)` | `lo ≤ x < hi` |
| `P.within(*vs)` / `P.without(*vs)` | membership / exclusion |

## Step Reference

### Start

| Step | Builder | Returns |
|------|---------|---------|
| `V(*ids)` | `.V()` or `.V(1, 2)` | Vertex |
| `E(*labels)` | `.E()` or `.E("knows")` — query edges by label | Edge |
| `addV(label)` | `.addV("person").property("id", 1)` | Vertex |
| `addE(label)` | `.addE("knows").from_(v1).to(v2)` | Edge |

### Traversal

| Step | Builder | Returns |
|------|---------|---------|
| `out(*labels)` | `.out("knows")` | Vertex |
| `in_(*labels)` | `.in_("knows")` | Vertex |
| `both(*labels)` | `.both("knows")` | Vertex |
| `outE(*labels)` | `.outE("knows")` | Edge |
| `inE(*labels)` | `.inE("knows")` | Edge |
| `bothE(*labels)` | `.bothE("knows")` | Edge |
| `outV()` | `.outV()` | Vertex (from Edge src) |
| `inV()` | `.inV()` | Vertex (from Edge dst) |
| `otherV()` | `.outE().otherV()` — edge traversers only | Vertex |

### Filter

Filters do not change the traverser type — they pass through whatever they receive.

| Step | Builder | Returns |
|------|---------|---------|
| `has(key)` | `.has("email")` — existence check | same as input |
| `has(key, value)` | `.has("age", 30)` — shorthand for `eq` | same as input |
| `has(key, P.pred(...))` | `.has("age", P.gt(Int64(30)))` | same as input |
| `has(label, key, value)` | `.has("person", "name", "Alice")` | same as input |
| `hasLabel(*labels)` | `.hasLabel("person")` | same as input |
| `hasId(value)` | `.hasId(1)` or `.hasId([1, 2, 3])` or `.hasId(P.gt(0))` | same as input |
| `hasRank(value)` | `.hasRank(P.eq(1))` — edge only | same as input |
| `is_(value)` | `.is_("Alice")` or `.is_(P.gt(Int64(18)))` — scalar filter | same as input |

### Pagination

| Step | Builder | Returns |
|------|---------|---------|
| `limit(n)` | `.limit(10)` | same as input |
| `range(lo, hi)` | `.range(0, 20)` | same as input |
| `skip(n)` | `.skip(5)` | same as input |
| `tail(n)` | `.tail(3)` | same as input |

### Order

| Step | Builder | Returns |
|------|---------|---------|
| `order()` | `.order()` | same as input |
| `order().by(key, order)` | `.order().by("age", "asc")` or `.order().by("age", Order.desc)` | same as input |
| `order().by(k1).by(k2)` | `.order().by("city", Order.asc).by("name", Order.desc)` | same as input |

### Aggregation

| Step | Builder | Returns |
|------|---------|---------|
| `count()` | `.count()` | Int64 |
| `fold()` | `.fold()` | List |
| `unfold()` | `.unfold()` | element (flattens List) |
| `sum()` / `max()` / `min()` / `mean()` | `.sum()`, `.max()`, `.min()`, `.mean()` | numeric |
| `dedup()` | `.dedup()` | same as input |
| `degree([dir])` | `.degree()` / `.degree(Direction.OUT)` / `.degree("in")` | Int64 |

### Group

| Step | Builder | Returns |
|------|---------|---------|
| `group()` | groups by traverser value | Map |
| `group().by(key)` | `.group().by("city")` — groups vertices/edges by property | Map |
| `groupCount()` | counts occurrences by traverser value | Map |
| `groupCount().by(key)` | `.groupCount().by("city")` — counts by property | Map |

### Path

| Step | Builder | Returns |
|------|---------|---------|
| `path()` | `.path()` | Path |
| `simplePath()` | `.simplePath()` | filter (same as input) |
| `cyclicPath()` | `.cyclicPath()` | filter (same as input) |

### Sub-traversal

| Step | Builder | Returns |
|------|---------|---------|
| `where(t)` | `.where(__.out("knows"))` — keep only vertices with a knows edge | filter (same as input) |
| `coalesce(t1, t2, ...)` | `.coalesce(__.unfold(), __.addV(...).property("id", 99))` | first non-empty branch |
| `union(t1, t2, ...)` | `.union(t1, t2)` | merged output |
| `and_(t1, ...)` / `or_(t1, ...)` | `.and_(t1, t2)` / `.or_(t1, t2)` | filter (same as input) |
| `not_(t)` | `.not_(__.has("age", P.lt(Int64(18))))` | filter (same as input) |
| `repeat(t).times(n)` | `.repeat(__.out("knows")).times(3)` | loop body output |
| `emit()` | `.emit()` — emit intermediate results during repeat | loop body output |
| `choose(pred, true, false)` | `.choose(pred_t, true_t, false_t)` | true or false branch |
| `local(t)` | `.local(__.out("knows"))` | sub-traversal output |

### Labels & Identity

| Step | Builder | Returns |
|------|---------|---------|
| `as_(*labels)` | `.as_("x")` | same as input |
| `select(*labels)` | `.select("x")` | labelled value |
| `id()` | `.id()` | Int64 (vertex) or str (edge) |
| `label()` | `.label()` | str |
| `rank()` | `.rank()` | int (edge only) |
| `identity()` | `.identity()` | same as input |
| `constant(value)` | `.constant(42)` | given value |

### Vector Search

| Step | Builder | Returns |
|------|---------|---------|
| `nearest(prop, query, k)` | `.nearest("emb", Vector([0.1, 0.9]), 5)` | Vertex/Edge — from the upstream traverser stream, returns the k most similar to query (an explicit vector you supply), ordered by the index's configured `DistanceMetric`; falls back to cosine when no index is available |
| `similarity(prop, query, metric)` | `.similarity("emb", Vector([0.1, 0.9]), DistanceMetric.Cosine)` | `float` — similarity score between the current traverser's prop embedding and query using the given `metric` (Cosine ∈ [-1, 1], DotProduct = raw dot product, Euclidean = 1 − L2²). Does not require a vector index. |
| `neighbors(source_prop, target_prop, k, entity_type)` | `.neighbors("q_emb", "a_emb", 5, VectorEntityType.Vertex)` | Flat-map: reads `source_prop` from each traverser as the query vector and searches the `target_prop` HNSW index of `entity_type`, emitting up to `k` nearest results. `source_prop` and `target_prop` may differ for cross-index similarity. Requires a declared HNSW index. |
| `with_metric(metric)` | `.nearest(…).with_metric(DistanceMetric.Euclidean)` | Overrides the distance metric for the immediately preceding `nearest()` step. Not applicable after `similarity()` (metric is a required parameter there) or `neighbors()` (index metric is fixed at build time). |
| `with_ef_search(ef)` | `.nearest(…).with_ef_search(100)` | Overrides the HNSW beam width for the immediately preceding `nearest()` or `neighbors()` step. Higher values improve recall at the cost of latency. |

`Vector([f32, ...])` is the query type. A plain `list[float]` is auto-coerced. For a complete setup example including index declaration, see [Vector search](#vector-search).

### Extraction

| Step | Builder | Returns |
|------|---------|---------|
| `values(*keys)` | `.values("name", "age")` | scalar — flat list of values |
| `properties(*keys)` | `.properties("name")` | Property object |

### Mutation

| Step | Builder | Returns |
|------|---------|---------|
| `property(key, value)` | `.property("name", "Alice")` | same as input (chainable) |
| `drop()` | `.drop()` | same as input |

### Terminal

| Method | Returns |
|--------|---------|
| `.next()` | single value or `None` |
| `.to_list()` / `.toList()` | `list` of all results |
| `.to_set()` / `.toSet()` | `set` of all results (elements must be hashable) |
| `.iterate()` | `None` — executes traversal, discards results |

## Known Limitations

- **`group()` / `groupCount()` without `by()`** — when traversers are vertices or
  edges and no `.by(key)` is specified, a `TypeError` is raised because the Rust FFI
  represents vertices as Python dicts, which cannot be used as dict keys. Always pair
  `group()` with a `.by("property_key")` when grouping vertex or edge traversers.
- **Edge rank values > 0** require multi-edge engine support (v0.2). Default rank is 0;
  at most one edge per label between any two vertices is supported.
- **`addV()` requires explicit `property("id", n)`** — no auto-increment vertex IDs.
- **Embedded only** — no server/client mode. Queries run in-process.

## API Stability

RocksGraph follows [semver](https://semver.org/) for PyPI versioning.

### Stable surface (no breaking changes within `0.x`)

| Item | Stability |
|------|-----------|
| `Graph(path)`, `Graph.read()`, `Graph.begin()`, `Graph.open_schema()`, `Graph.open_bulk_loader()` | Stable |
| All traversal step methods (`.V()`, `.out()`, `.has()`, `.nearest()`, etc.) | Stable |
| Terminal methods: `.next()`, `.to_list()`, `.to_set()`, `.iterate()` | Stable |
| `Vertex`, `Edge`, `Property`, `Vector` types | Stable |
| `P`, `T`, `Direction`, `Order` enums | Stable |
| `DataType`, `GraphOptions`, `SchemaMode`, `EdgeMode` | Stable |

### Provisional surface (may change in a future `0.x`)

| Item | Status |
|------|--------|
| `VectorIndexConfig`, `DistanceMetric` | Provisional — v0.3 may extend these |
| `IndexManager` (`rebuild`, `save`, `save_all`) | Provisional — export/import planned for v0.3 |

### Version contract

| Version | API | On-disk format |
|---------|-----|----------------|
| 0.1.0 | Superseded — upgrade to 0.2.0, no migration needed | Stable for graph data |
| 0.2.x | Core graph API stable. Vector search (`nearest`, `similarity`) stable. `VectorIndexConfig`, `DistanceMetric`, and `IndexManager` provisional — v0.3 may tune performance characteristics and extend the management API | Stable for graph data; vector WAL format may change |
| 0.4.x | (planned) All APIs stable including vector config and management | Frozen |
| 1.0.0 | (planned) Full semver guarantees — no breaking changes without a major bump | Frozen |

## Platform Support

| Platform | Architecture | Python |
|----------|-------------|--------|
| macOS | ARM64, x86_64 | 3.9+ |
| Linux | x86_64, ARM64 | 3.9+ |
| Windows | x86_64 | 3.9+ |

## License

RocksGraph is dual-licensed under **MIT** or **Apache-2.0**.

GitHub: [github.com/ThouAreAwesome/RocksGraph](https://github.com/ThouAreAwesome/RocksGraph)
