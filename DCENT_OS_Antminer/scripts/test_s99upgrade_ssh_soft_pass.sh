#!/bin/sh
# Host-side regression test for S99upgrade's first-boot/release-image SSH
# soft-pass policy. No hardware, SSH, flash, /dev, or /etc paths are touched:
# the real init script is run with temp-path seams and command shims.

set -eu

DIR=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
S99="$DIR/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99upgrade"

if [ ! -f "$S99" ]; then
    echo "SKIP: zynq S99upgrade not found at $S99" >&2
    exit 0
fi

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/dcent-s99-ssh-soft-pass.XXXXXX")
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
    shim_dir=$1

    cat >"$shim_dir/ip" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "addr" ]; then
    echo "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500"
    echo "    inet 192.0.2.10/24 brd 192.0.2.255 scope global eth0"
    exit 0
fi
exit 1
EOF

    cat >"$shim_dir/netstat" <<'EOF'
#!/bin/sh
# Deliberately no :22 listener. The test decides whether that is a soft-pass
# or a hard fail through marker files in the redirected config directory.
exit 0
EOF

    cat >"$shim_dir/pidof" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "dcentrald" ] && [ -n "${S99_TEST_PID:-}" ]; then
    echo "$S99_TEST_PID"
    exit 0
fi
exit 1
EOF

    cat >"$shim_dir/wget" <<'EOF'
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

    cat >"$shim_dir/sleep" <<'EOF'
#!/bin/sh
exit 0
EOF

    cat >"$shim_dir/sync" <<'EOF'
#!/bin/sh
exit 0
EOF

    chmod 0755 "$shim_dir"/*
}

make_fw_shims() {
    shim_dir=$1

    cat >"$shim_dir/fw_printenv" <<'EOF'
#!/bin/sh
echo "$*" >> "$S99_TEST_WORK/fw_printenv.log"
if [ "${1:-}" = "upgrade_stage" ]; then
    if [ -f "$S99_TEST_WORK/upgrade_stage.present" ]; then
        echo "upgrade_stage=1"
        exit 0
    fi
    exit 1
fi
echo "firmware=1"
if [ -f "$S99_TEST_WORK/upgrade_stage.present" ]; then
    echo "upgrade_stage=1"
fi
echo "first_boot=yes"
exit 0
EOF

    cat >"$shim_dir/fw_setenv" <<'EOF'
#!/bin/sh
echo "$*" >> "$S99_TEST_WORK/fw_setenv.log"
if [ "${1:-}" = "upgrade_stage" ]; then
    rm -f "$S99_TEST_WORK/upgrade_stage.present"
fi
exit 0
EOF

    chmod 0755 "$shim_dir/fw_printenv" "$shim_dir/fw_setenv"
}

run_case() {
    label=$1
    mode=$2
    expected_marker=$3
    expected_text_1=$4
    expected_text_2=${5:-}

    work="$ROOT/$label"
    shim="$work/shims"
    config_dir="$work/etc/dcentos"
    default_dir="$work/etc/default"
    root_ssh_dir="$work/root/.ssh"
    data_ssh_dir="$work/data/keys/dropbear"
    mkdir -p "$shim" "$config_dir" "$default_dir" "$root_ssh_dir" "$data_ssh_dir"
    : > "$work/mtd4"
    : > "$work/fw_env.config"
    : > "$work/dcentrald"
    : > "$work/upgrade_stage.present"
    : > "$work/fw_setenv.log"
    chmod 0755 "$work/dcentrald"

    case "$mode" in
        first_boot_grace)
            : > "$config_dir/first-boot-grace"
            ;;
        release_image_keyonly)
            : > "$config_dir/release-image"
            printf '%s\n' 'DROPBEAR_ARGS="-s"' > "$default_dir/dropbear"
            ;;
        no_marker)
            ;;
        *)
            echo "FAIL: unknown test mode $mode" >&2
            exit 1
            ;;
    esac

    make_common_shims "$shim"
    make_fw_shims "$shim"

    /bin/sleep 300 &
    ALIVE_PID=$!

    out="$work/s99.out"
    set +e
    PATH="$shim:$PATH" \
    S99_TEST_WORK="$work" \
    S99_TEST_PID="$ALIVE_PID" \
    DCENTOS_MTD4_NODE="$work/mtd4" \
    DCENTOS_FW_ENV_CONFIG="$work/fw_env.config" \
    DCENTOS_DCENTRALD_BIN="$work/dcentrald" \
    DCENTOS_UPGRADE_COMMIT_MARKER="$work/commit-marker" \
    DCENTOS_BOOT_SUCCESS_WINDOW_S=1 \
    DCENTOS_FW_COMMIT_RETRIES=1 \
    DCENTOS_FW_COMMIT_SETTLE_S=0 \
    DCENTOS_CONFIG_DIR="$config_dir" \
    DCENTOS_DROPBEAR_DEFAULTS="$default_dir/dropbear" \
    DCENTOS_ROOT_AUTHORIZED_KEYS="$root_ssh_dir/authorized_keys" \
    DCENTOS_DATA_AUTHORIZED_KEYS="$data_ssh_dir/authorized_keys" \
        sh "$S99" start >"$out" 2>&1
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

    grep -F "$expected_text_1" "$out" >/dev/null || {
        cat "$out" >&2
        echo "FAIL: $label missing expected output: $expected_text_1" >&2
        exit 1
    }
    if [ -n "$expected_text_2" ]; then
        grep -F "$expected_text_2" "$out" >/dev/null || {
            cat "$out" >&2
            echo "FAIL: $label missing expected output: $expected_text_2" >&2
            exit 1
        }
    fi

    if ! grep -Fx "$expected_marker" "$work/commit-marker" >/dev/null 2>&1; then
        cat "$out" >&2
        echo "FAIL: $label expected commit marker '$expected_marker'" >&2
        exit 1
    fi

    case "$expected_marker" in
        committed)
            grep -Fx "upgrade_stage" "$work/fw_setenv.log" >/dev/null || {
                cat "$out" >&2
                echo "FAIL: $label did not clear upgrade_stage" >&2
                exit 1
            }
            ;;
        blocked)
            if [ -s "$work/fw_setenv.log" ]; then
                cat "$out" >&2
                echo "FAIL: $label called fw_setenv despite blocked SSH policy:" >&2
                cat "$work/fw_setenv.log" >&2
                exit 1
            fi
            ;;
    esac

    echo "ok - $label"
}

run_case \
    first_boot_grace \
    first_boot_grace \
    committed \
    "[SOFT-PASS] SSH:22 not listening yet (first-boot grace active)"

run_case \
    release_image_keyonly \
    release_image_keyonly \
    committed \
    "[SOFT-PASS] SSH:22 locked by release-image policy; dashboard(:80)+API(:8080) checks below prove manageability" \
    "[SOFT-PASS] SSH key-only mode without keys on release image (locked by policy; dashboard/API prove manageability)"

run_case \
    no_marker \
    no_marker \
    blocked \
    "[FAIL] SSH server not listening on port 22"

echo "S99UPGRADE_SSH_SOFT_PASS_OK"
