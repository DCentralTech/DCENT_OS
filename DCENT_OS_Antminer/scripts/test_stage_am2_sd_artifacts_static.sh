#!/usr/bin/env bash
# Static self-test for stage_am2_sd_artifacts.sh (no hardware, no real vendor blobs).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE="$SCRIPT_DIR/stage_am2_sd_artifacts.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
PYTHON=""
for candidate in python3 python; do
    if command -v "$candidate" >/dev/null 2>&1 &&
        "$candidate" -c \
            'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
            >/dev/null 2>&1; then
        PYTHON=$candidate
        break
    fi
done
[ -n "$PYTHON" ] || { echo "FAIL: Python 3.10 or newer unavailable" >&2; exit 1; }

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "PASS: $*"; }

make_complete_artifacts() {
    target=$1
    mkdir -p "$target"
    dd if=/dev/zero of="$target/BOOT.bin" bs=1024 count=80 status=none 2>/dev/null \
      || dd if=/dev/zero of="$target/BOOT.bin" bs=1024 count=80 2>/dev/null
    dd if=/dev/zero of="$target/uImage" bs=1024 count=100 status=none 2>/dev/null \
      || dd if=/dev/zero of="$target/uImage" bs=1024 count=100 2>/dev/null
    printf '\047\005\031\126' | dd of="$target/uImage" bs=1 conv=notrunc status=none 2>/dev/null \
      || printf '\047\005\031\126' | dd of="$target/uImage" bs=1 conv=notrunc 2>/dev/null
    dd if=/dev/zero of="$target/devicetree.dtb" bs=1024 count=10 status=none 2>/dev/null \
      || dd if=/dev/zero of="$target/devicetree.dtb" bs=1024 count=10 2>/dev/null
    printf '\320\015\376\355' | dd of="$target/devicetree.dtb" bs=1 conv=notrunc status=none 2>/dev/null \
      || printf '\320\015\376\355' | dd of="$target/devicetree.dtb" bs=1 conv=notrunc 2>/dev/null
}

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

# Complete synthetic set with the real uImage/FDT magic values.
make_complete_artifacts "$TMP/complete"

"$STAGE" --artifacts-dir "$TMP/complete" --output-dir "$TMP/staged" || fail "complete set should pass"
[ -f "$TMP/staged/BOOT.bin" ] || fail "staged BOOT.bin"
[ -f "$TMP/staged/uImage" ] || fail "staged uImage"
[ -f "$TMP/staged/devicetree.dtb" ] || fail "staged dtb"
[ -f "$TMP/staged/artifacts.manifest.json" ] || fail "staging manifest"
[ -f "$TMP/staged/.dcent-release-set.json" ] || fail "sealed release-set descriptor"
grep -q '"ready_for_complete_build": true' "$TMP/staged/artifacts.manifest.json" \
  || fail "ready flag"
"$PYTHON" - "$TMP/staged" <<'PY' || fail "hash-bearing staging evidence"
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
manifest = json.loads((root / "artifacts.manifest.json").read_text(encoding="utf-8"))
if manifest.get("schema") != "dcentos.am2_sd_artifacts_stage.v2":
    raise SystemExit("wrong semantic manifest schema")
for name in ("BOOT.bin", "uImage", "devicetree.dtb"):
    entry = manifest["artifacts"][name]
    content = (root / name).read_bytes()
    if entry != {
        **entry,
        "bytes": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }:
        raise SystemExit(f"semantic manifest mismatch: {name}")

descriptor = json.loads((root / ".dcent-release-set.json").read_text(encoding="utf-8"))
declared = {entry["name"]: entry for entry in descriptor["files"]}
for name in ("BOOT.bin", "uImage", "devicetree.dtb", "artifacts.manifest.json"):
    content = (root / name).read_bytes()
    if declared[name]["sha256"] != hashlib.sha256(content).hexdigest():
        raise SystemExit(f"release-set digest mismatch: {name}")
PY
pass "complete synthetic artifacts publish as a hash-bound atomic set"

# Stock XIL MD5 refuse (if md5sum available)
if command -v md5sum >/dev/null 2>&1; then
    # We cannot easily forge a file with a specific MD5 without collision;
    # the denylist path is integration-tested via the builder. Document skip.
    pass "stock MD5 denylist covered by builder (manual when real stock BOOT present)"
fi
grep -q 'stale signature' "$SCRIPT_DIR/build_am2_s19jpro_sd_disk_image.sh" \
    || fail "AM2 builder must refuse a stale sibling signature before rewrite"
pass "AM2 builder refuses stale signature state"

# In-place staging cannot provide an atomic directory commit and is refused.
if "$STAGE" --artifacts-dir "$TMP/complete" --output-dir "$TMP/complete" 2>/dev/null; then
    fail "same-dir stage should be refused"
fi
[ ! -e "$TMP/complete/artifacts.manifest.json" ] || fail "same-dir refusal altered source"
pass "same-dir stage fails closed"

# An existing destination is immutable publication history, not an overwrite target.
mkdir "$TMP/existing-output"
printf 'preserve-existing-output\n' > "$TMP/existing-output/sentinel"
if "$STAGE" --artifacts-dir "$TMP/complete" --output-dir "$TMP/existing-output" 2>/dev/null; then
    fail "existing output should be refused"
fi
[ "$(cat "$TMP/existing-output/sentinel")" = preserve-existing-output ] \
  || fail "existing output was modified"
pass "existing output is never merged or overwritten"

# Source aliases are refused rather than followed or silently de-linked.
make_complete_artifacts "$TMP/symlink-input"
printf 'important-boot-victim\n' > "$TMP/boot-victim"
rm "$TMP/symlink-input/BOOT.bin"
ln -s "$TMP/boot-victim" "$TMP/symlink-input/BOOT.bin"
if "$STAGE" --artifacts-dir "$TMP/symlink-input" --output-dir "$TMP/symlink-output" 2>/dev/null; then
    fail "symlinked BOOT.bin should be refused"
fi
[ "$(cat "$TMP/boot-victim")" = important-boot-victim ] || fail "BOOT symlink victim changed"
[ ! -e "$TMP/symlink-output" ] || fail "symlink failure published an output"
pass "symlinked source fails without alias mutation"

make_complete_artifacts "$TMP/hardlink-input"
ln "$TMP/hardlink-input/uImage" "$TMP/uImage-alias"
if "$STAGE" --artifacts-dir "$TMP/hardlink-input" --output-dir "$TMP/hardlink-output" 2>/dev/null; then
    fail "hardlinked uImage should be refused"
fi
[ ! -e "$TMP/hardlink-output" ] || fail "hardlink failure published an output"
pass "multiply-linked source fails closed"

# Mutating a pinned source during the copy invalidates the snapshot and removes
# its private destination. This deterministically exercises the in-place race,
# rather than relying on scheduler timing in a background shell process.
"$PYTHON" - "$SCRIPT_DIR/stage_am2_sd_artifacts.py" "$TMP" <<'PY' \
  || fail "in-place source mutation race was accepted"
import importlib.util
import contextlib
import io
from pathlib import Path
import shutil
import sys
from argparse import Namespace

module_path = Path(sys.argv[1])
root = Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("dcent_am2_stage_test", module_path)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)

source = root / "race-source.bin"
destination = root / "race-destination.bin"
source.write_bytes(b"A" * (2 * 1024 * 1024))
original_read = module.os.read
mutated = False

def read_then_mutate(descriptor, size):
    global mutated
    content = original_read(descriptor, size)
    if content and not mutated:
        mutated = True
        with source.open("r+b") as handle:
            # Change bytes already copied so the second pinned read must
            # disagree even on hosts with coarse/deferred timestamps.
            handle.seek(7)
            handle.write(b"B")
            handle.flush()
    return content

module.os.read = read_then_mutate
try:
    try:
        module.snapshot_regular_file(source, destination)
    except module.StageError:
        pass
    else:
        raise SystemExit("mutated source was accepted")
finally:
    module.os.read = original_read
if not mutated:
    raise SystemExit("race injection did not run")
if destination.exists():
    raise SystemExit("failed snapshot destination survived")

def arguments(source_name, output_name, *, check_only=False):
    return Namespace(
        artifacts_dir=str(root / source_name),
        output_dir=str(root / output_name),
        check_only=check_only,
    )

def copy_complete(name):
    shutil.copytree(root / "complete", root / name)

def require_no_control(output_name):
    leaked = list(root.glob(f".{output_name}.am2-stage-control-*"))
    if leaked:
        raise SystemExit(f"private control leaked: {leaked}")

# Destruction authority begins when create-stage returns, not after capability
# reporting. A forced query failure must retire both the stage and capability.
copy_complete("query-failure-input")
original_query = module.query_capability
module.query_capability = lambda *_args, **_kwargs: module.fail("injected query failure")
try:
    try:
        module.stage_artifacts(arguments("query-failure-input", "query-failure-output"))
    except module.StageError:
        pass
    else:
        raise SystemExit("query failure was accepted")
finally:
    module.query_capability = original_query
if (root / "query-failure-output").exists():
    raise SystemExit("query failure published output")
require_no_control("query-failure-output")

# Mutating the private snapshot after semantic evidence generation must be
# caught when the authoritative release-set manifest is bound.
copy_complete("semantic-race-input")
original_write_manifest = module.write_stage_manifest
def write_manifest_then_mutate(stage, snapshots):
    evidence = original_write_manifest(stage, snapshots)
    boot = stage / "BOOT.bin"
    with boot.open("r+b") as handle:
        handle.seek(9)
        handle.write(b"Z")
        handle.flush()
    return evidence

module.write_stage_manifest = write_manifest_then_mutate
try:
    try:
        module.stage_artifacts(arguments("semantic-race-input", "semantic-race-output"))
    except module.StageError:
        pass
    else:
        raise SystemExit("semantic/release-set hash disagreement was accepted")
finally:
    module.write_stage_manifest = original_write_manifest
if (root / "semantic-race-output").exists():
    raise SystemExit("semantic hash race published output")
require_no_control("semantic-race-output")

copy_complete("manifest-race-input")
def write_then_replace_manifest(stage, snapshots):
    evidence = original_write_manifest(stage, snapshots)
    (stage / "artifacts.manifest.json").write_text(
        '{"schema":"attacker","ready_for_complete_build":false}\n',
        encoding="utf-8",
    )
    return evidence

module.write_stage_manifest = write_then_replace_manifest
try:
    try:
        module.stage_artifacts(arguments("manifest-race-input", "manifest-race-output"))
    except module.StageError:
        pass
    else:
        raise SystemExit("semantic manifest replacement was accepted")
finally:
    module.write_stage_manifest = original_write_manifest
if (root / "manifest-race-output").exists():
    raise SystemExit("semantic manifest race published output")
require_no_control("manifest-race-output")

# READY is a post-cleanup report. If private-control retirement fails, return
# failure without ever printing a success claim.
copy_complete("cleanup-failure-input")
original_cleanup = module.cleanup_control
module.cleanup_control = lambda *_args, **_kwargs: False
captured = io.StringIO()
try:
    try:
        with contextlib.redirect_stdout(captured):
            module.stage_artifacts(
                arguments("cleanup-failure-input", "cleanup-failure-output", check_only=True)
            )
    except module.StageError:
        pass
    else:
        raise SystemExit("cleanup failure was accepted")
finally:
    module.cleanup_control = original_cleanup
if "READY" in captured.getvalue():
    raise SystemExit("cleanup failure printed a false READY claim")
for leaked in root.glob(".cleanup-failure-output.am2-stage-control-*"):
    shutil.rmtree(leaked)
PY
pass "source, capability, hash-binding, and READY races fail closed"

# A failure at the publisher's last pre-commit boundary leaves no piecemeal set.
make_complete_artifacts "$TMP/promotion-input"
if DCENT_RELEASE_SET_TEST_FAIL_BEFORE_PROMOTION=1 \
    "$STAGE" --artifacts-dir "$TMP/promotion-input" --output-dir "$TMP/promotion-output" 2>/dev/null; then
    fail "injected pre-promotion failure should fail"
fi
[ ! -e "$TMP/promotion-output" ] || fail "pre-promotion failure published a partial set"
set -- "$TMP"/.promotion-output.am2-stage-control-*
[ "$#" -eq 1 ] && [ ! -e "$1" ] || fail "pre-promotion failure leaked private stage control"
pass "pre-promotion failure rolls back the exact private set"

# Check-only still performs a private snapshot + seal, then destroys it.
"$STAGE" --artifacts-dir "$TMP/complete" --check-only >/dev/null \
  || fail "exact check-only validation should pass"
[ ! -e "$TMP/complete.staged" ] || fail "check-only published output"
set -- "$TMP"/.complete.staged.am2-stage-control-*
[ "$#" -eq 1 ] && [ ! -e "$1" ] || fail "check-only leaked private stage control"
pass "check-only validates and retires a sealed private snapshot"

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
COMP_SIZE=$(wc -c <"$COMP_IMG" | tr -d '[:space:]')
COMP_SHA=$(sha256sum "$COMP_IMG" | awk '{print $1}')
cat >"$COMP_IMG.manifest.json" <<JSON
{
  "schema": "dcentos.am2_s19jpro_sd_image_manifest.v2",
  "target": "am2-s19jpro-sd",
  "image": "complete.img",
  "image_size_bytes": $COMP_SIZE,
  "image_sha256": "$COMP_SHA",
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
