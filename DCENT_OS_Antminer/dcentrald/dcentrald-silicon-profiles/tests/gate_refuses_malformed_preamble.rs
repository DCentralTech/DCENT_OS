//!  B2 (2026-05-22): integration test — energize gate must refuse
//! malformed EEPROM preambles in strict mode AND surface them in
//! telemetry-only mode.
//!
//! Refusal classes covered:
//! - Unknown 2-byte preamble (`0xDE 0xAD`)
//! - Sub-byte buffer (caught upstream as ReadError, silently skipped — NOT
//!   a refusal on its own, since AT24 driver-binding races can produce
//!   transient empty reads; this is documented behaviour).
//! - All-`0xFF` (Unpopulated — not a refusal; documented).
//!
//! See `dcentrald-silicon-profiles::energize_gate` module docs §"What
//! this gate refuses" + §"What this gate does NOT do".

use dcentrald_silicon_profiles::energize_gate::{
    classify_chain, gate_chains_for_energize, ChainProbe, RefusalReason,
};
use dcentrald_silicon_profiles::hashboards::Hashboard;

#[test]
fn strict_refuses_unknown_preamble() {
    // Chain 0 is a healthy BHB42601 (.109 reference class); chain 1
    // reports junk preamble. The gate must refuse chain 1 and the
    // operator must see the refusal in the summary.
    let healthy = ChainProbe::Classified {
        chain_id: 0,
        preamble: [0x04, 0x11],
        sku: Hashboard::Bhb42601,
    };
    let bad = classify_chain(1, Some(&[0xde, 0xad, 0xbe, 0xef]));
    assert!(matches!(bad, ChainProbe::MalformedPreamble { .. }));

    let refusal = gate_chains_for_energize(&[healthy, bad], true)
        .expect_err("strict mode must refuse malformed preamble");

    assert_eq!(refusal.reasons.len(), 1);
    match &refusal.reasons[0] {
        RefusalReason::MalformedPreamble { chain_id, preamble } => {
            assert_eq!(*chain_id, 1);
            assert_eq!(*preamble, [0xde, 0xad]);
        }
        other => panic!("expected MalformedPreamble, got {:?}", other),
    }
    let s = refusal.summary();
    assert!(s.contains("chain=1"));
    assert!(s.contains("malformed-preamble"));
    assert!(s.contains("0xDE 0xAD"));
}

#[test]
fn telemetry_mode_surfaces_malformed_but_does_not_error() {
    // First-deploy mode: operator runs with DCENT_AM2_STRICT_SKU_REFUSE
    // unset. The gate must NOT block (returns Ok) but the refusal must
    // still surface so the operator sees it in logs.
    let healthy = ChainProbe::Classified {
        chain_id: 0,
        preamble: [0x04, 0x11],
        sku: Hashboard::Bhb42601,
    };
    let bad = classify_chain(2, Some(&[0xab, 0xcd, 0xef, 0x01]));

    let (bindings, telemetry) =
        gate_chains_for_energize(&[healthy, bad], false).expect("strict=false never errors");
    assert_eq!(bindings.len(), 1, "only chain 0 binds");
    assert_eq!(telemetry.reasons.len(), 1);
    assert!(matches!(
        telemetry.reasons[0],
        RefusalReason::MalformedPreamble { chain_id: 2, .. }
    ));
}

#[test]
fn hashboards_canonical_enum_compiles() {
    // Compile-touch: confirm the canonical Hashboard enum the gate
    // accepts is the same enum the hashboards module exports (no
    // duplicated type). If this stops compiling, someone has forked
    // the SKU enum and the gate's refusal table no longer covers all
    // SKUs.
    let _ = Hashboard::Bhb42601;
    let _ = Hashboard::Bhb56902;
}
