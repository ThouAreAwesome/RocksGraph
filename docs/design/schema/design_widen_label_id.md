# Design: `LabelId` — widen from `u16` to `i32`

## Problem

`LabelId` (`src/types/keys.rs`) is currently `u16`, with registration logic reserving
1 bit informally (max 32,768 distinct labels).  That ceiling is too low for workloads
with large numbers of distinct vertex/edge labels.  `Rank` stays `u16` — 65,536 parallel
edges between one vertex pair is not a realistic requirement.  Only `LabelId` widens.

There is no existing graph database to migrate — no on-disk data, no compatibility shims,
no dual encode/decode paths.  Every change is a direct replacement.

## Goals & non-goals

- **Goals:** Widen `LabelId` to `i32` (not `u32`); keep `Rank` at `u16`; keep in-memory
  struct sizes unchanged (no padding blowup); keep big-endian encoding sort-correct.
- **Non-goals:** Add new `Primitive`/`Value::UInt32` variant; touch `PropKey` id space
  or `Rank` encoding; provide backward-compatibility with old on-disk format.

## Design

### Why `i32`, not `u32`

- `Vertex`/`Edge`'s internal property accessors already represent the label as
  `Primitive::Int32(self.label_id as i32)` — this change makes that cast a no-op.
- Avoids adding a new `Primitive` variant, which would ripple through `type_bridge.rs`,
  `gvalue.rs`, and schema `DataType` validation.
- `i32` and `u32` are identical in size and alignment — no layout difference.
- Big-endian encoding of a **non-negative** `i32` sorts identically to the same bit
  pattern interpreted as `u32` (sign bit is 0 across the entire valid range).

### Memory layout — unaffected

Verified by direct measurement with `LabelId` widened and `Rank` left at `u16`:

| Type | Current (`u16`/`u16`) | `LabelId:i32`, `Rank:u16` | Δ |
|---|---|---|---|
| `VertexKey` | 8 | 8 | 0 |
| `CanonicalEdgeKey` | 24 | 24 | 0 |
| `EdgeKey` | 24 | 24 | 0 |
| `CanonicalKey` (enum) | 32 | 32 | 0 |
| `AdjacentEdgeCursor` | 16 | 16 | 0 |

The two `i64` fields in `CanonicalEdgeKey`/`EdgeKey` already force 8-byte alignment;
widening `label_id` from 2 to 4 bytes fills padding the compiler was already leaving
behind.  (This is *why* `Rank` must stay `u16` — widening both would push `EdgeKey`
to 32 bytes.)

### On-disk encoding — +2 bytes per LabelId occurrence

| Location | Current | New | Δ |
|---|---|---|---|
| `VertexValue.label_id` | 2B | 4B | +2B |
| `VertexDegree` | 10B total | 12B total | +2B |
| `CanonicalEdgeKey.label_id` | 20B total | 22B total | +2B |
| `EdgeValue.end_vertex_label` | 2B | 4B | +2B |
| Schema label entries | 2B | 4B | +2B |
| `vertex_id|label_id` scan prefix | 10B total | 12B total | +2B |

`Rank` is untouched everywhere it appears.

### `src/types/keys.rs`

- `pub type LabelId = u16;` → `pub type LabelId = i32;`.  Leave `Rank` untouched.
- Update doc comment: replace 15-bit reasoning with `1..=i32::MAX`, sign bit reserved,
  `0` reserved for "no such label".
- Update module-level summary table sizes for `CanonicalEdgeKey`/`EdgeKey`.

### `src/schema/definition.rs`

- `MAX_LABELS` → `i32::MAX as usize`.
- `register_vertex_label`/`register_edge_label`: `as u16 + 1` → `as LabelId + 1`.

### `src/store/rocks/encoding.rs`

- **`VertexValue`**: decode `bytes[0..2]` → `bytes[0..4]`; blob slice `bytes[2..]` → `bytes[4..]`.
- **`VertexDegree`**: `[u8; 10]` → `[u8; 12]`; all byte ranges shift by +2.
- **`EdgeValue`**: capacity hint `2 + ...` → `4 + ...`; decode `bytes[0..2]` → `bytes[0..4]`; blob `bytes[2..]` → `bytes[4..]`.
- **`CanonicalEdgeKey`** encode/decode: label byte range `[8..10]` → `[8..12]`; all subsequent fields shift by +2.
- **Schema label encode/decode**: `[u8; 2]` → `[u8; 4]`; signature from `u16` to `LabelId`.
- **`edge_scan_prefix`**: auto-widens with type; update doc comment and prefix-length table entry `10` → `12`.

### `src/engine/volcano/builder/build_step.rs` — sentinel migration

Replace all 7 `u16::MAX` label sentinels with `UNRESOLVED_LABEL_ID` (from `has_label`).
Do **not** touch unrelated `u16::MAX` sites for property-key id resolution.

### `src/types/element.rs`

Drop 4 now-unnecessary `as i32` casts: `Primitive::Int32(self.label_id)`.

### Test fixtures

Let the compiler find remaining `u16`-typed label literals; fix each as reported.

## Constraints / invariants

- `LabelId` range: `1..=i32::MAX` for real labels, `0` reserved for "no such label",
  negative values reserved as sentinels.
- Big-endian encoding of non-negative `i32` sorts identically to `u32` — no
  sign-flip trick needed.
- `Rank` must not widen: doing so would push `EdgeKey` from 24 to 32 bytes.
- `PropKey` id space (`u16`) is not affected by this change.

## Files changed

| File | Change |
|---|---|
| `src/types/keys.rs` | `LabelId` alias → `i32`; doc comments/table |
| `src/schema/definition.rs` | `MAX_LABELS`; register cast width |
| `src/store/rocks/encoding.rs` | All encode/decode byte ranges; schema label sigs; scan prefix |
| `src/engine/volcano/builder/build_step.rs` | 7× `u16::MAX` → `UNRESOLVED_LABEL_ID` |
| `src/types/element.rs` | Drop 4× `as i32` casts |
| Test files | Update `u16`-typed label fixtures → `LabelId` |

## Test plan

1. **Schema registration beyond old ceiling.** Register 40,000 labels; assert sequential
   ids and no premature `SchemaExhausted`.
2. **Round-trip encode/decode with `label_id > 65535`.** For each encode/decode pair,
   encode with `label_id = 100_000`, decode, assert equality.
3. **Existing encoding tests still pass** with updated fixture types.
4. **Sentinel migration regression.** Query `outE("nonexistent")` in `SchemaMode::Auto`;
   assert zero results — `UNRESOLVED_LABEL_ID` never accidentally matches a real edge.
5. **`Int32` exposure still correct.** `get_value(LABEL_KEY_ID)` on a vertex with
   `label_id > 65535` returns correct `Primitive::Int32`, no truncation.
6. **In-memory size assertions.** `assert_eq!(size_of::<CanonicalEdgeKey>(), 24)`,
   `assert_eq!(size_of::<EdgeKey>(), 24)` — catches accidental `Rank` widening.
7. **Full regression.** `cargo test --lib` and `just full-check` clean.
