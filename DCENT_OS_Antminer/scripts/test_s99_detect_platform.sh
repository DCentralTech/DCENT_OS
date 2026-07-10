#!/bin/sh
# Pins detect_platform() in S99verify — the OTA/boot health-verdict script whose
# platform classification drives the per-platform upgrade/rollback health checks
# (a mis-classified platform routes the wrong V-checks and can misjudge a
# rollback). This extracts the REAL function from the shipped script and drives
# it with every target SKU's stamps, so a regression in the board_family /
# board_target classification is caught offline instead of on a live upgrade.
#
# It also asserts the S19j-Pro canonical `am2-s19jpro-zynq` board_target is
# handled by the board_target FALLBACK here (S99verify uses an `am2-*` glob, so
# unlike the S82dcentrald exact-list it already covers the canonical) — the
# fourth consumer of that routing key, verified alongside the resolver, the
# acceptance harness, and the init script.
set -u

here=$(CDPATH= cd "$(dirname "$0")" && pwd)
root=$(CDPATH= cd "$here/.." && pwd)
SRC="$root/br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99verify"
[ -r "$SRC" ] || { echo "FAIL: S99verify not found at $SRC"; exit 1; }

# Extract the detect_platform() { ... } body (up to the first column-0 close).
fn=$(awk '/^detect_platform\(\) \{/{p=1} p{print} p&&/^\}/{exit}' "$SRC")
case "$fn" in
    *"detect_platform()"*"PLATFORM"*) : ;;
    *) echo "FAIL: could not extract detect_platform() from S99verify"; exit 1 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
BOARD_FAMILY_FILE="$tmp/board_family"
BOARD_TARGET_FILE="$tmp/board_target"
eval "$fn"

fails=0
check() { # $1=desc $2=expect
    if [ "$PLATFORM" = "$2" ]; then
        echo "  PASS: $1 -> PLATFORM=$PLATFORM"
    else
        echo "  FAIL: $1 -> PLATFORM=$PLATFORM (want $2)"
        fails=$((fails + 1))
    fi
}
setfam() { printf '%s\n' "$1" > "$BOARD_FAMILY_FILE"; rm -f "$BOARD_TARGET_FILE"; }
setbt()  { rm -f "$BOARD_FAMILY_FILE"; printf '%s\n' "$1" > "$BOARD_TARGET_FILE"; }

# --- Normal path: board_family stamped (what the per-SKU post-build writes) ---
setfam zynq-bm3-am2;    detect_platform; check "board_family=zynq-bm3-am2"    am2
setfam am2-s19jpro;     detect_platform; check "board_family=am2-s19jpro"     am2
setfam am3-aml-s21;     detect_platform; check "board_family=am3-aml-s21"     am3-aml
setfam am3-aml-s19jpro; detect_platform; check "board_family=am3-aml-s19jpro" am3-aml
setfam am3-bb-s19jpro;  detect_platform; check "board_family=am3-bb-s19jpro"  am3-bb
setfam cv1835-s19jpro;  detect_platform; check "board_family=cv1835-s19jpro"  cv1835

# --- board_target fallback (board_family absent/corrupt) ---
# The canonical S19j Pro board_target MUST classify as am2 here (glob am2-*).
setbt am2-s19jpro-zynq; detect_platform; check "fallback board_target=am2-s19jpro-zynq" am2
setbt am2-s19j;         detect_platform; check "fallback board_target=am2-s19j"         am2
setbt am3-s21;          detect_platform; check "fallback board_target=am3-s21"          am3-aml

if [ "$fails" -eq 0 ]; then
    echo "PASS: detect_platform classifies every target platform (incl. canonical am2-s19jpro-zynq fallback)"
    exit 0
fi
echo "FAIL: $fails detect_platform case(s) failed"
exit 1
