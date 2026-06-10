#!/bin/bash
# Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
#
# Runs the 'bench_gremlin_server' cargo binary benchmark.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== Running Gremlin Write Profile ==="

cd "$PROJECT_ROOT" || exit

PARALLELISM=3
STORE_DIR="$PROJECT_ROOT/data/rocksGraph_1M"

if [ -d "$STORE_DIR" ]; then
    echo "=== remove existing database $STORE_DIR"
    rm -rf "$STORE_DIR"
fi

# Execute the benchmark binary, passing all arguments to the binary itself.
# The '--' separates arguments for 'cargo run' from arguments for the binary.
cargo instruments -t cpu  --bin bench_write --release -- --data-dir "$STORE_DIR"  --parallelism $PARALLELISM "$@"

EXIT_CODE=$?
rm -rf "$STORE_DIR"

if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Benchmark failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Benchmark completed successfully. ==="
