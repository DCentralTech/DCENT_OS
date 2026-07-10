//! NF-08 (optional): exercise the REAL env-gated `ChipRegistry::new()` path.
//!
//! `ChipRegistry::new()` reads the two scaffold env gates
//! (`DCENT_ALLOW_SCAFFOLD_ASIC_DRIVERS` + `DCENT_CONFIRM_SCAFFOLD_DRIVERS_ARE_SIMULATOR_STUBS`)
//! at runtime. This file keeps EXACTLY ONE `#[test]` because it mutates
//! process-global env: each integration-test FILE compiles to its own process,
//! so a single test here cannot race parallel tests in other files. Test 2 in
//! `nf08_s23_detect_refuses_mining.rs` already proves the two-gate policy
//! env-free (pure fn + `with_scaffold_drivers()`); this is the belt-and-braces
//! proof that `new()` actually consumes those env vars. Edition 2021 -> plain
//! `std::env::set_var` is safe.

use dcentrald_asic::drivers::{
    bm1373, ChipRegistry, ALLOW_SCAFFOLD_ASIC_DRIVERS_ENV, SCAFFOLD_STUB_ACK_ENV,
};

#[test]
fn s23_env_gate_registers_scaffold_via_new() {
    // Customer/production path: gates unset -> production() -> no S23 driver.
    assert!(
        ChipRegistry::production()
            .detect(bm1373::ENUM_CHIP_ID)
            .is_none(),
        "0x1372 must be absent from the production registry"
    );

    // Both scaffold gates on -> new() registers the dual-keyed BM1373 scaffold.
    std::env::set_var(ALLOW_SCAFFOLD_ASIC_DRIVERS_ENV, "1");
    std::env::set_var(SCAFFOLD_STUB_ACK_ENV, "1");
    let resolved = ChipRegistry::new()
        .detect(bm1373::ENUM_CHIP_ID)
        .map(|drv| (drv.chip_name(), drv.chip_id()))
        .expect("both gates -> 0x1372 resolves via ChipRegistry::new()");
    std::env::remove_var(ALLOW_SCAFFOLD_ASIC_DRIVERS_ENV);
    std::env::remove_var(SCAFFOLD_STUB_ACK_ENV);

    assert_eq!(resolved.0, "BM1373");
    assert_eq!(resolved.1, bm1373::CHIP_ID, "driver reports canonical id");
}
