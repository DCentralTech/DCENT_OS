//!  B2 (2026-05-22): integration test — energize gate must refuse
//! when the per-chain EEPROM read times out before classification.
//!
//! A timeout means the AT24 EEPROM didn't surface on either i2c-0 or
//! i2c-1 sysfs within the deadline. On AM2 / am3 BB this is the
//! "xiic-i2c is rebinding" / "EEPROM electrically present but bus
//! contention is preventing the read" symptom. Per the matrix §7 #15
//! drive-half plan we must fail closed — an unknown EEPROM state
//! cannot be safely energized.

use dcentrald_silicon_profiles::energize_gate::{
    classify_chain, gate_chains_for_energize, ChainProbe, RefusalReason,
};
use dcentrald_silicon_profiles::hashboards::Hashboard;

#[test]
fn strict_refuses_timeout_chain() {
    let healthy = ChainProbe::Classified {
        chain_id: 0,
        preamble: [0x04, 0x11],
        sku: Hashboard::Bhb42601,
    };
    let timed_out = ChainProbe::Timeout { chain_id: 1 };
    let refusal = gate_chains_for_energize(&[healthy, timed_out], true)
        .expect_err("strict mode must refuse timeout");
    assert!(matches!(
        refusal.reasons[0],
        RefusalReason::EepromReadinessTimeout { chain_id: 1 }
    ));
    assert!(refusal.summary().contains("eeprom-readiness-timeout"));
    assert!(refusal.summary().contains("chain=1"));
}

#[test]
fn telemetry_mode_surfaces_timeout_but_does_not_error() {
    let healthy = ChainProbe::Classified {
        chain_id: 0,
        preamble: [0x04, 0x11],
        sku: Hashboard::Bhb42601,
    };
    let timed_out = ChainProbe::Timeout { chain_id: 2 };

    let (bindings, telemetry) =
        gate_chains_for_energize(&[healthy, timed_out], false).expect("strict=false never errors");
    assert_eq!(bindings.len(), 1);
    assert_eq!(telemetry.reasons.len(), 1);
    assert!(matches!(
        telemetry.reasons[0],
        RefusalReason::EepromReadinessTimeout { chain_id: 2 }
    ));
}

#[test]
fn read_error_is_silently_skipped_not_refused() {
    // A `ReadError` (sysfs node missing) is treated as "no board in
    // slot" — NOT a refusal. AM2/am3 may legitimately ship 2-board
    // units with the third slot's AT24 absent.
    //
    // (If the caller has independent evidence the slot HAS a board,
    // they should map the read failure to Timeout instead — that's
    // documented in `classify_chain`'s rustdoc.)
    let healthy = ChainProbe::Classified {
        chain_id: 0,
        preamble: [0x04, 0x11],
        sku: Hashboard::Bhb42601,
    };
    let absent = classify_chain(2, None); // ReadError { chain_id: 2 }
    let (bindings, telemetry) =
        gate_chains_for_energize(&[healthy, absent], true).expect("ReadError is not a refusal");
    assert_eq!(bindings.len(), 1);
    assert!(telemetry.is_empty());
}
