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

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DATASET="${1:-100k}"
shift # Remove dataset name from argument list

STORE_DIR="$PROJECT_ROOT/data/rocksGraph-$DATASET"

if [ "$DATASET" = "orkut" ]; then
    FILE_PATH="$PROJECT_ROOT/rocksgraph/bench_data/com-orkut.ungraph.txt"
else
    FILE_PATH="$PROJECT_ROOT/rocksgraph/bench_data/soc-LiveJournal1-$DATASET.txt"
fi

if [ ! -d "$STORE_DIR" ]; then
    echo "=== Error: Database directory $STORE_DIR not found. Run bench_write first."
    exit 1
fi

PARALLELISM=5
# Default: 10 000 randomly-sampled query pairs per benchmark.
# Pass --queries 0 as an extra argument to use the full file.
QUERIES=10000

echo "=== Running Read Benchmark ($DATASET, ${QUERIES} query pairs per benchmark) ==="
cargo run --bin bench_read --release -- \
  --data-dir "$STORE_DIR" \
  --file-path "$FILE_PATH" \
  --parallelism $PARALLELISM \
  --queries $QUERIES \
  "$@"

EXIT_CODE=$?
if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Read Benchmark failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Read Benchmark completed successfully. ==="
exit 0