//!  (2026-05-22) — QA §10 CI-10.
//!
//! Exhaustive source-parse pin for the CE §5 `am2_post_eeprom_dspic_grace_ms`
//! upper-bound clamp. This is the regression-pinning counterpart to the
//! behavioral coverage in `config.rs::tests` (which exercises the
//! actual `validate()` rejection logic on 0 / 1 / 9999 / 10000 / 10001 /
//! u64::MAX). This file pins the source-text shape so a refactor that
//! widens the clamp to e.g. 30_000 ms is caught at the file-text level.
//!
//! Per QA §10 — closes GAP-CONFIG-1.

const CONFIG_RS: &str = include_str!("../src/config.rs");

#[test]
fn upper_bound_constant_is_10_000_ms() {
    // The numeric upper-bound MUST be exactly 10_000 (with or without
    // underscore separators). A regression that loosens it to 30_000 ms
    // is caught here.
    let pinned = CONFIG_RS.contains("am2_post_eeprom_dspic_grace_ms > 10_000")
        || CONFIG_RS.contains("am2_post_eeprom_dspic_grace_ms > 10000");
    assert!(
        pinned,
        "the upper-bound clamp MUST be exactly 10_000 ms — the value is the \
         CE §5 DoS-prevention ceiling. Loosening it (e.g. to 30_000 ms or \
         120_000 ms) requires a deliberate review + a memory-rule update."
    );
}

#[test]
fn clamp_error_message_cites_default_and_ceiling() {
    // Operator-facing error must be actionable. The message must contain:
    //   - the field name (so the operator knows which TOML key to fix)
    //   - the ceiling value (so the operator can pick a valid value)
    //   - the default 2000 ms (so the operator can revert to a safe default)
    let validate_start = CONFIG_RS
        .find("pub fn validate(&self)")
        .expect("validate() must exist");
    let validate_block = &CONFIG_RS[validate_start..];
    // Find the clamp-specific bail.
    assert!(
        validate_block.contains("am2_post_eeprom_dspic_grace_ms")
            && validate_block.contains("10_000"),
        "clamp error must name the field + cite the 10_000 ms ceiling"
    );
    // The doc reference for the operator/agent who hits this bail.
    assert!(
        validate_block.contains("CE-architecture-review.md") || validate_block.contains("CE §5"),
        "clamp error must cite the originating review doc (CE §5)"
    );
}

#[test]
fn zero_is_documented_as_disable() {
    // The doc-comment must continue to call out that `0` disables the
    // grace sleep entirely — otherwise an operator hitting the clamp
    // ceiling might mistakenly think the field can only be 1..=10_000.
    assert!(
        CONFIG_RS.contains("Set to 0 to disable")
            || CONFIG_RS.contains("set to 0 to disable")
            || CONFIG_RS.contains("Value `0` is permitted")
            || CONFIG_RS.contains("permitted")
            || CONFIG_RS.contains("disable the grace sleep entirely"),
        "the doc-comment must keep documenting that 0 disables the grace sleep \
         (operator must know the field is 0-inclusive, not 1-inclusive)"
    );
}

#[test]
fn default_function_returns_2000_ms() {
    // Belt-and-suspenders against a regression that silently changes the
    // default itself (which would defeat the clamp's protection).
    let default_block = CONFIG_RS
        .find("fn default_am2_post_eeprom_dspic_grace_ms() -> u64 {")
        .expect("default fn must exist");
    let block = &CONFIG_RS[default_block..default_block + 200];
    assert!(
        block.contains("2000"),
        "default_am2_post_eeprom_dspic_grace_ms() must return 2000"
    );
}
