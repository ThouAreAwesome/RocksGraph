# Bulk Loading & Offline Ingestion

**Target:** RocksGraph v0.2.0+

RocksGraph provides a dedicated high-throughput **Bulk Loader** designed for importing millions of graph elements rapidly.

Bulk loading bypasses online transaction logging, sorts data chunks offline into temporary storage, generates sorted storage files directly, and ingests them atomically into the database.

---

## 1. When to Use Bulk Loading

| Metric | Transactional Writes (`TxnSession`) | Bulk Loader (`BulkLoader`) |
| :--- | :--- | :--- |
| **Throughput** | Per-transaction OCC validation + WAL overhead | Substantially higher — bypasses OCC and WAL entirely |
| **I/O Pattern** | Online transactional writes | Offline external sorting + direct batch ingestion |
| **ACID Isolation** | Snapshot Isolation (OCC) with RYOW | Offline batch ingestion |
| **Best For** | OLTP queries, live mutations | Initial dataset imports, large batch syncs |

> [!NOTE]
> See [`BENCHMARKS.md`](https://github.com/ThouAreAwesome/RocksGraph/blob/main/rocksgraph/BENCHMARKS.md) for measured figures. The write-path benchmarks there were run at different dataset scales (`TxnSession` at 1M edges, `BulkLoader` at 69M), so they aren't a controlled side-by-side comparison — don't derive a specific multiplier from them.

---

## 2. Bulk Loading Workflow

The bulk loading process executes in three distinct phases:

1. **Phase 1: Vertex Ingestion (`load_vertices`)**: Streams and sorts vertex records along with their labels and properties into temporary storage.
2. **Phase 2: Edge Ingestion (`load_edges`)**: Streams and sorts edge records with optional multi-edge ranks (`0` to `65,534`; `65,535` is a reserved sentinel for auto-assignment), connecting source and destination vertices.
3. **Phase 3: Final Ingestion & Index Build (`commit`)**: Atomically ingests the sorted data into the graph database. If vector indexes are declared in the schema, they are automatically constructed and saved to disk.

> [!NOTE]
> Each phase method may be called exactly once, in order. Calling `load_edges()` or `commit()` before `load_vertices()` returns `StoreError::VerticesNotLoaded`; calling `load_vertices()` or `load_edges()` a second time on the same loader returns `StoreError::UnsupportedOperation`.

---

## 3. Configuration Options

| Parameter | Method | Default | Description |
| :--- | :--- | :--- | :--- |
| **Sort Memory Buffer** | `.with_max_memory(bytes)` | `512 MiB` | RAM budget for in-memory sorting before spilling intermediate runs to disk. |
| **Target SST Size** | `.with_max_sst_size(bytes)` | `58 MiB` | Target file size for generated data files. |
| **Work Directory** | `.with_work_dir(path)` | Database temp dir | Scratch directory used for intermediate spill files. |

---

## 4. Bulk Loading Example

#### 🦀 Rust
```rust
use rocksgraph::{
    bulk::{BulkEdge, BulkVertex},
    Graph, StoreError,
};

fn import_large_graph(graph: &Graph) -> Result<(), StoreError> {
    let mut loader = graph.open_bulk_loader()?;

    // Optional configuration tuning:
    loader = loader.with_max_memory(1024 * 1024 * 1024) // 1 GiB sort buffer
                   .with_max_sst_size(64 * 1024 * 1024); // 64 MiB SST target

    // 1. Prepare and stream vertices
    let vertices = vec![
        BulkVertex::new(1i64, "person")
            .with_property("name", "Alice")
            .with_property("age", 30i32),
        BulkVertex::new(2i64, "person")
            .with_property("name", "Bob")
            .with_property("age", 32i32),
    ];
    loader.load_vertices(vertices)?;

    // 2. Prepare and stream edges (with optional multi-edge rank: 0..=65534)
    let edges = vec![
        BulkEdge::new(1i64, "knows", 2i64)
            .with_rank(0u16)
            .with_property("since", 2020i32)
            .with_property("weight", 0.95f64),
    ];
    loader.load_edges(edges)?;

    // 3. Finalize SST generation and atomically ingest into database
    loader.commit()?;

    println!("Bulk load completed successfully!");
    Ok(())
}
```

#### 🐍 Python
```python
from rocksgraph import Graph, BulkVertex, BulkEdge

def import_large_graph(graph: Graph):
    loader = graph.open_bulk_loader()

    # Optional configuration:
    loader.with_max_memory(1024 * 1024 * 1024)
    loader.with_max_sst_size(64 * 1024 * 1024)

    # 1. Prepare and load vertices
    vertices = [
        BulkVertex(1, "person", {"name": "Alice", "age": 30}),
        BulkVertex(2, "person", {"name": "Bob", "age": 32}),
    ]
    loader.load_vertices(vertices)

    # 2. Prepare and load edges (supports rank: int, 0..65534)
    edges = [
        BulkEdge(1, "knows", 2, {"since": 2020, "weight": 0.95}, rank=0),
    ]
    loader.load_edges(edges)

    # 3. Finalize and ingest atomically
    loader.commit()

    print("Bulk load completed successfully!")
```

---

## 5. Custom Data Sources

`load_vertices()`/`load_edges()` accept any iterator, not just a `Vec` — each item can be a `BulkVertex`/`BulkEdge` directly, or a `Result<BulkVertex, StoreError>`/`Result<BulkEdge, StoreError>`. That means you can stream your own file format lazily, parsing one record at a time, without buffering the whole dataset in memory first:

#### 🦀 Rust
```rust
use rocksgraph::{bulk::BulkVertex, StoreError};
use std::{fs::File, io::{BufRead, BufReader}};

fn stream_vertices_from_csv(path: &str) -> impl Iterator<Item = Result<BulkVertex, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("open csv"));
    reader.lines().map(|line| {
        let line = line.map_err(StoreError::Io)?;
        let mut cols = line.split(',');
        let id: i64 = cols
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| StoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "missing id column")))?;
        let name = cols.next().unwrap_or_default();
        Ok(BulkVertex::new(id, "person").with_property("name", name))
    })
}

// loader.load_vertices(stream_vertices_from_csv("people.csv"))?;
```

#### 🐍 Python
Python's `load_vertices()`/`load_edges()` accept any iterable, including a generator — the same lazy-streaming pattern applies:

```python
def stream_vertices_from_csv(path):
    with open(path) as f:
        for line in f:
            id_str, name = line.rstrip("\n").split(",", 1)
            yield BulkVertex(int(id_str), "person", {"name": name})

# loader.load_vertices(stream_vertices_from_csv("people.csv"))
```

> [!NOTE]
> There's no built-in format-adapter abstraction yet (CSV, JSON Lines, GraphSON, etc.) — you write the parsing loop for your own format, as shown above. A `BulkSource` trait with shipped adapters for common formats is a planned addition (see `docs/design/ingestion-bindings/design_bulkload_source_formats.md`); today, any iterator of `BulkVertex`/`BulkEdge` (or `Result` of one) works with `load_vertices`/`load_edges`.

---

## 6. Vector Indexes & Bulk Loading

If a vector index is declared in the schema before bulk loading, it's built automatically during `loader.commit()` and persisted to disk. If it wasn't declared beforehand, `.nearest()` on that property doesn't error — it silently falls back to an exact brute-force scan (see [Vector Search Deep Dive](vector_search.md#7-vector-search-anti-patterns)). Build the real index after the fact with `graph.index_manager().rebuild(VectorEntityType::Vertex, "embedding_property")` (or `graph.index_manager().rebuild(VectorEntityType.Vertex, "embedding_property")` in Python, `from rocksgraph import VectorEntityType`) before relying on `.nearest()` for performance.

---

## 7. Bulk Loading Best Practices

### Pattern 1: Ordered Phase Ingestion (Vertices First, Then Edges)
Always stream and complete all vertices via `load_vertices()` before calling `load_edges()`. The edge ingestion phase relies on the vertex catalog to resolve endpoint vertices and relationships.

### Pattern 2: Size Memory Sorter Buffer to Available RAM
The external sorter uses memory buffers to sort chunks before writing temporary runs. Increasing `with_max_memory` from the default 512 MiB to 1–2 GiB on powerful ingest nodes drastically reduces disk spill runs.

### Pattern 3: Use Dedicated NVMe Scratch Storage
Set `with_work_dir("/fast_nvme/tmp_sort")` to prevent sort temp files from competing with OS page cache on standard HDDs.

---

## 8. Bulk Loading Anti-Patterns

### ❌ Anti-Pattern 1: Online Transaction Loops for Initial Ingestion
Importing millions of records via standard `TxnSession` writes creates unnecessary WAL sync overhead and can take hours instead of seconds.

```python
# ❌ ANTI-PATTERN: 1,000,000 online transactions (~30 minutes)
for row in csv_data:
    with graph.begin() as txn:
        txn.g().addV("item").property("id", row.id).next()

# ✅ CORRECT: BulkLoader offline SST generation (~15 seconds)
loader.load_vertices(bulk_vertices)
loader.commit()
```

---

## Related Topics

- [Data Model & Type System](data_model.md) — Identifier capacities, rank limits, and types.
- [Performance Tuning](performance.md) — Throughput optimization and batching.
- [Schema Management](schema_management.md) — Pre-declaring schemas for bulk ingestion.
- [Vector Search Deep Dive](vector_search.md) — HNSW index build during bulk loads.
