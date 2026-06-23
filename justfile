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

# Run tests and open an HTML code coverage report in the browser
coverage:
    cargo llvm-cov --open

# Run tests and print a code coverage summary to the terminal
coverage-summary:
    cargo llvm-cov
