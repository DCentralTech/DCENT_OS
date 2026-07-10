//! Host-safe integration tests for `pic1704::programmer` (recovery-tool).
//!
//! These tests run on Windows / macOS / Linux without any HAL hardware
//! dependency. They cover the protocol-layer helpers (which build
//! `Vec<I2cTransactionStep>` without touching the bus) and the CLI-token
//! contract. The Service-attached entrypoints (`pic_seek_1704`, etc.)
//! require an `I2cServiceHandle` that itself needs a live `/dev/i2c-N`,
//! so their wire bytes are validated indirectly via the corresponding
//! `*_steps` helpers — same pattern as `tests/pic1704_protocol.rs`.
//!
//! Compiles only when the `recovery-tool` Cargo feature is enabled.
//! Production `dcentrald` builds skip this file entirely.

#![cfg(feature = "recovery-tool")]

use dcentrald_asic::pic1704::programmer::{
    chunked_write_plan, erase_steps, seek_steps, ConfirmedBrickedToken, OP_ERASE, OP_SEEK,
    OP_WRITE, WRITE_CHUNK_BYTES,
};
use dcentrald_asic::pic1704::REG_CONTROL;
use dcentrald_hal::i2c::I2cTransactionStep;

// ===========================================================================
//  seek_steps — "seek_writes_correct_bytes_for_address"
// ===========================================================================

#[test]
fn seek_writes_correct_bytes_for_address() {
    // Canonical address 0x12345678 — LE order: 78 56 34 12.
    let steps = seek_steps(0x12345678);
    assert_eq!(steps.len(), 1, "seek must emit exactly one Write step");
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(
                buf,
                &vec![REG_CONTROL, OP_SEEK, 0x78, 0x56, 0x34, 0x12],
                "seek wire format = [REG_CONTROL, OP_SEEK, addr_le[0..4]]",
            );
        }
        other => panic!("expected Write, got {:?}", other),
    }
}

#[test]
fn seek_zero_address_is_canonical() {
    let steps = seek_steps(0);
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, OP_SEEK, 0, 0, 0, 0]);
        }
        _ => panic!("expected Write"),
    }
}

#[test]
fn seek_max_address_is_all_ff() {
    let steps = seek_steps(u32::MAX);
    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, OP_SEEK, 0xFF, 0xFF, 0xFF, 0xFF]);
        }
        _ => panic!("expected Write"),
    }
}

// ===========================================================================
//  erase_steps — bootloader-only on the runtime side, byte-exact here
// ===========================================================================

#[test]
fn erase_emits_seek_then_erase_atomic() {
    // erase(addr=0x0400, n_pages=4)
    // Expected: Write[REG_CONTROL,OP_SEEK,00,04,00,00], Write[REG_CONTROL,OP_ERASE,4]
    let steps = erase_steps(0x0400, 4);
    assert_eq!(
        steps.len(),
        2,
        "erase must be SEEK + ERASE in one transaction"
    );

    match &steps[0] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf[0], REG_CONTROL);
            assert_eq!(buf[1], OP_SEEK);
            assert_eq!(&buf[2..], &[0x00, 0x04, 0x00, 0x00]);
        }
        _ => panic!("step 0 must be SEEK Write"),
    }
    match &steps[1] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, OP_ERASE, 4]);
        }
        _ => panic!("step 1 must be ERASE Write"),
    }
}

// `erase_refuses_when_version_is_app` is enforced inside
// `pic_erase_1704` via `refuse_if_not_bootloader(svc)`. We can't
// construct a real `Pic1704Service` here without an `I2cServiceHandle`
// (which needs `/dev/i2c-N`), but the unit-test in
// `pic1704::programmer::tests::programmer_ops_require_confirmed_bricked_token`
// locks in the function signature, and the runtime guard is verified
// on-target during `dev_deploy.sh --verify` against the recovery binary.
// The same gate covers seek/write/start-app.

// ===========================================================================
//  write — chunking
// ===========================================================================

#[test]
fn write_chunks_data_correctly_64_byte_boundary() {
    // Exactly one chunk, exactly WRITE_CHUNK_BYTES bytes.
    let data = vec![0xAA; WRITE_CHUNK_BYTES];
    let plan = chunked_write_plan(0x0500, &data);
    assert_eq!(plan.len(), 1);
    let (chunk_addr, steps) = &plan[0];
    assert_eq!(*chunk_addr, 0x0500);
    assert_eq!(steps.len(), 2, "chunk = SEEK + WRITE");
    match &steps[1] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf[0], REG_CONTROL);
            assert_eq!(buf[1], OP_WRITE);
            assert_eq!(&buf[2..], &vec![0xAA; WRITE_CHUNK_BYTES][..]);
        }
        _ => panic!("step 1 must be WRITE"),
    }
}

#[test]
fn write_chunks_partial_tail() {
    // 130 bytes → 64 + 64 + 2.
    let data: Vec<u8> = (0..130u32).map(|i| (i & 0xFF) as u8).collect();
    let plan = chunked_write_plan(0x1000, &data);
    assert_eq!(plan.len(), 3, "130 / 64 = 3 chunks (64+64+2)");

    // Addresses must advance by chunk-size, not the constant 64.
    assert_eq!(plan[0].0, 0x1000);
    assert_eq!(plan[1].0, 0x1000 + WRITE_CHUNK_BYTES as u32);
    assert_eq!(plan[2].0, 0x1000 + 2 * WRITE_CHUNK_BYTES as u32);

    // Last chunk has only 2 payload bytes (128, 129).
    match &plan[2].1[1] {
        I2cTransactionStep::Write(buf) => {
            assert_eq!(buf, &vec![REG_CONTROL, OP_WRITE, 128, 129]);
        }
        _ => panic!("expected Write for last chunk"),
    }
}

#[test]
fn write_chunks_empty_data_yields_empty_plan() {
    // Empty input — caller will be rejected at `pic_write_1704` boundary
    // with InvalidArg before this is ever called, but the helper must
    // not panic on an empty slice.
    let plan = chunked_write_plan(0x2000, &[]);
    assert!(plan.is_empty());
}

// ===========================================================================
//  start_app — wire-byte exactness covered by protocol::tests
// ===========================================================================
//
// The byte order (`[REG_VERSION, BL_MAGIC]` then `[REG_CONTROL, BL_CMD_JUMP]`)
// is exhaustively tested in `pic1704::protocol::tests::start_app_emits_magic_then_jump_in_order`.
// `pic_start_app_common` re-uses `super::protocol::start_app_steps` directly,
// so the byte format cannot drift independently.

#[test]
fn opcodes_are_canonical_bmminer_abi() {
    // Locking in the BraiinsOS bmminer ABI — these opcodes are shared
    // across PIC families (PIC16F1704, dsPIC33EP16GS202, PIC1704). If
    // any constant changes, dspic_flash and pic-recovery's S9 path must
    // change in lockstep.
    assert_eq!(OP_SEEK, 0x01);
    assert_eq!(OP_WRITE, 0x05);
    assert_eq!(OP_ERASE, 0x09);
}

#[test]
fn write_chunk_size_matches_pic1704_staging_buffer() {
    // PIC1704 staging buffer = 64 bytes per the bmminer reference.
    // Bumping this requires (1) live RE evidence on a CV1835/BB/AML
    // bootloader showing a larger window, AND (2) re-checking the
    // kernel I²C MAX_FRAME budget on every host platform.
    assert_eq!(WRITE_CHUNK_BYTES, 64);
}

// ===========================================================================
//  Token gate — programmer_ops_require_confirmed_bricked_token
// ===========================================================================

#[test]
fn token_construction_requires_exact_flag() {
    assert!(ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked").is_ok());
    // Substring / wrong case / typo / missing dashes — all rejected.
    assert!(ConfirmedBrickedToken::new_with_confirmation("").is_err());
    assert!(ConfirmedBrickedToken::new_with_confirmation("confirm-bricked").is_err());
    assert!(ConfirmedBrickedToken::new_with_confirmation("--confirm").is_err());
    assert!(ConfirmedBrickedToken::new_with_confirmation("--Confirm-Bricked").is_err());
    assert!(ConfirmedBrickedToken::new_with_confirmation("--force").is_err());
    assert!(ConfirmedBrickedToken::new_with_confirmation("YES").is_err());
}

#[test]
fn token_is_consumed_by_programmer_ops() {
    // Demonstrates the move-semantics contract: ConfirmedBrickedToken
    // is consumed (moved) on each programmer op call. Because the type
    // intentionally does NOT implement Clone or Copy, the recovery
    // binary must re-mint a fresh token at every op boundary. The
    // unit test in `programmer.rs::tests::programmer_ops_require_confirmed_bricked_token`
    // pins the function signatures so a future refactor that removes
    // the token argument breaks the build there.
    let token = ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked").unwrap();
    fn consume(_t: ConfirmedBrickedToken) {}
    consume(token);
    // `token` is now moved — the next reference would be a borrow-after-move
    // compile error. We rely on the borrow-checker to enforce that.
}
