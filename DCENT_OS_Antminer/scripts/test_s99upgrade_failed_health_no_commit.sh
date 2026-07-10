#!/bin/sh
# Host-side regression test for S99upgrade's failed-health path.
#
# This does not contact hardware and does not touch /dev or /etc. The init
# script's default production paths are redirected into a tempdir and every
# command with host/device side effects is shadowed by a shim.
set -eu

DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
S99="$DIR/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade"

if [ ! -f "$S99" ]; then
    echo "SKIP: zynq S99upgrade not found at $S99" >&2
    exit 0
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/dcent-s99-failed-health.XXXXXX")
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT INT TERM

SHIM="$WORK/shims"
mkdir -p "$SHIM"
: > "$WORK/mtd4"
: > "$WORK/fw_env.config"
: > "$WORK/dcentrald"
chmod 0755 "$WORK/dcentrald"

cat >"$SHIM/fw_printenv" <<'EOF'
#!/bin/sh
echo "$*" >> "$S99_TEST_WORK/fw_printenv.log"
if [ "${1:-}" = "upgrade_stage" ]; then
    echo "upgrade_stage=1"
    exit 0
fi
echo "firmware=1"
echo "upgrade_stage=1"
echo "first_boot=yes"
exit 0
EOF

cat >"$SHIM/fw_setenv" <<'EOF'
#!/bin/sh
echo "$*" >> "$S99_TEST_WORK/fw_setenv.log"
exit 0
EOF

cat >"$SHIM/ip" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "addr" ]; then
    echo "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500"
    echo "    inet 192.0.2.10/24 brd 192.0.2.255 scope global eth0"
    exit 0
fi
exit 1
EOF

cat >"$SHIM/netstat" <<'EOF'
#!/bin/sh
echo "tcp        0      0 0.0.0.0:22            0.0.0.0:*               LISTEN"
exit 0
EOF

cat >"$SHIM/pidof" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "dcentrald" ]; then
    echo "123"
    exit 0
fi
exit 1
EOF

cat >"$SHIM/wget" <<'EOF'
#!/bin/sh
case "$*" in
    *"/api/system/health"*)
        printf '%s' '{"daemon":{"uptime_s":0}}'
        exit 0
        ;;
    *)
        exit 0
        ;;
esac
EOF

cat >"$SHIM/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF

cat >"$SHIM/sync" <<'EOF'
#!/bin/sh
exit 0
EOF

chmod 0755 "$SHIM"/*

OUT="$WORK/s99.out"
set +e
PATH="$SHIM:$PATH" \
S99_TEST_WORK="$WORK" \
DCENTOS_MTD4_NODE="$WORK/mtd4" \
DCENTOS_FW_ENV_CONFIG="$WORK/fw_env.config" \
DCENTOS_DCENTRALD_BIN="$WORK/dcentrald" \
DCENTOS_UPGRADE_COMMIT_MARKER="$WORK/commit-marker" \
DCENTOS_BOOT_SUCCESS_WINDOW_S=1 \
    sh "$S99" start >"$OUT" 2>&1
rc=$?
set -e

if [ "$rc" -ne 0 ]; then
    cat "$OUT" >&2
    echo "FAIL: S99upgrade failed-health path exited $rc" >&2
    exit 1
fi

grep -F "HEALTH CHECK FAILED - NOT clearing upgrade_stage" "$OUT" >/dev/null || {
    cat "$OUT" >&2
    echo "FAIL: failed-health message missing" >&2
    exit 1
}

grep -F "/api/system/health reachable but reports a dead/zero-uptime daemon" "$OUT" >/dev/null || {
    cat "$OUT" >&2
    echo "FAIL: zero-uptime health failure was not exercised" >&2
    exit 1
}

if ! grep -Fx "blocked" "$WORK/commit-marker" >/dev/null 2>&1; then
    cat "$OUT" >&2
    echo "FAIL: failed-health path did not write blocked commit marker" >&2
    exit 1
fi

if [ -s "$WORK/fw_setenv.log" ]; then
    cat "$OUT" >&2
    echo "FAIL: failed-health path called fw_setenv:" >&2
    cat "$WORK/fw_setenv.log" >&2
    exit 1
fi

echo "S99UPGRADE_FAILED_HEALTH_NO_COMMIT_OK"
