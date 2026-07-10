//!  B2 (2026-05-22): integration test — energize gate must
//! ACCEPT a `a lab unit`-class clean BHB42601 chain set without surfacing
//! any refusal reason.
//!
//! This is the LOAD-BEARING "don't break what works" pin. `a lab unit` is
//! the operator-confirmed mining unit; any future change to the gate
//! that would block `a lab unit`'s 3× BHB42601 cold boot is a regression.
//!
//! Per the spec: "Preserve `a lab unit` mining proof. `a lab unit` is the
//! operator-confirmed mining unit. Whatever EEPROM SKU `a lab unit`'s
//! chains report MUST remain in the 'healthy' path."

use dcentrald_silicon_profiles::energize_gate::{
    classify_chain, gate_chains_for_energize, ChainProbe,
};
use dcentrald_silicon_profiles::hashboards::Hashboard;

/// The exact preamble bytes `a lab unit` reports per Wave K telemetry
/// ( §7 #15 observe-half).
const BHB42XXX_PREAMBLE: [u8; 2] = [0x04, 0x11];

/// A minimal 16-byte EEPROM stub starting with the BHB42xxx preamble
/// followed by representative trailing bytes. Real `a lab unit` EEPROMs are
/// 256 bytes — the gate only inspects the first 2.
fn fake_bhb42601_eeprom_bytes() -> Vec<u8> {
    let mut v = vec![0u8; 16];
    v[0] = BHB42XXX_PREAMBLE[0];
    v[1] = BHB42XXX_PREAMBLE[1];
    // Fill with deterministic non-zero non-FF bytes so the
    // "unpopulated" detector doesn't fire on an unlucky all-zero
    // trailing payload.
    for (i, b) in v.iter_mut().enumerate().skip(2) {
        *b = (0x10 + i as u8) & 0x7f;
    }
    v
}

#[test]
fn three_chain_bhb42601_clean_boot_accepts() {
    // Reproduces .109's 3× BHB42xxx populated boot. Strict mode is ON;
    // the gate must still accept.
    let bytes = fake_bhb42601_eeprom_bytes();
    let probes: Vec<ChainProbe> = (0u8..3)
        .map(|chain| classify_chain(chain, Some(&bytes)))
        .collect();
    for p in &probes {
        assert!(
            matches!(
                p,
                ChainProbe::Classified {
                    sku: Hashboard::Bhb42601,
                    ..
                }
            ),
            "expected .109's chains to classify as BHB42601, got {:?}",
            p
        );
    }

    let (bindings, telemetry) =
        gate_chains_for_energize(&probes, true).expect(".109 clean boot must not be refused");

    assert_eq!(bindings.len(), 3);
    assert!(
        telemetry.is_empty(),
        "no refusal reasons on clean .109 boot"
    );

    // Confirm every binding carries the canonical preamble + the
    // canonical .109 SKU. (`classify_by_eeprom_preamble` returns the
    // family-canonical Bhb42601 — refinement to BHB42603/BHB42801/etc
    // happens at the toolbox install layer via `/etc/subtype`.)
    for b in &bindings {
        assert_eq!(b.preamble, BHB42XXX_PREAMBLE);
        assert_eq!(b.sku, Hashboard::Bhb42601);
    }
}

#[test]
fn two_chain_unit_with_one_unpopulated_slot_accepts() {
    // A 2-board home unit (e.g. one hashboard pulled for service)
    // must NOT be refused — chain 2 surfacing as unpopulated is a
    // valid configuration.
    let bytes = fake_bhb42601_eeprom_bytes();
    let probes = vec![
        classify_chain(0, Some(&bytes)),
        classify_chain(1, Some(&bytes)),
        classify_chain(2, Some(&[0xff; 32])), // unpopulated
    ];
    let (bindings, telemetry) =
        gate_chains_for_energize(&probes, true).expect("2-board unit must not be refused");
    assert_eq!(bindings.len(), 2);
    assert!(telemetry.is_empty());
}

#[test]
fn s19k_pro_bhb56902_clean_boot_accepts() {
    // Sibling check: a pure-BHB56902 (S19k Pro) unit must also pass
    // strict mode cleanly. The mixed-SKU rule only blocks
    // cross-family pairings, not single-family BHB56902 units.
    let mut bytes = vec![0u8; 16];
    bytes[0] = 0x05;
    bytes[1] = 0x11;
    for (i, b) in bytes.iter_mut().enumerate().skip(2) {
        *b = (0x20 + i as u8) & 0x7f;
    }
    let probes: Vec<ChainProbe> = (0u8..3)
        .map(|chain| classify_chain(chain, Some(&bytes)))
        .collect();
    let (bindings, telemetry) = gate_chains_for_energize(&probes, true)
        .expect("3× BHB56902 clean boot must not be refused");
    assert_eq!(bindings.len(), 3);
    assert!(telemetry.is_empty());
    for b in &bindings {
        assert_eq!(b.sku, Hashboard::Bhb56902);
    }
}
