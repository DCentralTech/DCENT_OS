#!/bin/bash
# test_verify_release_keypair.sh — self-test the key-ceremony tooling with THROWAWAY
# keys (NEVER a production key). Proves generate_release_keypair.sh +
# verify_release_keypair.sh work end-to-end so the operator's air-gapped ceremony
# can't be defeated by a broken script: a matched pair PASSES and emits a 64-char
# firmware hex, a mismatched pair FAILS, and missing args error out. Skips cleanly
# when openssl/od are unavailable so the offline gate stays green on a minimal host.
set -u

DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)

if ! command -v openssl >/dev/null 2>&1 || ! command -v od >/dev/null 2>&1; then
    echo "SKIP: openssl/od not available — key-ceremony self-test skipped"
    exit 0
fi

fails=0
ok() { printf 'ok   - %s\n' "$*"; }
no() { printf 'FAIL - %s\n' "$*" >&2; fails=$((fails + 1)); }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

bash "$DIR/generate_release_keypair.sh" "$TMP/A" >/dev/null 2>&1 || no "generate throwaway keypair A"
bash "$DIR/generate_release_keypair.sh" "$TMP/B" >/dev/null 2>&1 || no "generate throwaway keypair B"

PA_PRIV="$TMP/A/dcent-release-ed25519.pem"
PA_PUB="$TMP/A/dcent-release-ed25519.pub.pem"
PB_PUB="$TMP/B/dcent-release-ed25519.pub.pem"

# Matched pair -> CEREMONY PASS (rc 0) + a 64-char (32-byte) firmware hex.
out=$(bash "$DIR/verify_release_keypair.sh" "$PA_PRIV" "$PA_PUB" 2>&1)
rc=$?
if [ "$rc" -eq 0 ]; then ok "matched pair -> CEREMONY PASS"; else no "matched pair should PASS (rc=$rc): $out"; fi
hex=$(printf '%s' "$out" | grep -oE '[0-9a-f]{64}' | head -n1)
if [ "${#hex}" -eq 64 ]; then ok "emits a 64-char (32-byte) firmware hex"; else no "expected a 64-char hex, got '${hex}'"; fi

# Mismatched pair (privA + pubB) -> CEREMONY FAIL (rc 1).
bash "$DIR/verify_release_keypair.sh" "$PA_PRIV" "$PB_PUB" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 1 ]; then ok "mismatched pair -> CEREMONY FAIL"; else no "mismatched pair MUST fail (rc=$rc)"; fi

# Missing args -> non-zero (usage error).
bash "$DIR/verify_release_keypair.sh" >/dev/null 2>&1
rc=$?
if [ "$rc" -ne 0 ]; then ok "missing args -> non-zero"; else no "missing args should be non-zero"; fi

# --expect-hex (firmware-consistency): the emitted hex must match itself; a wrong
# hex must FAIL; an uppercase form of the correct hex must still match.
GOOD_HEX=$(printf '%s' "$out" | grep -oE '[0-9a-f]{64}' | head -n1)
bash "$DIR/verify_release_keypair.sh" "$PA_PRIV" "$PA_PUB" "$GOOD_HEX" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 0 ]; then ok "matching --expect-hex -> PASS"; else no "matching expected hex should PASS (rc=$rc)"; fi

bash "$DIR/verify_release_keypair.sh" "$PA_PRIV" "$PA_PUB" \
    "0000000000000000000000000000000000000000000000000000000000000000" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 1 ]; then ok "mismatched --expect-hex -> FAIL"; else no "wrong expected hex MUST fail (rc=$rc)"; fi

UP_HEX=$(printf '%s' "$GOOD_HEX" | tr 'a-z' 'A-Z')
bash "$DIR/verify_release_keypair.sh" "$PA_PRIV" "$PA_PUB" "$UP_HEX" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 0 ]; then ok "uppercase --expect-hex matches (case-insensitive)"; else no "uppercase expected hex should match (rc=$rc)"; fi

if [ "$fails" -ne 0 ]; then
    printf '\nkey-ceremony self-test FAILED: %s assertion(s)\n' "$fails" >&2
    exit 1
fi
printf '\nkey-ceremony self-test passed.\n'
