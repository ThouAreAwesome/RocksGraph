# Design: Python bindings — `pip install rocksgraph`

Status: proposal — awaiting implementation.

## Problem

RocksGraph is a Rust crate. The two largest user segments identified in
[market_landscape_and_positioning.md](../market_landscape_and_positioning.md) are:

1. Rust application developers (`cargo add rocksgraph`)
2. **Python developers using Gremlin semantics** — data scientists, ML engineers,
   and developers already targeting Neptune or Cosmos DB Gremlin API

Segment 2 has no access path to RocksGraph today.

The [action roadmap](./market_landscape_and_positioning.md#31-工程路线图) marks
`pip install rocksgraph` as a P0 item.

## Goals & non-goals

- **Goals:**
  - `pip install rocksgraph` installs a native Python wheel — no Rust toolchain
    required.
  - `rocksgraph.Graph.open("./data")` returns a Python `Graph` object with a
    Gremlin-style traversal API: `snap.g().V([1]).out(["knows"]).values(["name"]).next()`.
  - **Compact binary protocol as the single FFI entry point.**  Python builder
    methods accumulate `(opcode, args)` tuples in pure Python; a single
    `_execute(bytes)` FFI call runs the query.  This protocol also serves all
    future language bindings (Node.js, Go, Java) — one Rust function, zero
    per-language step wrappers.
  - **Python builder covers all Gremlin steps:**
    - Traversal: `V`, `out`, `in`, `both`, `outE`, `inE`, `bothE`, `inV`, `outV`, `otherV`
    - Filtering: `has`, `hasLabel`, `hasId`, `hasRank`, `is`, `limit`, `range`, `skip`, `tail`, `dedup`, `order`, `order_by`
    - Extraction: `values`, `properties`, `id`, `label`, `rank`
    - Aggregation: `count`, `fold`, `sum`, `mean`, `max`, `min`, `unfold`, `group`, `groupCount`, `path`
    - Composition: `as_`, `select`, `identity`, `constant`, `simplePath`, `cyclicPath`
    - Sub-traversal: `where`, `not`, `and`, `or`, `union`, `coalesce`, `choose`, `local`, `repeat`, `until`, `emit`, `emit_if`
    - Mutation: `addV`, `addE`, `property`, `from`, `to`, `drop`
  - **All terminals:** `next()`, `to_list()`, `iter()`, `explain()`, `withProperties()`.
  - Value round-trip: `Int64 → int`, `String → str`, `List → list`, `Vertex → dict`, etc.
  - Predicate constructors (`eq`, `gt`, `lt`, `gte`, `lte`, `ne`, `between`,
    `within`, `without`) as module-level free functions.
  - Pre-compiled wheels for macOS (ARM64 + x86_64), Linux (x86_64), and
    Windows (x86_64) via GitHub Actions.

- **Non-goals (v0.1):**
  - Gremlin WebSocket server mode — separate design.
  - `group().by()` / `groupCount().by()` — the `by()` modulator is not yet supported
    in v0.1.  `group()` groups by the current traverser value only; `groupCount()`
    counts occurrences of the current value.  `by()` support is tracked in
    `docs/design_group_step.md`.
  - Bulk load (`SstBulkLoader`) — different type hierarchy.  Planned for v0.2.
  - Schema management (`open_management()`) — separate design.
  - TinkerPop Gremlin Bytecode (GLV) adapter — the compact binary protocol is
    the primary entry point.  GLV (the wire format used by `gremlinpython`)
    is primarily a remote-server protocol and is **not yet available for the embedded
    use case**.  Migrating from JanusGraph/Neptune to an embedded library is an
    architectural change that requires code changes regardless of query-language
    compatibility.  The binary protocol infrastructure (§1.2) is explicitly
    designed to accept a GLV→binary translation layer as a community contribution.

## Existing code to touch

| Path | Role |
|------|------|
| `Cargo.toml` (root) | Convert to workspace root |
| `rocksgraph/Cargo.toml` | Current `Cargo.toml`, moved into sub-crate |
| `rocksgraph/src/` | Unchanged. All existing code moves here. |
| `rocksgraph/src/bytecode/` | **New.** Compact binary protocol encoder/decoder (see §2). |
| `bindings/python/Cargo.toml` | **New.** Rust cdylib with PyO3 (FFI glue only, ~80 lines). |
| `bindings/python/pyproject.toml` | **New.** maturin build config. |
| `bindings/python/src/lib.rs` | **New.** Three FFI functions: `_open`, `_execute_read`, `_execute_write`. |
| `bindings/python/rocksgraph/` | **New.** Pure Python package (~400 lines total): |
| `bindings/python/rocksgraph/__init__.py` | Re-exports + type stubs. |
| `bindings/python/rocksgraph/_builder.py` | Traversal builder (~300 lines). |
| `bindings/python/rocksgraph/_codec.py` | Python-side opcode→bytes encoder (~100 lines). |
| `.github/workflows/python-release.yml` | **New.** Multi-platform wheel CI. |

---

## 1. Architecture: compact binary protocol as the single FFI entry point

### 1.1 Why not bind every step through FFI

The naive approach is to export each Gremlin step as a PyO3 method:

```
PyO3 exports: V(), out(), has(), hasLabel(), count(), values(), where(), not(), ...
  → ~30 FFI functions, ~700 lines of Rust boilerplate
  → Each step call crosses the Python↔Rust boundary (GIL acquire, argument conversion)
  → Adding a new step requires changes in both Rust binding and Python stub
```

For Python alone, the boilerplate is tolerable.  But every additional language
(Node.js, Go, Java) would need its own copy of all 30 step wrappers —
the N×M binding problem.

### 1.2 The compact binary protocol

A single Rust function serves all languages:

```rust
// rocksgraph/src/bytecode/mod.rs
pub fn execute(
    graph: &Graph,
    bytes: &[u8],
) -> Result<Vec<Value>, StoreError> {
    let plan = decode(bytes)?;
    let optimized = apply_rules(plan)?;
    // ... build physical plan, run pipeline, collect results
}
```

The protocol is a fixed-opcode, length-prefixed binary encoding.  Each step is
`[op: u8] [payload...]`:

```
Query:  [version: u8] [step_count: u16] [step...]*

Step:   [op: u8] [arg0_len: u16] [arg0_bytes...] [arg1_len: u16] [arg1_bytes...] ...
```

The version byte is `0x01` for this format.  `decode()` rejects unknown versions
with `StoreError::UnsupportedOperation`.  One byte, zero overhead, enables
backward-compatible protocol evolution without an out-of-band negotiation step.

**Opcode table** (one u8 per LogicalStep variant):

```
OP_V         = 0x01    [id_count: u16] [id: i64 BE]*count
OP_OUT       = 0x02    [label_count: u16] [label_id: i32 BE]*count
OP_IN        = 0x03
OP_BOTH      = 0x04
OP_HAS       = 0x10    [prop_key_id: u16] [pred_tag: u8] [pred_value...]
OP_HAS_LABEL = 0x11    [label_count: u16] [label_id: i32 BE]*count
OP_HAS_ID    = 0x12    [id_count: u16] [id: i64 BE]*count
OP_COUNT     = 0x20    no payload
OP_VALUES    = 0x21    [key_count: u16] [key_id: u16]*count
OP_WHERE     = 0x30    [sub_plan_bytes...]  ← nested plan
OP_NOT       = 0x31    [sub_plan_bytes...]
OP_AND       = 0x32    [sub_count: u16] ([sub_plan_bytes...])*count
OP_OR        = 0x33
OP_UNION     = 0x34
OP_COALESCE  = 0x35
OP_CHOOSE    = 0x36
OP_LOCAL     = 0x37
OP_REPEAT    = 0x40    [body_plan...] [until_plan...] [emit: u8] [times: u32]
OP_UNTIL     = 0x41    [cond_plan...]
OP_GROUP     = 0x50    no payload
OP_GROUP_CNT = 0x51    no payload
OP_PATH      = 0x52    no payload
```

**Example encoding** — `V([1, 2]).out(["knows"]).count()` → 25 bytes:

```
[0x01]                ← version = 1
[0x00, 0x03]          ← 3 steps
[0x01]                ← OP_V
  [0x00, 0x02]        ← 2 ids
  [0x00...id=1...]    ← 8 bytes BE
  [0x00...id=2...]
[0x02]                ← OP_OUT
  [0x00, 0x01]        ← 1 label
  [0x00...3...]       ← label_id 4 bytes BE
[0x20]                ← OP_COUNT (no payload)
```

No JSON parser.  No string-keyed dispatch.  Single-pass byte-by-byte decode
directly into `LogicalStep` variants.

### 1.3 Response encoding

Query results use the same design principles as requests: fixed tags + length
prefixes + big-endian integers.  This matters for languages whose FFI only
passes raw bytes (Go via CGO, Java via JNI).  Python (PyO3) and Node.js
(napi-rs) convert `Vec<Value>` to native objects directly, skipping this layer.

```
Response:  [row_count: u32 BE] [row...]*row_count

Row:       [tag: u8] [payload...]

TAG_NULL      = 0x00   —
TAG_BOOL      = 0x01   [0x00 | 0x01]
TAG_INT32     = 0x02   [4 bytes BE]
TAG_INT64     = 0x03   [8 bytes BE]
TAG_UINT16    = 0x04   [2 bytes BE]
TAG_FLOAT32   = 0x05   [4 bytes BE]
TAG_FLOAT64   = 0x06   [8 bytes BE]
TAG_STRING    = 0x07   [byte_len: u16 BE] [UTF-8 bytes]
TAG_LIST      = 0x08   [item_count: u32 BE] [row...]*count
TAG_VERTEX    = 0x09   [id: i64 BE] [label_len: u16 BE] [label: UTF-8]
                        [prop_count: u16 BE] [(key_id: u16 BE, row)]*count
TAG_EDGE      = 0x0A   [src_id: i64 BE] [dst_id: i64 BE]
                        [label_len: u16 BE] [label: UTF-8]
                        [rank: u16 BE] [prop_count: u16 BE] [(key_id: u16 BE, row)]*count
TAG_MAP       = 0x0B   [entry_count: u32 BE] [(key: row, value: row)]*count
TAG_PATH      = 0x0C   [step_count: u32 BE]
                        [(step_value: row, label_count: u16 BE, [TAG_STRING]*label_count)]*count
                        — label_count=0 for unlabelled steps; labels come from .as("a")
TAG_UUID      = 0x0D   [16 bytes BE]
TAG_BYTES     = 0x0E   [len: u32 BE] [bytes]
```

**Example** — query returns `[Int64(3), String("bob")]` (13 bytes):

```
[0x00, 0x00, 0x00, 0x02]    ← 2 rows
[0x03]                       ← TAG_INT64
  [0x00, 0x00, 0x00, 0x00,
   0x00, 0x00, 0x00, 0x03]  ← 8 bytes BE
[0x07]                       ← TAG_STRING
  [0x00, 0x03]               ← len=3
  'b' 'o' 'b'                ← UTF-8
```

The Rust core crate exposes both the response encoder and a native-result FFI
entry point:

```rust
// rocksgraph/src/bytecode/mod.rs

/// Decode query bytes, execute, return encoded results.
/// Used by Go, Java, and any binding that communicates via raw bytes.
pub fn execute_encoded(graph: &Graph, request: &[u8]) -> Result<Vec<u8>, StoreError>;

/// Decode query bytes, execute, return Vec<Value> directly.
/// Used by Python (PyO3) and Node.js (napi-rs) for native object conversion.
pub fn execute_native(graph: &Graph, request: &[u8]) -> Result<Vec<Value>, StoreError>;

/// Encode results to the binary response format.
pub fn encode_response(results: &[Value]) -> Vec<u8>;

/// Decode binary response back to Vec<Value>.
pub fn decode_response(bytes: &[u8]) -> Result<Vec<Value>, StoreError>;
```

```
Python user code
    │  snap.g().V([1]).out(["knows"]).values(["name"]).next()
    ▼
bindings/python/rocksgraph/_builder.py     Pure Python: accumulates (opcode, args) tuples
bindings/python/rocksgraph/_codec.py       Pure Python: tuples → bytes (compact binary format)
    │
    │  _execute(buf) — a single FFI call per query
    ▼
bindings/python/src/lib.rs                 ~80 lines: three #[pyfunction]s
    │
    ▼
rocksgraph/src/bytecode/mod.rs             decode(bytes) → LogicalPlan → optimize → execute
    │
    ▼
rocksgraph/src/engine/...                  Volcano pipeline — unchanged
rocksgraph/src/store/...                   RocksDB backend — unchanged
```

### 1.4 The N×M problem — solved

```
┌─────────────────────────────────────────────────────────┐
│          rocksgraph/src/bytecode/mod.rs                  │
│                                                         │
│  decode_request(bytes) → LogicalPlan                    │
│  execute_native(bytes)  → Vec<Value>    (PyO3, napi-rs) │
│  execute_encoded(bytes) → Vec<u8>       (CGO, JNI)      │
│  encode_response(vals)  → Vec<u8>                       │
│  decode_response(bytes) → Vec<Value>                    │
│                                                         │
│  ← written once in Rust. Every language calls this.     │
└────────────────────┬────────────────────────────────────┘
                     │
     ┌───────────────┼───────────────┬──────────────────┐
     ▼               ▼               ▼                  ▼
┌─────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
│ Python  │   │ Node.js  │   │   Go     │   │  Java    │
│         │   │          │   │          │   │          │
│ builder │   │ builder  │   │ builder  │   │ builder  │
│ + codec │   │ + codec  │   │ + codec  │   │ + codec  │
│  ~300 行│   │  ~200 行 │   │  ~200 行 │   │  ~200 行 │
│         │   │          │   │          │   │          │
│ PyO3    │   │ napi-rs  │   │ CGO      │   │ JNI      │
│ native  │   │ native   │   │ execute  │   │ execute  │
│ convert │   │ convert  │   │ + decode │   │ + decode │
│  ~80 行 │   │  ~30 行  │   │ response │   │ response │
└─────────┘   └──────────┘   └──────────┘   └──────────┘
```

Two response paths depending on FFI capability:

| Path | Used by | Mechanism |
|------|---------|-----------|
| **Native object** | Python (PyO3 `IntoPy`), Node.js (napi-rs `ToNapiValue`) | `Vec<Value>` → Python `list` / JS `Array` directly |
| **Binary decode** | Go (CGO), Java (JNI) | Rust `encode_response()` → target-language `decode_response()` |

Each new language: ~200 lines of pure target-language builder+codec + ~30 lines
FFI glue.  Languages on the binary path add ~30 lines of response decoder.
No per-step wrapper methods.  No N×M explosion.

### 1.5 Trade-off: hard-coded binary vs Protocol Buffers

The current design uses a manually-defined binary encoding.  An alternative is
Protocol Buffers (protobuf) — define a `.proto` schema and let `protoc` generate
encoders/decoders for every language.  The choice is deliberate for the current
stage of the project, but the trade-off is worth documenting in case the project
grows beyond its current assumptions.

**Comparison:**

| Dimension | Hard-coded binary (current) | Protocol Buffers |
|-----------|----------------------------|------------------|
| **Dependencies** | Zero (`std` only) | `protoc` binary + language plugins at build time; `protobuf` runtime lib per language |
| **Build complexity** | None — just Rust + target-language code | `.proto` → codegen step before compilation |
| **Opcode sync** | Manual: Python `OP.V = 0x01` must match Rust `const OP_V: u8 = 0x01` | Automatic: single `.proto` is the source of truth |
| **Wire size** | Smallest possible; fixed-width fields with no field tags | ~2-5 bytes overhead per field (varint field numbers + length-delimited wrappers) |
| **Adding a new step** | Add opcode constant + match arm in Rust decoder + match arm in each language encoder (~5 lines per language) | Add oneof variant or new message type in `.proto` → regenerate all languages |
| **Schema evolution** | Manual: version nibble or magic bytes; old decoders fail on unknown opcodes | Built-in: field numbers, `optional`/`repeated`, unknown field preservation |
| **Documentation** | Opcode table is the spec; must be kept in sync manually | `.proto` file is self-documenting |
| **Learning curve for contributors** | Must read the opcode spec | Standard protobuf workflow; widely understood |
| **~30 step types** | ~150 lines of match arms in Rust, ~100 lines per language encoder | ~80 lines of `.proto`, zero per-language encoder code |

**When protobuf becomes the better choice:**

1. **3+ language bindings.**  Manually syncing opcode constants across Python,
   Node.js, Go, and Java becomes a real maintenance burden.  A single `.proto`
   file eliminates this entirely.

2. **Schema evolution needs.**  If the on-disk or wire format starts needing
   backward-compatible evolution (e.g., optional fields added to existing step
   types), protobuf's field-number model handles this correctly without a
   version-negotiation layer.

3. **Community contributions.**  A `.proto` file is the standard way to say
   "this is our wire format" — contributors from any language can read it and
   generate bindings without studying a custom opcode table.

**Migration path:**

The protocol boundary — `execute(bytes: &[u8])` — is deliberately designed to
be format-agnostic.  Switching from hard-coded binary to protobuf is a
single-site change:

1. Add `rocksgraph.proto` to the repository.
2. Replace `decode_request()` internals with protobuf-generated parser.
3. Replace `encode_response()` / `decode_response()` with protobuf serialisation.
4. Replace each language's hand-written encoder with protobuf-generated code.

The FFI function signature (`execute(bytes)`) and every language's builder API
stay exactly the same.  Only the byte format inside `bytes` changes.

**Current stance:** Hard-coded binary is the right choice for Python-only (v0.1).
Re-evaluate when the second language binding (Node.js or Go) is added.

---

## 2. Python binding implementation

### 2.1 Rust FFI layer (`bindings/python/src/lib.rs` — ~90 lines)

Three `#[pyclass]` types, three FFI functions.

```rust
use pyo3::prelude::*;
use rocksgraph::{bytecode, Graph as CoreGraph, ReadSession, TxnSession, Value};

/// Python-facing graph handle.  Graph is Clone (Arc internally) — cheap to share.
#[pyclass]
struct PyGraph {
    inner: CoreGraph,
}

#[pymethods]
impl PyGraph {
    #[staticmethod]
    fn open(path: &str) -> PyResult<Self> {
        let inner = CoreGraph::open(path)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;
        Ok(Self { inner })
    }
}

/// Read-only session backed by a point-in-time RocksDB snapshot.
///
/// The snapshot is pinned when the Python session is created (via graph.read())
/// and released when the Python object is garbage-collected.  All queries on
/// this session see the same consistent committed state — matching the Rust
/// `ReadSession` semantics and TinkerPop session behaviour.
#[pyclass]
struct PyReadSession {
    inner: ReadSession,   // Self-contained: owns a RocksDB snapshot + caches.
                          // ReadSession is 'static — no lifetime issue with PyO3.
}

#[pymethods]
impl PyReadSession {
    /// Execute a binary-encoded query against the pinned snapshot.
    fn _execute(&mut self, bytes: &[u8]) -> PyResult<Vec<PyObject>> {
        let results = bytecode::execute_read(&mut self.inner, bytes)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;
        Python::with_gil(|py| {
            results.into_iter().map(|v| value_to_py(py, v)).collect()
        })
    }
}

/// Read-write transaction session backed by a RocksDB OCC transaction.
///
/// Like `ReadSession`, `TxnSession` is a self-contained type — it holds its own
/// transaction handle (internally Arc-cloned from the DB).  No lifetime
/// dependency on `PyGraph`.  Auto-rolls back on `__del__` if `commit()` was
/// not called.
#[pyclass]
struct PyTxnSession {
    inner: Option<TxnSession>,  // Option: take ownership on commit/rollback/drop
}

#[pymethods]
impl PyTxnSession {
    fn _execute(&mut self, bytes: &[u8]) -> PyResult<Vec<PyObject>> {
        let txn = self.inner.as_mut()
            .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("transaction closed"))?;
        let results = bytecode::execute_write(txn, bytes)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;
        Python::with_gil(|py| {
            results.into_iter().map(|v| value_to_py(py, v)).collect()
        })
    }

    fn commit(mut slf: PyRefMut<'_, Self>) -> PyResult<()> {
        let txn = slf.inner.take()
            .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>("already closed"))?;
        txn.commit()
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(e.to_string()))?;
        Ok(())
    }

    fn rollback(mut slf: PyRefMut<'_, Self>) {
        slf.inner = None;  // Drop → RocksDB transaction aborted
    }
}
```

**Why this compiles** — lifetime analysis of the Rust types:

| Rust type | What it holds | `'static`? | Safe in `#[pyclass]`? |
|-----------|--------------|:---:|:---:|
| `CoreGraph` | `Arc<RocksStorage>` + `Arc<RwLock<Schema>>` | ✅ | ✅ |
| `ReadSession` | `LogicalSnapshot<RocksStorage>` — owns Snapshot + caches + `Arc<Schema>` | ✅ (self-contained) | ✅ |
| `TxnSession` | `LogicalGraph<RocksStorage>` — owns Txn + caches + `Arc<Schema>` | ✅ (self-contained) | ✅ |

`ReadSession` and `TxnSession` do **not** borrow from `Graph`.  They hold
`Arc`-cloned references to the RocksDB handle internally, so they are fully
`'static` and safe to store directly in `#[pyclass]` structs.

That's it.  No `PyReadTraversal`, no `PyWriteTraversal`, no `PyGraphTraversal`.
Three classes, three FFI entry points.

### 2.2 Python builder (`bindings/python/rocksgraph/_builder.py` — ~300 lines)

The builder is pure Python.  Each step method appends a `(opcode, args)` tuple
and returns a new builder instance (immutable style avoids accidental sharing):

```python
from rocksgraph._codec import OP

class ReadTraversal:
    def __init__(self, session, steps=None):
        self._session = session
        self._steps = steps or []

    def V(self, ids):
        return ReadTraversal(self._session, self._steps + [(OP.V, tuple(ids))])

    def out(self, labels):
        return ReadTraversal(self._session, self._steps + [(OP.OUT, tuple(labels))])

    def in_(self, labels):
        return ReadTraversal(self._session, self._steps + [(OP.IN, tuple(labels))])

    def both(self, labels):
        return ReadTraversal(self._session, self._steps + [(OP.BOTH, tuple(labels))])

    def has(self, key, pred):
        return ReadTraversal(self._session, self._steps + [(OP.HAS, key, pred)])

    def hasLabel(self, labels):
        return ReadTraversal(self._session, self._steps + [(OP.HAS_LABEL, tuple(labels))])

    def hasId(self, ids_or_pred):
        return ReadTraversal(self._session, self._steps + [(OP.HAS_ID, ids_or_pred)])

    def count(self):
        return ReadTraversal(self._session, self._steps + [(OP.COUNT,)])

    def values(self, keys):
        return ReadTraversal(self._session, self._steps + [(OP.VALUES, tuple(keys))])

    def where_(self, sub_traversal):
        return ReadTraversal(self._session, self._steps + [(OP.WHERE, sub_traversal._steps)])

    def not_(self, sub_traversal):
        return ReadTraversal(self._session, self._steps + [(OP.NOT, sub_traversal._steps)])

    # ... ~20 more steps, same pattern ...

    # Terminals
    def next(self):
        buf = encode(self._steps)             # _codec.py
        results = self._session._execute(buf)
        return results[0] if results else None

    def to_list(self):
        buf = encode(self._steps)
        return self._session._execute(buf)

    def iter(self):
        buf = encode(self._steps)
        results = self._session._execute(buf)  # All results materialised in Rust first
        return iter(results)                   # Known limitation: not a true lazy stream.
                                               # For datasets larger than available RAM,
                                               # use .limit(n) or .range(lo, hi) to cap.
                                               # Streaming cursor API is planned for v0.2.

    def explain(self):
        buf = encode(self._steps)
        return self._session._execute_explain(buf)
```

**`__()` anonymous traversals** are a free-standing builder instance:

```python
class GraphTraversal:
    """Unattached traversal for sub-queries."""
    def __init__(self, steps=None):
        self._steps = steps or []
    # Same methods as ReadTraversal, minus terminals
    def V(self, ids): ...
    def out(self, labels): ...
    def otherV(self): ...
    def hasId(self, ids): ...
    # ...

def __():
    return GraphTraversal()

# Usage:
snap.g().V([1]).outE(["knows"]).where_(__().otherV().hasId([2])).count().next()
```

`GraphTraversal` has no `_session` — it cannot execute queries.  It only
accumulates steps.  When passed to `where_()`, its `_steps` list is nested
into the parent plan's byte sequence.

### 2.3 Python encoder (`bindings/python/rocksgraph/_codec.py` — ~100 lines)

```python
import struct

class OP:
    V          = 0x01
    OUT        = 0x02
    IN         = 0x03
    BOTH       = 0x04
    HAS        = 0x10
    HAS_LABEL  = 0x11
    HAS_ID     = 0x12
    COUNT      = 0x20
    VALUES     = 0x21
    WHERE      = 0x30
    NOT        = 0x31
    # ...

class PredicateTag:
    EQ     = 0x00
    GT     = 0x01
    LT     = 0x02
    GTE    = 0x03
    LTE    = 0x04
    NE     = 0x05
    WITHIN = 0x06
    WITHOUT= 0x07
    BETWEEN= 0x08

def encode(steps):
    """Encode a list of (opcode, *args) tuples to the compact binary format."""
    buf = bytearray()
    # Step count placeholder — filled at the end
    buf.extend(struct.pack('>H', len(steps)))
    for step in steps:
        op = step[0]
        if op == OP.V:
            ids = step[1]
            buf.append(op)
            buf.extend(struct.pack('>H', len(ids)))
            for vid in ids:
                buf.extend(struct.pack('>q', vid))
        elif op == OP.OUT:
            labels = step[1]
            buf.append(op)
            buf.extend(struct.pack('>H', len(labels)))
            for lbl in labels:
                # Schema resolution happens in Rust, not here.
                # Pass label names as strings; Rust resolves to LabelId.
                lbl_bytes = lbl.encode('utf-8')
                buf.extend(struct.pack('>H', len(lbl_bytes)))
                buf.extend(lbl_bytes)
        elif op == OP.HAS:
            key = step[1]
            pred = step[2]
            buf.append(op)
            key_bytes = key.encode('utf-8')
            buf.extend(struct.pack('>H', len(key_bytes)))
            buf.extend(key_bytes)
            _encode_predicate(buf, pred)
        elif op == OP.COUNT:
            buf.append(op)
            # No payload
        elif op == OP.WHERE:
            sub_steps = step[1]
            buf.append(op)
            sub_buf = encode(sub_steps)
            buf.extend(struct.pack('>I', len(sub_buf)))
            buf.extend(sub_buf)
        # ... etc for each opcode
    return bytes(buf)

def _encode_predicate(buf, pred):
    tag, value = pred
    buf.append(tag)
    # Encode value based on Python type
    if isinstance(value, bool):
        buf.append(0x01 if value else 0x00)
    elif isinstance(value, int):
        buf.extend(struct.pack('>q', value))
    elif isinstance(value, float):
        buf.extend(struct.pack('>d', value))
    elif isinstance(value, str):
        b = value.encode('utf-8')
        buf.extend(struct.pack('>H', len(b)))
        buf.extend(b)
    elif isinstance(value, (list, tuple)):
        buf.extend(struct.pack('>H', len(value)))
        for item in value:
            buf.extend(struct.pack('>q', item))
```

**Schema resolution** (label names → LabelId, property key names → u16) happens
in the Rust `decode()` path.  The Python encoder passes string names; the Rust
decoder resolves them against the schema registry.  This keeps the Python side
simple and avoids duplicating schema logic.

### 2.4 Value conversion (`_types.py`)

The Rust `execute()` function returns `Vec<Value>`.  The FFI layer converts each
to a Python native type:

```python
# In _types.py (used by the PyO3 layer or as reference for pure-Python conversion)

# Int64 → int, String → str, null → None
# Vertex → dict with {"id": int, "label": str, "properties": dict}
# Edge   → dict with {"out_v": int, "in_v": int, "label": str, "rank": int, "properties": dict}
# List   → list
# Map    → dict
# Path   → list of step entries
```

The conversion happens in the Rust FFI layer (PyO3's `IntoPy` trait) so values
are fully materialised before returning to Python.

---

## 3. Workspace layout

```
RocksGraph/
├── Cargo.toml                    ← [workspace] members = ["rocksgraph", "bindings/python"]
├── rocksgraph/                   ← existing crate (unchanged)
│   ├── Cargo.toml
│   └── src/
│       ├── api.rs
│       ├── gremlin/
│       ├── engine/
│       ├── store/
│       ├── bytecode/             ← new: compact binary protocol
│       │   └── mod.rs
│       └── lib.rs
├── bindings/
│   └── python/
│       ├── Cargo.toml            ← cdylib, depends on rocksgraph + pyo3
│       ├── pyproject.toml        ← maturin config
│       ├── src/
│       │   └── lib.rs            ← ~80 lines: _open, _execute_read, _execute_write
│       └── rocksgraph/           ← pure Python package
│           ├── __init__.py
│           ├── _builder.py       ← ReadTraversal, WriteTraversal, GraphTraversal, __()
│           ├── _codec.py         ← OP constants, encode()
│           └── _types.py         ← Value → Python type helpers
├── docs/
├── scripts/
└── ...
```

## 4. TinkerPop Bytecode adapter (community contribution)

GLV (TinkerPop Bytecode / `gremlinpython` wire format) is primarily a
remote-server protocol and is **not yet available for RocksGraph's embedded use case**.

From JanusGraph or Neptune to an embedded library is an architectural change —
the user switches from a network client to a process-in library.  That change
requires code changes regardless of query-language compatibility.  A GLV
adapter that preserves `gremlinpython` syntax verbatim would obscure, not
eliminate, the architectural difference.

If a Gremlin Server protocol is added to RocksGraph in the future, a GLV
adapter would be a natural fit as a translation layer on top of the binary
protocol defined in §1.2.  This is explicitly left open for community
contribution — the binary protocol infrastructure provides the necessary
interface (`bytecode/mod.rs`'s `decode()`), and a GLV→binary translation
layer is a ~200-line, self-contained module with publicly documented
input format (TinkerPop Bytecode spec).

---

## 5. User experience

```python
import rocksgraph
from rocksgraph import __, gt, between

graph = rocksgraph.Graph.open("./my_graph")

# Read query — Gremlin semantics
snap = graph.read()
name = snap.g().V([1]).out(["knows"]).values(["name"]).next()
# → "bob"

# Predicate filtering
adults = snap.g().V([]).has("age", gt(30)).values(["name"]).to_list()

# Sub-traversal
edges = snap.g().V([1]).outE(["knows"]).where_(__().otherV().hasId([2])).count().next()

# Lazy iteration + plan inspection
for v in snap.g().V([]).iter():
    print(v)
print(snap.g().V([1]).out(["knows"]).count().explain())

# Write transaction
txn = graph.begin()
txn.g().addV("person").property("id", 1).property("name", "alice").next()
txn.g().addV("person").property("id", 2).property("name", "bob").next()
txn.g().addE("knows").from_(1).to(2).next()
txn.commit()

# Error handling
try:
    txn.g().addV("undeclared_label").next()
except rocksgraph.StoreError as e:
    print(f"Schema error: {e}")
```

---

## 6. Implementation plan

The work is split into five merge requests (MRs) with clear boundaries.
MR 1 and MR 2 can be swapped in order; MR 3/4/5 are strictly sequential.

### MR 1 — Bytecode module (pure Rust, no structural changes)

Adds `src/bytecode/mod.rs` in the existing flat crate layout.  Zero structural
changes — the bytecode module is self-contained and does not depend on a
workspace layout.

1. Create `src/bytecode/mod.rs`.
2. Implement `encode()` + `decode()` for all `LogicalStep` variants.
3. Implement `execute_read(graph, bytes)` and `execute_write(txn, bytes)`.
4. Round-trip tests + end-to-end tests.

   **Verify:** `cargo test` passes.  Round-trip: `decode(encode(plan)) == plan`
   for every `LogicalStep` variant.  End-to-end: execute a sequence of known
   queries and assert correct results.

### MR 2 — Workspace conversion (structural only, zero logic changes)

Moves all source files into a `rocksgraph/` sub-crate and converts the root
`Cargo.toml` to a Cargo workspace.  No bytecode, no Python, no new logic.

5. Move `src/`, `Cargo.toml`, and all existing files into `rocksgraph/`.
6. Convert root `Cargo.toml` to `[workspace] members = ["rocksgraph"]`.
7. Update CI, `justfile`, documentation paths to reflect new layout.

   **Verify:** `cargo test` from workspace root produces **identical results**
   to MR 1.  Reviewer can confirm the diff contains only path changes.

### MR 3 — Python FFI layer (Rust + build config, ~90 lines)

Adds the `bindings/python/` crate.  Three `#[pyclass]` types, three FFI
functions.  This is the first MR that requires the workspace from MR 2.

8. Create `bindings/python/` with `Cargo.toml`, `pyproject.toml`, `src/lib.rs`.
9. Add `"bindings/python"` to workspace members.
10. Implement `PyGraph` (open), `PyReadSession` (_execute), `PyTxnSession`
    (_execute + commit + rollback).
11. **Launch a Windows CI job at this stage** (not MR 5) to surface RocksDB
    compilation issues early.  RocksDB (`librocksdb-sys`) on Windows requires
    MSVC + LLVM and is historically fragile in CI.

    **Verify:** `cd bindings/python && maturin develop` succeeds.  Python
    `import rocksgraph` loads without error.  Windows CI job is green or has
    a known issue filed.

### MR 4 — Python package (pure Python, ~400 lines)

All builder, encoder, and type-conversion logic in pure Python.

12. Implement `_codec.py` — opcode constants and `encode(steps)`.
13. Implement `_builder.py` — `ReadTraversal`, `WriteTraversal`, `GraphTraversal`,
    `__()`, all steps, predicate constructors.
    **Note:** `group()` and `groupCount()` docstrings must state that `.by()`
    is not yet supported (see Non-goals, §Goals).  The first thing
    any TinkerPop user will try is `group().by("name")` — catching this in
    documentation prevents a wave of bug reports.
14. Implement `_types.py` — Value type helpers.
15. Smoke-test level step coverage: `V`, `out`, `has`, `hasLabel`, `values`,
    `count`, `where`, `not`, `repeat`, `addV`, `addE`, `property`, `group`,
    `groupCount`, `path`, `order`, `fold`, `unfold`, `withProperties`.

    **Verify:** Python script equivalent to `gremlin/tests.rs` smoke tests
    passes end-to-end.

### MR 5 — CI wheel builds + PyPI release

16. Create `bindings/python/rocksgraph/__init__.py` with type stubs.
17. Create `.github/workflows/python-release.yml` — multi-platform wheel builds
    (macOS ARM64 + x86_64, Linux x86_64, Windows x86_64).
18. Publish to TestPyPI first, verify `pip install`, then publish to PyPI.

    **Verify:** `pip install rocksgraph` on a clean machine with no Rust
    toolchain.  `import rocksgraph; graph = rocksgraph.Graph.open("./test")`
    succeeds on all four target platforms.

### Known risks

- **Windows RocksDB compilation.**  `librocksdb-sys` requires MSVC and LLVM on
  Windows.  The Windows CI job is introduced in MR 3 (not MR 5) to surface
  build failures before the release pipeline depends on it.  If RocksDB
  compilation on Windows proves infeasible in v0.1, the Windows wheel is
  dropped from the release matrix with a tracked issue.

- **`group().by()` expectation mismatch.**  `group()` and `groupCount()` are
  supported in v0.1 without the `by()` modulator.  TinkerPop users will
  immediately try `group().by("name")` and get: `TypeError: group() got an
  unexpected keyword argument 'by'`.  The MR 4 builder docstrings must state
  this limitation explicitly, and the Non-goals section (§Goals) already
  documents it.  `by()` support is tracked in `docs/design_group_step.md`.

---

## 7. Test plan

### 7.1 Bytecode round-trip (MR 1)

- `decode(encode(plan)) == plan` for all `LogicalStep` variants.

### 7.2 Smoke tests (MR 4)

- Open graph, add vertices + edge, commit, read back.

### 7.3 Value round-trip (MR 4)

- Every `Value` variant survives write → read → Python type conversion.

### 7.4 Sub-traversal tests (MR 4)

- `where_(__().otherV().hasId([2]))` filters correctly.
- `not_(__().hasLabel(["person"]))` inverts correctly.
- `repeat(__().out(["knows"])).times(3)` traverses 3 hops.

### 7.5 Error path (MR 4)

- Schema violation → `StoreError` raised.
- Rollback → data not persisted.
- Session after commit → `RuntimeError`.

### 7.6 Platform matrix (MR 5)

- Wheels install cleanly on macOS ARM64, macOS x86_64, Linux x86_64, Windows x86_64.

---

## 8. Out of scope (deferred to v0.2+)

- TinkerPop Bytecode (GLV) adapter — not yet available for the embedded use case;
  explicitly left open for community contribution (see §4).
- Bulk load — planned for v0.2.
- Schema management — separate design.
- Non-Python bindings (Node.js, Go, Java) — the compact binary protocol makes
  these trivial to add; deferred only because Python is the priority segment.
