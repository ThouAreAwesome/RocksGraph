# Python Bindings — Pre-Publish Checklist

## Scope for v0.1

v0.1 supports: vertex CRUD, edge CRUD, property filtering, basic traversals
(`out`/`in_`/`both`), aggregations (`count`/`fold`/`unfold`/`sum`/`max`/`min`/`mean`/`degree`), `hasLabel`, `hasId`,
`values`, all predicates, `is()` scalar filter, `order().by()`, `path()`, `as_`/`select`, `coalesce`, `choose`, `local`, `cyclicPath`,
`repeat`/`times`/`until`/`emit`, `union`, `where`, `simplePath`, `drop`,
`limit`/`range`/`skip`/`tail`. Edge traversals (`outE`/`inE`/`bothE`/`inV`/`outV`)
and edge properties are fully tested end-to-end.

59 of 60 opcodes are fully wired in both the Python encoder and the Rust decoder.
The single exception is `OP_ENDVERTEXFILTER` (internal-only, never exposed to users).

**Important constraint**: `addV` requires an explicit `property('id', N)` step.
The engine does not auto-generate vertex IDs. Every `addV` call must supply a
unique integer ID via the preceding property step.

Items marked **[v0.2]** are deferred — blocked by a known engine limitation.

---

## Build & import check

- [x] **macOS (ARM64, Python 3.9)**: `maturin develop` works with
  `/opt/homebrew/bin/maturin`. The `.so` is placed inside
  `bindings/python/rocksgraph/` as `_rocksgraph.cpython-39-darwin.so`.
  No manual copy needed.
- [ ] **Linux / Windows**: Not yet verified. Should work with
  `pip install maturin && maturin develop`.
- [x] `_builder.py` uses `from rocksgraph._rocksgraph import PyGraph`
  (the correct package-relative import).
- [x] `python -c "from rocksgraph import Graph, __, P, Int64"` imports cleanly.

---

## Known codec bugs — ALL FIXED ✅

- [x] **`outE`/`inE`/`bothE` missing rank byte** — fixed in `_codec.py`.
- [x] **`degree()` missing direction byte** — fixed in `_codec.py` and `_builder.py`.
- [x] **Stale docstring on `group()`** — removed.

---

## Known runtime bugs / limitations

- [x] **`next()` returning a full list** — fixed; returns `results[0]` or `None`.

- [x] **`group()`/`groupCount()` `by()` key now implemented** — `GroupStep` and
  `GroupCountStep` in `group.rs` now look up the named property value as the
  grouping key. `test_group_by` and `test_group_count_by` pass.

- [x] **`group()` / `groupCount()` without `by()` on vertex/edge traversers** — fixed
  in `lib.rs`: Map keys for Vertex→`int(id)`, Edge→`tuple(src,dst,label,rank)`,
  both hashable. `test_group_no_by` and `test_group_count_no_by` pass.

- [x] **`fold()`/`unfold()` type compat issue** — resolved; `fold()` returns a
  single-element list containing a list, which is the correct Gremlin semantics.
  `test_fold_unfold_roundtrip` passes.

- [x] **`as_()`/`select()` returns path refs** — resolved; `select()` correctly
  returns vertex/edge values. `test_as_select_roundtrip` passes.

- [x] **`coalesce()` sub-traversal not yet supported** — resolved; `coalesce()`
  works correctly. The previous skip was due to a test bug (passing `None` as a
  traversal argument instead of a real traversal). `test_coalesce_upsert` passes.

- [ ] **Non-zero edge rank is rejected at runtime** — `addE(...).property("rank", N)`
  where `N > 0` raises `UnsupportedOperation("Non-zero rank N is not allowed for
  single-edge relationship")`. Only the default rank (0) is supported, meaning at
  most one edge per label between any two vertices. Multi-edge support is v0.2.

---

## 1. Local build

- [x] `cd bindings/python && maturin develop` (macOS ARM64, maturin 1.14.1)
- [x] `python -c "from rocksgraph._rocksgraph import PyGraph"` works
- [x] `python -c "from rocksgraph import Graph, __, P, Int64"` imports cleanly

---

## 2. Codec unit tests — 26/26 pass ✅

- [x] **`test_eq_encodes_tag_byte` index arithmetic corrected** — from `buf[idx + 5]`
  to `buf[idx + 3]`.

Verified passing:

- [x] All 9 predicate tags (`eq`→`0x00` through `without`→`0x08`)
- [x] `OP_LIMIT`, `OP_SKIP`, `OP_TAIL` encode as signed i64
- [x] `OP_RANGE` encodes as two signed i64s
- [x] `OP_REPEAT` times encodes as signed i64
- [x] `OP_HASPROPERTY` encodes key string then predicate
- [x] `OP_HASLABEL` encodes a predicate (not a label list)
- [x] `OP_OUTE`/`OP_INE`/`OP_BOTHE` are 1 byte longer than vertex traversals (rank byte)
- [x] `order().by()` does not mutate a cloned traversal
- [x] `_vertex_id({"id": 7})` returns `7`; `_vertex_id(42)` returns `42`
- [x] `degree()` → direction `0` (Both); `degree("out")` → `1`; `degree("in")` → `2`

---

## 3. Property type round-trips — 18/18 pass ✅

- [x] `Int32` — positive, negative, `-(2**31)`, `2**31 - 1`
- [x] `Int64` — `-(2**63)`, `2**63 - 1`
- [x] `UInt16` — `0`, `65535`
- [x] `Float32` — within float32 epsilon
- [x] `Float64` — including `1e300`
- [x] `str` — ASCII and multi-byte UTF-8
- [x] `bool` — `True` and `False`
- [x] `Uuid` — write with `Uuid("xxxxxxxx-xxxx-...")`, read back matches
- [x] `bytes` — arbitrary byte sequence
- [x] Plain Python `int` (falls back to Int64)
- [x] Plain Python `float` (falls back to Float64)

---

## 4. Predicate correctness — 9/9 pass ✅

- [x] `P.gt`, `P.gte`, `P.lt`, `P.lte`, `P.between`, `P.within`, `P.without`,
  `P.neq`, `P.eq` — all return the correct filtered result set

---

## 5. `has()` existence check ✅

- [x] `has("email")` matches only vertices that have an `email` property
- [x] `has("email")` does NOT match a vertex where `email` was never set
- [x] `has("weight")` existence check on edge properties (`TestHasOnEdge` passes)

---

## 6. Transaction semantics — 4/4 pass ✅

- [x] `commit()` persists data visible to a new read session
- [x] `rollback()` discards writes
- [x] Double `commit()` behaviour verified
- [x] Snapshot isolation verified (read session opened before commit does not see new data)

---

## 7. Persistence across reopen — 4/4 pass ✅

- [x] Vertex data survives a `del graph` / reopen cycle
- [x] All property types survive reopen (`test_all_types_survive_reopen`)
- [x] Vertex IDs are stable across reopen (`test_ids_stable_across_reopen`)
- [x] Edge data (src, dst, label, properties) survives reopen (`test_edge_survives_reopen`)

---

## 8. Graph traversal correctness

### 8a. Vertex traversals — all v0.1 features pass ✅

- [x] `addV().property(...).next()` returns a dict with `"id"`, `"label"`,
  `"properties"` keys
- [x] `V(id)` fetches the exact vertex by ID
- [x] `out("label")` traverses to neighbours
- [x] `in_("label")` — `TestInBothTraversals::test_in_traversal` passes
- [x] `both("label")` — `TestInBothTraversals::test_both_traversal` passes
- [x] `values("key")` returns a flat list of values
- [x] `order().by("key", "asc")` and `order().by("key", "desc")`
- [x] `order().by("key1", "asc").by("key2", "desc")` multi-key sort
- [x] `limit(n)`, `range(lo, hi)`, `skip(n)`
- [x] `tail(n)` — `TestTail::test_tail` passes
- [x] `count()` returns an integer
- [x] `dedup()` removes duplicates
- [x] `fold()` collects all traversers into a single list
- [x] `unfold()` emits each element individually
- [x] `path()` — `TestPath::test_path` passes
- [x] `as_("x").select("x")` — `test_as_select_roundtrip` passes
- [x] `coalesce(t1, t2)` — short-circuits to first successful branch
- [x] `union(t1, t2)` — `TestUnion::test_union` passes
- [x] `repeat(__.out()).times(N)` — `TestRepeat::test_repeat_out` passes
- [x] `where(sub_traversal)` — `TestV02SubTraversals::test_where` passes
- [x] `simplePath()` — `TestV02SubTraversals::test_simplePath` passes
- [x] `has("label", "key", value)` 3-arg form
- [x] `drop()` removes a vertex
- [x] `group().by("key")` — `test_group_by` passes; Rust `GroupStep` now correctly
  looks up property values as grouping keys.
- [x] `groupCount().by("key")` — same fix; `GroupCountStep` also implemented.

**All wired steps now runtime-tested ✅:**

- [x] `repeat().until(sub_traversal)` — `test_repeat_until` passes
- [x] `repeat().emit()` — `test_repeat_emit` passes
- [x] `degree()` — 3 runtime tests (default/out/in) pass
- [x] `sum()` / `max()` / `min()` / `mean()` — `TestAggregations` passes
- [x] `cyclicPath()` — `test_cyclicPath` passes
- [x] `choose(pred, true_t, false_t)` — `test_choose` passes
- [x] `local(sub_traversal)` — `test_local` passes

### 8b. Edge traversals — all confirmed working ✅

- [x] `addE("label").from_(v1).to(v2)` — confirmed working
- [x] `outE("label")` returns edge dicts with `"src"`, `"dst"`, `"label"`, `"rank"`, `"properties"`
- [x] `outE("label").inV()` produces destination vertices
- [x] `inE("label").outV()` produces source vertices
- [x] `bothE("label")` returns edges in both directions
- [x] Edge properties round-trip correctly
- [x] `has("key")` existence check on edge traversers

### 8c. Not targeted for v0.1 — engine limitation [v0.2]

- [ ] [v0.2] `addE(...).property("rank", N)` with N > 0 — engine rejects at runtime;
  requires multi-edge support
- [ ] [v0.2] `hasRank(P.eq(N))` — depends on non-zero rank support above

---

## 9. Edge rank [v0.2]

Engine limitation: non-zero rank is rejected at runtime
(`"Non-zero rank N is not allowed for single-edge relationship"`).
Only one edge per label between any two vertices is allowed (rank=0 default).

- [ ] [v0.2] `addE(...).property("rank", n)` stores rank correctly
- [ ] [v0.2] `hasRank(P.eq(n))` filters edges by rank
- [ ] [v0.2] Rank survives a close/reopen cycle

---

## 10. Test infrastructure

- [x] `tests/__init__.py` exists — `from tests.conftest import addv` works correctly.
- [x] `test_eq_encodes_tag_byte` fixed (see §2).
- [x] Module docstrings at line 1 in all test files.

---

## 11. Documentation

- [x] `README.md` exists with quickstart, data model, session model, type system,
  full step reference, and common patterns. Quickstart verified to run correctly.
- [ ] Quickstart must run without errors against the **published wheel** (can only
  verify after TestPyPI/PyPI release)
- [x] `pyproject.toml` has `readme = "README.md"`
- [x] `rocksgraph/__init__.pyi` type stubs exist — covers all public classes
  (`Graph`, `ReadSession`, `TxSession`, `Traversal`, `__`, `P`, typed wrappers)
- [ ] [v0.2] Map `StoreError` variants to a `rocksgraph.StoreError` Python exception

---

## 12. CI verification

- [x] `python-tests` job (ubuntu + macos) added to `ci.yml`; runs `maturin develop && pytest tests/`
- [x] `python-windows-ci` job updated to run `maturin develop && pytest tests/`
- [ ] Verify CI passes on all platforms (requires push to GitHub)

---

## 13. Release workflow fixes (before first publish)

- [x] `python-release.yml` uses `PyO3/maturin-action@v1` with `manylinux: auto`
- [x] `aarch64-unknown-linux-gnu` added to build matrix
- [ ] Consider `pyo3/abi3-py39` for one wheel per platform (optional optimization)
- [x] `test-wheels` job installs built wheels and runs a smoke import test
- [x] `publish-pypi` and `publish-testpypi` list `test-wheels` in `needs:`

---

## 14. TestPyPI dry run

- [ ] Version in `bindings/python/Cargo.toml` and `pyproject.toml` match
- [ ] Trigger `python-release.yml` via `workflow_dispatch` with `testpypi: yes`
- [ ] All target wheels appear as artifacts
- [ ] `pip install rocksgraph --index-url https://test.pypi.org/simple/` works on a
  clean machine
- [ ] Full test suite passes against the TestPyPI-installed wheel

---

## 15. Final publish

- [ ] Update or create `CHANGELOG.md` for the Python package
- [ ] Bump version in both `bindings/python/Cargo.toml` and `pyproject.toml`
- [ ] Push a `v*` tag — `python-release.yml` publishes to PyPI automatically
- [ ] Verify the PyPI project page renders the README correctly
- [ ] `pip install rocksgraph` works on a fresh machine
- [ ] Run the full test suite one more time against the published wheel

---

## Test summary — 142 passed, 3 skipped

The 5 skipped tests fall into two categories:

| Test | Reason | Root cause |
|------|---------|------------|
| `test_adde_with_rank` | Non-zero rank rejected by engine | Engine: single-edge only |
| `test_hasRank` | Depends on non-zero rank support | Engine: single-edge only |
| `test_hasRank_not_eq` | Depends on non-zero rank support | Engine: single-edge only |

**Sharpest v0.1 user-facing limitations:**

1. **`addV` requires explicit vertex ID** — every `addV` must be followed by
   `.property("id", N)` before execution. The error is
   `TraversalError("AddVStep cannot be built without a vertex ID")`.

2. **Multi-edge (rank > 0) is unsupported** — only one edge per label between any
   two vertices is allowed. Non-zero rank raises `UnsupportedOperation` at runtime.

---

## Rust features not yet exposed in Python bindings

These exist in the Rust API but are not reachable from Python (no FFI, no bytecode,
or no builder method). Listed in rough priority order.

### Need new FFI (not a single opcode)

| Feature | Rust API | Effort |
|---------|----------|:------:|
| `explain()` | returns physical plan tree string | small |
| `iter()` lazy traversal | returns Iterator | medium |
| `Graph.open_with_options()` | schema mode, edge mode | medium |
| `Graph.statistics()` | RocksDB stats | small |
| `set_batch_size()` / `clear_caches()` | performance tuning | small |
| Schema management | `open_schema()` | large |
| Bulk loading | `SstBulkLoader` | large |

### Need builder method

(All builder methods are now implemented — see scope above.)
