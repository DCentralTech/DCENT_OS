#!/bin/sh
# Prove that legacy install/restore compatibility paths have no authority.

set -eu

CDPATH=
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
SD_INSTALL="$SCRIPT_DIR/sd_nand_install.sh"
S19_REVERT="$SCRIPT_DIR/revert_to_stock_s19_am2.sh"

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); printf 'ok %d - %s\n' "$((PASS + FAIL))" "$1"; }
not_ok() { FAIL=$((FAIL + 1)); printf 'not ok %d - %s\n' "$((PASS + FAIL))" "$1" >&2; }

for script in "$SD_INSTALL" "$S19_REVERT"; do
    name=${script##*/}
    if [ -f "$script" ] && [ ! -L "$script" ] && [ -x "$script" ]; then
        ok "$name is a regular executable file"
    else
        not_ok "$name is missing, non-executable, or a symlink"
    fi
    if /bin/sh -n "$script"; then
        ok "$name parses as POSIX shell"
    else
        not_ok "$name has invalid shell syntax"
    fi

    noncomment_body=$(awk '
        NR == 1 && /^#!/ { next }
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*$/ { next }
        { print }
    ' "$script")
    for forbidden in \
        fw_setenv fw_printenv flash_erase nandwrite nanddump \
        ubiattach ubidetach ubiformat ubiupdatevol ubimkvol \
        dd mount umount rm reboot poweroff halt wget curl tar
    do
        if printf '%s\n' "$noncomment_body" |
            grep -E "(^|[^A-Za-z0-9_])${forbidden}([^A-Za-z0-9_]|$)" >/dev/null 2>&1; then
            not_ok "$name executable body contains forbidden authority: $forbidden"
        else
            ok "$name executable body excludes: $forbidden"
        fi
    done
    for forbidden_path in /dev/mtd /dev/ubi /proc/sysrq-trigger /sys/class/ubi
    do
        if printf '%s\n' "$noncomment_body" | grep -F -- "$forbidden_path" >/dev/null 2>&1; then
            not_ok "$name executable body names forbidden mutation path: $forbidden_path"
        else
            ok "$name executable body excludes path: $forbidden_path"
        fi
    done
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/dcent-legacy-writers.XXXXXX")
trap 'rm -rf "$WORK"' 0 1 2 15
SHIM="$WORK/shim"
LOG="$WORK/external.log"
INPUT="$WORK/input"
mkdir "$SHIM"
: > "$LOG"
printf '%s\n' REVERT > "$INPUT"
cat > "$SHIM/command-shim" <<'EOF'
#!/bin/sh
printf '%s\n' "$0 $*" >> "$DCENT_LEGACY_WRITER_EXTERNAL_LOG"
exit 99
EOF
chmod 0755 "$SHIM/command-shim"
for command_name in \
    fw_setenv fw_printenv flash_erase nandwrite nanddump \
    ubiattach ubidetach ubiformat ubiupdatevol ubimkvol \
    dd mount umount rm reboot poweroff halt wget curl tar \
    find head od sha256sum sync sleep date id uname grep cat
do
    ln -s command-shim "$SHIM/$command_name"
done

run_case() {
    script=$1
    label=$2
    expected_status=$3
    shift 3
    output="$WORK/$label.out"
    set +e
    PATH="$SHIM:$PATH" \
        DCENT_LEGACY_WRITER_EXTERNAL_LOG="$LOG" \
        DCENT_FORCE_INSTALL=1 \
        DCENT_ALLOW_UNSAFE_RECOVERY=1 \
        DCENT_STOCK_REVERT_ALLOW_UNVERIFIED=1 \
        "$script" "$@" < "$INPUT" > "$output" 2>&1
    status=$?
    set -e
    if [ "$status" -eq "$expected_status" ]; then
        ok "$label exits $expected_status"
    else
        not_ok "$label exits $status instead of $expected_status"
    fi
    if grep -F 'performs zero storage, environment, boot-selector, or restart operations' "$output" >/dev/null 2>&1; then
        ok "$label explains the zero-mutation boundary"
    else
        not_ok "$label omits the zero-mutation boundary"
    fi
}

run_case "$SD_INSTALL" sd-no-args 78
run_case "$SD_INSTALL" sd-help 0 --help
run_case "$SD_INSTALL" sd-plan 0 --plan
run_case "$SD_INSTALL" sd-dry-run 0 --dry-run
run_case "$SD_INSTALL" sd-auto-yes 78 --yes
run_case "$SD_INSTALL" sd-slot 78 --slot 2
run_case "$SD_INSTALL" sd-preserve-env 78 --preserve-env
run_case "$SD_INSTALL" sd-force-unsafe 78 --force-unsafe
run_case "$SD_INSTALL" sd-dry-run-plus-write 78 --dry-run --yes
run_case "$SD_INSTALL" sd-positional 78 candidate.img
run_case "$SD_INSTALL" sd-unknown 2 --invented-mode

run_case "$S19_REVERT" revert-no-args 78
run_case "$S19_REVERT" revert-help 0 --help
run_case "$S19_REVERT" revert-plan 0 --plan
run_case "$S19_REVERT" revert-dry-run 0 --dry-run
run_case "$S19_REVERT" revert-image-sha 78 stock.tar.gz deadbeef
run_case "$S19_REVERT" revert-dry-run-plus-image 78 --dry-run stock.tar.gz deadbeef
run_case "$S19_REVERT" revert-unknown 2 --invented-mode

if [ ! -s "$LOG" ]; then
    ok 'all execution cases invoke no intercepted external command'
else
    not_ok 'an execution case invoked an external command'
    cat "$LOG" >&2
fi

printf 'Legacy boot-environment writer containment: %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
