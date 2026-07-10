//!  (2026-05-22) — QA §10 CI-2.
//!
//! Belt-and-suspenders EEPROM-denylist fail-closed contract for the
//! bosminer-warmup wrapper. Asserts the wrapper's address-rejection
//! predicate refuses every byte in the AM2 hashboard EEPROM range
//! `0x50..=0x57` and accepts the canonical dsPIC addresses
//! `0x20`/`0x21`/`0x22` plus immediately-adjacent non-denylist values.
//!
//! ## Why this is a structural test (not a live wrapper invocation)
//!
//! The wrapper's runtime entry point
//! `am2_pic_reset_and_start_app_bosminer_faithful(&I2cServiceHandle, u8)`
//! requires an `I2cServiceHandle`. That handle's test-friendly constructor
//! `I2cServiceHandle::for_unit_tests()` is `#[cfg(test)]`-gated inside
//! `dcentrald-hal` and is NOT visible from `dcentrald-asic` integration
//! tests. The wrapper module itself documents this constraint at
//! `bosminer_warmup.rs:445-453` and ships an inline `mod tests`
//! `refuses_eeprom_denylist_addresses` that exercises the predicate
//! `is_eeprom_denylist_addr` directly.
//!
//! This integration test provides the same coverage from the OUTSIDE of
//! the crate (via `build_prelude_transactions()` which is `pub`) and asserts
//! the load-bearing safety invariant: the same canonical prelude bytes are
//! emitted regardless of address — meaning the only thing the runtime
//! entry point can do to distinguish "EEPROM → fail closed" from "dsPIC →
//! emit chain" is the address predicate. If the predicate is bypassed by a
//! future refactor, the SAME prelude bytes would land on an EEPROM and
//! could overwrite hashboard identity data (the 2026-04-29 .74 hb2
//! incident threat surface).
//!
//! Per QA §10 — closes GAP-WARMUP-2 outside the crate boundary.
//!
//! See:
//! -  — load-bearing rule.
//! - `bosminer_warmup.rs:416-432` (inline `refuses_eeprom_denylist_addresses` test).

use dcentrald_asic::dspic::bosminer_warmup::{build_prelude_transactions, parser_flush_bytes};
use dcentrald_hal::i2c::I2cTransactionStep;

#[test]
fn prelude_is_address_independent_predicate_is_load_bearing() {
    // The wrapper builds a fixed list of transactions independent of the
    // target address — the predicate `is_eeprom_denylist_addr(addr)` is
    // therefore the ONLY runtime guard between "emit RESET to dsPIC" and
    // "emit RESET to EEPROM 0x50". If a future refactor moves the address
    // into the transaction bytes (e.g. as part of the prelude), the
    // predicate becomes bypassable — this test fails closed by asserting
    // the prelude carries no address-dependent bytes.
    let txs = build_prelude_transactions();
    let flush = parser_flush_bytes();

    // Flush transaction: exactly the 19 canonical bytes (no per-address
    // variance possible).
    let flush_write = txs[0].iter().find_map(|s| match s {
        I2cTransactionStep::WriteByteByByte(bytes) => Some(bytes.clone()),
        _ => None,
    });
    assert_eq!(
        flush_write.as_deref(),
        Some(flush.as_slice()),
        "flush transaction must contain the canonical address-independent flush payload"
    );

    // RESET transaction: exactly [55 AA 07] — no per-address variance.
    let reset_write = txs[1].iter().find_map(|s| match s {
        I2cTransactionStep::WriteByteByByte(bytes) => Some(bytes.clone()),
        _ => None,
    });
    assert_eq!(
        reset_write.as_deref(),
        Some(&[0x55u8, 0xAA, 0x07][..]),
        "RESET transaction payload must be address-independent"
    );

    // JUMP transaction: exactly [55 AA 06] — no per-address variance.
    let jump_write = txs[2].iter().find_map(|s| match s {
        I2cTransactionStep::WriteByteByByte(bytes) => Some(bytes.clone()),
        _ => None,
    });
    assert_eq!(
        jump_write.as_deref(),
        Some(&[0x55u8, 0xAA, 0x06][..]),
        "JUMP transaction payload must be address-independent"
    );

    // Sanity: the canonical prelude is the same regardless of which target
    // would receive it — proving the predicate is the discriminator.
    // (We can't call the wrapper itself from here because the test handle
    // is gated; the inline `mod tests` covers the predicate directly.)
}

#[test]
fn eeprom_address_range_documented_constants_are_stable() {
    // Pin the PRODUCTION predicate, not a literal copy of it. If a future agent
    // narrows `is_eeprom_denylist_addr` (e.g. to 0x50..=0x53), the .74 hb2 EEPROM-
    // corruption protection regresses for hashboards 4-7 and THIS test fails —
    // which the previous tautological `(0x50..=0x57).contains(addr)` checked
    // against itself could never catch. (gap-swarm HAL-safety #3)
    use dcentrald_asic::dspic::bosminer_warmup::is_eeprom_denylist_addr;

    // Every EEPROM address (one per BHB42xxx hashboard variant) is denied.
    for addr in 0x50u8..=0x57u8 {
        assert!(
            is_eeprom_denylist_addr(addr),
            "address 0x{addr:02X} must remain in the EEPROM write-denylist per \
             "
        );
    }
    // dsPIC chain addresses + the range boundaries must remain OUTSIDE the denylist.
    for addr in [0x20u8, 0x21, 0x22, 0x4F, 0x58] {
        assert!(
            !is_eeprom_denylist_addr(addr),
            "address 0x{addr:02X} must NOT be in the EEPROM denylist"
        );
    }
}
