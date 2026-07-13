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

require_absent_pattern() {
    file=$1
    pattern=$2
    label=$3

    if grep -F -- "$pattern" "$file" >/dev/null 2>&1; then
        fail "$label: forbidden pattern '$pattern' remains in $file"
    else
        pass "$label"
    fi
}

require_pattern_count() {
    file=$1
    pattern=$2
    expected=$3
    label=$4

    if [ ! -f "$file" ]; then
        fail "$label: missing file $file"
        return
    fi
    actual=$(grep -F -c -- "$pattern" "$file" || true)
    if [ "$actual" -eq "$expected" ]; then
        pass "$label"
    else
        fail "$label: expected $expected occurrence(s) of '$pattern' in $file, found $actual"
    fi
}

api_lib='dcentrald/dcentrald-api/src/lib.rs'
rest_rs='dcentrald/dcentrald-api/src/rest.rs'
rest_late_rs='dcentrald/dcentrald-api/src/rest/late.rs'
hardware_rs='dcentrald/dcentrald/src/runtime/hardware_info.rs'
publication_rs='dcentrald/dcentrald/src/asic_identity_publication.rs'
daemon_rs='dcentrald/dcentrald/src/daemon.rs'
dispatcher_rs='dcentrald/dcentrald/src/work_dispatcher.rs'
main_rs='dcentrald/dcentrald/src/main.rs'

for f in "$api_lib" "$rest_rs" "$rest_late_rs" "$hardware_rs" \
    "$publication_rs" "$daemon_rs" "$dispatcher_rs" "$main_rs"; do
    require_file "$f"
done

require_pattern "$api_lib" 'pub struct HardwareIdentification {' \
    'hardware identification DTO exists'
require_pattern "$api_lib" 'pub use dcent_schema::capability::IdentityConfidence as HardwareIdentityConfidence;' \
    'hardware identity reuses the canonical capability confidence enum'
require_pattern "$api_lib" 'pub confidence: HardwareIdentityConfidence,' \
    'hardware identification confidence is typed'
require_pattern "$api_lib" 'pub sources: Vec<String>,' \
    'legacy hardware identification source tags remain wire-compatible'
require_pattern "$api_lib" 'pub evidence: Vec<HardwareIdentityEvidence>,' \
    'canonical typed hardware identity evidence is public'
require_pattern "$api_lib" 'pub enum HardwareIdentityEvidenceLevel {' \
    'declared observed measured evidence levels are closed'
require_pattern "$api_lib" 'Measured(MeasuredIdentitySource),' \
    'measured evidence uses a distinct source namespace'
require_pattern "$api_lib" 'measured_asic_enumeration' \
    'measured ASIC evidence requires an enumeration constructor'
require_pattern "$api_lib" 'pub struct HardwareCompositionToken {' \
    'measured identity exposes a generation/composition token'
require_pattern "$api_lib" 'pub composition: Option<HardwareCompositionToken>,' \
    'typed identity evidence carries optional composition binding'
require_absent_pattern "$api_lib" 'pub confidence: String,' \
    'unconstrained confidence string is removed'
require_pattern "$api_lib" 'pub note: Option<String>,' \
    'hardware identification operator note exists'
require_pattern "$api_lib" 'pub identification: HardwareIdentification,' \
    'HardwareInfo carries structured identification'

require_pattern "$hardware_rs" 'fn resolve_chip_identity(' \
    'runtime has pure identity resolver'
require_pattern "$hardware_rs" 'configured model and baked board target agree on ASIC family' \
    'resolver documents correlated declaration agreement'
require_pattern "$hardware_rs" 'configured model and baked board target disagree; using configured model chip label' \
    'resolver documents low-confidence conflict'
require_pattern "$hardware_rs" 'ASIC family is declared by configured model only' \
    'resolver labels config-only identity as declared'
require_pattern "$hardware_rs" 'ASIC family is declared by baked board target only' \
    'resolver labels board-target identity as declared'
require_pattern "$hardware_rs" 'info.identification = chip_identity.identification;' \
    'collect_hardware_info publishes confidence telemetry'
require_pattern "$hardware_rs" 'identification_confidence = ?info.identification.confidence' \
    'hardware-info startup log uses typed confidence'
require_pattern "$hardware_rs" 'correlated_declarations_do_not_mint_high_confidence' \
    'correlated declarations have a negative Rust regression test'
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
require_pattern "$rest_rs" 'strongest_asic_evidence_level()' \
    'capability authority derives from typed ASIC evidence'
require_pattern "$rest_rs" 'has_declared_asic_target(hw, "am1-s9", "BM1387")' \
    'BM1387 beta support requires exact typed S9 composition evidence'
require_pattern "$rest_late_rs" 'hardware_identity_bm1387_chip_id_alone_does_not_promote_unknown_composition' \
    'BM1387 chip-only promotion has a negative Rust regression test'
require_pattern "$rest_late_rs" 'hardware_identity_bm1387_t9_declaration_is_not_the_s9_beta_anchor' \
    'BM1387 T9 composition has a negative Rust regression test'

require_pattern "$main_rs" 'mod asic_identity_publication;' \
    'measured ASIC publication module is wired into the daemon'
require_pattern "$publication_rs" 'pub(crate) struct EnumeratedMiningChainReceipt {' \
    'successful GetAddress enumeration has an explicit receipt type'
require_pattern "$publication_rs" 'from_successful_get_address' \
    'enumeration receipt construction names its measured provenance'
require_pattern "$publication_rs" 'state.active = None;' \
    'every composition transition invalidates the prior generation first'
require_pattern "$daemon_rs" 'self.asic_enumeration_receipts.clear();' \
    'every init generation invalidates earlier enumeration receipts'
require_pattern_count "$daemon_rs" \
    'EnumeratedMiningChainReceipt::from_successful_get_address(' 1 \
    'daemon has exactly one production receipt-minting site'
require_pattern "$daemon_rs" 'chain.mining && chain.chain_id == receipt.chain_id()' \
    'dispatcher snapshot filters receipts to currently mining chains'
require_pattern "$dispatcher_rs" 'publication.publish(self.chip_id)' \
    'active dispatcher supplies the final identity-consensus input'
require_pattern "$publication_rs" 'exact_consensus_publishes_generation_bound_measured_identity' \
    'exact all-chain consensus has a positive Rust regression test'
require_pattern "$publication_rs" 'partial_and_mixed_enumeration_never_publish_measured_identity' \
    'partial and mixed enumeration have negative Rust regression tests'
require_pattern "$publication_rs" 'assumed_chain_fields_without_get_address_receipts_never_publish_measured_identity' \
    'hot-start and model-assumed chain fields cannot mint measured identity'
require_pattern "$publication_rs" 'later_composition_revokes_and_rejects_stale_generation' \
    'stale dispatcher generations have a negative Rust regression test'
require_absent_pattern "$hardware_rs" 'measured_asic_enumeration(' \
    'config and platform collection cannot mint measured ASIC identity'
require_absent_pattern "$daemon_rs" 'measured_asic_enumeration(' \
    'daemon orchestration cannot bypass the publication port'
require_absent_pattern "$dispatcher_rs" 'measured_asic_enumeration(' \
    'dispatcher cannot construct measured evidence directly'

if grep -R -F -- 'identification.confidence = "exact".to_string()' \
    dcentrald/dcentrald-api/src dcentrald/dcentrald-api/tests >/dev/null 2>&1; then
    fail 'legacy exact-string test fixtures remain'
else
    pass 'legacy exact-string test fixtures are removed'
fi

if [ "$failures" -ne 0 ]; then
    printf '\nhardware-identification confidence audit failed: %s failure(s)\n' "$failures" >&2
    exit 1
fi

printf '\nhardware-identification confidence audit passed.\n'
