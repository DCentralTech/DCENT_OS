#!/usr/bin/env bash
#
# sign_sd_image.sh — Sign a DCENT_OS SD-card .img with the Ed25519 release
# key, emitting a sibling `<name>.img.sig` next to the image.
#
# Mirrors the Ed25519 signing approach used by package_sysupgrade.sh
# (openssl pkeyutl -sign -rawin -inkey <ed25519.pem>). The same release
# keypair is reused — we do NOT introduce a separate SD signing key.
#
# Algorithm: Ed25519 (DCENT_OS canonical).
# Rationale: DCENT_OS sysupgrade tar packages, OTA bundles, and the toolbox
# `_verify_signature()` path all use Ed25519 (see
# projects/dcent-toolbox/src/dcent_toolbox/core/install_package.py and
# DCENT_OS_Antminer/scripts/verify_sysupgrade_signature.sh). The Preparedness
# Sweep plan mentions RSA-4096 by analogy to VNish, but DCENT_OS already
# canonicalized on Ed25519 — see memory rule
# for the VNish-specific RSA contract (RSA is VNish's choice, not ours).
# Picking Ed25519 here keeps the SD .sig consumable by the same toolbox path
# that handles every other DCENT_OS signed artifact.
#
# Inputs:
#   <img-path>                Path to the SD .img file to sign.
#   --key <key-path>          Ed25519 private key in PEM form. Default:
#                             $DCENT_RELEASE_SIGNING_KEY env var.
#   --pubkey <pub-path>       Optional Ed25519 public key in PEM form for
#                             local verify-after-sign. Default:
#                             $DCENT_RELEASE_PUBKEY_FILE env var. When unset
#                             the public key is derived from the private key
#                             for a self-consistency check.
#   --output-sig <path>       Override the output .sig path. Default:
#                             `<img-path>.sig`.
#
# Behavior when DCENT_RELEASE_SIGNING_KEY is unset:
#   - Print a single WARN line on stderr.
#   - Exit 0 (do NOT fail the build).
#   - Do NOT touch the image. Do NOT produce a stale .sig.
#
# The build pipeline calls this after each SD .img is produced so signed
# fleets stay one cp away. Lab builds without the key just see a WARN.

set -euo pipefail

usage() {
    cat <<'EOF'
Usage: sign_sd_image.sh <img-path> [--key <key.pem>] [--pubkey <pub.pem>]
                                   [--output-sig <sig-path>]

Sign a DCENT_OS SD-card .img with Ed25519 and emit <img>.sig next to it.

Env vars:
  DCENT_RELEASE_SIGNING_KEY  Ed25519 private key in PEM form. If unset, the
                             script emits a WARN and exits 0 (lab builds).
  DCENT_RELEASE_PUBKEY_FILE  Ed25519 public key in PEM form. Optional; used
                             for local verify-after-sign self-check.

Algorithm: Ed25519 (matches DCENT_OS sysupgrade tarball + toolbox
_verify_signature contract).
EOF
}

IMG_PATH=""
SIGNING_KEY="${DCENT_RELEASE_SIGNING_KEY:-}"
VERIFY_PUBKEY="${DCENT_RELEASE_PUBKEY_FILE:-}"
OUTPUT_SIG=""

while [ $# -gt 0 ]; do
    case "$1" in
        --key)
            SIGNING_KEY="${2:?--key requires a path}"
            shift 2
            ;;
        --key=*)
            SIGNING_KEY="${1#--key=}"
            shift
            ;;
        --pubkey)
            VERIFY_PUBKEY="${2:?--pubkey requires a path}"
            shift 2
            ;;
        --pubkey=*)
            VERIFY_PUBKEY="${1#--pubkey=}"
            shift
            ;;
        --output-sig)
            OUTPUT_SIG="${2:?--output-sig requires a path}"
            shift 2
            ;;
        --output-sig=*)
            OUTPUT_SIG="${1#--output-sig=}"
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

if [ -z "$OUTPUT_SIG" ]; then
    OUTPUT_SIG="$IMG_PATH.sig"
fi

if [ -z "$SIGNING_KEY" ]; then
    echo "[WARN] DCENT_RELEASE_SIGNING_KEY is unset — skipping SD .img signing for $IMG_PATH (lab build)" >&2
    exit 0
fi

if [ ! -f "$SIGNING_KEY" ]; then
    echo "ERROR: signing key not found: $SIGNING_KEY" >&2
    exit 1
fi

command -v openssl >/dev/null 2>&1 || {
    echo "ERROR: openssl is required to sign SD .img" >&2
    exit 1
}

# Sign with Ed25519. -rawin tells openssl the input is the raw message body
# (not a pre-hashed digest), which is the Ed25519 contract.
openssl pkeyutl -sign -rawin \
    -inkey "$SIGNING_KEY" \
    -in "$IMG_PATH" \
    -out "$OUTPUT_SIG" \
    || {
        echo "ERROR: openssl failed to sign $IMG_PATH" >&2
        exit 1
    }

# Self-consistency verify: derive pubkey from private key if one wasn't
# provided. If verification fails, refuse to leave the .sig in place — that
# would ship a broken signature.
TMP_PUB=""
if [ -n "$VERIFY_PUBKEY" ]; then
    if [ ! -f "$VERIFY_PUBKEY" ]; then
        echo "ERROR: --pubkey not found: $VERIFY_PUBKEY" >&2
        rm -f "$OUTPUT_SIG"
        exit 1
    fi
    VERIFY_KEY="$VERIFY_PUBKEY"
else
    TMP_PUB="$(mktemp)"
    trap 'rm -f "$TMP_PUB"' EXIT INT TERM
    openssl pkey -in "$SIGNING_KEY" -pubout -out "$TMP_PUB" >/dev/null 2>&1 || {
        echo "ERROR: failed to derive public key from $SIGNING_KEY" >&2
        rm -f "$OUTPUT_SIG"
        exit 1
    }
    VERIFY_KEY="$TMP_PUB"
fi

openssl pkeyutl -verify -rawin -pubin \
    -inkey "$VERIFY_KEY" \
    -sigfile "$OUTPUT_SIG" \
    -in "$IMG_PATH" \
    >/dev/null \
    || {
        echo "ERROR: verify-after-sign failed for $IMG_PATH (key/pubkey mismatch?)" >&2
        rm -f "$OUTPUT_SIG"
        exit 1
    }

SIG_BYTES=$(stat -c%s "$OUTPUT_SIG" 2>/dev/null || stat -f%z "$OUTPUT_SIG" 2>/dev/null)
IMG_SHA=$(sha256sum "$IMG_PATH" | awk '{print $1}')
SIG_SHA=$(sha256sum "$OUTPUT_SIG" | awk '{print $1}')

echo "Signed: $IMG_PATH"
echo "  algorithm: Ed25519"
echo "  signature: $OUTPUT_SIG ($SIG_BYTES bytes)"
echo "  img sha256: $IMG_SHA"
echo "  sig sha256: $SIG_SHA"
