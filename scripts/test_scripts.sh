#!/bin/bash
# Runs the Rust test suite for the MultiGraph project.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== Running MultiGraph Test Suite ==="

cd "$PROJECT_ROOT" || exit

# Run the full test suite, including integration tests
cargo test

if [ $? -eq 0 ]; then
    echo "=== All tests passed successfully. ==="
else
    echo "=== Some tests failed. ==="
    exit 1
fi