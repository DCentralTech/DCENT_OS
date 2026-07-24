#!/bin/sh
# Stable CLI entry point for the evidence-pinned AM2 artifact stage.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PYTHON=''
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 &&
        "$candidate" -c \
            'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
            >/dev/null 2>&1; then
        PYTHON=$candidate
        break
    fi
done
if [ -z "$PYTHON" ]; then
    echo "ERROR: Python 3.10 or newer is required for recoverable AM2 artifact staging" >&2
    exit 1
fi
exec "$PYTHON" "$SCRIPT_DIR/stage_am2_sd_artifacts.py" "$@"
