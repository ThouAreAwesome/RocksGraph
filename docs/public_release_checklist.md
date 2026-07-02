# Public Release Checklist

> Updated: 2026-07-01. v0.1.0 release prep.

---

## v0.1.0 — Released 2026-07

- [x] `cargo publish --dry-run` passes
- [x] `cargo clippy --all-targets -- --deny warnings` clean
- [x] `cargo fmt --all --check` clean
- [x] `cargo test --all-targets` — 701 tests pass
- [x] Doc examples compile and run (10 doctests)
- [x] BENCHMARKS.md updated with latest numbers
- [x] CHANGELOG.md up to date
- [x] `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` clean

### Deferred past v0.1.0

`valueMap()` / `elementMap()` and `branch()` are deliberately deferred (ergonomic gaps with
existing workarounds, not functional blockers) — see `docs/TODO.md` for rationale.

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
