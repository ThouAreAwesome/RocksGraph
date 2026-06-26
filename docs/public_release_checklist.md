# Public Release Checklist

> Generated: 2026-06-25. Covers the gaps between RocksGraph v0.1.0 (beta) and a
> credible public release on crates.io / GitHub. Reconciled against actual code
> on 2026-06-26 — items confirmed done are removed rather than left to rot here;
> only genuinely-open items remain below.

---

## Table of Contents

1. [P1 — Feature Gaps for Credibility](#p1--feature-gaps-for-credibility)
2. [Suggested Implementation Order](#suggested-implementation-order)

---

## P1 — Feature Gaps for Credibility

These are Gremlin steps users will expect from a graph traversal engine. Verified
against the traversal API directly — nothing here is "implemented but not exposed."

### 11. `valueMap()` / `elementMap()` — bulk property extraction

**Issue:** No single step returns "all properties as a map." Today this requires
`.properties([...])` + `.values([...])` as two separate steps.

### 13. `branch()` — multi-way conditional branching

**Issue:** `choose()` (binary/predicate branching) is implemented; `branch()` (dispatch
to one of several traversals by a key function) has no `.branch()` builder method.

---

## Suggested Implementation Order

```
Phase 1 — Feature Completion (est. 3-5 days)
  P1-11: Implement valueMap() / elementMap()
  P1-13: Implement branch()

Phase 2 — Release (est. 1 hour)
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

# Dependency audit (deny.toml; also runs in CI)
just audit

# Code coverage summary
cargo llvm-cov --summary-only

# Check that docs build without errors
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```
