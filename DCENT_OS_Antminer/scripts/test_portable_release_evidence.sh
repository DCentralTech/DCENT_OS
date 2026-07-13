#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for tool in python3 git openssl; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: portable release evidence gate requires $tool" >&2
        exit 1
    }
done
python3 "$SCRIPT_DIR/test_portable_release_evidence.py"
