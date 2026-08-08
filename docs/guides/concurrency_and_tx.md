# Concurrency, Transactions & Sessions

**Target:** RocksGraph v0.2.0+

RocksGraph provides ACID transactions with Snapshot Isolation and Optimistic Concurrency Control (OCC).

It supports concurrent readers and writers without global database locks.

---

## 1. Session Taxonomy

RocksGraph organizes operations into five distinct session/handle types:

```
Graph(path)
  ├── .read()             ──► ReadSession     (Lock-free point-in-time snapshot reads)
  ├── .begin()            ──► TxnSession      (ACID transactional writes with RYOW)
  ├── .open_schema()      ──► SchemaSession   (Atomic DDL — labels, types, vector indexes)
  ├── .open_bulk_loader() ──► BulkLoader      (High-throughput offline SST file ingestion)
  └── .index_manager()    ──► IndexManager    (Vector index maintenance — rebuild, save)
```

| Session | Concurrency Model | Write Mechanism | Consistency |
| :--- | :--- | :--- | :--- |
| **`ReadSession`** | Multi-threaded lock-free | Read-only | Point-in-time Snapshot Isolation |
| **`TxnSession`** | Concurrent writers (OCC) | Transactional write buffer | ACID, Read-Your-Own-Writes |
| **`SchemaSession`** | Serialized atomic updates | Catalog update | Immediate catalog consistency |
| **`BulkLoader`** | Single offline worker | Direct data generation | Atomic ingestion at commit |
| **`IndexManager`** | Direct, immediate execution | No staging — each call mutates live index state | Applied instantly, nothing to commit |

> [!TIP]
> **`IndexManager`**: unlike `SchemaSession`, which accumulates DDL changes and commits them atomically, `IndexManager` (`graph.index_manager()`) executes each operation (`rebuild()`, `save_all()`) immediately against the live index — there's no batching or rollback. See [Vector Search Deep Dive](vector_search.md) and [Bulk Loading](bulk_loading.md) for usage.
>
> **Python context managers**: `ReadSession`, `TxnSession`, `SchemaSession`, and `BulkLoader` all support `with ... as x:` — for `TxnSession`/`SchemaSession` this commits on clean exit and rolls back on exception (see §3 below); for `ReadSession`/`BulkLoader` it's a scoping convenience with no commit/rollback semantics.

> [!TIP]
> **Thread-Safety & Multi-Threading**: The root `Graph` instance is internally `Arc`-wrapped and implements `Send + Sync` in Rust (thread-safe in Python). You can clone `graph` handles cheaply across worker threads. Each thread opens its own `ReadSession` or `TxnSession` independently.

---

## 2. Reading Data: `ReadSession`

`ReadSession` captures a point-in-time database snapshot upon creation. It provides a static, immutable view of the graph that never blocks writers and is never blocked by concurrent transactions.

#### 🦀 Rust
```rust
let mut snap = graph.read();
let results = snap.g().V([1]).out(["knows"]).values(["name"]).to_list()?;
```

#### 🐍 Python
```python
snap = graph.read()
results = snap.g().V(1).out("knows").values("name").to_list()
```

---

## 3. Writing Data: `TxnSession` & Read-Your-Own-Writes (RYOW)

`TxnSession` stages mutations in a local buffer before committing. Queries executed against the transaction see uncommitted mutations immediately:

> [!NOTE]
> **Automatic Rollback on Drop**: `TxnSession` does not require an explicit `rollback()` call. If a `TxnSession` is dropped before `.commit()` is called (or if an unhandled exception exits Python's `with graph.begin() as txn:` context manager), uncommitted mutations are automatically discarded.

#### 🦀 Rust
```rust
let mut txn = graph.begin();

// 1. Write vertex 100
txn.g().addV("item").property("id", 100i64).property("price", 25.0f64).next()?;

// 2. Query vertex 100 inside the same transaction (RYOW)
let price = txn.g().V([100]).values(["price"]).to_list()?;
println!("Uncommitted price seen: {:?}", price); // [Float64(25.0)]

// 3. Commit to make changes permanent and visible to other sessions
txn.commit()?;
```

#### 🐍 Python
```python
with graph.begin() as txn:
    # 1. Write vertex 100
    txn.g().addV("item").property("id", 100).property("price", 25.0).next()

    # 2. Query inside the transaction (RYOW)
    price = txn.g().V(100).values("price").to_list()
    print("Uncommitted price seen:", price) # [25.0]

    # Context manager automatically commits on clean block exit
```

---

## 4. OCC Conflict Handling & Retry Loops

RocksGraph uses Optimistic Concurrency Control. If two transactions modify the same vertex or edge concurrently, the first transaction to commit succeeds, while the second will fail with a conflict error (`StoreError::Conflict` in Rust, `TransactionError` in Python).

### Implementing a Conflict Retry Loop

#### 🦀 Rust
```rust
use rocksgraph::{Graph, StoreError, Value};

fn transfer_credits(graph: &Graph, from: i64, to: i64, amount: f64) -> Result<(), StoreError> {
    const MAX_RETRIES: usize = 5;

    for attempt in 1..=MAX_RETRIES {
        let mut txn = graph.begin();

        // Read current balances within transaction
        let from_bal: f64 = match txn.g().V([from]).values(["balance"]).to_list()?.first() {
            Some(Value::Float64(v)) => *v,
            _ => return Err(StoreError::NotFound),
        };

        let to_bal: f64 = match txn.g().V([to]).values(["balance"]).to_list()?.first() {
            Some(Value::Float64(v)) => *v,
            _ => return Err(StoreError::NotFound),
        };

        if from_bal < amount {
            return Err(StoreError::UnsupportedOperation("insufficient funds".into()));
        }

        // Apply updates
        txn.g().V([from]).property("balance", from_bal - amount).next()?;
        txn.g().V([to]).property("balance", to_bal + amount).next()?;

        // Attempt commit
        match txn.commit() {
            Ok(()) => return Ok(()),
            Err(StoreError::Conflict) if attempt < MAX_RETRIES => {
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt as u64));
                continue; // Retry with fresh snapshot
            }
            Err(e) => return Err(e),
        }
    }

    Err(StoreError::Conflict)
}
```

#### 🐍 Python
```python
import time
from rocksgraph import Graph, TransactionError

def transfer_credits(graph: Graph, from_id: int, to_id: int, amount: float, max_retries: int = 5):
    for attempt in range(1, max_retries + 1):
        try:
            with graph.begin() as txn:
                from_results = txn.g().V(from_id).values("balance").to_list()
                to_results = txn.g().V(to_id).values("balance").to_list()
                if not from_results or not to_results:
                    raise LookupError("sender or receiver account not found")
                from_bal, to_bal = from_results[0], to_results[0]

                if from_bal < amount:
                    raise ValueError("insufficient funds")

                txn.g().V(from_id).property("balance", from_bal - amount).next()
                txn.g().V(to_id).property("balance", to_bal + amount).next()
                # Commit occurs on exiting the `with` block
            return
        except TransactionError:
            if attempt == max_retries:
                raise
            time.sleep(0.01 * attempt)
```

---

## 5. What Actually Conflicts: The OCC Conflict Matrix

Conflict detection works on individual physical keys, not whole traversals — knowing which operations touch a key tells you what will and won't conflict:

- **Point reads enroll in the conflict check.** `.V(id)`, `.hasId(id)`, and other point lookups register that specific vertex/edge key, even if the transaction never writes it — a concurrent write to that key committed before yours will conflict.
- **Scans don't.** `.out()`, `.in()`, `.both()`, and similar adjacency scans read a plain snapshot; the elements they pass over aren't tracked, so a scan alone never conflicts, however large.
- **Writes always do.** Any `.property(...)`, `addV()`, `addE()` registers that key; a concurrent write to the same key conflicts at commit.

| Scenario | Conflict? |
| :--- | :--- |
| Two transactions write the same vertex/edge | ✅ (whichever commits second) |
| One transaction reads a vertex with `.V(id)`; another writes and commits to that vertex first | ✅ |
| Two transactions write disjoint vertices/edges (no shared keys) | ❌ — safe to run fully in parallel |
| A transaction scans `.out()` over many edges but writes none of them | ❌, regardless of scan size |
| A transaction scans `.out()`, then writes one scanned edge that another transaction also writes | ✅, but only for that one edge |
| A `ReadSession`, alongside any number of `TxnSession`s | ❌ — never conflicts; it reads an immutable snapshot outside OCC entirely |
| Two transactions in `SchemaMode::Auto` both auto-register the same new label/property key for the first time | ✅ — first-time registration writes shared catalog metadata; see [Schema Management](schema_management.md#10-schema-anti-patterns) |

---

## 6. Transaction Best Practices

### Pattern 1: Keep Transactions Ultra-Short
Perform all heavy computations, embedding generation, or external API calls **before** opening `txn = graph.begin()`. Hold the transaction open only for the brief moment required to execute mutations and commit.

```python
# ✅ BEST PRACTICE: Heavy work done outside transaction window
embedding = model.encode(document_text) # 50ms outside txn

with graph.begin() as txn:              # < 1ms inside txn
    txn.g().addV("doc").property("id", doc_id).property("emb", Vector(embedding)).next()
```

### Pattern 2: Size Batches to Your Contention, Not Just Your Throughput
Batching amortizes per-commit overhead, but every extra read/write also widens the transaction's conflict set (§5) — and if the commit conflicts, the whole batch is wasted and must be retried, not just the colliding row. The right size depends on whether the batch's keys overlap with concurrent writers:

- **Disjoint keys, low contention** (e.g. a single-writer import into fresh IDs): batch large. There's little to collide with, so bigger batches just mean fewer commits.
- **Shared/hotspot keys, many concurrent writers** (e.g. workers updating overlapping vertices or shared counters): batch small, down to one mutation per transaction if needed — a large batch multiplies the odds that *something* in it collides, and a single collision discards the entire batch.

There's no RocksGraph-specific benchmark backing a universal batch size — it depends on your workload's contention and mutation cost. Start with a modest batch (a few dozen to a couple hundred mutations) and measure: watch commit latency and conflict/retry rate as you increase it, and stop increasing once conflicts start rising or the throughput gain flattens out.

```python
# ✅ Low contention: large batches for throughput
with graph.begin() as txn:
    for item in disjoint_batch:
        txn.g().addV("item").property("id", item.id).next()

# ✅ High contention on shared keys: small batches, so a conflict
# costs one retry instead of redoing the whole batch
for update in hotspot_updates:
    with graph.begin() as txn:
        txn.g().V(update.id).property("counter", update.value).next()
```

### Pattern 3: Use `ReadSession` for Analytical Traversals
`ReadSession` creates zero lock contention and zero memory overhead on write paths. For long-running traversals, pathfinding, or vector scans, always use `graph.read()`.

---

## 7. Transaction Anti-Patterns

### ❌ Anti-Pattern 1: Long-Lived Open Transactions
Holding a transaction open across network requests, user prompts, or long calculations increases OCC conflict probability and pins memory.

```python
# ❌ ANTI-PATTERN: Transaction open across slow network call
with graph.begin() as txn:
    txn.g().addV("user").property("id", 1).next()
    resp = requests.post("https://api.external.com/notify") # 300ms blocking I/O!
    txn.g().addV("log").property("id", 2).property("status", resp.status_code).next()
```

### ❌ Anti-Pattern 2: Committing Per-Record When Nothing Forces You To
Committing every entity in its own transaction has real per-commit overhead. This is the *default* to avoid — batch when keys are disjoint and contention is low (Pattern 2 above). It stops being an anti-pattern once you actually have hotspot contention: at that point, small or single-mutation transactions are the correct trade-off, not a mistake.

```python
# ❌ ANTI-PATTERN: 10,000 separate transactions for independent, non-contended writes
for user in users:
    with graph.begin() as txn:
        txn.g().addV("user").property("id", user.id).next()
```

### ❌ Anti-Pattern 3: Ignoring OCC Conflict Errors
In multi-threaded environments, two concurrent transactions modifying the same vertex key will trigger `StoreError::Conflict` (`TransactionError` in Python). Never silently drop or ignore conflict exceptions; always implement an exponential backoff retry loop.

---

## Related Topics

- [Getting Started](getting_started.md) — 5-minute practical onboarding.
- [Bulk Loading](bulk_loading.md) — High-throughput offline SST ingestion.
- [Performance Tuning](performance.md) — Optimizing throughput and concurrency.
