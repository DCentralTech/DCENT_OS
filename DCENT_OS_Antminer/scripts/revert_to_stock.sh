#!/bin/sh
# Legacy S9 entry point. Keep one implementation of the containment policy.

set -eu

SCRIPT_DIR=${0%/*}
if [ "$SCRIPT_DIR" = "$0" ]; then
    SCRIPT_DIR=.
fi

exec /bin/sh "$SCRIPT_DIR/revert_to_stock_s9.sh" "$@"
