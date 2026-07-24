#!/usr/bin/env bash
# Compatibility entry point for the strict AM1 external-SD recovery probe.
set -euo pipefail
umask 077
SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
PYTHON_BIN="${PYTHON:-$(command -v python3 || command -v python || true)}"
[ -n "$PYTHON_BIN" ] || { echo "ERROR: Python is required" >&2; exit 1; }
exec "$PYTHON_BIN" "$SCRIPT_DIR/am1_nand_sd_recovery_probe.py" "$@"
