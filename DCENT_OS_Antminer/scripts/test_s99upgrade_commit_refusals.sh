#!/bin/sh
# Host-side regression tests for S99upgrade commit-refusal paths.
#
# No hardware, flash, /dev, or /etc paths are touched. The real init script is
# executed with temp-path redirects and command shims.
set -eu

DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
S99="$DIR/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade"

if [ ! -f "$S99" ]; then
    echo "SKIP: zynq S99upgrade not found at $S99" >&2
    exit 0
fi

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcent-s99-commit-refusals.XXXXXX")
ALIVE_PID=
cleanup() {
    if [ -n "$ALIVE_PID" ]; then
        kill "$ALIVE_PID" 2>/dev/null || true
        wait "$ALIVE_PID" 2>/dev/null || true
    fi
    rm -rf "$ROOT"
}
trap cleanup EXIT INT TERM

make_common_shims() {
    shim=$1

    cat >"$shim/ip" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "addr" ]; then
    echo "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500"
    echo "    inet 192.0.2.10/24 brd 192.0.2.255 scope global eth0"
    exit 0
fi
exit 1
EOF

    cat >"$shim/netstat" <<'EOF'
#!/bin/sh
echo "tcp        0      0 0.0.0.0:22            0.0.0.0:*               LISTEN"
exit 0
EOF

    cat >"$shim/pidof" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "dcentrald" ] && [ -n "${S99_TEST_PID:-}" ]; then
    echo "$S99_TEST_PID"
    exit 0
fi
exit 1
EOF

    cat >"$shim/wget" <<'EOF'
#!/bin/sh
case "$*" in
    *"/api/system/health"*)
        printf '%s' '{"daemon":{"uptime_s":42}}'
        exit 0
        ;;
    *)
        exit 0
        ;;
esac
EOF

    cat >"$shim/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF

    cat >"$shim/sync" <<'EOF'
#!/bin/sh
exit 0
EOF

    chmod 0755 "$shim"/*

    for cmd in grep sed awk tr head seq; do
        src=$(command -v "$cmd" || true)
        if [ -z "$src" ]; then
            echo "SKIP: required host test command not found: $cmd" >&2
            exit 0
        fi
        ln -s "$src" "$shim/$cmd"
    done
}

make_fw_printenv() {
    shim=$1
    mode=$2

    cat >"$shim/fw_printenv" <<EOF
#!/bin/sh
echo "\$*" >> "\$S99_TEST_WORK/fw_printenv.log"
case "\$1" in
    upgrade_stage)
        echo "upgrade_stage=1"
        exit 0
        ;;
esac
case "$mode" in
    bad_crc)
        echo "Warning: Bad CRC, using default environment"
        exit 0
        ;;
    verify_still_present)
        echo "firmware=1"
        echo "upgrade_stage=1"
        echo "first_boot=yes"
        exit 0
        ;;
    *)
        echo "firmware=1"
        echo "upgrade_stage=1"
        echo "first_boot=yes"
        exit 0
        ;;
esac
EOF
    chmod 0755 "$shim/fw_printenv"
}

make_fw_setenv() {
    shim=$1

    cat >"$shim/fw_setenv" <<'EOF'
#!/bin/sh
echo "$*" >> "$S99_TEST_WORK/fw_setenv.log"
exit 0
EOF
    chmod 0755 "$shim/fw_setenv"
}

run_case() {
    label=$1
    mode=$2
    expected_text=$3

    work="$ROOT/$label"
    shim="$work/shims"
    mkdir -p "$shim" "$work/etc/dcentos" "$work/etc/default" "$work/root/.ssh" "$work/data/keys/dropbear"
    : > "$work/mtd4"
    : > "$work/fw_env.config"
    : > "$work/dcentrald"
    : > "$work/fw_printenv.log"
    : > "$work/fw_setenv.log"
    chmod 0755 "$work/dcentrald"

    make_common_shims "$shim"
    case "$mode" in
        missing_fw_printenv)
            make_fw_setenv "$shim"
            ;;
        missing_fw_setenv)
            make_fw_printenv "$shim" "$mode"
            ;;
        missing_fw_env_config)
            make_fw_printenv "$shim" "$mode"
            make_fw_setenv "$shim"
            rm -f "$work/fw_env.config"
            ;;
        bad_crc|verify_still_present)
            make_fw_printenv "$shim" "$mode"
            make_fw_setenv "$shim"
            ;;
        *)
            echo "FAIL: unknown mode $mode" >&2
            exit 1
            ;;
    esac

    /bin/sleep 300 &
    ALIVE_PID=$!

    out="$work/s99.out"
    set +e
    # Keep host /usr/sbin/fw_setenv out of this fixture; each case must be
    # controlled only by the shims above.
    PATH="$shim" \
    S99_TEST_WORK="$work" \
    S99_TEST_PID="$ALIVE_PID" \
    DCENTOS_MTD4_NODE="$work/mtd4" \
    DCENTOS_FW_ENV_CONFIG="$work/fw_env.config" \
    DCENTOS_DCENTRALD_BIN="$work/dcentrald" \
    DCENTOS_UPGRADE_COMMIT_MARKER="$work/commit-marker" \
    DCENTOS_BOOT_SUCCESS_WINDOW_S=1 \
    DCENTOS_FW_COMMIT_RETRIES=1 \
    DCENTOS_FW_COMMIT_SETTLE_S=0 \
    DCENTOS_CONFIG_DIR="$work/etc/dcentos" \
    DCENTOS_DROPBEAR_DEFAULTS="$work/etc/default/dropbear" \
    DCENTOS_ROOT_AUTHORIZED_KEYS="$work/root/.ssh/authorized_keys" \
    DCENTOS_DATA_AUTHORIZED_KEYS="$work/data/keys/dropbear/authorized_keys" \
        /bin/sh "$S99" start >"$out" 2>&1
    rc=$?
    set -e

    kill "$ALIVE_PID" 2>/dev/null || true
    wait "$ALIVE_PID" 2>/dev/null || true
    ALIVE_PID=

    if [ "$rc" -ne 0 ]; then
        cat "$out" >&2
        echo "FAIL: $label exited $rc" >&2
        exit 1
    fi

    grep -F "$expected_text" "$out" >/dev/null || {
        cat "$out" >&2
        echo "FAIL: $label missing expected refusal text: $expected_text" >&2
        exit 1
    }

    if ! grep -Fx "blocked" "$work/commit-marker" >/dev/null 2>&1; then
        cat "$out" >&2
        echo "FAIL: $label did not write blocked commit marker" >&2
        exit 1
    fi
    if grep -Fx "committed" "$work/commit-marker" >/dev/null 2>&1; then
        cat "$out" >&2
        echo "FAIL: $label wrote committed marker on a refusal path" >&2
        exit 1
    fi

    echo "ok - $label"
}

run_case missing_fw_setenv missing_fw_setenv "fw_setenv missing"
run_case missing_fw_printenv missing_fw_printenv "fw_printenv missing"
run_case missing_fw_env_config missing_fw_env_config "/etc/fw_env.config missing"
run_case bad_crc bad_crc "current U-Boot env reads Bad CRC / default"
run_case verify_still_present verify_still_present "upgrade_stage could NOT be cleared after 1 fw_setenv attempts"

echo "S99UPGRADE_COMMIT_REFUSALS_OK"
