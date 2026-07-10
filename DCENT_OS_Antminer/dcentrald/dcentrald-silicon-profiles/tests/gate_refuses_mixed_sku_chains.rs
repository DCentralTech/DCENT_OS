//!  B2 (2026-05-22): integration test — energize gate must refuse
//! when the chains report cross-family SKUs (BHB42xxx + BHB56902).
//!
//! Mixed-SKU is a hard-stop because the autotuner PVT envelopes,
//! chip-init register tables, and dsPIC voltage targets all diverge
//! between BHB42xxx (BM1362) and BHB56902 (BM1366). A single unit
//! reporting both means either (a) the hashboards were physically
//! mismatched during a rebuild, or (b) one EEPROM was overwritten —
//! both cases require operator inspection BEFORE any voltage.
//!
//! Same-family aliases (BHB42601 + BHB42801 on different chains, both
//! `0x04 0x11` preamble) are ALLOWED — these are valid high-bin/
//! standard mixes that share the same chip-init + PVT envelope shape.

use dcentrald_silicon_profiles::energize_gate::{
    gate_chains_for_energize, ChainProbe, RefusalReason,
};
use dcentrald_silicon_profiles::hashboards::Hashboard;

fn classified(chain_id: u8, sku: Hashboard, preamble: [u8; 2]) -> ChainProbe {
    ChainProbe::Classified {
        chain_id,
        preamble,
        sku,
    }
}

#[test]
fn strict_refuses_bhb42xxx_plus_bhb56902() {
    // The canonical cross-family pairing called out in the spec:
    // "chain 0 reads BHB42601, chain 1 reads BHB56902 — refuse all".
    let probes = vec![
        classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
        classified(1, Hashboard::Bhb56902, [0x05, 0x11]),
    ];
    let refusal = gate_chains_for_energize(&probes, true)
        .expect_err("strict mode must refuse cross-family mismatch");

    // Both chains should appear in the refusal — neither is
    // unilaterally correct.
    assert_eq!(refusal.reasons.len(), 2);

    let chain0_reason = refusal
        .reasons
        .iter()
        .find(|r| r.chain_id() == 0)
        .expect("chain 0 refusal must surface");
    let chain1_reason = refusal
        .reasons
        .iter()
        .find(|r| r.chain_id() == 1)
        .expect("chain 1 refusal must surface");
    assert!(matches!(chain0_reason, RefusalReason::MixedSkuChain { .. }));
    assert!(matches!(chain1_reason, RefusalReason::MixedSkuChain { .. }));

    let s = refusal.summary();
    assert!(s.contains("mixed-sku"));
    assert!(s.contains("BHB42601"));
    assert!(s.contains("BHB56902"));
}

#[test]
fn same_family_aliases_are_accepted() {
    // BHB42601 + BHB42801: distinct variants but identical preamble
    // (`0x04 0x11`). The mixed-SKU rule MUST NOT block this pairing.
    let probes = vec![
        classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
        classified(1, Hashboard::Bhb42801, [0x04, 0x11]),
        classified(2, Hashboard::Bhb42611, [0x04, 0x11]),
    ];
    let (bindings, telemetry) = gate_chains_for_energize(&probes, true)
        .expect("BHB42xxx family aliases must not be refused");
    assert_eq!(bindings.len(), 3);
    assert!(telemetry.is_empty());
}

#[test]
fn three_chains_one_mismatch_refuses_all_pairs() {
    // 2× BHB42xxx + 1× BHB56902. The mismatch chain (slot 2) pairs
    // with BOTH healthy chains; refusal must capture both pairings so
    // the operator sees full evidence in the summary.
    let probes = vec![
        classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
        classified(1, Hashboard::Bhb42601, [0x04, 0x11]),
        classified(2, Hashboard::Bhb56902, [0x05, 0x11]),
    ];
    let refusal = gate_chains_for_energize(&probes, true)
        .expect_err("strict mode must refuse cross-family mismatch");
    // 2 pairings × 2 reasons (one per chain in each pair) = 4
    assert_eq!(refusal.reasons.len(), 4);
    let mismatched_count = refusal
        .reasons
        .iter()
        .filter(|r| matches!(r, RefusalReason::MixedSkuChain { .. }))
        .count();
    assert_eq!(mismatched_count, 4);
}
