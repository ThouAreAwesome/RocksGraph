#!/bin/bash
# Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== Running Gremlin Read Benchmark ==="

cd "$PROJECT_ROOT" || exit

PARALLELISM=3
STORE_DIR="$PROJECT_ROOT/data/rocksGraph_1M"

# Execute the benchmark binary
cargo run --bin bench_read --release -- --data-dir "$STORE_DIR" --parallelism $PARALLELISM "$@"

EXIT_CODE=$?
if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Benchmark failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Benchmark completed successfully. ==="