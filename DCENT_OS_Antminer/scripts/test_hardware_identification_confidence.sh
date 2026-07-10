#!/bin/sh
#
# Static hardware-identification confidence audit.
#
# HAL-8 requires identity telemetry to state how confident the firmware is in
# the reported ASIC/control-board identity. This host-only gate pins the DTO,
# runtime resolver, public JSON surfaces, and regression tests without probing
# live hardware.

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

require_file() {
    if [ -f "$1" ]; then
        pass "required file exists: $1"
    else
        fail "required file missing: $1"
    fi
}

require_pattern() {
    file=$1
    pattern=$2
    label=$3

    if [ ! -f "$file" ]; then
        fail "$label: missing file $file"
        return
    fi

    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        pass "$label"
    else
        fail "$label: missing pattern '$pattern' in $file"
    fi
}

api_lib='dcentrald/dcentrald-api/src/lib.rs'
rest_rs='dcentrald/dcentrald-api/src/rest.rs'
rest_late_rs='dcentrald/dcentrald-api/src/rest/late.rs'
hardware_rs='dcentrald/dcentrald/src/runtime/hardware_info.rs'

for f in "$api_lib" "$rest_rs" "$rest_late_rs" "$hardware_rs"; do
    require_file "$f"
done

require_pattern "$api_lib" 'pub struct HardwareIdentification {' \
    'hardware identification DTO exists'
require_pattern "$api_lib" 'pub confidence: String,' \
    'hardware identification confidence field exists'
require_pattern "$api_lib" 'pub sources: Vec<String>,' \
    'hardware identification evidence sources exist'
require_pattern "$api_lib" 'pub note: Option<String>,' \
    'hardware identification operator note exists'
require_pattern "$api_lib" 'pub identification: HardwareIdentification,' \
    'HardwareInfo carries structured identification'

require_pattern "$hardware_rs" 'fn resolve_chip_identity(' \
    'runtime has pure identity resolver'
require_pattern "$hardware_rs" 'configured model and baked board target agree on ASIC family' \
    'resolver documents high-confidence agreement'
require_pattern "$hardware_rs" 'configured model and baked board target disagree; using configured model chip label' \
    'resolver documents low-confidence conflict'
require_pattern "$hardware_rs" 'ASIC family is pinned by configured model only' \
    'resolver documents medium-confidence config-only identity'
require_pattern "$hardware_rs" 'ASIC family is pinned by baked board target' \
    'resolver documents board-target identity'
require_pattern "$hardware_rs" 'info.identification = chip_identity.identification;' \
    'collect_hardware_info publishes confidence telemetry'
require_pattern "$hardware_rs" 'identification_confidence = %info.identification.confidence' \
    'hardware-info startup log includes confidence'
require_pattern "$hardware_rs" 'chip_identity_surfaces_conflict_without_changing_precedence' \
    'conflict precedence has a Rust regression test'
require_pattern "$hardware_rs" 'chip_identity_unknown_is_explicit' \
    'unknown identity has a Rust regression test'

require_pattern "$rest_rs" '"identification_confidence": &hw.identification.confidence' \
    '/api/system/info exposes identification confidence'
require_pattern "$rest_rs" '"identification": &hw.identification' \
    '/api/system/info exposes structured identification'
require_pattern "$rest_late_rs" 'mcp_device_info_surfaces_hardware_identification_confidence' \
    'MCP device-info identity serialization has a Rust regression test'

if [ "$failures" -ne 0 ]; then
    printf '\nhardware-identification confidence audit failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nhardware-identification confidence audit passed.\n'
