# Design: Vector Index Concurrency Model

Status: proposal — extracted from `design_vector_search.md` §9.

---

## Table of Contents

- [1. The two race conditions](#1-the-two-race-conditions)
  - [Race 1 — WAL counter overwrite](#race-1--wal-counter-overwrite)
  - [Race 2 — concurrent index mutation](#race-2--concurrent-index-mutation)
- [2. Fix 1: process-local `AtomicU64` clock](#2-fix-1-process-local-atomicu64-clock)
- [3. Fix 2: `RwLock` per index](#3-fix-2-rwlock-per-index)
  - [3a. Single-index commit sequence](#3a-single-index-commit-sequence)
  - [3b. Memory limit enforcement and OOM prevention](#3b-memory-limit-enforcement-and-oom-prevention)
  - [3c. Multi-index commit sequence](#3c-multi-index-commit-sequence)
- [4. Options comparison](#4-options-comparison)
- [5. Known limitations and design decisions](#5-known-limitations-and-design-decisions)
  - [5a. Timestamp vs commit order for same-entity concurrent writes](#5a-timestamp-vs-commit-order-for-same-entity-concurrent-writes)
  - [5b. OOM causes permanent crash loop without the replay safety net](#5b-oom-causes-permanent-crash-loop-without-the-replay-safety-net)
  - [5c. Write lock held during brute-force search (v0.1)](#5c-write-lock-held-during-brute-force-search-v01)
  - [5d. Read-your-own-writes within an uncommitted transaction](#5d-read-your-own-writes-within-an-uncommitted-transaction)

---

Graph data updates are inherently parallel — multiple `TxSession` objects can
be active and committing concurrently. The naive single-threaded design has two
race conditions that must be fixed before any concurrent workload is safe.

---

## 1. The two race conditions

### Race 1 — WAL key collision

The old design used a single `Arc<Mutex<u64>>` counter. Two concurrent commits
both read `vector_wal_seq = N` from RocksDB, both compute the next key as `N + 1`,
and both write to the same WAL CF key. The second silently overwrites the first's
vector mutation. The new 15-byte composite key eliminates this race entirely:
each session calls `fetch_add(1, AcqRel)` on a process-local `AtomicU64` clock
and appends a 4-byte random suffix. No two sessions can produce the same key.

### Race 2 — concurrent index mutation

Two commits both call `apply_vector_op_to_index` concurrently on the same
`VectorIndex`. HNSW implementations are not thread-safe for concurrent
mutation: concurrent inserts can corrupt adjacency lists, and a concurrent
search + insert can produce incorrect results or a panic.

---

## 2. Fix 1: process-local `AtomicU64` clock

**Decision: replace the `Arc<Mutex<u64>>` counter with a process-local `AtomicU64` clock.**

Each session calls `vector_wal_timestamp()` which does `fetch_add(1, AcqRel)`
on a static `AtomicU64`. This is ~5 ns per call and requires no lock. A 4-byte
random suffix is appended to the key to prevent collision between two sessions
that happen to call `fetch_add` at the same moment across threads. No mutex is
held across the `write()` call; sessions proceed fully independently.

The `__meta` CF key `vector_wal_clock_hwm` stores the high watermark of the
clock, persisted on every snapshot flush. On `Graph::open`, the clock is seeded
from `max(SystemTime_micros, stored_hwm)` to survive NTP skew and fast restarts.
Full design in `design_vector_wal.md` §4.

---

## 3. Fix 2: `RwLock` per index

**Decision: wrap each `VectorIndex` in `Arc<RwLock<Box<dyn VectorIndex>>>`.**

Lock semantics:
- **Write lock** — acquired in `TxSession::commit` after `db.write_opt()` returns,
  to call `insert`/`remove` on the index
- **Read lock** — acquired for the full duration of a `vectorNear` search

This allows concurrent searches to proceed without blocking each other, while
serializing concurrent index mutations. A search never observes a
partially-inserted vector.

**Read-your-own-writes is preserved**: after `TxSession::commit` returns
successfully, the write lock has been released and the vector is visible to any
subsequent `vectorNear` call in the same or any other session.

### 3a. Single-index commit sequence

```
1. Call vector_wal_timestamp() → ts (fetch_add, ~5 ns, no lock)
   Append 4-byte random suffix → WAL key
2. Build WriteBatch (graph + WAL entries)
3. db.write_opt(batch, sync_opts)?          ← durable commit (fsync)

4. Acquire RwLock write lock                ← index lock
5. index.insert / index.remove
6. Release RwLock write lock
```

A **crash** between step 3 and step 4 is safe: the WAL entry is durable and
will be replayed on the next `Graph::open`. See `design_vector_wal.md` §10.

### 3b. Memory limit enforcement and OOM prevention

**Design principle: the memory limit is a hard boundary enforced before any
durable write. A commit that would exceed the limit is rejected entirely — no
graph write, no WAL entry, no split-brain.**

#### Memory limit in `VectorIndexRuntimeOpts`

`memory_limit_bytes` is an **environmental constraint** — it is supplied per-open
via `GraphOptions::vector_runtime` as a `VectorIndexRuntimeOpts` entry and is never
persisted to CF_SCHEMA. `Graph::open` passes the resolved limit to `UsearchHnswIndex`
at construction time. The persisted `VectorIndexConfig` (dimension, metric, algorithm)
contains no memory limit.

```rust
// Supplied at open, applied to UsearchHnswIndex at construction; never saved to disk
pub struct VectorIndexRuntimeOpts {
    pub entity_type:        VectorEntityType,
    pub property:           String,
    pub memory_limit_bytes: Option<usize>,  // None = unlimited (use with caution)
}
```

#### Pre-insert memory check in `UsearchHnswIndex`

The memory check lives inside `UsearchHnswIndex::insert`, making it the single
enforcement point used by both the commit path and WAL replay:

```rust
fn insert(&mut self, key: &EntityKey, vector: &[f32]) -> Result<()> {
    if let Some(limit) = self.memory_limit_bytes {
        let estimated = self.current_memory_bytes() + self.per_insert_overhead;
        if estimated > limit {
            return Err(VectorError::MemoryLimitExceeded {
                current:   self.current_memory_bytes(),
                limit,
                estimated,
            });
        }
    }
    // ... actual usearch insert ...
}
```

`current_memory_bytes()` is a cheap structural estimate, not a precise malloc probe:

```rust
fn current_memory_bytes(&self) -> usize {
    // raw vector storage: size × dim × bytes-per-scalar
    let bytes_per_scalar = match self.config.quantization {
        Quantization::F32       => 4,
        Quantization::F16       => 2,
        Quantization::RaBitQ(_) => 1,  // bit-packed; approximate
    };
    let vectors = self.index.size() * self.config.dimension * bytes_per_scalar;

    // HNSW adjacency: layer-0 has 2M neighbours per node; higher layers add M each.
    // 3×M per node is a pessimistic upper bound (e.g. M=16 → 48 × 8 = 384 bytes/node),
    // accounting for multi-layer nodes without per-node level tracking.
    // Use the actual M from config rather than a hardcoded constant.
    let m = match &self.config.algorithm {
        AnnAlgorithm::Hnsw { m, .. } => *m,
        _ => 16,
    };
    let adjacency = self.index.size() * (3 * m) * 8;

    // Label maps are in the `vector_edge_labels` CF (not in heap). No in-memory
    // map term here — label lookups go through the RocksDB block cache.

    // 20% safety margin: this estimate omits usearch's node headers and internal
    // bookkeeping. If usearch exposes index.memory_usage() in the locked crate version,
    // replace the adjacency estimate with that value and retain only the safety margin.
    ((vectors + adjacency) * 12) / 10
}
```

Pessimistic by design — better to refuse a commit than to OOM. The 3×M adjacency
multiplier is ~50% higher than the minimum (2×M for layer-0 only), providing headroom
for multi-layer nodes and usearch's per-node bookkeeping.

#### Commit path (step order matters)

```
TxSession::commit()
  │
  ├─ [PRE-CHECK] for each vector in the transaction:
  │     index.would_exceed_limit(vector)?  ← calls insert() logic, no-op on success
  │     → MemoryLimitExceeded?  → return Err  ← BEFORE WAL write, no durable side-effect
  │
  ├─ For each op: vector_wal_timestamp() + rand suffix → WAL key  (~5 ns each, no lock)
  ├─ Build WriteBatch (graph + WAL entries)
  ├─ db.write_opt(batch, sync_opts)?   ← fsync; only reached if pre-check passed
  │
  └─ index.insert(vector)   ← safe: memory was pre-checked; limit cannot be
                               exceeded between check and insert because the
                               RwLock write lock is held from step 4 onward
```

The pre-check reads the index under the RwLock **read** lock to allow concurrent
searches during the check. The actual insert acquires the **write** lock. Because
no concurrent insert can run between the pre-check and the insert (the write lock
is acquired before the insert), the estimate remains valid.

#### WAL replay — breaking the OOM death loop

WAL replay calls the same `index.insert()`, which includes the same memory check.
If the limit is set and the entry would exceed it, `insert()` returns
`VectorError::MemoryLimitExceeded` — the replay loop skips that entry rather than
aborting:

```rust
for entry in wal_entries_since(snapshot_timestamp) {
    match index.insert(&entry.key, &entry.vector) {
        Ok(()) => {}
        Err(VectorError::MemoryLimitExceeded { .. }) => {
            warn!("WAL replay skipped ts={} key={:?}: memory limit reached",
                  entry.ts, entry.key);
            skipped_count += 1;
        }
        Err(e) => return Err(e),  // other errors still abort replay
    }
}
```

This breaks the crash loop: the offending entry is skipped, the database boots,
and `VectorIndexStats.replay_skipped_count` gives the operator a clear signal
that the index is partial and needs more memory or an explicit rebuild after
capacity is freed.

Without a configured limit (`None`), the memory check is skipped and a true
usearch allocation failure at replay time could still loop. Operators running
without a limit accept this risk — the recommended default is to always set
`memory_limit_bytes` sized to ~80 % of available RAM.

#### Step-7 error for non-OOM failures

For errors other than memory (e.g. corrupted vector data, usearch internal
assertion), the outcome is:

```
3. db.write_opt(...)  → ✅ durable
5. index.insert(...)  → ❌ Err(non-OOM)
```

`TxSession::commit` propagates the error. The graph state is committed; the
index is transiently stale. WAL replay on the next `Graph::open` will retry
the insert and succeed (assuming the underlying fault was transient). Log at
`error` level with entity key and property; do **not** panic.

### 3c. Multi-index commit sequence

When a transaction touches N vector indexes (e.g. both `image_embedding` and
`text_embedding`), all N WAL entries are bundled in the same `WriteBatch` and
covered by a single `fsync`. The in-memory index updates then happen
sequentially, one index at a time:

```
1. For each pending op: call vector_wal_timestamp() + random suffix → WAL key
2. Build WriteBatch:
   - graph mutations
   - WAL[ts_A + rand_A] = op for index A
   - WAL[ts_B + rand_B] = op for index B
3. db.write_opt(batch, sync_opts)?            ← ONE fsync covers A + B + graph

4a. Acquire RwLock write lock — index A
5a. index_A.insert / index_A.remove           ← may fail
6a. Release RwLock write lock — index A

4b. Acquire RwLock write lock — index B
5b. index_B.insert / index_B.remove           ← may fail independently
6b. Release RwLock write lock — index B
```

**Partial failure**: if 5a succeeds but 5b fails (or vice versa), both WAL
entries are already durable (step 3). Index A is consistent in-memory; index B
is stale but will catch up on the next WAL replay. `commit()` returns `Err`
after index B fails, so the caller is informed. On restart, both WAL entries
replay from the last snapshot timestamp — the already-applied index A operation
replays as an idempotent upsert or no-op delete, causing no harm.

---

## 4. Options comparison

| Option | Mechanism | Concurrent reads | Concurrent writes | Read-your-own-writes |
|--------|-----------|:---:|:---:|:---:|
| A — `RwLock` per index **(chosen)** | `Arc<RwLock<Box<dyn VectorIndex>>>` | ✅ | serialized | ✅ |
| B — concurrent ANN library | `usearch` lock-free inserts | ✅ | ✅ | ✅ |
| C — background writer thread | channel + single applier | ✅ | ✅ (queued) | ❌ |

**Option C rejected**: a commit returns before the background thread applies
the vector, violating read-your-own-writes — unacceptable for an embedded OLTP
database.

**Option A chosen for v0.2**. If benchmarks show write-lock contention at high
concurrent write rates, Option B (`usearch` lock-free inserts) can replace the
inner implementation without changing the `Arc<RwLock<>>` wrapper, the WAL
design, or the `VectorIndex` trait.

---

## 5. Known limitations and design decisions

### 5a. Timestamp vs commit order for same-entity concurrent writes

Two concurrent sessions writing the same entity's same vector property call
`fetch_add` in some order to get their timestamps, then call `db.write_opt` in
potentially a different order (due to OS scheduling). The session that committed
first may have a higher timestamp than the session that committed second.

WAL replay uses timestamp order within each `(prop_key_id, entity_type)` prefix.
For a same-entity conflict, replay may apply operations in timestamp order rather
than commit order. The graph property value (in the main CFs) is resolved correctly
by RocksDB's sequence numbers; the vector index may reflect a stale value for that
entity until the next write to it. This is an edge-case write conflict that
self-heals on the next update to that entity. Full discussion in `design_vector_wal.md` §6.

### 5b. `memory_limit_bytes: None` leaves OOM crash loop risk

HNSW memory is unbounded and unmanaged — unlike RocksDB's block cache there is
no eviction mechanism. If `memory_limit_bytes` is `None`, the pre-commit check
in §3b is skipped. A user who inserts enough vectors to exhaust host RAM will
trigger a true usearch allocation failure. At commit time this hits step 7
(after the WAL write), causing split-brain. On restart, WAL replay attempts the
same insert, hits the same OOM, and crashes again — **permanent crash loop
until hardware is upgraded or the index is manually deleted from `__meta` CF**.

**Recommendation**: always supply a `VectorIndexRuntimeOpts` entry with
`memory_limit_bytes` set to ~80 % of available RAM in `GraphOptions::vector_runtime`.
Treat `None` (no entry / missing entry for an index) as an expert escape hatch, not
a safe default. Document this prominently in the Python/JS `Graph` constructor
docstring and any getting-started guide.

### 5c. Write lock held during brute-force search (v0.1)

`BruteForceIndex::search` holds the `RwLock` read lock for the full linear
scan — up to ~50ms at 100K × 1536 dims. A concurrent `TxSession::commit`
blocks at step 6 (write lock acquisition) for this duration after its
RocksDB fsync at step 3 is already complete. This produces a visible commit
stall at prototyping scale. Eliminated in v0.2 where HNSW search completes
in <5ms.

### 5d. Read-your-own-writes within an uncommitted transaction

**Problem**: `vectorNear` operates on the committed HNSW index. If a transaction
inserts a vertex with an embedding and then issues `vectorNear` in the same
uncommitted transaction, the newly inserted vertex is invisible — its vector is
in `pending_vector_ops` but not yet applied to the index.

```python
tx = graph.tx()
tx.addV('doc').property('embedding', [0.1, 0.2, ...])
tx.V().vectorNear([0.1, 0.2, ...], 5)   # ← does NOT find 'doc'
```

**Fix**: `VectorNearStep` merges HNSW results with a brute-force scan of the
transaction's `pending_vector_ops`. The merge logic:

1. Compute the **effective pending state**: replay `pending_vector_ops` in order;
   last op per entity key wins. Result: a map of `EntityKey → Option<Vec<f32>>`
   (None = deleted, Some(v) = pending insert or update).
2. **Filter HNSW results**: remove any entity whose effective pending state is
   `None` (deleted in this transaction). Overfetch by `pending_deletes.len()` to
   compensate.
3. **Brute-force scan**: compute exact distances for all entities with a `Some`
   pending vector, using the same metric as the index.
4. **Merge and truncate**: combine and sort by distance, return top k.

```rust
fn execute_vector_near_with_pending(
    step:        &VectorNearStep,
    index:       &Arc<RwLock<Box<dyn VectorIndex>>>,
    pending_ops: &[(EntityKey, VectorOpKind)],
) -> Result<Vec<(EntityKey, f32)>> {
    // Step 1: compute effective pending state
    let mut effective: HashMap<&EntityKey, Option<&[f32]>> = HashMap::new();
    for (key, op) in pending_ops {
        match op {
            VectorOpKind::Insert(v) => { effective.insert(key, Some(v)); }
            VectorOpKind::Delete    => { effective.insert(key, None); }
        }
    }
    let pending_deletes: HashSet<&EntityKey> =
        effective.iter().filter_map(|(k, v)| v.is_none().then_some(*k)).collect();
    let pending_inserts: Vec<(&EntityKey, &[f32])> =
        effective.iter().filter_map(|(k, v)| v.map(|vec| (*k, vec))).collect();

    // Step 2: HNSW search with overfetch for pending deletes
    let hnsw_k = step.k + pending_deletes.len();
    let guard = index.read().unwrap();
    let mut results = guard.search(&step.query, hnsw_k)?;
    drop(guard);
    results.retain(|(key, _)| !pending_deletes.contains(key));

    // Step 3: brute-force scan of pending inserts
    for (key, vec) in &pending_inserts {
        let dist = compute_raw_distance(&step.query, vec, step.metric);
        results.push(((*key).clone(), dist));
    }

    // Step 4: merge, sort ascending by distance, return top k
    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
    results.truncate(step.k);
    Ok(results)
}
```

**Cost**: the brute-force scan is O(|pending_inserts| × dim). Transactions
typically touch O(10s–100s) entities — negligible. A transaction inserting 10K
vectors before searching is an unusual pattern; the cost (~10K × 1536 × 10 ns
≈ 150 ms) is proportional to what the user chose to do.

**Accuracy**: the pending inserts use exact distance, not approximate HNSW.
The merged result is more accurate than the base HNSW query for any newly
inserted entities. Already-committed entities continue to use approximate HNSW.
