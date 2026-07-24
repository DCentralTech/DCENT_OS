#!/bin/sh
# Generate one verified, no-replace candidate signature for the stock-Bitmain
# manifest. Promotion into the two tracked signature locations is a separate
# reviewed worktree change; this helper never truncates either placeholder.

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "$PROJECT_ROOT/../.." && pwd)
. "$SCRIPT_DIR/lib/release_envelope.sh"

KEY_PATH=${1:-}
MANIFEST_PATH=${2:-"$REPO_ROOT/knowledge-base/firmware-archive/stock-bitmain-manifest.json"}
SIG_OUT_PATH=${3:-"${MANIFEST_PATH}.sig.candidate"}
PUBKEY_PATH=${DCENT_RELEASE_PUBKEY_FILE:-}

if [ -z "$KEY_PATH" ]; then
    echo "Usage: DCENT_RELEASE_PUBKEY_FILE=<trusted.pem> $0 <private_key.pem> [manifest_path] [candidate_path]" >&2
    exit 64
fi
[ -f "$KEY_PATH" ] || {
    echo "Private key not found: $KEY_PATH" >&2
    exit 65
}
[ -f "$MANIFEST_PATH" ] || {
    echo "Manifest not found: $MANIFEST_PATH" >&2
    exit 66
}
[ -n "$PUBKEY_PATH" ] || {
    echo "DCENT_RELEASE_PUBKEY_FILE must name the trusted Ed25519 public key" >&2
    exit 67
}
[ -f "$PUBKEY_PATH" ] || {
    echo "Trusted public key not found: $PUBKEY_PATH" >&2
    exit 68
}
[ "$SIG_OUT_PATH" != "$MANIFEST_PATH" ] || {
    echo "Candidate signature path must differ from the manifest path" >&2
    exit 69
}
[ -f "$SCRIPT_DIR/sign_release_artifact.py" ] || {
    echo "Exact release artifact signer is missing: $SCRIPT_DIR/sign_release_artifact.py" >&2
    exit 70
}

dcent_release_run_python "$SCRIPT_DIR/sign_release_artifact.py" \
    "$MANIFEST_PATH" \
    --key "$KEY_PATH" \
    --pubkey "$PUBKEY_PATH" \
    --output-sig "$SIG_OUT_PATH" >/dev/null

SIG_LEN=$(wc -c < "$SIG_OUT_PATH" | tr -d '[:space:]')
[ "$SIG_LEN" = 64 ] || {
    echo "Candidate signature length is $SIG_LEN bytes; expected 64" >&2
    exit 71
}

echo "Verified stock-manifest signature candidate: $SIG_OUT_PATH"
echo "No existing signature pathname was overwritten."
echo "After review, copy these exact candidate bytes to both tracked files:"
echo "  $REPO_ROOT/knowledge-base/firmware-archive/stock-bitmain-manifest.json.sig"
echo "  $PROJECT_ROOT/dcentrald/dcentrald-api/assets/stock-bitmain-manifest.json.sig"
echo "Then rebuild with DCENT_MANIFEST_PUBLIC_KEY_HEX derived from the trusted public key."
