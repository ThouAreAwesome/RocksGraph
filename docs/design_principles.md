# RocksGraph Design Principles

## Positioning

RocksGraph is **Gremlin-inspired, not Gremlin-compatible**.

It takes the core ideas of TinkerPop — pipeline-based traversal, vertex/edge duality,
lazy evaluation, composable steps — but makes deliberate departures where the TinkerPop
standard's design choices create unnecessary complexity, performance overhead, or
poor ergonomics in practice.

These departures are not gaps to be filled. They are intentional trade-offs made from
production experience with TinkerPop Gremlin.

---

## Why not comply with TinkerPop

TinkerPop Gremlin was designed as a universal, database-agnostic traversal language.
That goal introduced compromises that are felt in every real implementation:

**Forced full-property materialization.** `out()` returning a `Vertex` always fetches all
properties. There is no way to return a typed `Vertex`/`Edge` with partial properties.
The only alternatives — `valueMap()`, `elementMap()`, `project()` — lose the typed
structure. In practice, callers often need the typed result with just a few properties.

**Vertex label inconsistency.** A vertex label carries no structural weight — vertex ID
alone is globally unique. Yet the label adds API surface (`hasLabel()`, `Key::Label`),
a value-read cost at materialization, and modeling questions (one label or many? required
or optional?). This inconsistency with edge labels, where the label is genuinely
structural and always in-pipeline at zero cost, creates persistent asymmetry.

**The VertexProperty indirection.** TinkerPop wraps vertex properties in a `VertexProperty`
object to support multi-cardinality (a vertex can have multiple values for the same key).
This indirection surprises users — `vertex.property("name")` returns a `VertexProperty`,
not a value — and adds allocation overhead for the overwhelmingly common single-value case.

**Dynamic typing throughout.** A traversal's return type is not known at compile time.
Whether `.next()` returns a `Vertex`, an `Integer`, or a `Map` depends on the step chain
at runtime. This makes Rust integration awkward and prevents the compiler from catching
misuse.

**`by()` modulator complexity.** The `by()` modulator is a powerful but opaque mechanism
that changes the behavior of the preceding step in non-obvious ways. It is difficult to
explain, easy to misuse, and hard to reason about in complex traversals.

---

## Principled Departures

### Compile-time read/write separation *(implemented)*

`ReadTraversal` and `WriteTraversal` are distinct types. Write steps (`addV`, `addE`,
`property`, `drop`) do not exist on `ReadTraversal`. The compiler enforces this.

TinkerPop has no such distinction — mutating and non-mutating traversals have the same type.

### Numeric label IDs *(implemented)*

Labels are `u16` integers backed by a schema registry. String-to-ID mapping is explicit
and managed by the caller.

This eliminates string allocation on every traversal step, enables schema enforcement at
registration time, and makes traversal hot paths allocation-free.

### Single-threaded per query, multi-threaded across queries *(implemented)*

**Within a single traversal, everything is single-threaded.** The Volcano pipeline uses
`Rc<dyn GremlinStep>` for step references, `Rc<Traverser>` for traverser trees, and
`RefCell` for interior mutability inside `BufferedStep`. There are no thread pools, no
work-stealing queues, no intra-query parallelism.

**Across queries, concurrency is at the session boundary.** A `Graph` handle is cheaply
cloneable (`Arc` internally) and safe to share across threads. Each thread creates its
own `ReadSession` (pinned to a RocksDB snapshot) or `TxSession` (an OCC transaction) and
drives it independently. Multiple sessions can read or write concurrently against the
same `RocksStorage`; RocksDB handles the I/O concurrency internally.

This split is deliberate:

- **It keeps the hot path allocation-free for synchronisation.** `Rc` instead of `Arc`
  means no atomic reference counting on every traverser produced. `RefCell` instead of
  `Mutex` means no lock acquisition on every `next()` call into the pipeline.
- **It eliminates a whole class of bugs.** No data races, no deadlocks, no
  non-deterministic ordering between steps in the same query. The traversal engine is
  a deterministic function from (plan, snapshot) → results.
- **It matches how graph databases are used in practice.** Most applications issue many
  small, independent queries (e.g. one per HTTP request). Session-per-request maps
  naturally onto this pattern without forcing the query engine itself to be
  thread-safe.

TinkerPop's `Traversal` interface implies the same model (single-threaded iteration via
`hasNext()`/`next()`), so this is consistent with Gremlin semantics — RocksGraph just
encodes it in the type system rather than leaving it as a runtime convention.

```
┌─── Thread pool (N workers) ──────────────────────────────────────────┐
│                                                                      │
│  ┌─ Thread A ──────────────┐  ┌─ Thread B ──────────────┐           │
│  │ ReadSession              │  │ TxSession                │           │
│  │  Rc<PhysicalPlan>        │  │  Rc<PhysicalPlan>        │           │
│  │  Rc<Traverser> tree      │  │  Rc<Traverser> tree      │           │
│  │  RefCell<BufferedStep>   │  │  RefCell<BufferedStep>   │           │
│  │  overlay: HashMap caches │  │  overlay: dirty HashMap  │           │
│  └──────────┬───────────────┘  └──────────┬───────────────┘           │
│             │ RocksDB snapshot             │ OCC txn                  │
│             ▼                              ▼                          │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │  RocksStorage (Arc<OptimisticTransactionDB>)                    │  │
│  │  Schema (Arc<RwLock<Schema>>)                                   │  │
│  └────────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  Per-query:  Rc / RefCell / HashMap   — single-threaded, no sync    │
│  Cross-query: Arc / RwLock             — shared, concurrent safe    │
└──────────────────────────────────────────────────────────────────────┘
```

### `withProperties()` fetch hint *(implemented)*

A trailing step that controls which properties are fetched during materialization:
- `.withProperties([])` — return `Vertex`/`Edge` with id and label only; zero extra reads
  for edges (label is in `EdgeKey`), minimal reads for vertices
- `.withProperties(["name", "age"])` — return typed `Vertex`/`Edge` with only those keys
- No `.withProperties()` — default behavior, all properties fetched

This fills the gap TinkerPop cannot fill: a typed result with selective property loading.

#### Why no `valueMap()` / `elementMap()`

These TinkerPop steps extract properties into an unstructured `Map`, losing the typed
`Vertex`/`Edge` wrapper. They exist in Gremlin because TinkerPop cannot return a `Vertex`
with partial properties — the only choices are the full `Vertex` (all properties) or
`valueMap()` (untyped map of values).

`withProperties()` is the principled RocksGraph alternative: the result stays typed, and
the caller picks which properties to fetch. The mid-pipeline use case (extract properties
then continue traversing on them) is rare enough that supporting two overlapping APIs is
not justified. Skip `valueMap()` and `elementMap()` — they are TinkerPop workarounds that
`withProperties()` renders unnecessary.

### Reserved-key disjoint model *(implemented)*

`id`, `label`, and `rank` carry structural meaning beyond an ordinary property, so they
are accessible **only** through dedicated steps — `id()`/`label()`/`rank()` for
extraction, `hasId()`/`hasLabel()`/`hasRank()` for filtering. The generic property
machinery (`values()`/`properties()`/`has()`) rejects all three outright rather than
quietly accepting them as a second access path.

TinkerPop's generic steps and reserved tokens (`values("id")`, `Key`-style routing)
let the same value be reached two ways. RocksGraph treats that overlap as a defect, not
a convenience — it already produced one real bug (label decoding diverging between the
two paths) before being closed by removing the second path entirely.

See `docs/design_reserved_keys.md` for the full design.

### Vertex label as an optional concern *(under consideration)*

Vertex label may be dropped from the core model, treating vertices as ID-only entities
where semantic typing is a user property. This would:
- Remove the `hasLabel()` / `Key::Label` asymmetry
- Eliminate the materialization cost of label-only reads
- Simplify `VertexKey` to a plain `i64`

Edge label remains structural and non-optional, as in every mainstream graph database.

See `docs/design_vertex_label.md` for the full analysis.

### `group()` / `groupCount()` `by()` modulators *(under consideration)*

TinkerPop's `group()` takes independent key- and value-`by()` modulators, where the
value modulator's shape (whether it ends in a reducing `Barrier` step like `count()`
or `sum()`) decides whether each map entry's value is a `List` or a reduced scalar.
RocksGraph's `group()`/`groupCount()` have no `by()` modulator at all today — `group()`
always groups by raw identity into `List`s, and `groupCount()` is a separate, fixed
step rather than a generalization. Closing this gap would mean choosing between
TinkerPop's full generality and the "narrowly scoped steps" value below.

See `docs/design_group_step.md` for the verified TinkerPop semantics, the current
implementation's exact behavior (including a dead `key` field left over from an
earlier attempt), and the options under consideration.

---

## AI-Assisted Query Authoring as a Design Assumption

The traditional argument for TinkerPop compatibility is ecosystem inertia: users know the
API, existing tooling exists, answers are findable. As AI-assisted code generation becomes
the primary authoring interface for most users, this argument weakens significantly:

- Users describe what they want in natural language; the AI generates the traversal
- A non-standard but principled API is equally easy for AI to emit as a standard one
- The AI can explain departures from TinkerPop on demand and translate between models
- A clean, internally consistent API is actually easier for AI to use correctly than a
  standard-compliant one with known inconsistencies

What matters in an AI-assisted world is that the API is **consistent, typed, and well-documented**,
not that it matches an external specification. Complexity that exists purely for
compatibility becomes a liability rather than an asset.

---

## Core Design Values

**Explicit over implicit.** Property fetching, label resolution, and type conversions
should be visible in the traversal, not hidden in library behavior.

**Typed at the boundary.** Internal pipeline values (`GValue`) stay cheap and unresolved.
User-facing values (`Value`) are fully materialized and statically typed. The conversion
happens exactly once, at the terminal.

**Zero-cost defaults.** The common path — traversing edges, filtering, extracting scalars —
should not pay for features not in use. No allocations for label strings in the hot path.
No property fetches for elements that are only waypoints.

**Principled schema.** Labels and property keys are schema-registered identifiers, not
free-form strings. The schema is the contract.

**Strict syntax, narrowly scoped steps.** Each step's contract should be small enough to
state in one sentence. A value that carries structural meaning beyond "a property" gets
its own dedicated step rather than being absorbed into a generic, do-everything one —
e.g. `id`/`label`/`rank` are reached only through `id()`/`label()`/`rank()` and
`hasId()`/`hasLabel()`/`hasRank()`, never through `values()`/`has()`. Two access paths to
the same data are a latent inconsistency waiting to happen, not redundancy worth keeping
for convenience.
