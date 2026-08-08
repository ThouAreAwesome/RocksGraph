# Design: scan & batch retrieval APIs

## Problem

The `GraphSnapshot` / `GraphTransaction` traits use individual point lookups
(`get_vertex`, `get_edge`) for all data access.  This is adequate for navigation but
creates three gaps:

1. No batching — N point lookups = N RocksDB reads, not 1 `MultiGet`.
2. No stateless pagination for adjacent-edge scans — large neighborhoods require
   the full result set in memory or repeated full re-scans.
3. No full-graph scan primitive — analytical workloads have no safe traversal path.

## Goals & non-goals

- **Goals:** Add `get_vertices`/`get_edges` batch methods (backed by `MultiGet`);
  paginated `get_adjacent_edges` with suffix cursors; `scan_vertices`/`scan_edges`
  for full-graph scans.
- **Non-goals:** Change the volcano step internals (those adapt to the new APIs);
  cursor-based pagination for anything other than adjacency and full-graph scans.

## Design

### New structs

```rust
/// Suffix cursor for paginating edges adjacent to a vertex.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AdjacentEdgeCursor {
    pub label_id: LabelId,
    pub secondary_id: VertexKey,
    pub rank: Rank,
}

/// Optional filters and pagination parameters for adjacent edges.
#[derive(Debug, Clone, Copy)]
pub struct AdjacentEdgesOptions<'a> {
    pub label: Option<LabelId>,
    pub dst: Option<&'a [VertexKey]>,
    pub rank: Option<Rank>,
    pub start_from: Option<AdjacentEdgeCursor>,
}
```

### Trait methods

```rust
trait GraphSnapshot {
    // Batch point lookups
    fn get_vertex(&mut self, key: VertexKey) -> Result<Option<Vertex>, _>;  // default → get_vertices(&[key])
    fn get_vertices(&mut self, keys: &[VertexKey]) -> Result<Vec<Vertex>, _>;
    fn get_edge(&mut self, key: &EdgeKey) -> Result<Option<Edge>, _>;        // default → get_edges(&[key])
    fn get_edges(&mut self, keys: &[EdgeKey]) -> Result<Vec<Edge>, _>;

    // Paginated adjacency
    fn get_adjacent_edges(
        &mut self, vertex: VertexKey, direction: Direction,
        opts: AdjacentEdgesOptions<'_>, limit: Option<u32>,
    ) -> Result<(Vec<Edge>, Option<AdjacentEdgeCursor>), _>;

    // Full-graph scans
    fn scan_vertices(
        &mut self, label: Option<LabelId>, start_from: Option<VertexKey>,
        limit: u32,
    ) -> Result<(Vec<Vertex>, Option<VertexKey>), _>;

    fn scan_edges(
        &mut self, label: Option<LabelId>, start_from: Option<CanonicalEdgeKey>,
        limit: u32,
    ) -> Result<(Vec<Edge>, Option<CanonicalEdgeKey>), _>;
}
```

### Cursor semantics — physical last-returned key with seek-and-skip

Cursors represent the exact physical key of the last returned element:

1. **Inclusive seek** — iterator lands on the `start_from` key.
2. **Seek-and-skip** — if the first element matches `start_from`, skip it and yield
   subsequent elements.  If it doesn't match (cursor element was deleted concurrently),
   yield it as the first element of the new batch.

This is simpler and more robust than "logical next key" incrementing, especially for
compound keys where field-level overflow arithmetic would be needed.

### RocksDB backing

- `get_vertices`/`get_edges` → `db.multi_get_cf` on `vertices` / `edges_out` CFs.
- `get_adjacent_edges` → `edges_out` (OUT) or `edges_in` (IN) CF; `set_iterate_upper_bound`
  prevents scanning past the current vertex's prefix.
- `scan_vertices` → `vertices` CF (optionally via `vertex_labels` index if label supplied).
- `scan_edges` → `edges_out` CF.

### Execution engine integration

- **`VStep` / `EStep`** without input IDs → loop `scan_vertices` / `scan_edges` in batches.
- **`OutEStep` / `InEStep`** → construct `AdjacentEdgesOptions`, cursor-stash in step state.

## Constraints / invariants

- Batch methods omit non-existent keys from results — callers must reconcile if
  they need to know which keys were absent.
- Adjacent-edge pagination respects `set_iterate_upper_bound` — iterator never
  leaks into the next vertex's key range.
