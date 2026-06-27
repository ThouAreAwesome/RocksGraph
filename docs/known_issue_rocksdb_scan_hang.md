# rocksdb 0.22 + macOS ARM: `scan_edges` pagination hang

## Problem

`test_scan_edges_paginates_across_src_id_prefixes` (in `src/store/rocks/snapshot.rs:474`
and `src/store/rocks/transaction.rs:1158`) intermittently hangs on macOS ARM (Darwin 24.4,
Apple Silicon M-series).

**Not a code bug.** The `scan_edges` Rust code is correct: the RocksDB iterator loop has a
bound (`result.len() >= limit`) and exhausts via `for item in iter` → `None`.  The hang
comes from RocksDB itself.

## Root cause

`rocksdb 0.22` bundles `librocksdb-sys 0.16` which wraps **RocksDB 8.10.0** (mid-2023).

RocksDB 8.x has a known issue on Apple Silicon where `set_total_order_seek(true)` doesn't
fully disable prefix-extractor-based optimizations when the merge iterator crosses SST
boundaries.  Specifically:

1. The `edges_out` CF has an 8-byte fixed prefix extractor (for `outE()` / `inE()`
   prefix scans).  This is used as a `set_prefix_same_as_start(true)` optimisation
   for single-vertex adjacency scans — correct and necessary.

2. Full-graph scans (`scan_edges`) disable this via `set_total_order_seek(true)`.
   However in RocksDB 8.x on ARM64, the merge iterator internally still tracks
   prefix boundaries during re-seeks, and can stall when crossing from one SST
   to the next when there are many small SST files.

3. The snapshot test creates **50 separate SST files** (one per `src_id`, each
   committed+flushed in its own transaction).  This is enough to hit the stall
   deterministically on ARM64.

4. `flush_cf` congestion: 50 sequential `db.flush_cf()` calls on ARM64/APFS
   create internal RocksDB flush-queue contention that can also cause long stalls.

RocksDB 9.x rewrote the merge iterator to properly honour `total_order_seek` across
SST boundaries.  RocksDB 10.x added further Apple Silicon fixes including APFS-aware
`flush_cf`.

## Fix options

| Option | Effort | What it does |
|--------|--------|-------------|
| **A: Upgrade `rocksdb` to 0.23+** (aleksuss fork, wraps RocksDB 10.x) | Change `Cargo.toml` (`rocksdb = "0.23"`), fix any API breakage in the `Transaction` wrapper | Fixes the real RocksDB bug; applies to all workloads |
| **B: Upgrade to MaterializeInc/rust-rocksdb** (wraps RocksDB 8.3–9.x) | Same Cargo.toml change | Less aggressive jump; may not include 10.x ARM fixes |
| **C: Reduce SST count in test** (50 → 15) | One line in `snapshot.rs` | Avoids the hang but doesn't fix the underlying RocksDB issue |
| **D: Batch inserts in the test** — create all edges in one transaction, flush once | A few lines | Tests pagination correctness; no longer tests cross-SST boundary resilience |

**Recommendation**: Option A (upgrade) + Option D (batch the test setup).  The upgrade
fixes the real issue for all workloads; batching makes the test fast regardless of RocksDB
version.

## Related

- `src/store/rocks/snapshot.rs:315` — `RocksSnapshot::scan_edges`
- `src/store/rocks/transaction.rs:378` — `Transaction::scan_edges`
- `src/store/rocks/store.rs` — CF creation with prefix extractor
- `Cargo.lock` — `rocksdb 0.22.0`, `librocksdb-sys 0.16.0+8.10.0`
