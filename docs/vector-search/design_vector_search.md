# Design: Vector Search in RocksGraph

Status: proposal — all decisions settled below; ready for implementation review.

---

## Table of Contents

- [Design: Vector Search in RocksGraph](#design-vector-search-in-rocksgraph)
  - [Table of Contents](#table-of-contents)
  - [1. Motivation](#1-motivation)
  - [2. Prior art: how other databases architect this](#2-prior-art-how-other-databases-architect-this)
    - [Pattern A — tight WAL coupling](#pattern-a--tight-wal-coupling)
    - [Pattern B — dual checkpoint + sequence-number replay](#pattern-b--dual-checkpoint--sequence-number-replay)
    - [Pattern C — growing segment + sealed segment](#pattern-c--growing-segment--sealed-segment)
    - [Pattern D — versioned snapshots](#pattern-d--versioned-snapshots)
    - [Comparison matrix](#comparison-matrix)
    - [Chosen approach: Pattern B](#chosen-approach-pattern-b)
  - [3. Architecture overview](#3-architecture-overview)
  - [4. Type system: `GValue::FloatVector`](#4-type-system-gvaluefloatvector)
    - [4a. Why not `GValue::List` of floats](#4a-why-not-gvaluelist-of-floats)
    - [4b. Hash implementation](#4b-hash-implementation)
    - [4c. Python API: explicit `Vector([...])` wrapper](#4c-python-api-explicit-vector-wrapper)
    - [4d. JavaScript/TypeScript API](#4d-javascripttypescript-api)
  - [5. Index configuration](#5-index-configuration)
    - [5a. Entity types: vertex and edge vector indexes](#5a-entity-types-vertex-and-edge-vector-indexes)
    - [5b. Named explicit indexes](#5b-named-explicit-indexes)
    - [5c. Un-indexed `FloatVector` properties](#5c-un-indexed-floatvector-properties)
    - [5d. Dimension validation on insert](#5d-dimension-validation-on-insert)
  - [6. Query API](#6-query-api)
  - [7. Algorithm design](#7-algorithm-design)
  - [8. `VectorIndex` trait](#8-vectorindex-trait)
    - [8a. Entity key](#8a-entity-key)
    - [8b. Trait definition](#8b-trait-definition)
    - [8c. `Graph` struct](#8c-graph-struct)
  - [9. Concurrency model](#9-concurrency-model)
  - [10. Crash consistency](#10-crash-consistency)
  - [11. Performance expectations](#11-performance-expectations)
  - [12. Out of scope](#12-out-of-scope)

---

## 1. Motivation

Embeddings are the standard representation for unstructured data (text, images,
audio) as dense float vectors. Graph + vector is the most requested feature
combination: the graph handles explicit relationships, vectors handle semantic
similarity. Together they enable RAG pipelines, recommendations, and multi-modal
search in a single embedded database — no separate vector DB process, no network
hop, no dual-write consistency problem.

---

## 2. Prior art: how other databases architect this

### Pattern A — tight WAL coupling

Used by **pgvector**, **Neo4j**, **Cassandra SAI**.

The vector index is a first-class citizen of the primary transaction system.
In pgvector, HNSW index entries live in PostgreSQL's buffer pool and are covered
by the same WAL as heap data. One WAL write covers the row and the index entry.
Crash recovery replays both atomically.

**Tradeoff**: true ACID by construction, but the index format must fit the WAL
and buffer-pool model. pgvector had to rewrite HNSW internals to match
PostgreSQL's page layout. Using external ANN libraries unchanged is impractical.

### Pattern B — dual checkpoint + sequence-number replay

Used by **Qdrant**, **Weaviate**, and **this proposal**.

The primary store has its own WAL. The vector index has a separate snapshot file
that records the sequence number of the last operation it ingested. On recovery:

1. Load the latest HNSW snapshot (last ingested seq = S)
2. Scan the primary WAL for vector mutations since S
3. Replay them into the index

Weaviate makes the WAL concrete with a dedicated "HNSW commit log" — an
append-only file of `(insert/delete, vertex_id, vector)` entries separate from
the object WAL. Qdrant uses the same idea with an explicit `wal.bin` keyed by
operation ID.

**Tradeoff**: can use any ANN library unchanged; flexible. Small inconsistency
window between commit and in-memory index update — closed by recovery replay.

### Pattern C — growing segment + sealed segment

Used by **Milvus**, **Elasticsearch/Lucene**.

Writes go into a mutable "growing segment" backed by brute-force search. When
the segment reaches a size threshold it is sealed: an offline ANN index is built
and the segment becomes immutable. Queries fan out across all segments and merge
results.

**Tradeoff**: simple consistency (growing segment is authoritative for recent
writes); brute-force on new data is immediately correct. Query complexity
increases as segment count grows. Overkill for a single-process embedded DB.

### Pattern D — versioned snapshots

Used by **LanceDB**.

Every write creates a new immutable version of the dataset. The ANN index is
built per-version and tied to it. There is no "index out of sync" problem
because the index is never mutated — a new one is built when the data version
advances.

**Tradeoff**: index rebuild cost on every write batch. Only practical for
analytical / batch workloads.

### Comparison matrix

|                   | pgvector      | Neo4j         | Qdrant         | Milvus           | LanceDB     | **RocksGraph**                                     |
| ----------------- | ------------- | ------------- | -------------- | ---------------- | ----------- | -------------------------------------------------- |
| Pattern           | A             | A             | B              | C                | D           | **B**                                              |
| Vector storage    | Column        | Node property | Payload field  | Collection field | Column      | Vertex/edge property                               |
| ANN algorithm     | HNSW, IVFFlat | HNSW (Lucene) | HNSW           | IVF + HNSW       | IVF, HNSW   | HNSW (v0.2), BruteForce (v0.1)                     |
| Index rebuild     | Manual        | Manual        | Auto on insert | Auto on seal     | Per version | Lazy on write                                      |
| Crash consistency | WAL           | WAL           | WAL + seqno    | WAL              | Versioning  | Dedicated CF + seqno (§10, `design_vector_wal.md`) |
| Cold start        | Moderate      | Moderate      | Small          | Small            | Small       | Small (snapshot + WAL replay)                      |

### Chosen approach: Pattern B

Pattern B gives the best fit for RocksGraph:
- Any pure-Rust ANN library can be used unchanged
- Crash consistency is well-understood (see `design_vector_wal.md`)
- Single-process embedded DB — no distributed coordination needed
- WAL replay on startup is fast for typical graph sizes

---

## 3. Architecture overview

```
  ┌──────────────────────────────────────────────────────────────────────┐
  │  Traversal pipeline                                                  │
  │                                                                      │
  │  rs.traversal().V().hasLabel("doc")                                  │
  │    .nearest("embedding", query_vec, k)                            │
  │    .out("cites").values("title")                                     │
  └──────────────────┬──────────────────────────────────────┬───────────┘
        graph steps  │                                      │  nearest
                     ▼                                      ▼
  ┌───────────────────────────────┐         ┌───────────────────────────────┐
  │  Graph store (RocksDB)        │   WAL   │  VectorIndex   (per index)    │
  │                               │  replay │  in-memory                    │
  │  vertices CF                  │ ───────▶│                               │
  │  edges CF                     │ on open │  usearch HNSW graph           │
  │  props CF                     │         │  EntityKey ↔ Vec<f32>         │
  │  CF_VECTOR_WAL                │         │                               │
  │  __meta CF                    │         │  snapshot on disk             │
  │                               │         │  (last_replayed_timestamp)          │
  └───────────────────────────────┘         └───────────────────────────────┘
                  ▲                                          ▲
                  │                                          │
                  └──────────────────────┬───────────────────┘
                                         │
               ┌─────────────────────────┴──────────────────────────────┐
               │  TxSession::commit                                     │
               │                                                        │
               │  ① WriteBatch: graph mutations + WAL  (one fsync)      │
               │  ② index.insert / index.remove  (in-memory, after ①)  │
               └────────────────────────────────────────────────────────┘
```

Two independent layers:

- **Graph store**: all graph data in RocksDB. Source of truth. Durable on every
  commit. Owns `CF_VECTOR_WAL` which records every vector mutation alongside
  its graph mutation in one `WriteBatch` (one `fsync`). Each future index type
  gets its own separate WAL CF (e.g. `CF_TEXT_WAL`); see `design_vector_wal.md` §2.

- **VectorIndex**: in-memory ANN structure. Derived from the graph store.
  Rebuilt from a persisted snapshot + WAL replay on `Graph::open`. Updated in
  memory after each commit.

---

## 4. Type system: `GValue::FloatVector`

### 4a. Why not `GValue::List` of floats

Storing a 1536-dim embedding as a `List<Float64>` costs ~25 KB per vector (enum
tag + `Vec` overhead per element). The SIMD distance function receives a
`&[GValue]` slice and must branch on each element. Both costs are unacceptable.

A dedicated variant keeps vectors dense and contiguous:

```rust
pub enum GValue {
    // ... existing variants ...
    FloatVector(Vec<f32>),  // densely packed; slice maps directly to SIMD
}
```

~50 lines of Rust: one enum variant, encoder writes length-prefixed f32 LE
bytes, decoder reads them back.

### 4b. Hash implementation

`f32` does not implement `Hash` (`NaN != NaN`). `DedupStep` and other pipeline
steps use `HashSet<GValue>`. Fix: canonicalize NaN on hash, use bit-level
representation:

```rust
impl Hash for GValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            // ... existing arms ...
            GValue::FloatVector(v) => {
                for f in v {
                    // canonicalize all NaN bit patterns to a single one
                    let bits = if f.is_nan() { f32::NAN.to_bits() } else { f.to_bits() };
                    bits.hash(state);
                }
            }
        }
    }
}
```

`PartialEq` for `FloatVector` uses bitwise equality (NaN == NaN). This deviates
from IEEE 754 semantics intentionally: two stored vectors with NaN at the same
position are considered equal for deduplication purposes.

### 4c. Python API: explicit `Vector([...])` wrapper

**Decision: explicit wrapper, not auto-detect `list[float]`.**

Rationale:
- `list[float]` is ambiguous — a 3-element list could be a 3-dim embedding or
  a geographic coordinate. Auto-detect would silently mis-classify it.
- Consistent with existing explicit wrappers (`Int64`, `UInt16`, `Float32`).
- Clear user intent at write time.

```python
from rocksgraph import Graph, Vector

embedding = model.encode("hello world")   # numpy array or list[float]
tx.traversal().addV("doc") \
    .property("id", 1) \
    .property("title", "hello world") \
    .property("embedding", Vector(embedding)) \
    .next()
```

`Vector` accepts a `list[float]`, `np.ndarray` (cast to float32), or `bytes`.
It raises `TypeError` if elements cannot be cast to `f32`.

### 4d. JavaScript/TypeScript API

```typescript
import { Graph, Vector } from "rocksgraph";

tx.traversal().addV("doc")
    .property("id", 1)
    .property("embedding", new Vector(Float32Array.from(embedding)))
    .next();
```

`Vector` wraps a `Float32Array` (not `number[]`) — this signals dense binary
data and enables zero-copy transfer to Rust via napi-rs `Buffer`.

---

## 5. Index configuration

### 5a. Entity types: vertex and edge vector indexes

Vector properties can be stored on **both vertices and edges**. Each index
declaration carries an `entity_type` that determines which storage CF is
scanned at cold-start rebuild and which traverser type `nearest` produces.

```rust
pub enum VectorEntityType {
    Vertex,
    Edge,
}
```

The `Graph` struct keys `vector_indexes` by `(VectorEntityType, SmolStr)` so a
vertex `"embedding"` index and an edge `"embedding"` index can coexist without
collision:

```rust
pub struct Graph {
    db:             Arc<DB>,
    vector_indexes: HashMap<(VectorEntityType, SmolStr), Arc<RwLock<Box<dyn VectorIndex>>>>,
}
```

### 5b. Named explicit indexes

**Decision: declare indexes explicitly via `SchemaSession`, not implicitly and not at `Graph::open`.**

Rationale:
- Validates dimension on every insert — a 1537-dim vector inserted into a
  1536-dim index is caught immediately, not silently corrupted.
- User controls which properties consume HNSW memory.
- Multiple vector properties with different dimensions, metrics, or entity
  types are each independently configured.
- Structural parameters (dimension, metric, algorithm) are persisted once to CF_SCHEMA
  and reloaded automatically on every open — the database is the source of truth,
  not the call site.

```rust
// Declare once — persisted across all future opens
let mut sess = g.open_schema();
sess.add_vector_index(VectorIndexConfig {
    entity_type: VectorEntityType::Vertex,
    property:    "embedding",
    dimension:   1536,
    metric:      DistanceMetric::Cosine,
    algorithm:   AnnAlgorithm::Hnsw { m: 16, ef_construction: 200 },
});
sess.add_vector_index(VectorIndexConfig {
    entity_type: VectorEntityType::Edge,
    property:    "embedding",
    dimension:   768,
    metric:      DistanceMetric::Cosine,
    algorithm:   AnnAlgorithm::Hnsw { m: 16, ef_construction: 200 },
});
sess.commit()?;

// Subsequent opens — no re-declaration; indexes reload from CF_SCHEMA
let g = Graph::open(path)?;
```

### 5c. Un-indexed `FloatVector` properties

Both vertices and edges can have `FloatVector` properties not listed in
`vector_indexes`. They are stored in the respective props CF as raw bytes and
readable via `.values("prop")`.

Behaviour under query steps when no index is declared:

- **`order().by(__.similarity(prop, q, metric)).limit(k)`** — works; explicit
  `metric` parameter required (nothing to infer from). Optimizer computes similarities
  inline (exact brute-force, O(N)).
- **`where(__.similarity(prop, q, metric).is_(gt(t)))`** — works; explicit
  `metric` parameter required.
- **`similarity(prop, q)` (no metric)**  — raises `VectorError::MetricRequired`.
- **`nearest(prop, q, k)`** — raises `VectorError::NoVectorIndex`. The sugar form
  explicitly requests ANN index usage.
- **`neighbors(source_prop, target_prop, k, entity_type)`** — raises
  `VectorError::NoVectorIndex`. Requires a declared index to search.

### 5d. Dimension validation on insert

When `OP_PROPERTY` sets a `FloatVector`, the engine checks the declared
dimension for that `(entity_type, prop_key)` pair:

```rust
if let Some(config) = self.vector_index_config(entity_type, prop_key) {
    if vec.len() != config.dimension {
        return Err(StoreError::DimensionMismatch {
            property: prop_key.to_string(),
            expected: config.dimension,
            got:      vec.len(),
        });
    }
}
```

Surfaced to Python/JS as `rocksgraph.DimensionMismatchError`.

---

## 6. Query API

The core query steps are `similarity(prop, query_vec)` (map, `Vertex/Edge → f32`)
and `neighbors(source_prop, target_prop, k, entity_type)` (flat-map, per-traverser
ANN search). `nearest(prop, query_vec, k)` is syntactic sugar for the common
`order().by(similarity).limit(k)` pattern and is the ANN execution hint
attachment point. Scoring is expressed via `project().by(similarity(...))`;
threshold filtering via `where(similarity(...).is_(gt(t)))`. Full step
catalogue, scenario coverage, and stability guarantees are in `design_vector_api.md`.

Representative combined graph + vector examples:

```python
# 1. Vertex semantic search → graph traversal (no scores needed)
rs.traversal().V() \
    .nearest("embedding", Vector(query_vec), 5) \
    .out("cites").values("title") \
    .to_list()

# 2. Graph filter → vertex semantic ranking with scores
rs.traversal().V().has("status", "published") \
    .nearest("embedding", Vector(query_vec), 10) \
    .project("vertex", "similarity") \
      .by(identity()) \
      .by(__.similarity("embedding", Vector(query_vec))) \
    .to_list()

# 3. Multi-hop → vertex similarity
rs.traversal().V(author_id) \
    .out("wrote") \
    .nearest("embedding", Vector(query_vec), 3) \
    .to_list()

# 4. Edge semantic search → source vertices
rs.traversal().E() \
    .nearest("embedding", Vector(query_vec), 5) \
    .outV().values("name") \
    .to_list()

# 5. Traverse edges, then rank by edge vector similarity
rs.traversal().V(doc_id) \
    .outE("related") \
    .nearest("embedding", Vector(query_vec), 3) \
    .inV().values("title") \
    .to_list()
```

---

## 7. Algorithm design

v0.1 ships `BruteForceIndex` (linear scan, exact, no WAL overhead, rebuilds from
props CF on every open). v0.2 replaces it with `UsearchHnswIndex` backed by the
usearch crate (HNSW, O(log N) search, WAL + snapshot persistence). RaBitQ compression
(v0.4) follows behind the same `VectorIndex` trait. Algorithm selection rationale
(including why IVF was evaluated and rejected) is in
`design_ann_algorithm_and_library.md`; implementation details are in
`design_hnsw_impl.md`.

**Write-lock latency note (v0.1)**: `BruteForceIndex::search` holds the `RwLock`
read lock for the full linear scan — up to ~50ms at 100K × 1536 dims. A
concurrent `TxSession::commit` blocks on the write lock for this duration after
its RocksDB fsync completes, causing a visible commit stall. Acceptable at
prototyping scale; eliminated in v0.2 where HNSW search completes in <5ms.

---

## 8. `VectorIndex` trait

### 8a. Entity key

Edges have a composite identity `(src, dst, label, rank)` — a bare `i64` is
insufficient. A shared `EntityKey` enum covers both vertex and edge cases:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityKey {
    Vertex(i64),
    Edge(CanonicalEdgeKey),
}

// Reuses the existing type from rocksgraph/src/types/keys.rs.
// label_id is the schema-registered integer for the edge label string —
// all label strings are interned to i32 IDs at write time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalEdgeKey {
    pub src_id:   i64,
    pub label_id: i32,
    pub dst_id:   i64,
    pub rank:     u16,
}
```

`EntityKey` must implement `Hash + Eq` because HNSW implementations maintain
an internal `HashMap<EntityKey, usize>` mapping between external entity keys
and their internal contiguous node indices.

### 8b. Trait definition

The `load` function is a free function rather than a trait method to avoid
`dyn` object-safety problems (associated functions returning `Self` require
`Sized`):

```rust
pub trait VectorIndex: Send + Sync {
    fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<()>;
    fn remove(&mut self, key: &EntityKey) -> Result<()>;
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(EntityKey, f32)>>;
    fn save(&self, path: &Path, last_replayed_timestamp: u64) -> Result<()>;
    fn last_replayed_timestamp(&self) -> u64;
    fn set_last_replayed_timestamp(&mut self, seq: u64);  // used during WAL catch-up in online index build
}

// Free constructor — not part of the trait (avoids dyn incompatibility)
pub fn load_vector_index(path: &Path) -> Result<Box<dyn VectorIndex>> { ... }
```

`search` returns `(EntityKey, f32)` pairs in the ANN library's native ordering.
`NearestStep` normalises these raw values to a unified **higher = more similar**
score before caching them on traversers:

| Metric | usearch output | Normalised similarity |
|--------|---------------|----------------------|
| L2 / Euclidean | ascending distance ∈ [0, ∞) | `1 / (1 + dist)` → (0, 1] |
| InnerProduct | raw dot product, unbounded | `sigmoid(ip)` → (0, 1) |
| Cosine | **distance** `1 − cos(θ)` ∈ [0, 2] | `1 − (dist / 2)` → [0, 1] |

> **Implementation watch (usearch-specific):** usearch returns cosine *distance*
> (`1 − cos(θ)`), not cosine *similarity*. The normalization is `1 − (dist / 2)`,
> giving 1.0 for identical vectors and 0.0 for opposite vectors. Using `1 − dist`
> would only be correct for pre-normalized unit vectors (where the max distance is
> 1). Verify the formula when wiring up `UsearchHnswIndex::search` in v0.2.

`EntityKey::Vertex(id)` becomes a vertex traverser; `EntityKey::Edge(ek)` becomes
an edge traverser.

### 8c. `Graph` struct

The `vector_indexes` map is keyed by `(VectorEntityType, SmolStr)` so a vertex
`"embedding"` index and an edge `"embedding"` index can coexist without
collision. Wrapped for concurrent access (see `design_vector_concurrency.md`):

```rust
pub struct Graph {
    db:             Arc<DB>,
    vector_indexes: HashMap<(VectorEntityType, SmolStr), Arc<RwLock<Box<dyn VectorIndex>>>>,
}
```

---

## 9. Concurrency model

The naive WAL key design required a shared `Arc<Mutex<u64>>` that serialized
all vector-touching commits. The new composite key (`[prop_key_id][entity_type][ts][random]`)
eliminates that mutex: each session calls `fetch_add(1, AcqRel)` on a process-local
`AtomicU64` clock (~5 ns), then appends a 4-byte random suffix to prevent key
collision. Concurrent index mutations are still serialized by an
`Arc<RwLock<Box<dyn VectorIndex>>>` per index (readers share, writers
serialize). `RwLock` over lock-free usearch inserts was chosen to preserve
read-your-own-writes. Full analysis, lock-acquisition order, and option
comparison in `design_vector_concurrency.md`.

---

## 10. Crash consistency

Every vector mutation is co-committed with the graph mutation in a single
`WriteBatch` (one `fsync`), keyed by a 15-byte composite timestamp key in
`CF_VECTOR_WAL`. On `Graph::open`, the HNSW snapshot is loaded and the WAL is
replayed per-index via a prefix seek on `[prop_key_id][entity_type]`, applying
only entries with `ts > last_replayed_timestamp`. Full design (including the
rationale for separate WAL CFs per index type) in `design_vector_wal.md`.

---

## 11. Performance expectations

| Operation                             | Latency    | Notes                         |
| ------------------------------------- | ---------- | ----------------------------- |
| Point lookup `V(id)`                  | 10–50 μs   | 3 key comparisons in LSM tree |
| Edge traversal `out("label")`         | 50–200 μs  | prefix scan                   |
| `nearest` brute-force (100K, k=10) | ~50 ms     | AVX2, 1536 dims               |
| `nearest` HNSW (1M, k=10)          | 1–3 ms     | ~150 distance calcs           |
| Insert vertex + vector (HNSW)         | 2–10 ms    | ~800 distance calcs in index  |
| Commit (WAL sync)                     | 100–500 μs | single fsync                  |

**Memory at scale — HNSW, 1M × 1536 dims:**
- Raw vectors: 1M × 1536 × 4 B = **6.0 GB**
- HNSW adjacency lists (M=16, ~2 layers avg): 1M × 32 × 8 B ≈ **0.3 GB**
- Total: **~6.3 GB** in addition to RocksDB block cache

Users targeting >500K vectors should plan for this or enable RaBitQ (v0.4),
which reduces memory to ~190 MB for the same dataset.

---

## 12. Out of scope

- **Sparse vectors** (SPLADE, late-interaction) — dense embeddings cover 90%+
  of use cases
- **Hybrid BM25 + vector** — graph traversals already combine text predicates
  with vector steps; full BM25 requires an inverted index (separate design)
- **GPU acceleration** — embedded DB target is CPU; GPU adds deployment
  complexity inconsistent with the zero-dependency goal
- **Multi-vector per vertex property** — recommended pattern is to create
  separate "chunk" vertices with edges to the parent document (graph-native);
  no `List<FloatVector>` support planned

For the version roadmap, algorithm selection rationale, and the full catalogue of
settled decisions, see `design_roadmap.md`. Sub-documents covering each concern
in depth: `design_vector_api.md` (query API), `design_vector_wal.md` (crash
consistency), `design_vector_concurrency.md` (locking), `design_hnsw_impl.md`
(usearch integration), `design_ann_algorithm_and_library.md` (algorithm choice),
`design_vector_quantization.md` (f16/RaBitQ), `docs/api/design_api_overview.md`
(session model, language bindings, lifecycle states).
