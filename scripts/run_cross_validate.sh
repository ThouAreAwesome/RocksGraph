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
#
# Cross-validates that BulkLoader and the transactional (TxnSession) OLTP
# write path produce equivalent databases: generates a diversified synthetic
# LDBC-shaped dataset (typed properties + a FloatVector "embedding"), loads
# it via BulkLoader (Rust), TxnSession (Rust), and BulkLoader (Python), then
# compares the resulting databases structurally and via ANN
# (.nearest()/.neighbors()) recall. The Python pass exists to catch PyO3
# marshaling bugs (Vector/Uuid/Bytes across the FFI boundary) — it reuses the
# same comparison logic against the Rust bulk DB rather than re-verifying
# BulkLoader-vs-TxnSession equivalence a second time.
#
# Usage: scripts/run_cross_validate.sh [num_vertices] [num_edges]

set -eo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

NUM_VERTICES="${1:-50000}"
NUM_EDGES="${2:-250000}"

WORK_DIR="/tmp/rocksgraph_cross_validate"
DATASET_DIR="$WORK_DIR/dataset"
BULK_DB="$WORK_DIR/bulk_db"
OLTP_DB="$WORK_DIR/oltp_db"
PY_BULK_DB="$WORK_DIR/py_bulk_db"

rm -rf "$WORK_DIR"
mkdir -p "$DATASET_DIR"

echo "=== 1/5: Generating synthetic dataset ($NUM_VERTICES vertices, $NUM_EDGES edges) ==="
python3 "$PROJECT_ROOT/scripts/generate_synthetic_ldbc.py" "$DATASET_DIR" "$NUM_VERTICES" "$NUM_EDGES"

echo ""
echo "=== 2/5: Loading via BulkLoader (Rust) ==="
(cd "$PROJECT_ROOT/rocksgraph" && cargo run --release --bin bulk_load_ldbc_typed -- "$DATASET_DIR" "$BULK_DB")

echo ""
echo "=== 3/5: Loading via TxnSession (Rust OLTP) ==="
(cd "$PROJECT_ROOT/rocksgraph" && cargo run --release --bin oltp_load_ldbc -- "$DATASET_DIR" "$OLTP_DB")

echo ""
echo "=== 4/5: Loading via BulkLoader (Python bindings) ==="
PYTHONPATH="$PROJECT_ROOT/bindings/python${PYTHONPATH:+:$PYTHONPATH}" python3 \
    "$PROJECT_ROOT/bindings/python/examples/bulkload_ldbc_typed.py" "$DATASET_DIR" "$PY_BULK_DB"

echo ""
echo "=== 5/5: Cross-validating the three databases ==="
FAILED=0

echo "--- Rust BulkLoader vs Rust TxnSession ---"
set +e
(cd "$PROJECT_ROOT/rocksgraph" && cargo run --release --bin cross_validate_load -- --bulk-db "$BULK_DB" --oltp-db "$OLTP_DB")
[ $? -ne 0 ] && FAILED=1
set -e

echo ""
echo "--- Rust BulkLoader vs Python BulkLoader (PyO3 marshaling check) ---"
set +e
(cd "$PROJECT_ROOT/rocksgraph" && cargo run --release --bin cross_validate_load -- --bulk-db "$BULK_DB" --oltp-db "$PY_BULK_DB")
[ $? -ne 0 ] && FAILED=1
set -e

if [ $FAILED -ne 0 ]; then
    echo "=== Cross-validation FAILED. ==="
    exit 1
fi

echo "=== Cross-validation passed. ==="
exit 0
