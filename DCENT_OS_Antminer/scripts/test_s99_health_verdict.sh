#!/bin/sh
# test_s99_health_verdict.sh — functional test of daemon_real_health_verdict from
# the zynq S99upgrade init script: the load-bearing A/B-slot rollback commit-vs-
# revert decision.
#
# The BRICK-SAFETY contract (documented in S99upgrade): a fresh slot is committed
# (upgrade_stage cleared) only on a REAL healthy daemon; a *too-strict* gate only
# costs the new upgrade (U-Boot reverts to the known-good slot), while committing a
# BROKEN slot too early is the dangerous failure. So the classification must be:
#   - positive uptime            -> "healthy"   (commit is allowed)
#   - reachable, zero uptime      -> "unhealthy" (POSITIVE failure -> block commit)
#   - absent proof (no wget / empty body / unparseable) -> "unknown" (SOFT-PASS:
#     do NOT newly-revert a unit the old logic would have committed)
#
# This sources the REAL function (extracted from the script, so it can't drift from
# a hand-copied duplicate) and drives it with a mock `wget`, so a regression that
# turned an absence-of-proof into a "block" (needless reverts) or a zero-uptime into
# a "commit" (bricks) is caught. No boot-script modification.
set -u

DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd) # DCENT_OS_Antminer
S99="$DIR/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade"

if [ ! -f "$S99" ]; then
    echo "SKIP: zynq S99upgrade not found at $S99" >&2
    exit 0
fi

# Extract just the daemon_real_health_verdict function (opening line to its
# column-0 closing brace) and define it in this shell.
FN=$(awk '/^daemon_real_health_verdict\(\) \{/{p=1} p{print} p&&/^\}$/{exit}' "$S99")
if [ -z "$FN" ]; then
    echo "FAIL: could not extract daemon_real_health_verdict from S99upgrade" >&2
    exit 1
fi
eval "$FN"

fails=0
ok() { printf 'ok   - %s\n' "$*"; }
no() { printf 'FAIL - %s\n' "$*" >&2; fails=$((fails + 1)); }

# Mock wget: `daemon_real_health_verdict` calls `wget -q -T 4 -O - URL`; this shell
# function shadows the binary and echoes the canned body regardless of args.
MOCK_BODY=''
wget() { printf '%s' "$MOCK_BODY"; }

expect() { # <label> <expected-verdict>
    got=$(daemon_real_health_verdict)
    if [ "$got" = "$2" ]; then ok "$1 -> $2"; else no "$1: expected '$2', got '$got'"; fi
}

MOCK_BODY='{"daemon":{"uptime_s":42},"process":{"uptime_s":0}}'
expect "positive daemon uptime (commit allowed)" "healthy"

MOCK_BODY='{"daemon":{"uptime_s":0}}'
expect "reachable but zero uptime (POSITIVE failure, block commit)" "unhealthy"

MOCK_BODY=''
expect "empty body (absent proof, SOFT-PASS)" "unknown"

MOCK_BODY='{"status":"ok","note":"no uptime here"}'
expect "unparseable body (absent proof, SOFT-PASS)" "unknown"

# The daemon block precedes any other uptime in this endpoint's shape; the first
# parsed uptime_s must be the daemon's (a positive daemon uptime is not masked by a
# later zero).
MOCK_BODY='{"daemon":{"uptime_s":7},"pool":{"uptime_s":0}}'
expect "daemon uptime read first (not masked by a later zero)" "healthy"

if [ "$fails" -ne 0 ]; then
    printf '\nS99 health-verdict test FAILED: %s assertion(s)\n' "$fails" >&2
    exit 1
fi
printf '\nS99 health-verdict test passed.\n'
