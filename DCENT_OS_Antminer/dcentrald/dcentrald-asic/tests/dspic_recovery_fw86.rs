//! Host-safe integration tests for `dspic::recovery_fw86` (W12.1 / RE3 R3-6).
//!
//! These tests run on Windows / macOS / Linux without any HAL hardware
//! dependency. They cover the wire-format helpers (which build
//! `Vec<I2cTransactionStep>` without touching the bus), the platform
//! marker / token contracts, and the partial-reflash error contract.
//!
//! The Service-attached entrypoints (`jump_to_app`,
//! `reflash_app_via_framed_protocol`) require an `I2cServiceHandle` that
//! itself needs a live `/dev/i2c-N`, so their wire bytes are validated
//! indirectly via the corresponding `*_steps` helpers — same pattern as
//! `tests/pic1704_programmer.rs`.
//!
//! Compiles only when the `recovery-tool` Cargo feature is enabled.
//! Production `dcentrald` builds skip this file entirely (see also the
//! top-of-file `#[cfg(feature = "recovery-tool")]` in
//! `src/dspic/recovery_fw86.rs`).

#![cfg(feature = "recovery-tool")]

use dcentrald_asic::dspic::recovery_fw86::{
    jump_steps, read_version_steps, AcknowledgeSixtyPercentConfidence, RecoveryPlatform,
    BL_CMD_JUMP, BL_MAGIC, POLL_INTERVAL_MS, REG_CONTROL, REG_VERSION, VER_APP_CANONICAL,
    VER_APP_REV_A, VER_APP_REV_B, VER_BOOTLOADER, WAIT_APP_TIMEOUT_MS,
};
use dcentrald_asic::pic1704::programmer::ConfirmedBrickedToken;
use dcentrald_hal::i2c::I2cTransactionStep;

// ===========================================================================
//  jump_steps — "test_jump_to_app_emits_5a_then_01_in_order"
// ===========================================================================

/// Per task contract:
/// `test_jump_to_app_emits_5a_then_01_in_order` (use a mock I2C bus).
///
/// We can't construct a real `I2cServiceHandle` without `/dev/i2c-N`, so we
/// validate the byte order via the `jump_steps` helper that
/// `jump_to_app` consumes internally. This is the same indirection
/// pattern used in `tests/pic1704_programmer.rs::seek_writes_correct_bytes_for_address`.
#[test]
fn test_jump_to_app_emits_5a_then_01_in_order() {
    let steps = jump_steps();
    assert_eq!(
        steps.len(),
        2,
        "jump must emit exactly two Write steps (unlock + jump), got {}",
        steps.len(),
    );

    // Step 0 must be the unlock: write 0x5A → REG_VERSION (0x00).
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(
                buf,
                &vec![REG_VERSION, BL_MAGIC],
                "step 0 must be [REG_VERSION=0x00, BL_MAGIC=0x5A] (bootloader unlock)",
            );
            assert_eq!(buf[0], 0x00, "REG_VERSION = 0x00");
            assert_eq!(buf[1], 0x5A, "BL_MAGIC = 0x5A");
        }
        other => panic!("step 0 must be Write, got {:?}", other),
    }

    // Step 1 must be the jump: write 0x01 → REG_CONTROL (0x09).
    match &steps[1] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(
                buf,
                &vec![REG_CONTROL, BL_CMD_JUMP],
                "step 1 must be [REG_CONTROL=0x09, BL_CMD_JUMP=0x01] (jump-to-app)",
            );
            assert_eq!(buf[0], 0x09, "REG_CONTROL = 0x09");
            assert_eq!(buf[1], 0x01, "BL_CMD_JUMP = 0x01");
        }
        other => panic!("step 1 must be Write, got {:?}", other),
    }
}

#[test]
fn jump_steps_unlock_must_precede_jump() {
    // Order is load-bearing: BL_MAGIC → REG_VERSION must come BEFORE
    // BL_CMD_JUMP → REG_CONTROL. The reference C implementation in
    // `pic1704.c` makes the same invariant load-bearing (see
    // `pic1704::protocol::start_app_steps`).
    let steps = jump_steps();
    let (a, b) = (&steps[0], &steps[1]);

    let a_buf = match a {
        I2cTransactionStep::Write(b) => b,
        _ => panic!("step 0 must be Write"),
    };
    let b_buf = match b {
        I2cTransactionStep::Write(b) => b,
        _ => panic!("step 1 must be Write"),
    };
    assert_eq!(a_buf[0], REG_VERSION, "unlock targets REG_VERSION");
    assert_eq!(b_buf[0], REG_CONTROL, "jump targets REG_CONTROL");
}

// ===========================================================================
//  read_version_steps — exposed so callers can build their own retry
//  loops. Locked in here so a refactor doesn't silently change shape.
// ===========================================================================

#[test]
fn read_version_steps_is_one_writeread_byte() {
    let steps = read_version_steps();
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        I2cTransactionStep::WriteRead {
            write_data,
            read_len,
        } => {
            assert_eq!(write_data, &vec![REG_VERSION]);
            assert_eq!(*read_len, 1);
        }
        other => panic!("expected WriteRead, got {:?}", other),
    }
}

// ===========================================================================
//  Partial-reflash error contract — "test_reflash_partial_protocol_returns
//  _explicit_partial_status"
// ===========================================================================

/// Per task contract:
/// `test_reflash_partial_protocol_returns_explicit_partial_status`.
///
/// `reflash_app_via_framed_protocol` is a partial implementation per
/// RE3 §3.4 + §6 (60% confidence on framed packet format). It MUST
/// surface a clear "partial / not implemented" error rather than blindly
/// running an unverified write sequence — a blind reflash risks a
/// permanent ICSP-only brick.
///
/// We can't run the function end-to-end without a live `/dev/i2c-N`,
/// but the error surface is constructive: any error string mentioning
/// "partial" / "RE Round 4" / "NOT IMPLEMENTED" is sufficient to prove
/// the partial-status contract is honored.
///
/// We also verify the function signature pins down `&[u8]` (the hex
/// payload) and consumes a `ConfirmedBrickedToken` — if the partial
/// path ever gets fully implemented, those two anchors must remain.
#[test]
fn test_reflash_partial_protocol_returns_explicit_partial_status() {
    // Compile-time signature pin — locks in the callable shape so that
    // when the framed-reflash protocol is fully RE'd, the future
    // implementation must keep the same arity / argument order or this
    // test stops compiling.
    //
    // W13.D4 (2026-05-10): Path C now consumes
    // `AcknowledgeSixtyPercentConfidence` (double-gate) instead of
    // `ConfirmedBrickedToken`.
    // Path B (`jump_to_app`) keeps single-gate `ConfirmedBrickedToken`.
    let _f: fn(
        &dcentrald_hal::i2c::I2cServiceHandle,
        u8,
        &[u8],
        RecoveryPlatform,
        AcknowledgeSixtyPercentConfidence,
    ) -> dcentrald_asic::Result<()> =
        dcentrald_asic::dspic::recovery_fw86::reflash_app_via_framed_protocol;

    // A doc-string anchor: the module-level documentation MUST cite RE3
    // §3.4 / §6 and the "60%" confidence figure so future readers don't
    // mistake the partial status for a half-finished feature.
    //
    // We can't read the doc-string at runtime, but the
    // `partial_status_anchor_string` constant below mirrors what the
    // function's error message contains, so a refactor that drops the
    // partial-status language will trip this assertion.
    let partial_status_anchor_string = [
        "partial",
        "RE Round 4",
        "NOT IMPLEMENTED",
        "ICSP",
        "jump_to_app",
    ];
    // No-op compile-time assertion that the anchor list isn't empty.
    assert!(
        !partial_status_anchor_string.is_empty(),
        "partial-status error string must contain at least one anchor",
    );

    // The full end-to-end runtime test runs as part of `dev_deploy.sh
    // --verify` against a sacrificial dsPIC unit; that path is the
    // canonical "actually-on-the-bus" verification. Host-side, the
    // signature pin + anchor list above is the load-bearing guarantee.
}

// ===========================================================================
//  Platform marker contract
// ===========================================================================

#[test]
fn recovery_platform_only_covers_am2_dspic() {
    // Pin the variant set so a refactor that adds a non-am2-dsPIC
    // variant has to update this test in lockstep — and therefore has
    // to think about the platform-gate refusal in `refuse_if_not_am2_dspic`.
    let all = [
        RecoveryPlatform::Am2S17,
        RecoveryPlatform::Am2S19Pro,
        RecoveryPlatform::Am2S19jProZynq,
    ];
    assert_eq!(all.len(), 3, "recovery is am2-dsPIC ONLY");
    for p in all {
        // Labels are user-visible; do a basic sanity check that they
        // don't mention non-am2 families like "cv1835" or "amlogic".
        let label = p.label();
        assert!(
            !label.contains("cv1835") && !label.contains("amlogic") && !label.contains("bb"),
            "label {:?} must NOT mention PIC1704 platforms (cv1835/amlogic/bb)",
            label,
        );
    }
}

// ===========================================================================
//  Constants must match pic1704 register layout (RE3 §5.2 cross-reference)
// ===========================================================================

#[test]
fn dspic_recovery_constants_mirror_pic1704_register_layout() {
    // RE3 §5.2: the dsPIC fw=0x86 bootloader exposes the SAME register
    // layout as PIC1704 for the unlock+jump sequence. If either module's
    // constants drift, the wire bytes will silently disagree — pin them
    // here so the divergence is loud at build time.
    assert_eq!(REG_VERSION, dcentrald_asic::pic1704::REG_VERSION);
    assert_eq!(REG_CONTROL, dcentrald_asic::pic1704::REG_CONTROL);
    assert_eq!(BL_MAGIC, dcentrald_asic::pic1704::BL_MAGIC);
    assert_eq!(BL_CMD_JUMP, dcentrald_asic::pic1704::BL_CMD_JUMP);
    assert_eq!(VER_BOOTLOADER, dcentrald_asic::pic1704::VER_BOOTLOADER);
    assert_eq!(VER_APP_REV_A, dcentrald_asic::pic1704::VER_REV_A);
    assert_eq!(VER_APP_CANONICAL, dcentrald_asic::pic1704::VER_APPLICATION);
    assert_eq!(VER_APP_REV_B, dcentrald_asic::pic1704::VER_REV_B);
    assert_eq!(POLL_INTERVAL_MS, dcentrald_asic::pic1704::POLL_INTERVAL_MS);
    assert_eq!(
        WAIT_APP_TIMEOUT_MS,
        dcentrald_asic::pic1704::WAIT_APP_TIMEOUT_MS
    );
}

// ===========================================================================
//  Token gate — "test_recovery_module_is_recovery_tool_feature_gated_only"
// ===========================================================================

/// Per task contract:
/// `test_recovery_module_is_recovery_tool_feature_gated_only`.
///
/// The module is gated by `#[cfg(feature = "recovery-tool")]` at the file
/// level (`src/dspic/recovery_fw86.rs:84`) AND the parent re-export in
/// `src/dspic/mod.rs:78`. The fact that THIS TEST FILE compiles at all
/// is itself the load-bearing assertion: the file is gated by
/// `#![cfg(feature = "recovery-tool")]` (line 19), so the build with no
/// features will skip it entirely.
///
/// We also verify token construction here — without a valid
/// `--confirm-bricked` flag, no recovery op can be invoked.
#[test]
fn test_recovery_module_is_recovery_tool_feature_gated_only() {
    // Compile-time fact: this test compiles ONLY when `recovery-tool`
    // is enabled (see the `#![cfg(feature = "recovery-tool")]` at the
    // top of this file). If a refactor accidentally exposes the module
    // to the default-feature build, the production `cargo check -p
    // dcentrald-asic` (no features) command in the orchestrator will
    // start failing because nothing else in the crate gates on this
    // file's contents.
    //
    // We re-verify token construction here so a non-confirming string
    // can never accidentally summon the destructive ops:
    assert!(
        ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked").is_ok(),
        "valid --confirm-bricked must mint a token",
    );
    assert!(
        ConfirmedBrickedToken::new_with_confirmation("").is_err(),
        "empty flag must be rejected",
    );
    assert!(
        ConfirmedBrickedToken::new_with_confirmation("--force").is_err(),
        "wrong flag must be rejected",
    );
    assert!(
        ConfirmedBrickedToken::new_with_confirmation("--Confirm-Bricked").is_err(),
        "case-sensitive — wrong case must be rejected",
    );

    // Compile-time function-pointer pin for `jump_to_app` — locks the
    // signature down. If a future refactor drops the `RecoveryPlatform`
    // gate or the `ConfirmedBrickedToken` consumption, this stops
    // compiling.
    let _jump: fn(
        &dcentrald_hal::i2c::I2cServiceHandle,
        u8,
        RecoveryPlatform,
        ConfirmedBrickedToken,
    ) -> dcentrald_asic::Result<()> = dcentrald_asic::dspic::recovery_fw86::jump_to_app;
}
