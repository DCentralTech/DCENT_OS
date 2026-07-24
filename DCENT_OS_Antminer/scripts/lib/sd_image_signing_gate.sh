#!/bin/sh
#
# Shared SD-image signing guard.
#
# Release signing of raw .img carriers must prove boot artifacts are complete.
# A rootfs-only lab image may exist for staging, but it must not receive a
# release Ed25519 signature or a success-style production banner.

dcent_sd_require_complete_manifest_for_signing() {
    image=$1
    manifest=${2:-$1.manifest.json}

    if [ ! -f "$manifest" ]; then
        echo "ERROR: refusing to sign SD image: missing completeness manifest: $manifest" >&2
        echo "       Image: $image" >&2
        return 1
    fi

    if [ -n "${BASH_SOURCE:-}" ]; then
        dcent_sd_gate_dir=$(CDPATH= cd -- "$(dirname -- "$BASH_SOURCE")" && pwd)
        dcent_sd_signer="$dcent_sd_gate_dir/../sign_sd_image.sh"
    elif [ -n "${SCRIPT_DIR:-}" ]; then
        dcent_sd_signer="$SCRIPT_DIR/sign_sd_image.sh"
    else
        echo "ERROR: caller must set SCRIPT_DIR before sourcing the SD signing gate" >&2
        return 1
    fi
    if [ ! -x "$dcent_sd_signer" ]; then
        echo "ERROR: SD image manifest validator is missing: $dcent_sd_signer" >&2
        return 1
    fi

    if "$dcent_sd_signer" "$image" --manifest "$manifest" --check-only >/dev/null; then
        return 0
    fi

    echo "ERROR: refusing SD release image: manifest is incomplete or not bound to the image: $manifest" >&2
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
