# Gremlin Step Reference

**Target:** RocksGraph v0.2.0+

RocksGraph provides a Gremlin-compatible traversal language executed on top of a streaming, pull-based query engine. All traversals are lazily evaluated stream pipelines, minimizing memory allocations and enabling early termination via `.limit()`.

> [!NOTE]
> Snippets below are excerpts, not full programs — they assume `graph`/`snap`/`txn` are already open as shown in [Getting Started](getting_started.md).

---

## Traversal Anatomy & Execution Model

A traversal pipeline starts from an initial **Source Step** (`g.V()`, `g.E()`, `g.addV()`, `g.addE()`), passes through zero or more **Transform / Filter / Navigation Steps**, and is materialized via terminal collectors:
- **`to_list()` / `.to_list()`**: Drains the entire traversal stream into a vector/list.
- **`next()` / `.next()`**: Yields the single next item in the stream.
- **`iter()`**: Streams items lazily without buffering.

### Property Materialization Hint: `withProperties()`
By default, element traversals fetch **only `id` and `label`** (0 property I/O reads). Use `withProperties()` before the source step to specify property retrieval:
- 🦀 Rust: `g.withProperties([]).V(...)` fetches **all** properties; `g.withProperties(["name", "age"]).V(...)` fetches **only** the named ones.
- 🐍 Python: takes variadic arguments, not a list — `g.withProperties().V(...)` (no arguments) fetches **all**; `g.withProperties("name", "age").V(...)` fetches **only** the named ones.

---

## 1. Source Steps

Source steps initiate the traversal pipeline.

### `V([ids...])` — Vertex Stream
Starts a traversal over all vertices or specific vertex IDs (`i64`).

| Property | Details |
| :--- | :--- |
| **Signature** | `g.V([ids...])` |
| **Input $\rightarrow$ Output** | `() -> Vertex` |
| **🦀 Rust** | `snap.g().V([]).to_list()?` (all vertices) or `snap.g().V([1, 2]).to_list()?` |
| **🐍 Python** | `snap.g().V().to_list()` (all vertices) or `snap.g().V(1, 2).to_list()` |

### `E([ids...])` — Edge Stream
Starts a traversal over all edges in the graph or specific 30-character canonical Edge IDs.

| Property | Details |
| :--- | :--- |
| **Signature** | `g.E([ids...])` |
| **Input $\rightarrow$ Output** | `() -> Edge` |
| **🦀 Rust** | `snap.g().E([]).to_list()?` or `snap.g().E(["AAAAAAAAAAEAAAADAAAAAAAAAAIAAA".to_string()]).to_list()?` (owned `String`s — `E` takes `IntoIterator<Item = String>`, not `&str`) |
| **🐍 Python** | `snap.g().E().to_list()` or `snap.g().E("AAAAAAAAAAEAAAADAAAAAAAAAAIAAA").to_list()` |

### `addV(label)` — Add Vertex
Creates a new vertex within an active transaction.

| Property | Details |
| :--- | :--- |
| **Signature** | `txn.g().addV(label)` |
| **Input $\rightarrow$ Output** | `() -> Vertex` |
| **🦀 Rust** | `txn.g().addV("person").property("id", 1i64).property("name", "Alice").next();` |
| **🐍 Python** | `txn.g().addV("person").property("id", 1).property("name", "Alice").next()` |

### `addE(label)` — Add Edge
Creates a directed edge between two vertices within an active transaction.

| Property | Details |
| :--- | :--- |
| **Signature** | `txn.g().addE(label).from(src).to(dst)` |
| **Input $\rightarrow$ Output** | `() -> Edge` |
| **🦀 Rust** | `txn.g().addE("knows").from(1i64).to(2i64).property("since", 2023).property("rank", 0u16).next();` |
| **🐍 Python** | `txn.g().addE("knows").from_(1).to(2).property("since", 2023).property("rank", 0).next()` |

---

## 2. Navigation Steps

Navigation steps traverse graph relationships between vertices and edges.

### Directional Traversal Matrix

```
       ┌───────────┐           ┌───────────┐
       │  outV()   │           │   inV()   │
       └─────▲─────┘           └─────▲─────┘
             │                       │
      ──outE(label)──► [ Edge ] ──inE(label)──►
             │                       │
       ┌─────▼─────┐           ┌─────▼─────┐
       │  Vertex A │──out(label)─►│  Vertex B │
       └───────────┘           └───────────┘
```

### `out([labels...])`
Traverses from current vertices to adjacent outgoing neighbor vertices.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Vertex -> Vertex` |
| **Arguments** | Optional list of edge label strings |
| **🦀 Rust** | `snap.g().V([1]).out(["knows"]).to_list()?` |
| **🐍 Python** | `snap.g().V(1).out("knows").to_list()` |

### `in([labels...])`
Traverses to adjacent incoming source vertices.

> [!NOTE]
> `in` is a Rust keyword, so the Rust method is the raw identifier `r#in`, not `in_`.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Vertex -> Vertex` |
| **🦀 Rust** | `snap.g().V([2]).r#in(["knows"]).values(["name"]).to_list()?` |
| **🐍 Python** | `snap.g().V(2).in_("knows").values("name").to_list()` |

### `both([labels...])`
Traverses adjacent neighbors along both incoming and outgoing edges.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Vertex -> Vertex` |
| **🦀 Rust** | `snap.g().V([1]).both(["knows"]).to_list()?` |
| **🐍 Python** | `snap.g().V(1).both("knows").to_list()` |

### Edge Navigation: `outE()`, `inE()`, `bothE()`
Steps from vertices to incident **Edges**.

| Method | Direction | Input $\rightarrow$ Output | Example (Python) |
| :--- | :--- | :--- | :--- |
| `outE([labels])` | Outgoing edges | `Vertex -> Edge` | `snap.g().V(1).outE("knows").to_list()` |
| `inE([labels])` | Incoming edges | `Vertex -> Edge` | `snap.g().V(2).inE("knows").to_list()` |
| `bothE([labels])`| All incident edges | `Vertex -> Edge` | `snap.g().V(1).bothE().to_list()` |

### Vertex Resolvers: `inV()`, `outV()`, `otherV()`
Steps from **Edges** back to connected **Vertices**.

| Method | Target Vertex | Input $\rightarrow$ Output | Example (Rust) |
| :--- | :--- | :--- | :--- |
| `inV()` | Target / Head vertex | `Edge -> Vertex` | `snap.g().E([]).inV().to_list()?` |
| `outV()` | Source / Tail vertex | `Edge -> Vertex` | `snap.g().E([]).outV().to_list()?` |
| `otherV()`| The vertex opposite to traverser origin | `Edge -> Vertex` | `snap.g().V([1]).bothE([]).otherV().to_list()?` |

---

## 3. Filtering Steps

Filtering steps discard items from the stream based on property predicates or cardinality boundaries.

### `has(key, [val | predicate])`
Filters elements where property `key` matches a value or predicate (`gt`, `lt`, `gte`, `lte`, `eq`, `neq`, `within`). Note: Reserved structural keys (`"id"`, `"label"`, `"rank"`) cannot be queried with `.has()` — use `.hasId()`, `.hasLabel()`, or `.hasRank()` instead. Attempting `.has("id", ...)` (or `"label"`/`"rank"`) is rejected at query validation with a `SchemaViolation` (`SchemaError` in Python).

| Property | Details |
| :--- | :--- |
| **🦀 Rust** | `snap.g().V([]).has("age", 30).to_list()?`<br>`snap.g().V([]).has("age", gt(25)).to_list()?` (predicate helpers like `gt` are top-level functions — `use rocksgraph::gt;`, not a `P::` path; there is no `has_where` step) |
| **🐍 Python** | `snap.g().V().has("age", 30).to_list()`<br>`snap.g().V().has("age", P.gt(25)).to_list()` |

### `hasLabel([labels...])`
Filters elements matching specific vertex/edge labels.

| Property | Details |
| :--- | :--- |
| **🦀 Rust** | `snap.g().V([]).hasLabel("person").to_list()?` |
| **🐍 Python** | `snap.g().V().hasLabel("person").to_list()` |

### `hasId([ids...])`
Filters vertices by integer IDs or edges by canonical ID strings.

| Property | Details |
| :--- | :--- |
| **🦀 Rust** | `snap.g().V([]).hasId([1, 2, 3]).to_list()?` |
| **🐍 Python** | `snap.g().V().hasId([1, 2, 3]).to_list()` (single argument — a value, a list, or a `P` predicate; `hasId(1, 2, 3)` is a `TypeError`) |

### `hasRank([ranks...])`
Filters edges matching specific `u16` discriminator ranks (0 to 65,535).

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Edge -> Edge` (Non-matching on Vertices) |
| **🦀 Rust** | `snap.g().V([1]).outE(["transfer"]).hasRank(0u16).to_list()?` |
| **🐍 Python** | `snap.g().V(1).outE("transfer").hasRank(0).to_list()` |

### `limit(n)`, `skip(n)`, `range(low, high)`
Slices the traversal stream with early engine termination.

| Property | Details |
| :--- | :--- |
| **🦀 Rust** | `snap.g().V([]).hasLabel("person").limit(10).to_list()?` |
| **🐍 Python** | `snap.g().V().hasLabel("person").limit(10).to_list()` |

### `dedup()`
Removes duplicate objects from the stream (by traverser identity — takes no arguments in either language).

| Property | Details |
| :--- | :--- |
| **🦀 Rust** | `snap.g().V([1]).out(["knows"]).out(["knows"]).dedup().to_list()?` |
| **🐍 Python** | `snap.g().V(1).out("knows").out("knows").dedup().to_list()` |

---

## 4. Transformation & Projection Steps

### `values([keys...])`
Extracts property values from vertices or edges as scalar values. Reserved keys (`"id"`, `"label"`, `"rank"`) are disallowed in `values()`.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Element -> Value` |
| **🦀 Rust** | `snap.g().V([1]).values(["name"]).to_list()?` |
| **🐍 Python** | `snap.g().V(1).values("name").to_list()` |

### `id()`, `label()`, `rank()`
Extracts the structural integer ID, string label, or edge rank of elements.

| Step | Scope | Output Type | Description |
| :--- | :--- | :--- | :--- |
| **`id()`** | Vertex & Edge | `Int64` (Vertex) / `String` (Edge) | Returns primary identifier |
| **`label()`** | Vertex & Edge | `String` | Returns entity label |
| **`rank()`** | Edge only | `UInt16` (`u16`) | Returns edge rank (0 to 65,535) |

### `path()`
Returns the complete sequence of vertices and edges traversed along a path.

| Property | Details |
| :--- | :--- |
| **🦀 Rust** | `snap.g().V([1]).out(["knows"]).out(["knows"]).path().to_list()?` |
| **🐍 Python** | `snap.g().V(1).out("knows").out("knows").path().to_list()` |

---

## 5. Vector Steps

### `nearest(prop, q, k)` — Entry-point ANN Seed
Finds top-$k$ nearest vertices to query vector `q`. Must immediately follow `g.V([])`.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `() -> Vertex` |
| **🦀 Rust** | `snap.g().V([]).nearest("emb", vec![0.1, 0.9, 0.0], 5).to_list()?` |
| **🐍 Python** | `snap.g().V().nearest("emb", [0.1, 0.9, 0.0], 5).to_list()` |

### `similarity(prop, q, metric)` — Mid-stream Scorer
Evaluates similarity between traverser's vector property and reference vector.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Vertex -> Float` |
| **🦀 Rust** | `snap.g().V([1]).out(["knows"]).similarity("emb", q, DistanceMetric::Cosine).to_list()?` |
| **🐍 Python** | `snap.g().V(1).out("knows").similarity("emb", q, DistanceMetric.Cosine).to_list()` |

### `neighbors(source_prop, target_prop, k, entity_type)` — Semantic Graph Expansion
Expands each traverser vertex to its $k$ nearest neighbors in the target vector index.

> [!NOTE]
> In v0.2, vector indexes and `neighbors()` support only `VectorEntityType::Vertex` (`VectorEntityType.Vertex` in Python). Edge vector indexing is planned for v0.3.

| Property | Details |
| :--- | :--- |
| **Input $\rightarrow$ Output** | `Vertex -> Vertex` |
| **🦀 Rust** | `snap.g().V([1]).neighbors("emb", "emb", 3, VectorEntityType::Vertex).to_list()?` |
| **🐍 Python** | `snap.g().V(1).neighbors("emb", "emb", 3, VectorEntityType.Vertex).to_list()` |

### Vector Step Modifiers (Tuning Knobs)

Vector search steps can be customized at query time using chained modifier steps:

- **`.with_ef_search(ef: usize)`**: Dynamically adjusts the size of the dynamic candidate list during HNSW graph exploration (higher = higher recall, lower latency). Must immediately follow `.nearest()` or `.neighbors()`.
  - 🦀 Rust: `snap.g().V([]).nearest("emb", q, 10).with_ef_search(100).to_list()?`
  - 🐍 Python: `snap.g().V().nearest("emb", q, 10).with_ef_search(100).to_list()`
- **`.with_metric(metric: DistanceMetric)`**: Overrides the distance metric at query time for brute-force or metric-compatible searches. Must immediately follow `.nearest()`.
  - 🦀 Rust: `snap.g().V([]).nearest("emb", q, 5).with_metric(DistanceMetric::Cosine).to_list()?`
  - 🐍 Python: `snap.g().V().nearest("emb", q, 5).with_metric(DistanceMetric.Cosine).to_list()`

---

## 6. Aggregation, Ordering & Degree Optimization

| Step | Output Type | Description | Example (Python) |
| :--- | :--- | :--- | :--- |
| `count()` | `Int64` | Total number of elements in stream | `snap.g().V().hasLabel("person").count().next()` |
| `sum()` | `Float / Int` | Sum of numeric properties | `snap.g().V().values("age").sum().next()` |
| `mean()` | `Float` | Arithmetic mean of values | `snap.g().V().values("age").mean().next()` |
| `max()`, `min()` | Scalar | Maximum or minimum value | `snap.g().V().values("score").max().next()` |
| `groupCount()`| `Map<Key, Int64>`| Frequency histogram by key/label | `snap.g().V().groupCount().by("city").next()` |
| `degree(dir)` | `Int64` | Direct degree calculation (`OUT`, `IN`, `BOTH`) | `snap.g().V(1).degree(Direction.OUT).next()` |
| `order()` | *(unchanged)* | Sort traverser stream (default ascending) | `snap.g().V().values("age").order().next()` |
| `by(key, order)` | *(unchanged)* | Modulate order by property key and/or direction (`Order.Asc` / `Order.Desc`) | `snap.g().V().order().by("age", Order.Desc).to_list()` |
| `fold()` | `List[Element]` | Collects entire stream into a single list | `snap.g().V(1).out("knows").values("name").fold().next()` |

### Degree Optimization & Counts Are O(1)

RocksGraph maintains fast degree counters on vertices. You can query degrees directly using `.degree()`:
- 🦀 Rust: `snap.g().V([1]).degree(DegreeDirection::Out).to_list()?` (`degree()` requires an explicit direction — use `DegreeDirection::Both` for both; there is no `degree_default()`)
- 🐍 Python: `snap.g().V(1).degree(Direction.OUT).to_list()` (or bare `snap.g().V(1).degree().to_list()`, which defaults to both)

Additionally, `.out().count()`, `.in_().count()`, and `.both().count()` are automatically rewritten by the optimizer to run in O(1) instead of performing a full adjacency scan.

> [!IMPORTANT]
> **Activation conditions for O(1) count rewrite**: only when `.out()`/`.in_()`/`.both()` are completely **unlabeled and unfiltered** and immediately followed by `.count()`. Adding a label (`.out("knows").count()`) or a property predicate falls back to a standard edge scan.

### Ordering & Sorting Modulators

- 🦀 **Rust**:
  - `snap.g().V([]).order().by("age").to_list()?` (sort by property ascending)
  - `snap.g().V([]).order_by("age", Order::Desc).to_list()?` (sort by property descending)
- 🐍 **Python**:
  - `snap.g().V().values("age").order().by(Order.Desc).to_list()` (sort values directly)
  - `snap.g().V().order().by("age", Order.Desc).to_list()` (sort by property)
  - `snap.g().V().similarity("emb", q, DistanceMetric.Cosine).order().by(Order.Desc).to_list()` (sort by similarity score descending — `metric` is required, not optional)

---

## 7. Plan Inspection: `explain()`

Use `.explain()` to inspect the physical query execution plan tree generated by the query optimizer:

#### 🦀 Rust
```rust
let plan_str = snap.g().V([1]).out(["knows"]).explain()?;
println!("{}", plan_str);
```

#### 🐍 Python
```python
print(snap.g().V(1).out("knows").explain())
```

---

## 8. Branching & Control Flow

### `repeat(traversal)`, `times(n)`, `until(predicate)`, `emit()`
Executes recursive multi-hop loop traversals.

```python
# Traverse up to 3 hops of friends, emitting each intermediate friend
snap.g().V(1).repeat(out("knows")).times(3).emit().values("name").to_list()
```

> [!NOTE]
> `.repeat()` requires at least one loop terminator — `.times(n)` and/or `.until(predicate)`. Omitting both is rejected at query build time with a traversal error, not a silent infinite loop.

### `union(t1, t2, ...)`
Executes multiple sub-traversals in parallel and merges their output streams.

```python
# Retrieve both incoming and outgoing relationships in a single pass
snap.g().V(1).union(outE("knows"), inE("knows")).to_list()
```

### `coalesce(t1, t2, ...)`
Evaluates sub-traversals sequentially, returning results from the first branch that yields at least one item (ideal for idempotent upserts).

```python
# Get nickname if present, else fallback to formal name
snap.g().V(1).coalesce(values("nickname"), values("name")).next()
```

---

## 9. Traversal Best Practices

### Pattern 1: Filter Each Hop When You Reach It
`.has()` immediately followed by `.limit(n)` already stops as soon as `n` matching items are found — no extra work needed. Apply `.has()` on a hop's properties as soon as you reach that hop, rather than navigating further away from it first; see [Performance Tuning](performance.md#rule-1-filter-each-hop-when-you-reach-it) for why this differs from reordering across hops.

```python
# ✅ BEST PRACTICE: Terminate immediately after finding 5 candidates
snap.g().V(1).out("knows").has("age", P.gt(30)).limit(5).to_list()
```

### Pattern 2: Specify Edge Labels in Navigation Steps
Always supply explicit edge labels (`.out(["knows"])`) rather than scanning all outgoing edges indiscriminately.

```rust
// ✅ BEST PRACTICE: Reads only "knows" adjacency records from storage
snap.g().V([1]).out(["knows"]).to_list()?;
```

### Pattern 3: Use `.values()` for Specific Scalar Lookups
Avoid returning full `Vertex` or `Edge` maps if your application only needs specific scalar fields like `"name"` or `"email"`.

```python
# ✅ BEST PRACTICE: Zero-copy scalar extraction
names = snap.g().V(1).out("knows").values("name").to_list()
```

### Pattern 4: Deduplicate Across Multi-Hop Expansions
When traversing 2 or more hops of social graphs or web networks, duplicate paths converge quickly. Add `.dedup()` to bound intermediate stream cardinality.

```python
# ✅ BEST PRACTICE: Prevent exponential graph expansion
friends_of_friends = snap.g().V(1).out("knows").out("knows").dedup().to_list()
```

---

## 10. Traversal Anti-Patterns

### ❌ Anti-Pattern 1: Client-Side Filtering
Materializing thousands of graph elements into Python/Rust memory before filtering wastes CPU cycles and database cache.

```python
# ❌ ANTI-PATTERN: Fetches 50,000 vertices and filters in Python
all_people = snap.g().V().to_list()
active_users = [p for p in all_people if p.properties.get("status") == "active"]

# ✅ CORRECT: Let the database engine filter during storage scan
active_users = snap.g().V().has("status", "active").to_list()
```

### ❌ Anti-Pattern 2: N+1 Traversal Query Loops
Executing individual traversal queries in an application loop is a classic performance killer.

```python
# ❌ ANTI-PATTERN: N roundtrips through query engine
friend_names = []
for user_id in user_ids:
    friend_names.extend(snap.g().V(user_id).out("knows").values("name").to_list())

# ✅ CORRECT: Single batched multi-source traversal
friend_names = snap.g().V(*user_ids).out("knows").values("name").to_list()
```

---

## Related Topics

- [Getting Started](getting_started.md) — 5-minute practical introduction.
- [Vector Search Deep Dive](vector_search.md) — HNSW vector queries and similarity scoring.
- [Data Model](data_model.md) — Properties, types, and entity structures.
- [Performance Tuning](performance.md) — Query tuning and memory optimization.
