#!/bin/bash
# Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PID_FILE="$PROJECT_ROOT/server.pid"
LOG_FILE="$PROJECT_ROOT/server.log"
CONFIG_FILE="${1:-$PROJECT_ROOT/config/server.toml}"

if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE")
    if ps -p "$PID" > /dev/null; then
        echo "Error: Server is already running with PID $PID."
        exit 1
    else
        echo "Removing stale PID file."
        rm "$PID_FILE"
    fi
fi

if [ ! -f "$CONFIG_FILE" ]; then
    echo "Error: Configuration file not found at $CONFIG_FILE"
    exit 1
fi

# Ensure the data directory exists as defined in config (defaults to data/rocksdb_main)
DATA_DIR=$(grep "data_dir =" "$CONFIG_FILE" | cut -d'=' -f2 | tr -d '" ' | sed 's/^"//;s/"$//' || echo "data/rocksdb_main")
mkdir -p "$PROJECT_ROOT/$DATA_DIR"

echo "Building and Starting MultiGraph Server..."
echo "Config: $CONFIG_FILE"
echo "Logs:   $LOG_FILE"

# Start the server in the background from the project root
cd "$PROJECT_ROOT" || exit

# Build separately so that $! below captures multigraph's PID, not a subshell's PID.
# (cargo build && nohup ... & would background a compound list, making $! the subshell PID.
#  stop_server.sh would then kill the wrong process, leaving multigraph holding the RocksDB lock.)
if ! cargo build --release; then
    echo "Build failed."
    exit 1
fi

nohup ./target/release/multigraph --config "$CONFIG_FILE" >> "$LOG_FILE" 2>&1 &

NEW_PID=$!
echo "$NEW_PID" > "$PID_FILE"

echo "Server started in background with PID $NEW_PID."
echo "Check logs with: tail -f server.log"