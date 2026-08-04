# Design: Selective Property Encoding

## Status: Implemented (2026-07-02)

---

## Overview

The current property encoding decodes **all** properties into a `Vec<Property>` on first access,
even when only one property is needed.  A `has("age", gt(30))` on a vertex with 8 properties
unnecessarily parses 7 other values and allocates a `Vec`.  This design replaces the decode-everything
approach with a **sorted offset-index blob** that supports O(log n) single-key lookup, and a
**two-state overlay** that stays in the blob until the first mutation transitions it to a mutable map.

---

## Current Encoding (v1) — The Problem

```
prop_blob (v2) = [ count:u16 BE | (key:u16 BE | tag:u8 | payload:var)* ]
```

| Problem | Impact |
|---|---|
| No skip-ahead information | Must parse entry N to find where entry N+1 starts |
| `get_value(key)` triggers `ensure_decoded()` | All P properties decoded, `Vec<Property>` allocated |
| `has("age", gt(30))` on rejected element | Full decode wasted — 99% of work discarded at 1% selectivity |
| Keys not sorted | Cannot early-exit or binary-search |

---

## Implemented Encoding (v2) — Offset-Index Blob

```
                    ┌───── Directory (sorted by key) ──────┐  ┌── Value section ──┐
prop_blob (v2) =    [ ver:4 | cnt:12 | (key:u16 | off:u16)* | (tag:u8 | payload)* ]
                     ─────── 2 + count×4 bytes ───────────    ─── value section ───
                     ─ u16 BE header ─
                              ↑                                    ↑
                       binary search here                   decode here
```

### Layout detail

```
Byte  0-1:   version:4 | count:12       (u16 BE header)
             ─ bits 15–12 ─ ─ bits 11–0
             version = 0x2, count = up to 4095

Byte  2-5:   key₀  | off₀    (u16+u16) — directory entry 0  ← smallest key
Byte  6-9:   key₁  | off₁    (u16+u16) — directory entry 1
      ...
            ┌── value section (at byte 2 + count×4) ────────┐
            │  tag₀  | payload₀                             │ ← off₀ = 0
            │  tag₁  | payload₁                             │ ← off₁ = len(payload₀)+1
            │  ...                                          │
            └───────────────────────────────────────────────┘
```

- `version`: 4 bits — identifies the encoding format (current = 0x1)
- `count`: 12 bits — up to 4095 properties per element
- `key`: property key ID (u16 BE), same as schema CF
- `off`: byte offset from the start of the value section to this entry's `tag` byte (u16 BE)
- `tag`: data type identifier (unchanged from v1)
- Keys in the directory are **sorted ascending** — enables binary search
- Value section is written in the same sorted order — enables sequential scan for full decode

### Data type tags (unchanged from v1)

| Tag | Type    | Payload       |
|-----|---------|--------------:|
| 0   | NULL    | 0 B           |
| 1   | BOOL    | 1 B           |
| 2   | INT32   | 4 B           |
| 3   | INT64   | 8 B           |
| 4   | FLOAT32 | 4 B           |
| 5   | FLOAT64 | 8 B           |
| 6   | STRING  | u16 len + N B |
| 7   | UUID    | 16 B          |
| 8   | UINT16  | 2 B           |
| 9   | BYTES   | u16 len + N B |

### Storage overhead vs v1

| P  | Avg payload | before | after | extra |
|----|-------------|-------|-------|------:|
|  1 | 8 B (i64)   | 13 B  | 15 B  | +2 B  |
|  5 | 6 B         | 47 B  | 57 B  | +10 B |
| 10 | 6 B         | 92 B  | 112 B | +20 B |
| 20 | 8 B         | 222 B | 262 B | +40 B |

The overhead is **2 bytes per property** (the `off` field).  Both v1 and v2 format use a 2-byte header
(u16 count vs ver:4|cnt:12).  At 10 properties per element this is 20 extra bytes — negligible vs
the I/O cost of a RocksDB read.

---

## Labels: Fixed Prefix, Not Properties

`label_id` (vertex) and `end_vertex_label` (edge) are stored as a **fixed 4-byte prefix before**
the prop_blob — never as entries inside it.

```
VertexValue  =  [ label_id: i32 (4 B) | prop_blob ]
EdgeValue    =  [ end_vertex_label: i32 (4 B) | prop_blob ]
                 ──────── structural ────────   ── user props ──
```

### Rationale

1. **Immutability.** A vertex's label never changes.  User properties participate in the
   read-modify-write overlay cycle; label does not.

2. **Access frequency.** Label is the most-queried attribute:
   - `hasLabel("person")`, `label()`, vertex-label optimizer, end-vertex label filter —
     all need label, none need user props.
   - Making label require a prop_blob binary search would be a regression.

3. **Edge scan freebie.** During `outE()` iteration, the first 4 bytes of each edge value
   are the destination vertex's `label_id`.  This allows `hasLabel()`-on-the-other-vertex
   without loading the destination vertex record.  If `end_vertex_label` were folded into
   the prop_blob, this optimization would require decoding the blob on every edge.

4. **Reserved key synthesis.** `ID_KEY_ID` and `LABEL_KEY_ID` are synthesized at access time
   from struct fields (`self.id`, `self.label_id`) — they never touch the prop_blob.

---

## Two-State Property Storage in the Overlay

The blob is the canonical on-disk representation.  In the overlay (LogicalGraph), properties
live in one of two states:

```
                         ┌─────────────────────┐
    load from store      │                     │
    ────────────────────►│ PropertyMap::Blob   │
                         │                     │
                         │   read:  O(log n)   │
                         │   alloc:  zero      │
                         │   mutate: decode →  │────── first write ─────┐
                         └─────────────────────┘                        │
                                                                        ▼
                                                             ┌─────────────────────┐
                                                             │  PropertyMap::Map   │
                                                             │                     │
                                                             │  read:  O(1) hash   │
                                                             │  write: O(1) insert │
                                                             │  alloc: one-time    │
                                                             └─────────────────────┘
                                                                        │
                                                              commit    │
                                                             ┌──────────▼──────────┐
                                                             │  encode map → blob  │
                                                             │  write to RocksDB   │
                                                             └─────────────────────┘
```

### `PropertyMap` — the two-state enum

```rust
enum PropertyMap {
    /// No mutations yet.  All reads go through binary search on the
    /// sorted offset-index blob.  Zero allocation.
    Blob(PropertyBlob),

    /// At least one mutation has occurred.  All reads and writes go
    /// through the HashMap.  O(1) for both.
    Map(HashMap<u16, Primitive>),
}
```

### `PropertyBlob` — the encoded bytes

```rust
struct PropertyBlob(Box<[u8]>);

impl PropertyBlob {
    fn get_value(&self, key: u16) -> Option<Primitive>;
    fn to_map(&self) -> HashMap<u16, Primitive>;  // transition point
}
```

### Initial state

Elements loaded from the store always start in `Blob` state — the raw bytes are kept as-is
until the first mutation.  Elements created in-memory (`addV`, `addE`) have no raw bytes and
always start in `Map(HashMap::new())` state directly.

### Why `HashMap` not `Vec<Property>` for the Map state?

- `Vec`: `upsert_prop()` scans O(n) to find existing key; `set("age", 31)` on an element
  with 20 properties does 20 string-key comparisons.
- `HashMap`: O(1) insert, O(1) get.  The commit-time re-encode cost is the same either way.

The `Property.owner` field is dropped entirely — the owner is implicit from the `Vertex` or
`Edge` that wraps the `PropertyMap`.

---

## Vertex / Edge Structs (simplified)

```rust
pub struct Vertex {
    pub id: VertexKey,
    pub label_id: LabelId,
    pub props: PropertyMap,   // ← replaces raw_props + props: Vec<Property>
}

pub struct Edge {
    pub src_id: VertexKey,
    pub label_id: LabelId,
    pub dst_id: VertexKey,
    pub rank: Rank,
    pub src_label: Option<LabelId>,
    pub dst_label: Option<LabelId>,
    pub props: PropertyMap,   // ← same
}
```

No `ensure_decoded()`.  No `raw_props`.  No `DecoderFn` function pointer.  The state
transition is explicit: `Blob::to_map()` on first mutation.

---

## Access Pattern Cost

| Pattern | Type | before | after |
|---|---|---|---|
| Label only | P1 | O(1) — value prefix bytes | O(1) — unchanged |
| Single key (`get_value`) | P2 | O(P) decode + Vec alloc | **O(log P)**, 0 alloc |
| K keys (`values([k1,k2])`) | P3 | O(P) decode + K×O(P) scan | **O(K log P)**, or O(P) merge-scan |
| All properties | P4 | O(P) decode + Vec alloc | O(P) decode + HashMap alloc |
| Key only (id/rank) | P5 | O(1) — RocksDB key | O(1) — unchanged |
| Filter + reject | P6 | O(P) per reject | **O(log P)** per reject |
| Write (encode) | P7 | O(P) | O(P log P) — sort once |
| Mutate one prop | — | O(P) decode + O(n) Vec scan | O(P) first time (Blob→Map), O(1) thereafter |

The biggest practical win is **P6**: at 1% selectivity, 99% of elements pay O(log P) instead
of O(P).

### Binary search detail (P2)

```
header = u16 BE at blob[0..2]
version = header >> 12     // bits 15-12
count   = header & 0xFFF   // bits 11-0
lo, hi = 0, count
while lo < hi:
    mid     = (lo + hi) / 2
    dir_key = u16 at blob[2 + mid*4]
    if   dir_key == K:  decode value at blob[value_start + offset]
    elif dir_key <  K:  lo = mid + 1
    else:               hi = mid
→ None  (key not present)
```

- `value_start = 2 + count × 4`
- Comparisons: ⌈log₂ count⌉ — for count=8: 3 steps, count=32: 5 steps
- Cache: directory is count×4 bytes; for count≤15 it fits in one 64-byte cache line

---

## Encode / Decode Sketch

### Encoding (write path)

```rust
fn encode_props(props: &[(u16, Primitive)]) -> Vec<u8> {
    let mut sorted: SmallVec<[&(u16, Primitive); 16]> = props.iter().collect();
    sorted.sort_unstable_by_key(|(k, _)| *k);

    // compute offsets
    let mut offsets = Vec::with_capacity(sorted.len());
    let mut voff: u16 = 0;
    for (_, v) in &sorted {
        offsets.push(voff);
        voff += 1 + payload_size(v) as u16;
    }

    // emit: header (u16 BE with version and count) + directory + value_section
    let header: u16 = (VERSION << 12) | (sorted.len() as u16 & 0xFFF);
    let mut buf = Vec::with_capacity(2 + sorted.len() * 4 + voff as usize);
    buf.extend_from_slice(&header.to_be_bytes());
    for ((k, _), off) in sorted.iter().zip(&offsets) {
        buf.extend_from_slice(&k.to_be_bytes());
        buf.extend_from_slice(&off.to_be_bytes());
    }
    for (_, v) in &sorted {
        write_tag_and_payload(&mut buf, v);
    }
    buf
}
```

### Single-key decode (read path P2/P6)

```rust
fn decode_prop_by_key(blob: &[u8], target_key: u16) -> Option<Primitive> {
    if blob.len() < 2 { return None; }
    let header = u16::from_be_bytes(blob[0..2].try_into().ok()?);
    let count = (header & 0xFFF) as usize;
    let value_start = 2 + count * 4;
    let (mut lo, mut hi) = (0, count);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let base = 2 + mid * 4;
        let k = u16::from_be_bytes(blob[base..base+2].try_into().ok()?);
        match k.cmp(&target_key) {
            Ordering::Equal => {
                let off = u16::from_be_bytes(blob[base+2..base+4].try_into().ok()?) as usize;
                return decode_single_value(blob, value_start + off);
            }
            Ordering::Less    => lo = mid + 1,
            Ordering::Greater => hi = mid,
        }
    }
    None
}
```

### Full decode (P4 / Blob→Map transition)

```rust
fn decode_all_to_map(blob: &[u8]) -> Option<HashMap<u16, Primitive>> {
    if blob.len() < 2 { return None; }
    let header = u16::from_be_bytes(blob[0..2].try_into().ok()?);
    let count = (header & 0xFFF) as usize;
    let value_start = 2 + count * 4;
    let mut map = HashMap::with_capacity(count);
    for i in 0..count {
        let base = 2 + i * 4;
        let key = u16::from_be_bytes(blob[base..base+2].try_into().ok()?);
        let off = u16::from_be_bytes(blob[base+2..base+4].try_into().ok()?) as usize;
        let value = decode_single_value(blob, value_start + off)?;
        map.insert(key, value);
    }
    Some(map)
}
```

---

## LogicalGraph Overlay Integration

The overlay members stay the same but `Vertex.props` / `Edge.props` change type:

```rust
// Before
struct LogicalGraph<S> {
    vertices:  HashMap<VertexKey, Vertex>,    // each has raw_props + props: Vec<Property>
    edges:     HashMap<CanonicalEdgeKey, Edge>,
    dirty:     HashMap<CanonicalKey, Existence>,  // mutation tracker — unchanged
    ...
}

// After
struct LogicalGraph<S> {
    vertices:  HashMap<VertexKey, Vertex>,    // each has props: PropertyMap
    edges:     HashMap<CanonicalEdgeKey, Edge>,
    dirty:     HashMap<CanonicalKey, Existence>,  // unchanged
    ...
}
```

`dirty` still only tracks **existence state** (New/Modified/Tombstone), not property values.
Property values are resolved through `PropertyMap`:

| Operation | Blob state | Map state |
|---|---|---|
| `has("age", gt(30))` | `blob.get_value(key).map_or(false, \|v\| pred(v))` | `map.get(key).map_or(false, \|v\| pred(v))` |
| `get_value("name")` | `blob.get_value(key)` | `map.get(key).cloned()` |
| `set_property("age", 31)` | `blob.to_map()` → state transitions to Map, then `map.insert(key, val)` | `map.insert(key, val)` |
| `drop_property("age")` | `blob.to_map()` → transition, then `map.remove(key)` | `map.remove(key)` |
| Commit | n/a — Blob-state elements are never dirty | `encode_map_to_blob(map)` |

---

## Differences from the Previous Proposal

| Item | Previous doc | Refined design | Rationale |
|---|---|---|---|
| Overlay state | `raw_props` + `ensure_decoded()` → `Vec<Property>` | `PropertyMap::Blob` / `PropertyMap::Map` | Two explicit states, no `ensure_decoded()` trick, no function-pointer field |
| Mutation state | `Vec<Property>` | `HashMap<u16, Primitive>` | O(1) insert vs O(n) Vec scan; no `Property.owner` field needed |
| Full decode output | `Vec<Property>` | `HashMap<u16, Primitive>` | Direct mapping; `Property.owner` is implicit from parent |
| `Property` struct | `{ owner, key, value }` | Dropped from overlay; only used at public API boundary | Overlay doesn't need `owner`; it knows which element it belongs to |

---

## Migration Path

The 4-bit version field in the header enables format evolution:

| Version | Format | Status |
|---------|--------|--------|
| 0x2 | v2 (offset-index blob) | Shipped in v0.1.0 |

On read:
1. Read the u16 BE header, extract `version = header >> 12`, `count = header & 0xFFF`
2. `decode_prop_by_key` / `decode_all_to_map` reject blobs with `version != 0x2`
3. On write, always emit v2 format (version=0x2)

Since v0.1.0 is the first public release, v2 is the only format.  No v1 backward
compatibility is needed.  Future format versions (0x3+) can be introduced via the
version nibble with a converter path in `prop_codec.rs`.

---

## Reference: Complete On-Disk Row Layout

```
┌──────────────────────────────────────────────────────────────────────────┐
│                          VERTEX ROW                                       │
├────────────────────┬─────────────────────────────────────────────────────┤
│  KEY (8 B)         │  VALUE                                              │
│  vertex_id: i64    │  ┌──────────────┬──────────────────────────────────┐│
│                    │  │ label_id     │ prop_blob (v2)                            ││
│                    │  │ i32 (4 B)    │ [ver:4|cnt:12|directory|value_section]    ││
│                    │  └──────────────┴──────────────────────────────────┘│
└────────────────────┴─────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│                          EDGE ROW (edges_out)                             │
├──────────────────────────────────┬───────────────────────────────────────┤
│  KEY (22 B)                      │  VALUE                                │
│  src_id:i64|label_id:i32|        │  ┌──────────────────┬────────────────┐│
│  dst_id:i64|rank:u16             │  │ end_vertex_label │ prop_blob (v2) ││
│                                  │  │ i32 (4 B)        │                ││
│                                  │  └──────────────────┴────────────────┘│
└──────────────────────────────────┴───────────────────────────────────────┘
                (edges_in swaps src_id↔dst_id in the key)
```
