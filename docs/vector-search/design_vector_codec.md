
# Design: Binary Codec Extension for Vector Search

Status: proposal — implementation prerequisite for v0.1 end-to-end tests.

---

## Table of Contents

- [1. Scope](#1-scope)
- [2. Existing codec structure](#2-existing-codec-structure)
- [3. New primitive tag: `PRIM_FLOATVECTOR`](#3-new-primitive-tag-prim_floatvector)
- [4. `GValue` and `Value` enum additions](#4-gvalue-and-value-enum-additions)
- [5. `Vector` Python and TypeScript classes](#5-vector-python-and-typescript-classes)
  - [5a. Python](#5a-python)
  - [5b. TypeScript](#5b-typescript)
- [6. Encoding `FloatVector` in requests](#6-encoding-floatvector-in-requests)
- [7. Decoding `FloatVector` in responses](#7-decoding-floatvector-in-responses)
- [8. New opcodes](#8-new-opcodes)
  - [8a. Opcode assignments](#8a-opcode-assignments)
  - [8b. `OP_VECTORNEAR` (61)](#8b-op_vectornear-61)
  - [8c. `OP_VECTORSIMILARITY` (62)](#8c-op_vectorsimilarity-62)
  - [8d. `OP_NEARESTBY` (63)](#8d-op_nearestby-63)
  - [8e. `OP_WITHEFSEARCH` (64)](#8e-op_withefsearch-64)
  - [8f. `OP_WITHOVERFETCH` (66)](#8f-op_withoverfetch-66)
  - [8h. `OP_VECTORNEAR_MULTI` (67)](#8h-op_vectornear_multi-67)
- [9. Rust decoder additions](#9-rust-decoder-additions)
  - [9a. `bytecode/mod.rs` — new constants and `decode_step` arms](#9a-bytecodemodrs--new-constants-and-decode_step-arms)
  - [9b. `LogicalStep` additions](#9b-logicalstep-additions)
- [10. Complete opcode and primitive tag tables](#10-complete-opcode-and-primitive-tag-tables)
- [11. Wire format examples](#11-wire-format-examples)
  - [11a. Writing a vector property](#11a-writing-a-vector-property)
  - [11b. `vectorNear` query with score annotation](#11b-vectornear-query-with-score-annotation)
- [12. Endianness rules](#12-endianness-rules)
- [13. Implementation checklist](#13-implementation-checklist)

---

## 1. Scope

The Python and TypeScript clients send binary-encoded traversal plans to the Rust
engine. The Rust engine executes and returns results serialized as Python/JS objects.
Adding vector search requires extending this protocol in four places:

1. A new primitive tag (`PRIM_FLOATVECTOR`) for encoding `Vector` values in
   `OP_PROPERTY` and `OP_ADDV`/`OP_ADDE` payloads.
2. New opcodes for `vectorNear`, `vectorSimilarity`, `nearestBy`, and ANN search hints.
3. A new `Value` enum variant for `FloatVector` in the response path.
4. New Python/TS classes (`Vector`) and updates to `_encode_primitive`,
   `_post_process`, and `value_to_py`.

Score retrieval (`vectorSimilarity`) uses the standard `project()` pattern — no
dedicated response wrapper types are needed. See `design_vector_api.md` §3a.

All changes are backward-compatible: existing opcodes and primitive tags are
unchanged, and existing traversals continue to decode identically.

---

## 2. Existing codec structure

```
Request bytes:
  [VERSION: u8 = 0x01]
  [step_count: u16 BE]
  for each step:
    [opcode: u8]
    [step-specific payload...]

Response: Python list / JS array of Value objects
  produced by value_to_py() in bindings/python/src/lib.rs
  wrapped by _post_process() in rocksgraph/_builder.py
```

Existing constants in `rocksgraph/src/bytecode/mod.rs`:
- Opcodes `OP_BOTH = 1` … `OP_LOCAL = 60`
- Primitives `PRIM_NULL = 0` … `PRIM_BYTES = 9`

Existing Python mirrors in `rocksgraph/_codec.py`:
- Same opcode numbers (1–60)
- Same primitive numbers (0–9)

New additions must not reuse any of these values.

---

## 3. New primitive tag: `PRIM_FLOATVECTOR`

```python
# rocksgraph/_codec.py  — append after PRIM_BYTES = 9
PRIM_FLOATVECTOR = 10
```

```rust
// rocksgraph/src/bytecode/mod.rs — append after PRIM_BYTES
pub const PRIM_FLOATVECTOR: u8 = 10;
```

**Wire encoding** for a `FloatVector` value (used inside `OP_PROPERTY` and
`OP_ADDV`/`OP_ADDE` property maps):

```
[PRIM_FLOATVECTOR: u8 = 10]
[dim: u32 BE]                 // number of f32 elements
[f32_data: dim × 4 bytes]     // raw f32 values, little-endian each
```

The `dim` field allows the Rust decoder to validate dimension against the declared
`VectorIndexConfig` before touching the in-memory index (fast fail, no partial write).

**Endianness note**: structural fields (`dim`, string lengths, counts) are big-endian
throughout the protocol. The `f32_data` blob uses little-endian f32 for each element
because that is native on x86 and ARM — numpy's `ndarray.tobytes()` and JS's
`Float32Array.buffer` are both little-endian on all common deployment targets,
making encoding and decoding zero-copy on the dominant platforms. See §12.

---

## 4. `GValue` and `Value` enum additions

### `GValue` (internal pipeline type)

File: `rocksgraph/src/types/gvalue.rs`

```rust
pub enum GValue {
    Vertex(VertexKey),
    Edge(EdgeKey),
    Property(Property),
    Scalar(Primitive),
    List(Vec<GValue>),
    Map(Vec<(GValue, GValue)>),
    Path(Vec<(GValue, Option<SmallVec<[SmolStr; STEP_LABEL_INLINE]>>)>),
    // ── NEW ────────────────────────────────────────────────────────────
    FloatVector(Vec<f32>),
}
```

`FloatVector` is a top-level `GValue` variant, **not** `Scalar(Primitive::FloatVector)`.
Rationale: `Primitive` is for values that can appear in predicates (`P.eq`, `P.gt`,
etc.). FloatVector cannot meaningfully be used in a predicate and should not be
processed by scalar filter steps. Keeping it at the top level makes the type
system reject misuse at the Rust level.

Similarity scores produced by `VectorSimilarityStep` are plain `f32` scalars
(`GValue::Scalar(Primitive::Float32)`), surfaced to callers via the standard
`project()` step. No dedicated `ScoredVertex`/`ScoredEdge` wrapper is needed.

### `Value` (public-facing response type)

File: `rocksgraph/src/gremlin/value.rs`

```rust
pub enum Value {
    // ... existing variants unchanged ...
    // ── NEW ────────────────────────────────────────────────────────────
    FloatVector(Vec<f32>),
}
```

The conversion from `GValue` to `Value` (via the existing `From` / `Into` impl)
must handle the new `FloatVector` variant.

### Hash implementation for `GValue::FloatVector`

`GValue` must implement `Hash` for dedup steps. NaN bit patterns are canonicalized:

```rust
impl Hash for GValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            // ... existing arms ...
            GValue::FloatVector(v) => {
                for f in v {
                    let bits = if f.is_nan() { f32::NAN.to_bits() } else { f.to_bits() };
                    bits.hash(state);
                }
            }
        }
    }
}
```

---

## 5. `Vector` Python and TypeScript classes

### 5a. Python

File: `rocksgraph/_types.py`

```python
import struct
from typing import Union

try:
    import numpy as np
    _NUMPY = True
except ImportError:
    _NUMPY = False


class Vector:
    """Dense float32 vector for use as a graph property or ANN query."""

    __slots__ = ("_data",)

    def __init__(self, data: Union[list, bytes, "np.ndarray"]):
        if _NUMPY and isinstance(data, np.ndarray):
            self._data: bytes = data.astype(np.float32).tobytes()
        elif isinstance(data, (list, tuple)):
            self._data = struct.pack(f"<{len(data)}f", *data)
        elif isinstance(data, (bytes, bytearray, memoryview)):
            if len(data) % 4 != 0:
                raise ValueError("bytes length must be a multiple of 4")
            self._data = bytes(data)
        else:
            raise TypeError(f"Vector accepts list, np.ndarray, or bytes; got {type(data)}")

    @property
    def dim(self) -> int:
        return len(self._data) // 4

    def tolist(self) -> list:
        return list(struct.unpack(f"<{self.dim}f", self._data))

    def numpy(self) -> "np.ndarray":
        if not _NUMPY:
            raise ImportError("numpy is not installed")
        return np.frombuffer(self._data, dtype=np.float32).copy()

    def __len__(self) -> int:
        return self.dim

    def __repr__(self) -> str:
        return f"Vector(dim={self.dim})"

    def __eq__(self, other) -> bool:
        return isinstance(other, Vector) and self._data == other._data

    def __hash__(self) -> int:
        return hash(self._data)
```

**Response representation**: when `value_to_py` returns a `FloatVector`, it becomes
a Python `Vector` instance (not a raw list), so that users can immediately call
`.numpy()` or `.tolist()` on results from `.values("embedding")`.

### 5b. TypeScript

File: `bindings/nodejs/index.ts`

```typescript
export class Vector {
    readonly data: Float32Array;

    constructor(data: Float32Array | number[]) {
        if (data instanceof Float32Array) {
            this.data = data;
        } else {
            this.data = Float32Array.from(data);
        }
    }

    get dim(): number { return this.data.length; }

    // Zero-copy access to the underlying Buffer for napi-rs
    toBuffer(): Buffer {
        return Buffer.from(this.data.buffer, this.data.byteOffset, this.data.byteLength);
    }
}
```

---

## 6. Encoding `FloatVector` in requests

### Python `_encode_primitive` addition

File: `rocksgraph/_codec.py` — extend `_encode_primitive`:

```python
def _encode_primitive(val: Any, buf: bytearray):
    from ._types import Int32, Int64, UInt16, Float32, Float64, Uuid, Vector
    # ... existing isinstance checks unchanged ...
    elif isinstance(val, Vector):
        buf.append(PRIM_FLOATVECTOR)
        buf.extend(struct.pack(">I", val.dim))   # big-endian u32 element count
        buf.extend(val._data)                    # raw LE f32 bytes
    else:
        raise ValueError(f"Unsupported primitive type: {type(val)}")
```

The `val._data` bytes are already little-endian f32 (set in `Vector.__init__`).

### Builder method addition

File: `rocksgraph/_builder.py` — `Traversal.property` already delegates to
`_encode_primitive`; no builder change needed. `Vector` values flow through the
existing `OP_PROPERTY` path automatically.

### Rust `decode_primitive` addition

File: `rocksgraph/src/bytecode/mod.rs`:

```rust
fn decode_primitive(bytes: &[u8], offset: &mut usize) -> Result<GValue, StoreError> {
    let tag = read_u8(bytes, offset)?;
    match tag {
        // ... existing arms 0–9 unchanged ...
        PRIM_FLOATVECTOR => {
            let dim = read_u32_be(bytes, offset)? as usize;
            let byte_len = dim * 4;
            if *offset + byte_len > bytes.len() {
                return Err(StoreError::CodecError("truncated vector data".into()));
            }
            let mut vec = Vec::with_capacity(dim);
            for i in 0..dim {
                let f = f32::from_le_bytes(
                    bytes[*offset + i * 4 .. *offset + i * 4 + 4]
                        .try_into()
                        .unwrap()
                );
                vec.push(f);
            }
            *offset += byte_len;
            Ok(GValue::FloatVector(vec))
        }
        other => Err(StoreError::CodecError(format!("unknown primitive tag {other}").into())),
    }
}
```

---

## 7. Decoding `FloatVector` in responses

### `value_to_py` addition

File: `bindings/python/src/lib.rs`:

```rust
fn value_to_py(py: Python<'_>, value: Value) -> PyResult<PyObject> {
    match value {
        // ... existing arms unchanged ...
        Value::FloatVector(v) => {
            // Return a Python Vector object wrapping the raw bytes.
            // Import lazily to avoid requiring numpy at module load.
            let rocksgraph = py.import_bound("rocksgraph")?;
            let vector_cls = rocksgraph.getattr("Vector")?;
            // Pack as LE f32 bytes, pass as Python bytes
            let mut buf: Vec<u8> = Vec::with_capacity(v.len() * 4);
            for f in &v {
                buf.extend_from_slice(&f.to_le_bytes());
            }
            let py_bytes = PyBytes::new_bound(py, &buf);
            Ok(vector_cls.call1((py_bytes,))?.into())
        }
    }
}
```

Similarity scores (`f32`) returned by `VectorSimilarityStep` are plain
`Value::Scalar(Primitive::Float32)` and decoded by the existing scalar arm — no
new arm is needed.

---

## 8. New opcodes

### 8a. Opcode assignments

```python
# rocksgraph/_codec.py — append after OP_LOCAL = 60
OP_VECTORNEAR         = 61
OP_VECTORSIMILARITY   = 62
OP_NEARESTBY          = 63
OP_WITHEFSEARCH       = 64
OP_WITHNPROBE         = 65
OP_WITHOVERFETCH      = 66
OP_VECTORNEAR_MULTI   = 67   # v0.5+
```

```rust
// rocksgraph/src/bytecode/mod.rs — append after OP_LOCAL
pub const OP_VECTORNEAR:         u8 = 61;
pub const OP_VECTORSIMILARITY:   u8 = 62;
pub const OP_NEARESTBY:          u8 = 63;
pub const OP_WITHEFSEARCH:       u8 = 64;
pub const OP_WITHNPROBE:         u8 = 65;
pub const OP_WITHOVERFETCH:      u8 = 66;
pub const OP_VECTORNEAR_MULTI:   u8 = 67;  // v0.5+
```

---

### 8b. `OP_VECTORNEAR` (61)

**Semantics**: syntactic-sugar step that the optimizer rewrites to
`order().by(vectorSimilarity(prop, q), desc).limit(k)`. Returns the `k` most
similar entities to `query` on property `prop_key`. This is the only attachment
point for ANN hint modulators (`withEfSearch`, `withOverfetch`).

**Wire encoding**:

```
[OP_VECTORNEAR: u8 = 61]
[prop_key_len: u16 BE]
[prop_key: prop_key_len bytes, UTF-8]
[k: u32 BE]
[dim: u32 BE]
[vector_data: dim × 4 bytes, LE f32]
```

**Python builder**:

```python
# rocksgraph/_codec.py — in _encode_step:
elif opcode == OP_VECTORNEAR:
    prop_key, query, k = args         # query is a Vector instance
    _encode_string(prop_key, buf)
    buf.extend(struct.pack(">I", k))
    buf.extend(struct.pack(">I", query.dim))
    buf.extend(query._data)           # LE f32 bytes

# rocksgraph/_builder.py — in Traversal:
def vectorNear(self, property: str, query: "Vector", k: int) -> "Traversal":
    return self._add(OP_VECTORNEAR, (property, query, k))
```

**Rust decoder** (in `decode_step`):

```rust
OP_VECTORNEAR => {
    let prop_key = read_smolstr(bytes, offset)?;
    let k        = read_u32_be(bytes, offset)? as usize;
    let dim      = read_u32_be(bytes, offset)? as usize;
    let byte_len = dim * 4;
    if *offset + byte_len > bytes.len() {
        return Err(StoreError::CodecError("truncated vectorNear query".into()));
    }
    let mut query = Vec::with_capacity(dim);
    for i in 0..dim {
        query.push(f32::from_le_bytes(
            bytes[*offset + i*4 .. *offset + i*4 + 4].try_into().unwrap()
        ));
    }
    *offset += byte_len;
    Ok(LogicalStep::VectorNear(VectorNearStep { prop_key, k, query }))
}
```

---

### 8c. `OP_VECTORSIMILARITY` (62)

**Semantics**: map step `Vertex/Edge → f32`. Reads the named property from the
incoming traverser, computes normalized similarity against `query_vec` using the
given metric (or the metric inferred from the declared index when `metric` is
omitted). Higher score always means more similar, regardless of metric:

| Metric | Score formula |
|--------|--------------|
| Cosine | cosine similarity (unchanged) |
| L2 / Euclidean | `1 / (1 + l2_distance)` |
| InnerProduct | `sigmoid(inner_product)` |

**Cache behavior**: when the ANN index scan for `vectorNear` has already computed
similarity scores, the engine caches `(prop_name, query_vec) → score` on each
traverser. A subsequent `vectorSimilarity(prop, query_vec)` call with the same
`(prop_name, query_vec)` pair is a cache hit with zero re-computation cost.

**Wire encoding**:

```
[OP_VECTORSIMILARITY: u8 = 62]
[prop_key_len: u16 BE]
[prop_key: prop_key_len bytes, UTF-8]
[metric: u8]                  // 0x00=infer-from-index, 0x01=cosine, 0x02=l2, 0x03=ip
[dim: u32 BE]
[vector_data: dim × 4 bytes, LE f32]
```

Metric tag `0x00` ("infer from index") is the default. When no declared
`VectorIndexConfig` exists for `prop_key`, the engine raises
`VectorError::MetricRequired { property }` — not `VectorError::NoVectorIndex`
(that is reserved for `vectorNear` and `nearestBy`). Explicit metric tags
`0x01`–`0x03` allow brute-force similarity on un-indexed `FloatVector` properties
without raising any error.

**Python builder**:

```python
# Metric tag constants (rocksgraph/_codec.py)
METRIC_INFER  = 0x00
METRIC_COSINE = 0x01
METRIC_L2     = 0x02
METRIC_IP     = 0x03

_METRIC_TAG = {"cosine": METRIC_COSINE, "l2": METRIC_L2, "ip": METRIC_IP}

# rocksgraph/_codec.py — in _encode_step:
elif opcode == OP_VECTORSIMILARITY:
    prop_key, query, metric = args    # metric is str | None
    _encode_string(prop_key, buf)
    buf.append(_METRIC_TAG.get(metric, METRIC_INFER))
    buf.extend(struct.pack(">I", query.dim))
    buf.extend(query._data)

# rocksgraph/_builder.py — in Traversal (and in AnonymousTraversal / __):
def vectorSimilarity(self, prop_name: str, query_vec: "Vector",
                     metric: str | None = None) -> "Traversal":
    return self._add(OP_VECTORSIMILARITY, (prop_name, query_vec, metric))
```

**Rust decoder**:

```rust
OP_VECTORSIMILARITY => {
    let prop_key = read_smolstr(bytes, offset)?;
    let metric   = read_u8(bytes, offset)?;
    let dim      = read_u32_be(bytes, offset)? as usize;
    let byte_len = dim * 4;
    if *offset + byte_len > bytes.len() {
        return Err(StoreError::CodecError("truncated vectorSimilarity query".into()));
    }
    let mut query = Vec::with_capacity(dim);
    for i in 0..dim {
        query.push(f32::from_le_bytes(
            bytes[*offset + i*4 .. *offset + i*4 + 4].try_into().unwrap()
        ));
    }
    *offset += byte_len;
    Ok(LogicalStep::VectorSimilarity(VectorSimilarityStep { prop_key, metric, query }))
}
```

---

### 8d. `OP_NEARESTBY` (63)

**Semantics**: flat-map step `Vertex/Edge → [Vertex/Edge × k]`. Reads the vector
stored in `source_prop` of the incoming traverser, searches the ANN index on
`target_prop` of `entity_type` entities, and emits the `k` most similar results.
Must always appear inside `local()`.

**Wire encoding**:

```
[OP_NEARESTBY: u8 = 63]
[source_prop_len: u16 BE]
[source_prop: source_prop_len bytes, UTF-8]
[target_prop_len: u16 BE]
[target_prop: target_prop_len bytes, UTF-8]
[k: u32 BE]
[entity_type: u8]             // 0x00=vertex, 0x01=edge
```

**Python builder**:

```python
# rocksgraph/_codec.py
ENTITY_VERTEX = 0x00
ENTITY_EDGE   = 0x01

elif opcode == OP_NEARESTBY:
    source_prop, target_prop, k, entity_type = args
    _encode_string(source_prop, buf)
    _encode_string(target_prop, buf)
    buf.extend(struct.pack(">I", k))
    buf.append(entity_type)

# rocksgraph/_builder.py — in AnonymousTraversal / __:
def nearestBy(self, source_prop: str, target_prop: str, k: int,
              entity_type: "VectorEntityType") -> "Traversal":
    from ._codec import ENTITY_VERTEX, ENTITY_EDGE
    tag = ENTITY_VERTEX if entity_type == VectorEntityType.VERTEX else ENTITY_EDGE
    return self._add(OP_NEARESTBY, (source_prop, target_prop, k, tag))
```

**Rust decoder**:

```rust
OP_NEARESTBY => {
    let source_prop = read_smolstr(bytes, offset)?;
    let target_prop = read_smolstr(bytes, offset)?;
    let k           = read_u32_be(bytes, offset)? as usize;
    let entity_type = read_u8(bytes, offset)?;
    Ok(LogicalStep::NearestBy(NearestByStep { source_prop, target_prop, k, entity_type }))
}
```

**Validation**: the execution engine raises `VectorError::NoVectorIndex` if no
index is declared on `target_prop` for the given `entity_type`.

---

### 8e. `OP_WITHEFSEARCH` (64)

**Semantics**: per-query HNSW `ef_search` override. No effect on `BruteForce`
indexes. Raises `VectorError::WrongAlgorithmParam` on `BruteForce` (no ef_search concept).

**Wire encoding**:

```
[OP_WITHEFSEARCH: u8 = 64]
[ef: u32 BE]
```

**Python builder**:

```python
elif opcode == OP_WITHEFSEARCH:
    buf.extend(struct.pack(">I", args))

def withEfSearch(self, ef: int) -> "Traversal":
    return self._add(OP_WITHEFSEARCH, ef)
```

**Rust decoder**:

```rust
OP_WITHEFSEARCH => {
    let ef = read_u32_be(bytes, offset)? as usize;
    Ok(LogicalStep::WithEfSearch(ef))
}
```

---

### 8f. `OP_WITHOVERFETCH` (66)

**Semantics**: hint to the engine to fetch `k × factor` ANN candidates before
applying graph predicates. Only meaningful in post-filter mode (v0.2). Silently
ignored in pre-filter mode (v0.3). Raises `VectorError::InvalidParam` if
`factor < 1.0`.

**Wire encoding**:

```
[OP_WITHOVERFETCH: u8 = 66]
[factor: f32 BE]
```

**Python builder**:

```python
elif opcode == OP_WITHOVERFETCH:
    buf.extend(struct.pack(">f", args))

def withOverfetch(self, factor: float) -> "Traversal":
    return self._add(OP_WITHOVERFETCH, factor)
```

**Rust decoder**:

```rust
OP_WITHOVERFETCH => {
    let factor = read_f32_be(bytes, offset)?;
    Ok(LogicalStep::WithOverfetch(factor))
}
```

---

### 8h. `OP_VECTORNEAR_MULTI` (67)

**Semantics**: ANN search with multiple query vectors, results merged via a fusion
strategy. Deferred to v0.5+ — the opcode and wire format are defined now so that
Python/TS builders can be written today; the Rust decoder returns
`VectorError::NotImplemented` until v0.5+.

**Wire encoding**:

```
[OP_VECTORNEAR_MULTI: u8 = 67]
[prop_key_len: u16 BE]
[prop_key: bytes]
[k: u32 BE]
[fusion: u8]              // 0x00=rrf, 0x01=max, 0x02=mean
[query_count: u16 BE]
for each query:
  [dim: u32 BE]
  [vector_data: dim × 4 bytes, LE f32]
```

**Fusion tag values**:

```python
FUSION_RRF  = 0x00   # reciprocal rank fusion (default)
FUSION_MAX  = 0x01   # per-candidate max score
FUSION_MEAN = 0x02   # per-candidate mean score
```

**Python builder**:

```python
elif opcode == OP_VECTORNEAR_MULTI:
    prop_key, queries, k, fusion = args
    _encode_string(prop_key, buf)
    buf.extend(struct.pack(">I", k))
    buf.append(fusion)
    buf.extend(struct.pack(">H", len(queries)))
    for q in queries:
        buf.extend(struct.pack(">I", q.dim))
        buf.extend(q._data)

# In Traversal — overloads vectorNear to accept a list:
def vectorNear(self, property: str, query, k: int,
               fusion: str = "rrf") -> "Traversal":
    from ._codec import FUSION_RRF, FUSION_MAX, FUSION_MEAN
    if isinstance(query, list):
        fusion_tag = {"rrf": FUSION_RRF, "max": FUSION_MAX,
                      "mean": FUSION_MEAN}[fusion]
        return self._add(OP_VECTORNEAR_MULTI, (property, query, k, fusion_tag))
    return self._add(OP_VECTORNEAR, (property, query, k))
```

---

## 9. Rust decoder additions

### 9a. `bytecode/mod.rs` — new constants and `decode_step` arms

Add the 7 new opcode constants after `OP_LOCAL = 60` (shown in §8a), then add
the corresponding match arms to `decode_step`. Each arm is shown in §8b–§8h.

Two helper functions must be added alongside the existing `read_u8`, `read_u16`,
`read_i64`, `read_smolstr`:

```rust
fn read_u32_be(bytes: &[u8], offset: &mut usize) -> Result<u32, StoreError> {
    let b: [u8; 4] = bytes.get(*offset .. *offset + 4)
        .ok_or_else(|| StoreError::CodecError("unexpected EOF (u32)".into()))?
        .try_into().unwrap();
    *offset += 4;
    Ok(u32::from_be_bytes(b))
}

fn read_f32_be(bytes: &[u8], offset: &mut usize) -> Result<f32, StoreError> {
    let b: [u8; 4] = bytes.get(*offset .. *offset + 4)
        .ok_or_else(|| StoreError::CodecError("unexpected EOF (f32)".into()))?
        .try_into().unwrap();
    *offset += 4;
    Ok(f32::from_be_bytes(b))
}
```

### 9b. `LogicalStep` additions

File: `rocksgraph/src/planner/logical_step/mod.rs`

```rust
pub enum LogicalStep {
    // ... existing 61 variants unchanged ...

    // ── Vector search steps ──────────────────────────────────────────
    VectorNear(VectorNearStep),
    VectorSimilarity(VectorSimilarityStep),
    NearestBy(NearestByStep),
    VectorNearMulti(VectorNearMultiStep),  // v0.5+
    WithEfSearch(usize),
    WithOverfetch(f32),
}

pub struct VectorNearStep {
    pub prop_key: SmolStr,
    pub k:        usize,
    pub query:    Vec<f32>,
}

pub struct VectorSimilarityStep {
    pub prop_key: SmolStr,
    pub metric:   u8,       // 0x00=infer, 0x01=cosine, 0x02=l2, 0x03=ip
    pub query:    Vec<f32>,
}

pub struct NearestByStep {
    pub source_prop: SmolStr,
    pub target_prop: SmolStr,
    pub k:           usize,
    pub entity_type: u8,    // 0x00=vertex, 0x01=edge
}

pub struct VectorNearMultiStep {              // v0.5+
    pub prop_key: SmolStr,
    pub k:        usize,
    pub queries:  Vec<Vec<f32>>,
    pub fusion:   FusionStrategy,
}

pub enum FusionStrategy { Rrf, Max, Mean }   // v0.5+
```

---

## 10. Complete opcode and primitive tag tables

### Primitive tags

| Tag | Constant | Type | Payload after tag |
|:---:|----------|------|-------------------|
| 0 | `PRIM_NULL` | Null | none |
| 1 | `PRIM_BOOL` | Bool | `u8` (0 or 1) |
| 2 | `PRIM_INT32` | i32 | `i32 BE` |
| 3 | `PRIM_INT64` | i64 | `i64 BE` |
| 4 | `PRIM_UINT16` | u16 | `u16 BE` |
| 5 | `PRIM_FLOAT32` | f32 | `f32 BE` |
| 6 | `PRIM_FLOAT64` | f64 | `f64 BE` |
| 7 | `PRIM_STRING` | String | `[len: u16 BE][utf8 bytes]` |
| 8 | `PRIM_UUID` | UUID | `[16 bytes]` |
| 9 | `PRIM_BYTES` | Bytes | `[len: u32 BE][bytes]` |
| **10** | **`PRIM_FLOATVECTOR`** | **Vec\<f32\>** | **`[dim: u32 BE][dim × f32 LE]`** |

### Opcodes

| Opcode | Constant | Ships | Payload summary |
|:------:|----------|:-----:|-----------------|
| 1–60 | `OP_BOTH` … `OP_LOCAL` | v0.1 | Unchanged — see existing `bytecode/mod.rs` |
| **61** | **`OP_VECTORNEAR`** | **v0.1** | `[prop_key][k: u32 BE][dim: u32 BE][dim × f32 LE]` |
| **62** | **`OP_VECTORSIMILARITY`** | **v0.1** | `[prop_key][metric: u8][dim: u32 BE][dim × f32 LE]` |
| **63** | **`OP_NEARESTBY`** | **v0.2** | `[source_prop][target_prop][k: u32 BE][entity_type: u8]` |
| **64** | **`OP_WITHEFSEARCH`** | **v0.2** | `[ef: u32 BE]` |
| **65** | reserved (was `OP_WITHNPROBE`; IVF removed from roadmap) | — | — |
| **66** | **`OP_WITHOVERFETCH`** | **v0.3** | `[factor: f32 BE]` |
| **67** | **`OP_VECTORNEAR_MULTI`** | **v0.5+** | `[prop_key][k][fusion: u8][count: u16][dim][data]…` |

---

## 11. Wire format examples

### 11a. Writing a vector property

Python call:
```python
tx.g().addV("doc").property("embedding", Vector([0.1, 0.2, 0.3])).next()
```

Encoded bytes (VERSION byte + 2 steps):

```
01                           VERSION = 1
00 02                        step_count = 2

-- Step 1: OP_ADDV (19) --
13                           opcode = OP_ADDV
00 03 64 6f 63              label = "doc" (len=3)
00                           vid = None
00 00                        props count = 0

-- Step 2: OP_PROPERTY (23) --
17                           opcode = OP_PROPERTY
00 09 65 6d 62 65 64 64
      69 6e 67              key = "embedding" (len=9)
0a                           PRIM_FLOATVECTOR = 10
00 00 00 03                  dim = 3
cd cc cc 3d                  0.1 as LE f32
cd cc 4c 3e                  0.2 as LE f32
9a 99 99 3e                  0.3 as LE f32
```

### 11b. `vectorNear` query with score annotation

Python call:
```python
rs.g().V() \
    .vectorNear("embedding", Vector([0.1, 0.2, 0.3]), k=5) \
    .project("vertex", "similarity") \
      .by(identity()) \
      .by(__.vectorSimilarity("embedding", Vector([0.1, 0.2, 0.3]))) \
    .to_list()
```

Encoded bytes (VERSION + 3 steps — `project` and its `by` sub-traversals use
existing opcodes and are omitted here for brevity; only the vector steps shown):

```
01                           VERSION = 1
00 03+                       step_count = 3 + project steps

-- Step 1: OP_V (24) --
18                           opcode = OP_V
00 00                        ids count = 0 (all vertices)

-- Step 2: OP_VECTORNEAR (61) --
3d                           opcode = OP_VECTORNEAR (0x3d = 61)
00 09 65 6d 62 65 64 64
      69 6e 67              prop_key = "embedding" (len=9)
00 00 00 05                  k = 5
00 00 00 03                  dim = 3
cd cc cc 3d                  0.1 as LE f32
cd cc 4c 3e                  0.2 as LE f32
9a 99 99 3e                  0.3 as LE f32

-- Step 3 (inside project by): OP_VECTORSIMILARITY (62) --
3e                           opcode = OP_VECTORSIMILARITY (0x3e = 62)
00 09 65 6d 62 65 64 64
      69 6e 67              prop_key = "embedding" (len=9)
00                           metric = 0x00 (infer from index)
00 00 00 03                  dim = 3
cd cc cc 3d                  0.1 as LE f32
cd cc 4c 3e                  0.2 as LE f32
9a 99 99 3e                  0.3 as LE f32
```

Response from Rust (before `_post_process`):
```python
[
    {"vertex": {"id": 7, "label": "doc", "properties": {...}}, "similarity": 0.9821},
    {"vertex": {"id": 2, "label": "doc", "properties": {...}}, "similarity": 0.9643},
    ...
]
```

After `_post_process` (standard `project()` dict — no wrapper type needed):
```python
[
    {"vertex": Vertex(id=7, label='doc', ...), "similarity": 0.9821},
    {"vertex": Vertex(id=2, label='doc', ...), "similarity": 0.9643},
    ...
]
```

---

## 12. Endianness rules

| Field type | Endianness | Rationale |
|------------|:----------:|-----------|
| All structural integers (step count, string lengths, `k`, `dim`, `ef`, `nprobe`) | Big-endian | Consistent with existing protocol |
| `f32` in predicate values (`PRIM_FLOAT32`, `OP_WITHOVERFETCH`) | Big-endian | Consistent with existing `PRIM_FLOAT32` encoding |
| `f32` elements in vector data (`PRIM_FLOATVECTOR`, `OP_VECTORNEAR`, `OP_VECTORSIMILARITY`, `OP_VECTORNEAR_MULTI`) | **Little-endian** | Native format on x86/ARM; numpy `tobytes()` and JS `Float32Array.buffer` are LE; zero-copy encode/decode on common platforms |

The asymmetry (LE for bulk vector data, BE for everything else) is intentional and
permanently fixed. The `dim` field immediately before the vector data is always
big-endian; only the f32 elements themselves are little-endian.

---

## 13. Implementation checklist

### Python (`rocksgraph/_codec.py`, `rocksgraph/_builder.py`, `rocksgraph/_types.py`)

- [ ] Add `PRIM_FLOATVECTOR = 10` to `_codec.py`
- [ ] Add `OP_VECTORNEAR = 61` … `OP_VECTORNEAR_MULTI = 67` to `_codec.py`
- [ ] Add `METRIC_INFER`, `METRIC_COSINE`, `METRIC_L2`, `METRIC_IP` constants to `_codec.py`
- [ ] Add `ENTITY_VERTEX`, `ENTITY_EDGE` constants to `_codec.py`
- [ ] Add `FUSION_RRF`, `FUSION_MAX`, `FUSION_MEAN` constants to `_codec.py`
- [ ] Extend `_encode_primitive` to handle `Vector` → `PRIM_FLOATVECTOR`
- [ ] Add `_encode_step` cases for all 7 new opcodes
- [ ] Add `Vector` class to `_types.py` (numpy optional dependency)
- [ ] Add `vectorNear`, `vectorSimilarity`, `nearestBy`, `withEfSearch`, `withOverfetch` methods to `Traversal` and `AnonymousTraversal` (`__`)
- [ ] Export `Vector` from `rocksgraph/__init__.py`
- [ ] Add `Vector` to `rocksgraph/__init__.pyi` type stubs

### Rust (`rocksgraph/src/`)

- [ ] Add `PRIM_FLOATVECTOR = 10` to `bytecode/mod.rs`
- [ ] Add `OP_VECTORNEAR = 61` … `OP_VECTORNEAR_MULTI = 67` to `bytecode/mod.rs`
- [ ] Add `read_u32_be` and `read_f32_be` helpers to `bytecode/mod.rs`
- [ ] Extend `decode_primitive` with `PRIM_FLOATVECTOR` arm
- [ ] Add 7 new `decode_step` match arms in `bytecode/mod.rs`
- [ ] Add `GValue::FloatVector` to `types/gvalue.rs`
- [ ] Add `Hash` impl arm for `GValue::FloatVector`
- [ ] Add `Value::FloatVector` to `gremlin/value.rs`
- [ ] Add `VectorNearStep`, `VectorSimilarityStep`, `NearestByStep`, `WithEfSearch`, `WithOverfetch` to `planner/logical_step/mod.rs`
- [ ] Add `value_to_py` arm for `FloatVector` in `bindings/python/src/lib.rs`
- [ ] Add codec round-trip tests to `bindings/python/tests/test_codec.py`
