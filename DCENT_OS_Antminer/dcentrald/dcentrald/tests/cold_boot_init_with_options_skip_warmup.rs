//!  (2026-05-22) — QA §10 CI-3 / closes GAP-DSPIC-1.
//!
//! Structural source-parse pin for the `skip_warmup_loop=true` branch of
//! `dcentrald_asic::dspic::DspicService::cold_boot_init_with_options`.
//! The branch:
//!
//!   1. SKIPS the 5×1Hz pre-voltage heartbeat loop (`for tick in 1..=5`).
//!   2. Issues ONE sanity heartbeat before SetVoltage.
//!   3. On sanity-heartbeat failure, returns `AsicError::Pic` with detail
//!      "dsPIC sanity heartbeat after external warmup failed".
//!
//! ## Why this is a structural test
//!
//! The `cold_boot_init_with_options` function is in `dcentrald-asic` and
//! its live execution path requires a real `I2cServiceHandle` (channel
//! into a dedicated I²C-service worker that opens `/dev/i2c-0`). The
//! `for_unit_tests` test handle is `#[cfg(test)]`-gated inside
//! `dcentrald-hal` and not visible from `dcentrald-asic` integration
//! tests. The live evidence on `a lab unit` (2026-05-22 Phase 3 `cold_boot_init
//! OK target_mv=13700`) is the runtime proof; this source-parse pin
//! catches a regression that would silently flip `skip_warmup_loop` back
//! to running the legacy 5-tick loop without changing the public API
//! signature.
//!
//! Per QA §10 — closes GAP-DSPIC-1 at the source level.

const DSPIC_MOD_RS: &str = include_str!("../../dcentrald-asic/src/dspic/mod.rs");

#[test]
fn cold_boot_init_with_options_skip_warmup_branch_is_present_and_correct() {
    // The function MUST take `skip_warmup_loop: bool`.
    assert!(
        DSPIC_MOD_RS.contains("skip_warmup_loop: bool"),
        "cold_boot_init_with_options must accept `skip_warmup_loop: bool`"
    );

    // The skip branch must exist and short-circuit the 5-tick loop.
    assert!(
        DSPIC_MOD_RS.contains("if skip_warmup_loop {"),
        "must branch on `skip_warmup_loop`"
    );

    // The exact failure-detail string the spec calls out — used by the
    // hybrid path's error-propagation message format. A future refactor
    // that changes this string must be deliberate (this test pins the
    // operator-facing log/error wording).
    assert!(
        DSPIC_MOD_RS.contains("dsPIC sanity heartbeat after external warmup failed"),
        "skip_warmup_loop branch must return AsicError::Pic with the canonical \
         'sanity heartbeat after external warmup failed' detail message"
    );

    // The legacy branch's 5-tick loop must STILL be present (the
    // legacy path is preserved byte-for-byte when `skip_warmup_loop=false`).
    assert!(
        DSPIC_MOD_RS.contains("for tick in 1..=5"),
        "legacy 5-tick pre-voltage warmup loop must still exist for \
         skip_warmup_loop=false (legacy callers byte-identical)"
    );

    // The thin `cold_boot_init` trampoline must still call
    // `cold_boot_init_with_options(voltage_mv, false)` so existing call
    // sites preserve byte-identical behaviour.
    assert!(
        DSPIC_MOD_RS.contains("self.cold_boot_init_with_options(voltage_mv, false)"),
        "cold_boot_init must trampoline to cold_boot_init_with_options(_, false) \
         to preserve byte-identical legacy semantics on non-Wave-22 callers"
    );
}

#[test]
fn skip_warmup_branch_issues_single_sanity_heartbeat() {
    // The skip branch's single sanity heartbeat lives inside the
    // `if skip_warmup_loop { ... }` block. Extract that block's body and
    // assert it contains exactly ONE `self.send_heartbeat()` call (not
    // five, not zero — otherwise the contract documented in the function
    // doc-comment is violated).
    let body = DSPIC_MOD_RS;
    let if_start = body
        .find("if skip_warmup_loop {")
        .expect("skip_warmup_loop branch must be present");
    // Slice from the `if` to the matching `} else {` (the legacy 5-tick
    // loop is the else branch).
    let after_if = &body[if_start..];
    let else_pos = after_if
        .find("} else {")
        .expect("skip_warmup_loop branch must have a legacy `else` branch");
    let skip_block = &after_if[..else_pos];
    // Count only ACTUAL calls, not the literal string as it appears inside the
    // branch's own `//` explanatory comment. The  doc-comment that
    // documents this very "exactly one heartbeat" contract mentions
    // `self.send_heartbeat()` in prose, so a bare string-count sees 2. Strip
    // comment lines first so the test pins the real call count, not the
    // documentation. (2026-06-07: the bare-string count was a false positive —
    // the code is correct with one retry-loop call; the comment inflated it.)
    let heartbeat_count: usize = skip_block
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .map(|l| l.matches("self.send_heartbeat()").count())
        .sum();
    assert_eq!(
        heartbeat_count, 1,
        "skip_warmup_loop=true must issue exactly ONE sanity heartbeat before \
         SetVoltage (got {heartbeat_count} real calls in branch body, comments excluded)"
    );
}
