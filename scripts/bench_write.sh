#!/bin/bash
# Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
#
# Runs the 'bench_gremlin_server' cargo binary benchmark.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== Running Gremlin Server Benchmark ==="

cd "$PROJECT_ROOT" || exit

CONFIG_FILE="$PROJECT_ROOT/config/bench.toml"
# Execute the benchmark binary, passing all arguments to the binary itself.
# The '--' separates arguments for 'cargo run' from arguments for the binary.
cargo run --bin bench_gremlin_server --release -- --config "$CONFIG_FILE" "$@"

EXIT_CODE=$?
if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Benchmark failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Benchmark completed successfully. ==="