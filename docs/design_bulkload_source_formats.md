# Design: Bulk Load Source Format Abstraction

Status: partially implemented — `EdgeListSource` (SNAP format) is shipped with lazy
streaming.  The `BulkSource` trait and additional format adapters remain proposals.

## Problem

`BulkLoader` (opened via `graph.open_bulk_loader()`) accepts vertex and edge
iterators — the universal protocol for bulk data ingestion:

```rust
let mut loader = graph.open_bulk_loader()?;
loader.load_vertices(vertex_iter);
loader.load_edges(edge_iter);
loader.commit()?;
```

But every caller must write its own file-parsing code to produce those iterators.
Different datasets ship in different formats — the soc-LiveJournal edge list is one
format; real-world pipelines also produce GraphSON, CSV, JSON Lines, and adjacency
lists.  Without a shared abstraction, format parsing is reimplemented per dataset
and per tool.

```
Format adapter                    BulkLoader (graph session)
─────────────                     ─────────────────────────

EdgeListSource::open("snap.txt")  
  │                               
  ├─ scan edge list once          
  │  ├─ collect unique vertex IDs 
  │  └─ build edge records        
  │                               
  ├─ vertices() → iter<BulkVertex> → load_vertices() writes vertex SST
  └─ edges()    → iter<BulkEdge>   → load_edges()    writes edge SSTs
```

Every format adapter decomposes into two iterators. `BulkLoader` processes them
in order (vertices first to build the label file, then edges annotated with
dst vertex labels) — the adapter handles the format-specific extraction; the
bulk loader handles the storage pipeline.

## Goals & non-goals

- **Goal:** define a `BulkSource` trait that any file format can implement to
  produce the vertex and edge iterators consumed by `BulkLoader`.
- **Goal:** ship format implementations for the most common graph dataset formats.
- **Goal:** schema can come from the file (auto-detected) or from the caller
  (explicitly declared) — both paths must be supported. The schema is
  read from the `Graph` handle at `open_bulk_loader()` time; adapters
  for formats without embedded schema produce a minimal declaration.
- **Non-goal:** general-purpose ETL / streaming pipeline framework.
- **Non-goal:** Gremlin-level traversal during import.

## `BulkSource` trait

```rust
/// A source of graph data that can produce a schema declaration and
/// two lazy iterators of vertices and edges for `BulkLoader`.
pub trait BulkSource {
    type VertexIter: Iterator<Item = Result<BulkVertex, BulkSourceError>>;
    type EdgeIter:   Iterator<Item = Result<BulkEdge,   BulkSourceError>>;

    /// Produce the schema, vertex stream, and edge stream.
    ///
    /// Called once.  For formats that embed schema information (GraphSON,
    /// JSON), this scans the file to extract labels and property types.
    /// For formats without schema (edge list), returns a minimal schema
    /// derived from context or explicit configuration.
    fn open(self) -> Result<(BulkSchema, Self::VertexIter, Self::EdgeIter), BulkSourceError>;
}
```

`BulkLoader` accepts any `BulkSource` via a convenience method.

`BulkLoader` is opened as a session on a `Graph`:

```rust
let mut loader = graph.open_bulk_loader()?;

// Option 1: load iterators directly
loader.load_vertices(vertex_iter);
loader.load_edges(edge_iter);

// Option 2: load from a BulkSource adapter
loader.load_from_source(edge_list_source)?;

let stats = loader.commit()?;
```

`load_from_source` calls `source.open()` and feeds the returned iterators
into the pipeline.  `commit()` processes vertices first (to build the label
file), then edges (annotated with dst vertex labels), writes SST files,
and ingests them atomically.

## Format implementations

### 1. `EdgeListSource` — SNAP / soc-LiveJournal format

```
# comment lines start with #
0 1
0 2
1 3
...
```

One directed edge per line: `src_id<sep>dst_id`.  No vertex records, no properties,
no schema in the file.  Vertices are inferred from edge endpoints.

**Schema**: caller must provide vertex label, edge label, and property key
declarations (or use defaults — single label for all vertices/edges, no properties).

**Two-pass**: Pass 1 collects all unique vertex IDs; Pass 2 streams edges.

```rust
pub struct EdgeListSource {
    path:         PathBuf,
    separator:    char,          // default: whitespace
    vertex_label: String,        // default: "Vertex"
    edge_label:   String,        // default: "Edge"
    comment_char: char,          // default: '#'
    weighted:     bool,          // if true, third column is "weight" f64
}

impl EdgeListSource {
    pub fn new(path: &Path) -> Self;
    pub fn with_separator(mut self, sep: char) -> Self;
    pub fn with_labels(mut self, vertex: &str, edge: &str) -> Self;
    pub fn with_weighted(mut self) -> Self;
}
```

### 2. `WeightedEdgeListSource` — shorthand for `EdgeListSource::new(p).with_weighted()`

### 3. `CsvEdgeSource` — CSV edge table with named columns

```csv
src,dst,weight,timestamp
0,1,0.5,1620000000
0,2,0.8,1620000001
```

Column mapping is configured; any column can be mapped to a property.
Vertex records may come from a separate CSV file.

```rust
pub struct CsvEdgeSource {
    edge_path:    PathBuf,
    vertex_path:  Option<PathBuf>,  // optional separate vertex CSV
    src_col:      String,
    dst_col:      String,
    vertex_label: String,
    edge_label:   String,
    /// Remaining columns become edge properties; caller declares their DataType.
    prop_cols:    Vec<(String, DataType)>,
}
```

### 4. `JsonLinesSource` — one vertex or edge object per line (JSON Lines / NDJSON)

```jsonl
{"type":"vertex","id":1,"label":"person","name":"Alice","age":30}
{"type":"vertex","id":2,"label":"software","name":"ripple"}
{"type":"edge","src":1,"dst":2,"label":"created","weight":0.4}
```

Schema is inferred from the first N lines (configurable).  Property types are
inferred from JSON value types (`number` → `Float64` or `Int64` depending on
whether fractional, `string` → `String`, `boolean` → `Bool`).

```rust
pub struct JsonLinesSource {
    path:           PathBuf,
    schema_scan_n:  usize,   // lines to scan for type inference; default 10_000
    /// Override inferred types for specific properties.
    type_overrides: HashMap<String, DataType>,
}
```

### 5. `GraphSONSource` — TinkerPop GraphSON v3

GraphSON v3 is a JSON format used by Apache TinkerPop.  It is the standard
interchange format for Gremlin-compatible graph databases.

```json
{
  "@type": "tinker:graph",
  "@value": {
    "vertices": [
      {"@type": "g:Vertex", "@value": {"id": 1, "label": "person",
        "properties": {"name": [{"@value": {"value": "Alice"}}]}}}
    ],
    "edges": [
      {"@type": "g:Edge", "@value": {"id": 0, "label": "knows",
        "inV": 2, "outV": 1, "properties": {}}}
    ]
  }
}
```

Schema is fully embedded in the file (vertex labels, edge labels, property
names).  Property types are inferred from the first occurrence of each property.

```rust
pub struct GraphSONSource {
    path:           PathBuf,
    type_overrides: HashMap<String, DataType>,
}
```

### 6. `AdjacencyListSource` — adjacency list (each line: vertex + its neighbors)

```
0: 1 2 3
1: 0 2
2: 0 1 3
```

Each line is one vertex followed by its out-neighbors.  No properties, no labels
unless configured.

```rust
pub struct AdjacencyListSource {
    path:         PathBuf,
    vertex_label: String,
    edge_label:   String,
}
```

## Schema: inferred vs. declared

| Format | Schema source | Caller action required |
|---|---|---|
| Edge list | none in file | declare vertex label, edge label, optional props |
| Weighted edge list | none in file | same + declare `weight: Float64` |
| CSV edges | header row for names; types from config | declare `prop_cols` with types |
| JSON Lines | inferred from first N lines | optionally override inferred types |
| GraphSON | fully embedded | none (optionally override types) |
| Adjacency list | none in file | declare vertex label, edge label |

For formats with type inference, the inferred schema is returned from `open()` and
is visible to the caller before any data is loaded.  The caller can inspect it and
reject or adjust types before proceeding.

## Two-pass formats

Some formats (edge list, adjacency list) require two passes because vertex records
must be emitted before edges, but the file only contains edges:

- **Pass 1**: Collect unique vertex IDs from the edge stream.
- **Pass 2**: Emit synthetic `BulkVertex { id, label, props: {} }` for each unique
  vertex, then stream edges.

The `EdgeListSource` and `AdjacencyListSource` handle this internally.  For large
files that do not fit in memory, the two-pass is implemented with external sort on
vertex IDs (same infrastructure as the degree computation pass in the internal `BulkLoader` pipeline).

## Interaction with `SchemaMode` and `EdgeMode`

`BulkSource` implementations produce raw `BulkVertex`/`BulkEdge` records without
knowledge of schema or edge mode.  Mode enforcement is entirely the responsibility
of `BulkLoader`:

- **`SchemaMode::Strict`**: `BulkLoader` validates every label and property key
  name against the schema read from the `Graph` handle.  A `BulkSource` that
  produces an undeclared label will cause `load_vertices()`/`load_edges()` to abort with
  `StoreError::SchemaViolation` before the current phase writes its SSTs.
- **`SchemaMode::Auto`**: unknown labels encountered in the `BulkSource` output are
  automatically registered during processing.
- **`EdgeMode::Single`**: duplicate `(src, dst, label)` edges emitted by a
  `BulkSource` are rejected at the SST write pass with `StoreError::DuplicateEdge`.
- **`EdgeMode::Multi`**: `BulkLoader` assigns ranks; `BulkEdge::rank` may be
  `None` (auto-assign) or `Some(r)` (explicit, for sources that carry rank info).

Format implementations that *know* they may produce duplicates (e.g., a
`GraphSONSource` reading a multigraph export) should document this in their
`BulkEdge::rank` handling.

## `BulkSourceError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum BulkSourceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error at line {line}: {msg}")]
    Parse { line: u64, msg: String },
    #[error("schema conflict: property '{key}' seen as {t1} and {t2}")]
    TypeConflict { key: String, t1: DataType, t2: DataType },
    #[error("missing required column: {0}")]
    MissingColumn(String),
}
```

## Files

| File | Contents |
|---|---|
| `src/store/rocks/bulk_source.rs` | `BulkSource` trait, `BulkSourceError` |
| `src/store/rocks/bulk_source_edge_list.rs` | `EdgeListSource` |
| `src/store/rocks/bulk_source_csv.rs` | `CsvEdgeSource` |
| `src/store/rocks/bulk_source_json.rs` | `JsonLinesSource` |
| `src/store/rocks/bulk_source_graphson.rs` | `GraphSONSource` |
| `src/store/rocks/bulk_source_adjlist.rs` | `AdjacencyListSource` |

## Implementation plan

### Step 1 — `BulkSource` trait + `EdgeListSource` (~200 lines)

Implement the trait and the simplest format (SNAP edge list).  Wire into
`bench_write.rs` to replace the current hard-coded parser.

### Step 2 — `CsvEdgeSource` + `JsonLinesSource` (~300 lines)

CSV covers most tabular export formats (Neo4j, SQL dumps).  JSON Lines covers
most REST API exports and modern data pipeline outputs.

### Step 3 — `GraphSONSource` (~300 lines)

Enables direct import of data exported from any TinkerPop-compatible graph
database (JanusGraph, Amazon Neptune, etc.).

### Step 4 — `AdjacencyListSource` (~100 lines)

Covers classic adjacency list format used in many academic graph datasets.

## Test plan

### Format correctness

For each source implementation:
- Load a small known graph (10 vertices, 20 edges).
- Run `g.V().count()`, `g.E([]).count()`, verify property values.

### Schema inference

- `JsonLinesSource`: verify inferred types match actual JSON value types.
- `GraphSONSource`: verify labels and properties round-trip correctly.
- `EdgeListSource`: verify synthetic vertices are created for all unique IDs.

### Large file streaming

- 69 M edge `EdgeListSource`: verify peak memory stays bounded
  (no full file loaded into memory).

### Error handling

- Malformed JSON line → `BulkSourceError::Parse` with correct line number.
- CSV missing required column → `BulkSourceError::MissingColumn`.
- Type conflict across JSON objects → `BulkSourceError::TypeConflict`.
