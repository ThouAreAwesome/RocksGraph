# Design: Vector WAL — Crash Consistency for Vector Indexes

Status: proposal — complements `design_vector_search.md` §9.

---

## Table of Contents

- [1. Problem](#1-problem)
- [2. Chosen approach: separate WAL CF per index type](#2-chosen-approach-separate-wal-cf-per-index-type)
- [3. Column family schema](#3-column-family-schema)
  - [`CF_VECTOR_WAL`](#cf_vector_wal)
  - [`vector_edge_labels` CF](#vector_edge_labels-cf)
  - [`__meta` CF](#__meta-cf)
- [4. Timestamp key generator](#4-timestamp-key-generator)
- [5. Write path](#5-write-path)
- [6. Recovery path (`Graph::open`)](#6-recovery-path-graphopen)
  - [Recovery scenarios](#recovery-scenarios)
- [7. First open: full rebuild vs WAL replay](#7-first-open-full-rebuild-vs-wal-replay)
- [8. Online index build: WAL catch-up for `add_vector_index`](#8-online-index-build-wal-catch-up-for-add_vector_index)
  - [The problem](#the-problem)
  - [The solution: WAL catch-up](#the-solution-wal-catch-up)
  - [Why writes do not need to be blocked](#why-writes-do-not-need-to-be-blocked)
  - [Edge cases](#edge-cases)
  - [Interaction with crash recovery](#interaction-with-crash-recovery)
- [9. HNSW snapshot and WAL trimming](#9-hnsw-snapshot-and-wal-trimming)
  - [Snapshot format](#snapshot-format)
  - [When to flush](#when-to-flush)
  - [WAL trimming](#wal-trimming)
- [10. Multi-property support](#10-multi-property-support)
- [11. Failure scenarios and guarantees](#11-failure-scenarios-and-guarantees)
  - [Guarantee](#guarantee)
  - [Scenario analysis](#scenario-analysis)
- [12. RocksDB CF options for `CF_VECTOR_WAL`](#12-rocksdb-cf-options-for-cf_vector_wal)
- [13. Implementation checklist (v0.2)](#13-implementation-checklist-v02)

---

## 1. Problem

RocksGraph's vector index (HNSW) lives in memory and is periodically flushed to
disk. The graph store (RocksDB) is durable on every commit. A crash between a
graph commit and the next HNSW flush leaves the two out of sync: the graph store
has the new vertex and its vector property, but the in-memory index — and the
on-disk snapshot — do not.

On the next `Graph::open`, the HNSW snapshot is stale. Without a recovery
mechanism, the graph and the vector index diverge permanently.

---

## 2. Chosen approach: separate WAL CF per index type

**Pattern: each index type gets its own dedicated RocksDB column family for WAL entries, written in the same `WriteBatch` as the graph mutation.**

For vector indexes, this CF is `CF_VECTOR_WAL` (RocksDB CF name: `"vector_wal"`). A future full-text search index would get a separate `CF_TEXT_WAL` (`"text_wal"`). All declared vector property indexes — regardless of how many are declared — share one `CF_VECTOR_WAL`, partitioned internally by `(prop_key_id, entity_type)` key prefix (see §3). The "per index type" boundary is between categories of index, not between individual declared indexes within the same category.

This gives one `fsync` covering both the graph data and the index mutation record. No secondary sync, no two-phase commit.

**Why separate CFs per index type, not a shared `CF_INDEX_WAL` with a discriminant byte:**

| Concern | Separate CF per type (chosen) | Shared `CF_INDEX_WAL` with discriminant |
|---------|-------------------------------|----------------------------------------|
| **Compaction tuning** | Each CF tuned for its data characteristics: vector entries are dense float blobs (~6 KB per 1536-dim insert); text entries are variable-length strings (10 B–100 KB). Different write buffer sizes, compression settings per CF. | One tuning policy for mixed entry sizes — inevitably wrong for at least one type. |
| **Recovery isolation** | Each index type iterates only its own CF during `Graph::open`. No cross-type entries skipped. | Recovery must demux a discriminant byte and skip foreign entries at every step. |
| **WAL trimming** | `delete_range_cf` on `CF_VECTOR_WAL` trims only vector entries; text entries are never touched. | Range tombstones span the same key space as other types; trimming becomes cross-type. |
| **Operational clarity** | `CF_VECTOR_WAL` can be inspected, monitored, or selectively backed up independently. | Interleaved entries make per-type inspection and selective backup harder. |

Alternatives that apply to **all** index WAL designs (vector, text, or any future type):

| Approach | Why rejected |
|----------|-------------|
| Separate append-only WAL file | Requires a second `fsync` per commit, or risks losing the WAL entry on crash; two durability paths to manage |
| Scan all committed graph mutations on recovery | Requires iterating every committed batch to find index-touching mutations; no efficient seek by seqno |
| Tight WAL coupling (Pattern A) | Forces index internals to fit RocksDB's page model; cannot use external index libraries unchanged |
| Accept stale index on crash | Permanent divergence; unacceptable for correctness |

---

## 3. Column family schema

### `CF_VECTOR_WAL`

```rust
pub const CF_VECTOR_WAL: &str = "vector_wal";
```

**Key** — 15-byte composite, big-endian. RocksDB's bytewise comparator gives
per-index chronological order, enabling efficient prefix-seeks during recovery:

```
key = [prop_key_id: u16 BE][entity_type: u8][ts: u64 BE][random: u32 BE]
       └─ 2 bytes ─┘        └─ 1 byte ─┘    └─ 8 bytes ─┘  └─ 4 bytes ─┘
```

- `prop_key_id` — schema-interned u16 for the property name (same registry used for `label_id` in `CanonicalEdgeKey`)
- `entity_type` — `0x00` = Vertex, `0x01` = Edge; disambiguates vertex "embedding" from edge "embedding" with the same `prop_key_id`
- `ts` — microsecond timestamp from the process-local `AtomicU64` clock (§4); monotonic within a process, no system call
- `random` — 4-byte random suffix; prevents key collision between two sessions that receive the same timestamp value

Entries for the same `(prop_key_id, entity_type)` sort together and in timestamp order, making recovery a targeted seek rather than a full-CF scan.

**Value** — variable-length binary. `entity_type` and `prop_key` are carried by
the key; the value contains only the operation payload:

```
[op_type:     u8     ]   // 0x00 = Insert, 0x01 = Delete
[entity_key:  ...    ]   // variable — see below
[vector_len:  u32 LE ]   // number of f32 elements; 0 for Delete ops
[vector_data: [f32 LE]]  // vector_len × 4 bytes; absent for Delete ops
```

**Entity key encoding** by `entity_type`:

```
// entity_type = 0x00 (Vertex)
[vertex_id: i64 LE]                                      // 8 bytes

// entity_type = 0x01 (Edge) — mirrors CanonicalEdgeKey wire layout (22 bytes fixed)
[src_id: i64 LE][label_id: i32 LE][dst_id: i64 LE][rank: u16 LE]
```

Edge label strings are interned to `label_id: i32` at write time via the schema
registry. The WAL stores the integer, not the string — fixed 22 bytes for any edge,
regardless of label length.

Total size for a 1536-dim vertex Insert (key + value): `15 + 1 + 8 + 4 + 6144 = 6172 bytes`.
Total size for a 1536-dim edge Insert: `15 + 1 + 22 + 4 + 6144 = 6186 bytes`.

### `vector_edge_labels` CF

HNSW implementations (usearch) require a monotonic `u64` node label per entry.
`EntityKey::Vertex(id)` maps bijectively to `u64` via a direct bit-cast
(`i64 as u64`), so no storage is needed. `EntityKey::Edge(CanonicalEdgeKey)` has
no natural u64 form — the mapping is stored here:

```
key   = u64 BE   // monotonic label counter (next_edge_label)
value = CanonicalEdgeKey binary (22 bytes, same layout as WAL entity_key encoding)
```

At insert time, `next_edge_label` is incremented and the new label → CEK pair is
written in the same `WriteBatch` as the `CF_VECTOR_WAL` entry and graph mutation —
one `fsync` covers all three. At search time, usearch returns `u64` labels; the
engine looks up each label here to recover the `CanonicalEdgeKey`. On `Graph::open`,
the CF is iterated once to rebuild the in-memory reverse map.

See `design_hnsw_impl.md` §4 for the full label-mapping design and
`design_ann_algorithm_and_library.md` §4a for the rationale (in-memory HashMap
was rejected in favour of this persistent CF).

### `__meta` CF

Stores the WAL clock high watermark and other graph-level metadata:

```
key   = b"vector_wal_clock_hwm"
value = u64 LE              // max timestamp issued by this process — persisted on every snapshot flush
```

On `Graph::open`, the clock is seeded as `max(SystemTime_micros, stored_hwm)`.
This ensures monotonicity across process restarts and survives NTP clock skew:
if the system clock moves backward, the stored high watermark is used instead.

---

## 4. Timestamp key generator

**Decision: replace the `Arc<Mutex<u64>>` counter with a process-local `AtomicU64` clock.**

The WAL key must be assigned before the `WriteBatch` is submitted so it can be included in the batch. The new scheme uses a monotonically increasing `AtomicU64` seeded from wall clock time, advanced by `fetch_add`:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

static WAL_CLOCK: AtomicU64 = AtomicU64::new(0);

/// Called once during `Graph::open`. Seeds the clock from
/// `max(wall_clock_micros, stored_high_watermark)`.
fn init_wal_clock(db: &DB) {
    let wall_us = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;

    let stored_hwm = db.get_cf(&db.cf_handle("__meta").unwrap(), b"vector_wal_clock_hwm")
        .ok()
        .flatten()
        .map(|v| u64::from_le_bytes(v[..8].try_into().unwrap()))
        .unwrap_or(0);

    WAL_CLOCK.store(wall_us.max(stored_hwm), Ordering::Release);
}

/// Returns the next WAL timestamp. ~5 ns per call. No lock.
fn vector_wal_timestamp() -> u64 {
    WAL_CLOCK.fetch_add(1, Ordering::AcqRel)
}
```

**Seeding from `max(wall_clock, stored_hwm)`** is required for two safety properties:
1. **NTP regression safety**: if the system clock moves backward between restarts (NTP adjustment), the stored high watermark prevents issuing timestamps that overlap with entries already in the WAL CF from the previous run.
2. **Fast-restart safety**: if a process crashes and immediately restarts (within microseconds), the new process would otherwise seed from a wall clock value below the previous session's maximum timestamp — causing WAL entries from the crash to appear "already replayed" during recovery.

**High watermark persistence**: on every snapshot flush, `WAL_CLOCK.load(Ordering::Acquire)` is written to `__meta` CF key `vector_wal_clock_hwm`. This is the only durable state the clock needs.

**No global serialization**: two `TxSession` objects calling `vector_wal_timestamp()` concurrently each receive a distinct value from `fetch_add`. No mutex is held; no session waits for another. The only serialization remaining is the `RwLock` write lock on each index during in-memory index mutation (unchanged from the prior design).

**Timestamp order vs commit order**: within a single session, timestamp order and commit order are the same. Across concurrent sessions, a session that calls `fetch_add` first but `db.write_opt` second (due to scheduling) will have a lower timestamp than a session that committed first. For same-entity concurrent writes, WAL replay may apply operations in timestamp order rather than commit order. This is an edge-case write conflict — the graph property value (in the main CFs) is resolved correctly by RocksDB's sequence numbers; the vector index may reflect a stale state for that entity until the next write to it. Documented in §5 known limitations.

---

## 5. Write path

```rust
impl TxSession {
    pub fn commit(mut self) -> Result<()> {
        if self.pending_vector_ops.is_empty() {
            // fast path: no vectors touched, no WAL overhead
            return self.inner.commit();
        }

        let wal_cf = self.db.cf_handle(CF_VECTOR_WAL).unwrap();

        let mut batch = WriteBatch::default();

        // 1. graph mutations (vertices, edges, scalar properties)
        self.inner.flush_to_batch(&mut batch)?;

        // 2. vector WAL entries — one per pending op; each gets a unique timestamp
        for op in self.pending_vector_ops.iter() {
            let ts     = vector_wal_timestamp();
            let random = rand::random::<u32>();
            let mut key = [0u8; 15];
            key[0..2].copy_from_slice(&op.prop_key_id.to_be_bytes());
            key[2]    = op.entity_type as u8;
            key[3..11].copy_from_slice(&ts.to_be_bytes());
            key[11..15].copy_from_slice(&random.to_be_bytes());
            batch.put_cf(&wal_cf, &key, encode_vector_op(op));
        }

        // 3. single fsync covers graph data + vector WAL entries
        self.db.write_opt(batch, &write_options_with_sync())?;

        // 4. apply to in-memory index AFTER the durable write.
        //    A crash here is safe: the WAL entry is durable and will be
        //    replayed on the next Graph::open.
        for op in self.pending_vector_ops.drain(..) {
            let index = self.vector_indexes
                .get(&op.prop_key)
                .expect("index config validated on insert");
            let mut guard = index.write().unwrap();
            match &op.kind {
                VectorOpKind::Insert(vec) => guard.insert(op.vertex_id, vec)?,
                VectorOpKind::Delete      => guard.remove(op.vertex_id)?,
            }
            // write lock released at end of each iteration —
            // allows concurrent reads between individual index updates
        }

        Ok(())
    }
}
```

Step 4 is the only inconsistency window. Between the durable commit (step 3)
and the in-memory index update (step 4), the HNSW index is momentarily stale.
A crash here is safe: the WAL entry is on disk and will be replayed on the
next `Graph::open`.

**Step-4 error (not crash)**: if a step-4 `insert`/`remove` returns an error
(e.g. OOM in usearch), `commit()` propagates the error to the caller. The
RocksDB batch is already durable — there is no rollback. The caller receives
`Err` indicating the index is temporarily inconsistent; the WAL entry is present
and self-heals on the next `Graph::open`. The inconsistency window is bounded
to the time until the next restart. See `design_hnsw_impl.md` §13 for the
rationale against an inline retry queue.

The write lock in step 4 is **per-operation**, released between each
`insert`/`remove`. This means a concurrent `vectorNear` search can acquire the
read lock between two pending ops and observe a partially-applied batch.
This is acceptable — the partially-applied state is consistent (no half-written
vector), and full visibility is guaranteed only after `commit()` returns.

---

## 6. Recovery path (`Graph::open`)

```rust
fn recover_vector_indexes(
    db:      &DB,
    indexes: &HashMap<(VectorEntityType, SmolStr), Arc<RwLock<Box<dyn VectorIndex>>>>,
) -> Result<()> {
    // Recovery runs single-threaded before Graph opens to queries,
    // so write locks are uncontested. No risk of deadlock.
    let wal_cf = db.cf_handle(CF_VECTOR_WAL).unwrap();

    for ((entity_type, prop_key), index) in indexes.iter() {
        let prop_key_id = schema_registry.intern(prop_key);
        let cutoff_ts   = index.read().unwrap().last_replayed_timestamp();

        // Seek to the first entry for this (prop_key_id, entity_type) prefix.
        let mut prefix = [0u8; 3];
        prefix[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
        prefix[2] = *entity_type as u8;

        let iter = db.iterator_cf(
            &wal_cf,
            IteratorMode::From(&prefix, Direction::Forward),
        );

        let mut guard = index.write().unwrap(); // held for entire replay of this index
        for item in iter {
            let (key, value) = item?;
            // Stop when we leave the (prop_key_id, entity_type) prefix.
            if !key.starts_with(&prefix) {
                break;
            }
            // Extract timestamp from key bytes [3..11].
            let entry_ts = u64::from_be_bytes(key[3..11].try_into().unwrap());
            if entry_ts <= cutoff_ts {
                continue;  // already applied in a prior session
            }

            let op = decode_vector_op(&value)?;
            match op.kind {
                VectorOpKind::Insert(vec) => guard.insert(&op.entity_key, &vec)?,
                VectorOpKind::Delete      => guard.remove(&op.entity_key)?,
            }
        }
    }

    Ok(())
}
```

Because entries for the same `(prop_key_id, entity_type)` sort together, each
index recovery is a targeted prefix seek — it does not scan the entire CF from
the beginning. Each index replays only its own entries independently.

**Recovery is called before the graph accepts any queries.** Users cannot
observe a stale index.

### Recovery scenarios

| Scenario | WAL state | Index snapshot | Recovery action |
|----------|-----------|----------------|-----------------|
| Clean shutdown | Flushed | Current | prefix seek finds no new entries; returns immediately |
| Crash after commit, before index update | WAL entry present | Stale by 1+ ops | prefix seek finds missed entries; replays them |
| Crash during HNSW flush | WAL entry present | Partially written or missing | Load previous good snapshot; replay all entries since its timestamp |
| First open (no snapshot) | Entries present from all prior commits | Does not exist | `last_replayed_timestamp = 0`; replay entire prefix range (or rebuild from props CF — see §7) |
| INSERT followed by DELETE for same entity | Both entries in WAL, in timestamp order | May have neither applied | Replay in timestamp order: Insert runs first, Delete runs second — correct final state is absent from index |

**Note on INSERT→DELETE correctness**: WAL entries are replayed in strict timestamp
order within each `(prop_key_id, entity_type)` prefix, so a vertex that was inserted
and later dropped will always produce the correct final index state (absent), regardless
of which entries are present in the WAL vs the snapshot. An entity's final index
membership is determined by the last WAL operation for that
`(entity_type, prop_key, entity_key)` triple — the replay sequence is deterministic
and safe.

**Known limitation — timestamp vs commit order**: across concurrent sessions, WAL
entries are ordered by `fetch_add` timestamp, not by RocksDB commit order. Two
concurrent sessions writing the same entity's same vector property may replay in
the wrong order after a crash. The graph property value is resolved correctly by
RocksDB sequence numbers; only the vector index may reflect a stale value for that
entity until the next write to it. This self-heals on the next update to that entity.

---

## 7. First open: full rebuild vs WAL replay

On the very first `Graph::open` after a graph is created (no snapshot file
exists), `last_replayed_timestamp = 0` and the WAL contains all vector mutations since
the graph was created. Two strategies:

**Strategy A — replay the entire WAL:**
- Efficient if the WAL is short (most vectors were inserted recently)
- WAL entries are already in chronological order; no sorting needed
- Can be slow if the graph has a long history and WAL was never trimmed

**Strategy B — full scan of the props CF:**
- Scan all vertices for `FloatVector` properties matching the index config
- Ignores WAL entirely; rebuilds from ground truth
- Consistent regardless of WAL length or trimming history

**Decision: Strategy B for first open (no snapshot file).**

Rationale: the props CF is the authoritative ground truth. A WAL-only replay
could miss vectors inserted before WAL tracking was enabled (e.g. when
upgrading from a version without vector support). Strategy B is always correct.

The scan target depends on `VectorIndexConfig.entity_type`:
- `VectorEntityType::Vertex` → scan vertex props CF for `FloatVector` values
  matching the declared property key
- `VectorEntityType::Edge` → scan edge props CF for `FloatVector` values
  matching the declared property key

Both scans are sequential RocksDB reads. For a graph with 500K vertices and
200K edges each carrying 768-dim embeddings, the combined rebuild time is
roughly 1–3 minutes.

After the first rebuild, the index is saved with `last_replayed_timestamp` set to
the current `WAL_CLOCK` value. Subsequent opens use snapshot + WAL replay
(Strategy A pattern).

---

## 8. Online index build: WAL catch-up for `add_vector_index`

When `open_schema().add_vector_index(config).commit()` is called on a graph
that already has `FloatVector` values stored, the synchronous variant must
batch-build the HNSW index from all current property data. This happens inside
`SchemaSession::commit()` and raises a write-consistency problem if not handled
correctly.

### The problem

During the bulk scan + HNSW build:

- The new index is not yet visible in the schema registry.
- Concurrent `TxSession` commits write `FloatVector` values to `CF_VERTICES`
  and `CF_VECTOR_WAL`. Because the index does not exist in the registry, those
  sessions do not call `VectorIndex::insert()` on the new index — they have no
  knowledge of it.
- After the build completes and the schema is atomically published, every write
  that arrived concurrently is present in `CF_VERTICES` and `CF_VECTOR_WAL` but
  **absent from the new index**.

Blocking all writes for the full build duration prevents this, but is
unacceptable — a 10M-vector build can take minutes.

### The solution: WAL catch-up

WAL entries are written for **all** `FloatVector` property writes once the
property key is schema-registered, regardless of whether a vector index exists
for that property. Every concurrent write during the build is therefore already
captured in `CF_VECTOR_WAL`. The build procedure uses a WAL mark to replay
only the delta that accumulated during the bulk scan:

```
1. WAL_MARK = WAL_CLOCK.load(Acquire)
   Upper bound of timestamps already in CF_VECTOR_WAL when the build starts.

2. Bulk build (no lock held — the index is not yet registered):
     Scan CF_VERTICES for FloatVector entries matching (entity_type, prop_key_id).
     RocksDB iterator sees a point-in-time snapshot of CF_VERTICES.
     Batch-insert all found vectors into the new HNSW index.
     Concurrent TxSession commits proceed freely and append to CF_VECTOR_WAL.

3. Acquire write lock on the new index.

4. WAL catch-up:
     Replay all CF_VECTOR_WAL entries for (prop_key_id, entity_type)
     with ts > WAL_MARK, in timestamp order.
     This brings the index current with every write that arrived during step 2.

5. index.last_replayed_timestamp = WAL_CLOCK.load(Acquire)

6. Atomically commit schema update (CAS on CF_SCHEMA version key).
   The index is now visible to all sessions.

7. Release write lock.
```

The write lock (step 3) is held only during WAL replay (step 4) and the schema
CAS (step 6), not during the bulk build (step 2). If N writes arrived during
the build, catch-up is O(N) — fast relative to the bulk scan of the full
dataset. Step 6's CAS also prevents a concurrent `SchemaSession` from
interleaving a conflicting schema change.

### Why writes do not need to be blocked

Two facts together guarantee safety:

1. **All concurrent FloatVector writes go to CF_VECTOR_WAL**: the property key
   is already schema-registered as `DataType::FloatVector`, so `TxSession::commit()`
   always writes a WAL entry. This is true even before any vector index exists.

2. **The bulk scan reads a consistent RocksDB snapshot**: the iterator opened
   in step 2 sees `CF_VERTICES` at the moment it was created. Any key written
   after that point is not visible to the scan — but it carries a WAL timestamp
   `> WAL_MARK` and is replayed in step 4.

After step 4, the union of {bulk-scanned vectors} + {WAL-replayed vectors}
equals exactly the set of committed `FloatVector` values for this property at
the time the write lock was acquired.

### Edge cases

**Entity written and deleted during the build**: both WAL entries (Insert
followed by Delete, in timestamp order) have `ts > WAL_MARK` and are replayed
in order. Correct final state: absent from the index.

**Entity written before `add_vector_index` was called, never touched again**:
present in `CF_VERTICES`, captured by the bulk scan in step 2. No WAL entry
needed or expected.

**`add_vector_index_async` (v0.3)**: the same WAL_MARK + catch-up mechanism
applies. The bulk scan runs in a background thread; WAL catch-up runs when the
thread finishes, under the same brief write lock before schema commit.

### Interaction with crash recovery

If the process crashes during step 2 (bulk build), the schema CAS (step 6) has
not run. On `Graph::open`, the new index is not in the schema registry — the
partial build is silently abandoned. No crash marker is needed for the online
build path. The next `add_vector_index` call starts fresh.

If the process crashes after step 6 (schema committed) but the index has no
snapshot yet (`last_replayed_timestamp = 0`), standard crash recovery (§6)
treats it as a first-open and runs the full Strategy B rebuild from
`CF_VERTICES` (§7). This is always correct.

---

## 9. HNSW snapshot and WAL trimming

### Snapshot format

One file per declared vector index, stored alongside the RocksDB directory:

```
{db_path}/vector_idx_{prop_key}.snapshot
```

File layout:

```
[magic:                  u32 = 0x52475F56 ]   // "RG_V"
[format_version:         u16              ]   // for future schema evolution
[last_replayed_timestamp: u64 LE          ]   // WAL timestamp cutoff at flush time
[dimension:              u32 LE           ]   // sanity check on load
[metric:                 u8               ]   // DistanceMetric enum value
[algorithm:              u8               ]   // AnnAlgorithm enum value
[hnsw_payload:           ...              ]   // algorithm-specific serialized bytes
```

The `last_replayed_timestamp` in the header is the sole link between the snapshot
and the WAL. It is the timestamp cutoff for recovery replay: only WAL entries with
`ts > last_replayed_timestamp` for the matching `(prop_key_id, entity_type)` prefix
are replayed.

### When to flush

The HNSW index is flushed to disk:
1. **On clean shutdown** — always, to minimize WAL replay on next open
2. **Periodically** — configurable interval (default: every 10 minutes or
   every 10K mutations, whichever comes first) **(v0.3 — background thread)**
3. **After tombstone rebuild** — the rebuilt index is immediately flushed

**v0.2 limitation — no online snapshot**: in v0.2, trigger 2 is not yet
implemented. The only guaranteed flush is on clean shutdown (trigger 1). If a
process runs for weeks without a clean shutdown (crash or SIGKILL), the WAL
accumulates the entire history of vector mutations. On the next `Graph::open`,
WAL replay must process all of them — which can take minutes at high write rates.

Concretely: at 1,000 vector writes/hour, a 7-day unclean run accumulates 168,000
WAL entries × ~6 KB each ≈ ~1 GB of WAL to replay on restart.

Users can call `graph.save_vector_index()` explicitly to checkpoint the snapshot
and allow WAL trimming. Periodic background snapshots are a v0.3 feature tracked
in `design_hnsw_rebuild.md`.

Flushing is a background operation (v0.3) and does not block reads or writes.

### WAL trimming

After a successful snapshot flush, WAL entries up to `last_replayed_timestamp` are
no longer needed for recovery. They can be trimmed per index:

```rust
fn trim_vector_wal(
    db:           &DB,
    prop_key_id:  u16,
    entity_type:  VectorEntityType,
    cutoff_ts:    u64,
) -> Result<()> {
    let wal_cf = db.cf_handle(CF_VECTOR_WAL).unwrap();

    // start of this index's key space: [prop_key_id][entity_type][0...][0...]
    let mut start_key = [0u8; 15];
    start_key[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
    start_key[2] = entity_type as u8;

    // end key (exclusive): [prop_key_id][entity_type][cutoff_ts + 1][0...]
    let mut end_key = [0u8; 15];
    end_key[0..2].copy_from_slice(&prop_key_id.to_be_bytes());
    end_key[2] = entity_type as u8;
    end_key[3..11].copy_from_slice(&(cutoff_ts + 1).to_be_bytes());

    // range tombstone — O(1) write, compacted lazily by RocksDB
    db.delete_range_cf(&wal_cf, &start_key, &end_key)?;
    Ok(())
}
```

`delete_range_cf` writes a single range tombstone rather than deleting entries
individually. RocksDB removes them during compaction. Trimming is called once
per declared index after each snapshot flush — no global scan needed.

**If trimming is skipped**, WAL growth is bounded by mutations since the last
flush. At 1000 vector writes/day with daily flushes: `1000 × 6172 B ≈ 6 MB`
— negligible. Trimming is therefore optional in v0.2 and can be enabled in v0.3.

---

## 10. Multi-property support

All declared vector indexes share one `CF_VECTOR_WAL`. Entries for different
property indexes sort into separate key prefixes (`[prop_key_id][entity_type]`),
so they do not interleave with each other within the CF. A future full-text
search index would get its own separate `CF_TEXT_WAL` — it shares nothing with
`CF_VECTOR_WAL`, including compaction tuning, recovery iteration, and WAL trimming.

Recovery is fully parallel: each index performs its own prefix seek by
`[prop_key_id][entity_type]` and reads only its own entries. No global scan,
no cross-index interleaving. Two sessions writing to completely independent
indexes (`image_embedding` vs `text_embedding`) do not contend on any shared
lock — `fetch_add` on the `AtomicU64` clock is the only shared operation, at
~5 ns per call.

---

## 11. Failure scenarios and guarantees

### Guarantee

**After any failure, when `Graph::open` completes, the vector index reflects
exactly the set of vectors in the committed graph store — no more, no less.**

### Scenario analysis

**Crash during `WriteBatch::write` (before fsync completes):**
- RocksDB's own WAL rolls back the partial batch
- The vector WAL entry was in the same batch — also rolled back
- The HNSW snapshot is unchanged
- Recovery: nothing to replay; index is consistent

**Crash after `WriteBatch::write`, before step 4 (in-memory update):**
- Graph mutation: committed and durable
- Vector WAL entry: committed and durable
- In-memory HNSW: does not reflect the new vector
- Recovery: WAL entry found via prefix seek with `ts > last_replayed_timestamp`; replayed into index

**Crash during HNSW snapshot write:**
- The partially-written snapshot file is detected on load (magic/checksum
  mismatch) and discarded
- Fall back to the previous good snapshot + WAL replay since its seqno
- Recovery: correct, slightly more WAL to replay

**Clean shutdown with snapshot flush, then crash before trimming:**
- Next open loads the fresh snapshot (timestamp cutoff = S)
- WAL entries with `ts ≤ S` may still exist (trimming hadn't happened)
- Recovery: prefix seek finds no entries with `ts > S`; returns immediately
- The untrimmed entries are harmless and will be cleaned up on next trim

---

## 12. RocksDB CF options for `CF_VECTOR_WAL`

These options are specific to vector WAL data characteristics: dense float blobs
(~6 KB each), sequential write pattern, and compressibility near zero. A future
`CF_TEXT_WAL` would use different settings — smaller write buffer, LZ4 compression
(text compresses well), and potentially a different compaction style.

```rust
fn vector_wal_cf_options() -> Options {
    let mut opts = Options::default();
    // vectors are dense floats — compression saves little and adds CPU cost
    opts.set_compression_type(DBCompressionType::None);
    // sequential write pattern; large write buffer reduces flush frequency
    opts.set_write_buffer_size(64 * 1024 * 1024);  // 64 MB
    // range tombstones (from trimming) are cleaned up eagerly
    opts.set_level_compaction_dynamic_level_bytes(true);
    opts
}
```

---

## 13. Implementation checklist (v0.2)

- [ ] Add `CF_VECTOR_WAL` (`"vector_wal"`) and `__meta` CFs to `Graph::open` CF list
- [ ] Call `init_wal_clock(db)` on `Graph::open` to seed `WAL_CLOCK` from `max(wall_clock_micros, stored_hwm)`
- [ ] Define `EntityKey`, `EdgeKey`, `VectorEntityType` types
- [ ] Add `HashMap<(VectorEntityType, SmolStr), Arc<RwLock<Box<dyn VectorIndex>>>>` to `Graph` struct
- [ ] Implement `encode_vector_op` / `decode_vector_op` with value carrying only `op_type` + `entity_key` + vector (no `entity_type` or `prop_key` — those are in the key)
- [ ] Add `pending_vector_ops: Vec<VectorOp>` to `TxSession`
- [ ] Hook `OP_PROPERTY` handler: push to `pending_vector_ops` when value is
      `GValue::FloatVector` and the property key is declared as `DataType::FloatVector`
      (regardless of whether a vector index exists — WAL entries are needed for
      online index build catch-up even before an index is declared; see §8)
- [ ] Hook `OP_DROP` (vertex drop): push `VectorOpKind::Delete` with
      `EntityKey::Vertex` for all indexed vertex vector properties on the
      dropped vertex
- [ ] Hook `OP_DROP` (edge drop, if applicable): push `VectorOpKind::Delete`
      with `EntityKey::Edge` for all indexed edge vector properties
- [ ] Implement `TxSession::commit` write path using `vector_wal_timestamp()` + random suffix for each WAL key (§5)
- [ ] Persist `WAL_CLOCK.load(Acquire)` to `__meta` CF `vector_wal_clock_hwm` on every snapshot flush
- [ ] Implement `recover_vector_indexes` with per-index prefix seek by `[prop_key_id][entity_type]` + timestamp cutoff (§6)
- [ ] Implement snapshot save/load with `last_replayed_timestamp` field in header (§9)
- [ ] Implement `trim_vector_wal` with per-index range delete (§9, optional for v0.2)
- [ ] Add `Strategy B` full rebuild path for first open — scan vertex props CF
      for `Vertex` configs, edge props CF for `Edge` configs (§7)
- [ ] Add `DimensionMismatch` error variant to `StoreError`
- [ ] **Online index build — `add_vector_index` on existing data (§8)**:
  - [ ] Capture `WAL_MARK = WAL_CLOCK.load(Acquire)` before opening the `CF_VERTICES` iterator
  - [ ] Bulk-scan `CF_VERTICES` and batch-insert into the new index (no lock held during scan)
  - [ ] After bulk build, acquire write lock on the new index
  - [ ] Replay `CF_VECTOR_WAL` entries for `(prop_key_id, entity_type)` with `ts > WAL_MARK`
  - [ ] Set `index.last_replayed_timestamp = WAL_CLOCK.load(Acquire)` before schema CAS
  - [ ] Apply same WAL_MARK + catch-up logic to `add_vector_index_async` background thread (v0.3)
