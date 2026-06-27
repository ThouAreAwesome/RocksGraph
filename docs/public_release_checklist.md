# Public Release Checklist

> Generated: 2026-06-25. Covers the gaps between RocksGraph v0.1.0 (beta) and a
> credible public release on crates.io / GitHub. Reconciled against actual code
> on 2026-06-27 — items confirmed done are removed rather than left to rot here;
> only genuinely-open items remain below.

---

No open blockers or feature gaps remain for the first publish. `valueMap()` /
`elementMap()` and `branch()` are deliberately deferred past v0.1.0 (ergonomic gaps with
existing workarounds, not functional blockers) — see `docs/TODO.md`'s "deferred past the
first publish" section for the rationale and workarounds.

## Suggested Implementation Order

```
Release (est. 1 hour)
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
