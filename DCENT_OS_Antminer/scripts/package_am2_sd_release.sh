#!/usr/bin/env bash
#
# package_am2_sd_release.sh — Bundle a complete AM2 SD .img for operator handoff.
#
# Requires CE-410 completeness (boot_artifacts_complete) unless
# --allow-incomplete-lab is set (lab-only; renames path semantics).
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/sd_image_signing_gate.sh
. "$SCRIPT_DIR/lib/sd_image_signing_gate.sh"

IMAGE=""
LABEL=""
OUT_ROOT=""
REQUIRE_COMPLETE=1
ALLOW_INCOMPLETE_LAB=0

usage() {
    cat <<EOF
Usage: $(basename "$0") --image <path.img> --label <slug> [--output-root <dir>]
                        [--require-complete|--allow-incomplete-lab]

Packages:
  <label>.img (+ .sig/.manifest.json if present)
  SHA256SUMS
  TESTER_README.txt
  BUILD_INFO.txt

Complete images only by default (CE-410 gate).
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --image) IMAGE="${2:?}"; shift 2 ;;
        --image=*) IMAGE="${1#*=}"; shift ;;
        --label) LABEL="${2:?}"; shift 2 ;;
        --label=*) LABEL="${1#*=}"; shift ;;
        --output-root) OUT_ROOT="${2:?}"; shift 2 ;;
        --output-root=*) OUT_ROOT="${1#*=}"; shift ;;
        --require-complete) REQUIRE_COMPLETE=1; ALLOW_INCOMPLETE_LAB=0; shift ;;
        --allow-incomplete-lab) REQUIRE_COMPLETE=0; ALLOW_INCOMPLETE_LAB=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "ERROR: unknown arg: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$IMAGE" ] || [ -z "$LABEL" ]; then
    echo "ERROR: --image and --label are required" >&2
    usage >&2
    exit 2
fi
if [ ! -f "$IMAGE" ]; then
    echo "ERROR: image not found: $IMAGE" >&2
    exit 1
fi

MANIFEST="${IMAGE}.manifest.json"
if [ ! -f "$MANIFEST" ]; then
    # try sibling without double extension
    MANIFEST_ALT="${IMAGE%.img}.img.manifest.json"
    [ -f "$MANIFEST_ALT" ] && MANIFEST="$MANIFEST_ALT"
fi

if [ "$REQUIRE_COMPLETE" -eq 1 ]; then
    if ! dcent_sd_require_complete_manifest_for_signing "$IMAGE" "$MANIFEST"; then
        echo "ERROR: refusing to package incomplete AM2 SD image as release" >&2
        echo "       Use --allow-incomplete-lab only for lab handoff of rootfs-only images" >&2
        exit 1
    fi
elif [ "$ALLOW_INCOMPLETE_LAB" -eq 1 ]; then
    echo "WARNING: packaging incomplete lab AM2 SD image (not a public release)" >&2
    # Always force the incomplete token — do not treat *LAB* alone as safe.
    case "$LABEL" in
        *UNSIGNED-LAB-ROOTFS-ONLY*) ;;
        *) LABEL="${LABEL}-UNSIGNED-LAB-ROOTFS-ONLY" ;;
    esac
    case "$LABEL" in
        *complete*|*COMPLETE*)
            echo "ERROR: incomplete lab package label must not claim 'complete'" >&2
            exit 1
            ;;
    esac
    # Never ship a release signature with incomplete media.
    if [ -f "${IMAGE}.sig" ]; then
        echo "ERROR: refusing incomplete package that has a sibling .sig (CE-410)" >&2
        exit 1
    fi
fi

PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
if [ -z "$OUT_ROOT" ]; then
    OUT_ROOT="$PROJECT_DIR/releases/am2-sd/$LABEL"
fi
mkdir -p "$OUT_ROOT"

IMG_BASENAME="dcentos-${LABEL}.img"
cp -f "$IMAGE" "$OUT_ROOT/$IMG_BASENAME"
[ -f "$MANIFEST" ] && cp -f "$MANIFEST" "$OUT_ROOT/${IMG_BASENAME}.manifest.json"
[ -f "${IMAGE}.sig" ] && cp -f "${IMAGE}.sig" "$OUT_ROOT/${IMG_BASENAME}.sig"

(
    cd "$OUT_ROOT"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$IMG_BASENAME" >SHA256SUMS
        # Use if/then (not `&&`) so set -e does not abort when optional
        # sidecars are absent.
        if [ -f "${IMG_BASENAME}.manifest.json" ]; then
            sha256sum "${IMG_BASENAME}.manifest.json" >>SHA256SUMS
        fi
        if [ -f "${IMG_BASENAME}.sig" ]; then
            sha256sum "${IMG_BASENAME}.sig" >>SHA256SUMS
        fi
    fi
)

cat >"$OUT_ROOT/TESTER_README.txt" <<EOF
DCENT_OS AM2 S19j Pro SD recovery / try media
Label: $LABEL

THIS IS NOT A NAND INSTALLER.
- Boot from this SD for recovery or temporary try when the image is complete and signed.
- Persistent DCENT_OS install on AM2 uses DCENT_Toolbox + signed sysupgrade tar with
  restore_verified gates — not this SD alone.
- Incomplete / lab images must not be treated as production release media.

Write with balenaEtcher or DCENT_OS_Antminer/scripts/write_sd_card.sh (dry-run first).
After boot, optional read-only proof:
  bash DCENT_OS_Antminer/scripts/am2_sd_recovery_probe.sh <ip> \\
    --known-hosts <pinned-known-hosts> \\
    --expected-host-key-sha256 SHA256:<fingerprint>
EOF

cat >"$OUT_ROOT/BUILD_INFO.txt" <<EOF
label=$LABEL
source_image=$IMAGE
packaged_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date)
require_complete=$REQUIRE_COMPLETE
allow_incomplete_lab=$ALLOW_INCOMPLETE_LAB
EOF

echo "package_am2_sd_release: wrote $OUT_ROOT"
ls -la "$OUT_ROOT"
