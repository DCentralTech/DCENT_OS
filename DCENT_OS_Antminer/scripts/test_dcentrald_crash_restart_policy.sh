#!/bin/sh
# Offline contract for persistent dcentrald hardware-session admission.

set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
BOARD_DIR="$PROJECT_DIR/br2_external_dcentos/board"
CONFIG_DIR="$PROJECT_DIR/br2_external_dcentos/configs"
HELPER="$BOARD_DIR/common/rootfs-overlay/usr/libexec/dcentos/dcentrald-session-latch.sh"
FAILURES=0
CHECKED=0
CONFIGS_CHECKED=0

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    FAILURES=$((FAILURES + 1))
}

pass() {
    printf 'PASS: %s\n' "$*"
}

if [ ! -f "$HELPER" ]; then
    fail 'canonical hardware-session latch helper is missing'
elif ! sh -n "$HELPER"; then
    fail 'canonical hardware-session latch helper is not POSIX-shell parseable'
else
    pass 'canonical hardware-session latch helper is POSIX-shell parseable'
    if sh "$HELPER" self-test; then
        pass 'session-latch transition and persistence fixtures pass'
    else
        fail 'session-latch transition or persistence fixture failed'
    fi
fi

if [ -f "$HELPER" ]; then
    grep -Fq 'No process exit status is accepted as a physical SafeOff receipt' "$HELPER" \
        || fail 'helper does not state that process exit is not physical disposition evidence'
    grep -Fq 'mkdir "$LOCK_DIR"' "$HELPER" \
        || fail 'helper lacks atomic cross-process admission serialization'
    grep -Fq 'sync_state' "$HELPER" \
        || fail 'helper lacks a persistence barrier'
    grep -Fq 'expected-zero-awaiting-typed-disposition' "$HELPER" \
        || fail 'helper does not retain expected zero exits as unresolved'
    grep -Fq 'admit_update_window()' "$HELPER" \
        || fail 'helper lacks serialized manual-resolution update admission'
    grep -Fq 'update_transaction_is_absent || return 1' "$HELPER" \
        || fail 'daemon admission does not refuse an active update transaction'
    if grep -Eq '^[[:space:]]*clean\)|mark_session_clean' "$HELPER"; then
        fail 'helper exposes exit-status-based session clearing'
    else
        pass 'helper exposes no exit-status-based session clearing'
    fi
fi

for defconfig in "$CONFIG_DIR"/*_defconfig; do
    [ -f "$defconfig" ] || continue
    if ! grep -q '^BR2_ROOTFS_OVERLAY=' "$defconfig"; then
        continue
    fi
    CONFIGS_CHECKED=$((CONFIGS_CHECKED + 1))
    relative=${defconfig#"$PROJECT_DIR"/}
    if grep -Fq 'BR2_ROOTFS_OVERLAY="$(BR2_EXTERNAL_DCENTOS_PATH)/board/common/rootfs-overlay ' "$defconfig" \
        || grep -Fq 'BR2_ROOTFS_OVERLAY="$(BR2_EXTERNAL_DCENTOS_PATH)/board/common/rootfs-overlay"' "$defconfig"; then
        pass "$relative installs the canonical common overlay first"
    else
        fail "$relative does not install the canonical common overlay first"
    fi
done

OLD_IFS=$IFS
IFS='
'
for supervisor in $(find "$BOARD_DIR" -type f -name S82dcentrald 2>/dev/null | sort); do
    CHECKED=$((CHECKED + 1))
    relative=${supervisor#"$PROJECT_DIR"/}

    if sh -n "$supervisor"; then
        pass "$relative is POSIX-shell parseable"
    else
        fail "$relative is not POSIX-shell parseable"
    fi

    if grep -Eq 'MAX_CRASH_RESTARTS|RESTART_DELAY|CRASH_COUNT' "$supervisor"; then
        fail "$relative retains automatic crash-restart state"
    else
        pass "$relative has no automatic crash-restart state"
    fi

    if grep -Eq 'ORPHAN_PIDS|Killing orphaned|dcentrald exited cleanly|automatic restart disabled' "$supervisor"; then
        fail "$relative retains kill-and-replace or exit-status disposition logic"
    else
        pass "$relative has no kill-and-replace or exit-status disposition logic"
    fi

    # Some targets deliberately retain the historical init filename only as a
    # typed negative capability. They never acquire hardware ownership, create
    # a daemon log, or publish a process supervisor, so admission/latch rules
    # for an activating S82 do not apply.
    if grep -Fxq 'DCENT_RUNTIME_OWNER_POLICY=not-implemented-refusal' "$supervisor"; then
        grep -Fq 'runtime hardware ownership NOT IMPLEMENTED; daemon start refused' "$supervisor" \
            || fail "$relative marks refusal policy without the canonical operator-visible refusal"
        if grep -Fq 'SESSION_LATCH_HELPER=' "$supervisor" \
            || grep -Eq '^[[:space:]]*(exec[[:space:]]+)?(/usr/local/bin/)?dcentrald([[:space:]]|$)' "$supervisor"; then
            fail "$relative refusal policy can still bind or launch a hardware-owner supervisor"
        else
            pass "$relative is a typed non-activating runtime-owner refusal"
        fi
        continue
    fi

    grep -Fq 'EXPECTFILE="/var/run/dcentrald.expected_exit.pid"' "$supervisor" \
        || fail "$relative keeps expected-exit state outside protected runtime storage"
    grep -Fq 'SESSION_LATCH_HELPER="/usr/libexec/dcentos/dcentrald-session-latch.sh"' "$supervisor" \
        || fail "$relative does not bind the canonical session-latch helper"
    grep -Fq 'SESSION_TOKEN=$(/bin/sh "$SESSION_LATCH_HELPER" prepare' "$supervisor" \
        || fail "$relative does not synchronously prepare persistent admission"
    grep -Fq '"$SESSION_LATCH_HELPER" supervise "$SESSION_TOKEN"' "$supervisor" \
        || fail "$relative does not pass the serialized admission token to the common supervisor"
    grep -Fq '"$SESSION_LATCH_HELPER" abandon "$SESSION_TOKEN" supervisor-launch-failed' "$supervisor" \
        || fail "$relative does not latch failed supervisor publication"
    grep -Fq '"$SESSION_LATCH_HELPER" latch forced-stop-timeout' "$supervisor" \
        || fail "$relative does not latch before a forced stop"
    case "$relative" in
        br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S82dcentrald)
            grep -Fq 'restart is refused before stop' "$supervisor" \
                || fail "$relative can stop a healthy owner before refusing unsafe readmission"
            ;;
        *)
            grep -Fq '"$0" stop || exit $?' "$supervisor" \
                || fail "$relative restart does not propagate stop failure"
            grep -Fq 'exec "$0" start' "$supervisor" \
                || fail "$relative restart does not propagate start refusal"
            ;;
    esac

    RUNNING_LINE=$(grep -n 'RUNNING_PIDS=$(pidof dcentrald' "$supervisor" | head -n 1 | cut -d: -f1)
    PREPARE_LINE=$(grep -n 'SESSION_TOKEN=$(/bin/sh "$SESSION_LATCH_HELPER" prepare' "$supervisor" | head -n 1 | cut -d: -f1)
    SUPERVISE_LINE=$(grep -n '"$SESSION_LATCH_HELPER" supervise "$SESSION_TOKEN"' "$supervisor" | head -n 1 | cut -d: -f1)
    if [ -n "$RUNNING_LINE" ] && [ -n "$PREPARE_LINE" ] && [ -n "$SUPERVISE_LINE" ] \
        && [ "$RUNNING_LINE" -lt "$PREPARE_LINE" ] && [ "$PREPARE_LINE" -lt "$SUPERVISE_LINE" ]; then
        pass "$relative refuses a live owner, persists admission, then publishes the supervisor"
    else
        fail "$relative admission ordering is not live-owner -> persistent marker -> supervisor"
    fi
done
IFS=$OLD_IFS

[ "$CONFIGS_CHECKED" -gt 0 ] || fail 'no Buildroot overlay defconfigs were discovered'
[ "$CHECKED" -gt 0 ] || fail 'no shipped S82dcentrald supervisors were discovered'

for platform in zynq amlogic; do
    web_root="$BOARD_DIR/$platform/rootfs-overlay/root/web"
    server="$web_root/server.py"
    mcp="$web_root/mcp_server.py"
    recovery="$web_root/static/recovery.html"
    diagnostic="$web_root/static/diagnostic.html"

    grep -Fq 'result = subprocess.run(' "$mcp" \
        || fail "$platform MCP service control is not synchronous"
    grep -Fq '"returncode": result.returncode' "$mcp" \
        || fail "$platform MCP service control does not report the init result"
    if [ "$platform" = amlogic ]; then
        grep -Fq '"status": "manual_resolution_required"' "$server" \
            || fail 'amlogic dashboard does not expose the manual-resolution policy'
        grep -Fq '/etc/init.d/S37board_setup start' "$recovery" \
            || fail 'amlogic recovery does not re-establish the boot-safe baseline'
        if grep -Fq 'Run guarded restart' "$recovery" \
            || grep -Fq 'Run guarded restart' "$diagnostic" \
            || grep -Fq '["/etc/init.d/S82dcentrald", "restart"]' "$server"; then
            fail 'amlogic web recovery advertises or executes forbidden restart'
        fi
    else
        grep -Fq 'result = subprocess.run(' "$server" \
            || fail "$platform dashboard service control is not synchronous"
        grep -Fq 'if result.returncode != 0:' "$server" \
            || fail "$platform dashboard service control does not propagate init refusal"
        grep -Fq '"status": "restart_refused"' "$server" \
            || fail "$platform dashboard service control lacks a stable refusal result"
        grep -Fq 'Run guarded restart' "$recovery" \
            || fail "$platform recovery UI lacks guarded restart control"
        grep -Fq 'Run guarded restart' "$diagnostic" \
            || fail "$platform diagnostic UI lacks guarded restart control"
    fi
done

REST_RS="$PROJECT_DIR/dcentrald/dcentrald-api/src/rest.rs"
REST_LATE_RS="$PROJECT_DIR/dcentrald/dcentrald-api/src/rest/late.rs"
RESTART_RS="$PROJECT_DIR/dcentrald/dcentrald/src/restart.rs"
grep -Fq 'const DAEMON_RESTART_REFUSAL' "$REST_RS" \
    || fail 'in-daemon control planes lack the canonical restart refusal'
grep -Fq 'StatusCode::CONFLICT' "$REST_LATE_RS" \
    || fail 'REST restart does not report a conflict'
grep -Fq 'Automatic daemon restart refused' "$RESTART_RS" \
    || fail 'automatic recovery can still claim to schedule process replacement'
if grep -Rq 'trigger_daemon_restart\|build_daemon_restart_command' \
    "$PROJECT_DIR/dcentrald/dcentrald-api/src" \
    "$PROJECT_DIR/dcentrald/dcentrald-api/tests"; then
    fail 'in-process restart implementation remains reachable'
else
    pass 'REST, gRPC, and CGMiner preserve the live owner and refuse unsafe replacement'
fi

if [ "$FAILURES" -ne 0 ]; then
    printf 'dcentrald persistent admission policy failed: %s failure(s), %s supervisor(s), %s defconfig(s)\n' \
        "$FAILURES" "$CHECKED" "$CONFIGS_CHECKED" >&2
    exit 1
fi

printf 'dcentrald persistent admission policy passed across %s supervisor(s) and %s defconfig(s).\n' \
    "$CHECKED" "$CONFIGS_CHECKED"
