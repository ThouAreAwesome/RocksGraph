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

# This script prepares the benchmark data for the RocksGraph benchmarks.
#
# 1. It creates a `bench_data` directory.
# 2. It downloads the soc-LiveJournal1 dataset from SNAP if not present.
# 3. It decompresses the data and removes comment headers.
# 4. It shuffles the entire dataset and saves it.
# 5. It takes the first 1 million lines from the shuffled data to create a
#    smaller, consistent dataset for benchmarking.

set -e # Exit immediately if a command exits with a non-zero status.

# --- Configuration ---
DATA_URL="https://snap.stanford.edu/data/soc-LiveJournal1.txt.gz"
GZ_FILE_NAME="soc-LiveJournal1.txt.gz"
BENCH_DIR="bench_data"
GZ_FILE_PATH="$BENCH_DIR/$GZ_FILE_NAME"
DECOMPRESSED_FILE="$BENCH_DIR/soc-LiveJournal1.txt"
SHUFFLED_FILE="$BENCH_DIR/soc-LiveJournal1-shuffled.txt"
FINAL_FILE="$BENCH_DIR/soc-LiveJournal1-1M.txt"
LINE_COUNT=1000000

# --- Script ---

echo "==> Ensuring benchmark directory '$BENCH_DIR' exists..."
mkdir -p "$BENCH_DIR"

# 1. Download and move the compressed file
if [ ! -f "$GZ_FILE_PATH" ]; then
  echo "==> Downloading dataset from $DATA_URL..."
  wget -O "$GZ_FILE_NAME" "$DATA_URL"
  echo "==> Moving compressed file into $BENCH_DIR..."
  mv "$GZ_FILE_NAME" "$BENCH_DIR/"
else
  echo "==> Dataset '$GZ_FILE_PATH' already exists. Skipping download."
fi

# 2. Decompress the file
if [ ! -f "$DECOMPRESSED_FILE" ]; then
  echo "==> Decompressing data..."
  gunzip -k "$GZ_FILE_PATH"
  echo "==> Removing comment lines..."
  sed -i '' '/^#/d' "$DECOMPRESSED_FILE"
else
  echo "==> Decompressed file already exists. Skipping."
fi

# 4. Shuffle the lines
if [ ! -f "$SHUFFLED_FILE" ]; then
  echo "==> Shuffling data..."
  gshuf "$DECOMPRESSED_FILE" > "$SHUFFLED_FILE"
else
  echo "==> Shuffled file already exists. Skipping."
fi

# 5. Get the first 1 million lines
if [ ! -f "$FINAL_FILE" ]; then
  echo "==> Sampling first $LINE_COUNT lines..."
  head -n "$LINE_COUNT" "$SHUFFLED_FILE" > "$FINAL_FILE"
else
  echo "==> Final file already exists. Skipping."
fi


echo "✅ Benchmark data preparation complete. Files are in '$BENCH_DIR'."