# Design: widen `LabelId` from `u16` to `i32`

Status: proposal. Not implemented.

## Problem

`LabelId` (`src/types/keys.rs`) is currently `u16`, with the registration logic
reserving 1 bit informally (the doc comment says "15 bits used semantically,
max 32,768 distinct labels, high bit reserved for a possible future tag").
That ceiling is too low for workloads that declare large numbers of distinct
vertex/edge labels. `Rank` (also `u16`, disambiguates parallel edges between
the same `(src, label, dst)` triple) does not need the same headroom — 65,536
parallel edges between one vertex pair is not a realistic requirement — so
`Rank` stays exactly as it is. Only `LabelId` widens.

There is no existing graph database to migrate — no on-disk data, no
compatibility shims, no dual encode/decode paths for an "old format." Every
change below is a direct replacement.

## Approach

`LabelId` becomes `i32`, not `u32`, restricted by convention to the range
`1..=i32::MAX` (`0` stays reserved for "no such label," matching the existing
convention in `register_vertex_label`/`register_edge_label`; negative values
are never assigned by the schema and continue to be used internally as
"doesn't exist" sentinels — see the `UNRESOLVED_LABEL_ID` section below).

**Why `i32` and not `u32`:**

- `Vertex`/`Edge`'s internal property accessors already represent the label
  as `Primitive::Int32(self.label_id as i32)` (`src/types/element.rs`, 4 call
  sites). `Int32` is already the chosen representation for labels at the
  value layer — this change makes that cast a no-op instead of introducing a
  bit-reinterpretation hazard (a `u32` value ≥ 2^31 would silently go
  negative through `as i32`).
- It avoids adding a new `Primitive`/`Value::UInt32` variant, which would
  ripple through `type_bridge.rs`'s bidirectional conversions, `gvalue.rs`'s
  hand-written `PartialEq`/`Hash`/`Ord` impls, and schema `DataType`
  validation — all for a type that already has a home in `Int32`.
- `i32` and `u32` are identical in size and alignment — no memory-layout
  difference either way.
- Big-endian encoding of a **non-negative** `i32` sorts identically to the
  same bit pattern interpreted as `u32` (the sign bit is `0` across the
  entire valid range). No sign-flip trick is needed, unlike `VertexKey`
  (`edge_scan_prefix` XORs the sign bit of the full-range signed `i64` for
  exactly this reason) — `LabelId` never reaches its negative half by
  construction.

**Memory layout is unaffected.** Verified by direct measurement
(`std::mem::size_of`) with `LabelId` widened and `Rank` left at `u16`:

| Type | Current (`u16`/`u16`) | `LabelId:i32`, `Rank:u16` | Δ |
|---|---|---|---|
| `VertexKey` | 8 | 8 | 0 |
| `CanonicalEdgeKey` | 24 | 24 | 0 |
| `EdgeKey` | 24 | 24 | 0 |
| `CanonicalKey` (enum) | 32 | 32 | 0 |
| `AdjacentEdgeCursor` | 16 | 16 | 0 |

The two `i64` fields in `CanonicalEdgeKey`/`EdgeKey` already force 8-byte
alignment; widening `label_id` from 2 to 4 bytes just fills padding the
compiler was already leaving behind. (This is *why* `Rank` must stay `u16` —
widening both would push `EdgeKey` to 32 bytes, confirmed separately; this
document only widens `LabelId`.)

**On-disk encoding grows by a fixed, predictable amount.** Every occurrence
of `LabelId` in a key or value gains exactly 2 bytes (disk space was
explicitly stated as not a concern):

| Location | Current | New | Δ |
|---|---|---|---|
| `vertices` value prefix (`VertexValue.label_id`) | 2B | 4B | +2B |
| `vertex_degree` value (`VertexDegree`) | 10B total | 12B total | +2B |
| `edges_out`/`edges_in` **key** (`CanonicalEdgeKey.label_id`) | 20B total | 22B total | +2B |
| `edges_out`/`edges_in` value prefix (`EdgeValue.end_vertex_label`) | 2B | 4B | +2B |
| `schema` CF, vertex/edge label entries | 2B | 4B | +2B |
| `vertex_id\|label_id` scan prefix | 10B total | 12B total | +2B |

`Rank` is untouched everywhere it appears (`CanonicalEdgeKey.rank`,
`EdgeKey.rank`, `AdjacentEdgeCursor.rank`, the `edges_out`/`edges_in` key
suffix, `Primitive::UInt16` at the value layer).

## Changes by file

### `src/types/keys.rs`

- `pub type LabelId = u16;` → `pub type LabelId = i32;`. Leave
  `pub type Rank = u16;` untouched.
- Update the doc comment above `LabelId`: replace "15 bits are used
  semantically (max 32 768 distinct labels); stored as u16, with the high
  bit reserved for a possible future tag" with the new range (`1..=i32::MAX`,
  sign bit reserved, `0` reserved for "no such label").
- Update the module-level summary table sizes for `CanonicalEdgeKey`/`EdgeKey`
  (the table currently lists raw content widths, e.g. `20 B`/`22 B`; bump
  each by 2 wherever a `LabelId` contributes, consistent with the table's
  existing convention — these are documentation only, not derived from
  `size_of`, so keep them internally consistent with whatever convention is
  already there rather than switching to padded `size_of` values mid-table).

### `src/schema/definition.rs`

- `MAX_LABELS: usize = (1 << 15) - 1` → a value appropriate for the new
  range, e.g. `i32::MAX as usize`. Update the doc comment above it (currently
  references the 15-bit reasoning).
- `register_vertex_label`/`register_edge_label`: `self.vertex_labels.len() as
  u16 + 1` → `self.vertex_labels.len() as LabelId + 1` (same for
  `edge_labels`). The `>= MAX_LABELS` exhaustion check and the "ids start at
  1, 0 reserved" comment stay as-is — just the integer width changes.
- `vertex_labels: BiHashMap<LabelId, SmolStr>` / `edge_labels:
  BiHashMap<LabelId, SmolStr>` — no code change needed, both are already
  generic over the `LabelId` alias.

### `src/store/rocks/encoding.rs`

This is where most of the mechanical byte-layout work is. Update the
module-level doc table (lines documenting `vertices`/`vertex_degree`/
`edges_out`/`edges_in` key/value layouts and the prefix-scan-length table) to
reflect every width below.

**`VertexValue`** (`[ label_id:LabelId | property_blob ]`):
- `encode()`: `buf.extend_from_slice(&self.label_id.to_be_bytes())` — no
  source change needed once the field type changes (`to_be_bytes()` on `i32`
  naturally produces 4 bytes); update the doc comment ("2-byte" → "4-byte").
- `decode()`: `u16::from_be_bytes(bytes[0..2]...)` → `LabelId::from_be_bytes(bytes[0..4]...)`;
  blob slice shifts from `bytes[2..]` to `bytes[4..]`.

**`VertexDegree`** (`[ vertex_label_id:LabelId | out_e_cnt:u32 | in_e_cnt:u32 ]`):
- `encode()` returns `[u8; 10]` → `[u8; 12]`. `buf[0..2]` (label) →
  `buf[0..4]`; `buf[2..6]` (`out_e_cnt`) → `buf[4..8]`; `buf[6..10]`
  (`in_e_cnt`) → `buf[8..12]`.
- `decode()`: `bytes.len() != 10` → `!= 12`. `bytes[0..2]` → `bytes[0..4]`
  (`LabelId::from_be_bytes`); `out_e_cnt` reads from `bytes[4..8]`;
  `in_e_cnt` reads from `bytes[8..12]`.

**`EdgeValue`** (`[ end_vertex_label:LabelId | property_blob ]`):
- `encode()`: capacity hint `2 + self.property_blob.len()` → `4 + ...`;
  `to_be_bytes()` naturally widens once the field type changes. Update the
  doc comment ("2-byte big-endian `u16`" → "4-byte big-endian `i32`").
- `decode()`: `bytes.len() < 2` → `< 4`; `u16::from_be_bytes(bytes[0..2]...)`
  → `LabelId::from_be_bytes(bytes[0..4]...)`; blob slice `bytes[2..]` →
  `bytes[4..]`.

**`CanonicalEdgeKey` on-disk key encode/decode** (the function building the
`edges_out`/`edges_in` CF key, around the `buf[8..10].copy_from_slice(&k.label_id.to_be_bytes())`
site): label byte range `[8..10]` → `[8..12]`; every subsequent field
(`DstId`/`SrcId`, `Rank`) shifts its byte range by +2. Same shift on the
matching decode function (currently reads `label_id` from `bytes[8..10]` via
`u16::from_be_bytes`, then `LabelId::from_be_bytes(bytes[8..12]...)`, etc.).

**Schema label value encode/decode:**
- `encode_schema_label_value(id: u16) -> [u8; 2]` → `encode_schema_label_value(id: LabelId) -> [u8; 4]`.
- `decode_schema_label_value(bytes: &[u8]) -> Option<u16>` → `Option<LabelId>`,
  reading all 4 bytes instead of 2.
- Leave `encode_schema_prop_value`/`decode_schema_prop_value` untouched —
  property-key ids are a separate `u16` id space, not `LabelId`.
- Call sites in `src/schema/management.rs` (2 sites) and
  `src/graph/logical.rs` (2 sites) already pass/receive `LabelId`-typed
  values (from `declare_vertex_label`/`vertex_label_str`/etc.) — no change
  needed there beyond the signature change propagating.
- Call sites in `src/store/rocks/store.rs` (2 sites, schema warmup/load)
  receive `Option<LabelId>` instead of `Option<u16>` — again no value-level
  change needed, just confirm nothing downstream re-narrows to `u16`.

**`edge_scan_prefix`** (`src/store/rocks/encoding.rs`, builds the
`vertex_id|label_id` scan prefix): `prefix.extend_from_slice(&lbl.to_be_bytes())`
needs no source change (widens automatically with the type), but update the
doc comment ("`label_id` (2 B)" → "(4 B)") and the prefix-length table entry
(`10` → `12`).

### `src/engine/volcano/builder/build_step.rs` — sentinel migration

Two different "label doesn't exist" sentinel conventions currently coexist:

1. `steps::has_label::UNRESOLVED_LABEL_ID: i32 = -1` (already `i32`, used by
   the `HasLabel` step's predicate resolution).
2. `u16::MAX` literals (7 occurrences, all in label-resolving closures for
   `Both`/`BothE`/`OutE`/`InE`/`BothE` variants —
   `.map(|id_opt| id_opt.unwrap_or(u16::MAX))` — used as an "impossible,
   never-matches-a-real-edge" placeholder when `resolve_read_edge_label`
   returns `None` in `SchemaMode::Auto`).

`u16::MAX` (65535) only worked as a sentinel because real ids topped out at
32767 — once ids can reach `i32::MAX`, it stops being a safe "impossible"
value. Replace all 7 occurrences of `u16::MAX` in this file with
`steps::has_label::UNRESOLVED_LABEL_ID` (or relocate that constant somewhere
more shared if `has_label` shouldn't be a dependency of the `Both`/`BothE`
branches — either way, unify on one negative sentinel, not two
inconsistent ones).

Do **not** touch the unrelated `u16::MAX` sites in this file at lines ~372
and ~395 (`SchemaMode::Auto => Ok((k.clone(), u16::MAX))`) — those are
property-key id resolution (`schema.prop_key_id`), a separate `u16` id space
that this change does not touch. Do not touch `src/planner/optimizer/mod.rs`'s
`primitive_to_rank` (bounds-checks `Rank`, not `LabelId`) either.

### `src/types/element.rs`

Four call sites currently do `Primitive::Int32(self.label_id as i32)`
(`Vertex::get_property`/`get_value`, `Edge::get_property`/`get_value`). Once
`label_id` is itself `i32`, drop the cast: `Primitive::Int32(self.label_id)`.
Not required for correctness (the cast becomes a no-op), but leaving it in
would be a stale signal that label_id is some other width — clean it up.

### Test fixtures and helpers

Several places type a `label_id` parameter explicitly as `u16` (e.g.
`encoding.rs`'s test helper `fn make_vertex(id: i64, label_id: u16, ...)`).
Let the compiler find these — after making the changes above, `cargo build
--lib` and `cargo test --lib --no-run` will fail at every remaining `u16`
literal/type-annotation that assumed `LabelId`'s old width; fix each as
reported rather than trying to pre-enumerate them all here.

## Files changed

| File | Change |
|---|---|
| `src/types/keys.rs` | `LabelId` alias → `i32`; doc comments/table |
| `src/schema/definition.rs` | `MAX_LABELS`; `register_vertex_label`/`register_edge_label` cast width |
| `src/store/rocks/encoding.rs` | Doc table; `VertexValue`/`VertexDegree`/`EdgeValue` encode/decode byte ranges; `CanonicalEdgeKey` key encode/decode byte ranges; `encode_schema_label_value`/`decode_schema_label_value` signatures; `edge_scan_prefix` doc comment |
| `src/engine/volcano/builder/build_step.rs` | Replace 7× `u16::MAX` label sentinels with `UNRESOLVED_LABEL_ID` |
| `src/types/element.rs` | Drop 4× now-unnecessary `as i32` casts |
| Test files (`encoding.rs` tests, others found via compiler errors) | Update `u16`-typed label fixtures to `LabelId` |

## Test plan

1. **Schema registration beyond the old ceiling.** Register more than 32,768
   vertex labels (and edge labels) in one `Schema`; assert ids are assigned
   sequentially starting at 1, no `SchemaExhausted` error until truly
   exhausted, and the last assigned id is representable (e.g. register
   40,000 labels, assert the 40,000th has id `40000`).
2. **Round-trip encode/decode at widths `u16` could never represent.** For
   each of `VertexValue`, `VertexDegree`, `EdgeValue`,
   `encode_schema_label_value`/`decode_schema_label_value`, and the
   `CanonicalEdgeKey`/`EdgeKey` on-disk key functions: encode a value with
   `label_id` > 65535 (e.g. `100_000`), decode it back, assert equality.
   This is the test that actually proves the new width works end-to-end —
   simply compiling is not sufficient evidence, since `u16`-range values
   would round-trip correctly even with a leftover bug in the new byte
   ranges.
3. **Existing encoding round-trip tests still pass** with updated fixture
   types (the existing tests around `encoding.rs` lines 683–890 using small
   literal label ids like `7`, `1`, `5`).
4. **Sentinel migration regression.** In `SchemaMode::Auto`, query
   `outE("nonexistent_label")`/`bothE("nonexistent_label")` (and the other
   five `Both`/`BothE`/`OutE`/`InE` variants touched in `build_step.rs`)
   against a graph containing real edges; assert zero results — i.e. the new
   `UNRESOLVED_LABEL_ID` sentinel still never accidentally matches a real
   edge's label, the same property `u16::MAX` used to guarantee.
5. **`Int32` exposure still correct.** `.values([Key::Label])`-style or
   direct `get_value(LABEL_KEY_ID)` access on a vertex/edge with a label id
   > 65535 returns `Primitive::Int32` with the correct value (no truncation,
   no sign flip) — this is the scenario the `i32`-vs-`u32` choice was
   specifically meant to make safe.
6. **In-memory size assertions** (optional but cheap, given this design
   leans on a specific measured fact): a `static_assertions`-style
   `assert_eq!(std::mem::size_of::<CanonicalEdgeKey>(), 24)` /
   `assert_eq!(std::mem::size_of::<EdgeKey>(), 24)` test, so a future
   accidental widening of `Rank` (which *would* grow `EdgeKey` to 32 bytes,
   per the measurement in this doc) fails loudly instead of silently.
7. **Full regression.** `cargo test --lib` and `just full-check` clean.
