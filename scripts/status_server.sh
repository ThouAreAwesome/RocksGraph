#!/bin/bash
# Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PID_FILE="$PROJECT_ROOT/server.pid"

if [ -f "$PID_FILE" ]; then
    PID=$(cat "$PID_FILE")
    if ps -p "$PID" > /dev/null; then
        echo "Server is running (PID: $PID)"
        exit 0
    else
        echo "Server PID file exists ($PID) but process is not running."
        exit 1
    fi
else
    echo "Server is not running."
    exit 1
fi