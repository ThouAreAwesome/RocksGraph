# Design: `Bytes` — a new primitive property type (Phase 1)

Status: implemented (Phase 1) — `Bytes(Vec<u8>)` type and `Null`-first reordering
landed and verified (692/692 lib tests, clippy/fmt clean).  `Timestamp` and `Date` remain
deferred; see **Deferred: Timestamp and Date** section.

## Problem

RocksGraph stores property values as one of nine `Primitive` variants. Binary payloads —
ML embeddings, hash digests, binary keys, arbitrary blobs — have no first-class
representation. Callers must encode them as base64 `String`, which wastes space, makes
equality predicates unreliable (encoding variants), and loses type information.

## Phase 1 scope

**Only `Bytes(Vec<u8>)` is added in Phase 1.**

`Timestamp` and `Date` are deferred to Phase 2. The primary reason: Rust's standard library
has no ergonomic native `Date` type and no `Timestamp(i64)` constructor. The builder API
would require callers to compute raw epoch milliseconds or days-since-epoch themselves, and
display formatting (in `explain()`, error messages) would show raw integers. A proper
implementation needs either `chrono` (well-maintained but adds compile time and binary
size) or `time` crate — adding an external dependency for a single type is not justified
until the need is confirmed. See `## Deferred: Timestamp and Date` for details.

## Goals & non-goals

**Goals:**
- Add `Primitive::Bytes(Vec<u8>)` as a new scalar variant.
- On-disk encoding: 2-byte `u16` length prefix + raw bytes (max 65 535 bytes).
- Reorder `Null` to be the **first** variant in `Primitive`, `DataType`, and the property
  blob TAG table. No backward storage compatibility is required.
- Full propagation through all six layers.
- All comparison predicates are enabled; ordering is byte-lexicographic.

**Non-goals:**
- `Timestamp` / `Date` — Phase 2.
- Bytes larger than 65 535 bytes — needs a chunking or external-blob design.
- Compression of byte payloads.

## Reordering: Null moves to position 0

Since no backward storage compatibility is required, `Null` moves to first position in
all three on-disk/in-memory numbering spaces. This resolves the existing hack where
`Primitive::Null` mapped to `DataType::String` — `DataType::Null` is now its own variant.

### `Primitive` enum (new order)

```rust
pub enum Primitive {
    Null,              // ← was last; now first
    Bool(bool),
    Int32(i32),
    Int64(i64),
    UInt16(u16),
    Float32(f32),
    Float64(f64),
    String(SmolStr),
    Uuid(u128),
    Bytes(Vec<u8>),   // ← new
}
```

### Property blob TAGs (`encoding.rs`) — new numbering

```rust
const TAG_NULL:    u8 = 0;   // ← was 7; moved to 0
const TAG_BOOL:    u8 = 1;   // ← was 0
const TAG_INT32:   u8 = 2;   // ← was 1
const TAG_INT64:   u8 = 3;   // ← was 2
const TAG_FLOAT32: u8 = 4;   // ← was 3
const TAG_FLOAT64: u8 = 5;   // ← was 4
const TAG_STRING:  u8 = 6;   // ← was 5
const TAG_UUID:    u8 = 7;   // ← was 6
const TAG_UINT16:  u8 = 8;   // ← was 8 (unchanged)
const TAG_BYTES:   u8 = 9;   // ← new
```

### `DataType` enum (`schema/definition.rs`) — new numbering

`DataType::Null` is added as a first-class variant (previously `Primitive::Null` mapped
to `DataType::String` as a workaround — that mapping is removed).

```rust
#[repr(u8)]
pub enum DataType {
    Null    = 0,   // ← new variant; first position
    Bool    = 1,   // ← was 0
    Int32   = 2,   // ← was 1
    Int64   = 3,   // ← was 2
    Float32 = 4,   // ← was 3
    Float64 = 5,   // ← was 4
    String  = 6,   // ← was 5
    Uuid    = 7,   // ← was 6
    UInt16  = 8,   // ← was 7
    Bytes   = 9,   // ← new
}
```

`from_primitive` (previously `Primitive::Null => DataType::String`) becomes:
```rust
Primitive::Null     => DataType::Null,   // ← corrected
Primitive::Bytes(_) => DataType::Bytes,  // ← new
```

## Design

### Layers touched (in order)

| # | Layer | File |
|---|---|---|
| 1 | Internal scalar | `src/types/gvalue.rs` |
| 2 | On-disk codec | `src/store/rocks/encoding.rs` |
| 3 | Schema metadata | `src/schema/definition.rs` |
| 4 | Public scalar | `src/gremlin/value.rs` |
| 5 | Type bridge | `src/gremlin/type_bridge.rs` |
| 6 | Tests | encoding, schema, type_bridge, gremlin/tests.rs |

### Layer 1 — `Primitive` (`src/types/gvalue.rs`)

Add `Bytes(Vec<u8>)` variant (shown in full-order above).

Extend `PartialEq`, `Eq`, `Hash` to handle `Bytes` (byte-by-byte comparison/hash).
Extend `PartialOrd` to handle `(Bytes, Bytes)` via byte-lexicographic ordering:

```rust
(Self::Bytes(a), Self::Bytes(b)) => a.partial_cmp(b),
```

`is_integer()`, `is_numeric()`, and `to_i64()` are **not** extended — `Bytes` is neither
numeric nor integer.

`loose_eq` already falls through to `self == other` for non-integer/non-numeric pairs,
which is correct for `Bytes` (exact byte equality).

### Layer 2 — On-disk codec (`src/store/rocks/encoding.rs`)

Renumber all existing TAG constants as shown above.

Add encode arm:
```rust
Primitive::Bytes(b) => {
    assert!(b.len() <= u16::MAX as usize, "Bytes property exceeds 65535-byte limit");
    buf.push(TAG_BYTES);
    buf.extend_from_slice(&(b.len() as u16).to_be_bytes());
    buf.extend_from_slice(b);
}
```

Add decode arm:
```rust
TAG_BYTES => {
    if pos + 2 > blob.len() { return None; }
    let len = u16::from_be_bytes(blob[pos..pos + 2].try_into().ok()?) as usize;
    pos += 2;
    if pos + len > blob.len() { return None; }
    let b = blob[pos..pos + len].to_vec();
    pos += len;
    Primitive::Bytes(b)
}
```

**Length guard:** the `assert!` in encode is a defensive check; the real guard should be
at the property-write boundary (see Constraints). Property blobs with unknown future TAGs
continue to hit `_ => return None` in the decode match.

Reorder existing `TAG_BOOL`, `TAG_INT32`, … arms in the encode/decode match to match the
new numbering. The encode match is over variant names (unaffected), but the decode match
is over numeric TAG values and must reflect the new numbering.

### Layer 3 — Schema metadata (`src/schema/definition.rs`)

Rename no existing variants. Change `#[repr(u8)]` values for all variants (as shown above).
Add `DataType::Null = 0` and `DataType::Bytes = 9`.

Update `from_u8`:
```rust
0  => Some(DataType::Null),
1  => Some(DataType::Bool),
2  => Some(DataType::Int32),
3  => Some(DataType::Int64),
4  => Some(DataType::Float32),
5  => Some(DataType::Float64),
6  => Some(DataType::String),
7  => Some(DataType::Uuid),
8  => Some(DataType::UInt16),
9  => Some(DataType::Bytes),
```

Update `from_primitive`:
```rust
Primitive::Null     => DataType::Null,   // was DataType::String — corrected
Primitive::Bytes(_) => DataType::Bytes,  // new
```

Update `data_type_u8_roundtrip` test to include `Null` and `Bytes`.

### Layer 4 — `Value` (`src/gremlin/value.rs`)

Add `Bytes(Vec<u8>)` variant to `Value`. `From` impls:

```rust
impl From<Vec<u8>> for Value {
    fn from(b: Vec<u8>) -> Self { Value::Bytes(b) }
}
impl From<&[u8]> for Value {
    fn from(b: &[u8]) -> Self { Value::Bytes(b.to_vec()) }
}
```

`Vec<u8>` is not currently used for any `From<T> for Value` impl, so no ambiguity.
Callers write: `.property("embedding", vec![0x01u8, 0x02, 0x03])`.

Extend `PartialOrd`, `PartialEq` on `Value` for `(Bytes, Bytes)` — byte-lexicographic.
`is_integer()` and `is_numeric()` are **not** extended.

### Layer 5 — Type bridge (`src/gremlin/type_bridge.rs`)

Add to `value_to_primitive`:
```rust
Value::Bytes(b) => Some(Primitive::Bytes(b)),
```

Add to `primitive_to_value`:
```rust
Primitive::Bytes(b) => Value::Bytes(b),
```

No change to predicate validation — `Bytes` is a scalar so `value_to_primitive(...).is_none()`
returns `false`, and all existing predicate arms accept it.

## Files changed

| File | Change |
|---|---|
| `src/types/gvalue.rs` | Add `Bytes` variant; reorder `Null` to first; extend `PartialOrd`, `PartialEq`, `Hash`, `From` |
| `src/store/rocks/encoding.rs` | Renumber all TAGs; add `TAG_BYTES=9`; add encode/decode arms for `Null` (new TAG 0) and `Bytes` |
| `src/schema/definition.rs` | Renumber `DataType`; add `DataType::Null=0` and `DataType::Bytes=9`; fix `Null => DataType::Null`; extend `from_u8`, `from_primitive`; update test |
| `src/gremlin/value.rs` | Add `Bytes` variant; `From<Vec<u8>>`, `From<&[u8]>`; extend `PartialOrd`, `PartialEq` |
| `src/gremlin/type_bridge.rs` | `value_to_primitive`, `primitive_to_value` |

## Implementation plan

- [ ] **Step 1 — Renumber + Null reorder** (`gvalue.rs`, `encoding.rs`, `schema/definition.rs`):
  Change TAG values and DataType repr values; add `DataType::Null`; fix the `Null =>
  DataType::String` mapping. Verify `all_primitive_types_roundtrip` and
  `data_type_u8_roundtrip` still pass (after updating the numeric expectations in those tests).

- [ ] **Step 2 — Add `Bytes` variant** to `Primitive`, TAG, `DataType`, `Value`, type bridge.
  Verify `all_primitive_types_roundtrip` extended with Bytes round-trip.

- [ ] **Step 3 — E2E + `just full-check`**: end-to-end property write/read; predicate tests.

## Test plan

### Encoding round trips (`encoding.rs`)

Extend `all_primitive_types_roundtrip`:
- `Bytes(vec![])` — empty
- `Bytes(vec![0xFF; 100])` — non-empty
- `Bytes(vec![0x00; 65535])` — max length

Separately test that writing a `Bytes` value of length `>= 65536` is caught at the
write boundary (once the guard is added to `encode_props` or the property-set path).

Verify the TAG renumbering doesn't corrupt existing Primitive type round trips — run the
full `all_primitive_types_roundtrip` test and verify every variant decodes to the same
value it encoded.

### Schema round trips (`schema/definition.rs`)

Extend `data_type_u8_roundtrip` — add `DataType::Null` and `DataType::Bytes` to the `all`
array. Verify `DataType::Null.to_u8() == 0` and `DataType::Bytes.to_u8() == 9`.

Verify `DataType::from_primitive(Primitive::Null) == DataType::Null` (the old `String`
mapping is gone).

### Type bridge (`type_bridge.rs`)

Extend `test_value_to_primitive` and `test_primitive_to_value` with `Bytes` round trips.

### End-to-end (`gremlin/tests.rs`)

| Test | Coverage |
|---|---|
| `test_bytes_property_write_and_read` | Write `Bytes` as vertex/edge property; read back via `values(["blob"])`; assert `Value::Bytes` returned and byte-exact equality |
| `test_bytes_predicate_eq_ne` | Filter with `Eq`/`Ne` on `Bytes` value |
| `test_bytes_predicate_ordering` | Filter with `gt`/`lt` on two `Bytes` values; verify byte-lexicographic result |
| `test_schema_strict_rejects_wrong_type` | Declare key as `Bytes`; try writing `String`; expect `SchemaViolation` |

## Constraints / invariants

1. **No backward storage compatibility** — TAG and DataType values are renumbered freely.
   Any database written with the old encoding will not be readable after this change.
   This is acceptable for the current project stage.

2. **Bytes max 65 535 bytes** — enforced at property-write time (not silently truncated).
   Specifically: add a length check in the property-set path in `LogicalGraph` before the
   value reaches `encode_props`, returning `StoreError::SchemaViolation` if exceeded.

3. **Byte-lexicographic ordering** — `gt`/`lt`/`between` on `Bytes` work but are
   semantically meaningful only for ordered keys (e.g. binary-encoded sortable IDs).
   Document this in the builder API. For general blobs, callers should use `Eq`/`Ne`.

4. **`DataType::Null` for schema purposes** — properties declared as `Null` type are not
   useful (what would it mean to declare a key as "always null"?). The schema `declare_prop_key`
   should reject `DataType::Null` to prevent nonsensical schema declarations. `Primitive::Null`
   can still appear as a value in any typed property (representing "unset"), but no key may
   be *declared* as `DataType::Null`.

## Deferred: Timestamp and Date (Phase 2)

Both types are deferred because Rust's standard library lacks ergonomic representations:

| Type | Rust std option | Gap |
|---|---|---|
| Timestamp | `i64` ms since epoch via `SystemTime::duration_since(UNIX_EPOCH)` | No constructor literal; display shows raw integers; arithmetic awkward without `chrono`/`time` |
| Date | None | No built-in `Date` type at all; `i32` days-since-epoch has no standard anchor for construction; parsing ISO 8601 (`"2024-06-30"`) requires external crate |

Phase 2 should decide: adopt `chrono` as a dependency (adds ergonomic APIs, ISO 8601
parsing, formatting, arithmetic) or define thin newtype wrappers (`struct Timestamp(i64)`,
`struct Date(i32)`) with documented conventions and accept the ergonomic limitations.
