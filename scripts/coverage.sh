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
# Runs the RocksGraph test suite under cargo-llvm-cov and reports code
# coverage.
#
# By default this opens an HTML report in the browser. Pass --summary to
# print a terminal-only summary instead (e.g. for headless/CI use).

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$PROJECT_ROOT" || exit

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "=== cargo-llvm-cov not found. Installing... ==="
    rustup component add llvm-tools-preview
    cargo install cargo-llvm-cov --locked
fi

if [ "$1" = "--summary" ]; then
    shift
    echo "=== Running RocksGraph Test Suite with Coverage (summary) ==="
    cargo llvm-cov --summary-only "$@"
else
    echo "=== Running RocksGraph Test Suite with Coverage (HTML report) ==="
    cargo llvm-cov --open "$@"
fi

EXIT_CODE=$?
if [ $EXIT_CODE -ne 0 ]; then
    echo "=== Coverage run failed with exit code $EXIT_CODE. ==="
    exit 1
fi

echo "=== Coverage run completed successfully. ==="
