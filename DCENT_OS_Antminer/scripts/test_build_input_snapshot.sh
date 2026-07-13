#!/bin/sh
# Focused offline gate for exact-tree external build-input snapshots.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

command -v python3 >/dev/null 2>&1 || {
    echo "ERROR: build-input snapshot test requires python3" >&2
    exit 1
}

python3 "$SCRIPT_DIR/test_build_input_snapshot.py" -q
echo "build-input snapshot: PASS"
