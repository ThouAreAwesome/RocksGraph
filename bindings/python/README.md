# RocksGraph

**An embeddable property graph database with Gremlin traversals and vector search.**
Open a graph with one line of code, traverse it by relationship, and search it by
vector similarity — no server, no cluster, no JVM. Just `pip install rocksgraph`.

```bash
pip install rocksgraph
```

## Quickstart

```python
from rocksgraph import Graph, P, Int64
import tempfile

db = tempfile.mkdtemp()
graph = Graph(db)

# ── Write ──────────────────────────────────────────────
with graph.tx() as tx:
    alice = tx.traversal().addV("person").property("id", 1) \
        .property("name", "Alice").property("age", Int64(30)).next()
    bob = tx.traversal().addV("person").property("id", 2) \
        .property("name", "Bob").property("age", Int64(25)).next()
    tx.traversal().addE("knows").from_(alice).to(bob).property("since", Int64(2020)).next()

# ── Read ───────────────────────────────────────────────
snap = graph.read()
snap.traversal().V().count().to_list()                            # [2]
snap.traversal().V(1).out("knows").values("name").to_list()       # ["Bob"]
snap.traversal().V().has("age", P.gt(Int64(28))).values("name").to_list()  # ["Alice"]
snap.traversal().V().hasLabel("person").order().by("age", "asc").values("name").to_list()
                                                                  # ["Bob", "Alice"]
```

## Data Model

```
Vertex:   Vertex(id, label, properties={str: value})
Edge:     Edge(src, dst, label, rank, properties={str: value})
Property: Property(key, value)
Path:     {"objects": [Vertex|Edge|...], "labels": [[str]]}
```

- Both vertex and edge properties are **single-valued**: one value per key.
  `{"name": "Alice", "age": 30}`. Use `.values("key")` to read them,
  `.properties("key")` to get `Property` objects.
- Every vertex must have an explicit `property("id", n)` — no auto-increment.
- Edge `rank` defaults to 0; non-zero ranks require multi-edge engine support (v0.2).
- Traversal results (`next()`, `to_list()`) return `Vertex`/`Edge` objects with `.properties`
  always `{}` unless `.withProperties()` was used. Call `.values()` / `.properties()` to
  read property data via the traversal.

## Result Types

`next()` and `to_list()` return typed Python objects, not raw dicts.

### `Vertex`

```python
v = snap.traversal().V(1).next()
v.id          # int
v.label       # str
v.properties  # dict — always {} unless withProperties() was used

# Dict-style access still works (backward compat)
v["id"]       # same as v.id
"id" in v     # True
```

Vertices are **hashable** by `id` — safe to use in `set()` or as dict keys.

### `Edge`

```python
e = snap.traversal().V(1).outE("knows").next()
e.src    # int — source vertex id
e.dst    # int — destination vertex id
e.label  # str
e.rank   # int — 0 in v0.1
e.properties  # dict — always {} unless withProperties() was used

e["src"]  # dict-style access
```

Edges are **hashable** by `(src, dst, label, rank)`.

### `Property`

```python
props = snap.traversal().V(1).properties("name").to_list()
p = props[0]
p.key    # str — "name"
p.value  # Any — "Alice"

p["key"]   # dict-style access
```

## Session Model

```
Graph(path)
  ├─ .tx()     → TxSession   (read-write, commit/rollback)
  └─ .read()   → ReadSession (pinned snapshot, immutable)

Session.traversal() → GraphTraversal  (immutable step builder)
  .next()             → single result or None
  .to_list()          → list of all results
  .to_set()           → set of all results (requires hashable elements)
  .iterate()          → None — execute for side-effects, discard results
```

`Graph` is cheap to clone internally; create one `Graph` instance and share it.
Sessions are single-threaded — create one per thread.

`TxSession` supports the context manager protocol — auto-commits on success,
auto-rolls-back on any exception:

```python
with graph.tx() as tx:
    tx.traversal().addV("person").property("id", 1).property("name", "Alice").next()
# committed automatically

with graph.tx() as tx:
    tx.traversal().addV("person").property("id", 2).next()
    raise ValueError("oops")   # rolled back automatically; exception still propagates
```

### Using `.traversal()`

`.traversal()` returns a fresh `GraphTraversal` tied to the current session.
Each method (`.V()`, `.out()`, `.has()`, …) returns a **new** traversal object —
the original is never mutated. Nothing executes until you call `.next()`,
`.to_list()`, `.to_set()`, or `.iterate()`.

```python
snap = graph.read()
snap.traversal().V().count().to_list()       # query 1 → [2]
snap.traversal().V(1).out("knows").to_list() # query 2, independent, same snapshot
```

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

## Enums

```python
from rocksgraph import T, Direction, Order
```

### `T` — element token keys

| Token | String equivalent | Typical use |
|-------|------------------|-------------|
| `T.id` | `"id"` | `order().by(T.id, "asc")` |
| `T.label` | `"label"` | `order().by(T.label, Order.asc)` |
| `T.key` | `"key"` | Property key reference |
| `T.value` | `"value"` | Property value reference |

### `Direction` — traversal direction

| Token | Use |
|-------|-----|
| `Direction.OUT` | `degree(Direction.OUT)` — out-degree only |
| `Direction.IN` | `degree(Direction.IN)` — in-degree only |
| `Direction.BOTH` | `degree(Direction.BOTH)` — default, both directions |

### `Order` — sort order

| Token | Use |
|-------|-----|
| `Order.asc` | `order().by("age", Order.asc)` |
| `Order.desc` | `order().by("age", Order.desc)` |

## Predicates

```python
from rocksgraph import P

P.eq(v)           # equal
P.neq(v)          # not equal
P.gt(v)           # greater than
P.gte(v)          # greater than or equal
P.lt(v)           # less than
P.lte(v)          # less than or equal
P.between(lo, hi) # lo ≤ x < hi
P.within(*vs)     # x in {vs}
P.without(*vs)    # x not in {vs}
```

All predicates work on user properties (`has("age", P.gt(Int64(30)))`) and vertex IDs (`hasId(P.within(1, 2, 3))`).

## Examples

Examples assume `from rocksgraph import Graph, P, __, Int64, T, Direction, Order` and `graph = Graph(path)`.

### Property filtering with predicates

```python
snap = graph.read()

# Age > 30 → list of name strings
snap.traversal().V().has("age", P.gt(Int64(30))).values("name").to_list()

# Age between 20 and 40 (inclusive lo, exclusive hi)
snap.traversal().V().has("age", P.between(Int64(20), Int64(40))).values("name").to_list()

# Name is Alice → list of Vertex objects
snap.traversal().V().has("name", "Alice").to_list()

# Property existence → count as Int64
snap.traversal().V().has("email").count().to_list()
```

### Edge creation

```python
with graph.tx() as tx:
    # next() returns a Vertex object; from_() / to() extract its .id automatically
    alice = tx.traversal().addV("person").property("id", 1).property("name", "Alice").next()
    bob   = tx.traversal().addV("person").property("id", 2).property("name", "Bob").next()
    tx.traversal().addE("knows").from_(alice).to(bob).property("since", Int64(2020)).next()
```

### Sub-traversal filtering

```python
# Vertices that have at least one "knows" edge pointing to vertex 2
snap = graph.read()
snap.traversal().V().where(__.out("knows").hasId(2)).values("name").to_list()
```

### Selective property loading

`withProperties()` controls which properties are populated on the returned `Vertex`/`Edge`
objects. Without it, `.properties` is always `{}` — use `.values()` / `.properties()` steps instead.

```python
snap = graph.read()

# Default — .properties is empty; use .values() to read
v = snap.traversal().V(1).next()
v.properties  # {}
snap.traversal().V(1).values("name").to_list()  # ["Alice"]

# Load specific properties into the object
v = snap.traversal().withProperties("name", "age").V(1).next()
v.properties  # {"name": "Alice", "age": 30}

# Load all properties
v = snap.traversal().withProperties().V(1).next()
```

### Ordering with enums

```python
snap = graph.read()

# Sort ascending / descending using Order enum
snap.traversal().V().order().by("age", Order.asc).values("name").to_list()
snap.traversal().V().order().by("age", Order.desc).values("name").to_list()

# Multi-key sort
snap.traversal().V().order().by("city", Order.asc).by("name", Order.asc).values("name").to_list()
```

### Degree with Direction enum

```python
snap = graph.read()
snap.traversal().V(1).degree(Direction.OUT).to_list()   # [2] — out-edges only
snap.traversal().V(1).degree(Direction.IN).to_list()    # [1] — in-edges only
snap.traversal().V(1).degree(Direction.BOTH).to_list()  # [3] — all edges
snap.traversal().V(1).degree().to_list()                # [3] — default is BOTH
```

### Side-effect traversals with `iterate()`

```python
# drop() discards the removed elements — use iterate() to avoid materialising them
with graph.tx() as tx:
    tx.traversal().V().hasLabel("temp").drop().iterate()
```

### Coalesce (upsert)

```python
with graph.tx() as tx:
    tx.traversal().V().has("email", "a@b.com").fold().coalesce(
        __.unfold(),
        __.addV("user").property("id", 99).property("email", "a@b.com")
    ).next()
```

### Repeat (loop)

```python
# 2-hop neighbours via "link" edges
snap = graph.read()
snap.traversal().V(1).repeat(__.out("link")).times(2).values("name").to_list()
```

### Transactions

```python
# Context manager — recommended
with graph.tx() as tx:
    tx.traversal().addV("person").property("id", 1).property("name", "Alice").next()
# auto-committed

# Manual commit / rollback
tx = graph.tx()
tx.traversal().addV("person").property("id", 2).property("name", "Bob").next()
tx.rollback()  # discard

graph.read().traversal().V().hasLabel("person").count().to_list()  # [1] — only Alice
```

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
| `nearest(prop, query, k)` | `.nearest("emb", Vector([0.1, 0.9]), 5)` | Vertex/Edge — top-k by cosine similarity, descending |
| `similarity(prop, query)` | `.similarity("emb", Vector([0.1, 0.9]))` | `float` — cosine similarity score |
| `neighbors(prop, k)` | `.neighbors("emb", 5)` | Vertex — k nearest neighbors in vector space |

`Vector([f32, ...])` is the query type. A plain `list[float]` is auto-coerced. Results from `nearest` are ordered by descending similarity.

```python
from rocksgraph import Graph, Vector
import tempfile

graph = Graph(tempfile.mkdtemp())

with graph.tx() as tx:
    tx.traversal().addV("doc").property("id", 1).property("emb", Vector([1.0, 0.0])).next()
    tx.traversal().addV("doc").property("id", 2).property("emb", Vector([0.0, 1.0])).next()

snap = graph.read()
# top-2 nearest to [1.0, 0.0]
results = snap.traversal().V().hasLabel("doc").nearest("emb", Vector([1.0, 0.0]), 2).to_list()
# cosine similarity of vertex 1's embedding
score = snap.traversal().V(1).similarity("emb", Vector([1.0, 0.0])).next()  # 1.0
```

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

## Platform Support

| Platform | Architecture | Python |
|----------|-------------|--------|
| macOS | ARM64, x86_64 | 3.9+ |
| Linux | x86_64, ARM64 | 3.9+ |
| Windows | x86_64 | 3.9+ |

## License

RocksGraph is dual-licensed under **MIT** or **Apache-2.0**.

GitHub: [github.com/ThouAreAwesome/RocksGraph](https://github.com/ThouAreAwesome/RocksGraph)
