# RocksGraph

**An embeddable Gremlin-style graph database for Python.** Open a graph with one
line of code, traverse it with zero infrastructure — like SQLite for property graphs.

No server. No cluster. No JVM. Just `pip install rocksgraph`.

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
tx = graph.tx()
alice = tx.traversal().addV("person").property("id", 1) \
    .property("name", "Alice").property("age", Int64(30)).next()
bob = tx.traversal().addV("person").property("id", 2) \
    .property("name", "Bob").property("age", Int64(25)).next()
tx.traversal().addE("knows").from_(alice).to(bob).property("since", Int64(2020)).next()
tx.commit()

# ── Read ───────────────────────────────────────────────
snap = graph.read()
snap.traversal().V().count().to_list()                            # [2]
snap.traversal().V(1).out("knows").values("name").to_list()       # ["Bob"]
snap.traversal().V().has("age", P.gt(Int64(28))).values("name").to_list()  # ["Alice"]
snap.traversal().V().hasLabel("person").order().by("age", "asc").values("name").to_list() # ["Bob", "Alice"]
```

## Data Model

```
Vertex:  {"id": int, "label": str, "properties": {str: value}}
Edge:    {"src": int, "dst": int, "label": str, "rank": int, "properties": {str: value}}
```

- Properties are single-valued per key: `{"name": "Alice", "age": 30}`.
  Use `.values("key")` to read them, `.properties("key")` for key+value metadata.
- Every vertex must have an explicit `property("id", n)` — no auto-increment id.
- Edge `rank` defaults to 0; non-zero ranks require multi-edge engine support (v0.2).
- Vertex/Edge dicts from `next()` / `to_list()` always have `properties: {}`.
  Call `.values("key")` or `.properties("key")` on the traversal to read property data.
- `.path()` returns `{"objects": [...], "labels": [[str]]}` — the sequence of
  elements traversed. Objects can be Vertex, Edge, Property, or scalar depending
  on the traversal steps.

## Session Model

```
Graph(path)
  ├─ .tx()     → TxSession   (read-write, commit/rollback)
  └─ .read()   → ReadSession (pinned snapshot, immutable)

Session.traversal() → GraphTraversal  (immutable step builder)
  .next()             → single result or None
  .to_list()          → list of all results
```

`Graph` is cheap to clone internally; create one `Graph` instance and share it. Sessions are single-threaded — create one per thread.

### Using `.traversal()`

`.traversal()` returns a fresh `GraphTraversal` tied to the current session.
Each method (`.V()`, `.out()`, `.has()`, …) returns a **new** traversal object —
the original is never mutated. Nothing executes until you call `.next()` or
`.to_list()`. Call `.traversal()` again anytime for a new query.

```python
# Read session — pinned snapshot, no writes allowed
snap = graph.read()
snap.traversal().V().count().to_list()             # query 1 → [2]
snap.traversal().V(1).out("knows").to_list()       # query 2, independent

# Write session — supports addV / addE / drop / property
tx = graph.tx()
alice = tx.traversal().addV("person").property("id", 1).property("name", "Alice").next()
bob   = tx.traversal().addV("person").property("id", 2).property("name", "Bob").next()
tx.traversal().addE("knows").from_(alice).to(bob).next()
tx.commit()   # all writes become visible atomically; tx is unusable after this
```
```

## Type System

Python `int` and `float` auto-convert to `Int64` / `Float64`. Use typed wrappers for precision control:

| Wrapper                | Rust equivalent       | Python input |
| ---------------------- | --------------------- | ------------ |
| `Int32(42)`            | `i32`                 | `int`        |
| `Int64(42)`            | `i64`                 | `int`        |
| `UInt16(5)`            | `u16`                 | `int`        |
| `Float32(3.14)`        | `f32`                 | `float`      |
| `Float64(1e300)`       | `f64`                 | `float`      |
| `Uuid("550e8400-...")` | `Uuid`                | `str`        |
| raw `int` / `float`    | → `Int64` / `Float64` | auto         |

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

Examples assume `from rocksgraph import Graph, P, __, Int64` and `graph = Graph(path)`.

### Property filtering with predicates

```python
snap = graph.read()

# Age > 30 → list of name strings, e.g. ["Alice"]
snap.traversal().V().has("age", P.gt(Int64(30))).values("name").to_list()

# Age between 20 and 40 (inclusive lo, exclusive hi)
snap.traversal().V().has("age", P.between(Int64(20), Int64(40))).values("name").to_list()

# Name is Alice → list of Vertex dicts
snap.traversal().V().has("name", "Alice").to_list()

# Property existence → count as Int64, e.g. [3]
snap.traversal().V().has("email").count().to_list()
```

### Edge creation

```python
tx = graph.tx()
# next() returns a vertex dict: {"id": 1, "label": "person", "properties": {}}
alice = tx.traversal().addV("person").property("id", 1).property("name", "Alice").next()
bob   = tx.traversal().addV("person").property("id", 2).property("name", "Bob").next()
# from_(alice) / to(bob) extract the "id" field from the dict — equivalent to from_(1).to(2)
tx.traversal().addE("knows").from_(alice).to(bob).property("since", Int64(2020)).next()
tx.commit()
```

### Sub-traversal filtering

```python
# Vertices that have at least one "knows" edge pointing to vertex 2
snap = graph.read()
snap.traversal().V().where(__.out("knows").hasId(2)).values("name").to_list()
# → ["Alice"] (if Alice has a "knows" edge to vertex 2)
```

### Selective property loading

`withProperties()` controls which properties are fetched when a vertex or edge
materializes. Without it, the result dict has `properties: {}` and you must
call `.values()`/`.properties()` to read them.

```python
snap = graph.read()

# Default — properties dict is empty
v = snap.traversal().V(1).next()
# → {"id": 1, "label": "person", "properties": {}}

# Load only specific properties
v = snap.traversal().withProperties("name", "age").V(1).next()
# → {"id": 1, "label": "person", "properties": {"name": ["Alice"], "age": [30]}}

# withProperties() with no arguments loads all properties
v = snap.traversal().withProperties().V(1).next()
# → {"id": 1, "label": "person", "properties": {"name": ["Alice"], "age": [30], "city": ["NY"]}}
```

### Coalesce (upsert)

```python
# Return existing vertex or create a new one (write transaction required for addV branch)
tx = graph.tx()
tx.traversal().V().has("email", "a@b.com").fold().coalesce(
    __.unfold(),
    __.addV("user").property("id", 99).property("email", "a@b.com")
).next()
# → existing Vertex dict if found, otherwise the newly created one
tx.commit()
```

### Repeat (loop)

```python
# 2-hop neighbours via "link" edges → list of name strings
snap = graph.read()
snap.traversal().V(1).repeat(__.out("link")).times(2).values("name").to_list()
```

### Transactions

```python
tx = graph.tx()
tx.traversal().addV("person").property("id", 1).property("name", "Alice").next()
tx.commit()    # persist

tx = graph.tx()
tx.traversal().addV("person").property("id", 2).property("name", "Bob").next()
tx.rollback()  # discard

graph.read().traversal().V().hasLabel("person").count().to_list()
# → [1] — only Alice
```

## Step Reference

### Start

| Step          | Builder                                        | Returns |
| ------------- | ---------------------------------------------- | ------- |
| `V(*ids)`     | `.V()` or `.V(1, 2)`                           | Vertex  |
| `E(*labels)`  | `.E()` or `.E("knows")` — query edges by label | Edge    |
| `addV(label)` | `.addV("person").property("id", 1)`            | Vertex  |
| `addE(label)` | `.addE("knows").from_(v1).to(v2)`              | Edge    |

### Traversal

| Step             | Builder                                   | Returns                |
| ---------------- | ----------------------------------------- | ---------------------- |
| `out(*labels)`   | `.out("knows")`                           | Vertex                 |
| `in_(*labels)`   | `.in_("knows")`                           | Vertex                 |
| `both(*labels)`  | `.both("knows")`                          | Vertex                 |
| `outE(*labels)`  | `.outE("knows")`                          | Edge                   |
| `inE(*labels)`   | `.inE("knows")`                           | Edge                   |
| `bothE(*labels)` | `.bothE("knows")`                         | Edge                   |
| `outV()`         | `.outV()`                                 | Vertex (from Edge src) |
| `inV()`          | `.inV()`                                  | Vertex (from Edge dst) |
| `otherV()`       | `.outE().otherV()` — edge traversers only | Vertex                 |

### Filter

Filters do not change the traverser type — they pass through whatever they receive.

| Step                     | Builder                                                 | Returns       |
| ------------------------ | ------------------------------------------------------- | ------------- |
| `has(key)`               | `.has("email")` — existence check                       | same as input |
| `has(key, value)`        | `.has("age", 30)` — shorthand for `eq`                  | same as input |
| `has(key, P.pred(...))`  | `.has("age", P.gt(Int64(30)))`                          | same as input |
| `has(label, key, value)` | `.has("person", "name", "Alice")`                       | same as input |
| `hasLabel(*labels)`      | `.hasLabel("person")`                                   | same as input |
| `hasId(value)`           | `.hasId(1)` or `.hasId([1, 2, 3])` or `.hasId(P.gt(0))` | same as input |
| `hasRank(value)`         | `.hasRank(P.eq(1))` — edge only                         | same as input |

### Pagination

| Step            | Builder         | Returns       |
| --------------- | --------------- | ------------- |
| `limit(n)`      | `.limit(10)`    | same as input |
| `range(lo, hi)` | `.range(0, 20)` | same as input |
| `skip(n)`       | `.skip(5)`      | same as input |
| `tail(n)`       | `.tail(3)`      | same as input |

### Order

| Step                    | Builder                                         | Returns       |
| ----------------------- | ----------------------------------------------- | ------------- |
| `order()`               | `.order()`                                      | same as input |
| `order().by(key)`       | `.order().by("age", "asc")`                     | same as input |
| `order().by(k1).by(k2)` | `.order().by("city", "asc").by("name", "desc")` | same as input |

### Aggregation

| Step                                   | Builder                                           | Returns                 |
| -------------------------------------- | ------------------------------------------------- | ----------------------- |
| `count()`                              | `.count()`                                        | Int64                   |
| `fold()`                               | `.fold()`                                         | List                    |
| `unfold()`                             | `.unfold()`                                       | element (flattens List) |
| `sum()` / `max()` / `min()` / `mean()` | `.sum()`, `.max()`, `.min()`, `.mean()`           | numeric                 |
| `dedup()`                              | `.dedup()`                                        | same as input           |
| `degree()`                             | `.degree()` or `.degree("out")` / `.degree("in")` | Int64                   |

### Group ⚠️

**⚠️ Always fails on vertex/edge traversers.** The Rust engine groups by the raw traverser value regardless of any `.by()` modifier, so vertex/edge dicts (unhashable) raise `TypeError`. Workaround: project to a scalar first — `V().values("city").group()` — but this groups city strings by themselves, not vertices by a property key.

| Step                       | Builder                                               | Returns |
| -------------------------- | ----------------------------------------------------- | ------- |
| `group()` / `groupCount()` | groups/counts by traverser value — ⚠️ scalar keys only | Map     |
| `group().by(key)`          | `.group().by("city")` — ⚠️ see Known Limitations       | Map     |
| `groupCount().by(key)`     | `.groupCount().by("city")` — ⚠️ see Known Limitations  | Map     |

### Path

| Step           | Builder         | Returns                |
| -------------- | --------------- | ---------------------- |
| `path()`       | `.path()`       | Path                   |
| `simplePath()` | `.simplePath()` | filter (same as input) |
| `cyclicPath()` | `.cyclicPath()` | filter (same as input) |

### Sub-traversal

| Step                             | Builder                                                          | Returns                |
| -------------------------------- | ---------------------------------------------------------------- | ---------------------- |
| `where(t)`                       | `.where(__.out("knows"))` — keep only vertices with a knows edge | filter (same as input) |
| `coalesce(t1, t2, ...)`          | `.coalesce(__.unfold(), __.addV(...).property("id", 99))`        | first non-empty branch |
| `union(t1, t2, ...)`             | `.union(t1, t2)`                                                 | merged output          |
| `and_(t1, ...)` / `or_(t1, ...)` | `.and_(t1, t2)` / `.or_(t1, t2)`                                 | filter (same as input) |
| `not_(t)`                        | `.not_(__.has("age", P.lt(Int64(18))))`                          | filter (same as input) |
| `repeat(t).times(n)`             | `.repeat(__.out("knows")).times(3)`                              | loop body output       |
| `emit()`                         | `.emit()` — emit intermediate results during repeat              | loop body output       |
| `choose(pred, true, false)`      | `.choose(pred_t, true_t, false_t)`                               | true or false branch   |
| `local(t)`                       | `.local(__.out("knows"))`                                        | sub-traversal output   |

### Labels & Identity

| Step              | Builder         | Returns                      |
| ----------------- | --------------- | ---------------------------- |
| `as_(*labels)`    | `.as_("x")`     | same as input                |
| `select(*labels)` | `.select("x")`  | labelled value               |
| `id()`            | `.id()`         | Int64 (vertex) or str (edge) |
| `label()`         | `.label()`      | str                          |
| `rank()`          | `.rank()`       | int (edge only)              |
| `identity()`      | `.identity()`   | same as input                |
| `constant(value)` | `.constant(42)` | given value                  |

### Extraction

| Step                | Builder                  | Returns                      |
| ------------------- | ------------------------ | ---------------------------- |
| `values(*keys)`     | `.values("name", "age")` | scalar — flat list of values |
| `properties(*keys)` | `.properties("name")`    | Property                     |

### Mutation

| Step                   | Builder                      | Returns                   |
| ---------------------- | ---------------------------- | ------------------------- |
| `property(key, value)` | `.property("name", "Alice")` | same as input (chainable) |
| `drop()`               | `.drop()`                    | same as input             |

### Terminal

| Method                     | Returns                |
| -------------------------- | ---------------------- |
| `.next()`                  | single value or `None` |
| `.to_list()` / `.toList()` | `list` of all results  |

## Known Limitations

- **`group()` / `groupCount()` always fail when vertices or edges are the current
  traversers** — the Rust engine groups by the raw traverser value and ignores the
  `.by(key)` modifier. Vertex and edge dicts are not hashable in Python, so
  `TypeError: unhashable type: 'dict'` is raised regardless of whether `.by()` is
  used. This is a v0.2 item. As a limited workaround, project to scalar values
  *before* grouping: `V().values("city").group()` groups city strings by themselves
  (returns `{"NY": ["NY", "NY"], "SF": ["SF"]}`), but this is not the same semantics
  as grouping vertex objects by a property.
- **Edge rank values > 0** require multi-edge engine support (v0.2). Default rank is 0;
  at most one edge per label between any two vertices is supported.
- **`addV()` requires explicit `property("id", n)`** — no auto-increment vertex IDs.
- **Embedded only** — no server/client mode. Queries run in-process.

## Platform Support

| Platform | Architecture  | Python |
| -------- | ------------- | ------ |
| macOS    | ARM64, x86_64 | 3.9+   |
| Linux    | x86_64, ARM64 | 3.9+   |
| Windows  | x86_64        | 3.9+   |

## Roadmap

Planned for the next release:

- **`explain()`** — print the physical plan tree for a traversal
- **`iter()`** — lazy streaming iterator as an alternative to `to_list()`
- **`is(pred)`** step — filter the current scalar traverser value with a predicate
- **`Graph.open_with_options()`** — control schema mode and edge mode at open time
- **Schema management** — programmatic label and property key registration
- **Performance tuning** — `set_batch_size()`, `clear_caches()`
- **Bulk loading** — `SstBulkLoader` for large initial datasets

## License

RocksGraph is dual-licensed under **MIT** or **Apache-2.0**.

GitHub: [github.com/ThouAreAwesome/RocksGraph](https://github.com/ThouAreAwesome/RocksGraph)
