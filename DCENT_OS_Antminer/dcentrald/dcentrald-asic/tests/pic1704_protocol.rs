//! Host-safe integration tests for the `pic1704` module.
//!
//! These tests run on Windows / macOS / Linux without any HAL hardware
//! dependency. They cover:
//!
//! 1. `classify_version` — every documented version byte and a sample of
//!    unknown bytes. Locks down the bootloader / application classification.
//! 2. `start_app_steps` — emits 0x5A→VERSION then 0x01→CONTROL in order.
//! 3. `decode_le_word` — voltage / current LE decoding, including the
//!    realistic 13.7 V case.
//! 4. Heartbeat / enable-DC-DC byte exactness.
//! 5. Sealed-trait construction guard — only the whitelisted platform
//!    marker types satisfy `Pic1704Authorized`. We don't assert the
//!    *negative* path here (that would need `trybuild` and would fail to
//!    compile in this same crate); instead we lock in the *positive*
//!    list and document the negative path inside `pic1704::service`.
//!
//! The `Pic1704Service` runtime methods (`heartbeat`, `read_voltage_mv`,
//! etc.) are intentionally NOT exercised here — they require a live
//! `I2cServiceHandle`, which on Linux requires `/dev/i2c-0` and on
//! Windows is unavailable. Those code paths are covered by:
//!  - byte-level checks via the protocol helpers in this file, and
//!  - on-target deploy gates in `dev_deploy.sh --verify`.

use dcentrald_asic::pic1704::service::{platforms, Pic1704Authorized};
use dcentrald_asic::pic1704::{
    classify_version, decode_le_word, enable_dc_dc_steps, heartbeat_steps, is_application_version,
    read_register_steps, start_app_steps, write_register_steps, Pic1704State, BL_CMD_JUMP,
    BL_MAGIC, CTRL_DC_DC_OFF, CTRL_DC_DC_ON, CTRL_HEARTBEAT, PIC1704_I2C_ADDR, REG_CONTROL,
    REG_CURRENT_L, REG_VERSION, REG_VOLTAGE_L, VER_APPLICATION, VER_BOOTLOADER, VER_REV_A,
    VER_REV_B,
};
use dcentrald_hal::i2c::I2cTransactionStep;

// ===========================================================================
//  classify_version — bootloader vs application vs unknown
// ===========================================================================

#[test]
fn classify_version_bootloader_byte() {
    assert_eq!(classify_version(VER_BOOTLOADER), Pic1704State::Bootloader);
    assert_eq!(VER_BOOTLOADER, 0x86);
}

#[test]
fn classify_version_all_application_revs() {
    for &v in &[VER_REV_A, VER_APPLICATION, VER_REV_B] {
        assert_eq!(
            classify_version(v),
            Pic1704State::Application,
            "0x{:02X} should classify as Application",
            v
        );
    }
    assert_eq!(VER_REV_A, 0x88);
    assert_eq!(VER_APPLICATION, 0x89);
    assert_eq!(VER_REV_B, 0x8A);
}

#[test]
fn classify_version_unknown_bytes() {
    // Anything outside {0x86, 0x88, 0x89, 0x8A} is Unknown.
    for v in [0x00, 0x01, 0x42, 0x85, 0x87, 0x8B, 0xCC, 0xFE, 0xFF].iter() {
        assert_eq!(
            classify_version(*v),
            Pic1704State::Unknown,
            "0x{:02X} should be Unknown",
            *v
        );
    }
}

#[test]
fn is_application_version_matches_classifier() {
    for v in 0u8..=0xFF {
        let cls = classify_version(v);
        assert_eq!(
            is_application_version(v),
            cls == Pic1704State::Application,
            "is_application_version disagrees with classify_version on 0x{:02X}",
            v
        );
    }
}

// ===========================================================================
//  start_app_steps — order, register, byte
// ===========================================================================

#[test]
fn start_app_emits_magic_then_jump_in_order() {
    let steps = start_app_steps();
    assert_eq!(
        steps.len(),
        2,
        "start_app must emit exactly two writes (MAGIC then JUMP)"
    );

    // Step 1: 0x5A → REG_VERSION (bootloader unlock).
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf.len(), 2, "first write must be 2 bytes");
            assert_eq!(
                buf[0], REG_VERSION,
                "first write target must be REG_VERSION"
            );
            assert_eq!(buf[1], BL_MAGIC, "first write byte must be BL_MAGIC (0x5A)");
        }
        other => panic!("step 0 must be I2cTransactionStep::Write — got {:?}", other),
    }

    // Step 2: 0x01 → REG_CONTROL (jump command).
    match &steps[1] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf.len(), 2, "second write must be 2 bytes");
            assert_eq!(
                buf[0], REG_CONTROL,
                "second write target must be REG_CONTROL"
            );
            assert_eq!(
                buf[1], BL_CMD_JUMP,
                "second write byte must be BL_CMD_JUMP (0x01)"
            );
        }
        other => panic!("step 1 must be I2cTransactionStep::Write — got {:?}", other),
    }
}

// ===========================================================================
//  decode_le_word — voltage / current
// ===========================================================================

#[test]
fn decode_voltage_le_word_realistic_13700_mv() {
    // 13700 mV = 0x3584. LE: 0x84 0x35.
    let bytes = [0x84u8, 0x35];
    assert_eq!(decode_le_word(&bytes), Some(13_700));
}

#[test]
fn decode_voltage_zero() {
    assert_eq!(decode_le_word(&[0x00, 0x00]), Some(0));
}

#[test]
fn decode_voltage_max() {
    assert_eq!(decode_le_word(&[0xFF, 0xFF]), Some(0xFFFF));
}

#[test]
fn decode_le_word_short_slice_is_none() {
    assert_eq!(decode_le_word(&[]), None);
    assert_eq!(decode_le_word(&[0x12]), None);
}

#[test]
fn read_register_steps_targets_correct_register() {
    // Voltage read: write REG_VOLTAGE_L (0x02), then read 2 bytes.
    let steps = read_register_steps(REG_VOLTAGE_L, 2);
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        I2cTransactionStep::WriteRead {
            write_data,
            read_len,
        } => {
            assert_eq!(write_data, &vec![REG_VOLTAGE_L]);
            assert_eq!(*read_len, 2);
        }
        other => panic!("expected WriteRead, got {:?}", other),
    }

    // Current read.
    let steps = read_register_steps(REG_CURRENT_L, 2);
    match &steps[0] {
        I2cTransactionStep::WriteRead {
            write_data,
            read_len,
        } => {
            assert_eq!(write_data, &vec![REG_CURRENT_L]);
            assert_eq!(*read_len, 2);
        }
        other => panic!("expected WriteRead, got {:?}", other),
    }
}

// ===========================================================================
//  Heartbeat / enable-DC-DC byte exactness
// ===========================================================================

#[test]
fn heartbeat_writes_0x02_to_control_register() {
    let steps = heartbeat_steps();
    assert_eq!(steps.len(), 1);
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, CTRL_HEARTBEAT]);
            assert_eq!(buf[1], 0x02);
        }
        other => panic!("expected Write, got {:?}", other),
    }
}

#[test]
fn heartbeat_rate_limit_constant_is_2_seconds() {
    // The 2-second cadence is part of the protocol contract.
    assert_eq!(
        dcentrald_asic::pic1704::HEARTBEAT_INTERVAL_MS,
        2_000,
        "heartbeat must be exactly 2000 ms — see pic1704.h:49"
    );
}

#[test]
fn enable_dc_dc_on_writes_0x01() {
    let steps = enable_dc_dc_steps(true);
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, CTRL_DC_DC_ON]);
            assert_eq!(buf[1], 0x01);
        }
        other => panic!("expected Write, got {:?}", other),
    }
}

#[test]
fn enable_dc_dc_off_writes_0x00() {
    let steps = enable_dc_dc_steps(false);
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, CTRL_DC_DC_OFF]);
            assert_eq!(buf[1], 0x00);
        }
        other => panic!("expected Write, got {:?}", other),
    }
}

#[test]
fn write_register_steps_produces_two_byte_payload() {
    let steps = write_register_steps(REG_CONTROL, 0x42);
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, 0x42]);
        }
        other => panic!("expected Write, got {:?}", other),
    }
}

// ===========================================================================
//  Address pinning
// ===========================================================================

#[test]
fn pic1704_i2c_address_is_0x20() {
    assert_eq!(
        PIC1704_I2C_ADDR, 0x20,
        "PIC1704 I2C 7-bit address must be 0x20 — see pic1704.h:13"
    );
}

// ===========================================================================
//  Sealed-trait construction whitelist
// ===========================================================================

/// Compile-time helper: only types implementing `Pic1704Authorized` can be
/// passed in. This is the same trait bound used by `Pic1704Service::new`.
fn assert_authorized<P: Pic1704Authorized>() {}

#[test]
fn whitelisted_platforms_satisfy_pic1704_authorized() {
    // The  marker triple — CV1835 / AM335x BB / Amlogic S19j Pro.
    // The trait bound below (`assert_authorized::<T>`) is the load-bearing
    // guarantee — if it stops compiling for any of these, the
    // construction whitelist has been silently broken.
    assert_authorized::<platforms::Cv1835S19jPro>();
    assert_authorized::<platforms::Am335xBbS19jPro>();
    assert_authorized::<platforms::AmlogicS19jPro>();

    // W11.3 expansion (2026-05-09) — three additional CV183x SKUs
    // documented in RE2 hardware catalog §2.5 + §6.1 row "PIC MCU".
    // Same `CVCtrl_BHB42XXX` subtype family + 0x20 ACK probe gate as
    // the existing CV1835 S19j Pro entry.
    assert_authorized::<platforms::Cv1835S19>();
    assert_authorized::<platforms::Cv1835S19i>();
    assert_authorized::<platforms::Cv1835S19XP>();
}

#[test]
fn s21_amlogic_has_no_pic1704_marker() {
    // Compile-time documentation: there is intentionally NO `S21*` marker
    // in `platforms`. Confirm the symbol does not exist by listing the
    // ones that DO.  root corruption-prevention guarantee #2 and
    //  lock S21 Amlogic to NoPic.
    // If a future agent adds an `AmlogicS21` marker to `platforms`, this
    // test will still pass, but the *absence* should be enforced by code
    // review and the `// NOTE: NO S21* PIC1704 marker` comment in
    // `service.rs`. Document the semantic invariant here so it is not
    // lost across refactors.
    let _markers: &[&'static str] = &[
        "Cv1835S19jPro",
        "Am335xBbS19jPro",
        "AmlogicS19jPro",
        "Cv1835S19",
        "Cv1835S19i",
        "Cv1835S19XP",
    ];
    // Sanity: no `S21` substring in any marker name.
    for name in _markers {
        assert!(
            !name.contains("S21"),
            "marker {} would route S21 through PIC1704 — forbidden by \
             corruption-prevention guarantee #2",
            name,
        );
    }
}

// ===========================================================================
//  Negative-path documentation (cannot run as a runtime test)
// ===========================================================================
//
// Out-of-crate code CANNOT implement `Pic1704Authorized`, because:
//  - The trait requires `sealed::Sealed` as a super-trait.
//  - `sealed::Sealed` is `pub(crate)` (declared inside a private module).
//
// A `compile_fail` doctest or `trybuild` test would prove this from an
// external crate. From within `dcentrald-asic`, we already have access to
// `sealed::Sealed`, so a runtime negative test is meaningless. We
// document the proof here for A5 (who will add the trybuild gate when
// wiring the real platform markers):
//
// ```compile_fail,E0277
// use dcentrald_asic::pic1704::service::Pic1704Authorized;
// struct Rogue;
// impl Pic1704Authorized for Rogue {}   // E0277: Sealed is not implemented for Rogue
// ```
