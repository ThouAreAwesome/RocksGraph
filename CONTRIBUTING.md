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

## Code style

Match the style of the surrounding code rather than introducing a new convention.
`rustfmt.toml` is authoritative for formatting — don't hand-format against it. Comments should
explain *why*, not *what*; avoid restating what well-named code already says.

## Pull requests

- Keep PRs focused — one logical change per PR is easier to review than a bundle of unrelated
  fixes.
- Include tests for new functionality or bug fixes.
- Describe the *why* in the PR description, not just the *what* — the diff already shows what
  changed.

## License

By contributing, you agree that your contributions are licensed under the same terms as the
project: GPL-2.0-or-later (see [LICENSE](LICENSE)).

## Reporting bugs

Open a GitHub issue with a minimal reproduction where possible. For security vulnerabilities,
see [SECURITY.md](SECURITY.md) instead of filing a public issue.
