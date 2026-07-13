#!/bin/sh
# Offline adversarial contract for post-build snapshot receipts.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$SCRIPT_DIR/test_binary_build_receipt.py"
