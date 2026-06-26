# Public Release Checklist

> Generated: 2026-06-25. Covers the gaps between RocksGraph v0.1.0 (beta) and a
> credible public release on crates.io / GitHub.

---

## Table of Contents

1. [P0 — Release Blockers](#p0--release-blockers)
2. [P1 — Feature Gaps for Credibility](#p1--feature-gaps-for-credibility)
3. [P2 — Polish & Trust Signals](#p2--polish--trust-signals)
4. [P3 — Nice-to-Have](#p3--nice-to-have)
5. [Suggested Implementation Order](#suggested-implementation-order)

---
---

### 5. Missing `rust-version` in Cargo.toml

**Issue:** No `rust-version` field. crates.io displays "Requires Rust ≥ ???" — left blank
it looks unmaintained. Downstream users need to know if their toolchain is compatible.

**Solution:** Add to `Cargo.toml`:
```toml
rust-version = "1.80"
```
(Choose the actual MSRV based on what the project compiles with. Run `cargo msrv` or
manually test with `rustup run 1.XX cargo check` to determine the real floor.)


---

### 7. No MSRV policy documented

**Issue:** README and docs don't state what Rust version is required, or what the upgrade
policy is.

**Solution:** Add an "MSRV Policy" section to README.md:
```markdown
## Minimum Supported Rust Version

RocksGraph targets stable Rust. The MSRV is Rust 1.80. Bumping the MSRV is
considered a minor (not patch) change.
```

---

## P1 — Feature Gaps for Credibility

These are Gremlin steps or capabilities that users will expect from a graph database
traversal engine. Their absence makes the product feel incomplete.



### 14. No safety / `unsafe` policy

**Issue:** Graph databases handle raw byte encoding and RocksDB FFI. Users need to know
whether `unsafe` is used and what guarantees apply.

**Solution:**
- Add `#![forbid(unsafe_code)]` to `lib.rs` if the crate (and all deps through FFI) permits.
  If RocksDB bindings require `unsafe`, add a prominent safety section in the README:
  ```markdown
  ## Safety

  RocksGraph itself contains no `unsafe` code. The RocksDB dependency
  (`rust-rocksdb`) wraps a C++ library via FFI and is widely audited.
  ```
- If the crate _does_ contain `unsafe`, document every block with a `SAFETY:` comment
  explaining the invariant.

---

### 15. Empty `store/distributed/` placeholder module

**Issue:** `src/store/distributed/mod.rs` exists but is empty. It's confusing — users
may try to import it or wonder why it's there.

**Solution:**
- Remove the `pub mod distributed;` declaration from `src/store/mod.rs` and delete
  `src/store/distributed/`.
- If a distributed backend is planned for later, re-add it when it has real code.
  Empty modules in a published crate signal dead-ends.

---

### 16. `_ => unreachable!()` wildcards in step builder

**Issue:** `src/engine/volcano/builder/build_step.rs` and
`src/planner/logical_step/mod.rs` use `_ => unreachable!()` (or `_ => {}`) to handle
unknown `LogicalStep` variants. Adding a new step variant compiles but panics at
runtime.

**Solution:**
- Replace wildcard match arms with exhaustive per-variant arms. Every `LogicalStep`
  variant should have an explicit handler, even if it's:
  ```rust
  LogicalStep::NewStep { .. } => {
      Err(StoreError::UnsupportedOperation("new_step is not yet implemented".into()))
  }
  ```
- This ensures the compiler forces every handler to account for new variants.

---

## P2 — Polish & Trust Signals

These separate an "it compiles" release from a "this is production-quality" release.

### 17. Missing CONTRIBUTING.md

**Issue:** External contributors have no guidance on setup, testing, or PR expectations.

**Solution:** Create `CONTRIBUTING.md` with:
- Prerequisites (Rust stable, `just`)
- `just build` / `just test` / `just full-check` workflow
- Code style (match existing, `rustfmt.toml` is authoritative)
- PR template expectations (tests for new features, clippy clean)
- License note (all contributions are GPL-2.0-or-later)

---

### 18. Missing SECURITY.md

**Issue:** No documented vulnerability reporting process. Standard for any database.

**Solution:** Create `SECURITY.md` with:
- Contact email for vulnerability reports (`austinhan1024@gmail.com`)
- Response timeline commitment (e.g., "we will acknowledge within 72 hours")
- Disclosure policy (coordinated disclosure, 90-day window)
- Scope: covers the Rust crate and its RocksDB dependency

---

### 19. No release automation

**Issue:** Publishing is manual. Manual steps = human error.

**Solution:** Add a `just release` recipe:
```just
release patch:
    cargo release {{patch}} --execute --no-confirm
```
Or use [`cargo-release`](https://github.com/crate-ci/cargo-release) for tag + publish.
At minimum, script the steps:
1. `cargo test && cargo clippy -- --deny warnings && cargo fmt --all --check`
2. Update version in `Cargo.toml`
3. Update `CHANGELOG.md`
4. `git tag vX.Y.Z`
5. `cargo publish`
6. `git push --tags`

---

### 20. No dependency audit (`cargo-deny`)

**Issue:** Dependencies are not audited for known vulnerabilities (RUSTSEC), license
conflicts, or banned crates.

**Solution:**
- Add a `deny.toml` config to the repo root.
- Add a CI step: `cargo deny check advisories && cargo deny check licenses`.
- Add a `just audit` recipe.
- Run at least once before release; consider making it part of CI.

---

### 21. `GraphTraversal` is `#[doc(hidden)]` but users encounter it

**Issue:** `__()` returns a `GraphTraversal`, which users see in error messages and
type hints but cannot look up in docs. The type is hidden to avoid clutter, but this
creates a "phantom type" problem.

**Solution:**
- Add a section to the README or module docs explaining the `__()` pattern:
  ```markdown
  ### Anonymous sub-traversals with `__()`

  `__()` returns an internal type you never need to name. Use it with
  `where`, `coalesce`, `union` — any step that accepts a sub-traversal argument.
  ```
- Consider adding a type alias `AnonymousTraversal` that _is_ visible in docs but
  is just `GraphTraversal` under the hood.

---

### 22. No database upgrade/migration story

**Issue:** On-disk format changes between versions will break existing databases.
No documented upgrade path.

**Solution:**
- For v0.1.0: document that the on-disk format is unstable and may change without
  backward compatibility.
- Store a format version byte in the RocksDB metadata (e.g., in the `_schema` CF).
  On open, check the version and either:
  - Reject with a clear error if the format is too old.
  - Run a migration if one is defined.
- Add a `Graph::open_with_upgrade(path)` or similar.
- Document: *"RocksGraph v0.x makes no backward-compatibility guarantees for on-disk
  data. Export/re-import is required across minor versions."*

---

### 23. No backup / restore documentation

**Issue:** RocksDB supports snapshots and `checkpoint`, but users don't know how to
back up their data.

**Solution:**
- Add a section to README or a `docs/operations.md`:
  ```markdown
  ## Backup

  RocksDB stores data in the directory passed to `Graph::open()`. To back up:
  1. Close the graph (`graph.close()`).
  2. Copy the entire directory.
  3. Re-open from the copy.

  For live backup, use RocksDB's `Checkpoint` API (not yet exposed by RocksGraph
  but available via the raw `RocksStorage` handle).
  ```
- Consider exposing `Graph::checkpoint(path)` in a future version, wrapping
  RocksDB's `Checkpoint::create_checkpoint`.

---

### 24. No user-facing logging / tracing

**Issue:** Debugging a traversal that produces wrong results is opaque. No way to
see which steps executed, what they produced, or where a filter dropped items.

**Solution:**
- Add `tracing` or `log` as an optional dependency behind a feature gate:
  ```toml
  [features]
  tracing = ["dep:tracing"]
  tracing = { version = "0.1", optional = true }
  ```
- Instrument key pipeline methods (`next()`, `produce()`, filter decisions) with
  `trace!` / `debug!` macros.
- Users opt in with `rocksgraph = { features = ["tracing"] }`.

---

### 25. Schema ID limitation not prominent enough

**Issue:** The 32,767 label/property-key limit (u16) is mentioned in README "Known
Limitations" but could be missed. This is a hard ceiling.

**Solution:**
- Keep it in Known Limitations (it's there).
- Also emit a clear `StoreError::SchemaExhausted("vertex labels: 32767/32767 used")`
  when the limit is hit. (Already implemented — verify the error message is clear.)
- In `docs/design_reserved_keys.md`, document which IDs are reserved and why the
  high bit is unused.

---

### 26. `Cardinality::Single` is the only variant

**Issue:** Multi-valued properties (lists/sets per key) are not supported. A vertex
cannot have `tags: ["rust", "database", "graph"]` as a single-property-key list.

**Solution:**
- This is a schema-level change. Add `Cardinality::List` and `Cardinality::Set` variants.
- The encoding format for property values changes (multiple values per key).
- This is a significant design change — for v0.1, document as a known limitation and
  defer to v0.2+.

---

### 27. `withProperties()` fetch hint (planned, not implemented)

**Issue:** `out()` always fetches all vertex properties. There's no way to say "I only
need `name` and `age`" to skip unnecessary RocksDB reads.

**Solution:**
- Add a `with_properties(keys: &[&str])` method on `ReadTraversal` / `WriteTraversal`
  that sets a fetch hint on the traversal context.
- During materialization (in `GraphCtx::get_vertex`), only load the specified keys.
- Empty list → id + label only; absent call → all properties (current behavior).
- This is already designed in `docs/design_principles.md` § "withProperties() fetch hint".

---

### 28. `Predicate::Within` semantics for large ID lists

**Issue:** `.hasId([1, 2, 3, ... 10000])` — how does this scale? Is it an index lookup
per ID or a linear scan?

**Solution:**
- For `Key::Id` with `Within`: RocksGraph already routes through `HasIdStep` which
  does individual `get_vertex(id)` calls. For large ID lists, batch optimization
  (`get_vertices(&[ids])`) is used. Document this.
- For `Key::Property` with `Within`: the predicate is evaluated in-memory after
  property fetch. This is O(n) in the within-list size. Document the tradeoff.
- Add a section to README: *"`within([...])` is optimized for ID-based lookups via
  batch vertex fetches. For property-based within filters, large lists may degrade
  performance — prefer `hasId()` with an explicit ID list."*

---

## P3 — Nice-to-Have

### 29. Split large source files

**Issue:** `builder.rs` (~870 lines), `traversal/mod.rs` (~815 lines), and test files
(~2,500–3,400 lines) are large. Not a release blocker, but documented in the codebase
assessment as P1 maintainability items.

**Solution:** Follow the suggested refactoring in `docs/codebase_assessment.md` §
"Consolidated Improvement Roadmap":
- Split `builder.rs` → `builder/mod.rs` + `builder/build_step.rs`.
- Split `traversal/mod.rs` → traits / read / write submodules.
- Split large test files by feature area.

---

### 30. `ScanVertices` and `ScanEdges` return `UnsupportedOperation`

**Issue:** Global scan methods on `GraphSnapshot` and `GraphTransaction` traits have
default implementations that return `UnsupportedOperation`. The RocksDB backend
implements them, but the trait-level default is confusing.

**Solution:** Either remove the default implementations (making them required methods)
or document in the trait doc-comment that backends _must_ override them to be fully
functional.

---

## Suggested Implementation Order

```
Phase 1 — Legal & Metadata (est. 2-3 hours)
  P0-1: Add LICENSE file
  P0-2: Create CHANGELOG.md
  P0-4: Fix "Gremlin-compatible" wording in lib.rs
  P0-5: Add rust-version to Cargo.toml
  P0-6: Fix categories in Cargo.toml
  P0-7: Document MSRV policy in README

Phase 2 — Doc Quality (est. 2-3 hours)
  P0-3: Convert doc examples from ignore/no_run to compilable
  P2-17: Create CONTRIBUTING.md
  P2-18: Create SECURITY.md
  P2-21: Document __() / GraphTraversal type pattern

Phase 3 — Feature Completion (est. 1-2 weeks)
  P1-8:  Implement order() / by()
  P1-9:  Implement range() / skip() / tail()
  P1-10: Implement group() / groupCount()
  P1-11: Implement valueMap() / elementMap()
  P1-12: Implement simplePath() / cyclicPath()
  P1-13: Implement choose() / branch()

Phase 4 — Hardening (est. 3-5 days)
  P1-14: Add safety documentation / forbid(unsafe_code) if feasible
  P1-15: Remove empty distributed/ module
  P1-16: Eliminate _ => unreachable!() wildcards
  P2-20: Set up cargo-deny in CI
  P2-22: Document upgrade/migration policy
  P2-23: Document backup/restore procedure

Phase 5 — Release (est. 1 day)
  P2-19: Add release automation (just release recipe)
  Run cargo publish --dry-run, verify all metadata
  Tag v0.1.0, git push --tags, cargo publish
```

---

## Quick Reference: Commands to Verify Before Release

```bash
# Check for compile warnings
cargo clippy --all-targets -- --deny warnings

# Run all tests (unit + integration + doc)
cargo test --all-targets

# Verify formatting
cargo fmt --all --check

# Dry-run publish (checks categories, license, metadata)
cargo publish --dry-run

# Dependency audit
cargo deny check advisories
cargo deny check licenses

# Code coverage summary
cargo llvm-cov --summary-only

# Check that docs build without errors
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```
