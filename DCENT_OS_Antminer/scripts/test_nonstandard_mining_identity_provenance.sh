#!/bin/sh
#
# Fail-closed audit for ASIC identity in mining engines that bypass Daemon.
#
# None of these engines currently retains a complete, generation-bound
# GetAddress receipt containing exact chain ID, enumerated chip count, and ASIC
# family for every active chain. They must therefore remain non-Measured:
#
# P0 serial_mining: family/geometry are resolved from model/config; some
#    GetAddress response counts are checked but not retained as identity, and
#    BM1366 may deliberately continue with a partial response set.
# P0 am3_bb_mining: GetAddress can retain CRC-verified repetitions of the one
#    captured BM1362 unassigned payload, but repetitions are not unique-chip or
#    chip-count evidence and the engine owns no composition generation. Address
#    assignment still uses catalog geometry. Trailer bits 6:5 remain opaque.
# P1 s19j_tap_mining: bosminer performed enumeration; dcentrald has no receipt.
# P1 stock_mining: read_chip_id() identifies the FPGA, and no per-chain ASIC
#    GetAddress result is retained.
#
# Promotion requires a typed, immutable receipt minted at successful response
# parsing plus a composition generation owned by that engine. Config, model,
# EEPROM, topology, non-zero RX, and a predecessor firmware are not substitutes.

set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd)
cd "$PROJECT_DIR"

failures=0

pass() {
    printf 'PASS: %s\n' "$*"
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

require_pattern() {
    file=$1
    pattern=$2
    label=$3
    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label: missing pattern '$pattern' in $file"
    fi
}

forbid_pattern() {
    pattern=$1
    label=$2
    shift 2
    if grep -F -- "$pattern" "$@" >/dev/null 2>&1; then
        fail "$label: forbidden pattern '$pattern' found"
    else
        pass "$label"
    fi
}

serial_rs='dcentrald/dcentrald/src/serial_mining.rs'
stock_rs='dcentrald/dcentrald/src/stock_mining.rs'
am3_rs='dcentrald/dcentrald/src/am3_bb_mining.rs'
protocol_rs='dcentrald/dcentrald-asic/src/protocol.rs'
tap_rs='dcentrald/dcentrald/src/s19j_tap_mining.rs'
engines="$serial_rs $stock_rs $am3_rs $tap_rs"

for file in $engines; do
    if [ -f "$file" ]; then
        pass "required engine exists: $file"
    else
        fail "required engine missing: $file"
    fi
done

# Pin the evidence behind the NO-SHIP decision so a nearby refactor cannot
# silently turn an assumption into a measurement.
require_pattern "$serial_rs" 'native serial mining requires an explicit recognized mining.model; chip count is geometry, not ASIC identity' \
    'serial identity remains explicitly declarative'
require_pattern "$serial_rs" 'let pre_responses = serial.read_all_responses(500)?;' \
    'serial GetAddress responses remain transient init checks'
require_pattern "$serial_rs" 'rambo_mode tolerance' \
    'serial partial-chain continuation remains visible'
require_pattern "$am3_rs" 'assigned_chips: usize,' \
    'AM3 result distinguishes assigned geometry from enumeration'
require_pattern "$am3_rs" 'initial_get_address_rx_bytes: usize,' \
    'AM3 GetAddress result remains byte-level liveness only'
require_pattern "$am3_rs" 'let n_assign = expected_chips_per_chain.clamp' \
    'AM3 address assignment remains catalog/config derived'
require_pattern "$am3_rs" 'struct CrcVerifiedUnassignedGetAddressObservation {' \
    'AM3 response parsing is explicitly limited to unassigned observations'
require_pattern "$am3_rs" '`response_frames` counts byte frames, not unique ASICs.' \
    'AM3 response repetitions cannot silently become a chip count'
require_pattern "$am3_rs" 'GetAddressIntegrity::CommandResponseCrc5Verified' \
    'AM3 parser records the narrow verified response-integrity claim'
require_pattern "$protocol_rs" 'pub fn bm13xx_command_response_crc5(data: &[u8]) -> u8 {' \
    'shared Rust protocol owns the command/register response CRC primitive'
require_pattern "$protocol_rs" 'intentionally not exposed here until an independent retained job-response' \
    'shared Rust response CRC does not claim job-response production support'
require_pattern "$am3_rs" 'am3_bb_get_address_stream_rejects_truncated_live_fixture' \
    'AM3 truncated live response has a golden negative regression test'
require_pattern "$am3_rs" 'am3_bb_get_address_stream_rejects_bad_preamble_and_non_fixture_payload' \
    'AM3 noise and non-captured payloads have golden negative regression tests'
require_pattern "$am3_rs" 'am3_bb_get_address_stream_rejects_bad_low_five_crc' \
    'AM3 corrupted response CRC has a golden negative regression test'
require_pattern "$am3_rs" 'am3_bb_get_address_stream_ignores_opaque_trailer_bits_six_and_five' \
    'AM3 parser does not assign meaning to trailer bits 6:5'
require_pattern "$am3_rs" 'am3_bb_get_address_uses_response_crc_not_shared_command_crc5' \
    'AM3 observed trailer uses the response-specific CRC rather than command CRC analogy'
require_pattern "$tap_rs" '**bosminer already did it**' \
    'tap mode continues to declare external enumeration ownership'
require_pattern "$stock_rs" 'let chip_id = fpga.read_chip_id();' \
    'stock chip identifier remains explicitly FPGA-sourced'

forbid_pattern 'measured_asic_enumeration(' \
    'non-standard engines cannot directly mint measured ASIC evidence' \
    $engines
forbid_pattern 'HardwareCompositionToken' \
    'non-standard engines cannot fabricate composition tokens' \
    $engines
forbid_pattern 'EnumeratedMiningChainReceipt' \
    'standard-daemon GetAddress receipts cannot be copied from assumptions' \
    $engines
forbid_pattern 'MeasuredEnumeration' \
    'non-standard engines cannot convert observations into Measured enumeration' \
    $engines
forbid_pattern 'HardwareIdentityConfidence::High' \
    'non-standard engines cannot directly assert High identity confidence' \
    $engines

if [ "$failures" -ne 0 ]; then
    printf '\nnon-standard mining identity provenance audit failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nnon-standard mining identity provenance audit passed (all engines remain non-Measured).\n'
