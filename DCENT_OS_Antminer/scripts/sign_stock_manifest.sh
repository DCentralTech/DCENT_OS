#!/bin/sh
# W29 (2026-05-13): release-time helper — ed25519-sign the
# stock-Bitmain manifest so dcentrald can verify the at-rest pin
# against `STOCK_MANIFEST_SIG_BAKED` at runtime.
#
# This is NOT load-bearing for tests — the test suite uses an
# in-process keypair via `verify_manifest_signature_with_explicit_pubkey`.
# This script exists so the release process is reproducible from a
# private key in HSM/Vault.
#
# Usage:
#   ./sign_stock_manifest.sh <private_key.pem> [manifest_path] [sig_out_path]
#
# Defaults:
#   manifest_path =
#   sig_out_path  =
#
# Then build with the matching public key pinned:
#   DCENT_MANIFEST_PUBLIC_KEY_HEX=<hex64> \
#     cargo build --release --target armv7-unknown-linux-musleabihf
#
# The pubkey must be the raw 32-byte ed25519 key as 64 hex chars (NOT
# the PEM SPKI). To extract from the private key:
#   openssl pkey -in <key.pem> -pubout -outform DER \
#     | xxd -p -c 64 | tail -c 65 | head -c 64

set -eu

KEY_PATH=${1:-}
MANIFEST_PATH=${2:-knowledge-base/firmware-archive/stock-bitmain-manifest.json}
SIG_OUT_PATH=${3:-knowledge-base/firmware-archive/stock-bitmain-manifest.json.sig}

if [ -z "$KEY_PATH" ]; then
    echo "Usage: $0 <private_key.pem> [manifest_path] [sig_out_path]" >&2
    exit 64
fi

if [ ! -f "$KEY_PATH" ]; then
    echo "Private key not found: $KEY_PATH" >&2
    exit 65
fi

if [ ! -f "$MANIFEST_PATH" ]; then
    echo "Manifest not found: $MANIFEST_PATH" >&2
    exit 66
fi

# openssl pkeyutl -sign -rawin produces a raw ed25519 64-byte
# signature, which is exactly what `verify_manifest_signature` expects.
openssl pkeyutl -sign \
    -inkey "$KEY_PATH" \
    -rawin \
    -in "$MANIFEST_PATH" \
    -out "$SIG_OUT_PATH"

SIG_LEN=$(wc -c < "$SIG_OUT_PATH")
if [ "$SIG_LEN" -ne 64 ]; then
    echo "WARN: signature length is $SIG_LEN bytes (expected 64 for ed25519)" >&2
fi

echo "Signed $MANIFEST_PATH -> $SIG_OUT_PATH ($SIG_LEN bytes)"
echo "Reminder: rebuild dcentrald-api with DCENT_MANIFEST_PUBLIC_KEY_HEX pinned."
