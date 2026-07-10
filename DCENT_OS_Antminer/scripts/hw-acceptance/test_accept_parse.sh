#!/bin/sh
#
# test_accept_parse.sh — hardware-free unit tests for lib/accept_parse.sh.
#
# Feeds captured CGMiner/REST/log fixtures through the pure parsers and asserts
# the accepted-share counter, hashrate, elapsed, enumeration, and PASS/FAIL
# verdict logic. Contacts NO miner. Runs standalone AND inside the offline CI
# gate (ci_offline_gates.sh -> accept_parse_selftest) so the load-bearing
# accept-gate math can never silently regress.
#
# Exit 0 = all asserts pass; exit 1 = at least one failed.

# NOTE: no `set -e` — accept_verdict/accept_enum_verdict return non-zero BY DESIGN
# (that is the FAIL signal under test). `set -e` would abort at the first expected
# failure before we can capture its rc. The harness does its own assertion tally.
set -u

DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
FIX="$DIR/fixtures"

# shellcheck source=lib/accept_parse.sh
. "$DIR/lib/accept_parse.sh"

fails=0
ok() { printf 'ok   - %s\n' "$*"; }
no() { printf 'FAIL - %s\n' "$*" >&2; fails=$((fails + 1)); }

# assert_eq <label> <expected> <actual>
assert_eq() {
    if [ "$2" = "$3" ]; then
        ok "$1 (= '$3')"
    else
        no "$1: expected '$2' got '$3'"
    fi
}

# assert_rc <label> <expected_rc> <actual_rc>
assert_rc() {
    if [ "$2" -eq "$3" ]; then
        ok "$1 (rc=$3)"
    else
        no "$1: expected rc $2 got $3"
    fi
}

# --- accept_parse_accepted --------------------------------------------------
# The decoy: summary_accepted7 also carries "Difficulty Accepted":1792.0 — a
# naive '"Accepted"' substring match would return 1792. Must return 7.
assert_eq "accepted: 7 (not the Difficulty Accepted 1792 decoy)" \
    "7" "$(accept_parse_accepted < "$FIX/summary_accepted7.json")"
assert_eq "accepted: 0 (fresh miner, zero shares)" \
    "0" "$(accept_parse_accepted < "$FIX/summary_zero.json")"
assert_eq "accepted: 42 (whitespace/pretty-printed body)" \
    "42" "$(accept_parse_accepted < "$FIX/summary_spaced.json")"
assert_eq "accepted: empty (error response, no SUMMARY)" \
    "" "$(accept_parse_accepted < "$FIX/summary_no_accepted.json")"
assert_eq "accepted: empty (connection-refused garbage)" \
    "" "$(accept_parse_accepted < "$FIX/summary_malformed.txt")"

# --- accept_parse_mhs_av / elapsed -----------------------------------------
assert_eq "mhs av: 13500.42" \
    "13500.42" "$(accept_parse_mhs_av < "$FIX/summary_accepted7.json")"
assert_eq "elapsed: 615" \
    "615" "$(accept_parse_elapsed < "$FIX/summary_accepted7.json")"
assert_eq "mhs av: 95000.7 (spaced body)" \
    "95000.7" "$(accept_parse_mhs_av < "$FIX/summary_spaced.json")"

# --- accept_parse_enumerated (REST body + log line) ------------------------
assert_eq "enumerated: 342 (REST chips_enumerated)" \
    "342" "$(accept_parse_enumerated < "$FIX/status_enumerated_342.json")"
assert_eq "enumerated: 189 (dcentrald log line)" \
    "189" "$(accept_parse_enumerated < "$FIX/log_enumerated_189.txt")"
assert_eq "enumerated: 84 (S15 capture-first fixture)" \
    "84" "$(accept_parse_enumerated < "$FIX/status_s15_capture_84.json")"
assert_eq "enumerated: 144 (S17 fixture)" \
    "144" "$(accept_parse_enumerated < "$FIX/log_s17_enumerated_144.txt")"
assert_eq "enumerated: 195 (S17+ BM1396 fixture)" \
    "195" "$(accept_parse_enumerated < "$FIX/status_s17plus_enumerated_195.json")"
assert_eq "enumerated: 90 (T17 fixture)" \
    "90" "$(accept_parse_enumerated < "$FIX/status_t17_enumerated_90.json")"
assert_eq "enumerated: 132 (T17+ BM1396 fixture)" \
    "132" "$(accept_parse_enumerated < "$FIX/log_t17plus_enumerated_132.txt")"

# --- accept_verdict (the accept gate decision) -----------------------------
# PASS when count >= threshold; FAIL otherwise; junk coerces safely.
v=$(accept_verdict 7 3); rc=$?; assert_eq "verdict 7>=3 text" "PASS" "$v"; assert_rc "verdict 7>=3 rc" 0 "$rc"
v=$(accept_verdict 3 3); rc=$?; assert_eq "verdict 3>=3 text (boundary)" "PASS" "$v"; assert_rc "verdict 3>=3 rc" 0 "$rc"
v=$(accept_verdict 2 3); rc=$?; assert_eq "verdict 2<3 text" "FAIL" "$v"; assert_rc "verdict 2<3 rc" 1 "$rc"
v=$(accept_verdict 0 1); rc=$?; assert_eq "verdict 0<1 text (dead miner)" "FAIL" "$v"; assert_rc "verdict 0<1 rc" 1 "$rc"
v=$(accept_verdict "" 3); rc=$?; assert_eq "verdict empty->0 text" "FAIL" "$v"; assert_rc "verdict empty->0 rc" 1 "$rc"
v=$(accept_verdict "xx" 1); rc=$?; assert_eq "verdict junk->0 text" "FAIL" "$v"; assert_rc "verdict junk->0 rc" 1 "$rc"

# End-to-end: parse a live-shaped body then gate on it.
n=$(accept_parse_accepted < "$FIX/summary_accepted7.json")
v=$(accept_verdict "$n" 5); rc=$?
assert_eq "e2e parse+gate (7 shares, N=5) text" "PASS" "$v"
assert_rc "e2e parse+gate (7 shares, N=5) rc" 0 "$rc"

# --- accept_enum_verdict (enumeration sanity) ------------------------------
v=$(accept_enum_verdict 342 342); rc=$?; assert_eq "enum 342==342" "PASS" "$v"; assert_rc "enum 342==342 rc" 0 "$rc"
v=$(accept_enum_verdict 340 342); rc=$?; assert_eq "enum 340~342 (in band)" "PASS" "$v"; assert_rc "enum 340~342 rc" 0 "$rc"
v=$(accept_enum_verdict 28 342); rc=$?; assert_eq "enum 28<<342 (partial chain, FAIL)" "FAIL" "$v"; assert_rc "enum 28 rc" 1 "$rc"
v=$(accept_enum_verdict 96 0); rc=$?; assert_eq "enum 96 vs UNCONFIRMED->CAPTURE" "CAPTURE" "$v"; assert_rc "enum capture rc" 0 "$rc"
v=$(accept_enum_verdict 0 0); rc=$?; assert_eq "enum 0 vs UNCONFIRMED->FAIL" "FAIL" "$v"; assert_rc "enum 0/0 rc" 1 "$rc"
v=$(accept_enum_verdict "$(accept_parse_enumerated < "$FIX/status_s15_capture_84.json")" 0); rc=$?; assert_eq "enum S15 capture fixture" "CAPTURE" "$v"; assert_rc "enum S15 capture rc" 0 "$rc"
v=$(accept_enum_verdict "$(accept_parse_enumerated < "$FIX/log_s17_enumerated_144.txt")" 144); rc=$?; assert_eq "enum S17 144 fixture" "PASS" "$v"; assert_rc "enum S17 fixture rc" 0 "$rc"
v=$(accept_enum_verdict "$(accept_parse_enumerated < "$FIX/status_s17plus_enumerated_195.json")" 195); rc=$?; assert_eq "enum S17+ 195 fixture" "PASS" "$v"; assert_rc "enum S17+ fixture rc" 0 "$rc"
v=$(accept_enum_verdict "$(accept_parse_enumerated < "$FIX/status_t17_enumerated_90.json")" 90); rc=$?; assert_eq "enum T17 90 fixture" "PASS" "$v"; assert_rc "enum T17 fixture rc" 0 "$rc"
v=$(accept_enum_verdict "$(accept_parse_enumerated < "$FIX/log_t17plus_enumerated_132.txt")" 132); rc=$?; assert_eq "enum T17+ 132 fixture" "PASS" "$v"; assert_rc "enum T17+ fixture rc" 0 "$rc"

# --- accept_parse_temp_c + accept_temp_safe (soak thermal guard) -----------
assert_eq "temp: 49.3 (REST temp_c)" \
    "49.3" "$(accept_parse_temp_c < "$FIX/status_enumerated_342.json")"
assert_eq "temp: max across multiple readings (never mask a hot board)" \
    "71" "$(printf '{"temp_c":55,"chip_temp_c":71,"board_temp_c":60}' | accept_parse_temp_c)"
assert_eq "temp: empty when absent" \
    "" "$(printf '{"hashrate_ghs":100}' | accept_parse_temp_c)"

v=$(accept_temp_safe 49.3 75); rc=$?; assert_eq "temp 49.3<=75 SAFE" "SAFE" "$v"; assert_rc "temp safe rc" 0 "$rc"
v=$(accept_temp_safe 75 75); rc=$?; assert_eq "temp 75<=75 boundary SAFE" "SAFE" "$v"; assert_rc "temp boundary rc" 0 "$rc"
v=$(accept_temp_safe 80 75); rc=$?; assert_eq "temp 80>75 HOT" "HOT" "$v"; assert_rc "temp hot rc" 1 "$rc"
v=$(accept_temp_safe 91.5 75); rc=$?; assert_eq "temp 91.5>75 HOT (fractional)" "HOT" "$v"; assert_rc "temp hot frac rc" 1 "$rc"
v=$(accept_temp_safe "" 75); rc=$?; assert_eq "temp missing fails closed" "UNKNOWN" "$v"; assert_rc "temp missing rc" 1 "$rc"
v=$(accept_temp_safe "junk" 75); rc=$?; assert_eq "temp junk fails closed" "UNKNOWN" "$v"; assert_rc "temp junk rc" 1 "$rc"

# --- accept_soak_verdict (sustained-mining stability gate) ------------------
# Catches what the single-point accept gate cannot: first-shares-then-die-spiral,
# thermal throttle, or stall. Each soak() arg is one "elapsed acc mhs temp" line.
soak() { printf '%s\n' "$@"; }

v=$(soak "60 5 13000000 62" "120 8 12950000 64" "180 12 13010000 63" | accept_soak_verdict 75); rc=$?
assert_eq "soak: stable run PASS" "SOAK_PASS" "$v"; assert_rc "soak stable rc" 0 "$rc"

v=$(soak "60 5 13000000 62" "120 8 5000000 64" "180 9 4800000 63" | accept_soak_verdict 75); rc=$?
assert_eq "soak: hashrate collapse FAIL" "SOAK_FAIL:hashrate_collapse(min=4800000<floor=9100000)" "$v"
assert_rc "soak collapse rc" 1 "$rc"

v=$(soak "60 5 13000000 62" "120 5 13000000 64" "180 5 13000000 63" | accept_soak_verdict 75); rc=$?
assert_eq "soak: shares stalled FAIL" "SOAK_FAIL:shares_not_advancing(5->5)" "$v"
assert_rc "soak stalled rc" 1 "$rc"

v=$(soak "60 5 13000000 62" "120 8 13000000 80" "180 12 13000000 63" | accept_soak_verdict 75); rc=$?
assert_eq "soak: thermal excursion FAIL" "SOAK_FAIL:thermal_excursion" "$v"; assert_rc "soak hot rc" 1 "$rc"

v=$(soak "60 5 13000000 62" "120 8 13000000 " "180 12 13000000 63" | accept_soak_verdict 75); rc=$?
assert_eq "soak: thermal blind fails closed" "SOAK_FAIL:thermal_blind" "$v"; assert_rc "soak blind rc" 1 "$rc"

v=$(soak "60 5 13000000 62" | accept_soak_verdict 75); rc=$?
assert_eq "soak: too few samples FAIL" "SOAK_FAIL:too_few_samples(1<3)" "$v"; assert_rc "soak short rc" 1 "$rc"

v=$(soak "60 5 13000000 62" "120 8 xx 64" "180 12 13000000 63" | accept_soak_verdict 75); rc=$?
assert_eq "soak: nonnumeric mhs fails closed" "SOAK_FAIL:nonnumeric_mhs" "$v"; assert_rc "soak junk mhs rc" 1 "$rc"

# Retention boundary: min == exactly 70% of max -> PASS (floor is inclusive).
v=$(soak "60 5 10000000 62" "120 8 7000000 63" "180 12 9000000 63" | accept_soak_verdict 75 70); rc=$?
assert_eq "soak: min==70% of max PASS" "SOAK_PASS" "$v"; assert_rc "soak retention boundary rc" 0 "$rc"

# Redirected (not piped) stdin must accumulate identically (no subshell loss).
v=$(accept_soak_verdict 75 <<SOAK_TEST_EOF
60 5 13000000 62
120 8 12900000 64
180 12 13000000 63
SOAK_TEST_EOF
); rc=$?
assert_eq "soak: redirected stdin PASS" "SOAK_PASS" "$v"; assert_rc "soak redirect rc" 0 "$rc"

# --- accept_boot_verdict (serial/boot-log stall-stage diagnosis) ------------
# Turns a captured UART cold-boot log into an actionable stall-point verdict —
# the missing analysis step for the deferred SD-first cold-boot blockers.
bl() { printf '%s\n' "$@"; }

v=$(bl "U-Boot 2019.01" "Starting kernel" "Freeing unused kernel memory" "dcentrald v0.6" "enumerated 189 chips" "ACCEPT GATE PASS: 5 accepted shares" | accept_boot_verdict); rc=$?
assert_eq "boot: full boot to mining -> PASS" "BOOT_PASS" "$v"; assert_rc "boot pass rc" 0 "$rc"

v=$(bl "U-Boot 2019.01" "Starting kernel" "Kernel panic - not syncing" | accept_boot_verdict); rc=$?
assert_eq "boot: kernel panic diagnosed at kernel" "BOOT_FAIL:kernel" "$v"; assert_rc "boot kernel rc" 1 "$rc"

v=$(bl "U-Boot 2019.01" "Booting Linux" "BusyBox v1.31" "Starting S40network" | accept_boot_verdict); rc=$?
assert_eq "boot: userspace stall diagnosed at init" "BOOT_FAIL:init" "$v"; assert_rc "boot init rc" 1 "$rc"

v=$(bl "U-Boot SPL 2019" "Run /sbin/init" "dcentrald v0.6" "enumerated 126 chips" | accept_boot_verdict); rc=$?
assert_eq "boot: enum-no-shares diagnosed at enum" "BOOT_FAIL:enum" "$v"; assert_rc "boot enum rc" 1 "$rc"

v=$(bl "U-Boot 2019.01" "Hit any key to stop autoboot" | accept_boot_verdict); rc=$?
assert_eq "boot: bootloader hang diagnosed at uboot" "BOOT_FAIL:uboot" "$v"; assert_rc "boot uboot rc" 1 "$rc"

v=$(bl "random noise with no boot markers" | accept_boot_verdict); rc=$?
assert_eq "boot: markerless log fails closed at none" "BOOT_FAIL:none" "$v"; assert_rc "boot none rc" 1 "$rc"

# --- accept_matrix_verdict (release-readiness roll-up / validation dashboard) --
# stderr grid discarded (2>/dev/null); stdout carries only the machine verdict.
mj() { printf '%s\n' "$@"; }

v=$(mj '{"result":"PASS","sku":"S9","mode":"capstone"}' '{"result":"PASS","sku":"S19jPro","mode":"soak"}' | accept_matrix_verdict 2>/dev/null); rc=$?
assert_eq "matrix: all PASS -> RELEASE_GO" "RELEASE_GO" "$v"; assert_rc "matrix go rc" 0 "$rc"

v=$(mj '{"result":"PASS","sku":"S9","mode":"capstone"}' '{"result":"FAIL","sku":"S21","mode":"soak"}' 'log noise line' | accept_matrix_verdict 2>/dev/null); rc=$?
assert_eq "matrix: a FAIL -> RELEASE_NOGO lists sku:phase" "RELEASE_NOGO:S21:soak" "$v"; assert_rc "matrix nogo rc" 1 "$rc"

v=$(mj '{"result":"FAIL","sku":"S17","mode":"bootlog"}' '{"result":"FAIL","sku":"S21XP","mode":"capstone"}' | accept_matrix_verdict 2>/dev/null); rc=$?
assert_eq "matrix: multiple FAILs listed comma-separated" "RELEASE_NOGO:S17:bootlog,S21XP:capstone" "$v"; assert_rc "matrix multi rc" 1 "$rc"

v=$(printf '' | accept_matrix_verdict 2>/dev/null); rc=$?
assert_eq "matrix: no results fails closed" "RELEASE_NOGO:no_results" "$v"; assert_rc "matrix empty rc" 1 "$rc"

v=$(mj 'just a log line' 'another non-json' | accept_matrix_verdict 2>/dev/null); rc=$?
assert_eq "matrix: non-JSON only fails closed" "RELEASE_NOGO:no_results" "$v"; assert_rc "matrix nonjson rc" 1 "$rc"

# --- accept_ota_verdict (witnessed-OTA-capstone stage diagnosis) -------------
# Encodes the  OTA truth contracts: uploaded != scheduled != flashed !=
# mining, so a weaker signal can never score as a capstone pass.
ol() { printf '%s\n' "$@"; }

v=$(ol "upload accepted" "OTA signature verified" "sysupgrade scheduled" "reboot observed" "version matches expected" "ACCEPT GATE PASS: 5 accepted shares" | accept_ota_verdict); rc=$?
assert_eq "ota: full capstone -> PASS" "OTA_PASS" "$v"; assert_rc "ota pass rc" 0 "$rc"

v=$(ol "upload accepted" "signature check: BAD" | accept_ota_verdict); rc=$?
assert_eq "ota: unsigned image stalls at uploaded (not verified)" "OTA_FAIL:uploaded" "$v"; assert_rc "ota unsigned rc" 1 "$rc"

v=$(ol "uploaded" "signature verified" "sysupgrade scheduled" | accept_ota_verdict); rc=$?
assert_eq "ota: scheduled != flashed" "OTA_FAIL:scheduled" "$v"; assert_rc "ota scheduled rc" 1 "$rc"

v=$(ol "uploaded" "signature verified" "scheduled" "reboot observed" "post-reboot version ok" | accept_ota_verdict); rc=$?
assert_eq "ota: version-ok-no-shares stalls at version_confirmed" "OTA_FAIL:version_confirmed" "$v"; assert_rc "ota version rc" 1 "$rc"

v=$(ol "random noise no markers" | accept_ota_verdict); rc=$?
assert_eq "ota: markerless transcript fails closed" "OTA_FAIL:none" "$v"; assert_rc "ota none rc" 1 "$rc"

if [ "$fails" -ne 0 ]; then
    printf '\naccept_parse tests FAILED: %s assertion(s)\n' "$fails" >&2
    exit 1
fi
printf '\naccept_parse tests passed.\n'
