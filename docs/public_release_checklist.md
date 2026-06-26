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


## P1 — Feature Gaps for Credibility

These are Gremlin steps or capabilities that users will expect from a graph database
traversal engine. Their absence makes the product feel incomplete.


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
