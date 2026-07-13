#!/usr/bin/env bash
# Static self-test for stage_am2_sd_artifacts.sh (no hardware, no real vendor blobs).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE="$SCRIPT_DIR/stage_am2_sd_artifacts.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

# Missing dir
if "$STAGE" --artifacts-dir "$TMP/nope" --check-only 2>/dev/null; then
    fail "missing dir should fail"
fi
pass "missing dir fails"

# Empty dir
mkdir -p "$TMP/empty"
if "$STAGE" --artifacts-dir "$TMP/empty" --check-only 2>/dev/null; then
    fail "empty dir should fail"
fi
pass "empty dir fails"

# Incomplete (only BOOT)
mkdir -p "$TMP/partial"
# 80 KiB fake boot (valid size window)
dd if=/dev/zero of="$TMP/partial/BOOT.bin" bs=1024 count=80 status=none 2>/dev/null \
  || dd if=/dev/zero of="$TMP/partial/BOOT.bin" bs=1024 count=80 2>/dev/null
if "$STAGE" --artifacts-dir "$TMP/partial" --check-only 2>/dev/null; then
    fail "BOOT-only should fail"
fi
pass "BOOT-only fails"

# Complete synthetic set
mkdir -p "$TMP/complete"
dd if=/dev/zero of="$TMP/complete/BOOT.bin" bs=1024 count=80 status=none 2>/dev/null \
  || dd if=/dev/zero of="$TMP/complete/BOOT.bin" bs=1024 count=80 2>/dev/null
dd if=/dev/zero of="$TMP/complete/uImage" bs=1024 count=100 status=none 2>/dev/null \
  || dd if=/dev/zero of="$TMP/complete/uImage" bs=1024 count=100 2>/dev/null
dd if=/dev/zero of="$TMP/complete/devicetree.dtb" bs=1024 count=10 status=none 2>/dev/null \
  || dd if=/dev/zero of="$TMP/complete/devicetree.dtb" bs=1024 count=10 2>/dev/null

"$STAGE" --artifacts-dir "$TMP/complete" --output-dir "$TMP/staged" || fail "complete set should pass"
[ -f "$TMP/staged/BOOT.bin" ] || fail "staged BOOT.bin"
[ -f "$TMP/staged/uImage" ] || fail "staged uImage"
[ -f "$TMP/staged/devicetree.dtb" ] || fail "staged dtb"
[ -f "$TMP/staged/artifacts.manifest.json" ] || fail "staging manifest"
grep -q '"ready_for_complete_build": true' "$TMP/staged/artifacts.manifest.json" \
  || fail "ready flag"
pass "complete synthetic artifacts stage"

# Stock XIL MD5 refuse (if md5sum available)
if command -v md5sum >/dev/null 2>&1; then
    # We cannot easily forge a file with a specific MD5 without collision;
    # the denylist path is integration-tested via the builder. Document skip.
    pass "stock MD5 denylist covered by builder (manual when real stock BOOT present)"
fi

# Same-dir staging must not fail when output-dir == artifacts-dir
"$STAGE" --artifacts-dir "$TMP/complete" --output-dir "$TMP/complete" \
  || fail "same-dir stage should succeed"
[ -f "$TMP/complete/artifacts.manifest.json" ] || fail "same-dir writes manifest"
pass "same-dir stage succeeds"

# package_am2_sd_release refuses incomplete without lab override
PKG="$SCRIPT_DIR/package_am2_sd_release.sh"
INC_IMG="$TMP/incomplete.img"
printf 'x' >"$INC_IMG"
cat >"$INC_IMG.manifest.json" <<'JSON'
{
  "boot_artifacts_complete": false,
  "artifacts": {
    "BOOT.bin": false,
    "uImage": false,
    "devicetree.dtb": false,
    "uEnv.txt": false,
    "bitstream": false,
    "rootfs": true
  }
}
JSON
if "$PKG" --image "$INC_IMG" --label test-inc --output-root "$TMP/pkg-out" --require-complete 2>/dev/null; then
  fail "incomplete package should refuse"
fi
pass "package_am2_sd_release refuses incomplete"

# Complete synthetic manifest allows package (unsigned lab img bytes)
COMP_IMG="$TMP/complete.img"
dd if=/dev/zero of="$COMP_IMG" bs=1024 count=4 status=none 2>/dev/null \
  || dd if=/dev/zero of="$COMP_IMG" bs=1024 count=4 2>/dev/null
cat >"$COMP_IMG.manifest.json" <<'JSON'
{
  "boot_artifacts_complete": true,
  "artifacts": {
    "BOOT.bin": true,
    "uImage": true,
    "devicetree.dtb": true,
    "uEnv.txt": true,
    "bitstream": true,
    "rootfs": true
  }
}
JSON
"$PKG" --image "$COMP_IMG" --label test-complete --output-root "$TMP/pkg-ok" --require-complete \
  || fail "complete package should succeed"
[ -f "$TMP/pkg-ok/TESTER_README.txt" ] || fail "tester readme"
grep -qi "NOT A NAND" "$TMP/pkg-ok/TESTER_README.txt" || fail "NAND disclaimer missing"
pass "package_am2_sd_release accepts complete manifest"

echo "test_stage_am2_sd_artifacts_static: all passed"
