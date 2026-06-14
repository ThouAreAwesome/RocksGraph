#!/bin/bash
## Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>
##
## This file is part of RocksGraph.
##
## RocksGraph is free software: you can redistribute it and/or modify
## it under the terms of the GNU General Public License as published by
## the Free Software Foundation, either version 2 of the License, or
## (at your option) any later version.
##
## RocksGraph is distributed in the hope that it will be useful,
## but WITHOUT ANY WARRANTY; without even the implied warranty of
## MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
## GNU General Public License for more details.
##
## You should have received a copy of the GNU General Public License
## along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.
#
# Runs the 'bench_gremlin_server' cargo binary benchmark.

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "=== Running Gremlin Write Benchmark ==="

cd "$PROJECT_ROOT" || exit

PARALLELISM=3
STORE_DIR="$PROJECT_ROOT/data/rocksGraph-1M"
FILE_PATH="$PROJECT_ROOT/bench_data/soc-LiveJournal1-1M.txt"

if [ -d "$STORE_DIR" ]; then
    echo "=== remove existing database $STORE_DIR"
    rm -rf "$STORE_DIR"
fi

# Execute the benchmark binary, passing all arguments to the binary itself.
# The '--' separates arguments for 'cargo run' from arguments for the binary.
cargo run --bin bench_write --release -- --data-dir "$STORE_DIR"  --file-path "$FILE_PATH" --parallelism $PARALLELISM "$@"

EXIT_CODE=$?
if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Benchmark failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Benchmark completed successfully. ==="