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
BENCH_DATA_DIR="$PROJECT_ROOT/rocksgraph/bench_data"
mkdir -p "$BENCH_DATA_DIR"

DATASET="${1:-100k}"

# Check for shuf or gshuf command (macOS vs Linux)
SHUF_CMD="shuf"
if ! command -v shuf &> /dev/null; then
    if command -v gshuf &> /dev/null; then
        SHUF_CMD="gshuf"
    else
        echo "Warning: neither shuf nor gshuf is installed. Shuffling will be skipped if needed."
        SHUF_CMD=""
    fi
fi

prepare_livejournal_sub() {
    local suffix="$1"
    local lines="$2"
    local final_file="$BENCH_DATA_DIR/soc-LiveJournal1-$suffix.txt"
    
    if [ -f "$final_file" ]; then
        echo "=== Dataset soc-LiveJournal1-$suffix already prepared."
        return 0
    fi
    
    local shuffled_file="$BENCH_DATA_DIR/soc-LiveJournal1-shuffled.txt"
    if [ -f "$shuffled_file" ]; then
        echo "=== Sampling first $lines lines from shuffled dataset..."
        head -n "$lines" "$shuffled_file" > "$final_file"
        echo "=== soc-LiveJournal1-$suffix prepared successfully."
        return 0
    fi
    
    local raw_gz="$BENCH_DATA_DIR/soc-LiveJournal1.txt.gz"
    if [ ! -f "$raw_gz" ]; then
        echo "=== Downloading raw LiveJournal dataset from SNAP..."
        curl -L -o "$raw_gz" "https://snap.stanford.edu/data/soc-LiveJournal1.txt.gz"
        if [ $? -ne 0 ]; then
            echo "Error: Download failed."
            exit 1
        fi
    fi
    
    local decompressed="$BENCH_DATA_DIR/soc-LiveJournal1.txt"
    if [ ! -f "$decompressed" ]; then
        echo "=== Decompressing LiveJournal dataset..."
        gunzip -c "$raw_gz" | grep -v '^#' > "$decompressed"
    fi
    
    if [ ! -f "$shuffled_file" ]; then
        if [ -n "$SHUF_CMD" ]; then
            echo "=== Shuffling dataset..."
            $SHUF_CMD "$decompressed" > "$shuffled_file"
        else
            echo "=== Warning: Skipping shuffle, using decompressed file directly."
            cp "$decompressed" "$shuffled_file"
        fi
    fi
    
    echo "=== Sampling first $lines lines from shuffled dataset..."
    head -n "$lines" "$shuffled_file" > "$final_file"
    echo "=== soc-LiveJournal1-$suffix prepared successfully."
}

case "$DATASET" in
    orkut)
        TXT_FILE="$BENCH_DATA_DIR/com-orkut.ungraph.txt"
        GZ_FILE="$BENCH_DATA_DIR/com-orkut.ungraph.txt.gz"
        
        if [ -f "$TXT_FILE" ]; then
            echo "=== com-Orkut dataset already prepared at $TXT_FILE"
            exit 0
        fi
        
        if [ ! -f "$GZ_FILE" ]; then
            echo "=== Downloading com-Orkut ungraph dataset from SNAP..."
            curl -L -o "$GZ_FILE" "https://snap.stanford.edu/data/bigdata/communities/com-orkut.ungraph.txt.gz"
            if [ $? -ne 0 ]; then
                echo "Error: Failed to download com-Orkut."
                exit 1
            fi
        fi
        
        echo "=== Decompressing com-Orkut dataset..."
        gunzip -c "$GZ_FILE" > "$TXT_FILE"
        echo "=== com-Orkut dataset prepared successfully."
        ;;
        
    10M)
        prepare_livejournal_sub "10M" 10000000
        ;;
        
    1M)
        prepare_livejournal_sub "1M" 1000000
        ;;
        
    100k)
        prepare_livejournal_sub "100k" 100000
        ;;
        
    10k)
        prepare_livejournal_sub "10k" 10000
        ;;
        
    1000)
        prepare_livejournal_sub "1000" 1000
        ;;
        
    10)
        prepare_livejournal_sub "10" 10
        ;;
        
    shuffled)
        # Full LiveJournal dataset (~69M edges, all edges shuffled).
        # Unlike the sub-samples above, "shuffled" is the complete shuffle result.
        SHUFFLED_FILE="$BENCH_DATA_DIR/soc-LiveJournal1-shuffled.txt"
        if [ -f "$SHUFFLED_FILE" ]; then
            echo "=== soc-LiveJournal1-shuffled already prepared at $SHUFFLED_FILE"
            exit 0
        fi

        local raw_gz="$BENCH_DATA_DIR/soc-LiveJournal1.txt.gz"
        if [ ! -f "$raw_gz" ]; then
            echo "=== Downloading raw LiveJournal dataset from SNAP..."
            curl -L -o "$raw_gz" "https://snap.stanford.edu/data/soc-LiveJournal1.txt.gz"
            if [ $? -ne 0 ]; then echo "Error: Download failed."; exit 1; fi
        fi

        local decompressed="$BENCH_DATA_DIR/soc-LiveJournal1.txt"
        if [ ! -f "$decompressed" ]; then
            echo "=== Decompressing LiveJournal dataset..."
            gunzip -c "$raw_gz" | grep -v '^#' > "$decompressed"
        fi

        if [ -n "$SHUF_CMD" ]; then
            echo "=== Shuffling full dataset (this may take a few minutes)..."
            $SHUF_CMD "$decompressed" > "$SHUFFLED_FILE"
        else
            echo "=== Warning: shuf not available, copying without shuffle."
            cp "$decompressed" "$SHUFFLED_FILE"
        fi
        echo "=== soc-LiveJournal1-shuffled prepared successfully."
        ;;

    *)
        echo "Unknown dataset: $DATASET"
        echo "Available options: orkut, shuffled, 10M, 1M, 100k, 10k, 1000, 10"
        exit 1
        ;;
esac
