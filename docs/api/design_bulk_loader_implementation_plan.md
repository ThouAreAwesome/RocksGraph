# Implementation Plan: Storage-Agnostic Bulk Loader API

Status: proposal.

---

## Current state

```
User → SstBulkLoader::new(db_path, work_dir)  ← standalone, knows about SST
         .load_initial(schema, vertices, edges, opts)
```

Leaks: `Sst` in name, user manages `work_dir`, user passes `BulkSchema` separately,
not integrated with `Graph`, `RocksOptions` required even for defaults.

## Target state

```
User → graph.open_bulk_loader()               ← session on Graph
         .load_vertices(iter)
         .load_edges(iter)
         .commit()
```

Hides: SST is pipeline detail, `work_dir` auto-managed, schema read from graph,
`RocksOptions` inherited from graph open. `SstBulkLoader` remains internal.

---

## Implementation plan

### Phase 1: Internal rename (zero user impact)

| Step | What | File | Effort |
|------|------|------|:---:|
| 1.1 | Rename `SstBulkLoader` → `BulkLoadPipeline` (internal, not exported) | `bulk_loader.rs` | 5 min |
| 1.2 | Keep `SstBulkLoader` as a deprecated type alias pointing to `BulkLoadPipeline` | `bulk_loader.rs` | 1 line |
| 1.3 | Rename file: `bulk_loader.rs` stays (it's already the right name) | — | — |

### Phase 2: Add Graph-based session

| Step | What | File | Effort |
|------|------|------|:---:|
| 2.1 | Add `BulkLoader` session struct wrapping `BulkLoadPipeline` | `src/store/rocks/bulk_loader.rs` | ~30 lines |
| 2.2 | `BulkLoader::new(graph: &Graph, work_dir: Option<PathBuf>)` — reads schema from graph, auto-creates work_dir at `{db_path}/_bulk_work` if not provided | same | ~30 lines |
| 2.3 | `BulkLoader::load_vertices(&mut self, iter)` → pass-through to `BulkLoadPipeline` | same | ~5 lines |
| 2.4 | `BulkLoader::load_edges(&mut self, iter)` → pass-through | same | ~5 lines |
| 2.5 | `BulkLoader::commit(self) → BulkLoadStats` — runs pipeline, writes crash marker, calls `IngestExternalFile`, cleans work_dir | same | ~15 lines |
| 2.6 | `Drop for BulkLoader` — cleans work_dir if commit wasn't called | same | ~10 lines |
| 2.7 | Add `Graph::open_bulk_loader(&self, work_dir: Option<PathBuf>) → BulkLoader` | `src/api.rs` | ~5 lines |

### Phase 3: Migrate `BulkSource` trait

| Step | What | File | Effort |
|------|------|------|:---:|
| 3.1 | Change `BulkSource` trait to refer to `BulkLoader`, not `SstBulkLoader` | `src/store/rocks/bulk_source.rs` | ~5 lines |
| 3.2 | Add `BulkLoader::load_from_source(source: impl BulkSource)` convenience | `bulk_loader.rs` | ~10 lines |

### Phase 4: Public exports

| Step | What | File | Effort |
|------|------|------|:---:|
| 4.1 | Add `pub use BulkLoader, BulkVertex, BulkEdge, BulkLoadStats` | `src/lib.rs` | 1 line |
| 4.2 | Mark `SstBulkLoader` as `#[deprecated]`, point to `BulkLoader` | `src/lib.rs` | 1 attribute |
| 4.3 | Keep `BulkLoadPipeline` private (`pub(crate)`), not exported | `src/lib.rs` | — |

### Phase 5: Docs

| Step | What | Effort |
|------|------|:---:|
| 5.1 | Replace `rocksgraph/README.md` §Bulk Load example with `graph.open_bulk_loader()` | 10 lines |
| 5.2 | Move SST details in README to a collapsible "How it works" footnote | 5 lines |
| 5.3 | Add header note to `docs/design_bulkload_sst_ingest.md`: "API superseded — see `docs/api/design_bulk_loader.md`" | 2 lines |
| 5.4 | Update `docs/design_bulkload_source_formats.md` to reference `BulkLoader` not `SstBulkLoader` | 10 lines |
| 5.5 | Update `bench_write.rs` to use new API (both old and new for comparison) | ~10 lines |

### Phase 6: Python/JS bindings (future)

| Step | What | Effort |
|------|------|:---:|
| 6.1 | Expose `graph.open_bulk_loader()` as `BulkLoader` Python class with `__enter__`/`__exit__` | ~40 lines |
| 6.2 | Expose `BulkLoader` to Node.js via napi-rs | ~40 lines |

---

## Migration window

`SstBulkLoader::new(db_path, work_dir).load_initial(...)` remains functional with
a deprecation warning for one minor version (v0.3 deprecation → v0.4 removal).
All benchmarks and CI switch to `graph.open_bulk_loader()` in the same PR.
