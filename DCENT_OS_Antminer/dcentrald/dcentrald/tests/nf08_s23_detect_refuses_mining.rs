//! NF-08: the daemon refuses to mine on an Antminer S23 (BM1373).
//!
//! Real S23 silicon self-reports chip-id **0x1372** on enumeration
//! (NerdQAxePlus `chip_id[6]`), while DCENT keys the pre-hardware scaffold on
//! its canonical **0x1373** and dual-keys BOTH ids to the same fail-closed
//! BM1373 scaffold (operator decision 2026-07-08). This test lives in the
//! DAEMON crate (not the ASIC crate) to prove that the *daemon's* and
//! *work-dispatcher's* ASIC-dispatch routes both go through the same
//! env-gated `ChipRegistry`, so an S23 that is absent from `production()`
//! gets NO driver -> NO work -> refusal to mine.
//!
//! It follows the source-pin idiom of `tests/mixed_chip_divergence.rs`
//! (include_str! + `.contains(...)`) for the parts that cannot be exercised
//! host-side (the fail-closed `init_chain`/`send_work` bodies need a live
//! `FpgaChain::open` UIO device; the daemon/dispatcher routing lives in
//! HAL-heavy code paths). Additive + fail-closed: it asserts existing
//! behavior, changes no production source.

use dcentrald_asic::drivers::{
    bm1373, is_scaffold_driver_chip, should_register_scaffold_drivers, ChipDriver, ChipRegistry,
    MinerProfile, ALLOW_SCAFFOLD_ASIC_DRIVERS_ENV, SCAFFOLD_STUB_ACK_ENV,
};

// Source-pin the routing/fail-closed evidence. Relative paths match the
// `mixed_chip_divergence.rs` precedent (`../src`, `../../<sibling-crate>`).
const DRIVERS_MOD_RS: &str = include_str!("../../dcentrald-asic/src/drivers/mod.rs");
const BM1373_RS: &str = include_str!("../../dcentrald-asic/src/drivers/bm1373.rs");
const DAEMON_RS: &str = include_str!("../src/daemon.rs");
const WORK_DISPATCHER_RS: &str = include_str!("../src/work_dispatcher.rs");

/// The customer/production `ChipRegistry::new()` path (both gates unset ->
/// `production()`) gives NO driver for either S23 id, so a live S23 cannot be
/// driven. Both ids are classified as scaffold chips.
#[test]
fn s23_both_ids_absent_from_production_registry() {
    assert_eq!(bm1373::ENUM_CHIP_ID, 0x1372, "S23 enumerates as 0x1372");
    assert_eq!(bm1373::CHIP_ID, 0x1373, "DCENT canonical scaffold key");

    let prod = ChipRegistry::production();
    assert!(
        prod.detect(0x1372).is_none(),
        "0x1372 must not resolve to a production driver"
    );
    assert!(
        prod.detect(0x1373).is_none(),
        "0x1373 must not resolve to a production driver"
    );

    assert!(is_scaffold_driver_chip(0x1372));
    assert!(is_scaffold_driver_chip(0x1373));
}

/// The BM1373 scaffold is dual-keyed only behind BOTH scaffold gates. With the
/// gates on (proved env-free via `with_scaffold_drivers()`), both ids resolve
/// to the same BM1373 driver reporting the canonical id, and both map to the
/// "Antminer S23" profile.
#[test]
fn s23_dual_keyed_only_behind_both_scaffold_gates() {
    // Two-gate policy (pure fn — no process env mutation).
    assert!(!should_register_scaffold_drivers(false, false));
    assert!(!should_register_scaffold_drivers(true, false));
    assert!(!should_register_scaffold_drivers(false, true));
    assert!(should_register_scaffold_drivers(true, true));
    assert_ne!(
        ALLOW_SCAFFOLD_ASIC_DRIVERS_ENV, SCAFFOLD_STUB_ACK_ENV,
        "the two gates must be independent env vars"
    );

    // Env-free both-gates-on equivalent.
    let s = ChipRegistry::with_scaffold_drivers();
    let a = s
        .detect(0x1372)
        .expect("0x1372 resolves with scaffold drivers");
    let b = s
        .detect(0x1373)
        .expect("0x1373 resolves with scaffold drivers");
    assert_eq!(a.chip_name(), "BM1373");
    assert_eq!(b.chip_name(), "BM1373");
    // The driver reports its own canonical id regardless of the key it was
    // registered under.
    assert_eq!(a.chip_id(), bm1373::CHIP_ID);
    assert_eq!(b.chip_id(), bm1373::CHIP_ID);

    assert_eq!(MinerProfile::for_chip(0x1372).unwrap().name, "Antminer S23");
    assert_eq!(MinerProfile::for_chip(0x1373).unwrap().name, "Antminer S23");
}

/// Even when registered (scaffold gates on), the BM1373 driver is fail-closed:
/// its hardware-touching methods return `Err`. `init_chain`/`set_frequency`/
/// `send_work` cannot be called host-side (they need a live `FpgaChain::open`
/// UIO device), so the `Err` bodies are source-pinned; the host-constructible
/// driver still exposes safe, stable metadata.
#[test]
fn s23_scaffold_driver_is_fail_closed() {
    assert!(
        BM1373_RS.contains("BM1373 driver is a pre-hardware scaffold"),
        "init_chain must refuse live bring-up"
    );
    assert!(
        BM1373_RS.contains("BM1373 set_frequency gated until live S23 verification"),
        "set_frequency must fail closed"
    );
    assert!(
        BM1373_RS.contains("BM1373 send_work gated until live S23 verification"),
        "send_work must fail closed"
    );

    let d = bm1373::Bm1373Driver::new();
    assert_eq!(d.chip_id(), 0x1373);
    assert_eq!(d.cores_per_chip(), 128);
    assert_eq!(d.response_length(), 11);
}

/// Both ASIC-dispatch routes resolve drivers through the registry, while the
/// dispatcher first requires its typed production-write identity. S23 scaffold
/// IDs are runtime-tested against that typed boundary in `work_dispatcher.rs`.
#[test]
fn daemon_and_dispatcher_gate_dispatch_through_registry_and_typed_identity() {
    assert!(DAEMON_RS.contains("ChipRegistry::with_execution_policy("));
    assert!(DAEMON_RS.contains("registry.detect(self.chip_id)"));
    assert!(DAEMON_RS.contains("No built-in driver for ChipID"));

    assert!(WORK_DISPATCHER_RS.contains("ChipRegistry::with_execution_policy("));
    assert!(WORK_DISPATCHER_RS.contains("normalize_dispatch_write_identity("));
    assert!(WORK_DISPATCHER_RS.contains("registry.detect(chip.chip_id())"));
    assert!(WORK_DISPATCHER_RS.contains("if driver.is_none()"));
    assert!(WORK_DISPATCHER_RS.contains("cannot generate ASIC work for unknown chip type"));

    // The env-gated `new()` consumes the two-gate policy, and the S23 scaffold
    // is dual-keyed via `register_alias`.
    assert!(DRIVERS_MOD_RS.contains("should_register_scaffold_drivers(allow, ack)"));
    assert!(DRIVERS_MOD_RS.contains("register_alias_with_maturity("));
    assert!(DRIVERS_MOD_RS.contains("bm1373::ENUM_CHIP_ID,"));
}
