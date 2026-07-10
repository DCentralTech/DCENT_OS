//!  B2 (2026-05-22): integration test — `DCENT_AM2_STRICT_SKU_REFUSE`
//! must default OFF.
//!
//! Per the /22 rollout pattern, the first deploy of this binary
//! lands the gate in TELEMETRY-ONLY mode so the operator can confirm
//! `a lab unit` (and `a lab unit` after the dsPIC layer is solved) class clean as
//! BHB42601 with zero false-positive refusal. Only AFTER that
//! confirmation does a follow-up  commit promote the env default
//! to ON.
//!
//! These tests pin that contract — if the default flips silently, this
//! test breaks and the operator gets a chance to review.
//!
//! These tests mutate process-global env vars (`DCENT_AM2_STRICT_SKU_REFUSE`,
//! `DCENT_AM2_ACCEPT_DEGRADED_HARDWARE`). Cargo runs the tests in one binary
//! across MULTIPLE THREADS by default, so without serialization they race on
//! the shared var (e.g. `strict_recognizes_explicit_1` sets "1" while
//! `strict_default_is_off` concurrently removes it → intermittent failure).
//! bug-hunt 2026-05-28: the original "we orchestrate ordering manually" comment
//! described an intent that was never implemented — there was no lock, so the
//! suite was flaky-RED. `ENV_LOCK` now actually serializes every test that
//! touches these vars (the workspace does not depend on the `serial_test`
//! crate, so a hand-rolled `static Mutex` is the no-new-dep fix). Poisoning is
//! ignored (a panicking test should not cascade-fail the rest).

use std::sync::Mutex;

use dcentrald_silicon_profiles::energize_gate::{
    accept_degraded_hardware_enabled, strict_sku_refuse_enabled,
};

/// Serializes every test that mutates the shared process-global env vars, so
/// cargo's parallel test threads cannot race on them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Lock the env mutex, ignoring poison (a prior panicking test must not
/// cascade-fail the rest — the guard's only job is mutual exclusion).
fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[test]
fn strict_default_is_off() {
    let _env = env_guard();
    std::env::remove_var("DCENT_AM2_STRICT_SKU_REFUSE");
    assert!(
        !strict_sku_refuse_enabled(),
        "strict_sku_refuse_enabled must default to false (Wave-23 first-deploy rollout)"
    );
}

#[test]
fn strict_recognizes_explicit_1() {
    let _env = env_guard();
    std::env::set_var("DCENT_AM2_STRICT_SKU_REFUSE", "1");
    assert!(strict_sku_refuse_enabled());
    std::env::remove_var("DCENT_AM2_STRICT_SKU_REFUSE");
}

#[test]
fn strict_recognizes_true_case_insensitive() {
    let _env = env_guard();
    std::env::set_var("DCENT_AM2_STRICT_SKU_REFUSE", "TrUe");
    assert!(strict_sku_refuse_enabled());
    std::env::remove_var("DCENT_AM2_STRICT_SKU_REFUSE");
}

#[test]
fn strict_rejects_other_values() {
    let _env = env_guard();
    // Belt-and-suspenders: prevent accidental "set DCENT_AM2_STRICT_SKU_REFUSE=foo"
    // from silently enabling the strict path.
    std::env::set_var("DCENT_AM2_STRICT_SKU_REFUSE", "yes");
    assert!(!strict_sku_refuse_enabled());
    std::env::set_var("DCENT_AM2_STRICT_SKU_REFUSE", "0");
    assert!(!strict_sku_refuse_enabled());
    std::env::set_var("DCENT_AM2_STRICT_SKU_REFUSE", "");
    assert!(!strict_sku_refuse_enabled());
    std::env::remove_var("DCENT_AM2_STRICT_SKU_REFUSE");
}

#[test]
fn accept_degraded_default_is_off() {
    let _env = env_guard();
    std::env::remove_var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE");
    assert!(
        !accept_degraded_hardware_enabled(),
        "accept_degraded_hardware_enabled must default to false — lab override only"
    );
}

#[test]
fn accept_degraded_recognizes_explicit_1() {
    let _env = env_guard();
    std::env::set_var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE", "1");
    assert!(accept_degraded_hardware_enabled());
    std::env::remove_var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE");
}
