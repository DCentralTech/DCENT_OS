#!/usr/bin/env bash
#
# verify_sd_image.sh — Verify a DCENT_OS SD-card .img against its sibling
# `<name>.img.sig` Ed25519 signature.
#
# Sibling to sign_sd_image.sh. The toolbox `_verify_signature()` path
# consumes the same .sig contract.
#
# Inputs:
#   <img-path>                Path to the SD .img file to verify.
#   --pubkey <pub-path>       Ed25519 public key in PEM form (required).
#                             Default: $DCENT_RELEASE_PUBKEY_FILE env var.
#   --sig <sig-path>          Override the input .sig path. Default:
#                             `<img-path>.sig`.

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: verify_sd_image.sh <img-path> [--pubkey <pub.pem>] [--sig <sig-path>]

Verify a DCENT_OS SD-card .img with Ed25519 against its sibling .img.sig.

Env vars:
  DCENT_RELEASE_PUBKEY_FILE  Ed25519 public key in PEM form. Required unless
                             --pubkey is passed.
EOF
}

IMG_PATH=""
VERIFY_PUBKEY="${DCENT_RELEASE_PUBKEY_FILE:-}"
SIG_PATH=""

while [ $# -gt 0 ]; do
    case "$1" in
        --pubkey)
            VERIFY_PUBKEY="${2:?--pubkey requires a path}"
            shift 2
            ;;
        --pubkey=*)
            VERIFY_PUBKEY="${1#--pubkey=}"
            shift
            ;;
        --sig)
            SIG_PATH="${2:?--sig requires a path}"
            shift 2
            ;;
        --sig=*)
            SIG_PATH="${1#--sig=}"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        -*)
            echo "ERROR: unknown flag: $1" >&2
            usage >&2
            exit 1
            ;;
        *)
            if [ -z "$IMG_PATH" ]; then
                IMG_PATH="$1"
            else
                echo "ERROR: multiple positional arguments; only <img-path> is accepted" >&2
                usage >&2
                exit 1
            fi
            shift
            ;;
    esac
done

if [ -z "$IMG_PATH" ]; then
    echo "ERROR: missing <img-path>" >&2
    usage >&2
    exit 1
fi

if [ ! -f "$IMG_PATH" ]; then
    echo "ERROR: SD image not found: $IMG_PATH" >&2
    exit 1
fi

if [ -z "$SIG_PATH" ]; then
    SIG_PATH="$IMG_PATH.sig"
fi

if [ ! -f "$SIG_PATH" ]; then
    echo "ERROR: signature not found: $SIG_PATH" >&2
    exit 1
fi

if [ -z "$VERIFY_PUBKEY" ]; then
    echo "ERROR: no public key provided; pass --pubkey <pub.pem> or set DCENT_RELEASE_PUBKEY_FILE" >&2
    exit 1
fi

if [ ! -f "$VERIFY_PUBKEY" ]; then
    echo "ERROR: public key not found: $VERIFY_PUBKEY" >&2
    exit 1
fi

command -v openssl >/dev/null 2>&1 || {
    echo "ERROR: openssl is required to verify SD .img" >&2
    exit 1
}

openssl pkeyutl -verify -rawin -pubin \
    -inkey "$VERIFY_PUBKEY" \
    -sigfile "$SIG_PATH" \
    -in "$IMG_PATH" \
    >/dev/null \
    || {
        echo "ERROR: signature does NOT match image (image: $IMG_PATH, sig: $SIG_PATH)" >&2
        exit 1
    }

echo "OK: signature verified ($IMG_PATH)"
