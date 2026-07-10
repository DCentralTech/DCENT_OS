#!/bin/sh
#
# Shared SD-image signing guard.
#
# Release signing of raw .img carriers must prove boot artifacts are complete.
# A rootfs-only lab image may exist for staging, but it must not receive a
# release Ed25519 signature or a success-style production banner.

dcent_sd_manifest_boot_artifacts_complete() {
    manifest=$1
    if [ ! -f "$manifest" ]; then
        return 2
    fi

    if command -v python3 >/dev/null 2>&1; then
        python3 - "$manifest" <<'PY'
import json
import sys

REQUIRED_ARTIFACTS = [
    "BOOT.bin",
    "uImage",
    "devicetree.dtb",
    "uEnv.txt",
    "bitstream",
    "rootfs",
]

try:
    with open(sys.argv[1], "r", encoding="utf-8") as fh:
        data = json.load(fh)
except Exception:
    sys.exit(3)

artifacts = data.get("artifacts")
complete = (
    data.get("boot_artifacts_complete") is True
    and isinstance(artifacts, dict)
    and all(artifacts.get(name) is True for name in REQUIRED_ARTIFACTS)
)
sys.exit(0 if complete else 1)
PY
        return $?
    fi

    # Minimal fallback for the manifest this repo emits. It intentionally only
    # accepts the literal JSON boolean true, not quoted strings or absent keys.
    grep -E '"boot_artifacts_complete"[[:space:]]*:[[:space:]]*true' "$manifest" >/dev/null 2>&1 || return 1
    for artifact in '"BOOT.bin"' '"uImage"' '"devicetree.dtb"' '"uEnv.txt"' '"bitstream"' '"rootfs"'; do
        grep -E "$artifact[[:space:]]*:[[:space:]]*true" "$manifest" >/dev/null 2>&1 || return 1
    done
    return 0
}

dcent_sd_require_complete_manifest_for_signing() {
    image=$1
    manifest=${2:-$1.manifest.json}

    if [ ! -f "$manifest" ]; then
        echo "ERROR: refusing to sign SD image: missing completeness manifest: $manifest" >&2
        echo "       Image: $image" >&2
        return 1
    fi

    if dcent_sd_manifest_boot_artifacts_complete "$manifest"; then
        return 0
    fi

    echo "ERROR: refusing to sign SD image: boot_artifacts_complete is not true in $manifest" >&2
    echo "       Image: $image" >&2
    echo "       AM2 S19j Pro SD release signing requires BOOT.bin, uImage," >&2
    echo "       devicetree.dtb, uEnv.txt, bitstream, and rootfs." >&2
    return 1
}

dcent_sd_mark_incomplete_lab_image() {
    image=$1
    manifest=${2:-$1.manifest.json}

    case "$image" in
        *-UNSIGNED-LAB-ROOTFS-ONLY.img)
            printf '%s\n' "$image"
            return 0
            ;;
        *.img)
            lab_image=${image%.img}-UNSIGNED-LAB-ROOTFS-ONLY.img
            ;;
        *)
            lab_image=$image-UNSIGNED-LAB-ROOTFS-ONLY
            ;;
    esac

    mv "$image" "$lab_image"
    if [ -f "$manifest" ]; then
        mv "$manifest" "$lab_image.manifest.json"
    fi
    printf '%s\n' "$lab_image"
}
