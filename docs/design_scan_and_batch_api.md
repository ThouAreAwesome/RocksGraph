# Design Proposal: Scan and Batch Retrieval APIs (Refined)

This document outlines the refined design to support batch lookups, stateless pagination, and full-graph scanning in the `GraphStore` / `GraphTransaction` / `GraphSnapshot` traits.

---

## 1. Core Objectives
1. **Batching for Performance**: Replace individual point lookups (`get_vertex`, `get_edge`) with batch versions (`get_vertices`, `get_edges`) returning flat vectors (`Vec<Vertex>`, `Vec<Edge>`) to optimize I/O via RocksDB's `MultiGet`.
2. **Stateless Pagination & Clean Parameters**: Expose `vertex` and `direction` as direct parameters, wrap optional filters/cursors into an options struct (`AdjacentEdgesOptions`), and utilize suffix-based cursor pagination (`AdjacentEdgeCursor`).
3. **Full-Graph Scans**: Provide batch-based, paginated scanning of all vertices and edges in the database, allowing large analytical scans to execute safely.

---

## 2. API Specification

We will update the `GraphSnapshot` and `GraphTransaction` traits with the following structures and methods.

### New Structs

```rust
/// Suffix cursor for paginating edges adjacent to a specific vertex.
/// Represents the physical key of the last returned edge (specifically, the suffix parts).
/// Ordered exactly as the database sorting keys: (label_id, secondary_id, rank).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AdjacentEdgeCursor {
    pub label_id: LabelId,
    pub secondary_id: VertexKey,
    pub rank: Rank,
}

impl AdjacentEdgeCursor {
    /// Create a cursor from an existing Edge.
    pub fn from_edge(edge: &Edge, direction: Direction) -> Self {
        AdjacentEdgeCursor {
            label_id: edge.label_id,
            secondary_id: match direction {
                Direction::OUT => edge.dst_id,
                Direction::IN => edge.src_id,
            },
            rank: edge.rank,
        }
    }
}

/// Optional filters and pagination parameters for querying adjacent edges.
#[derive(Debug, Clone, Copy)]
pub struct AdjacentEdgesOptions<'a> {
    pub label: Option<LabelId>,
    pub dst: Option<&'a [VertexKey]>,
    pub rank: Option<Rank>,
    pub start_from: Option<AdjacentEdgeCursor>,
}
```

### Trait Interfaces

```rust
pub trait GraphSnapshot {
    // ── Single & Batch Reads ──────────────────────────────────────────────────

    /// Fetch a single vertex.
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, StoreError> {
        let results = self.get_vertices(&[key])?;
        Ok(results.into_iter().next())
    }

    /// Fetch a list of vertices in batch (omitting any keys that were not found).
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<Vertex>, StoreError>;

    /// Fetch a single edge.
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<Edge>, StoreError> {
        let results = self.get_edges(&[key.clone()])?;
        Ok(results.into_iter().next())
    }

    /// Fetch a list of edges in batch (omitting any keys that were not found).
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<Edge>, StoreError>;

    // ── Adjacent Edge Traversals ──────────────────────────────────────────────

    /// Scan committed edges adjacent to a vertex with options and stateless pagination.
    ///
    /// - `vertex` and `direction` remain direct parameters in the method signature.
    /// - Returns the fetched edges and the cursor representing the last returned edge (if more exist).
    fn get_adjacent_edges(
        &mut self,
        vertex: VertexKey,
        direction: Direction,
        opts: AdjacentEdgesOptions<'_>,
        limit: Option<u32>,
    ) -> Result<(Vec<Edge>, Option<AdjacentEdgeCursor>), StoreError>;

    // ── Full-Graph Scans ──────────────────────────────────────────────────────

    /// Scan all vertices in the database in batch mode.
    ///
    /// - `label`: optionally restrict to vertices with this label.
    /// - `start_from`: inclusive starting VertexKey (represents the last seen vertex key).
    /// - `limit`: maximum number of vertices to return in this batch.
    /// Returns the batch and the cursor for the last returned vertex (if more exist).
    fn scan_vertices(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<Vertex>, Option<VertexKey>), StoreError>;

    /// Scan all unique canonical edges (OUT direction index) in the database in batch mode.
    ///
    /// - `label`: optionally restrict to edges with this label.
    /// - `start_from`: inclusive starting CanonicalEdgeKey (represents the last seen edge key).
    /// - `limit`: maximum number of edges to return in this batch.
    /// Returns the batch and the cursor for the last returned edge (if more exist).
    fn scan_edges(
        &mut self,
        label: Option<LabelId>,
        start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<Edge>, Option<CanonicalEdgeKey>), StoreError>;
}
```

*Note: The `GraphTransaction` trait will replicate these read interfaces alongside its mutation operations.*

---

## 3. Key Behavioral Contracts & Cursor Semantics

### Physical Last-Returned Key with Seek-and-Skip
To achieve maximum robustness and simplicity under concurrent mutation, cursors (`VertexKey`, `AdjacentEdgeCursor`, `CanonicalEdgeKey`) represent the **exact physical key of the last returned element** of the batch:

1. **Inclusive Seek**: When querying the next batch, the backend seeks the database iterator directly to the `start_from` cursor key.
2. **Seek-and-Skip Logic**:
   - Because seek is inclusive, the iterator will land exactly on the last returned element (if it still exists).
   - The backend checks if the first element returned by the iterator matches the `start_from` key.
   - If it **matches**, it is skipped, and iteration proceeds to yield the subsequent elements.
   - If it **does not match** (meaning the cursor element was deleted concurrently in another transaction), the iterator has landed on the next lexicographically existing element. In this case, we **do not skip** it, and yield it as the first element of the new batch.

#### Why this is superior to "Logical Next Key" (Incremented Cursors):
- For simple keys (like `VertexKey`/`i64`), incrementing by 1 (`last_key + 1`) is trivial.
- However, for compound keys (like `AdjacentEdgeCursor` or `CanonicalEdgeKey`), incrementing the key logically is complex and error-prone because it requires handling numeric overflows across multiple fields (e.g. `rank` overflow -> increment `secondary_id` -> increment `label_id`).
- Using the **Physical Last-Returned Key with Seek-and-Skip** guarantees that pagination behaves correctly, safely, and identically across all key types without needing key-increment arithmetic.

---

## 4. RocksDB Implementation Details

### `get_vertices` & `get_edges`
- Will utilize `db.multi_get_cf` against the `vertices` and `edges_out` column families respectively. 
- Results are returned as a flat vector `Vec<T>`, matching the behavior of the administrative lookup tooling and omitting non-existent items.

### `get_adjacent_edges` (Stateless Pagination)
- Targets either `edges_out` (for `Direction::OUT`) or `edges_in` (for `Direction::IN`).
- Seek is performed by constructing the physical prefix key from `(vertex_id)` and combining it with the `AdjacentEdgeCursor` suffix fields `(label_id, secondary_id, rank)` if `start_from` is supplied.
- Uses `ReadOptions::set_iterate_upper_bound` to prevent the iterator from scanning past the current vertex's prefix range.

### `scan_vertices`
- Scans the `vertices` column family.
- If `label` is provided, we can leverage the secondary index `vertex_labels` to only scan matching keys, or perform a direct range scan if the index exists.
- Seek is performed from the `start_from` key.

### `scan_edges`
- Scans the `edges_out` (canonical OUT direction) column family.
- Seek is performed from the `start_from` canonical key.

---

## 5. Execution Engine Integration

The volcano physical steps will be updated to consume these paginated APIs:
- **`VStep` / `EStep`**: When seeding without input IDs (e.g., `g.V()`), these steps will loop using `scan_vertices` / `scan_edges` fetching batches (e.g., 1000 items) sequentially.
- **`OutEStep` / `InEStep`**: When expanding edges, they will construct `AdjacentEdgesOptions` and call `get_adjacent_edges`. If an adjacent edge scan exceeds the limits or needs lazy pagination, it stores the `AdjacentEdgeCursor` within the Volcano step state, resuming the iteration on the next `pull()` call.
