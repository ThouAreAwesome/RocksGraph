#!/bin/bash
# Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PID_FILE="$PROJECT_ROOT/server.pid"

if [ ! -f "$PID_FILE" ]; then
    echo "Error: server.pid not found. Is the server running?"
    exit 1
fi

PID=$(cat "$PID_FILE")

echo "Stopping MultiGraph Server (PID: $PID)..."
kill "$PID"

# Wait for the process to exit
TIMEOUT=30
COUNT=0
while ps -p "$PID" > /dev/null; do
    if [ $COUNT -ge $TIMEOUT ]; then
        echo "Timeout reached. Force killing..."
        kill -9 "$PID"
        break
    fi
    sleep 1
    ((COUNT++))
    echo -n "."
done
echo ""

rm "$PID_FILE"
echo "Server stopped successfully."