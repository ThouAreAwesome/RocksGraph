set allow-duplicate-recipes := true
set allow-duplicate-variables := true
set shell := ["bash", "-euo", "pipefail", "-c"]

# ---------------------------------------------------------------------------- #
#                                 DEPENDENCIES                                 #
# ---------------------------------------------------------------------------- #

# Rust: https://rust-lang.org/tools/install
cargo := require("cargo")
rustc := require("rustc")

# ---------------------------------------------------------------------------- #
#                                    RECIPES                                   #
# ---------------------------------------------------------------------------- #

# Show available commands
default:
    @just --list

# Build the program
build:
    cargo build

# Build the program for release
build-release:
    cargo build --release
alias br := build-release

# Check the program for errors without building
check:
    cargo check

# Run the program
run:
    cargo run

# Generate documentation
doc:
    cargo doc --open

# Run all code checks
full-check:
    cargo fmt --all --check
    cargo clippy -- --deny warnings
alias fc := full-check

full-write:
    cargo fmt --all
alias fw := full-write

# Run tests
test:
    cargo test

# Repeatedly runs every test whose name contains "concurrent" (vector-index
# locking, schema-session locking, isolation, edge/vertex-deletion races) plus
# test_rebuild_survives_multiple_capacity_growth_cycles, which predates that
# naming convention — name new concurrency regression tests with "concurrent"
# in them so this picks them up automatically. Surfaces rare races a single
# `cargo test` run has a real chance of missing: several bugs in
# rocksgraph/src/vector/hnsw.rs's resize_lock/upsert_locks were found this way
# with well under 100% single-run reproduction rates.
# Run before merging concurrency-sensitive changes to hnsw.rs's locking or the vector commit/checkpoint path.
stress-test rounds="50":
    #!/usr/bin/env bash
    set -euo pipefail
    for i in $(seq 1 {{rounds}}); do
        echo "=== round $i/{{rounds}} ==="
        cargo test -p rocksgraph --lib -- concurrent test_rebuild_survives_multiple_capacity_growth_cycles --test-threads=1
    done
alias st := stress-test

# Run tests and open an HTML code coverage report in the browser
coverage:
    cargo llvm-cov --open

# Run tests and print a code coverage summary to the terminal
coverage-summary:
    cargo llvm-cov

# Audit dependencies for RUSTSEC advisories, license conflicts, and banned/duplicate
# crates (requires `cargo install cargo-deny`)
audit:
    cargo deny check
alias au := audit

# Cut a release: verifies the tree is clean, full-check/test/audit/dry-run all pass,
# and CHANGELOG.md has an entry — then tags, pushes the tag, and runs `cargo publish`.
# Bump the version in Cargo.toml and add the CHANGELOG.md entry yourself first.
release:
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(grep -m1 '^version = ' rocksgraph/Cargo.toml | sed -E 's/version = "(.*)"/\1/')
    echo "Preparing to release v$version"

    if [ -n "$(git status --porcelain)" ]; then
        echo "Working tree is not clean — commit or stash changes first." >&2
        exit 1
    fi

    if ! grep -q "## \[$version\]" rocksgraph/CHANGELOG.md; then
        echo "rocksgraph/CHANGELOG.md has no entry for $version — add one first." >&2
        exit 1
    fi

    just full-check
    just test
    just audit
    cargo publish -p rocksgraph --dry-run

    read -r -p "About to tag v$version, push tags, and run 'cargo publish'. Continue? [y/N] " confirm
    if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
        echo "Aborted."
        exit 1
    fi

    git tag "v$version"
    git push --tags
    cargo publish -p rocksgraph
