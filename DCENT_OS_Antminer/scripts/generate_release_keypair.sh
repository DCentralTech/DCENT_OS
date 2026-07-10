#!/bin/bash
# generate_release_keypair.sh — Generate a DCENT_OS Ed25519 release keypair
#
# Writes an Ed25519 private key and matching public key to a local directory.
# Keep the private key out of the repo and CI logs.

set -euo pipefail

OUT_DIR="${1:-./release-keys}"
mkdir -p "$OUT_DIR"

PRIV_KEY="$OUT_DIR/dcent-release-ed25519.pem"
PUB_KEY="$OUT_DIR/dcent-release-ed25519.pub.pem"

command -v openssl >/dev/null 2>&1 || { echo "openssl is required" >&2; exit 1; }

if [ -e "$PRIV_KEY" ] || [ -e "$PUB_KEY" ]; then
    echo "Refusing to overwrite existing key material in $OUT_DIR" >&2
    exit 1
fi

openssl genpkey -algorithm ED25519 -out "$PRIV_KEY"
openssl pkey -in "$PRIV_KEY" -pubout -out "$PUB_KEY"
chmod 600 "$PRIV_KEY"
chmod 644 "$PUB_KEY"

echo "Generated:"
echo "  Private: $PRIV_KEY"
echo "  Public:  $PUB_KEY"
echo ""
echo "Suggested environment:"
echo "  export DCENT_RELEASE_SIGNING_KEY=$PRIV_KEY"
echo "  export DCENT_RELEASE_PUBKEY_FILE=$PUB_KEY"
echo "  export DCENT_REQUIRE_RELEASE_KEY=1"
