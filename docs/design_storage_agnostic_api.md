# Design: Storage-Agnostic User API

Status: proposal.

---

## 1. Motivation

RocksGraph is tightly integrated with RocksDB — and it should be. The storage
engine is chosen, not pluggable. But the **user-facing API** should not require
the user to know this. Two places leak the implementation today:

| Leak | Current API | User pain |
|------|-------------|-----------|
| Open with RocksDB tunables | `Graph::open_with_rocksdb_options(path, schema, rocks_opts)` | User must import `RocksOptions`, learn CF names, understand memtable sizing |
| Bulk loader work dir | `graph.open_bulk_loader("/tmp/bulk_work")` | User manages a temp directory for SST generation — an implementation detail |

Both leaks are unnecessary. The user's mental model is "open a graph" and "load
data fast." The storage engine underneath should not leak into the function name
or required arguments.

---

## 2. Change 1: `StorageOptions` unified inside `GraphOptions`

Instead of creating multiple `open_with_*` methods (`open_with_options`, `open_with_storage_options`, `open_with_rocksdb_options`), all storage, runtime, and schema configurations are unified cleanly inside **`GraphOptions`**.

```rust
// Before (Method explosion and raw RocksDB leak)
let g = Graph::open_with_rocksdb_options(
    path,
    GraphOptions { mode: SchemaMode::Strict, ..Default::default() },
    &RocksOptions::default()
        .max_open_files(1000)
        .cf_options("CF_VERTICES", ColumnFamilyOptions::default()
            .block_based_table_factory(BlockBasedOptions::default()
                .block_cache_mb(512))),
)?;

// After (Unified, storage-agnostic, single entry point)
let g = Graph::open_with_options(
    path,
    GraphOptions {
        mode: SchemaMode::Strict,
        storage: StorageOptions {
            block_cache_mb: 512,
            max_open_files: 1000,
            ..Default::default()
        },
        vector_runtime: VectorRuntimeOptions {
            default_limit: Some(VectorIndexLimit { memory_limit_bytes: 4 * 1024 * 1024 * 1024 }),
            per_index_overrides: vec![
                IndexLimitOverride {
                    entity_type: VectorEntityType::Vertex,
                    property: "text_embedding".into(),
                    limit: VectorIndexLimit { memory_limit_bytes: 8 * 1024 * 1024 * 1024 },
                },
            ],
        },
        ..Default::default()
    },
)?;
```

### `StorageOptions` definition

`StorageOptions` exposes only user-relevant hardware & storage tunables without leaky RocksDB jargon (no CF names, no BlockBasedTableFactory boilerplate), leaving room for separate `QueryOptions` in the future:

```rust
#[derive(Debug, Clone)]
pub struct StorageOptions {
    /// Maximum number of open file descriptors. Default: 1000.
    pub max_open_files: i32,

    /// Block cache size in MB, shared automatically across all internal Column Families.
    /// Default: 256.
    pub block_cache_mb: usize,

    /// Global write buffer size in MB (memtable capacity). Default: 128.
    pub write_buffer_mb: usize,

    /// Background I/O and compaction threads. Default: number of CPU cores.
    pub max_background_jobs: i32,

    /// Advanced Escape Hatch: direct modification of raw RocksDB options.
    /// Allows power-users full access without needing separate open methods.
    pub custom_rocks_modifier: Option<std::sync::Arc<dyn Fn(&mut rocksdb::Options) + Send + Sync>>,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            max_open_files: 1000,
            block_cache_mb: 256,
            write_buffer_mb: 128,
            max_background_jobs: num_cpus::get() as i32,
            custom_rocks_modifier: None,
        }
    }
}
```

---

## 3. Change 2: Bulk loader auto-manages its work directory

```rust
// Before — user provides a temp path manually
let mut loader = graph.open_bulk_loader("/tmp/bulk_work")?;

// After — auto-managed work directory
let mut loader = graph.open_bulk_loader()?;

// Explicit override for datasets where a dedicated NVMe scratchpad is desired
let mut loader = graph.open_bulk_loader()
    .with_temp_dir("/mnt/nvme/scratch")?
    .with_max_memory(1024 * MB);
```

### Why defaulting to `{db_path}/_bulk_work` is critical:
1. **Zero Cross-Device Copy (Atomic Hard-Links):** By placing `_bulk_work` inside the DB directory by default, RocksDB's `IngestExternalFile` can **hard-link or move** the generated SST files into the active DB in $O(1)$ time rather than performing a slow byte-by-byte file copy across file systems.
2. **Deterministic Crash Recovery:** Crash markers and partial SST files live within the database boundary and are automatically swept during `Graph::open()` crash recovery.

```python
# Python API
with g.open_bulk_loader() as bulk:
    bulk.load_vertices(iter)
    bulk.load_edges(iter)
    # temp dir auto-created in {db_path}/_bulk_work, auto-cleaned on __exit__
```

---

## 4. Non-goals

- **Hiding RocksDB from internal design docs.** `design_vector_*.md` and internal modules keep their RocksDB-specific implementations — they target engine developers.
- **Pluggable backend abstraction layer.** RocksDB is the chosen, permanent storage engine. `StorageOptions` provides an ergonomic facade, avoiding leaky abstractions while retaining maximum engine performance.
- **Removing RocksDB power features.** Power users can use `StorageOptions.custom_rocks_modifier` to tweak low-level RocksDB parameters directly.

---

## 5. Implementation plan

| Step | What | Effort |
|------|------|:---:|
| 1 | Add `StorageOptions` struct and embed inside `GraphOptions` | ~35 lines |
| 2 | Implement conversion from `StorageOptions` to RocksDB DB & CF options | ~40 lines |
| 3 | Deprecate `Graph::open_with_rocksdb_options` in favor of `Graph::open_with_options` | 1 attribute |
| 4 | Update `open_bulk_loader` to default to `{db_path}/_bulk_work` | ~25 lines |
| 5 | Update `design_session_workflows.md` and user docs | ~15 lines |

