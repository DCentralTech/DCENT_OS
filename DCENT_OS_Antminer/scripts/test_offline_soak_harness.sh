#!/bin/sh
#
# CI-11 host-only regression for scripts/offline_soak_harness.sh.
#
# This test never contacts a miner or network endpoint. It proves that the
# offline soak harness detects bounded RSS/fd behavior, rejects synthetic
# resource-growth series, and can monitor a short local process while mapping
# the sample window onto a logical 4-hour soak.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
cd "$PROJECT_DIR"

sh -n scripts/offline_soak_harness.sh
sh scripts/offline_soak_harness.sh --self-test
