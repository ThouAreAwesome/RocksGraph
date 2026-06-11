#!/usr/bin/env bash
# Auto-prepends the BUSL-1.1 header to newly written .rs files.
# Invoked as a PostToolUse hook by Claude Code.
set -euo pipefail

input=$(cat)
file_path=$(printf '%s' "$input" | python3 -c "
import sys, json
d = json.load(sys.stdin)
print(d.get('tool_input', {}).get('file_path', ''))
" 2>/dev/null || true)

[[ -z "$file_path" || "$file_path" != *.rs ]] && exit 0
[[ -f "$file_path" ]] || exit 0
grep -q "SPDX-License-Identifier" "$file_path" && exit 0

python3 - "$file_path" <<'EOF'
import sys, pathlib
f = pathlib.Path(sys.argv[1])
lines = [
    "// Copyright (c) 2026 Austin Han <austinhan1024@gmail.com>",
    "//
    "// This file is part of RocksGraph.",
    "//",
    "// RocksGraph is free software: you can redistribute it and/or modify",
    "// it under the terms of the GNU General Public License as published by",
    "// the Free Software Foundation, either version 2 of the License, or",
    "// (at your option) any later version.",
    "//",
    "// RocksGraph is distributed in the hope that it will be useful,",
    "// but WITHOUT ANY WARRANTY; without even the implied warranty of",
    "// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the",
    "// GNU General Public License for more details.",
    "//",
    "// You should have received a copy of the GNU General Public License",
    "// along with RocksGraph.  If not, see <https://www.gnu.org/licenses/>.",
    "",
]
f.write_text("\n".join(lines) + "\n" + f.read_text())
EOF
