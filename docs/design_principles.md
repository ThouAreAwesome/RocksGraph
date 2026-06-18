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

### `withProperties()` fetch hint *(planned)*

A trailing step that controls which properties are fetched during materialization:
- `.withProperties([])` — return `Vertex`/`Edge` with id and label only; zero extra reads
  for edges (label is in `EdgeKey`), minimal reads for vertices
- `.withProperties(["name", "age"])` — return typed `Vertex`/`Edge` with only those keys
- No `.withProperties()` — default behavior, all properties fetched

This fills the gap TinkerPop cannot fill: a typed result with selective property loading.

### Vertex label as an optional concern *(under consideration)*

Vertex label may be dropped from the core model, treating vertices as ID-only entities
where semantic typing is a user property. This would:
- Remove the `hasLabel()` / `Key::Label` asymmetry
- Eliminate the materialization cost of label-only reads
- Simplify `VertexKey` to a plain `i64`

Edge label remains structural and non-optional, as in every mainstream graph database.

See `docs/design_vertex_label.md` for the full analysis.

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
