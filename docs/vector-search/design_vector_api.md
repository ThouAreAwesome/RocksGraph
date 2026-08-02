# Design: Vector Search API — Query Patterns and Step Design

Status: **stable** — step vocabulary, query patterns, and lifecycle operations are
finalised. All design decisions settled (§4).

---

## Table of Contents

- [Design: Vector Search API — Query Patterns and Step Design](#design-vector-search-api--query-patterns-and-step-design)
  - [Table of Contents](#table-of-contents)
  - [1. Design approach](#1-design-approach)
  - [2. Query patterns](#2-query-patterns)
    - [P1 — Semantic retrieval (full graph)](#p1--semantic-retrieval-full-graph)
    - [P2 — Typed semantic retrieval](#p2--typed-semantic-retrieval)
    - [P3 — Filtered semantic retrieval](#p3--filtered-semantic-retrieval)
    - [P4 — Graph-scoped retrieval](#p4--graph-scoped-retrieval)
    - [P5 — Semantic expansion](#p5--semantic-expansion)
    - [P6 — Scored retrieval](#p6--scored-retrieval)
    - [P7 — Threshold filter](#p7--threshold-filter)
    - [P8 — Edge ANN](#p8--edge-ann)
    - [P9 — Cascaded re-ranking](#p9--cascaded-re-ranking)
    - [P10 — Multi-index union](#p10--multi-index-union)
    - [P11 — Entity-to-entity similarity](#p11--entity-to-entity-similarity)
    - [P12 — Paginated ANN retrieval](#p12--paginated-ann-retrieval)
  - [3. Step vocabulary](#3-step-vocabulary)
    - [3a. Core steps](#3a-core-steps)
      - [`vectorSimilarity(prop_name, query_vec[, metric])`](#vectorsimilarityprop_name-query_vec-metric)
      - [`nearestBy(source_prop, target_prop, k, entity_type)`](#nearestbysource_prop-target_prop-k-entity_type)
    - [3b. `vectorNear` — syntactic sugar and ANN hint](#3b-vectornear--syntactic-sugar-and-ann-hint)
    - [3c. Removed from previous draft](#3c-removed-from-previous-draft)
  - [4. Design decisions](#4-design-decisions)
    - [C1 — `vectorSimilarity` direction and naming](#c1--vectorsimilarity-direction-and-naming)
    - [C2 — `nearestBy` entity type disambiguation](#c2--nearestby-entity-type-disambiguation)
    - [C3 — ANN hint surface for `order().by().limit()` form](#c3--ann-hint-surface-for-orderbylimit-form)
    - [C4 — `vectorSimilarity` recomputation](#c4--vectorsimilarity-recomputation)
    - [C5 — `nearestBy` per-traverser execution](#c5--nearestby-per-traverser-execution)
    - [C6 — Skip/range overfetch rule](#c6--skiprange-overfetch-rule)
  - [5. Support plan](#5-support-plan)
  - [6. Index lifecycle management](#6-index-lifecycle-management)
    - [6a. Declare index at open time](#6a-declare-index-at-open-time)
    - [6b. Add index to a running graph](#6b-add-index-to-a-running-graph)
    - [6c. Drop an index](#6c-drop-an-index)
    - [6d. Trigger explicit rebuild](#6d-trigger-explicit-rebuild)
    - [6e. Query index statistics](#6e-query-index-statistics)
    - [6f. Change index algorithm](#6f-change-index-algorithm)
  - [7. Write path](#7-write-path)
    - [7a. Insert entity with vector property](#7a-insert-entity-with-vector-property)
    - [7b. Update a vector property](#7b-update-a-vector-property)
    - [7c. Remove vector property, keep entity](#7c-remove-vector-property-keep-entity)
    - [7d. Remove entity — implicit vector cleanup](#7d-remove-entity--implicit-vector-cleanup)
    - [7e. Batch mutations in one transaction](#7e-batch-mutations-in-one-transaction)
    - [7f. Un-indexed FloatVector properties](#7f-un-indexed-floatvector-properties)
  - [8. Bulk operations](#8-bulk-operations)
    - [8a. Offline index build from unindexed data](#8a-offline-index-build-from-unindexed-data)
    - [8b. Index snapshot export / import](#8b-index-snapshot-export--import)
    - [8c. Offline batch reindex — new embedding model](#8c-offline-batch-reindex--new-embedding-model)
  - [9. Error types](#9-error-types)
  - [10. What is explicitly out of scope](#10-what-is-explicitly-out-of-scope)

---

## 1. Design approach

The step vocabulary is derived by first identifying query patterns in natural
language, then finding the minimum set of new Gremlin steps that can express all
patterns through composition with existing Gremlin primitives.

**Gremlin step kinds** used in this document:

| Kind     | Behaviour                    | Examples                         |
| -------- | ---------------------------- | -------------------------------- |
| Filter   | Reduce traverser set         | `has()`, `hasLabel()`, `where()` |
| Flat-map | Expand each traverser to N   | `out()`, `values()`              |
| Map      | Transform each traverser 1:1 | `id()`, `label()`, `project()`   |
| Barrier  | Consume all, emit a new set  | `order()`, `count()`, `limit()`  |

**Optimizer-decides model**: the expression
`V().order().by(__.vectorSimilarity("embedding", q), desc).limit(k)` is the
logical form for "find the k vertices nearest to q by embedding." The query planner
decides the physical execution:

- Incoming stream is small (e.g. `V(id).out("wrote")` → 20 vertices) → compute
  distances inline, exact result.
- Incoming stream is large (e.g. `V()` — full graph) → rewrite to ANN index scan,
  approximate result.

This mirrors how relational databases rewrite `ORDER BY embedding <-> q LIMIT k`
to an index scan when a vector index is available.

**Reading the patterns**: each pattern shows the **core form** using only the two
new steps (`vectorSimilarity`, `nearestBy`) and standard Gremlin. Where
`vectorNear` sugar covers the same intent, it is shown as a secondary block
labelled `# sugar`.

---

## 2. Query patterns

### P1 — Semantic retrieval (full graph)

*Find the k vertices most similar to this query vector.*

```python
# core
rs.traversal().V() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .to_list()

# sugar
rs.traversal().V().vectorNear("embedding", query_vec, k).to_list()
```

`vectorSimilarity("embedding", query_vec)` extracts the `"embedding"` property from
each vertex and computes its similarity to `query_vec`, emitting `f32`.
`order().by(desc).limit(k)` selects the k most similar.

**Optimizer**: for `V()` (full graph), rewrites `order().by(vectorSimilarity).limit(k)`
to an ANN index scan. Returns approximate top-k.

**Alignment**: `vectorSimilarity` map → `order()`/`limit()` barriers. No new step kinds.

---

### P2 — Typed semantic retrieval

*Find the k most similar vertices with label "doc".*

```python
# core
rs.traversal().V().hasLabel("doc") \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .to_list()

# sugar
rs.traversal().V().hasLabel("doc") \
    .vectorNear("embedding", query_vec, k) \
    .to_list()
```

**Optimizer**: small label set → inline exact. Large label set → ANN index scan
with label as pre-filter (v0.3+). In v0.2 the optimizer falls back to global ANN +
post-filter on label.

**v0.2 note**: post-filter may return fewer than k results if many top-k candidates
fail the label check.

---

### P3 — Filtered semantic retrieval

*Find the k most similar published ML papers.*

```python
# core
rs.traversal().V().hasLabel("doc") \
    .has("status", "published") \
    .has("category", "ML") \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .to_list()

# sugar
rs.traversal().V().hasLabel("doc") \
    .has("status", "published") \
    .has("category", "ML") \
    .vectorNear("embedding", query_vec, k) \
    .to_list()
```

Filter steps reduce the candidate pool before `order().by().limit()`. For small
filtered sets, the optimizer computes distances inline (exact). For large sets it
uses ANN with pre-filter.

---

### P4 — Graph-scoped retrieval

*Among Alice's papers, find the k most relevant to this query.*

```python
# core
rs.traversal().V(author_id).out("wrote") \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .to_list()

# sugar
rs.traversal().V(author_id).out("wrote") \
    .vectorNear("embedding", query_vec, k) \
    .to_list()
```

`out("wrote")` produces a small, bounded vertex set. The optimizer computes
distances inline — exact result, no ANN approximation needed. The most
Gremlin-native ANN pattern: graph traversal narrows candidates, distance ranking
selects the best.

---

### P5 — Semantic expansion

*Find similar documents, then follow their citation edges.*

```python
# core
rs.traversal().V() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .out("cites").values("title") \
    .to_list()
# → ["Paper A", "Paper B", ...]

# sugar
rs.traversal().V().vectorNear("embedding", query_vec, k) \
    .out("cites").values("title") \
    .to_list()
```

After `limit(k)`, the pipeline holds k vertex traversers. `out("cites")` and
`values("title")` are standard steps on those traversers. The ANN result is the
starting point of a normal graph traversal.

---

### P6 — Scored retrieval

*Return results annotated with their distance to the query.*

```python
# core
rs.traversal().V() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .project("vertex", "similarity") \
      .by(identity()) \
      .by(__.vectorSimilarity("embedding", query_vec)) \
    .to_list()
# → [{"vertex": Vertex(...), "similarity": 0.97}, ...]

# sugar — vectorNear for retrieval; project() annotation has no sugar form
rs.traversal().V() \
    .vectorNear("embedding", query_vec, k) \
    .project("vertex", "similarity") \
      .by(identity()) \
      .by(__.vectorSimilarity("embedding", query_vec)) \
    .to_list()
```

`project()` + `by()` is idiomatic Gremlin for building a result map. The traverser
type inside each `by()` is still `Vertex` — `vectorSimilarity` computes the scalar
inline. The output traverser is `Map<String, Object>`.

**Note on recomputation**: `vectorSimilarity` is called once in `order().by()` and
again inside `project().by()`. See open challenge C4.

---

### P7 — Threshold filter

*Keep only vertices within distance `t` of the query.*

```python
# core — threshold only: all vertices within distance t (exact, full scan)
rs.traversal().V() \
    .where(__.vectorSimilarity("embedding", query_vec).is_(gt(t))) \
    .to_list()

# core — top-k then threshold: nearest k, keep those within distance t
rs.traversal().V() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .where(__.vectorSimilarity("embedding", query_vec).is_(gt(t))) \
    .to_list()

# sugar — vectorNear for top-k retrieval; where() threshold has no sugar form
rs.traversal().V() \
    .vectorNear("embedding", query_vec, k) \
    .where(__.vectorSimilarity("embedding", query_vec).is_(gt(t))) \
    .to_list()
```

`where()` + `is_()` is standard Gremlin predicate filtering. `vectorSimilarity` is
the computed predicate value. No special threshold step needed. The threshold-only
form (no top-k) has no sugar equivalent — `vectorNear` always implies a `limit(k)`
barrier.

---

### P8 — Edge ANN

*Find the k edges most semantically similar to this query.*

```python
# core
rs.traversal().E() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .to_list()

# sugar
rs.traversal().E().vectorNear("embedding", query_vec, k).to_list()
```

Scored variant — annotate results with distance (mirrors P6):

```python
# core
rs.traversal().E() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k) \
    .project("edge", "similarity") \
      .by(identity()) \
      .by(__.vectorSimilarity("embedding", query_vec)) \
    .to_list()

# sugar — vectorNear for retrieval; project() annotation has no sugar form
rs.traversal().E() \
    .vectorNear("embedding", query_vec, k) \
    .project("edge", "similarity") \
      .by(identity()) \
      .by(__.vectorSimilarity("embedding", query_vec)) \
    .to_list()
```

`E()` emits edge traversers. `values("embedding")` extracts the edge's vector.
`vectorSimilarity` and `order().by().limit()` work identically on edges as on
vertices. The incoming traverser type determines which index `vectorNear` and the
optimizer use: edge traversers → edge index.

---

### P9 — Cascaded re-ranking

*Narrow by title similarity, then re-rank survivors by content similarity.*

```python
# core
rs.traversal().V() \
    .order().by(__.vectorSimilarity("title_embedding", title_q), desc) \
    .limit(50) \
    .order().by(__.vectorSimilarity("content_embedding", content_q), desc) \
    .limit(10) \
    .to_list()

# sugar — chained vectorNear; each call is one barrier on the previous output
rs.traversal().V() \
    .vectorNear("title_embedding", title_q, 50) \
    .vectorNear("content_embedding", content_q, 10) \
    .to_list()
```

Two sequential barriers. First pass: optimizer uses ANN on `title_embedding` index.
Second pass: 50 candidates → inline exact computation on `content_embedding`. No
new step types needed.

---

### P10 — Multi-index union

*Find vertices similar to the query on EITHER the `title_emb` OR the `body_emb` index.*

```python
# core
rs.traversal().union(
    __.V().order().by(__.vectorSimilarity("title_emb", query_vec), desc).limit(k),
    __.V().order().by(__.vectorSimilarity("body_emb", query_vec), desc).limit(k)
).dedup().to_list()

# sugar — vectorNear inside each union branch; union() itself has no sugar form
rs.traversal().union(
    __.V().vectorNear("title_emb", query_vec, k),
    __.V().vectorNear("body_emb", query_vec, k)
).dedup().to_list()
```

Standard Gremlin `union()` merges two traversal branches. Each branch searches a
different index. `.dedup()` removes vertices that ranked in both.

**With similarity scores for application-level fusion**:

```python
# core
rs.traversal().union(
    __.V().order().by(__.vectorSimilarity("title_emb", query_vec), desc).limit(k)
        .project("v", "sim").by(identity()).by(__.vectorSimilarity("title_emb", query_vec)),
    __.V().order().by(__.vectorSimilarity("body_emb", query_vec), desc).limit(k)
        .project("v", "sim").by(identity()).by(__.vectorSimilarity("body_emb", query_vec))
).to_list()

# sugar — vectorNear for retrieval; project() annotation has no sugar form
rs.traversal().union(
    __.V().vectorNear("title_emb", query_vec, k)
        .project("v", "sim").by(identity()).by(__.vectorSimilarity("title_emb", query_vec)),
    __.V().vectorNear("body_emb", query_vec, k)
        .project("v", "sim").by(identity()).by(__.vectorSimilarity("body_emb", query_vec))
).to_list()
# collect both result lists, apply RRF or weighted fusion at application level
```

---

### P11 — Entity-to-entity similarity

*For each incoming vertex, find the k vertices most similar to it by its own stored
embedding.*

```python
# core — no sugar equivalent

# Single source vertex — nearestBy used directly (no local() needed)
rs.traversal().V(xx) \
    .nearestBy("embedding", "embedding", k, VectorEntityType.VERTEX) \
    .to_list()

# Multiple source vertices — local() makes per-traverser scoping explicit:
# each vertex runs its own ANN search independently
rs.traversal().V().has("age", between(20, 30)) \
    .local(__.nearestBy("embedding", "embedding", k, VectorEntityType.VERTEX)) \
    .to_list()

# Cross-property — for each question, find k relevant answers.
# Source: q_embedding (read from question traverser).
# Target: a_embedding index (searched by nearestBy).
# Precondition: both embeddings share the same dimension and model.
rs.traversal().V().hasLabel("question") \
    .local(__.nearestBy("q_embedding", "a_embedding", k, VectorEntityType.VERTEX)) \
    .to_list()
```

`nearestBy` is a flat-map step: each incoming traverser expands independently to k
results. For a single source (`V(xx).nearestBy(...)`) this works without any wrapper.
`local()` is the idiomatic choice when iterating over multiple source traversers — it
makes the per-traverser scoping explicit and consistent with standard Gremlin style
for sub-traversal steps.

**Type chain**: `Vertex → [Vertex × k]`

---

### P12 — Paginated ANN retrieval

*Retrieve results in pages — skip the first n results and return the next k.*

```python
# core — page 2 (results 11–20)
rs.traversal().V() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .range(10, 20) \
    .to_list()

# sugar — vectorNear followed by skip
rs.traversal().V().vectorNear("embedding", query_vec, 10).skip(10).to_list()
```

**Planner overfetch rule**: the optimizer detects `skip(n)` or `range(s, e)` immediately
downstream of an ANN-rewritten `order().by(vectorSimilarity).limit(k)` subtree and
increases the ANN fetch count automatically. The skip/range is then applied in memory:

| Expression                               | ANN fetches | In-memory slice |
| ---------------------------------------- | :---------: | --------------- |
| `order().by(vs).limit(k).skip(n)`        | `n + k`     | `[n : n+k]`     |
| `order().by(vs).range(s, e)`             | `e`         | `[s : e]`       |
| `vectorNear(prop, q, k).skip(n)`         | `n + k`     | `[n : n+k]`     |
| `vectorNear(prop, q, k).range(s, e)`     | `e`         | `[s : e]`       |

No additional ANN call is made; the user writes idiomatic Gremlin and the planner
handles the overfetch transparently (see §4 C6 for the design decision).

**Quality degrades at depth**: HNSW allocates search effort toward top-k recall,
not arbitrary-offset recall. Results at position `n+k` have lower recall than results
at position `k` for the same `ef_search`. At `skip=50, limit=10` (ANN fetches 60),
position 60 is less accurate than position 10. For exact results at depth, use the
brute-force path (O(N), always exact) or collect the full result set in one call and
paginate client-side.

**Non-determinism across calls**: ANN results are not stable between separate traversal
calls on a mutable index. If inserts or deletes are committed between two paginated
requests, page 2 may include results that appeared on page 1, or may miss results whose
ranking shifted. Cursor-based stable pagination across a mutable ANN index is not
supported in v0.2.

---

## 3. Step vocabulary

### 3a. Core steps

Two new steps are required. Everything else in §2 uses standard Gremlin.

---

#### `vectorSimilarity(prop_name, query_vec[, metric])`

**Kind**: map  
**Type**: `Vertex/Edge → f32`  
**Parameters**: `prop_name` — the property to read from the incoming traverser;
`query_vec` — a constant `Vector` value; `metric` — optional `DistanceMetric`
(inferred from declared index when one exists; required otherwise)

Reads the `prop_name` `FloatVector` property from the incoming `Vertex/Edge` traverser
and computes a normalised similarity score against `query_vec`.

```
vectorSimilarity("embedding", query_vec)   →   f32
```

**Direction**: always emits **higher = more similar**, regardless of the underlying
metric. The engine normalises raw metric values internally:

| Metric         | Raw value     | Emitted value                     |
| -------------- | ------------- | --------------------------------- |
| Cosine         | sim ∈ [0, 1]  | `cosine_sim` (no transform)       |
| Euclidean (L2) | dist ∈ [0, ∞) | `1 / (1 + l2_dist)` → (0, 1]      |
| InnerProduct   | unbounded     | `sigmoid(inner_product)` → (0, 1) |

`order(..., desc)` consistently means "most similar first."

Use cases:

**Ranking** (P1–P5, P8, P9) — optimizer rewrites to ANN index scan for large streams:
```python
.order().by(__.vectorSimilarity("embedding", query_vec), desc).limit(k)
```

**Threshold filter** (P7) — brute-force exact scan; no index required:
```python
.where(__.vectorSimilarity("embedding", query_vec).is_(gt(0.85)))
```

**Score annotation** (P6, P8 scored, P10 fusion) — reuses cached score from the
upstream `order()` barrier (see C4); no recomputation at k ≤ 1000:
```python
.project("vertex", "similarity")
  .by(identity())
  .by(__.vectorSimilarity("embedding", query_vec))
```

**Metric inference**: the metric is inferred from the declared index config for
`prop_name`. When no index is declared, the `metric` parameter is required:
`vectorSimilarity("prop", query_vec, DistanceMetric.COSINE)`.

---

#### `nearestBy(source_prop, target_prop, k, entity_type)`

**Kind**: flat-map  
**Type**: `Vertex/Edge → [Vertex/Edge × k]`  
**Parameters**: `source_prop` — the property on the incoming traverser whose value
is used as the query vector; `target_prop` — the name of the declared vector index
to search; `k` — number of results; `entity_type` — `VectorEntityType.VERTEX` or
`VectorEntityType.EDGE` (required — disambiguates when the same property name is
indexed on both vertices and edges)

**No metric parameter**: the design enforces at most one index per `(entity_type,
property_name)` pair (see §4 C2). Because `(target_prop, entity_type)` is therefore
a unique key into `Graph.vector_indexes`, there is never ambiguity about which index
— and which metric — applies. A caller-supplied metric would be either redundant (same
as the declared metric) or actively wrong (HNSW's internal neighbour graph is built for
its declared metric; searching with a different one produces nonsensical results).
This contrasts with `vectorSimilarity`, which can run without any declared index (exact
brute-force) and in that case has no index config to infer the metric from — hence its
optional `metric` parameter.

Reads the `source_prop` `FloatVector` from the incoming `Vertex/Edge` traverser and
returns the k most similar entities from the `target_prop` index. Requires a declared
vector index — raises `VectorError::NoVectorIndex` otherwise.

```
nearestBy("q_embedding", "a_embedding", k, VectorEntityType.VERTEX)   →   Vertex × k
```

- The **source property** (query vector origin) is the first parameter.
- The **target index** (which index to search) is the second parameter.
- Source and target may differ, as long as both share the same dimension and model.
- `local()` is recommended for multi-source traversals to make per-traverser
  scoping explicit; for a single source, `nearestBy` can be used without `local()`.

Use cases:

**Same-index similarity** (P11 — find vertices similar to a single source):
```python
rs.traversal().V(xx) \
    .nearestBy("embedding", "embedding", k, VectorEntityType.VERTEX) \
    .to_list()
```

**Same-index similarity, multiple sources** — `local()` makes per-traverser scoping explicit:
```python
rs.traversal().V().has("age", between(20, 30)) \
    .local(__.nearestBy("embedding", "embedding", k, VectorEntityType.VERTEX)) \
    .to_list()
```

**Cross-index similarity** (P11 variant — query vector on one property, search a
different index; both must share the same dimension and embedding model):
```python
rs.traversal().V().hasLabel("question") \
    .local(__.nearestBy("q_embedding", "a_embedding", k, VectorEntityType.VERTEX)) \
    .to_list()
```

---

### 3b. `vectorNear` — syntactic sugar and ANN hint

`vectorNear(prop, query_vec, k)` is retained as **syntactic sugar** for the common
case and as the explicit surface for ANN execution hints.

```python
# Sugar form
rs.traversal().V().vectorNear("embedding", query_vec, k)

# Expands logically to:
rs.traversal().V() \
    .order().by(__.vectorSimilarity("embedding", query_vec), desc) \
    .limit(k)
```

The sugar form is shorter and unambiguously signals "use the ANN index." It is also
the only attachment point for execution hint modulators — the `order().by().limit()`
form has no step to attach them to (see §4 C3):

| Modulator               | Effect                                                                |
| ----------------------- | --------------------------------------------------------------------- |
| `withEfSearch(ef)`      | Override HNSW `ef_search` for this query                              |
| `withOverfetch(factor)` | Fetch `k × factor` candidates before applying graph predicate filters |

**Skip/range interaction**: `vectorNear(prop, q, k).skip(n)` or `.range(s, e)` is
handled automatically by the planner overfetch rule — the user writes plain Gremlin,
no modulator needed. See §4 C6 and pattern P12.

---

### 3c. Removed from previous draft

| Removed                                             | Replacement                                                                 |
| --------------------------------------------------- | --------------------------------------------------------------------------- |
| `vectorDistance(query_vec)`                         | `vectorSimilarity(prop, query_vec)` — higher = more similar for all metrics |
| `withScore()` modulator                             | `project().by(identity()).by(__.vectorSimilarity(prop, q))`                 |
| `withMinScore(t)` / `withMaxDistance(t)` modulators | `where(__.vectorSimilarity(prop, q).is_(gt(t)))`                            |
| `ScoredVertex { vertex, score }`                    | `Map { "vertex": Vertex, "similarity": f32 }` via `project()`               |
| `ScoredEdge { edge, score }`                        | `Map { "edge": Edge, "similarity": f32 }` via `project()`                   |

---

## 4. Design decisions

### C1 — `vectorSimilarity` direction and naming

**Decision**: `vectorSimilarity` is the single step for all metrics. The engine
normalises each metric to a **higher = more similar** value in [0, 1] (or (0, 1)):

| Metric         | Raw value     | Emitted similarity                |
| -------------- | ------------- | --------------------------------- |
| Cosine         | sim ∈ [0, 1]  | `cosine_sim` (no transform)       |
| Euclidean (L2) | dist ∈ [0, ∞) | `1 / (1 + l2_dist)` → (0, 1]      |
| InnerProduct   | unbounded     | `sigmoid(inner_product)` → (0, 1) |

`order(..., desc)` consistently means "most similar first" for all metrics.
A similarity of 0.97 means "very close" for both Cosine and Euclidean — no
hidden sign-flip confusion. The metric is inferred from the declared index config;
it is an explicit parameter only when no index is declared.

---

### C2 — `nearestBy` entity type disambiguation

**One index per `(entity_type, property_name)` pair.** `add_vector_index` raises
`VectorError::IndexAlreadyExists` if a second index is declared for the same pair.
The `Graph` struct's `vector_indexes` map is keyed by `(VectorEntityType, SmolStr)`,
enforcing this at the data-structure level. Consequence: `(target_prop, entity_type)`
is always a unique identifier for an index, which is why `nearestBy` needs no metric
or algorithm parameter — there is never more than one candidate index to choose from.

**Why multiple metrics per property do not arise in practice.** An embedding model's
distance metric is not a free parameter chosen at query time — it is baked into the
model's training objective (loss function):

- **Cosine** — contrastive loss on unit-normalised vectors (most sentence-transformers,
  OpenAI `text-embedding-*`, most modern LLM pooling models). Using L2 on these gives
  wrong rankings because it conflates direction with magnitude (which is always 1, so L2
  reduces to angular distance — but with different normalisation assumptions).
- **Inner product** — bi-encoder retrieval models (some CLIP variants, dense retrieval
  models not explicitly normalised). Not interchangeable with cosine on un-normalised
  vectors.
- **Euclidean (L2)** — metric-learning models (image embedding, face recognition). Cosine
  on these gives wrong rankings because the model places similar items close in L2 space,
  not on the same angular ray.

Using the wrong metric for an embedding model does not raise an error — it silently
degrades retrieval quality. The metric must match what the model was trained with.

**Consequence for the API**: a property that stores embeddings from one model has one
meaningful distance metric. The one-index-per-pair constraint mirrors this physical
reality. There is no valid production use case for a Cosine index and a Euclidean index
on the same `"embedding"` property — one of the two would produce wrong results.

The one case that might appear to need two metrics — unit-normalised vectors where
cosine and inner product give identical rankings — still only needs one index, since the
rankings are the same regardless of which label is declared.

**Decision**: explicit `entity_type` parameter, always required.
`nearestBy("src", "a_embedding", k, VectorEntityType.VERTEX)` is unambiguous
because the one-index-per-pair constraint guarantees at most one vertex index and
at most one edge index for any given property name. The verbosity is acceptable
because `nearestBy` is a rare step (P11 only) and the entity type is semantically
meaningful information the user knows.

**Multiple indexes on the same property are not supported.** If embeddings from two
different models (or two different metrics) need to coexist on the same entity, store
them as two separate named properties (`emb_cosine`, `emb_ip`) and declare one index
per property. This is the same model Neo4j and Milvus use, and it maps cleanly to
the physical reality that each embedding model has exactly one correct distance metric.

---

### C3 — ANN hint surface for `order().by().limit()` form

**Decision**: `vectorNear` is the only hint surface. Users who need execution
control (`withEfSearch`, `withOverfetch`) write the sugar form; others write the
core `order().by(vectorSimilarity).limit()` form. Support for attaching hints to
the native form is left open for a future version.

---

### C4 — `vectorSimilarity` recomputation

**Decision**: ANN indexes (HNSW) return similarity scores alongside candidate
IDs at no extra cost — the scores are a byproduct of the index scan. The engine
attaches these scores to traversers during the `order().by().limit()` barrier. A
subsequent `vectorSimilarity(prop, q)` call with the same `(prop_name, query_vec)` reads
from this traverser-level cache instead of recomputing.

```python
.order().by(__.vectorSimilarity("embedding", q), desc)  # ANN scan: scores cached
.limit(k)
.project("vertex", "similarity")
  .by(identity())
  .by(__.vectorSimilarity("embedding", q))              # cache hit — no recompute
```

Cache is traverser-scoped and query-scoped; GC'd when the traversal completes.
For v0.1 BruteForce scans, the cache still applies (distances are computed during
`order()` and reused in `project()`). Implementation target: v0.2.

---

### C5 — `nearestBy` per-traverser execution

`local(__.nearestBy("embedding", "embedding", k, VectorEntityType.VERTEX))` on N source
vertices logically runs N ANN searches. The API form is fixed and correct.

Whether the engine executes these as N serial calls or as one batched multi-query
call (when the underlying ANN library supports it) is a pure optimizer decision —
transparent to the user and requiring no API change. The optimizer can ship this
as a performance improvement in any future version without touching the query surface.

---

### C6 — Skip/range overfetch rule

ANN libraries including usearch do not support stateless cursor pagination —
`search(query, k=10)` returns the 10 nearest candidates; there is no "start from
position 11" API. A naive `vectorNear(q, 10).skip(10)` would discard all 10 results
and return nothing.

**Decision**: the query planner intercepts `skip(n)` or `range(s, e)` immediately
downstream of an ANN-driven step and increases the ANN fetch count to `n + k` (or
`e`). The skip/range is applied in memory to the ordered result list. The user writes
idiomatic Gremlin; the planner handles the overfetch automatically.

```python
# User writes:
.vectorNear("embedding", q, 10).skip(10)
# Planner fetches 20 from ANN, returns positions [10:20].

# User writes:
.order().by(__.vectorSimilarity("embedding", q), desc).range(20, 30)
# Planner fetches 30 from ANN, returns positions [20:30].
```

**`withOverfetch` is a separate concern**: `withOverfetch(factor)` multiplies the ANN
fetch count for pre-filter scenarios (some candidates will be rejected by a subsequent
`has()`/`where()`). Skip/range overfetch is additive and computed exactly from the
skip value — it does not interact with `withOverfetch`, and neither modulator is needed
for plain pagination.

**Maximum effective depth**: usearch accepts any `k` up to `index.size()`. However,
HNSW search accuracy at high `k` degrades relative to the configured `ef_search`. For
reliable results beyond position ~`ef_search / 2`, raise `ef_search` via `withEfSearch`
proportionally. The practical recommendation is to keep skip depths ≤ 200 with the
default `ef_search=50`; for deeper pagination, increase `withEfSearch` or prefer a
single large-`k` call over repeated paged calls.

---

## 5. Support plan

Tracks which steps, patterns, and lifecycle operations ship in each version.

| Version   | Query steps and patterns                                                                                                                                                                                                                                                                 | Lifecycle and write                                                                                                                      |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| **v0.1**  | `vectorSimilarity(prop, q)` map step; `order().by(vectorSimilarity).limit()` using BruteForce exact scan (no index needed); `where(vectorSimilarity.is_(gt(t)))` threshold (exact); `vectorNear` sugar (BruteForce); P1–P10 all work via exact scan; P11 (`nearestBy`) not yet available | Declare index at open (`BruteForce`); insert / update / delete vector properties; un-indexed `FloatVector` store and read                |
| **v0.2**  | Optimizer rewrites `order().by(vectorSimilarity).limit()` to HNSW index scan for large streams; similarity score cache on traverser (C4); `nearestBy(source_prop, target_prop, k, entity_type)` inside `local()` (P11 available); `vectorNear` sugar (HNSW); planner skip/range overfetch rule (P12, C6); all P1–P12 patterns | `add_vector_index`; `drop_vector_index`; `rebuild_vector_index`; snapshot export / import (`export_vector_index`, `import_vector_index`) |
| **v0.3**  | Optimizer pre-filter for P2/P3: `V().has*().order().by(vectorSimilarity).limit()` uses HNSW with eligible-key filter (exact recall within filtered set); optimizer batches `nearestBy` calls when ANN lib supports multi-query (transparent); `add_vector_index_async` progress handle   | `change_vector_index_algorithm`; `add_vector_index_async`; offline batch reindex via SST + `rebuild_vector_index` (§8c) |
| **v0.4**  | RaBitQ quantization (`Quantization::RaBitQ`); `VectorIndexStats.quantization` introspection                                                                                                                                                                                               | `rebuild_vector_index` after quantization change                                                            |
| **v0.5+** | Multi-query single-index: `vectorNear("emb", [q1, q2], k, fusion="rrf")`; streaming cursors for ANN results                                                                                                                                                                              | —                                                                                                           |

**Notes**:
- v0.1 exact scan is always correct but O(N) — suitable for prototyping and small
  graphs (< 100K vectors).
- v0.2 ANN index scan is approximate but sub-linear. The `order().by().limit()` API
  is identical; only the execution changes.
- v0.3 pre-filter restores exact-within-filter semantics for P2/P3 without falling
  back to post-filter approximation.

---

## 6. Index lifecycle management

All vector indexes must be declared before ANN operations (`vectorNear`,
`nearestBy`, optimizer index scan). `order().by(vectorSimilarity).limit()` and
`where(vectorSimilarity)` work without a declared index via exact inline computation.

**Structural config** (`VectorIndexConfig`: dimension, metric, algorithm) is declared
once via `SchemaSession::add_vector_index()`, persisted to CF_SCHEMA, and reloaded
automatically on every subsequent `Graph::open`. It is never re-supplied at open
time — the database file is the source of truth.

**Environmental config** (`VectorRuntimeOptions`: `default_limit`, `per_index_overrides`) is supplied
per-open via `GraphOptions` and is never written to disk. This prevents a server-side
memory limit from being baked into a database file and crashing a client machine with
less RAM.

See `design_vector_index_declaration.md` for the rationale behind requiring explicit
declaration rather than implicit auto-creation.

### 6a. Declare index (once, via SchemaSession — persisted to CF_SCHEMA)

An index is declared once via `SchemaSession`. The structural parameters persist
to CF_SCHEMA and are reloaded automatically on every subsequent `Graph::open`.
You do not re-declare the index at open time.

```rust
// Rust — first-time declaration (run once, survives all future opens)
let mut sess = g.open_schema();
sess.add_vector_index(VectorIndexConfig {
    entity_type: VectorEntityType::Vertex,
    property:    "embedding",
    dimension:   1536,
    metric:      DistanceMetric::Cosine,
    algorithm:   AnnAlgorithm::Hnsw { m: 16, ef_construction: 200 },
});
sess.commit()?;

// Subsequent opens — indexes load from CF_SCHEMA automatically; nothing to configure
let g = Graph::open(path)?;

// Per-open memory cap (environmental, never written to disk)
let g = Graph::open_with_options(path, GraphOptions {
    vector_runtime: VectorRuntimeOptions {
        default_limit: Some(VectorIndexLimit {
            memory_limit_bytes: 5 * 1024 * 1024 * 1024, // 5 GB for all indexes
        }),
        per_index_overrides: vec![
            IndexLimitOverride {
                entity_type: VectorEntityType::Vertex,
                property:    "large_doc_embedding".into(),
                limit:       VectorIndexLimit { memory_limit_bytes: 8 * 1024 * 1024 * 1024 },
            },
        ],
    },
    ..Default::default()
})?;
```

```python
# Python — first-time declaration
with g.open_schema() as sess:
    sess.add_vector_index(VectorIndexConfig(
        entity_type = VectorEntityType.VERTEX,
        property    = "embedding",
        dimension   = 1536,
        metric      = DistanceMetric.COSINE,
        algorithm   = HnswConfig(m=16, ef_construction=200),
    ))
    sess.commit()

# Subsequent opens — nothing to declare; indexes reload from CF_SCHEMA
g = Graph(path)

# Per-open memory cap (environmental — set global cap and optional per-index overrides).
# Python accepts flat kwargs (vector_memory_limit, vector_index_limits); the binding
# constructs VectorRuntimeOptions internally. Pass vector_runtime=VectorRuntimeOptions(...)
# directly for full control.
g = Graph(
    path,
    vector_memory_limit=5 * 1024 ** 3,  # 5 GB global default
    vector_index_limits=[IndexLimit(entity_type=VectorEntityType.VERTEX, property="large_doc", memory_limit=8 * 1024 ** 3)],
)
```

**Structural type** — persisted to CF_SCHEMA, portable across machines:

```rust
pub struct VectorIndexConfig {
    pub entity_type: VectorEntityType,
    pub property:    String,
    pub dimension:   usize,
    pub metric:      DistanceMetric,
    pub algorithm:   AnnAlgorithm,
}
```

**Environmental type** — supplied per-open, never persisted:

```rust
#[derive(Debug, Clone)]
pub struct VectorIndexLimit {
    pub memory_limit_bytes: usize,  // must be > 0; use default_limit: None for unlimited
}

pub struct IndexLimitOverride {
    pub entity_type: VectorEntityType,
    pub property:    SmolStr,
    pub limit:       VectorIndexLimit,
}

pub struct VectorRuntimeOptions {
    /// Default limit applied to every vector index.
    /// None = unlimited (expert escape hatch).
    pub default_limit: Option<VectorIndexLimit>,

    /// Per-index overrides matched by (entity_type, property).
    /// Takes precedence over default_limit. Indexes with no matching
    /// override fall back to default_limit; if that is also None, unlimited.
    pub per_index_overrides: Vec<IndexLimitOverride>,
}
```

**Algorithm values**:

```rust
pub enum AnnAlgorithm {
    BruteForce,                                     // v0.1
    Hnsw { m: usize, ef_construction: usize },      // v0.2
}
```

**Metric values**:

```rust
pub enum DistanceMetric {
    Cosine,
    Euclidean,    // L2 distance
    InnerProduct, // dot product; caller must normalise for cosine-equivalent behaviour
}
```

**Ships in**: v0.1 (`BruteForce`), v0.2 (`Hnsw`).  
`VectorRuntimeOptions` and `GraphOptions::vector_runtime` ship alongside v0.2 HNSW.

---

### 6b. Add index to a running graph

```python
# Python — blocking (through SchemaSession)
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(
        entity_type = VectorEntityType.VERTEX,
        property    = "title_embedding",
        dimension   = 384,
        metric      = DistanceMetric.COSINE,
        algorithm   = HnswConfig(m=16, ef_construction=200),
    ))
    mgmt.commit()

# Python — non-blocking (v0.3)
with g.open_schema() as mgmt:
    handle = mgmt.add_vector_index_async(config)
    mgmt.commit()
stats = handle.stats()   # RebuildStats(phase="scanning", progress=0.42)
handle.wait()
```

**Error**: `VectorError::IndexAlreadyExists`.  
**Ships in**: v0.2 (blocking). Non-blocking: v0.3.

---

### 6c. Drop an index

```python
g.drop_vector_index(entity_type=VectorEntityType.VERTEX, property="embedding")
```

Removes the index and snapshot. Property values remain in the props CF.  
**Error**: `VectorError::IndexNotFound`.  
**Ships in**: v0.2.

---

### 6d. Trigger explicit rebuild

```python
g.rebuild_vector_index(entity_type=VectorEntityType.VERTEX, property="embedding")
```

Forces full rebuild from props CF, discarding current in-memory state. The call
blocks until the rebuild is complete (3–15 min for large indexes). Queries that
arrive during the rebuild block on the `RwLock` write lock. Use at a maintenance
window or call `add_vector_index_async` (v0.3) for a non-blocking variant.  
**Ships in**: v0.2 (blocking). Non-blocking background variant: v0.3.

---

### 6e. Query index statistics

```rust
pub struct VectorIndexStats {
    pub entry_count:         u64,
    pub tombstone_count:     u64,
    pub tombstone_ratio:     f32,
    pub memory_bytes:        u64,
    pub last_replayed_timestamp:   u64,
    pub snapshot_path:       Option<PathBuf>,
    pub rebuild_in_progress: bool,
    pub rebuild_progress:    Option<f32>,
    pub algorithm:           AnnAlgorithm,
    pub metric:              DistanceMetric,
    pub dimension:           usize,
}
```

```python
stats = g.vector_index_stats(entity_type=VectorEntityType.VERTEX, property="embedding")
print(f"entries={stats.entry_count} tombstones={stats.tombstone_ratio:.1%}")
```

**Ships in**: v0.2.

---

### 6f. Change index algorithm

```python
g.change_vector_index_algorithm(
    entity_type = VectorEntityType.VERTEX,
    property    = "embedding",
    algorithm   = HnswConfig(m=16, ef_construction=200),
)
```

Old algorithm remains searchable until rebuild completes; new algorithm swaps in
atomically. Metric and dimension cannot change via this method.  
**Ships in**: v0.3.

---

## 7. Write path

All vector mutations go through the normal traversal API. The engine detects
`FloatVector` values in `property()` calls and routes them through the WAL and
index update automatically.

### 7a. Insert entity with vector property

```python
tx = g.tx()
tx.traversal().addV("doc") \
    .property("id", 1) \
    .property("title", "Hello World") \
    .property("embedding", Vector(model.encode("Hello World"))) \
    .next()
tx.commit()
```

`Vector` accepts `list[float]`, `np.ndarray`, or `bytes`. Elements cast to `f32`.  
**Ships in**: v0.1.

---

### 7b. Update a vector property

```python
tx = g.tx()
tx.traversal().V(1).property("embedding", Vector(new_embedding)).next()
tx.commit()
```

Issues a WAL Delete for the old vector and WAL Insert for the new in one
`WriteBatch`.  
**Ships in**: v0.1.

---

### 7c. Remove vector property, keep entity

```python
tx = g.tx()
tx.traversal().V(1).properties("embedding").drop().iterate()
tx.commit()
```

**Ships in**: v0.1.

---

### 7d. Remove entity — implicit vector cleanup

Dropping a vertex or edge issues a WAL Delete for every indexed `FloatVector`
property it carried.

```python
tx = g.tx()
tx.traversal().V(1).drop().iterate()
tx.commit()
```

**Ships in**: v0.1.

---

### 7e. Batch mutations in one transaction

```python
tx = g.tx()
for doc_id, text in corpus[:1000]:
    tx.traversal().addV("doc") \
        .property("id", doc_id) \
        .property("embedding", Vector(model.encode(text))) \
        .next()
tx.commit()
# One WriteBatch, one fsync, one WAL counter block
```

For initial corpus loads larger than ~10K vectors, use `SstBulkLoader` (see `docs/api/design_api_overview.md` §4b) followed by `add_vector_index`.  
**Ships in**: v0.1.

---

### 7f. Un-indexed FloatVector properties

Vertices and edges can carry `FloatVector` properties with no declared index.
The behaviour under query steps:

- **`order().by(__.vectorSimilarity(prop, q, metric)).limit(k)`** — works without
  an index. `metric` parameter required (no index to infer from). Optimizer computes
  similarities inline (exact brute-force). Correct but O(N) on large graphs.
- **`where(__.vectorSimilarity(prop, q, metric).is_(gt(t)))`** — works without an
  index. Exact inline computation.
- **`vectorNear(prop, q, k)`** — raises `VectorError::NoVectorIndex`. The sugar
  form explicitly requests ANN index usage.
- **`nearestBy(source_prop, target_prop, k, entity_type)`** — raises
  `VectorError::NoVectorIndex`. Requires a declared index to search.

```python
tx.traversal().addV("doc").property("raw_vec", Vector(v)).next()
tx.commit()

rs.traversal().V(1).values("raw_vec").next()                            # ✅ read value
rs.traversal().V()                                                      # ✅ brute-force
    .order().by(__.vectorSimilarity("raw_vec", q), desc).limit(10)
rs.traversal().V().vectorNear("raw_vec", q, k=5).to_list()             # ❌ NoVectorIndex
```

**Ships in**: v0.1.

---

## 8. Bulk operations

For large initial graph loads (vertices, edges, and `FloatVector` properties),
use `SstBulkLoader` from the general graph API — it writes all data via SST
ingestion, bypassing the WAL entirely. After the load, `add_vector_index` builds
the vector index in one scan of the props CF. See `docs/api/design_api_overview.md`
§4b for the full pipeline.

### 8a. Offline index build from unindexed data

`add_vector_index` (through `open_schema()`) scans the props CF for all
`FloatVector` values matching the property and builds the index in one pass.
This is the standard way to index data that already exists in the graph, whether
it arrived via normal transactions or SST bulk load.

```python
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(
        entity_type = VectorEntityType.VERTEX,
        property    = "embedding",
        dimension   = 1536,
        metric      = DistanceMetric.COSINE,
        algorithm   = HnswConfig(m=16, ef_construction=200),
    ))
    mgmt.commit()
```

**Ships in**: v0.2.

---

### 8b. Index snapshot export / import

```python
g.export_vector_index(
    entity_type = VectorEntityType.VERTEX,
    property    = "embedding",
    path        = "/backups/graph-2026-07-30-embedding.vix",
)

g.import_vector_index(
    entity_type = VectorEntityType.VERTEX,
    property    = "embedding",
    path        = "/backups/graph-2026-07-30-embedding.vix",
)
```

**Error**: `VectorError::SnapshotSeqAhead` if snapshot is from a future WAL state.  
**Ships in**: v0.2.

---

### 8c. Offline batch reindex — new embedding model

```python
# Step 1: declare new index (add_property_key optional — add_vector_index
#          implicitly registers "embedding_v2" as DataType::FloatVector)
with g.open_schema() as mgmt:
    mgmt.add_vector_index(VectorIndexConfig(
        entity_type=VectorEntityType.VERTEX, property="embedding_v2",
        dimension=3072, metric=DistanceMetric.COSINE,
        algorithm=HnswConfig(m=16, ef_construction=200),
    ))
    mgmt.commit()

# Step 2: write new embeddings via normal transactions
for vid, text in all_docs():
    tx = g.tx()
    tx.traversal().V(vid).property("embedding_v2", Vector(new_model.encode(text))).next()
    tx.commit()

# Step 3: drop old index
with g.open_schema() as mgmt:
    mgmt.drop_vector_index(entity_type=VectorEntityType.VERTEX, property="embedding")
    mgmt.commit()
```

**Ships in**: v0.2 (steps 1 and 3). Step 2 uses the normal transaction write
path available from v0.1. For very large reindexing jobs, use `SstBulkLoader`
to write the new embeddings and then call `g.rebuild_vector_index` instead of
individual transactions.

---

## 9. Error types

| Variant                                            | When raised                                                                                                                                          |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `VectorError::NoVectorIndex { property }`          | `vectorNear` or `nearestBy` used on a property with no declared index. `vectorSimilarity` does NOT raise this — it computes inline without an index. |
| `VectorError::DimensionMismatch { expected, got }` | Vector inserted or queried with wrong dimension                                                                                                      |
| `VectorError::IndexAlreadyExists`                  | `add_vector_index` called for an existing `(entity_type, property)`                                                                                  |
| `VectorError::IndexNotFound`                       | `drop_vector_index`, `rebuild_vector_index`, or `export_vector_index` on non-existent index                                                          |
| `VectorError::MetricRequired { property }`         | `vectorSimilarity` used on a property with no declared index and no explicit `metric` parameter                                                      |
| `VectorError::AmbiguousIndex { property }`         | Removed — `nearestBy` now always requires explicit `entity_type` parameter (see C2)                                                                  |
| `VectorError::RebuildInProgress`                   | `add_vector_index` or `rebuild_vector_index` called while a rebuild is running                                                                       |
| `VectorError::SnapshotSeqAhead`                    | `import_vector_index` snapshot is from a future WAL state                                                                                            |
| `VectorError::SnapshotCorrupt`                     | Snapshot file has invalid magic, truncated data, or checksum mismatch                                                                                |
| `VectorError::WrongAlgorithmParam`                 | `withEfSearch` used on `BruteForce` (no ef_search concept)                                                                                           |
| `VectorError::InvalidParam { param, reason }`      | `withOverfetch` < 1.0, `k` = 0, or other invalid parameter                                                                                           |
| `VectorError::NotImplemented { feature }`          | Feature called before its implementation version ships                                                                                               |
| `VectorError::MemoryLimitExceeded { current, limit, estimated }` | Insert rejected before WAL write because estimated memory would exceed `memory_limit_bytes`; also returned during WAL replay (entry is skipped, not fatal) |

**Removed**: `VectorError::InvalidModulator` — previously raised when `withScore` /
`withMinScore` were used out of sequence. Those steps no longer exist.

Python: all are subclasses of `rocksgraph.VectorError` < `rocksgraph.RocksGraphError`.  
TypeScript: all extend `VectorError` which extends `Error`.

---

## 10. What is explicitly out of scope

| Feature                                             | Why excluded                                                                                                                                                               |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Sparse vectors (SPLADE, BM25)                       | Requires an inverted index (separate design doc)                                                                                                                           |
| Hybrid BM25 + dense vector                          | Compound ranking requires inverted index; graph predicates serve this role for now                                                                                         |
| GPU acceleration                                    | Embedded DB target is CPU; deployment complexity incompatible with zero-dependency goal                                                                                    |
| Multi-vector per property (`List<FloatVector>`)     | Recommended pattern: one "chunk" vertex per sub-vector with edges to the parent                                                                                            |
| Streaming cursors for ANN results                   | `to_list()` fetches all; streaming variant is v0.5                                                                                                                         |
| Cross-graph ANN (federated search)                  | Out of scope for embedded single-process DB                                                                                                                                |
| Cross-index union in one step                       | `union()` covers this without a new step (P10)                                                                                                                             |
| Multi-query single-index fusion                     | `union()` approximation runs two separate ANN searches and deduplicates — not true batch RRF. Native form `vectorNear("emb", [q1, q2], k, fusion="rrf")` deferred to v0.5+ |
| Custom distance metric functions                    | Reserved for v0.4+; placeholder: `DistanceMetric::Custom(fn)`                                                                                                              |
| `vectorNear` accepting anonymous traversal as query | Subsumed by `local(nearestBy())` pattern; deferred if still needed                                                                                                         |
