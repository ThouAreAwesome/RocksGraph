# Bulk Loading & Offline Ingestion

**Target:** RocksGraph v0.2.0+

RocksGraph provides a dedicated high-throughput **Bulk Loader** designed for importing millions of graph elements rapidly.

Bulk loading bypasses online transaction logging, sorts data chunks offline into temporary storage, generates sorted storage files directly, and ingests them atomically into the database.

> [!WARNING]
> `BulkLoader` does not check whether the database already has data, and it does not merge or reject on ID collisions the way `TxnSession`'s OCC does. If a vertex or edge you bulk-load has the same ID as one that already exists, `commit()` silently **overwrites** it — no error, no conflict signal, and the previous properties are simply gone. Bulk loading is designed for loading into an empty database (or a batch of definitely-new IDs); it is not a safe way to "sync" or re-run against data you've already loaded, and it is not an upsert.

---

## 1. When to Use Bulk Loading

| Metric | Transactional Writes (`TxnSession`) | Bulk Loader (`BulkLoader`) |
| :--- | :--- | :--- |
| **Throughput** | Per-transaction OCC validation + WAL overhead | Substantially higher — bypasses OCC and WAL entirely |
| **I/O Pattern** | Online transactional writes | Offline external sorting + direct batch ingestion |
| **ACID Isolation** | Snapshot Isolation (OCC) with RYOW | Offline batch ingestion |
| **Best For** | OLTP queries, live mutations | Initial dataset imports, large batch syncs |

> [!NOTE]
> See [Benchmarks](benchmarks.md) for measured figures. The write-path benchmarks there were run at different dataset scales (`TxnSession` at 1M edges, `BulkLoader` at 69M), so they aren't a controlled side-by-side comparison — don't derive a specific multiplier from them.

---

## 2. Bulk Loading Workflow

You drive `BulkLoader` through three calls, made exactly once each, in order: `load_vertices()`, then `load_edges()`, then `commit()`. Here's what to expect from each.

1. **`load_vertices(vertices)`** — pass any iterator of `BulkVertex` (or `Result<BulkVertex, StoreError>`, so a parse error can flow straight through — see [Custom Data Sources](#5-custom-data-sources)). Every vertex needs a unique `id`, a `label`, and a `props` map. Labels and property keys are checked against the schema as each vertex streams past: in Strict mode an undeclared label/key fails immediately with `StoreError::SchemaViolation`, naming the offending vertex; in Auto mode it's registered on the spot instead (see the caveat under [Best Practices](#7-bulk-loading-best-practices)). Nothing is written to the database yet — this call only builds up loader-internal state.
2. **`load_edges(edges)`** — same shape, for `BulkEdge` (`src`, `dst`, `label`, `props`, optional `rank` from `0` to `65,534`; `65,535` is reserved for auto-assignment). Must be called after `load_vertices()`. This call does **not** check that `src`/`dst` actually correspond to vertices you loaded in step 1 — that check happens in `commit()`, described below.
3. **`commit()`** — this is the only call that touches the real database, and it does so atomically: either every vertex and edge you streamed becomes visible at once, or (on error) none of it does, and the database is left exactly as it was before you started. This is also where the deferred validation from step 2 happens — an edge whose `src` or `dst` wasn't included in `load_vertices()` fails here with `StoreError::SchemaViolation` naming the missing vertex, not earlier when that edge was streamed. If vector indexes are declared in the schema, they're built as part of this call too, from the data you just loaded. **Atomic here means "all or nothing," not "conflict-checked"**: if any vertex/edge ID you're loading already exists in the database, it's silently overwritten — see the warning at the top of this guide.

> [!NOTE]
> Calling `load_edges()` or `commit()` before `load_vertices()` returns `StoreError::VerticesNotLoaded`; calling `load_vertices()` or `load_edges()` a second time on the same loader returns `StoreError::UnsupportedOperation`. And since validation is streamed rather than upfront, a bad record late in a large input still costs you the time spent processing everything before it — there's no separate "validate first, then load" pass.

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
`BulkVertex`/`BulkEdge` have no builder methods — all fields are `pub`; construct them with struct literals:

```rust
use rocksgraph::{
    bulk::{BulkEdge, BulkVertex},
    Graph, Primitive, StoreError,
};
use std::collections::HashMap;

fn import_large_graph(graph: &Graph) -> Result<(), StoreError> {
    let mut loader = graph.open_bulk_loader()?;

    // Optional configuration tuning:
    loader = loader.with_max_memory(1024 * 1024 * 1024) // 1 GiB sort buffer
                   .with_max_sst_size(64 * 1024 * 1024); // 64 MiB SST target

    // 1. Prepare and stream vertices
    let vertices = vec![
        BulkVertex {
            id: 1,
            label: "person".into(),
            props: HashMap::from([
                ("name".into(), Primitive::String("Alice".into())),
                ("age".into(), Primitive::Int32(30)),
            ]),
        },
        BulkVertex {
            id: 2,
            label: "person".into(),
            props: HashMap::from([
                ("name".into(), Primitive::String("Bob".into())),
                ("age".into(), Primitive::Int32(32)),
            ]),
        },
    ];
    loader.load_vertices(vertices)?;

    // 2. Prepare and stream edges (with optional multi-edge rank: 0..=65534)
    let edges = vec![
        BulkEdge {
            src: 1,
            dst: 2,
            label: "knows".into(),
            props: HashMap::from([
                ("since".into(), Primitive::Int32(2020)),
                ("weight".into(), Primitive::Float64(0.95)),
            ]),
            rank: Some(0),
        },
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

    # 2. Prepare and load edges — positional order is (src, dst, label, props, rank)
    edges = [
        BulkEdge(1, 2, "knows", {"since": 2020, "weight": 0.95}, rank=0),
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
use rocksgraph::{bulk::BulkVertex, Primitive, StoreError};
use std::{collections::HashMap, fs::File, io::{BufRead, BufReader}};

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
        Ok(BulkVertex {
            id,
            label: "person".into(),
            props: HashMap::from([("name".into(), Primitive::String(name.into()))]),
        })
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

### Deriving Vertices From an Edge-Only Source

The example above assumes your source lists vertices explicitly. Many real datasets don't — a plain edge list (`src dst` per line, no separate vertex file) is one of the most common shapes you'll encounter, and it's what [`bench_write.rs`](https://github.com/ThouAreAwesome/RocksGraph/blob/main/rocksgraph/src/bin/bench_write.rs) itself ingests for the numbers in [Benchmarks](benchmarks.md). `BulkLoader` still needs an explicit `BulkVertex` for every vertex an edge touches, so you have to derive that vertex set from the edges yourself, in two passes over the file:

1. **First pass** — stream every line, parsing out just the `src`/`dst` IDs, and collect them into a set. You only buffer the *set of unique IDs*, not the file's rows — for a graph with far more edges than vertices (the common case), this is much smaller than the dataset itself.
2. **Second pass** — re-open the same file and stream it again as edges, exactly like the CSV example above.

#### 🦀 Rust
```rust
use rocksgraph::{bulk::{BulkEdge, BulkVertex}, StoreError};
use std::{collections::{BTreeSet, HashMap}, fs::File, io::{BufRead, BufReader}};

// Pass 1: collect every distinct vertex ID mentioned by an edge.
// BTreeSet gives sorted, de-duplicated IDs — sorted order isn't required by
// `load_vertices`, but it does mean vertices arrive in the same order BulkLoader
// will write them, which is a minor sequential-I/O win.
fn collect_vertex_ids(path: &str) -> std::io::Result<BTreeSet<i64>> {
    let mut ids = BTreeSet::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let mut cols = line.split_whitespace();
        if let (Some(src), Some(dst)) = (cols.next(), cols.next()) {
            if let (Ok(src), Ok(dst)) = (src.parse::<i64>(), dst.parse::<i64>()) {
                ids.insert(src);
                ids.insert(dst);
            }
        }
    }
    Ok(ids)
}

// Pass 2: stream the same file again, this time as edges.
fn stream_edges_from_list(path: &str) -> impl Iterator<Item = Result<BulkEdge, StoreError>> {
    let reader = BufReader::new(File::open(path).expect("open edge list"));
    reader.lines().filter_map(|line| {
        let line = line.ok()?;
        let mut cols = line.split_whitespace();
        let (Some(src), Some(dst)) = (cols.next(), cols.next()) else { return None };
        let (Ok(src), Ok(dst)) = (src.parse::<i64>(), dst.parse::<i64>()) else { return None };
        Some(Ok(BulkEdge { src, dst, label: "knows".into(), props: HashMap::new(), rank: None }))
    })
}

// let vertices = collect_vertex_ids("edges.txt")?
//     .into_iter()
//     .map(|id| BulkVertex { id, label: "person".into(), props: HashMap::new() });
// loader.load_vertices(vertices)?;
// loader.load_edges(stream_edges_from_list("edges.txt"))?;
```

#### 🐍 Python
```python
def collect_vertex_ids(path):
    ids = set()
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) != 2:
                continue
            src, dst = int(parts[0]), int(parts[1])
            ids.add(src)
            ids.add(dst)
    return sorted(ids)


def stream_edges_from_list(path):
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) != 2:
                continue
            src, dst = int(parts[0]), int(parts[1])
            yield BulkEdge(src, dst, "knows")


# vertices = (BulkVertex(vid, "person") for vid in collect_vertex_ids("edges.txt"))
# loader.load_vertices(vertices)
# loader.load_edges(stream_edges_from_list("edges.txt"))
```

> [!WARNING]
> `BulkEdge`'s constructor argument order is `(src, dst, label, props=None, rank=None)` — not `(src, label, dst, ...)`. Passing them in the wrong order fails fast in Python (`dst` ends up a `str` where an `int` is expected), but it's an easy mistake since `label` reads naturally as the "middle" argument.

### Adapting This to Other Offline Formats

The two-pass, iterator-based pattern above isn't specific to whitespace-delimited edge lists — the same shape applies to whatever format your source data actually comes in:

- **CSV / TSV with a header row**: use your language's CSV reader (Rust's `csv` crate, Python's built-in `csv` module) instead of manual `split()` calls, and look up columns by header name instead of position — it's more robust to column reordering and doesn't silently misparse a field containing the delimiter character. The two-pass structure (collect IDs, then stream rows again) is unchanged.
- **JSON Lines (one JSON object per line)**: parse each line independently as its own JSON document inside the same `map`/generator you'd otherwise use for whitespace-split fields — JSON Lines is designed for exactly this kind of line-at-a-time streaming, unlike a single top-level JSON array, which normally has to be parsed as one whole document before you can iterate it (defeating the point of streaming). If your source is a single large JSON array, either convert it to JSON Lines first with a stream-based converter, or reach for a streaming JSON parser that can yield array elements incrementally.
- **Parquet or other columnar formats**: these are built for batch/columnar access, not row-at-a-time streaming, so the natural mapping is "read one row group into memory, then iterate its rows" — the row group takes the place of the per-line string in the examples above. Both `BulkVertex`/`BulkEdge` iterators are happy to be backed by nested loops (outer over row groups, inner over rows within one), as long as the innermost step still yields one record at a time.
- **GraphML / GraphSON / other graph-native exchange formats**: these formats already separate vertices and edges into distinct sections or element types, so you typically don't need the ID-derivation pass described above at all — stream the vertex elements directly into `load_vertices()`, then the edge elements into `load_edges()`, using a streaming XML/JSON parser (rather than a DOM-style "parse the whole tree" parser) to keep memory bounded on large exports.

In every case, the underlying constraint is the same one this guide keeps coming back to: `load_vertices`/`load_edges` accept anything that can produce one `BulkVertex`/`BulkEdge` at a time, so the only real design question for a new format is "how do I get a lazy, one-record-at-a-time iterator out of it" — not "how do I convert it into a `Vec` first."

---

## 6. Vector Indexes & Bulk Loading

If a vector index is declared in the schema before bulk loading, it's built automatically during `loader.commit()` and persisted to disk. If it wasn't declared beforehand, `.nearest()` on that property doesn't error — it silently falls back to an exact brute-force scan (see [Vector Search Deep Dive](vector_search.md#7-vector-search-anti-patterns)). Build the real index after the fact with `graph.index_manager().rebuild(VectorEntityType::Vertex, "embedding_property")` (or `graph.index_manager().rebuild(VectorEntityType.Vertex, "embedding_property")` in Python, `from rocksgraph import VectorEntityType`) before relying on `.nearest()` for performance.

---

## 7. Bulk Loading Best Practices

### Pattern 1: Declare a Strict Schema Before Bulk Loading
Open the database in `SchemaMode::Strict` and declare every vertex/edge label and property key you intend to load — *before* calling `open_bulk_loader()` — rather than letting Auto mode register them implicitly from whatever the first record happens to contain. In Strict mode, a record referencing an undeclared label or property key fails fast with a clear `StoreError::SchemaViolation` naming the offending key, at the point that record is streamed. In Auto mode, a typo'd label (`"Person"` vs. `"person"`) is simply registered as a second, unintended label with no warning — you only discover the split later, at query time.

> [!NOTE]
> BulkLoader strictly validates property value types during ingestion. If a property is declared as `Int64` in the schema (or was registered as `Int64` by the first record that used it in Auto mode), any subsequent record attempting to store a `String` under that key will immediately fail with a `StoreError::SchemaViolation`.

### Pattern 2: Ordered Phase Ingestion (Vertices First, Then Edges)
Always stream and complete all vertices via `load_vertices()` before calling `load_edges()`. The edge ingestion phase relies on the vertex catalog to resolve endpoint vertices and relationships.

### Pattern 3: Size Memory Sorter Buffer to Available RAM
The external sorter uses memory buffers to sort chunks before writing temporary runs. Increasing `with_max_memory` from the default 512 MiB to 1–2 GiB on powerful ingest nodes drastically reduces disk spill runs.

### Pattern 4: Use Dedicated NVMe Scratch Storage
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

### ❌ Anti-Pattern 2: Re-Running a Bulk Load as a "Sync" or Upsert
`BulkLoader` isn't idempotent, and it isn't a merge — it's designed to load into an empty database once. Re-running the same load (or a refreshed export) against a database that already has data doesn't skip or merge existing IDs; every colliding vertex/edge is silently overwritten, with no error and no way to tell afterward which records changed. If you need incremental, repeatable updates — refreshing a dataset on a schedule, syncing from an upstream source — use `TxnSession` with an idempotent upsert built on [`coalesce()`](step_reference.md) (check-then-create, retried on OCC conflict); [`bench_write_occ.rs`](https://github.com/ThouAreAwesome/RocksGraph/blob/main/rocksgraph/src/bin/bench_write_occ.rs) is a complete, working example of this pattern. Reserve `BulkLoader` for the true one-time initial load.

---

## Related Topics

- [Data Model & Type System](data_model.md) — Identifier capacities, rank limits, and types.
- [Performance Tuning](performance.md) — Throughput optimization and batching.
- [Schema Management](schema_management.md) — Pre-declaring schemas for bulk ingestion.
- [Vector Search Deep Dive](vector_search.md) — HNSW index build during bulk loads.
