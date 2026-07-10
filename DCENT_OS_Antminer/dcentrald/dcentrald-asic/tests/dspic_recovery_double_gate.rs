//! Host-safe integration tests for W13.D4 — Path B / Path C CLI split.
//!
//! (W13.A3, 2026-05-10):
//! Path B (`jump_to_app`) is 100% byte-exact per RE3 §5.2 and keeps the
//! single-flag UX (`--confirm-bricked` only). Path C
//! (`reflash_app_via_framed_protocol`) is 60% byte-exact per RE3 §3.4 +
//! §6 confidence table and now requires:
//!
//! 1. `--confirm-bricked` (canonical PIC1704 token gate)
//! 2. `--i-acknowledge-60-percent-byte-exact-confidence` (double-gate flag)
//! 3. Typed-serial confirmation matching the connected dsPIC's
//!    hashboard EEPROM serial (target-unit confirmation)
//!
//! These four named tests mirror the W13.D4 task contract:
//!   - jump_to_app_accepts_single_confirm_bricked
//!   - reflash_fw86_requires_double_flag
//!   - reflash_fw86_serial_mismatch_aborts
//!   - acknowledge_token_mintable_only_with_both_flags
//!
//! Compiles only when the `recovery-tool` Cargo feature is enabled.
//! Production `dcentrald` builds skip this file entirely (mirrors
//! `tests/dspic_recovery_fw86.rs`).

#![cfg(feature = "recovery-tool")]

use dcentrald_asic::dspic::recovery_fw86::{
    append_path_c_invocation_log, parse_serial_bytes, path_c_log_path,
    reflash_app_via_framed_protocol, AcknowledgeSixtyPercentConfidence, RecoveryPlatform,
    DEFAULT_PATH_C_LOG_DIR, HASHBOARD_EEPROM_I2C_ADDR, HASHBOARD_SERIAL_LEN, PATH_C_LOG_FILENAME,
};
use dcentrald_asic::pic1704::programmer::ConfirmedBrickedToken;
use dcentrald_hal::i2c::I2cServiceHandle;

// ===========================================================================
//  Test contract 1: jump_to_app_accepts_single_confirm_bricked
// ===========================================================================

#[test]
fn jump_to_app_accepts_single_confirm_bricked() {
    // Path B (jump-only) must keep single-flag UX — single
    // `--confirm-bricked` mints the canonical token.
    let tok = ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked");
    assert!(
        tok.is_ok(),
        "Path B (jump-only) must accept single --confirm-bricked",
    );

    // Compile-time signature pin: jump_to_app takes
    // `ConfirmedBrickedToken`, NOT the Path C double-gate token. If a
    // future refactor ever promotes Path B to the double-gate (which
    // would be a UX regression), this stops compiling.
    let _f: fn(
        &I2cServiceHandle,
        u8,
        RecoveryPlatform,
        ConfirmedBrickedToken,
    ) -> dcentrald_asic::Result<()> = dcentrald_asic::dspic::recovery_fw86::jump_to_app;
}

// ===========================================================================
//  Test contract 2: reflash_fw86_requires_double_flag
// ===========================================================================

#[test]
fn reflash_fw86_requires_double_flag() {
    // Single --confirm-bricked is NOT enough — Path C requires the
    // additional --i-acknowledge-60-percent-byte-exact-confidence.
    let r = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
        "--confirm-bricked",
        "",
        "ABC123",
        "ABC123",
    );
    assert!(r.is_err(), "missing --i-acknowledge flag must refuse mint",);

    // Wrong second flag is also rejected.
    let r2 = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
        "--confirm-bricked",
        "--i-acknowledge-50-percent-byte-exact-confidence",
        "ABC123",
        "ABC123",
    );
    assert!(r2.is_err(), "wrong second flag must refuse mint");

    // Missing first flag is also rejected.
    let r3 = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
        "",
        "--i-acknowledge-60-percent-byte-exact-confidence",
        "ABC123",
        "ABC123",
    );
    assert!(r3.is_err(), "missing --confirm-bricked must refuse mint");
}

// ===========================================================================
//  Test contract 3: reflash_fw86_serial_mismatch_aborts
// ===========================================================================

#[test]
fn reflash_fw86_serial_mismatch_aborts() {
    // Both flags present, but typed serial does not match connected
    // dsPIC's hashboard EEPROM serial → must refuse.
    let r = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
        "--confirm-bricked",
        "--i-acknowledge-60-percent-byte-exact-confidence",
        "WRONG_BOARD_SERIAL",
        "RIGHT_BOARD_SERIAL",
    );
    assert!(r.is_err(), "serial mismatch must abort token mint");
    let err = r.unwrap_err().to_string();
    assert!(
        err.contains("does not match")
            || err.contains("wrong unit")
            || err.contains("WRONG_BOARD_SERIAL"),
        "error must explain serial-mismatch, got {:?}",
        err,
    );

    // Empty typed-serial is also refused (silent empty-string is not a
    // confirmation — operator must actually type something).
    let r2 = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
        "--confirm-bricked",
        "--i-acknowledge-60-percent-byte-exact-confidence",
        "",
        "ABC123",
    );
    assert!(r2.is_err(), "empty typed serial must be refused");
}

// ===========================================================================
//  Test contract 4: acknowledge_token_mintable_only_with_both_flags
// ===========================================================================

#[test]
fn acknowledge_token_mintable_only_with_both_flags() {
    // Happy path — both flags present + matching serial → token mints
    // and carries the operator-confirmed serial through.
    let tok = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
        "--confirm-bricked",
        "--i-acknowledge-60-percent-byte-exact-confidence",
        "ABC123",
        "ABC123",
    )
    .expect("happy path with both flags + matching serial must mint");
    assert_eq!(
        tok.confirmed_serial(),
        "ABC123",
        "minted token must carry operator-confirmed serial through",
    );

    // Compile-time signature pin: reflash_app_via_framed_protocol now
    // takes the Path C double-gate token, not ConfirmedBrickedToken.
    let _f: fn(
        &I2cServiceHandle,
        u8,
        &[u8],
        RecoveryPlatform,
        AcknowledgeSixtyPercentConfidence,
    ) -> dcentrald_asic::Result<()> = reflash_app_via_framed_protocol;
}

// ===========================================================================
//  Auxiliary: serial parser & log helpers (host-safe coverage)
// ===========================================================================

#[test]
fn hashboard_eeprom_constants_match_at24c02_layout() {
    // The hashboard EEPROM (AT24C02) sits at 0x51 on am2 — same address
    // the daemon's hardware-info gather path reads. Pin the constants
    // so a future refactor doesn't accidentally retarget Path C
    // confirmation reads at a different I²C device.
    assert_eq!(HASHBOARD_EEPROM_I2C_ADDR, 0x51);
    assert_eq!(HASHBOARD_SERIAL_LEN, 16);
}

#[test]
fn parse_serial_bytes_strips_eeprom_padding() {
    // AT24C02 returns whatever was last programmed; trailing pads are
    // typically NULs or spaces.
    assert_eq!(parse_serial_bytes(b"BHB42801ABC123\0\0"), "BHB42801ABC123");
    assert_eq!(parse_serial_bytes(b"BHB42801ABC123  "), "BHB42801ABC123");
    assert_eq!(parse_serial_bytes(b""), "");
}

#[test]
fn path_c_log_path_default_is_var_log_dcent() {
    // Make absolutely sure no test left the env var set; this lives at
    // process scope so other tests in the same binary could leak it.
    std::env::remove_var("DCENT_PIC_RECOVERY_LOG_DIR");
    let p = path_c_log_path();
    assert_eq!(
        p,
        std::path::PathBuf::from(DEFAULT_PATH_C_LOG_DIR).join(PATH_C_LOG_FILENAME),
    );
}

#[test]
fn append_path_c_invocation_log_redirects_via_env_var() {
    // Path C invocations must persist to disk for forensic audit. Test
    // exercises the env-redirect path so we never write to /var/log
    // from `cargo test`.
    let dir = std::env::temp_dir().join(format!(
        "dcent_recovery_path_c_int_{}_{}",
        std::process::id(),
        // Salt with a unique-per-test suffix so parallel test runners
        // don't collide. The other host-test in `recovery_fw86.rs` uses
        // a different suffix.
        "int_test",
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("DCENT_PIC_RECOVERY_LOG_DIR", &dir);

    append_path_c_invocation_log(
        0x21,
        RecoveryPlatform::Am2S17,
        "BHB42801XYZ",
        "partial_bail_60pct",
    );
    append_path_c_invocation_log(
        0x21,
        RecoveryPlatform::Am2S17,
        "BHB42801XYZ",
        "pre_flight_refused_app_running",
    );

    let log = std::fs::read_to_string(dir.join(PATH_C_LOG_FILENAME))
        .expect("log file must exist after appends");
    assert_eq!(
        log.lines().count(),
        2,
        "two appends must produce two log lines, got {:?}",
        log,
    );
    assert!(log.contains("partial_bail_60pct"));
    assert!(log.contains("pre_flight_refused_app_running"));
    assert!(log.contains("serial=BHB42801XYZ"));
    assert!(log.contains("platform=am2-s17"));

    std::env::remove_var("DCENT_PIC_RECOVERY_LOG_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}
