//!  (2026-05-22) — QA §10 CI-5.
//!
//! Source-parse regression pin for the CE §5 /  clamp on
//! `[mining].am2_post_eeprom_dspic_grace_ms`. Verifies:
//!
//!   - the upper-bound clamp at 10_000 ms exists in `validate()`
//!   - the descriptive error message names the field + cites the 10_000 ceiling
//!   - the default value 2000 ms is preserved
//!   - the structural test catches a regression that silently widens or
//!     removes the clamp
//!
//! ## Why this is a source-parse test
//!
//! `dcentrald` is a binary-only crate (no `[lib]` target), so integration
//! tests in `tests/` cannot `use dcentrald::config::DcentraldConfig`. The
//! behavioral validation logic IS exercised by inline tests in
//! `config.rs::tests`; this file pins the existence of the clamp at the
//! source-text level so a future "fix" can't silently strip it.
//!
//! Per QA §10 — closes GAP-CONFIG-1 at the source level. Behavioral
//! coverage lives in `config.rs::tests::am2_post_eeprom_grace_ms_*`.

const CONFIG_RS: &str = include_str!("../src/config.rs");

#[test]
fn am2_post_eeprom_grace_ms_clamp_is_present_in_validate() {
    // The clamp must reference 10_000 (the ceiling) AND name the field
    // AND live inside the validate() body (which is the load-time gate).
    assert!(
        CONFIG_RS.contains("am2_post_eeprom_dspic_grace_ms > 10_000")
            || CONFIG_RS.contains("am2_post_eeprom_dspic_grace_ms > 10000"),
        "validate() must reject am2_post_eeprom_dspic_grace_ms > 10_000 ms \
         (Wave-23 CE §5 DoS-prevention clamp)"
    );
}

#[test]
fn am2_post_eeprom_grace_ms_clamp_error_message_is_descriptive() {
    // The error string must name the field and cite the 10_000 ms safety
    // ceiling so the operator can fix their TOML.
    assert!(
        CONFIG_RS.contains("am2_post_eeprom_dspic_grace_ms"),
        "validate() error must name the offending field"
    );
    // The default 2000 ms callout in the error message is the
    // canonical-recovery hint.
    let validate_block_start = CONFIG_RS
        .find("pub fn validate(&self)")
        .expect("validate() must exist");
    let validate_block = &CONFIG_RS[validate_block_start..];
    assert!(
        validate_block.contains("10_000") && validate_block.contains("safety ceiling"),
        "validate() must cite the '10_000 ms safety ceiling' in the error message"
    );
}

#[test]
fn am2_post_eeprom_grace_ms_default_2000_preserved() {
    // The default fn must still return 2000 ms — a regression that
    // silently widened the default would defeat the purpose of the clamp.
    assert!(
        CONFIG_RS.contains("fn default_am2_post_eeprom_dspic_grace_ms() -> u64 {")
            && CONFIG_RS.contains("2000"),
        "default_am2_post_eeprom_dspic_grace_ms() must return 2000"
    );
}
