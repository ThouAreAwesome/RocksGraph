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

echo "=== Preparing dataset '$DATASET' ==="
"$PROJECT_ROOT/scripts/prepare_dataset.sh" "$DATASET"
if [ $? -ne 0 ]; then
    echo "Error preparing dataset. Exiting."
    exit 1
fi

STORE_DIR="$PROJECT_ROOT/data/rocksGraph-$DATASET"

if [ "$DATASET" = "orkut" ]; then
    FILE_PATH="$PROJECT_ROOT/rocksgraph/bench_data/com-orkut.ungraph.txt"
    # Default memory limit to 1 GiB for Orkut unless explicitly specified
    DEFAULT_MAX_MEMORY="--max-memory 1073741824"
else
    FILE_PATH="$PROJECT_ROOT/rocksgraph/bench_data/soc-LiveJournal1-$DATASET.txt"
    DEFAULT_MAX_MEMORY=""
fi

if [ -d "$STORE_DIR" ]; then
    echo "=== Removing existing database $STORE_DIR"
    rm -rf "$STORE_DIR"
fi

# Check if user explicitly passed --max-memory
HAS_MAX_MEMORY=false
for arg in "$@"; do
    if [ "$arg" = "--max-memory" ]; then
        HAS_MAX_MEMORY=true
        break
    fi
done

EXTRA_ARGS=()
if [ "$HAS_MAX_MEMORY" = "false" ] && [ -n "$DEFAULT_MAX_MEMORY" ]; then
    # Add default max-memory option
    EXTRA_ARGS+=($DEFAULT_MAX_MEMORY)
fi

echo "=== Running Write Benchmark ($DATASET) ==="
cargo run --bin bench_write --release -- \
  --data-dir "$STORE_DIR" \
  --file-path "$FILE_PATH" \
  "${EXTRA_ARGS[@]}" \
  "$@"

EXIT_CODE=$?
if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Write Benchmark failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Write Benchmark completed successfully. ==="
exit 0
