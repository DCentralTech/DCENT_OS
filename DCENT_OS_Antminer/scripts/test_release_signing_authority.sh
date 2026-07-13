#!/bin/sh
# Host-side adversarial regression gate for the invocation signing authority.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec python3 "$SCRIPT_DIR/test_release_signing_authority.py"
