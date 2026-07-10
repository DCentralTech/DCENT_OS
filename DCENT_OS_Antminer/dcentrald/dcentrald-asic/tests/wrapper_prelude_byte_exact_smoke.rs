//!  (2026-05-22) — QA §10 CI-1.
//!
//! Byte-exact regression pin for the bosminer-warmup wrapper's prelude.
//! Asserts the wrapper's `build_prelude_transactions()` returns EXACTLY the
//! canonical bosminer chain:
//!
//!   - 19-byte parser flush bytes `[0x55, 0xAA, 0x00] + 16 × 0x00`
//!   - 3-byte RESET frame `[0x55, 0xAA, 0x07]` + 500 ms dwell
//!   - 3-byte JUMP frame  `[0x55, 0xAA, 0x06]` + 100 ms dwell
//!
//! These are the canonical bytes per `braiins_power.rs:479-505`. A future
//! agent that strips the flush or reorders the chain would corrupt the
//! dsPIC parser state on cold boot (matches the `a lab unit` 2026-04-24
//! corruption pattern); this test fails closed.
//!
//! Per QA §10 — provides an additional integration-tier check separate
//! from the inline `mod tests` already pinned in `bosminer_warmup.rs`.

use dcentrald_asic::dspic::bosminer_warmup::{
    build_prelude_transactions, parser_flush_bytes, JUMP_SETTLE_MS, RESET_DELAY_MS,
};
use dcentrald_hal::i2c::I2cTransactionStep;

#[test]
fn wrapper_prelude_emits_canonical_19_byte_flush_then_reset_then_jump() {
    // Flush — exactly 19 bytes, first three are 0x55 / 0xAA / 0x00.
    let flush = parser_flush_bytes();
    assert_eq!(flush.len(), 19, "flush must be 19 wire bytes");
    assert_eq!(flush[0], 0x55, "flush byte 0 = PIC_COMMAND_1 magic");
    assert_eq!(flush[1], 0xAA, "flush byte 1 = PIC_COMMAND_2 magic");
    assert_eq!(flush[2], 0x00, "flush byte 2 = cmd=0x00 (no-op opcode)");
    for (i, &b) in flush.iter().enumerate().skip(3) {
        assert_eq!(b, 0x00, "flush byte {i} (payload data) must be zero");
    }

    // Three transactions in canonical bosminer order: flush / RESET / JUMP.
    let txs = build_prelude_transactions();
    assert_eq!(
        txs.len(),
        3,
        "bosminer prelude must be exactly 3 transactions"
    );

    // ----- Transaction 0: parser flush -----
    let flush_tx = &txs[0];
    let mut saw_flush_write = false;
    for step in flush_tx {
        match step {
            I2cTransactionStep::WriteByteByByte(bytes) => {
                assert_eq!(
                    bytes.as_slice(),
                    flush.as_slice(),
                    "flush transaction must contain the exact 19-byte flush payload"
                );
                saw_flush_write = true;
            }
            I2cTransactionStep::SetTimeout(_) => {}
            other => panic!("flush transaction has unexpected step: {:?}", other),
        }
    }
    assert!(
        saw_flush_write,
        "flush transaction must contain WriteByteByByte"
    );

    // ----- Transaction 1: RESET + 500 ms dwell -----
    let reset_tx = &txs[1];
    let mut saw_reset_write = false;
    let mut saw_reset_sleep = false;
    for step in reset_tx {
        match step {
            I2cTransactionStep::WriteByteByByte(bytes) => {
                assert_eq!(
                    bytes.as_slice(),
                    &[0x55, 0xAA, 0x07],
                    "RESET frame must be exactly [0x55, 0xAA, 0x07]"
                );
                saw_reset_write = true;
            }
            I2cTransactionStep::SleepMs(ms) => {
                assert_eq!(
                    *ms, RESET_DELAY_MS,
                    "RESET dwell = 500 ms (bosminer canonical)"
                );
                assert_eq!(*ms, 500, "RESET_DELAY_MS constant must equal 500");
                saw_reset_sleep = true;
            }
            I2cTransactionStep::SetTimeout(_) => {}
            other => panic!("RESET transaction has unexpected step: {:?}", other),
        }
    }
    assert!(saw_reset_write, "RESET transaction must write [55 AA 07]");
    assert!(saw_reset_sleep, "RESET transaction must have 500 ms dwell");

    // ----- Transaction 2: JUMP + 100 ms dwell -----
    let jump_tx = &txs[2];
    let mut saw_jump_write = false;
    let mut saw_jump_sleep = false;
    for step in jump_tx {
        match step {
            I2cTransactionStep::WriteByteByByte(bytes) => {
                assert_eq!(
                    bytes.as_slice(),
                    &[0x55, 0xAA, 0x06],
                    "JUMP frame must be exactly [0x55, 0xAA, 0x06]"
                );
                saw_jump_write = true;
            }
            I2cTransactionStep::SleepMs(ms) => {
                assert_eq!(*ms, JUMP_SETTLE_MS, "JUMP dwell = 100 ms (BMMINER_DELAY)");
                assert_eq!(*ms, 100, "JUMP_SETTLE_MS constant must equal 100");
                saw_jump_sleep = true;
            }
            I2cTransactionStep::SetTimeout(_) => {}
            other => panic!("JUMP transaction has unexpected step: {:?}", other),
        }
    }
    assert!(saw_jump_write, "JUMP transaction must write [55 AA 06]");
    assert!(saw_jump_sleep, "JUMP transaction must have 100 ms dwell");

    // Belt-and-suspenders: no Read step anywhere (this is a prelude, not a probe).
    for (idx, tx) in txs.iter().enumerate() {
        for step in tx {
            assert!(
                !matches!(
                    step,
                    I2cTransactionStep::Read(_)
                        | I2cTransactionStep::ReadFrame { .. }
                        | I2cTransactionStep::WriteRead { .. }
                ),
                "transaction {idx} must not contain any read step (this is a prelude, \
                 not a probe — GET_VERSION is the caller's responsibility)"
            );
            // Also: never bulk Write (bosminer canonical = per-byte writes).
            assert!(
                !matches!(step, I2cTransactionStep::Write(_)),
                "transaction {idx} must not contain bulk Write — bosminer transport \
                 is per-byte ioctl(I2C_RDWR) per byte"
            );
        }
    }
}
