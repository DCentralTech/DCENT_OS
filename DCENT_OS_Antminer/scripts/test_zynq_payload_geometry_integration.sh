#!/bin/sh
# Static integration proof for the canonical Zynq package-geometry boundary.
# Offline only: this script never opens a device or contacts a miner.

set -u

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
cd "$PROJECT_ROOT" || exit 1

failures=0
tests=0

pass()
{
    tests=$((tests + 1))
    printf 'PASS: %s\n' "$1"
}

fail()
{
    tests=$((tests + 1))
    failures=$((failures + 1))
    printf 'FAIL: %s\n' "$1" >&2
}

require_pattern()
{
    _dcent_geometry_file=$1
    _dcent_geometry_pattern=$2
    _dcent_geometry_label=$3
    if grep -F -- "$_dcent_geometry_pattern" "$_dcent_geometry_file" >/dev/null 2>&1; then
        pass "$_dcent_geometry_label"
    else
        fail "$_dcent_geometry_label (missing: $_dcent_geometry_pattern)"
    fi
}

reject_pattern()
{
    _dcent_geometry_file=$1
    _dcent_geometry_pattern=$2
    _dcent_geometry_label=$3
    if grep -F -- "$_dcent_geometry_pattern" \
            "$_dcent_geometry_file" >/dev/null 2>&1; then
        fail "$_dcent_geometry_label (forbidden: $_dcent_geometry_pattern)"
    else
        pass "$_dcent_geometry_label"
    fi
}

require_order()
{
    _dcent_geometry_file=$1
    _dcent_geometry_first=$2
    _dcent_geometry_second=$3
    _dcent_geometry_label=$4
    if awk -v first="$_dcent_geometry_first" -v second="$_dcent_geometry_second" '
        first_line == 0 && index($0, first) { first_line = NR }
        first_line > 0 && index($0, second) { second_line = NR; exit }
        END { exit (first_line > 0 && second_line > first_line) ? 0 : 1 }
    ' "$_dcent_geometry_file"; then
        pass "$_dcent_geometry_label"
    else
        fail "$_dcent_geometry_label"
    fi
}

HOST_HELPER=scripts/lib/sysupgrade_zynq_geometry.sh
TARGET_HELPER=br2_external_dcentos/board/zynq/rootfs-overlay/usr/libexec/dcentos/sysupgrade-zynq-geometry.sh
PACKAGE=scripts/package_sysupgrade.sh
PRE_FLASH=scripts/pre_flash_validate.sh
AM1_MANIFEST=scripts/am1_nand_backup_manifest.sh
AM1_MANIFEST_PY=scripts/am1_nand_backup_manifest.py
AM1_EXECUTE=scripts/am1_nand_backup_execute.sh
AM1_EXECUTE_PY=scripts/am1_nand_backup_execute.py
AM1_VALIDATOR=scripts/validate_am1_nand_backup.py
AM1_PLAN_VALIDATOR=scripts/validate_am1_nand_backup_plan.py
AM1_EVIDENCE_TEST=scripts/test_validate_am1_nand_backup.py
ATOMIC_PUBLISHER=scripts/atomic_publish_file.py

[ -f "$HOST_HELPER" ] && [ ! -L "$HOST_HELPER" ] || {
    printf 'FAIL: canonical host geometry helper is missing\n' >&2
    exit 1
}
if [ -e "$TARGET_HELPER" ] || [ -L "$TARGET_HELPER" ]; then
    fail "rootfs overlay does not carry a second geometry implementation"
else
    pass "rootfs overlay does not carry a second geometry implementation"
fi

require_pattern "$HOST_HELPER" 'dcent_zynq_geometry_tar_preextract_ceiling()' \
    "helper owns the tar pre-extraction ceiling API"
require_pattern "$HOST_HELPER" 'AM2_ZYNQ_KERNEL_PACKAGE_LEBS=23' \
    "helper pins the captured AM2 package kernel window"
require_pattern "$HOST_HELPER" 'AM2_ZYNQ_ROOTFS_PACKAGE_LEBS=179' \
    "helper pins the captured AM2 package rootfs window"

for _dcent_geometry_post_build in \
    br2_external_dcentos/board/zynq/post-build.sh \
    br2_external_dcentos/board/zynq/am2-s17pro/post-build.sh \
    br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh \
    br2_external_dcentos/board/zynq/am2-s19pro/post-build.sh
do
    require_pattern "$_dcent_geometry_post_build" \
        'scripts/lib/sysupgrade_zynq_geometry.sh' \
        "$(basename "$(dirname "$_dcent_geometry_post_build")") installs the canonical host geometry source"
    require_pattern "$_dcent_geometry_post_build" \
        'usr/libexec/dcentos/sysupgrade-zynq-geometry.sh' \
        "$(basename "$(dirname "$_dcent_geometry_post_build")") installs the target geometry ABI"
done

for _dcent_geometry_post_image in \
    br2_external_dcentos/board/zynq/am2-s17pro/post-image.sh \
    br2_external_dcentos/board/zynq/am2-s19jpro/post-image.sh \
    br2_external_dcentos/board/zynq/am2-s19pro/post-image.sh
do
    require_pattern "$_dcent_geometry_post_image" \
        'scripts/lib/sysupgrade_zynq_geometry.sh' \
        "$(basename "$(dirname "$_dcent_geometry_post_image")") producer sources canonical geometry"
    require_pattern "$_dcent_geometry_post_image" \
        'dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" rootfs "$ROOTFS_SIZE"' \
        "$(basename "$(dirname "$_dcent_geometry_post_image")") producer gates rootfs bytes"
    require_pattern "$_dcent_geometry_post_image" \
        'dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" kernel "$KERNEL_SIZE"' \
        "$(basename "$(dirname "$_dcent_geometry_post_image")") producer gates kernel bytes"
done

require_pattern "$PACKAGE" '. "$SCRIPT_DIR/lib/sysupgrade_zynq_geometry.sh"' \
    "legacy S9/AM2 producer sources canonical geometry"
require_order "$PACKAGE" 'ROOTFS_SIZE=' \
    'dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" rootfs "$ROOTFS_SIZE"' \
    "legacy producer measures rootfs before geometry admission"
require_order "$PACKAGE" 'KERNEL_SIZE=' \
    'dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" kernel "$KERNEL_SIZE"' \
    "legacy producer measures kernel before geometry admission"
require_order "$PACKAGE" \
    'dcent_zynq_geometry_require_payload_fit "$BOARD_NAME" kernel "$KERNEL_SIZE"' \
    'header "Building Sysupgrade Package"' \
    "legacy producer admits payloads before package staging"

require_pattern "$PRE_FLASH" 'SCRIPT_DIR/lib/sysupgrade_zynq_geometry.sh' \
    "package-only validator sources canonical host geometry"
require_pattern "$PRE_FLASH" \
    'am1-s9|am2-s19j|am2-s19jpro|am2-s19pro|am2-s17p)' \
    "package-only validator covers every admitted Zynq board identity"

require_pattern "$AM1_MANIFEST" 'am1_nand_backup_manifest.py' \
    "AM1 backup manifest wrapper delegates to the strict local tool"
require_pattern "$AM1_MANIFEST_PY" 'validate_backup(' \
    "AM1 backup admission delegates to the strict JSON validator"
require_pattern "$AM1_MANIFEST_PY" 'args.expected_target' \
    "AM1 backup admission binds optional target identity"
require_pattern "$AM1_MANIFEST_PY" \
    'manifest_validation=fail' \
    "AM1 backup admission exposes a fail-closed result"
require_pattern "$PRE_FLASH" \
    'validation_output=$(bash "$SCRIPT_DIR/am1_nand_backup_manifest.sh" --validate' \
    "AM1 pre-flash admission invokes the Bash manifest validator"
require_pattern "$PRE_FLASH" '--expected-target "$miner"' \
    "AM1 pre-flash admission binds backup evidence to the target miner"
require_pattern "$PRE_FLASH" '--expected-mac "$observed_mac"' \
    "AM1 pre-flash admission binds backup evidence to the physical MAC"
require_pattern "$PRE_FLASH" '--expected-hwid "$observed_hwid"' \
    "AM1 pre-flash admission binds backup evidence to the factory HWID"
require_pattern "$PRE_FLASH" '[ "$live_geometry" = "$manifest_geometry" ]' \
    "AM1 pre-flash admission rechecks exact live MTD geometry"
require_pattern "$PRE_FLASH" 'StrictHostKeyChecking=yes' \
    "pre-flash admission requires strict SSH host authentication"
reject_pattern "$PRE_FLASH" 'StrictHostKeyChecking=no' \
    "pre-flash admission contains no unauthenticated SSH fallback"
require_pattern "$PRE_FLASH" 'DCENT_EXPECTED_HOST_KEY_SHA256' \
    "pre-flash admission binds the expected host fingerprint"
require_pattern "$PRE_FLASH" '--expected-host-key-sha256 "$EXPECTED_HOST_KEY_SHA256"' \
    "AM1 result evidence is bound to the pre-flash host fingerprint"
require_pattern "$PRE_FLASH" '--max-age-seconds 86400' \
    "AM1 pre-flash admission uses signed execution time for freshness"
require_pattern "$PRE_FLASH" \
    '"$backup_root"/"${safe_ip}-"*/am1_nand_backup.manifest.json' \
    "AM1 backup discovery preserves workspace spaces during globbing"
require_pattern "$PRE_FLASH" \
    'suggested_backup_dir="$backup_root/${safe_ip}-$(date -u +%Y%m%dT%H%M%SZ)"' \
    "AM1 recovery instruction allocates the discoverable per-endpoint namespace"
require_pattern "$PRE_FLASH" '--local-backup-dir \"$suggested_backup_dir\"' \
    "AM1 recovery instruction writes where pre-flash discovery scans"
reject_pattern "$PRE_FLASH" 'ls -t $candidate_glob' \
    "AM1 backup discovery has no word-splitting ls pipeline"
require_pattern "$AM1_EXECUTE" 'am1_nand_backup_execute.py' \
    "AM1 backup wrapper delegates to the strict host executor"
require_pattern "$AM1_EXECUTE_PY" 'validate_plan(plan)' \
    "AM1 backup producer strictly validates plans before SSH"
require_order "$AM1_EXECUTE_PY" 'validate_plan(plan)' 'executor.run()' \
    "AM1 backup producer validates plan data before target contact"
require_order "$AM1_EXECUTE_PY" 'def preflight(' \
    'self.ssh_stream(f"nanddump --bb=padbad --omitoob' \
    "AM1 backup producer proves exact live geometry before NAND reads"
require_pattern "$AM1_EXECUTE_PY" 'nanddump --bb=padbad --omitoob' \
    "AM1 backup producer preserves bad-block offsets in fixed-size images"
require_pattern "$AM1_EXECUTE_PY" 'timeout=self.args.timeout' \
    "AM1 backup producer bounds every NAND dump through host timeout"
require_pattern "$AM1_EXECUTE_PY" 'first.stat().st_size != expected_size' \
    "AM1 backup producer rejects incomplete partition bytes"
require_order "$AM1_EXECUTE_PY" 'json.dump(manifest, handle, indent=2)' \
    'validate_backup(' \
    "AM1 backup producer validates complete evidence before publication"
require_order "$AM1_EXECUTE_PY" 'validate_backup(' \
    'before_commit=termination.refuse_pending_before_commit' \
    "AM1 backup producer publishes only a complete staged manifest"
require_order "$AM1_EXECUTE_PY" \
    'before_commit=termination.refuse_pending_before_commit' \
    'termination.mark_committed()' \
    "AM1 backup producer records commit truth only after the fenced publish"

if python3 "$AM1_VALIDATOR" --self-test; then
    pass "AM1 backup validator rejects incomplete and ambiguous evidence"
else
    fail "AM1 backup validator rejects incomplete and ambiguous evidence"
fi
if python3 "$AM1_PLAN_VALIDATOR" --self-test; then
    pass "AM1 backup plan validator rejects unsafe pre-SSH authority"
else
    fail "AM1 backup plan validator rejects unsafe pre-SSH authority"
fi
if python3 "$ATOMIC_PUBLISHER" --self-test; then
    pass "atomic manifest publisher rejects directory and cross-directory targets"
else
    fail "atomic manifest publisher rejects directory and cross-directory targets"
fi

if python3 "$AM1_EVIDENCE_TEST"; then
    pass "AM1 strict transport and evidence adversarial suite passes"
else
    fail "AM1 strict transport and evidence adversarial suite passes"
fi

AM1_FIXTURE_ROOT="$(mktemp -d /tmp/dcent-am1-wrapper.XXXXXX)" || {
    printf 'FAIL: could not allocate AM1 wrapper fixture directory\n' >&2
    exit 1
}
cleanup_am1_fixture()
{
    case "${AM1_FIXTURE_ROOT:-}" in
        /tmp/dcent-am1-wrapper.*) rm -rf -- "$AM1_FIXTURE_ROOT" ;;
    esac
}
trap cleanup_am1_fixture EXIT HUP INT TERM
AM1_FIXTURE_DIR="$AM1_FIXTURE_ROOT/backup"
mkdir "$AM1_FIXTURE_DIR"
if python3 - "$AM1_FIXTURE_DIR" <<'PY'
import sys
from pathlib import Path

sys.path.insert(0, "scripts")
from validate_am1_nand_backup import fixture_manifest

fixture_manifest(Path(sys.argv[1]))
PY
then
    pass "AM1 backup wrapper fixture is reproducible"
else
    fail "AM1 backup wrapper fixture is reproducible"
fi

AM1_FIXTURE_MANIFEST="$AM1_FIXTURE_DIR/manifest.json"
if bash "$AM1_MANIFEST" --validate \
        --manifest "$AM1_FIXTURE_MANIFEST" \
        --expected-target 192.0.2.9 \
        --expected-mac 02:00:00:00:00:09 \
        --expected-hwid AM1-FIXTURE-9 >/dev/null; then
    pass "AM1 backup wrapper admits complete target-bound evidence"
else
    fail "AM1 backup wrapper admits complete target-bound evidence"
fi
if bash "$AM1_MANIFEST" --validate \
        --manifest "$AM1_FIXTURE_MANIFEST" \
        --expected-target 192.0.2.10 \
        --expected-mac 02:00:00:00:00:09 \
        --expected-hwid AM1-FIXTURE-9 >/dev/null 2>&1; then
    fail "AM1 backup wrapper rejects a mismatched target identity"
else
    pass "AM1 backup wrapper rejects a mismatched target identity"
fi
python3 - "$AM1_FIXTURE_DIR/mtd0_boot.nanddump" <<'PY'
import sys
from pathlib import Path

with Path(sys.argv[1]).open("ab") as artifact:
    artifact.write(b"corruption")
PY
if bash "$AM1_MANIFEST" --validate \
        --manifest "$AM1_FIXTURE_MANIFEST" \
        --expected-target 192.0.2.9 \
        --expected-mac 02:00:00:00:00:09 \
        --expected-hwid AM1-FIXTURE-9 >/dev/null 2>&1; then
    fail "AM1 backup wrapper propagates artifact-validation failure"
else
    pass "AM1 backup wrapper propagates artifact-validation failure"
fi

for _dcent_geometry_sysupgrade in \
    br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade \
    br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade \
    br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade \
    br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade
do
    _dcent_geometry_variant=$(basename "$(dirname "$(dirname "$(dirname "$_dcent_geometry_sysupgrade")")")")
    require_pattern "$_dcent_geometry_sysupgrade" \
        'ZYNQ_GEOMETRY_HELPER="/usr/libexec/dcentos/sysupgrade-zynq-geometry.sh"' \
        "$_dcent_geometry_variant binds the installed geometry ABI"
    require_pattern "$_dcent_geometry_sysupgrade" \
        'dcent_zynq_geometry_tar_preextract_ceiling "$EXPECTED_BOARD"' \
        "$_dcent_geometry_variant delegates archive ceiling policy"
    require_pattern "$_dcent_geometry_sysupgrade" \
        'dcent_zynq_geometry_require_payload_fit "$EXPECTED_BOARD" rootfs "$ROOTFS_SIZE"' \
        "$_dcent_geometry_variant gates extracted rootfs bytes"
    require_pattern "$_dcent_geometry_sysupgrade" \
        'dcent_zynq_geometry_require_payload_fit "$EXPECTED_BOARD" kernel "$PACKAGE_KERNEL_SIZE"' \
        "$_dcent_geometry_variant gates extracted package-kernel bytes"
    require_order "$_dcent_geometry_sysupgrade" \
        'dcent_zynq_geometry_require_payload_fit "$EXPECTED_BOARD" rootfs "$ROOTFS_SIZE"' \
        'if [ "$TEST_ONLY" = "1" ]; then' \
        "$_dcent_geometry_variant admits canonical rootfs geometry before test-only success"
    require_order "$_dcent_geometry_sysupgrade" \
        'dcent_zynq_geometry_require_payload_fit "$EXPECTED_BOARD" kernel "$PACKAGE_KERNEL_SIZE"' \
        'if [ "$TEST_ONLY" = "1" ]; then' \
        "$_dcent_geometry_variant admits canonical kernel geometry before test-only success"
    require_pattern "$_dcent_geometry_sysupgrade" 'payload_fits_ubi_volume' \
        "$_dcent_geometry_variant retains runtime volume-fit defense"
done

printf 'Zynq payload geometry integration: %s assertions, %s failures\n' \
    "$tests" "$failures"
[ "$failures" -eq 0 ]
