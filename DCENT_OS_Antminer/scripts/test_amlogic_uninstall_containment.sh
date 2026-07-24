#!/bin/sh
# Prove that the shipped Amlogic uninstall compatibility path has no authority.

set -eu

CDPATH=
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd)
UNINSTALL="$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/uninstall.sh"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok %d - %s\n' "$PASS" "$1"; }
not_ok() { FAIL=$((FAIL + 1)); printf 'not ok %d - %s\n' "$((PASS + FAIL))" "$1" >&2; }

if [ -f "$UNINSTALL" ] && [ ! -L "$UNINSTALL" ] && [ -x "$UNINSTALL" ]; then
    ok 'Amlogic uninstall entry point is a regular executable file'
else
    not_ok 'Amlogic uninstall entry point is missing, non-executable, or a symlink'
fi

if /bin/sh -n "$UNINSTALL"; then
    ok 'Amlogic uninstall entry point parses as POSIX shell'
else
    not_ok 'Amlogic uninstall entry point has invalid shell syntax'
fi

noncomment_body=$(awk '
    NR == 1 && /^#!/ { next }
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*$/ { next }
    { print }
' "$UNINSTALL")
for forbidden in \
    fw_setenv fw_printenv nandwrite nanddump flash_erase dd \
    mount umount pivot_root chroot rm reboot poweroff halt
do
    if printf '%s\n' "$noncomment_body" |
        grep -E "^[[:space:]]*${forbidden}([[:space:]]|$)" >/dev/null 2>&1; then
        not_ok "Amlogic uninstall executable body contains forbidden authority: $forbidden"
    else
        ok "Amlogic uninstall executable body excludes: $forbidden"
    fi
done
for forbidden_path in /dev/nand_env /dev/mtd /mnt/nvdata /proc/sysrq-trigger
do
    if printf '%s\n' "$noncomment_body" | grep -F -- "$forbidden_path" >/dev/null 2>&1; then
        not_ok "Amlogic uninstall executable body names forbidden mutation path: $forbidden_path"
    else
        ok "Amlogic uninstall executable body excludes path: $forbidden_path"
    fi
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/dcent-amlogic-uninstall.XXXXXX")
trap 'rm -rf "$WORK"' 0 1 2 15
SHIM="$WORK/shim"
LOG="$WORK/external.log"
mkdir "$SHIM"
: > "$LOG"
cat > "$SHIM/command-shim" <<'EOF'
#!/bin/sh
printf '%s\n' "$0 $*" >> "$AMLOGIC_UNINSTALL_EXTERNAL_LOG"
exit 99
EOF
chmod 0755 "$SHIM/command-shim"
for command_name in \
    fw_setenv fw_printenv nandwrite nanddump flash_erase dd \
    mount umount pivot_root chroot rm reboot poweroff halt sync sleep date
do
    ln -s command-shim "$SHIM/$command_name"
done

run_case() {
    label=$1
    expected_status=$2
    shift 2
    output="$WORK/$label.out"
    set +e
    PATH="$SHIM:$PATH" \
        AMLOGIC_UNINSTALL_EXTERNAL_LOG="$LOG" \
        DCENT_FORCE_UNINSTALL=1 \
        DCENT_ALLOW_UNSAFE_RECOVERY=1 \
        "$UNINSTALL" "$@" >"$output" 2>&1
    status=$?
    set -e
    if [ "$status" -eq "$expected_status" ]; then
        ok "$label exits $expected_status"
    else
        not_ok "$label exits $status instead of $expected_status"
    fi
    if grep -F 'zero environment, storage, rootfs, or reboot operations' "$output" >/dev/null 2>&1; then
        ok "$label explains the zero-mutation boundary"
    else
        not_ok "$label omits the zero-mutation boundary"
    fi
}

run_case default-plan 0
run_case explicit-plan 0 --plan
run_case dry-run 0 --dry-run
run_case help 0 --help
run_case confirmed-refusal 78 --confirm-uninstall
run_case unknown-refusal 2 --invented-mode

if [ ! -s "$LOG" ]; then
    ok 'all execution cases invoke no intercepted external command'
else
    not_ok 'an execution case invoked an external command'
    cat "$LOG" >&2
fi

if grep -F 'captured LuxOS bad-CRC procedure is not a DCENT_OS recovery contract' "$UNINSTALL" >/dev/null 2>&1; then
    ok 'refusal names the invalidated evidence boundary'
else
    not_ok 'refusal does not name the invalidated evidence boundary'
fi

printf 'Amlogic uninstall containment tests: %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
