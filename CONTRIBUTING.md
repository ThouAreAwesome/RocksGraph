# Contributing to RocksGraph

Thanks for considering a contribution. RocksGraph is a small project, so the process is
intentionally lightweight.

## Prerequisites

- A stable Rust toolchain meeting the [MSRV](README.md#development) (currently 1.80+)
- [`just`](https://github.com/casey/just) — all common workflows are wired up as `just` recipes;
  run `just --list` (or `just`) to see them all

## Workflow

```bash
just build        # cargo build
just test         # cargo test
just full-check    # cargo fmt --all --check && cargo clippy -- --deny warnings
just full-write    # cargo fmt --all (fixes formatting in place)
```

Before opening a PR:

1. Run `just full-check`. If it reports formatting issues, run `just full-write` and commit the
   result rather than hand-fixing formatting.
2. Run `just test` and make sure it passes. New behavior should come with new tests under the
   relevant module's `tests.rs` / `tests/` directory — see existing tests nearby for the
   project's testing conventions.
3. `cargo clippy -- --deny warnings` must be clean; CI enforces this on both Linux and macOS.

## Scripts

Beyond the `just` recipes above, `scripts/` has a few task-specific helpers — not part of the
required PR workflow, but useful when touching benchmarks or bulk loading. Each script documents
its own flags in its header comment; run it with no arguments to see the defaults.

- `prepare_dataset.sh <name>` — downloads/prepares a SNAP dataset (LiveJournal or Orkut) into
  `rocksgraph/bench_data/snap/`.
- `bench_write.sh <name>` / `bench_read.sh <name>` — prepare-and-bulk-load / query-benchmark a SNAP
  dataset; see [`docs/guides/benchmarks.md`](docs/guides/benchmarks.md) for the numbers these
  produce.
- `bench_integrity.sh <name>` — checks degree-CF consistency against a full adjacency scan on a
  store already built by `bench_write.sh`.
- `instruments_write.sh` / `instruments_read.sh` — macOS-only `cargo instruments` CPU-profiling
  wrappers around the same benchmarks.
- `coverage.sh [--summary]` — runs the test suite under `cargo-llvm-cov`; opens an HTML report by
  default.
- `generate_synthetic_ldbc.py <out_dir> <num_vertices> <num_edges>` — generates a synthetic
  LDBC-SNB-shaped dataset (every scalar `DataType` plus a `FloatVector` embedding) for the
  bulk-load examples and the cross-validation harness below.
- `run_bulkload.py --dataset {snap,ldbc} --lang {rust,python} --data-dir <path>` — runs one of the
  bulk-load examples under `rocksgraph/examples/` or `bindings/python/examples/`.
- `run_cross_validate.sh [num_vertices] [num_edges]` — cross-validates `BulkLoader` against the
  transactional (`TxnSession`) write path, and the Rust `BulkLoader` against its Python binding:
  generates a dataset, loads it three ways, and compares the resulting databases structurally and
  via ANN (`.nearest()`/`.neighbors()`) recall.

## Code style

Match the style of the surrounding code rather than introducing a new convention.
`rustfmt.toml` is authoritative for formatting — don't hand-format against it. Comments should
explain *why*, not *what*; avoid restating what well-named code already says.

## Documentation

Changes to `docs/guides/*.md` or the per-binding READMEs are user-facing
documentation, published to the GitHub Wiki. Before editing them, read
[`docs/documentation_principles.md`](docs/documentation_principles.md) —
it covers the failure modes we've actually hit (unverified API claims,
mislabeled anti-patterns, uncomparable benchmark numbers, and so on).

## Pull requests

- Keep PRs focused — one logical change per PR is easier to review than a bundle of unrelated
  fixes.
- Include tests for new functionality or bug fixes.
- Describe the *why* in the PR description, not just the *what* — the diff already shows what
  changed.

## License

By contributing, you agree that your contributions are dual-licensed under the terms of both the MIT License and the Apache License (Version 2.0). See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for details.

## Reporting bugs

Open a GitHub issue with a minimal reproduction where possible. For security vulnerabilities,
see [SECURITY.md](SECURITY.md) instead of filing a public issue.
