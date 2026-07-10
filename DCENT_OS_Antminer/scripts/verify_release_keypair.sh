#!/bin/bash
# verify_release_keypair.sh — Ceremony self-proof for a DCENT_OS Ed25519 release
# keypair.
#
# It does NOT generate keys (that is generate_release_keypair.sh). It PROVES an
# already-generated pair is internally consistent and usable, and emits the exact
# 32-byte public-key HEX to bake into the firmware (the release_ed25519.pub /
# DEFAULT_*_PUBKEY_HEX form the OTA verifier compares against — e.g. the beta's
# 26985575eae77d56c490ceeb9054af012eab5ae59119cd20eaa70dd7e722df83).
#
# Purpose: reduce the production key ceremony to two commands the operator runs in
# an air-gapped environment —
#     ./generate_release_keypair.sh ./release-keys
#     ./verify_release_keypair.sh ./release-keys/dcent-release-ed25519.pem \
#                                 ./release-keys/dcent-release-ed25519.pub.pem
# — the first mints the key, the second proves it round-trips (sign, verify, and
# REJECT a tampered payload) and prints the firmware hex to copy. A key that does
# not round-trip, or a hex mis-extracted from the PEM, would otherwise ship a
# firmware that cannot verify its own OTA updates (a bricked update path).
#
# POSIX-ish bash; needs only openssl + od (no xxd dependency).

set -euo pipefail

PRIV="${1:?usage: verify_release_keypair.sh <priv.pem> <pub.pem> [expected_firmware_hex]}"
PUB="${2:?usage: verify_release_keypair.sh <priv.pem> <pub.pem> [expected_firmware_hex]}"
# Optional: the 32-byte (64 hex) public key BAKED into the firmware
# (release_ed25519.pub / *_PUBKEY_HEX). If given, the ceremony additionally
# proves the keypair MATCHES the shipped firmware — a mismatch means the firmware
# cannot verify OTA updates signed with this key.
EXPECT_HEX="${3:-}"

command -v openssl >/dev/null 2>&1 || { echo "openssl is required" >&2; exit 2; }
command -v od >/dev/null 2>&1 || { echo "od is required" >&2; exit 2; }

fail() { echo "CEREMONY FAIL: $*" >&2; exit 1; }

# Raw 32-byte Ed25519 key from a PEM public key: the SubjectPublicKeyInfo DER is a
# fixed 12-byte prefix + the 32-byte key, so the last 32 bytes are the raw key.
raw_pub_hex() {
    openssl pkey -pubin -in "$1" -outform DER 2>/dev/null \
        | tail -c 32 | od -An -v -tx1 | tr -d ' \n'
}

[ -f "$PRIV" ] || fail "private key file not found: $PRIV"
[ -f "$PUB" ] || fail "public key file not found: $PUB"

# 1. Both parse as keys.
openssl pkey -in "$PRIV" -noout 2>/dev/null || fail "private key does not parse as a key"
openssl pkey -pubin -in "$PUB" -noout 2>/dev/null || fail "public key does not parse as a key"

# 2. The supplied public key MATCHES the private key (derive pub from priv, compare
#    the raw 32 bytes). Catches a mismatched pair before it ships.
DERIVED=$(openssl pkey -in "$PRIV" -pubout 2>/dev/null \
    | openssl pkey -pubin -outform DER 2>/dev/null | tail -c 32 | od -An -v -tx1 | tr -d ' \n')
GIVEN=$(raw_pub_hex "$PUB")
[ -n "$GIVEN" ] || fail "could not extract the raw public key from $PUB"
[ "$DERIVED" = "$GIVEN" ] || fail "public key does NOT match the private key"

# 3. Exactly 32 bytes (64 hex chars) — the firmware-baked form.
[ "${#GIVEN}" -eq 64 ] || fail "public key hex is ${#GIVEN} chars, expected 64 (32 bytes)"

# 4. End-to-end round-trip: sign a payload, verify it, and confirm a TAMPERED
#    payload is REJECTED (proves the signature is actually being checked, not just
#    that signing produced bytes).
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
printf 'dcentos-release-ceremony-selftest' > "$TMP/msg.bin"
openssl pkeyutl -sign -inkey "$PRIV" -rawin -in "$TMP/msg.bin" -out "$TMP/sig.bin" 2>/dev/null \
    || fail "signing the self-test payload failed"
openssl pkeyutl -verify -pubin -inkey "$PUB" -rawin -in "$TMP/msg.bin" -sigfile "$TMP/sig.bin" >/dev/null 2>&1 \
    || fail "a VALID signature did not verify against the public key"
printf 'dcentos-release-ceremony-selftesX' > "$TMP/tampered.bin"
if openssl pkeyutl -verify -pubin -inkey "$PUB" -rawin -in "$TMP/tampered.bin" -sigfile "$TMP/sig.bin" >/dev/null 2>&1; then
    fail "a TAMPERED payload VERIFIED — the signature is not being checked"
fi

# Optional: prove the keypair matches the hex ALREADY baked into a firmware build.
if [ -n "$EXPECT_HEX" ]; then
    _exp=$(printf '%s' "$EXPECT_HEX" | tr 'A-Z' 'a-z' | tr -d ' \n')
    if [ "$_exp" != "$GIVEN" ]; then
        fail "keypair public hex ($GIVEN) does NOT match the expected firmware-baked hex ($_exp) — this key cannot verify OTA updates on that firmware"
    fi
    echo "CEREMONY PASS: keypair matches, round-trips (sign + verify), rejects a tampered payload,"
    echo "               AND matches the firmware-baked public-key hex:"
    echo "  $GIVEN"
    exit 0
fi

echo "CEREMONY PASS: keypair matches, round-trips (sign + verify), and rejects a tampered payload."
echo "Firmware public-key hex (bake into release_ed25519.pub / *_PUBKEY_HEX):"
echo "  $GIVEN"
