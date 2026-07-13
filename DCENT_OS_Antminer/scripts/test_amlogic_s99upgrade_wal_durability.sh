#!/bin/sh
# Offline failure-injection test for the Amlogic firstboot WAL publication.
set -eu

ROOT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
S99="$ROOT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S99upgrade"

if [ ! -f "$S99" ]; then
    echo "SKIP: Amlogic S99upgrade not found at $S99" >&2
    exit 0
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/dcent-amlogic-wal.XXXXXX")
ALIVE_PID=
cleanup() {
    if [ -n "$ALIVE_PID" ]; then
        kill "$ALIVE_PID" 2>/dev/null || true
        wait "$ALIVE_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

SHIM="$WORK/shim"
mkdir -p "$SHIM" "$WORK/data"
: > "$WORK/mtd5"
: > "$WORK/flash.log"

cat > "$SHIM/nanddump" <<'EOF'
#!/bin/sh
if [ -f "$S99_TEST_FLAG_STATE" ]; then
    cat "$S99_TEST_FLAG_STATE"
else
    printf '\002'
fi
EOF

cat > "$SHIM/ip" <<'EOF'
#!/bin/sh
echo "2: eth0: <UP> mtu 1500"
echo "    inet 192.0.2.10/24 scope global eth0"
EOF

cat > "$SHIM/netstat" <<'EOF'
#!/bin/sh
echo "tcp 0 0 0.0.0.0:22 0.0.0.0:* LISTEN"
EOF

cat > "$SHIM/pidof" <<'EOF'
#!/bin/sh
printf '%s\n' "$S99_TEST_PID"
EOF

cat > "$SHIM/wget" <<'EOF'
#!/bin/sh
case "$*" in
    *api/system/health*) printf '%s' '{"daemon":{"uptime_s":42}}' ;;
esac
exit 0
EOF

cat > "$SHIM/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF

# Inject the crash-durability boundary failure. The recovery-flag writer must
# never run after the WAL staging sync cannot be proven.
cat > "$SHIM/sync" <<'EOF'
#!/bin/sh
count=0
[ ! -f "$S99_TEST_SYNC_COUNT" ] || count=$(cat "$S99_TEST_SYNC_COUNT")
count=$((count + 1))
printf '%s\n' "$count" > "$S99_TEST_SYNC_COUNT"
[ "$count" -ne "$S99_TEST_SYNC_FAIL_AT" ]
EOF

cat > "$SHIM/flash_erase" <<'EOF'
#!/bin/sh
echo "called: $0 $*" >> "$S99_TEST_FLASH_LOG"
exit 0
EOF

cat > "$SHIM/nandwrite" <<'EOF'
#!/bin/sh
echo "called: $0 $*" >> "$S99_TEST_FLASH_LOG"
printf '\003' > "$S99_TEST_FLAG_STATE"
exit 0
EOF

cat > "$SHIM/fw_setenv" <<'EOF'
#!/bin/sh
echo "called: $0 $*" >> "$S99_TEST_FLASH_LOG"
if [ "${1:-}" = "firstboot" ] && [ "${2:-}" = "0" ]; then
    printf '0\n' > "$S99_TEST_ENV_STATE"
fi
exit 0
EOF

cat > "$SHIM/fw_printenv" <<'EOF'
#!/bin/sh
value=1
[ ! -f "$S99_TEST_ENV_STATE" ] || value=$(cat "$S99_TEST_ENV_STATE")
echo "firstboot=$value"
EOF

chmod 0755 "$SHIM"/*

/bin/sleep 300 &
ALIVE_PID=$!
OUT="$WORK/s99.out"
PENDING="$WORK/data/.firstboot-pending"

set +e
PATH="$SHIM:$PATH" \
S99_TEST_PID="$ALIVE_PID" \
S99_TEST_FLASH_LOG="$WORK/flash.log" \
S99_TEST_SYNC_COUNT="$WORK/sync.count" \
S99_TEST_SYNC_FAIL_AT=1 \
S99_TEST_FLAG_STATE="$WORK/flag.state" \
S99_TEST_ENV_STATE="$WORK/env.state" \
DCENTOS_SYSTEM_MTD="$WORK/mtd5" \
DCENTOS_LOCAL_RECOVERY_FLAGS_OFFSET=0 \
DCENTOS_FIRSTBOOT_PENDING_FILE="$PENDING" \
DCENTOS_BOOT_SUCCESS_WINDOW_S=1 \
    /bin/sh "$S99" start > "$OUT" 2>&1
rc=$?
set -e

if [ "$rc" -eq 0 ]; then
    cat "$OUT" >&2
    echo "FAIL: WAL durability failure did not fail closed" >&2
    exit 1
fi
grep -F "durable WAL intent was not proven" "$OUT" >/dev/null || {
    cat "$OUT" >&2
    echo "FAIL: durable-WAL refusal was not reported" >&2
    exit 1
}
if [ -s "$WORK/flash.log" ]; then
    cat "$OUT" >&2
    cat "$WORK/flash.log" >&2
    echo "FAIL: recovery flag/env mutation ran after WAL durability failure" >&2
    exit 1
fi
staging_left=$(find "$WORK/data" -name '.firstboot-pending.tmp.*' -print | head -1)
if [ -e "$PENDING" ] || [ -n "$staging_left" ]; then
    echo "FAIL: pre-publication WAL failure left a marker or staging file" >&2
    exit 1
fi

# Inject failure after rename. The marker may be visible, but the script must
# report directory durability as unproven and still refuse flash/env mutation.
: > "$WORK/flash.log"
rm -f "$WORK/sync.count" "$PENDING"
OUT_AFTER_RENAME="$WORK/s99-after-rename.out"
set +e
PATH="$SHIM:$PATH" \
S99_TEST_PID="$ALIVE_PID" \
S99_TEST_FLASH_LOG="$WORK/flash.log" \
S99_TEST_SYNC_COUNT="$WORK/sync.count" \
S99_TEST_SYNC_FAIL_AT=2 \
S99_TEST_FLAG_STATE="$WORK/flag.state" \
S99_TEST_ENV_STATE="$WORK/env.state" \
DCENTOS_SYSTEM_MTD="$WORK/mtd5" \
DCENTOS_LOCAL_RECOVERY_FLAGS_OFFSET=0 \
DCENTOS_FIRSTBOOT_PENDING_FILE="$PENDING" \
DCENTOS_BOOT_SUCCESS_WINDOW_S=1 \
    /bin/sh "$S99" start > "$OUT_AFTER_RENAME" 2>&1
rc=$?
set -e

if [ "$rc" -eq 0 ]; then
    cat "$OUT_AFTER_RENAME" >&2
    echo "FAIL: post-rename directory-sync failure did not fail closed" >&2
    exit 1
fi
grep -F "marker is visible but directory durability is unproven" "$OUT_AFTER_RENAME" >/dev/null || {
    cat "$OUT_AFTER_RENAME" >&2
    echo "FAIL: post-rename durability uncertainty was not reported" >&2
    exit 1
}
if [ -s "$WORK/flash.log" ]; then
    cat "$OUT_AFTER_RENAME" >&2
    cat "$WORK/flash.log" >&2
    echo "FAIL: mutation ran after post-rename directory-sync failure" >&2
    exit 1
fi
if [ ! -f "$PENDING" ]; then
    echo "FAIL: post-rename failure did not leave the visible WAL for replay" >&2
    exit 1
fi
staging_left=$(find "$WORK/data" -name '.firstboot-pending.tmp.*' -print | head -1)
if [ -n "$staging_left" ]; then
    echo "FAIL: post-rename failure left a staging file" >&2
    exit 1
fi

# Healthy path regression: both sync boundaries succeed, then and only then
# the recovery flag and env are committed and the replay marker is cleared.
: > "$WORK/flash.log"
rm -f "$WORK/sync.count" "$PENDING"
printf '\002' > "$WORK/flag.state"
printf '1\n' > "$WORK/env.state"
OUT_SUCCESS="$WORK/s99-success.out"
set +e
PATH="$SHIM:$PATH" \
S99_TEST_PID="$ALIVE_PID" \
S99_TEST_FLASH_LOG="$WORK/flash.log" \
S99_TEST_SYNC_COUNT="$WORK/sync.count" \
S99_TEST_SYNC_FAIL_AT=999 \
S99_TEST_FLAG_STATE="$WORK/flag.state" \
S99_TEST_ENV_STATE="$WORK/env.state" \
DCENTOS_SYSTEM_MTD="$WORK/mtd5" \
DCENTOS_LOCAL_RECOVERY_FLAGS_OFFSET=0 \
DCENTOS_FIRSTBOOT_PENDING_FILE="$PENDING" \
DCENTOS_BOOT_SUCCESS_WINDOW_S=1 \
    /bin/sh "$S99" start > "$OUT_SUCCESS" 2>&1
rc=$?
set -e

if [ "$rc" -ne 0 ]; then
    cat "$OUT_SUCCESS" >&2
    echo "FAIL: healthy WAL/commit path exited $rc" >&2
    exit 1
fi
grep -F "recovery flag promoted 0x02 -> 0x03" "$OUT_SUCCESS" >/dev/null || {
    cat "$OUT_SUCCESS" >&2
    echo "FAIL: healthy path did not commit recovery flag" >&2
    exit 1
}
if [ "$(od -An -tu1 "$WORK/flag.state" | tr -d ' ')" != "3" ]; then
    echo "FAIL: healthy path did not publish recovery flag 0x03" >&2
    exit 1
fi
if [ "$(cat "$WORK/env.state")" != "0" ]; then
    echo "FAIL: healthy path did not clear firstboot env" >&2
    exit 1
fi
if [ -e "$PENDING" ]; then
    echo "FAIL: healthy path retained WAL marker after both commits" >&2
    exit 1
fi

echo "AMLOGIC_S99UPGRADE_WAL_DURABILITY_OK"
