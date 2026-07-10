#!/bin/sh
#
# test_accept_fuzz.sh — adversarial/fuzz robustness tests for lib/accept_parse.sh.
#
# The accept-parse layer reads UNTRUSTED bytes off the wire (a miner's CGMiner
# JSON on port 4028). It is the gate that decides the whole release's PASS/FAIL,
# so it must be robust to malformed, hostile, and confusing input: it must never
# crash/hang, never extract a share count from a NON-Accepted field, and never
# emit junk. This complements the fixture-based unit tests with negative/edge
# inputs.
#
# Contacts NO miner. Runs standalone AND inside the offline CI gate.
# Exit 0 = all asserts pass; exit 1 = at least one failed.
#
# NOTE: no `set -e` — the verdict helpers return non-zero by design.
set -u

DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)

# shellcheck source=lib/accept_parse.sh
. "$DIR/lib/accept_parse.sh"

fails=0
ok() { printf 'ok   - %s\n' "$*"; }
no() { printf 'FAIL - %s\n' "$*" >&2; fails=$((fails + 1)); }

# expect_empty <label> <input> — parser must return NOTHING (no false counter).
expect_empty() {
    got=$(printf '%s' "$2" | accept_parse_accepted)
    if [ -z "$got" ]; then ok "$1"; else no "$1: expected empty, got '$got'"; fi
}

# expect_val <label> <expected> <input>
expect_val() {
    got=$(printf '%s' "$3" | accept_parse_accepted)
    if [ "$got" = "$2" ]; then ok "$1 (= '$got')"; else no "$1: expected '$2' got '$got'"; fi
}

# --- must NOT extract a counter from a non-"Accepted" key --------------------
expect_empty "decoy: Difficulty Accepted only (no plain Accepted)" \
    '{"SUMMARY":[{"Difficulty Accepted":1792.0,"Rejected":0}]}'
expect_empty "decoy: Rejected key" '{"SUMMARY":[{"Rejected":9}]}'
expect_empty 'decoy: "AcceptedFoo":5 (no closing quote before key end)' \
    '{"SUMMARY":[{"AcceptedFoo":5}]}'
expect_empty 'decoy: FooAccepted":5 (no opening quote before Accepted)' \
    '{"SUMMARY":[{"FooAccepted":5}]}'
expect_empty 'decoy: "Accepted " trailing space in key' \
    '{"SUMMARY":[{"Accepted ":5}]}'
expect_empty "key present but no value" '{"SUMMARY":[{"Accepted":}]}'
expect_empty "key with non-numeric value" '{"SUMMARY":[{"Accepted":"lots"}]}'

# --- hostile / malformed bytes: must be safe (empty, no crash/hang) ----------
expect_empty "empty string" ""
expect_empty "connection refused text" "Connection refused"
expect_empty "html error page" "<html><body>502 Bad Gateway</body></html>"
expect_empty "json-ish garbage" '{{{,,,:::"Accep"}}}'
expect_empty "unicode noise" 'æ—¥æœ¬èªž Accepted ð Ÿ'
expect_empty "sql-ish injection attempt" "'; DROP TABLE shares; --"

# --- must still parse the REAL key across formatting noise -------------------
expect_val "spaced value" "7" '{"SUMMARY":[{"Accepted": 7}]}'
expect_val "tab before value" "8" '{"SUMMARY":[{"Accepted":	8}]}'
expect_val "no space" "5" '{"SUMMARY":[{"Accepted":5}]}'
expect_val "real key present alongside the Difficulty Accepted decoy" "3" \
    '{"SUMMARY":[{"Accepted":3,"Difficulty Accepted":768.0}]}'

# --- multiple Accepted keys: take the first (SUMMARY precedes POOLS) ---------
expect_val "first-of-many (summary before pools)" "12" \
    '{"SUMMARY":[{"Accepted":12}],"POOLS":[{"Accepted":99}]}'

# --- a lying/huge miner counter is the miner's data, not a parser fault, but
#     the parser must still return exactly the digits (no overflow/panic) -----
expect_val "very large counter parsed verbatim" "999999999999999999" \
    '{"SUMMARY":[{"Accepted":999999999999999999}]}'

# --- performance/DoS guard: a huge junk blob must terminate quickly, not hang.
#     Build ~200 KB of 'A' with no valid Accepted key; parser must return empty.
big=$(awk 'BEGIN{s="";for(i=0;i<4096;i++)s=s"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";print s}' 2>/dev/null)
got=$(printf '%s' "$big" | accept_parse_accepted)
if [ -z "$got" ]; then ok "large junk blob returns empty (no false match, no hang)"; else no "large junk blob: got '$got'"; fi

# --- the verdict must treat an empty parse as FAIL (dead-miner safety) -------
n=$(printf '%s' "garbage" | accept_parse_accepted)
v=$(accept_verdict "$n" 1)
if [ "$v" = "FAIL" ]; then ok "empty parse -> verdict FAIL (dead miner never passes)"; else no "empty parse verdict: got '$v'"; fi

# --- enumeration parser robustness ------------------------------------------
got=$(printf '%s' "no numbers here at all" | accept_parse_enumerated)
if [ -z "$got" ]; then ok "enum parser: no-match returns empty"; else no "enum parser junk: '$got'"; fi
got=$(printf '%s' '{"chips_enumerated":0}' | accept_parse_enumerated)
if [ "$got" = "0" ]; then ok "enum parser: explicit zero preserved (0 chips = real datum)"; else no "enum zero: '$got'"; fi

if [ "$fails" -ne 0 ]; then
    printf '\naccept_parse FUZZ tests FAILED: %s assertion(s)\n' "$fails" >&2
    exit 1
fi
printf '\naccept_parse fuzz tests passed.\n'
