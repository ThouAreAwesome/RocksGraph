# RocksGraph Design Principles

## Positioning

RocksGraph follows the Gremlin traversal model closely. Users familiar with JanusGraph,
AWS Neptune, or TinkerPop will find the core traversal primitives — `.g().V().out().values()`,
filter steps, path tracking, and aggregations — behave as expected.

We diverge from TinkerPop selectively and reluctantly: only when the standard's design
creates problems that cannot be resolved within its constraints (forced allocations on the
hot path, ambiguous access to structural values, type information that is genuinely
unrepresentable at compile time). Each divergence is documented below with the specific
problem it solves.

---

## Design tensions between TinkerPop's goals and RocksGraph's

TinkerPop Gremlin was designed as a universal, database-agnostic traversal language —
a goal it achieves well. RocksGraph is designed as an embeddable, single-process graph
engine where every allocation and every round-trip matters. These two design contexts
create natural tensions:

**Full-property materialization.** In TinkerPop, `out()` returning a `Vertex` always
fetches all properties. There is no mechanism in the standard for a typed `Vertex`/`Edge`
with partial properties. The standard alternatives — `valueMap()`, `elementMap()`,
`project()` — are untyped. For an embedded engine where I/O is the dominant cost,
selective property loading is essential. RocksGraph addresses this with `withProperties()`.

**Vertex label as an optional dimension.** In TinkerPop, a vertex label is a first-class
concept alongside the vertex ID — yet unlike edge labels (which are structurally
necessary for traversal), vertex labels add API surface and materialization cost without
carrying equivalent structural weight. RocksGraph treats the vertex label as a schema-level
convenience that is resolved on demand, rather than a required property of every vertex
traversal.

**VertexProperty indirection.** TinkerPop wraps vertex properties in a `VertexProperty`
object to support multi-cardinality. RocksGraph treats vertex properties as a flat
`HashMap<u16, Primitive>` — the common single-value case requires no indirection, and
multi-cardinality (when genuinely needed) can be modeled with list-valued properties.

**Dynamic typing vs compile-time enforcement.** TinkerPop's traversal return type varies
at runtime depending on the step chain — `.next()` might return a `Vertex`, an `Integer`,
or a `Map`. This works naturally in JVM languages but loses information that Rust's type
system could use. RocksGraph separates `ReadTraversal` and `WriteTraversal` at the type
level, and uses typed terminal methods.

**`by()` modulator complexity.** The `by()` modulator is powerful but opaque — it changes
the behavior of the preceding step in non-obvious ways. RocksGraph currently supports
`order().by(key)` but has not adopted the full `by()` modulator semantics because the
added expressiveness has not yet been shown to justify the API complexity in an embedded
context. This is an active area of design evaluation (see `docs/design_group_step.md`),
not a permanent exclusion.

---

## Necessary Divergences

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
- No `.withProperties()` call — return `Vertex`/`Edge` with id and label only; no property reads
- `.withProperties([])` — fetch and return all properties
- `.withProperties(["name", "age"])` — return typed `Vertex`/`Edge` with only those keys

This addresses a real-world need in embedded contexts — selective property loading
without losing the typed `Vertex`/`Edge` structure — that the standard Gremlin API has
no mechanism for.

#### Why no `valueMap()` / `elementMap()`

These TinkerPop steps extract properties into an unstructured `Map`, losing the typed
`Vertex`/`Edge` wrapper. They exist in Gremlin because TinkerPop cannot return a `Vertex`
with partial properties — the only choices are the full `Vertex` (all properties) or
`valueMap()` (untyped map of values).

`withProperties()` is the RocksGraph equivalent: the result stays typed as `Vertex`/`Edge`,
and the caller picks which properties to fetch. TinkerPop users accustomed to `valueMap()`
or `elementMap()` can use `withProperties()` instead — it achieves the same selective
loading without losing the typed wrapper.

### Reserved-key disjoint model *(implemented)*

`id`, `label`, and `rank` carry structural meaning beyond an ordinary property, so they
are accessible **only** through dedicated steps — `id()`/`label()`/`rank()` for
extraction, `hasId()`/`hasLabel()`/`hasRank()` for filtering. The generic property
machinery (`values()`/`properties()`/`has()`) rejects all three outright rather than
quietly accepting them as a second access path.

TinkerPop's generic steps and reserved tokens (`values("id")`, `Key`-style routing)
let the same value be reached through two independent code paths. RocksGraph takes the
position that structural values (`id`, `label`, `rank`) should have exactly one access
path: this eliminated a real bug where label decoding diverged between the two paths
during early development, and prevents that class of inconsistency permanently.

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
step rather than a generalization. This is a known compatibility gap relative to TinkerPop.

The design question under evaluation is *how* to close it — specifically whether to adopt
the full `by()` modulator semantics or introduce a narrower API that covers the common
cases without the full generality.

See `docs/design_group_step.md` for the verified TinkerPop semantics, the current
implementation's exact behavior (including a dead `key` field left over from an
earlier attempt), and the options under consideration.

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
