//! Bosminer-faithful pre-GET_VERSION dsPIC cold-boot prelude (Layer 1).
//!
//! Replays the byte-exact wire chain that
//! `bosminer-am2-s17::power::Control::reset_and_start_app` emits on cold boot
//! of a healthy fw=0x89 chip. Live-evidence on `a lab unit` (2026-05-22 BraiinsOS+
//! firmware=1 mtd7 slot): `PWR/1: Voltage controller reset → application
//! started → firmware version 0x89` — proven to work on the exact hardware
//! DCENT_OS currently cannot version.
//!
//! ### Why this wrapper is safe by construction
//!
//! The load-bearing memory rule [] is NARROWER
//! than its name suggests. It bans the **bare 3-byte form** `[55 AA 07]` on
//! fw=0x89 PRODUCTION-RUNTIME paths — the `a lab unit` 2026-04-24 corruption that
//! motivated the rule was the historical `dspic_flash::reset_pic` path going
//! straight to RESET without the canonical 16-byte parser flush prelude.
//!
//! This wrapper **always** emits the bosminer-canonical sequence:
//!
//! ```text
//! Step A.1: PARSER FLUSH  — [0x55, 0xAA, 0x00] + 16 × 0x00     (19 wire bytes)
//! Step A.2: RESET         — [0x55, 0xAA, 0x07]  + 500 ms dwell  ( 3 wire bytes)
//! Step D:   JUMP_TO_APP   — [0x55, 0xAA, 0x06]  + 100 ms dwell  ( 3 wire bytes)
//! ```
//!
//! The 16-zero flush guarantees the dsPIC MSSP parser FSM is at idle when the
//! RESET opcode lands (per `braiins_power.rs:481-495` source comment); RESET
//! is then deterministic, not a coin-flip. The `a lab unit` corruption mechanism
//! (bare RESET to a chip in unknown parser state) is **structurally
//! impossible** here — the wrapper emits the flush in the same call, and the
//! type signature gives no way to skip it.
//!
//! Bosminer-source provenance (GPL-3.0 BraiinsOS-Antminer at
//! ):
//!
//! - `braiins_power.rs:479-498` — `reset()` body (16-byte flush + 0x07 + 500 ms).
//! - `braiins_power.rs:500-505` — `jump_from_loader_to_app()` (0x06 + 100 ms).
//! - `braiins_power.rs:391` — `RESET_DELAY = 500 ms`.
//! - `braiins_power.rs:395` — `BMMINER_DELAY = 100 ms`.
//! - `braiins_power.rs:177-183` — per-byte writes through `I2cBackend::write`.
//!
//! ### Safety invariants enforced
//!
//! - **Per-byte writes** via `I2cTransactionStep::WriteByteByByte` — matches
//!   the bosminer transport (one ioctl(I2C_RDWR) message per byte).
//! - **EEPROM 0x50-0x57 denylist** — this wrapper refuses to target any
//!   address in `0x50..=0x57`. The actual HAL denylist is still enforced
//!   below us; this is belt-and-suspenders.
//! - **Single I²C owner architecture** — every transaction goes through the
//!   shared `I2cServiceHandle` (no raw bus access, no parallel fd).
//! - **No bulk Write step** — the flush+RESET+JUMP sequence MUST be the
//!   per-byte variant so the bosminer wire semantics are byte-identical.
//! - **No READ step** — this is a prelude, not a probe. GET_VERSION is the
//!   caller's responsibility (`pic_read_fw_version_service`).
//! - **`recovery-tool`-gated `dspic_flash::reset_pic` stays compile-error
//!   for the daemon.** This wrapper is parallel + safer by construction;
//!   the existing destructive-ops gate is unchanged.
//!
//! ### Load-bearing invariant — call BEFORE spawning the PIC heartbeat thread
//!
//!  (2026-05-22, CE §4.6 hardening): this wrapper holds the I²C
//! service queue for ~625 ms (3 transactions × ~200 ms each due to the
//! `SleepMs(500)` and `SleepMs(100)` dwells executed on the service worker
//! thread itself). The bus is bounded-stallable, but a PIC heartbeat thread
//! spawned BEFORE this wrapper runs would see the heartbeat opcode
//! arbitrarily interleave between the flush / RESET / JUMP transactions,
//! which is exactly the parser-state-corrupting race the bosminer-canonical
//! ordering prevents.
//!
//! In `s19j_hybrid_mining.rs::S19jHybridMiner::run()` this invariant is
//! held by source order: Phase 0d (this wrapper) runs at line ~5019;
//! `spawn_pic_heartbeat_thread` is called at line ~5689 — strictly later.
//! A future refactor MUST preserve this ordering. The PSU 1 Hz heartbeat
//! thread (spawned in Phase 0c at line ~4751) is on the same I²C service
//! queue and tolerates the bounded ≤600 ms stall (PSU spoof watchdog
//! tolerance is ~30 s per `s19j_hybrid_mining.rs:1499`).
//!
//! ### Concurrency invariant — single producer per dsPIC address
//!
//! Two concurrent callers to `am2_pic_reset_and_start_app_bosminer_faithful`
//! on the same address would FIFO-interleave their 3 transactions on the
//! mpsc service queue, corrupting the chain (caller-2's flush bytes landing
//! between caller-1's flush and RESET). Today's source has exactly one
//! production call site (`s19j_hybrid_mining.rs::run()` is single-threaded,
//! `&mut self`); do not add a parallel caller without serializing entry.
//!
//! See:
//! -  §1.2 + §5.2
//! -  §2.4, §4.6
//! -  (narrow restatement — bare RESET-without-flush stays banned)
//! -  (0x50-0x57 HAL write denylist)
//!
//! ### Examples
//!
//! Build the canonical prelude and inspect its byte-exact shape (pure, no
//! I/O — safe to call from any context):
//!
//! ```
//! use dcentrald_asic::dspic::bosminer_warmup::{
//!     build_prelude_transactions, parser_flush_bytes, JUMP_SETTLE_MS, RESET_DELAY_MS,
//! };
//! use dcentrald_hal::i2c::I2cTransactionStep;
//!
//! // The flush byte chain is exactly 19 wire bytes: [55 AA 00] + 16 × 00.
//! let flush = parser_flush_bytes();
//! assert_eq!(flush.len(), 19);
//! assert_eq!(&flush[0..3], &[0x55, 0xAA, 0x00]);
//!
//! // Exactly 3 transactions (flush → RESET+500ms → JUMP+100ms), in order.
//! let txs = build_prelude_transactions();
//! assert_eq!(txs.len(), 3, "must emit exactly 3 transactions");
//!
//! // The dwell constants match the bosminer source-of-truth.
//! assert_eq!(RESET_DELAY_MS, 500);
//! assert_eq!(JUMP_SETTLE_MS, 100);
//!
//! // Every transaction uses per-byte writes (matches bosminer transport).
//! // The runtime entry point `am2_pic_reset_and_start_app_bosminer_faithful`
//! // refuses EEPROM-range addresses (0x50..=0x57) BEFORE any wire byte is
//! // sent — the predicate is unconditional and is the first statement of
//! // the function.
//! for tx in &txs {
//!     for step in tx {
//!         // Never bulk Write (bosminer canonical = per-byte).
//!         assert!(!matches!(step, I2cTransactionStep::Write(_)));
//!         // Never Read (this is a prelude, not a probe).
//!         assert!(!matches!(step, I2cTransactionStep::Read(_)));
//!     }
//! }
//! ```

use dcentrald_hal::i2c::{I2cServiceHandle, I2cTransactionStep};

use crate::{AsicError, Result};

/// Bosminer `I2C_NUM_RETRIES` analog — clean whole-transaction retry budget on
/// EIO. Set at 1 here because each `I2cTransactionStep::WriteByteByByte` step
/// has its own service-thread retry internally; the outer retry exists only
/// for a transactional-error path (channel closed, etc).
const PRELUDE_TRANSACTION_RETRY_BUDGET: u32 = 1;

/// Parser-flush command byte = `0x00` (no-op opcode in the bosminer
/// `I2cBackend::write` framing). Source: `braiins_power.rs:496` which calls
/// `self.write(0x00, &[0u8; 16])` — the framing expands to
/// `[0x55, 0xAA, 0x00, 0x00 * 16]`.
const FLUSH_FRAME_LEN: usize = 19;

/// RESET opcode = `CMD_RESET_PIC = 0x07`. Source: `braiins_power.rs:497` which
/// calls `self.write_delay(RESET_PIC, &[], RESET_DELAY)`.
const RESET_FRAME: [u8; 3] = [0x55, 0xAA, 0x07];

/// JUMP_FROM_LOADER_TO_APP opcode = `0x06`. Source:
/// `braiins_power.rs:500-505`.
const JUMP_FRAME: [u8; 3] = [0x55, 0xAA, 0x06];

/// `RESET_DELAY` from `braiins_power.rs:391`. The chip needs ~500 ms to flush
/// its TX FIFO + re-enter the bootloader.
pub const RESET_DELAY_MS: u64 = 500;

/// `BMMINER_DELAY` from `braiins_power.rs:395`. The chip needs ~100 ms after
/// the JUMP opcode to switch from bootloader to app firmware.
pub const JUMP_SETTLE_MS: u64 = 100;

/// Per-byte writes use a tight per-byte I²C timeout (10 jiffies = 100 ms) so
/// a wedged chip doesn't burn the whole prelude budget on a single hung byte.
const PER_BYTE_TIMEOUT_JIFFIES: u32 = 10;

// ===========================================================================
//   — bosminer-plus-tuner 0.9.0 strace-derived FRAMED protocol
// ===========================================================================
//
// The constants/helpers below carry the byte sequence DECODED from a live
// `strace -e trace=ioctl,read,write` capture of bosminer-plus-tuner
// 0.9.0-912d084c (LEDE / `zynq-bm3-am2`) on `a lab unit` (BraiinsOS+, MAC
// aa:bb:cc:dd:ee:ff), 2026-05-23. Source:
// refactor-planning/phase0-probe/{BOSMINER-STRACE-ANALYSIS.md,
// bosminer-strace-init-full.log.gz}`.
//
// **Why this is a SEPARATE variant rather than a replacement of the existing
// S9-era bosminer-faithful chain:** the two are talking to DIFFERENT bosminer
// generations and therefore DIFFERENT dsPIC bootloader protocol surfaces.
//
//   * `build_prelude_transactions()` (above) replays the S9-era BraiinsOS
//     `braiins_power.rs` chain — BARE 3-byte opcodes `[55 AA 07]` /
//     `[55 AA 06]`, 19-byte one-transaction parser flush, no ACK reads.
//     Proven good on `a lab unit`'s 2026-05-15 first-shares baseline.
//   * `build_strace_derived_prelude_transactions()` (this block) replays the
//     bosminer-plus-tuner 0.9.0 chain — FRAMED 6-byte opcodes
//     `[55 AA 04 07 00 0B]` / `[55 AA 04 06 00 0A]`, 7 separate single-byte
//     `0x00` sync heartbeats ( corrected count), ACK byte drained
//     after each framed opcode.
//     Live-evidence on `a lab unit` 2026-05-23: produces fw=0x89 detection + the
//     correct chip-rail voltage path (vs DCENT_OS's current fw=0x82 path
//     which programs the bare SetVoltage `[10, hi, lo]` form and lands the
//     `a lab unit` chain rail at ~2 V instead of 13.7 V).
//
// Two separate functions, gated by independent env vars
// (`DCENT_AM2_PIC_RESET_AND_START_APP` vs `DCENT_AM2_PIC_RESET_STRACE_DERIVED`),
// so the operator can A/B the two protocol generations on `a lab unit` and `a lab unit`
// without recompilation.

/// Number of single-byte `0x00` sync heartbeats bosminer-plus-tuner emits
/// before the first framed opcode.
///
/// ** (2026-05-24, `a lab unit` live-evidence correction):** value
/// corrected from 8 → 7. The original  ports cited
/// `bosminer-strace-init-full.log` lines 12708-12715 (8 writes) — but
/// that was a lab capture from a different bosminer-plus-tuner build.
/// The authoritative `a lab unit`-specific ground-truth (
/// `wave38-bosminer-truth/bosminer-i2c0-slave20.txt` lines 1-7) shows
/// **exactly 7 single-byte `0x00` writes** before the framed RESET
/// `[55 AA 04 07 00 0B]` on the live bosminer that mines `a lab unit` at
/// 26.5 GH/s.  noted this gap in its commit message but didn't
/// wire it.  closes the gap.
///
/// 8 writes are individual `write(fd, "\x00", 1)` syscalls at ~6 ms
/// intervals (kernel `i2c-dev` ioctl latency at 100 kHz; not
/// artificially delayed). 's 6-ms inter-byte sleep
/// (`DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1`) matches this cadence
/// exactly.
///
/// 2026-06-07 — corrected 7 -> 8 from the TRUE-COLD `re018-cold-strace`
/// (3 independent decodes show exactly 8 leading `0x00` before the framed
/// RESET). The earlier  "8 -> 7" was based on
/// `wave38-bosminer-truth/bosminer-i2c0-slave20.txt`, a mid-run capture
/// whose extractor dropped `<unfinished ...>` bytes (the same misdecode that
/// produced the bogus READ-CONFIG-LATCH). A short flush can leave the
/// bootloader sync-FSM not-idle when the framed `55 AA` RESET lands.
///
pub const STRACE_SYNC_HEARTBEAT_COUNT: usize = 8;

/// Framed-mode RESET opcode wire bytes:
/// `[0x55, 0xAA, LEN=0x04, CMD=0x07, PAYLOAD=0x00, CKSUM=0x0B]`.
///
/// Checksum is `LEN + CMD + sum(PAYLOAD) = 0x04 + 0x07 + 0x00 = 0x0B`.
/// Source: `bosminer-strace-init-full.log` lines 12716-12721 (PWR/1), 13234-13239 (PWR/3).
pub const STRACE_RESET_FRAME_FRAMED: [u8; 6] = [0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B];

/// Framed-mode START_APP opcode wire bytes:
/// `[0x55, 0xAA, LEN=0x04, CMD=0x06, PAYLOAD=0x00, CKSUM=0x0A]`.
///
/// Checksum is `LEN + CMD + sum(PAYLOAD) = 0x04 + 0x06 + 0x00 = 0x0A`.
/// Source: `bosminer-strace-init-full.log` lines 12724-12729 (PWR/1), 13247-13252 (PWR/3).
pub const STRACE_START_APP_FRAME_FRAMED: [u8; 6] = [0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A];

/// Bytes per single i2c read of the framed echo+ack (1 — the dsPIC emits ONE
/// byte per i2c read transaction). The full ack is the command echo (0x07/0x06)
/// followed by a 0x01 status byte, read as TWO SEPARATE 1-byte reads (see the
/// builder, which issues `STRACE_FRAMED_ACK_READS` reads `STRACE_INTER_ACK_READ_MS`
/// apart). RE-018 LIVE on `a lab unit` (2026-06-07): a single `Read(2)` grabs the echo
/// + a bus-float garbage byte (`[07,0E]`) and never consumes the real 0x01, so
/// the chip stays mid-transmission and the subsequent GET_VERSION returns
/// all-`0xFF` (chip silent). The true-cold strace reads `"\x07"` then `"\x01"`
/// as two separate reads ~7 ms apart; matching that drains the chip cleanly.
pub const STRACE_FRAMED_ACK_LEN: usize = 1;

/// Number of separate 1-byte reads that drain one framed echo+ack (`[07][01]`).
/// Bosminer reads 2 (cmd echo, then 0x01 status). RE-018 LIVE 2026-06-07.
pub const STRACE_FRAMED_ACK_READS: usize = 2;

/// Inter-read settle between the two framed-ack reads — the natural i2c-dev
/// ioctl latency in the true-cold strace was ~7 ms (`.278` → `.285`).
pub const STRACE_INTER_ACK_READ_MS: u64 = 7;

/// Strace shows ~660 ms wall-clock gap between the RESET ACK read and the
/// START_APP first byte (16:42:03.466 → 16:42:03.971 on PWR/1). The chip's
/// minimum settle is ≤500 ms — we pick 500 ms to match the existing
/// `RESET_DELAY_MS` invariant from `braiins_power.rs:391`. The extra ~160 ms
/// in the live capture was bosminer's Tokio scheduler overhead, not a chip
/// requirement.
pub const STRACE_RESET_TO_START_APP_DELAY_MS: u64 = 500;

/// ** (2026-05-24, `a lab unit` ground-truth strace +  DCENT_OS
/// strace comparison):** bosminer reads ~20 bytes from EEPROM at
/// `0x50` (and `0x52`) on `/dev/i2c-0` BEFORE addressing the dsPIC at
/// `0x20`. DCENT_OS skips this entirely ( strace shows only 2
/// `I2C_SLAVE_FORCE` ioctls in the entire daemon run, both to 0x20).
///
/// Hypothesis: the EEPROM read bus activity (continuous SDA/SCL
/// toggles for ~20 byte-time periods) wakes the dsPIC's MSSP I2C
/// slave peripheral into command-accepting mode. Without it the
/// dsPIC stays in low-power CMD-echo mode ( evidence: framed
/// RESET → reads 0x07 echo, START_APP → 0x06 echo, GET_VERSION →
/// 0x45 ; vs bosminer's 0x01/0x01/[17 89 00 A5]).
///
///  emits a single `Read(N)` transaction on the EEPROM
/// address. We use Read-only (no preceding WriteByteByByte) because
/// the EEPROM denylist (`I2cBus::write_denylist [0x50..=0x57]`)
/// blocks writes to EEPROM addresses for safety
/// ( — protects against a
/// recurrence of the `a lab unit` hb2 EEPROM corruption incident). The
/// 24Cxx EEPROM responds to current-address-read with whatever its
/// address pointer points to (undefined post-cold-boot, but reads
/// generate the same bus activity as bosminer's write-then-read).
pub const EEPROM_WARMUP_READ_LEN: usize = 32;

/// Hashboard EEPROM bus-warmup read on `/dev/i2c-0` before dsPIC
/// init. Non-fatal — a missing/unresponsive EEPROM should not block
/// dsPIC init; we only care about the bus activity, not the data.
///
/// Source: bosminer-strace-init-full.log lines 296-411 — bosminer
/// reads ~24 bytes from EEPROM 0x50 (and similar from 0x52) BEFORE
/// any traffic to dsPIC 0x20.
pub fn am2_eeprom_bus_warmup_read(i2c: &I2cServiceHandle, eeprom_addr: u8) -> Result<Vec<u8>> {
    if !is_eeprom_denylist_addr(eeprom_addr) {
        return Err(AsicError::Pic {
            addr: eeprom_addr,
            detail: format!(
                "Wave-46 EEPROM bus warmup refused: addr 0x{:02X} is NOT in the AM2 hashboard \
                 EEPROM denylist range (0x50..=0x57). This function is read-only on EEPROMs \
                 — calling it with a non-EEPROM address would attempt a current-address read \
                 on the dsPIC which would corrupt its parser state.",
                eeprom_addr
            ),
        });
    }
    let steps = vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::Read(EEPROM_WARMUP_READ_LEN),
    ];
    let reads = i2c
        .transaction(eeprom_addr, steps)
        .map_err(|e| AsicError::Pic {
            addr: eeprom_addr,
            detail: format!(
                "Wave-46 EEPROM bus warmup read failed at 0x{:02X}: {} (non-fatal — caller may \
             continue with dsPIC init; bus activity may still have woken the dsPIC FSM)",
                eeprom_addr, e
            ),
        })?;
    Ok(reads.into_iter().flatten().collect())
}

/// ** (2026-05-24, `a lab unit` strace evidence):** the dsPIC needs
/// ~66 ms to process each framed opcode and prepare the ACK byte for
/// the master to read. Bosminer's `a lab unit` ground-truth
/// (`wave38-bosminer-truth/bosminer-i2c0-slave20.txt`):
///
/// ```text
/// line 17  [16:39:06.903573] W(1B): 0B   ← last byte of RESET frame
/// line 18  [16:39:06.969471] +65.9ms R(1B): 01   ← ACK byte
///
/// line 22  [16:39:07.504617] W(1B): 0A   ← last byte of START_APP frame
/// line 23  [16:39:07.571296] +66.7ms R(1B): 01   ← ACK byte
///
/// line 28  [16:39:08.116758] W(1B): 1B   ← last byte of GET_VERSION write
/// line 29  [16:39:08.183526] +66.8ms R(1B): 17   ← first byte of 4-byte reply
/// ```
///
/// DCENT_OS /28b pre- read the ACK back-to-back with the
/// last write byte (one `I2cTransactionStep::WriteByteByByte` followed
/// immediately by `I2cTransactionStep::Read(1)`), so the read returns
/// before the dsPIC has prepared the byte → all-FF / EIO.  adds
/// a 70 ms sleep (gives a 4 ms margin over the strace 66 ms) between
/// the framed-write end and the ACK read.
///
/// Live evidence drove this fix:  +  both shipped the
/// 500 ms post-ACK settle correctly, but BOTH still returned all-FF
/// because the pre-ACK 66 ms was missing.
pub const STRACE_WRITE_TO_ACK_READ_DELAY_MS: u64 = 70;

/// Strace shows ~506 ms wall-clock gap between the START_APP ACK read and
/// the GET_VERSION first byte (16:42:04.069 → 16:42:04.575 on PWR/1). The
/// chip needs this window to fully transition from bootloader to fw=0x89
/// application state. 500 ms is the tight matching value.
pub const STRACE_START_APP_SETTLE_MS: u64 = 500;

// ===========================================================================
//   Part 2 — READ-CONFIG-LATCH opcode 0x00 (2026-05-25)
// ===========================================================================
//
// **RE-018 Agent 3 finding** (
// RE-018-AGENT-3-CHIP-RAIL.md`): bosminer's framed cold-boot warmup contains
// FOUR opcodes between the 7-byte 0x00 sync prelude and the first LM75A
// passthrough, not THREE as  ports.
//
// Authoritative ground truth (
// phase0-probe/wave38-bosminer-truth/bosminer-i2c0-slave20.txt` lines 12-32):
//
//   line 12-17:  W: 55 AA 04 07 00 0B   R: 01            (framed RESET 0x07)
//   line 18:                              (66 ms ACK)
//   line 19-22:  W: AA 04 00 0A          R: 01            (READ-CONFIG 0x00)
//                                                          ← MISSING IN DCENT_OS
//   line 23:                              (67 ms ACK)
//   line 24-28:  W: AA 04 17 00 1B       R: 17 89 00 A5  (framed GET_VERSION)
//
// Timing between transactions: RESET → +511 ms wait → READ-CONFIG → +522 ms
// wait → GET_VERSION (the existing 500-ms post-Read settle on RESET already
// covers the first gap; we add a 500-ms post-Read settle on READ-CONFIG to
// cover the second).
//
// Byte-format note: bosminer omits the `0x55` preamble on the 2nd and 3rd
// frames (the dsPIC parser stays in framed mode after the first `55 AA` is
// seen). The captured READ-CONFIG bytes are exactly 4 wire bytes — we emit
// the same 4 bytes so the wire shape is byte-identical to bosminer-strace
// ground truth. (Emitting a 6-byte `[55 AA 04 00 00 0A]` re-sync form would
// also work in principle but diverges from the captured bytes; we stay
// byte-faithful.)
//
// Functional hypothesis (Agent 3): opcode 0x00 in the dsPIC's framed-mode
// command table is a NULL / READ-CONFIG-LATCH that LATCHES the just-RESET
// state into "configuration-loaded" mode, enabling the SetVoltage DAC
// programming path. Without it, dsPIC ACKs framed `0x10` SetVoltage at the
// parser level but the internal DAC setpoint never programs — chip rail
// never switches from idle (0 V) to 13.7 V, chain enum returns 0/126.
//
//  LIVE evidence (`wave55h-LIVE-standalone-failed-20260525.log`):
// dsPIC reports fw=0x89 at line 107 (chip already engaged), yet `Chain
// presence: count=0` at line 182 — proves the rail DC-DC isn't switching
// despite dsPIC + Loki spoof both ACKing.  Part 2 fills the gap.

/// Framed-mode READ-CONFIG-LATCH opcode wire bytes ( Part 2):
/// `[0xAA, LEN=0x04, CMD=0x00, CKSUM=0x0A]`.
///
/// 4 wire bytes — NO leading `0x55` preamble, per the captured bytes at
/// `bosminer-i2c0-slave20.txt` lines 19-22. The dsPIC parser stays in
/// framed mode after the first `55 AA` of the RESET frame is consumed, so
/// continuation frames can omit the second magic byte.
///
/// Checksum byte `0x0A` is treated here as a captured-byte constant (the
/// closed-source dsPIC firmware uses a different checksum function for
/// opcode 0x00 than for 0x07 / 0x06 / 0x17; the simple `LEN+CMD+PAYLOAD`
/// formula evaluates to 0x04 not 0x0A, so the firmware applies an
/// opcode-specific transform). We emit the captured ground-truth bytes
/// verbatim — a future RE pass can decode the checksum function once the
/// dsPIC firmware itself is in scope.
///
/// Source: `bosminer-i2c0-slave20.txt:19-22` (`a lab unit` bosminer cold-cold
/// capture, 2026-05-22; live-proven mining baseline for the `a lab unit` unit).
pub const STRACE_READ_CONFIG_LATCH_FRAMED: [u8; 4] = [0xAA, 0x04, 0x00, 0x0A];

// Wall-clock wait between the framed RESET ACK and the start of the
// READ-CONFIG-LATCH write (bosminer-i2c0-slave20.txt: line 18 R: 01 at
// 06.969471 → line 19 W: AA at 07.480660 = +511.2 ms). The chip needs
// this window to flush its RESET-entry state machine before accepting
// the next framed opcode. The existing `STRACE_RESET_TO_START_APP_DELAY_MS
// = 500` already provides a tight matching value (the strace's extra
// ~11 ms is scheduler overhead), so we reuse it via the existing post-Read
// settle on the RESET transaction. No new constant needed for the
// pre-READ-CONFIG wait — it's covered by the RESET tx's existing
// `SleepMs(500)`.

/// Wall-clock wait between the READ-CONFIG-LATCH ACK and the start of
/// the GET_VERSION write (`bosminer-i2c0-slave20.txt`:
/// line 23 R: 01 at 07.571296 → line 24 W: AA at 08.092906 = +521.6 ms).
/// 500 ms matches `STRACE_RESET_TO_START_APP_DELAY_MS` and is the tight
/// per-strace value (the extra ~21 ms in the capture is scheduler
/// overhead, not a chip requirement).
pub const STRACE_READ_CONFIG_LATCH_SETTLE_MS: u64 = 500;

/// ** Part 2 opt-in env helper** for the framed READ-CONFIG-LATCH
/// opcode. Default-OFF — when unset, the strace-derived warmup chain is
/// byte-identical to the pre- path (RESET → GET_VERSION via the
/// existing START_APP step). When set to `"1"`, the warmup chain inserts
/// the READ-CONFIG-LATCH transaction BETWEEN the framed RESET and the
/// existing START_APP step (so warmup becomes: 7×heartbeat → RESET →
/// READ-CONFIG-LATCH → START_APP).
///
/// **Intended usage:** standalone cold-cold NAND boot on `a lab unit`-class
/// hardware where bosminer is NOT pre-engaging the chip rail. The
/// dcentrald-am2-xil-env recipe-variant routing sets this when the
/// `STANDALONE_RE_FIX` umbrella is active.
///
/// **Must NOT be set** on the bosminer-handoff path ( PROVEN
/// MINING recipe) — the chip is already in fw=0x89 app mode and re-running
/// the warmup chain (including this new step) would re-RESET the chip
/// back to bootloader. The handoff launchers (`run_wave54_25_PROVEN_MINING.sh`,
/// `run_wave55i_25_HANDOFF.sh`) defensively `unset` this var for that
/// reason ( QA-Q3 cross-launcher safety pattern).
pub fn am2_dspic_read_config_latch_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// Returns the strace-derived prelude transaction list.
///
/// Each top-level `Vec<I2cTransactionStep>` is **one** call to
/// `I2cServiceHandle::transaction(addr, ...)`. The order MUST be:
///
/// 1..=7. Per-byte sync heartbeat (7 separate transactions, each
///        `WriteByteByByte([0x00])`).
/// 8.    RESET framed: `WriteByteByByte([55 AA 04 07 00 0B])` then
///       `Read(1)` to drain ACK, then `SleepMs(500)`.
/// 8.5.  **( Part 2, opt-in via `DCENT_AM2_DSPIC_READ_CONFIG_LATCH=1`)**
///       READ-CONFIG-LATCH framed: `WriteByteByByte([AA 04 00 0A])` then
///       `Read(1)` to drain ACK, then `SleepMs(500)`. Inserted between
///       RESET and START_APP to LATCH the post-RESET state into
///       "configuration-loaded" mode so subsequent SetVoltage opcodes
///       can program the DAC. See `STRACE_READ_CONFIG_LATCH_FRAMED`
///       doc-comment for the strace ground-truth citation.
/// 9.    START_APP framed: `WriteByteByByte([55 AA 04 06 00 0A])` then
///       `Read(1)` to drain ACK, then `SleepMs(500)`.
///
/// Total = 10 transactions by default, 11 with `DCENT_AM2_DSPIC_READ_CONFIG_LATCH=1`.
/// The 8 sync heartbeats are intentionally separate transactions (not an
/// 8-byte `WriteByteByByte([0;8])`) because the `a lab unit` strace evidence
/// shows bosminer makes 8 independent `write(fd, "\x00", 1)` calls —
/// each one's kernel ioctl creates the natural 6 ms inter-byte gap that
/// the dsPIC bootloader's sync FSM relies on. RE-018 (2026-06-07)
/// corrected the count to 8 (the true-cold `re018-cold-strace`; the
/// earlier  "7" was the dropped-byte `wave38` extract) — see
/// `STRACE_SYNC_HEARTBEAT_COUNT` doc-comment for the source citation.
///
/// ** Part 2 default-OFF safety**: the READ-CONFIG-LATCH step is
/// opt-in via env var so the default-deployed binary behavior is
/// byte-identical to the pre- path for the bosminer-handoff
/// recipe. Standalone cold-cold launchers opt in; handoff launchers
/// defensively unset the env var.
pub fn build_strace_derived_prelude_transactions() -> Vec<Vec<I2cTransactionStep>> {
    let read_config_latch_enabled = am2_dspic_read_config_latch_enabled();
    let mut txs: Vec<Vec<I2cTransactionStep>> = Vec::with_capacity(
        STRACE_SYNC_HEARTBEAT_COUNT + 2 + if read_config_latch_enabled { 1 } else { 0 },
    );

    // Steps 1..=7: per-byte sync heartbeats (7 separate transactions).
    for _ in 0..STRACE_SYNC_HEARTBEAT_COUNT {
        txs.push(vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(vec![0x00]),
        ]);
    }

    // Step 8 ( renumbered from 9): framed RESET +  70 ms
    // pre-Read settle (lets dsPIC prepare the ACK byte) + drain ACK +
    // 500 ms post-Read settle. The 500 ms post-Read settle also covers
    // the strace's +511 ms wait before the (optional) READ-CONFIG-LATCH
    // or the START_APP write — same chip-side dwell window either way.
    txs.push(vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(STRACE_RESET_FRAME_FRAMED.to_vec()),
        I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
        // Drain the framed echo+ack as TWO separate 1-byte reads (cmd echo,
        // then 0x01 status), STRACE_INTER_ACK_READ_MS apart — the dsPIC emits
        // one byte per i2c read. A single Read(2) reads the echo + bus-float
        // garbage and leaves the chip mid-ack → GET_VERSION goes all-0xFF.
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_INTER_ACK_READ_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_RESET_TO_START_APP_DELAY_MS),
    ]);

    // Step 8.5 ( Part 2, 2026-05-25, OPT-IN):
    // framed READ-CONFIG-LATCH opcode 0x00 inserted between RESET and
    // START_APP. Per RE-018 Agent 3 + bosminer-i2c0-slave20.txt lines
    // 19-22, this LATCHES the post-RESET state into "configuration-
    // loaded" mode so subsequent SetVoltage opcodes actually program
    // the DAC. Without it, dsPIC ACKs at the parser layer but chip
    // rail stays at 0 V → chain enum returns 0/126.
    //
    // 4-byte wire form (no 0x55 preamble) — the captured ground-truth
    // bytes from bosminer cold-cold. Same  70 ms pre-Read ACK
    // settle + 500 ms post-Read settle as RESET / START_APP.
    if read_config_latch_enabled {
        txs.push(vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(STRACE_READ_CONFIG_LATCH_FRAMED.to_vec()),
            I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
            I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
            I2cTransactionStep::SleepMs(STRACE_INTER_ACK_READ_MS),
            I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
            I2cTransactionStep::SleepMs(STRACE_READ_CONFIG_LATCH_SETTLE_MS),
        ]);
    }

    // Step 9 ( renumbered from 10): framed START_APP +
    // 70 ms pre-Read settle + drain ACK + 500 ms post-Read settle.
    txs.push(vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(STRACE_START_APP_FRAME_FRAMED.to_vec()),
        I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_INTER_ACK_READ_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_START_APP_SETTLE_MS),
    ]);

    let expected_len =
        STRACE_SYNC_HEARTBEAT_COUNT + 2 + if read_config_latch_enabled { 1 } else { 0 };
    debug_assert_eq!(txs.len(), expected_len);
    txs
}

/// COLD-BYTE-DIFF Fix B (2026-06-07) — the strace-derived prelude as ONE flat
/// transaction-step list: the byte-and-sleep-identical concatenation of every
/// step produced by [`build_strace_derived_prelude_transactions`].
///
/// **Why this exists.** The multi-transaction form (above) runs the cold `a lab unit`
/// warmup as N separate `I2cServiceHandle::transaction()` calls (8 heartbeat
/// flushes + framed RESET + optional READ-CONFIG-LATCH + framed START_APP).
/// Between those N service requests the single-owner i2c-0 worker can dequeue
/// OTHER producers' requests (thermal LM75 reads, PIC heartbeats, absent-slave
/// probes), interleaving foreign bytes into the cold dsPIC MSSP bootloader
/// parser and desyncing it so it echoes `0x82` instead of ACKing the framed
/// RESET/JUMP. The true-cold bosminer strace
/// proves
/// bosminer holds fd=22 EXCLUSIVELY for the entire warmup — zero foreign i2c-0
/// traffic from the first flush byte through fw=0x89.
///
/// Concatenating the SAME steps into ONE `Vec<I2cTransactionStep>` makes the
/// service worker run the entire warmup atomically — `execute_transaction`
/// processes one dequeued request's whole step list before touching the queue
/// again (`dcentrald-hal::i2c::execute_transaction`), so no other producer can
/// interleave at ANY byte / transaction boundary. This structurally reproduces
/// bosminer's exclusive-fd cold warmup.
///
/// **The on-wire byte sequence + timing are UNCHANGED.** This is LITERALLY
/// `flatten()` of [`build_strace_derived_prelude_transactions`] — same flush
/// bytes, same `0x55`-led RESET/JUMP frames, same 2-separate-reads ACK drain
/// (`STRACE_FRAMED_ACK_READS`), same `SleepMs` dwells, same order. Deriving it
/// from the same canonical builder means any future change to the warmup bytes
/// propagates to both forms automatically; a regression test pins the
/// concatenation identity. The only difference vs the N-transaction form is the
/// transaction boundary: the per-sub-transaction `set_slave(addr)` ioctl
/// collapses to one. `set_slave` emits NO wire byte, so the wire stream is
/// bit-identical — and one slave-select is in fact MORE bosminer-faithful
/// (bosminer issues exactly one `I2C_SLAVE` ioctl for the whole warmup).
pub fn build_strace_derived_prelude_single_transaction() -> Vec<I2cTransactionStep> {
    build_strace_derived_prelude_transactions()
        .into_iter()
        .flatten()
        .collect()
}

///  — emit the bosminer-plus-tuner 0.9.0 strace-derived FRAMED PIC
/// reset+start-app prelude on `addr`.
///
/// Behaviour: 7 single-byte 0x00 sync heartbeats ( corrected), then framed RESET
/// `[55 AA 04 07 00 0B]` + read ACK + 500 ms, then framed START_APP
/// `[55 AA 04 06 00 0A]` + read ACK + 500 ms. Live-evidence-derived from
///
/// BOSMINER-STRACE-ANALYSIS.md`. Default off — opt in via
/// `DCENT_AM2_PIC_RESET_STRACE_DERIVED=1`.
///
/// After this returns `Ok(())` the caller should invoke
/// `pic_read_fw_version_service(...)` to read the FW version byte. With this
/// prelude bosminer reads `0x89` on `a lab unit` (framed-DAC encoding family); with
/// the existing `am2_pic_reset_and_start_app_bosminer_faithful` (S9-era bare
/// 3-byte opcodes) DCENT_OS reads `0x82` (bare BE-mV SetVoltage family) on
/// the same chip. If `pic_read_fw_version_service` still reads `0x82` after
/// this prelude, that means the GET_VERSION read path itself also needs the
/// framed `[55 AA 04 17 00 1B]` shape — separate follow-up tracked as
/// " GET_VERSION framed-read upgrade".
///
/// ## Concurrency + ordering invariants
///
/// Identical to `am2_pic_reset_and_start_app_bosminer_faithful`:
/// - Single producer per dsPIC address (no parallel callers).
/// - MUST run BEFORE the PIC heartbeat thread for `addr` is spawned.
/// - EEPROM denylist 0x50..=0x57 refused fail-closed before any wire byte.
///
/// ## Errors
///
/// - `AsicError::Pic` if `addr` is in `0x50..=0x57` (EEPROM denylist).
/// - `AsicError::Pic` wrapping the underlying I²C error if any transaction
///   fails after the retry budget is exhausted.
pub fn am2_pic_reset_and_start_app_strace_derived(i2c: &I2cServiceHandle, addr: u8) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "Wave-28 strace-derived warmup refused: addr 0x{:02X} is in the AM2 hashboard \
                 EEPROM write-denylist range (0x50..=0x57); this warmup writes only to dsPIC \
                 addresses (0x20/0x21/0x22). Refusing fail-closed before any wire byte \
",
                addr
            ),
        });
    }

    //  HIGH-2 (2026-05-24, DCENT_EE swarm finding): the framed
    // RESET opcode 0x07 by design re-enters the dsPIC bootloader, which
    // transitions a chip already in fw=0x89 app mode BACK to fw=0x82
    // (BARE) mode. If `a lab unit`-class fw=0x89 mining is in progress (e.g.,
    // post-bosminer-handoff  recipe), running this warmup would
    // BREAK the working state. Probe `addr` for fw first; if it already
    // reports a valid fw byte, log and skip — do NOT re-issue RESET.
    //
    // Override: `DCENT_AM2_PIC_RESET_FORCE_REDO=1` (lab-only). Lets a
    // future agent or operator force the framed RESET even if the chip
    // is already in app mode. Default-OFF.
    let force_redo = std::env::var("DCENT_AM2_PIC_RESET_FORCE_REDO")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if !force_redo {
        // Best-effort probe: try one short fw read. Don't fail-closed if
        // the probe itself errors — that just means the chip isn't in
        // a clean app mode, so the warmup SHOULD run.
        let probe_steps = vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(vec![0x55, 0xAA, 0x17]),
            I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
            I2cTransactionStep::Read(1),
        ];
        if let Ok(reads) = i2c.transaction(addr, probe_steps) {
            if let Some(first_read) = reads.first().and_then(|r| r.first()) {
                let fw = *first_read;
                // Known valid fw bytes per `dspic::DspicFirmware` enum +
                // current investigation: 0x82 (BARE), 0x88, 0x89 (FRAMED),
                // 0x8A. 0x86 is degraded — also "alive" but refused per
                // .
                let is_alive_app = matches!(fw, 0x82 | 0x86 | 0x88 | 0x89 | 0x8A);
                if is_alive_app {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        observed_fw = format_args!("0x{:02X}", fw),
                        "Wave-55a precondition guard: dsPIC at 0x{:02X} is already \
                         in app mode (fw=0x{:02X}) — SKIPPING Wave-28 framed RESET \
                         to avoid transitioning chip from fw=0x89 back to fw=0x82. \
                         Set DCENT_AM2_PIC_RESET_FORCE_REDO=1 to override (lab only).",
                        addr, fw
                    );
                    return Ok(());
                }
            }
        }
    }

    let transactions = build_strace_derived_prelude_transactions();
    let total_txs = transactions.len();
    let read_config_latch_active = am2_dspic_read_config_latch_enabled();

    for (idx, steps) in transactions.into_iter().enumerate() {
        let label = if idx < STRACE_SYNC_HEARTBEAT_COUNT {
            format!(
                "sync-heartbeat[{}/{}]",
                idx + 1,
                STRACE_SYNC_HEARTBEAT_COUNT
            )
        } else if idx == STRACE_SYNC_HEARTBEAT_COUNT {
            "FRAMED_RESET[6B]+ACK+500ms".to_string()
        } else if read_config_latch_active && idx == STRACE_SYNC_HEARTBEAT_COUNT + 1 {
            "FRAMED_READ_CONFIG_LATCH[4B]+ACK+500ms".to_string()
        } else if idx == total_txs - 1 {
            "FRAMED_START_APP[6B]+ACK+500ms".to_string()
        } else {
            format!("step[{}]", idx)
        };

        let mut last_err: Option<dcentrald_hal::HalError> = None;
        let mut succeeded = false;
        for attempt in 1..=PRELUDE_TRANSACTION_RETRY_BUDGET.max(1) {
            tracing::trace!(
                target: "i2c_audit",
                addr = format_args!("0x{:02X}", addr),
                step = idx,
                label = label.as_str(),
                attempt,
                "Wave-28 strace-derived warmup: emitting prelude transaction"
            );
            match i2c.transaction(addr, steps.clone()) {
                Ok(reads) => {
                    // 2026-06-07 (RE-018 .25 cold-engage diagnostic): the warmup
                    // historically DISCARDED the reply bytes, so we never knew if a
                    // cold dsPIC bootloader actually ACK'd the framed RESET ([07 01])
                    // / JUMP ([06 01]) or just echoed its fw byte ([82 82..] = frame
                    // NOT recognized → chip stays in bootloader). Log them.
                    let ack: Vec<u8> = reads.iter().flatten().copied().collect();
                    tracing::info!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label.as_str(),
                        attempt,
                        ack = format_args!("{:02X?}", ack),
                        "Wave-28 strace-derived warmup: prelude ACK bytes (real ACK = [07/06, 01]; \
                         all-fw-byte echo = frame not recognized)"
                    );
                    succeeded = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label.as_str(),
                        attempt,
                        error = %e,
                        "Wave-28 strace-derived warmup: prelude transaction failed; clean retry"
                    );
                    last_err = Some(e);
                }
            }
        }
        if !succeeded {
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "Wave-28 strace-derived warmup step {} ({}) failed after {} attempt(s): {}",
                    idx,
                    label,
                    PRELUDE_TRANSACTION_RETRY_BUDGET.max(1),
                    last_err
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "no error captured".to_string()),
                ),
            });
        }
    }

    tracing::info!(
        addr = format_args!("0x{:02X}", addr),
        sync_heartbeats = STRACE_SYNC_HEARTBEAT_COUNT,
        reset_dwell_ms = STRACE_RESET_TO_START_APP_DELAY_MS,
        start_app_settle_ms = STRACE_START_APP_SETTLE_MS,
        "Phase 0-pre (Wave-28 strace-derived): bosminer-plus-tuner 0.9.0 FRAMED PIC \
         reset+start-app chain emitted (8×0x00 sync + framed[55 AA 04 07 00 0B]+ACK+500ms + \
         framed[55 AA 04 06 00 0A]+ACK+500ms — expected to land fw=0x89 on .25-class AM2)"
    );

    Ok(())
}

/// COLD-BYTE-DIFF Fix B (2026-06-07) — emit the  strace-derived FRAMED
/// warmup as ONE atomic `I2cServiceHandle::transaction()` so no other i2c-0
/// producer can interleave between the flush / RESET / JUMP steps and desync
/// the cold dsPIC MSSP bootloader parser.
///
/// Behaviour is byte-identical to
/// [`am2_pic_reset_and_start_app_strace_derived`] — same EEPROM denylist
/// refusal, same  precondition fw-probe (skip if the chip is already in
/// app mode unless `DCENT_AM2_PIC_RESET_FORCE_REDO=1`, so a fw=0x89 chip is
/// never re-RESET back to fw=0x82), same forensic ACK logging — **EXCEPT** the
/// warmup step list is executed as a SINGLE transaction instead of N separate
/// ones. The wire bytes + dwells are identical (the step list is
/// [`build_strace_derived_prelude_single_transaction`], which is `flatten()` of
/// the N-transaction builder).
///
/// This is the durable code half of COLD-BYTE-DIFF Fix B. The daemon gates it
/// on `DCENT_AM2_DSPIC_COLD_WARMUP_EXCLUSIVE=1` AND the `a lab unit` fingerprint,
/// default-OFF — so the fleet / handoff / legacy paths keep running the proven
/// N-transaction form (`am2_pic_reset_and_start_app_strace_derived`) unchanged.
///
/// ## Concurrency + ordering invariants
///
/// Identical to [`am2_pic_reset_and_start_app_strace_derived`]:
/// - Single producer per dsPIC address (no parallel callers).
/// - MUST run BEFORE the PIC heartbeat thread for `addr` is spawned.
/// - EEPROM denylist 0x50..=0x57 refused fail-closed before any wire byte.
///
/// ## Errors
///
/// - `AsicError::Pic` if `addr` is in `0x50..=0x57` (EEPROM denylist).
/// - `AsicError::Pic` wrapping the underlying I²C error if the single
///   transaction fails after the retry budget is exhausted.
pub fn am2_pic_reset_and_start_app_strace_derived_exclusive(
    i2c: &I2cServiceHandle,
    addr: u8,
) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "COLD-BYTE-DIFF Fix B exclusive warmup refused: addr 0x{:02X} is in the AM2 \
                 hashboard EEPROM write-denylist range (0x50..=0x57); this warmup writes only to \
                 dsPIC addresses (0x20/0x21/0x22). Refusing fail-closed before any wire byte \
",
                addr
            ),
        });
    }

    //  HIGH-2 precondition guard (byte-identical to the N-transaction
    // form): never re-RESET a chip that is already in app mode — the framed
    // RESET opcode 0x07 re-enters the bootloader and pulls a fw=0x89 chip back
    // to fw=0x82, which would re-break a post-handoff mining chip. The probe is
    // a separate read transaction; it is NOT part of the atomic warmup block.
    // Override: `DCENT_AM2_PIC_RESET_FORCE_REDO=1` (lab only).
    let force_redo = std::env::var("DCENT_AM2_PIC_RESET_FORCE_REDO")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    if !force_redo {
        let probe_steps = vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(vec![0x55, 0xAA, 0x17]),
            I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
            I2cTransactionStep::Read(1),
        ];
        if let Ok(reads) = i2c.transaction(addr, probe_steps) {
            if let Some(first_read) = reads.first().and_then(|r| r.first()) {
                let fw = *first_read;
                let is_alive_app = matches!(fw, 0x82 | 0x86 | 0x88 | 0x89 | 0x8A);
                if is_alive_app {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        observed_fw = format_args!("0x{:02X}", fw),
                        "COLD-BYTE-DIFF Fix B precondition guard: dsPIC at 0x{:02X} is already \
                         in app mode (fw=0x{:02X}) — SKIPPING exclusive framed RESET to avoid \
                         transitioning chip from fw=0x89 back to fw=0x82. Set \
                         DCENT_AM2_PIC_RESET_FORCE_REDO=1 to override (lab only).",
                        addr, fw
                    );
                    return Ok(());
                }
            }
        }
    }

    // The ENTIRE warmup as ONE step list → ONE service request → atomic on the
    // single-owner i2c-0 worker. No interleave point for foreign producers.
    let steps = build_strace_derived_prelude_single_transaction();
    let read_config_latch_active = am2_dspic_read_config_latch_enabled();
    let total_steps = steps.len();

    tracing::info!(
        target: "i2c_audit",
        addr = format_args!("0x{:02X}", addr),
        steps = total_steps,
        read_config_latch = read_config_latch_active,
        "COLD-BYTE-DIFF Fix B: emitting the ENTIRE strace-derived warmup as ONE atomic \
         i2c.transaction() (no interleave point for foreign i2c-0 producers; wire bytes \
         + dwells identical to the N-transaction form)"
    );

    let mut last_err: Option<dcentrald_hal::HalError> = None;
    for attempt in 1..=PRELUDE_TRANSACTION_RETRY_BUDGET.max(1) {
        match i2c.transaction(addr, steps.clone()) {
            Ok(reads) => {
                let ack: Vec<u8> = reads.iter().flatten().copied().collect();
                tracing::info!(
                    target: "i2c_audit",
                    addr = format_args!("0x{:02X}", addr),
                    attempt,
                    ack = format_args!("{:02X?}", ack),
                    "COLD-BYTE-DIFF Fix B exclusive warmup: atomic prelude ACK bytes \
                     (real ACK = [07/06, 01]; all-fw-byte echo = frame not recognized)"
                );
                tracing::info!(
                    addr = format_args!("0x{:02X}", addr),
                    sync_heartbeats = STRACE_SYNC_HEARTBEAT_COUNT,
                    reset_dwell_ms = STRACE_RESET_TO_START_APP_DELAY_MS,
                    start_app_settle_ms = STRACE_START_APP_SETTLE_MS,
                    "Phase 0-pre (COLD-BYTE-DIFF Fix B): strace-derived FRAMED warmup chain \
                     emitted as ONE atomic transaction (8×0x00 sync + framed[55 AA 04 07 00 0B]\
                     +ACK + framed[55 AA 04 06 00 0A]+ACK — expected to land fw=0x89 on \
                     .25-class AM2)"
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    target: "i2c_audit",
                    addr = format_args!("0x{:02X}", addr),
                    attempt,
                    error = %e,
                    "COLD-BYTE-DIFF Fix B exclusive warmup: atomic transaction failed; clean retry"
                );
                last_err = Some(e);
            }
        }
    }

    Err(AsicError::Pic {
        addr,
        detail: format!(
            "COLD-BYTE-DIFF Fix B exclusive warmup failed after {} attempt(s): {}",
            PRELUDE_TRANSACTION_RETRY_BUDGET.max(1),
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "no error captured".to_string()),
        ),
    })
}

/// Build the canonical 19-byte flush payload `[0x55, 0xAA, 0x00, 0x00 * 16]`.
///
/// Exposed for the unit tests in [`tests`] so they can assert the byte-exact
/// shape without having to reconstruct the constant in two places.
pub fn parser_flush_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(FLUSH_FRAME_LEN);
    bytes.push(0x55);
    bytes.push(0xAA);
    bytes.push(0x00); // cmd byte for the `write(0x00, &[0; 16])` framing
    bytes.extend(std::iter::repeat_n(0x00u8, 16));
    debug_assert_eq!(bytes.len(), FLUSH_FRAME_LEN);
    bytes
}

/// Returns the exact three I²C-service transaction-step lists this wrapper
/// would emit for the given dsPIC address.
///
/// Each top-level `Vec<I2cTransactionStep>` is **one** call to
/// `I2cServiceHandle::transaction(addr, ...)`. The order MUST be:
///
/// 1. Parser flush  — `WriteByteByByte([55 AA 00] + 16 × 00)`.
/// 2. RESET         — `WriteByteByByte([55 AA 07])` + `SleepMs(500)`.
/// 3. JUMP          — `WriteByteByByte([55 AA 06])` + `SleepMs(100)`.
///
/// This shape is what the structural unit tests in [`tests`] pin so a future
/// agent cannot strip the flush by accident.
pub fn build_prelude_transactions() -> Vec<Vec<I2cTransactionStep>> {
    let flush = parser_flush_bytes();

    let flush_steps = vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(flush),
        // No inter-byte sleep — per-byte writes already eat the kernel
        // ioctl latency between bytes (matches bosminer behaviour).
    ];

    let reset_steps = vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(RESET_FRAME.to_vec()),
        // RESET_DELAY — hold the I²C lock so no other thread interleaves a
        // heartbeat opcode while the chip is in bootloader-entry transition.
        I2cTransactionStep::SleepMs(RESET_DELAY_MS),
    ];

    let jump_steps = vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(JUMP_FRAME.to_vec()),
        // BMMINER_DELAY — give the dsPIC time to switch from bootloader to
        // app firmware before any subsequent GET_VERSION reaches it.
        I2cTransactionStep::SleepMs(JUMP_SETTLE_MS),
    ];

    vec![flush_steps, reset_steps, jump_steps]
}

/// Returns `true` if `addr` is in the AM2 hashboard EEPROM write-denylist
/// range `0x50..=0x57`.
///
/// This wrapper writes only to dsPIC addresses (0x20/0x21/0x22 on AM2). Any
/// caller that misroutes the target into the EEPROM range gets a clean
/// `AsicError::Pic` before any wire byte is sent. The HAL denylist is still
/// the load-bearing gate; this is a second line of defence so future bugs
/// can't accidentally use the bosminer prelude to corrupt a hashboard EEPROM.
/// The hashboard-EEPROM I²C address range (0x50..=0x57) that is write-protected
/// per the load-bearing corruption-prevention guarantee (the .74 hb2 EEPROM
/// corruption incident). `pub` so the integration test can pin the REAL range
/// rather than a literal copy of it (gap-swarm HAL-safety #3).
pub fn is_eeprom_denylist_addr(addr: u8) -> bool {
    (0x50..=0x57).contains(&addr)
}

/// Run the bosminer-faithful pre-GET_VERSION dsPIC wake on `addr`.
///
/// Behaviour: emits the 19-byte parser flush, then `[55 AA 07]` + 500 ms,
/// then `[55 AA 06]` + 100 ms — all via the existing `I2cServiceHandle`
/// transport so the single-I²C-owner contract is preserved.
///
/// After this returns `Ok(())` the caller should invoke
/// `pic_read_fw_version_service(...)` (or the equivalent on the
/// service-backed `Pic0x89Service`) to actually read the FW byte. This
/// function does NOT read GET_VERSION itself — that's a separate transaction
/// owned by the existing probing code.
///
/// ## Load-bearing call-order invariant (CE §4.6)
///
/// **MUST be called BEFORE any PIC heartbeat thread is spawned for this
/// address.** The wrapper holds the I²C service queue for ~625 ms through
/// the three transactions; a heartbeat thread that is already alive would
/// FIFO-interleave its 1 Hz heartbeat opcode between the flush / RESET /
/// JUMP transactions, corrupting the dsPIC MSSP parser state. The PSU
/// heartbeat thread on a different address (0x10) is safe because the
/// service serializes them and the PSU watchdog tolerance is ~30 s; the
/// PIC heartbeat thread on the SAME address is unsafe.
///
/// In 's `s19j_hybrid_mining.rs::run()`, Phase 0d (this wrapper)
/// runs at ~line 5019 while `spawn_pic_heartbeat_thread` runs at ~line
/// 5689 — invariant held by source-order, not by lock. Preserve this
/// ordering in any future refactor.
///
/// ## Errors
///
/// - `AsicError::Pic` if `addr` is in `0x50..=0x57` (EEPROM denylist).
/// - `AsicError::Pic` wrapping the underlying I²C error if any of the three
///   transactions fail after the per-step service retry budget is exhausted.
///
/// ## Forensic audit trail
///
/// Each transaction emits a `tracing::trace!` line with `target = "i2c_audit"`
/// so post-incident forensics can confirm the prelude actually ran. Enable
/// with `RUST_LOG=i2c_audit=trace,info`.
pub fn am2_pic_reset_and_start_app_bosminer_faithful(
    i2c: &I2cServiceHandle,
    addr: u8,
) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "bosminer warmup refused: addr 0x{:02X} is in the AM2 hashboard \
                 EEPROM write-denylist range (0x50..=0x57); the bosminer-faithful \
                 prelude writes only to dsPIC addresses (0x20/0x21/0x22). Refusing \
                 fail-closed before any wire byte",
                addr
            ),
        });
    }

    let transactions = build_prelude_transactions();

    let step_labels = ["parser-flush[19B]", "RESET[3B]+500ms", "JUMP[3B]+100ms"];

    for (idx, (steps, label)) in transactions.into_iter().zip(step_labels.iter()).enumerate() {
        let mut last_err: Option<dcentrald_hal::HalError> = None;
        let mut succeeded = false;
        for attempt in 1..=PRELUDE_TRANSACTION_RETRY_BUDGET.max(1) {
            tracing::trace!(
                target: "i2c_audit",
                addr = format_args!("0x{:02X}", addr),
                step = idx,
                label = label,
                attempt,
                "bosminer warmup: emitting prelude transaction"
            );
            match i2c.transaction(addr, steps.clone()) {
                Ok(_reads) => {
                    succeeded = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label,
                        attempt,
                        error = %e,
                        "bosminer warmup: prelude transaction failed; clean retry"
                    );
                    last_err = Some(e);
                }
            }
        }
        if !succeeded {
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "bosminer warmup step {} ({}) failed after {} attempt(s): {}",
                    idx,
                    label,
                    PRELUDE_TRANSACTION_RETRY_BUDGET.max(1),
                    last_err
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "no error captured".to_string()),
                ),
            });
        }
    }

    tracing::info!(
        addr = format_args!("0x{:02X}", addr),
        flush_bytes = FLUSH_FRAME_LEN,
        reset_delay_ms = RESET_DELAY_MS,
        jump_settle_ms = JUMP_SETTLE_MS,
        "Phase 0-pre: bosminer-faithful PIC reset_and_start_app chain emitted \
         (flush + 0x07 + 500ms + 0x06 + 100ms — fw=0x89-safe, parser primed)"
    );

    Ok(())
}

/// 5 variant — same as `am2_pic_reset_and_start_app_bosminer_faithful`
/// but SKIPS the `[0x55, 0xAA, 0x06]` JUMP_TO_APP step (and its 100 ms settle).
///
/// Use case: `a lab unit` XIL exhibits a chain-init failure mode where the
/// post-JUMP_TO_APP dsPIC firmware (fw=0x82) uses a SetVoltage frame
/// encoding (`[0x10, hi, lo]` of 16-bit mV) that empirically programs the
/// chip rail to ~2 V instead of the commanded 13.7 V (per the morning
/// 2026-05-22 PIC voltage feedback open question #1, reproduced in the
///  chain-enum-0 session).
///
/// BraiinsOS — which mines successfully on the SAME unit at ~37 TH/s —
/// reads `fw=0x89` (no JUMP) and uses the framed-DAC encoding
/// `framed_voltage_dac(13700) = 6` per `dspic::mod.rs:160`. That path
/// produces the BOS-proven chip rail at 13.7 V.
///
/// This wrapper still emits the byte-exact 19-byte parser flush + `0x07`
/// RESET + 500 ms — the part that primes the dsPIC MSSP parser for a
/// clean GET_VERSION read on cold-boot — but does NOT JUMP, so the dsPIC
/// stays in bootloader mode (fw=0x89). The DCENT_OS GET_VERSION machinery
/// then reads 0x89 and routes through the framed-DAC SetVoltage path.
///
/// Byte-exactness: this variant is byte-identical to the first two
/// transactions of `build_prelude_transactions()`. We literally call that
/// helper and truncate the returned vector to length 2, so a regression
/// that changes the canonical chain automatically propagates here.
///
/// All other invariants (single-producer per dsPIC address, I²C service
/// owner contract, EEPROM denylist refusal, retry budget) are identical
/// to `am2_pic_reset_and_start_app_bosminer_faithful`.
pub fn am2_pic_reset_only_bosminer_faithful(i2c: &I2cServiceHandle, addr: u8) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "bosminer warmup (no-jump variant) refused: addr 0x{:02X} is in the AM2 hashboard \
                 EEPROM write-denylist range (0x50..=0x57)",
                addr
            ),
        });
    }

    let mut transactions = build_prelude_transactions();
    transactions.truncate(2); // drop the JUMP_TO_APP step (and its 100 ms dwell)

    let step_labels = ["parser-flush[19B]", "RESET[3B]+500ms"];

    for (idx, (steps, label)) in transactions.into_iter().zip(step_labels.iter()).enumerate() {
        let mut last_err: Option<dcentrald_hal::HalError> = None;
        let mut succeeded = false;
        for attempt in 1..=PRELUDE_TRANSACTION_RETRY_BUDGET.max(1) {
            tracing::trace!(
                target: "i2c_audit",
                addr = format_args!("0x{:02X}", addr),
                step = idx,
                label = label,
                attempt,
                "bosminer warmup (no-jump): emitting prelude transaction"
            );
            match i2c.transaction(addr, steps.clone()) {
                Ok(_reads) => {
                    succeeded = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label,
                        attempt,
                        error = %e,
                        "bosminer warmup (no-jump): prelude transaction failed; clean retry"
                    );
                    last_err = Some(e);
                }
            }
        }
        if !succeeded {
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "bosminer warmup (no-jump) step {} ({}) failed after {} attempt(s): {}",
                    idx,
                    label,
                    PRELUDE_TRANSACTION_RETRY_BUDGET.max(1),
                    last_err
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "no error captured".to_string()),
                ),
            });
        }
    }

    tracing::info!(
        addr = format_args!("0x{:02X}", addr),
        flush_bytes = FLUSH_FRAME_LEN,
        reset_delay_ms = RESET_DELAY_MS,
        "Phase 0-pre (Wave-25.5 no-jump): bosminer-faithful PIC parser-flush+RESET emitted \
         (flush + 0x07 + 500ms — JUMP SKIPPED so PIC stays in fw=0x89 bootloader mode)"
    );

    Ok(())
}

// ===========================================================================
//   — bosminer-faithful LM75A passthrough warmup (2026-05-25)
// ===========================================================================
//
// ** (2026-05-25, RE finding by Phase 2c live-evidence RE pass).**
// Bosminer's healthy cold-boot trace to dsPIC slave 0x20 contains ZERO
// `0x10` (SetVoltage) and ZERO `0x15` (ENABLE_VOLTAGE) opcodes. The full
// 7.4-second window (`bosminer-i2c0-slave20.txt:33-203`) is dominated by
// 17 transactions to LM75A temp sensors via dsPIC opcodes `0x3B`
// (passthrough-write) and `0x3C` (passthrough-read) on sensor addresses
// `0x48..0x4B`. The "DCENT_OS-issues-BARE-0x10-SetVoltage" path was
// inherited from VNish/.139 research; live evidence proves bosminer
// engages the chip rail differently — through (likely) the Loki spoof's
// `0x83`/`0x86` opcodes (per `PHASE2B-APW12-PIC-PROTOCOL.md`) NOT through
// dsPIC `0x10` at all on `a lab unit`-class hardware.
//
// Hypothesis tested by this code: the LM75A polling sequence "warms" the
// dsPIC's I²C handler state machine so subsequent commands (in particular
// the Loki spoof's `0x83` SetVoltage) actually engage the chip rail.
//
// Frame format derived directly from the trace:
//   `[0x55, 0xAA, 0x06, CMD, SENSOR, FLAG, 0x00, CKSUM]`
//   where CMD ∈ {0x3B passthrough-write, 0x3C passthrough-read},
//         SENSOR ∈ {0x48, 0x49, 0x4A, 0x4B}, and
//         FLAG = 0x00 for 0x3B, 0x02 for 0x3C, and
//         CKSUM = (0x06 + CMD + SENSOR + FLAG + 0x00) & 0xFF
//                (matches the existing FRAMED RESET/START_APP checksum
//                 algorithm: LEN + CMD + sum(PAYLOAD)).
//
// Source citation:
// phase0-probe/wave38-bosminer-truth/bosminer-i2c0-slave20.txt` lines
// 33-203 (210 byte-level events grouped into 17 transactions).
//
// **Default-OFF behind `DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH=1`.**
// The Phase 0 orchestrator gates this further behind the
// `DCENT_AM2_STANDALONE_RE_FIX=1` umbrella + `a lab unit` fingerprint, so the
// fleet stays byte-identical when no env is set.

///  LM75A passthrough transaction count (4 sensors × ~4 reads
/// each + 1 trailing tx = 17 total). Matches the
/// `bosminer-i2c0-slave20.txt:33-203` transaction count exactly.
pub const LM75_PASSTHROUGH_TX_COUNT: usize = 17;

/// LM75A passthrough opcode: dsPIC passthrough-WRITE to sensor pointer.
/// Confirmed at `bosminer-i2c0-slave20.txt:36, 44, 58, 66, 76, 84, 98,
/// 106, 119, 129, 143, 152, 167, 176, 190, 199, 209`.
pub const LM75_PT_OPCODE_WRITE: u8 = 0x3B;

/// LM75A passthrough opcode: dsPIC passthrough-READ from sensor.
/// Confirmed at `bosminer-i2c0-slave20.txt:44, 66, 84, 106, 129, 152,
/// 176, 199` (the 0x3C variants in the trace).
pub const LM75_PT_OPCODE_READ: u8 = 0x3C;

/// LM75A frame length byte (LEN = 0x06 = 6 = CMD + SENSOR + FLAG + 0x00
/// + CKSUM as counted by bosminer's APW-style framer; matches every
/// `0x06` byte at trace offsets 35, 43, 57, 65, 75, 83, 97, 105, 118,
/// 128, 142, 151, 166, 175, 189, 198, 208).
pub const LM75_FRAME_LEN_BYTE: u8 = 0x06;

/// LM75A sensor I²C addresses on the `a lab unit` AM2 hashboard. Confirmed in
/// trace at offsets 37 (0x48), 59 (0x49), 77 (0x4A), 99 (0x4B), and the
/// round-2 repeats at 120/144/168/191.
pub const LM75_SENSOR_ADDRS: [u8; 4] = [0x48, 0x49, 0x4A, 0x4B];

/// Default per-byte sleep between WriteByteByByte bytes for the LM75A
/// passthrough chain. Bosminer's strace shows ~6.0 ms inter-byte gaps
/// at the kernel `i2c-dev` ioctl layer; our `WriteByteByByte` step
/// emits one ioctl per byte so the natural latency provides that gap.
/// No explicit `SleepMs` step needed between bytes within a frame.

///  Phase 0d-post (between dsPIC warmup and Loki SetVoltage):
/// inter-transaction sleep matches bosminer's ~6-12 ms cadence between
/// adjacent passthrough frames (kernel ioctl spacing; observed in
/// `bosminer-i2c0-slave20.txt` lines 33→42 +6ms, 42→55 +6ms, etc.).
pub const LM75_PASSTHROUGH_INTER_TX_MS: u64 = 6;

/// Build the byte-exact 8-byte LM75A passthrough frame:
/// `[0x55, 0xAA, 0x06, CMD, SENSOR, FLAG, 0x00, CKSUM]`
/// where CKSUM = (LEN + CMD + SENSOR + FLAG + 0x00) & 0xFF.
///
/// `flag = 0x00` for the passthrough-WRITE (0x3B) form.
/// `flag = 0x02` for the passthrough-READ (0x3C) form.
///
/// Source: every LM75A frame in `bosminer-i2c0-slave20.txt:33-203`
/// matches this 8-byte shape. The checksum algorithm is identical to
/// the existing FRAMED RESET/START_APP checksum (LEN + CMD + sum(PAYLOAD)).
pub fn build_lm75_passthrough_frame(cmd: u8, sensor: u8, flag: u8) -> [u8; 8] {
    let cksum = LM75_FRAME_LEN_BYTE
        .wrapping_add(cmd)
        .wrapping_add(sensor)
        .wrapping_add(flag)
        .wrapping_add(0x00);
    [
        0x55,
        0xAA,
        LM75_FRAME_LEN_BYTE,
        cmd,
        sensor,
        flag,
        0x00,
        cksum,
    ]
}

/// Build the 17-transaction LM75A passthrough warmup sequence as a
/// vector of `I2cTransactionStep` lists, one Vec per `i2c.transaction()`
/// call.
///
/// Order (matches `bosminer-i2c0-slave20.txt:33-203` byte-for-byte):
///
/// | Tx | Sensor | Direction | Frame                                |
/// |----|--------|-----------|--------------------------------------|
/// | 1  | 0x48   | 0x3B WR   | `[55 AA 06 3B 48 00 00 89]`         |
/// | 2  | 0x48   | 0x3C RD   | `[55 AA 06 3C 48 02 00 8C]`         |
/// | 3  | 0x49   | 0x3B WR   | `[55 AA 06 3B 49 00 00 8A]`         |
/// | 4  | 0x49   | 0x3C RD   | `[55 AA 06 3C 49 02 00 8D]`         |
/// | 5  | 0x4A   | 0x3B WR   | `[55 AA 06 3B 4A 00 00 8B]`         |
/// | 6  | 0x4A   | 0x3C RD   | `[55 AA 06 3C 4A 02 00 8E]`         |
/// | 7  | 0x4B   | 0x3B WR   | `[55 AA 06 3B 4B 00 00 8C]`         |
/// | 8  | 0x4B   | 0x3C RD   | `[55 AA 06 3C 4B 02 00 8F]`         |
/// | 9  | 0x48   | 0x3B WR   | `[55 AA 06 3B 48 00 00 89]` (round 2)|
/// | 10 | 0x48   | 0x3C RD   | `[55 AA 06 3C 48 02 00 8C]`         |
/// | 11 | 0x49   | 0x3B WR   | `[55 AA 06 3B 49 00 00 8A]`         |
/// | 12 | 0x49   | 0x3C RD   | `[55 AA 06 3C 49 02 00 8D]`         |
/// | 13 | 0x4A   | 0x3B WR   | `[55 AA 06 3B 4A 00 00 8B]`         |
/// | 14 | 0x4A   | 0x3C RD   | `[55 AA 06 3C 4A 02 00 8E]`         |
/// | 15 | 0x4B   | 0x3B WR   | `[55 AA 06 3B 4B 00 00 8C]`         |
/// | 16 | 0x4B   | 0x3C RD   | `[55 AA 06 3C 4B 02 00 8F]`         |
/// | 17 | 0x48   | 0x3B WR   | `[55 AA 06 3B 48 00 00 89]` (trail) |
///
/// Each transaction is `[SetTimeout, WriteByteByByte(frame), Read(N)]`
/// where N = 1 for 0x3B (ACK byte) and N = 6 for 0x3C (3C echo + 5
/// data bytes — bosminer's reads vary in length from 2 to 7 bytes; we
/// pick 6 as the upper-bound observed in the trace to ensure the dsPIC
/// has a chance to fully clock out its reply without TX-FIFO stall).
///
/// Read bytes are DISCARDED — we only care about the bus activity
/// warming the dsPIC's MSSP I²C handler state machine. The data is
/// not interpreted (we are not reading temperatures — the LM75A sensors
/// may not even be populated on `a lab unit`).
///
/// Note on the trace's "round 2" repetition: bosminer polls each sensor
/// twice in this window. We replay it byte-for-byte rather than collapse
/// to 8 + 1 because the hypothesis is that the FULL 17-tx volume warms
/// the dsPIC; truncating to 9 might miss the dsPIC state transition.
pub fn build_lm75_passthrough_transactions() -> Vec<Vec<I2cTransactionStep>> {
    let mut txs: Vec<Vec<I2cTransactionStep>> = Vec::with_capacity(LM75_PASSTHROUGH_TX_COUNT);

    // Helper closure to push a single LM75 transaction.
    let push_tx = |txs: &mut Vec<Vec<I2cTransactionStep>>, cmd: u8, sensor: u8, flag: u8| {
        let frame = build_lm75_passthrough_frame(cmd, sensor, flag);
        // Read budget: 1 byte for 0x3B (ACK), 6 bytes for 0x3C (3C echo
        // + up to 5 data bytes — upper bound from trace lines 44-54).
        let read_len = if cmd == LM75_PT_OPCODE_READ { 6 } else { 1 };
        txs.push(vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(frame.to_vec()),
            // Pre-Read settle: ~66 ms is the dsPIC's empirical ACK
            // prepare time ( finding from the same strace; lines
            // 17-18 / 22-23 / 28-29 in `bosminer-i2c0-slave20.txt`).
            // Apply the same floor here for safety — the framed 0x3B /
            // 0x3C opcodes go through the same MSSP ISR + parser as the
            // 0x07 RESET / 0x06 START_APP / 0x17 GET_VERSION opcodes, so
            // the dsPIC's ACK-prepare latency is identical.
            I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
            I2cTransactionStep::Read(read_len),
            // Inter-transaction settle ~6 ms to match bosminer cadence.
            I2cTransactionStep::SleepMs(LM75_PASSTHROUGH_INTER_TX_MS),
        ]);
    };

    // Round 1: 8 transactions (4 sensors × 2 directions each).
    for &sensor in &LM75_SENSOR_ADDRS {
        push_tx(&mut txs, LM75_PT_OPCODE_WRITE, sensor, 0x00); // 3B WR
        push_tx(&mut txs, LM75_PT_OPCODE_READ, sensor, 0x02); // 3C RD
    }
    // Round 2: 8 more transactions (same 4 sensors × 2 directions).
    for &sensor in &LM75_SENSOR_ADDRS {
        push_tx(&mut txs, LM75_PT_OPCODE_WRITE, sensor, 0x00); // 3B WR
        push_tx(&mut txs, LM75_PT_OPCODE_READ, sensor, 0x02); // 3C RD
    }
    // Trailing 17th tx: one more 0x3B WR to 0x48 (matches trace lines
    // 207-214 — bosminer issues this lone trailing write before moving
    // on to other dsPIC traffic; we replay it for byte-exact parity).
    push_tx(&mut txs, LM75_PT_OPCODE_WRITE, 0x48, 0x00);

    debug_assert_eq!(txs.len(), LM75_PASSTHROUGH_TX_COUNT);
    txs
}

/// ** opt-in env helper** for the LM75A passthrough warmup.
/// Default-OFF — when unset, the warmup is not emitted and the chain
/// behavior is byte-identical to the pre- path.
pub fn am2_dspic_lm75_passthrough_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

// ===========================================================================
//  Ghidra-RE Sensor-Only mode — DCENT_AM2_DSPIC_SENSOR_ONLY (2026-05-29)
// ===========================================================================
//
// **Ghidra static RE of `bosminer.bin` (2026-05-29).** Decisive finding
//: on the
// `a lab unit` AM2 Zynq unit bosminer engages the chip rail ENTIRELY on the Loki/PSU
// side (PWR_CONTROL gpio907 + the Loki/APW `0x83` SetVoltage-step over the
// uio17 bit-bang I²C window) and sends **ZERO dsPIC `0x10` (SetVoltage) and
// ZERO `0x15` (ENABLE_VOLTAGE)** to the per-board dsPIC. The dsPIC is used
// ONLY for LM75A sensor passthrough (`0x3B`/`0x3C`, sensors 0x48-0x4B) which
// WARMS the dsPIC's MSSP I²C slave FSM.
//
// dcentrald's bug on `a lab unit`: `cold_boot_init` SENDS dsPIC SetVoltage 0x10 +
// ENABLE 0x15 (the wrong subsystem) and only runs the LM75A passthrough on
// the SELECTED pic (0x20), not the effective-CHAIN pic (0x22). Hitting the
// cold dsPIC with SetVoltage/ENABLE opcodes its bootloader doesn't route →
// it echoes its FW byte (0x8A) to everything, the parser stays unwarmed, the
// Loki-side rail is interfered with, and chain enum returns 0.
//
// When `DCENT_AM2_DSPIC_SENSOR_ONLY=1` is set:
//
//   PART A — `DspicService::cold_boot_init_with_options` SKIPS the dsPIC
//            SetVoltage (0x10) + ENABLE_VOLTAGE (0x15) writes entirely
//            (cold_boot_init still completes Ok — GET_VERSION + the LM75A
//            passthrough warmup still run; chain enum is the real proof).
//   PART B — the s19j hybrid path ALSO runs the LM75A passthrough warmup on
//            the EFFECTIVE-CHAIN dsPIC addr (e.g. 0x22) so the chain dsPIC's
//            FSM gets warmed too (today it warms only 0x20).
//
// Default-OFF — when unset, the entire fleet (.79 / .129 / .135 / .109 / S9)
// is byte-identical to today: dsPIC SetVoltage/ENABLE still fire exactly as
// before, and the chain-pic LM75A warmup is not added.

/// **Ghidra-RE opt-in env helper** for dsPIC SENSOR-ONLY mode.
///
/// Default-OFF. When set to `"1"`, dcentrald treats the per-board dsPIC as a
/// LM75A sensor-passthrough device ONLY on `a lab unit`-class AM2 hardware: it skips
/// the dsPIC SetVoltage/ENABLE_VOLTAGE writes in `cold_boot_init` (the chip
/// rail is engaged Loki-side per the bosminer RE) and additionally warms the
/// effective-chain dsPIC's I²C FSM via the LM75A passthrough.
///
/// Source: .
pub fn am2_dspic_sensor_only_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_SENSOR_ONLY")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// **Post-JUMP heartbeat keep-alive opt-in env helper** (2026-06-07,
/// `a lab unit` standalone cold-engage). Default-OFF.
///
/// LIVE-confirmed blocker (`a lab unit` standalone, binary sha 8a293963): the
/// clean warmup chain transitions cold dsPIC 0x20 to fw=0x89 APP mode
/// (`8×flush → RESET [07] → JUMP [06] → GET_VERSION fw=0x89`), but by the
/// time `cold_boot_init` reaches the ENABLE (0x15) the chip has **drifted
/// back to fw=0x82 BOOTLOADER** — the ENABLE returns `ack_cmd=0x82` (the
/// bootloader FW byte) instead of a real `[0x15, 0x00/0x01]` ACK on both
/// the primary and alternate ENABLE forms, so the per-board chip rail
/// never energizes and chain enum returns 0. The cause is that nothing
/// services the app-mode watchdog between GET_VERSION and ENABLE; a
/// serviced dsPIC stays in app (contrast 0x22, which is already serviced
/// and gets a real ENABLE ACK).
///
/// When set to `"1"`, `DspicService::cold_boot_init_with_options` keeps the
/// cold-engaged FRAMED (fw=0x89) dsPIC serviced with framed `0x16`
/// heartbeats through the SetVoltage(0x10) → ENABLE(0x15) sequence so it
/// stays in app mode. The keep-alive uses the existing
/// `DspicService::send_heartbeat` framed frame (`[55 AA 04 16 00 1A]`) on
/// the same single-owner `I2cServiceHandle` — no new opcode, no new bus
/// owner. It is purely ADDITIVE and only fires for the framed protocol
/// (`!use_bare_protocol`); the bare fw=0x82 path has no fw=0x89→0x82 drift
/// to defend against and is untouched.
///
/// **Default-OFF + caller-fingerprinted.** The env is read at the
/// `s19j_hybrid_mining.rs` Phase 3 call site and AND-gated with the `a lab unit`
/// hardware fingerprint there (the platform fingerprint isn't visible from
/// this platform-agnostic crate), then handed to the service via
/// `DspicService::set_postjump_heartbeat_keepalive`. With the env unset OR
/// the fingerprint not matching, the service field stays `false` and
/// `cold_boot_init_with_options` is byte-identical to today for the entire
/// fleet/handoff/legacy paths.
///
/// Source:
/// fallback rung 3 ("Heartbeat keep-alive immediately post-JUMP").
pub fn am2_dspic_postjump_heartbeat_keepalive_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// **Post-JUMP keep-alive heartbeat interval (ms)** (2026-06-07, `a lab unit`
/// standalone cold-engage). Default `300`.
///
/// When the post-JUMP keep-alive is active (env above + `a lab unit` fingerprint),
/// `DspicService::cold_boot_init_with_options` services the cold-engaged
/// fw=0x89 dsPIC with a framed `0x16` heartbeat at most this many ms apart —
/// both interleaved between the ~290 ms-per-sensor LM75A pre-voltage reads
/// (the ~1.2 s un-serviced gap where the chip was drifting 0x89→0x82 LIVE on
/// `a lab unit`) and chunked across the SetVoltage/ENABLE settle sleeps. The default
/// `300` is well under the live-observed ~1.2 s app-mode-hold window, so the
/// chip is never un-serviced long enough to drift back to fw=0x82 bootloader.
///
/// Tunable via `DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS`; parsed values are
/// clamped to `[50, 1000]` ms to keep a sane heartbeat cadence (a value of 0
/// or garbage falls back to the `300` default rather than busy-looping). This
/// helper is ONLY consulted when the keep-alive is already active, so the
/// default-OFF byte-identical contract is unaffected.
pub fn am2_dspic_keepalive_interval_ms() -> u64 {
    std::env::var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .map(|v| v.clamp(50, 1000))
        .unwrap_or(300)
}

/// **Re-JUMP-before-ENABLE opt-in env helper** (2026-06-07, `a lab unit` standalone
/// cold-engage). Default-OFF.
///
/// LIVE-confirmed SOLE remaining blocker (`a lab unit` standalone, binary c0c0f977):
/// the cold-engage is SOLVED (Fix A + Fix B) — cold dsPIC 0x20 reliably reaches
/// fw=0x89 APP mode in the warmup (`GET_VERSION OK` at ~T+6.0 s). BUT the chip
/// then **drifts 0x89 → 0x82 over the ~8 s** between the warmup GET_VERSION
/// (T+6.0 s) and `cold_boot_init`'s ENABLE (T+14.4 s) — the s19j_hybrid
/// orchestration gap. By the ENABLE the chip is in BOOTLOADER and echoes
/// `[82, 82]` (`firmware_echo_mismatch`) on BOTH the 7-byte and 6-byte ENABLE
/// forms, so the per-board chip rail never energizes and chain enum returns 0.
/// The continuous framed-`0x16` keep-alive (`POSTJUMP_HEARTBEAT_KEEPALIVE`)
/// FAILED to hold app mode and does NOT transition 0x82 → 0x89 — heartbeats are
/// the wrong tool; the chip must be RE-JUMPED to 0x89 right before the voltage
/// commands.
///
/// When set to `"1"`, `DspicService::cold_boot_init_with_options` reads
/// GET_VERSION immediately before SetVoltage(0x10); if the chip has drifted
/// back to fw=0x82 it runs a bounded **`flush → framed-JUMP` (NO RESET)**
/// re-verify via [`am2_pic_jump_only_reverify`] to re-transition it to fw=0x89,
/// then SetVoltage → ENABLE run back-to-back (~100-200 ms wall-time, far under
/// the ~8 s drift window) so the chip is in APP mode when the ENABLE lands. The
/// re-JUMP reuses the existing single-owner `I2cServiceHandle`, NEVER issues a
/// RESET (the chip is a cold-0x82-class bootloader — JUMP-only is the safe
/// idempotent transition; a RESET here is the destructive-downgrade class), and
/// only fires for the framed protocol (`!use_bare_protocol`). It is purely
/// ADDITIVE.
///
/// **Default-OFF + caller-fingerprinted.** The env is read at the
/// `s19j_hybrid_mining.rs` Phase 3 call site and AND-gated with the `a lab unit`
/// hardware fingerprint there (the platform fingerprint isn't visible from this
/// platform-agnostic crate), then handed to the service via
/// `DspicService::set_rejump_before_enable`. With the env unset OR the
/// fingerprint not matching, the service field stays `false` and
/// `cold_boot_init_with_options` is byte-identical to today for the entire
/// fleet/handoff/legacy paths (no extra GET_VERSION/JUMP).
///
/// Source:
/// fallback rung 3 (re-JUMP immediately pre-ENABLE — supersedes the rung-3
/// keep-alive).
pub fn am2_dspic_rejump_before_enable_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// **Skip-SetVoltage-keep-ENABLE opt-in env helper** (2026-06-07, `a lab unit`
/// standalone cold-engage). Default-OFF.
///
/// PROVEN root cause
/// (
/// commit fc4eef92): the bosminer true-cold strace contains **ZERO** SetVoltage
/// (`0x10`) frames to the `a lab unit` dsPIC across 662k lines. The chip-rail voltage is
/// set on the **APW PSU** (PMBus), NOT the per-board dsPIC — the dsPIC is
/// sensor-passthrough + ENABLE only. DCENT's `cold_boot_init_with_options` sends
/// a `0x10` frame `[55 AA 04 10 DAC SUM]` between GET_VERSION(0x89) and ENABLE,
/// and that `0x10` faults the cold-engaged fw=0x89 app back to the fw=0x82
/// bootloader → the immediately-following ENABLE reads `[82, 82]` (LIVE TEST 5:
/// re-JUMP→0x89, "SetVoltage applied", then ENABLE→0x82). bosminer's ENABLE frame
/// is already byte-identical to DCENT's, so the ENABLE is correct — only the
/// `0x10` must go.
///
/// When set to `"1"`, the `s19j_hybrid_mining.rs` Phase 3 call site tells
/// `DspicService::cold_boot_init_with_options` (via
/// `set_skip_setvoltage_keep_enable`) to SKIP the `0x10` SetVoltage entirely and
/// go GET_VERSION(0x89) → [re-JUMP if drifted] → ENABLE(0x15) directly, exactly
/// like bosminer. The chip rail energizes via the **unchanged** ENABLE at the
/// dsPIC's power-on default voltage — bosminer-proven safe (bosminer never
/// SetVoltages and mines fine). It is purely SUBTRACTIVE (it OMITS the `0x10`
/// frame) and only fires for the framed protocol (`!use_bare_protocol`); the
/// proven BARE (fw=0x82) `a lab unit`/ cold path — where SetVoltage ACKs and is
/// load-bearing — is untouched.
///
/// **Do NOT reuse `DCENT_AM2_DSPIC_SENSOR_ONLY`** — that gate skips BOTH `0x10`
/// SetVoltage AND `0x15` ENABLE, and the live cold strace proves bosminer DOES
/// send `0x15` ENABLE to the dsPIC (only `0x10` is PSU-side). This gate is the
/// corrected half: skip `0x10`, keep `0x15`.
///
/// **Default-OFF + caller-fingerprinted.** The env is read at the
/// `s19j_hybrid_mining.rs` Phase 3 call site and AND-gated with the `a lab unit`
/// hardware fingerprint there (the platform fingerprint isn't visible from this
/// platform-agnostic crate), then handed to the service via
/// `DspicService::set_skip_setvoltage_keep_enable`. With the env unset OR the
/// fingerprint not matching, the service field stays `false` and
/// `cold_boot_init_with_options` is byte-identical to today for the entire
/// fleet/handoff/legacy paths (the `0x10` SetVoltage still fires).
///
/// Source:
/// (Fix A — "skip the dsPIC SetVoltage (0x10) but KEEP the ENABLE (0x15)").
pub fn am2_dspic_skip_setvoltage_keep_enable_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// **Bosminer-minimal ENABLE opt-in env helper** (2026-06-07, `a lab unit` standalone
/// cold-engage). Default-OFF. This is the CONSOLIDATED fix for the
/// LIVE-confirmed ENABLE `[82, 82]` drift blocker.
///
/// The env-by-env approach (skip SetVoltage, unset the `0x16` keep-alive, unset
/// the re-JUMP) FAILED to fully clean the GET_VERSION(0x89)→ENABLE window because
/// the individual pre-ENABLE commands come from MULTIPLE code paths: the parser
/// flush + sanity `0x16` heartbeat at the top of `cold_boot_init_with_options`,
/// the `0x30` LM75A pre-voltage read, the `rejump_to_app_mode_if_drifted`
/// re-verify (a second GET_VERSION + `0x06` JUMP), and the `0x10` SetVoltage.
/// LIVE TEST 6 (after unsetting the `0x16` keep-alive + the `0x06` re-JUMP +
/// skipping the `0x10` SetVoltage) STILL showed, between the warmup
/// GET_VERSION(0x89) and the ENABLE, batches of LM75A reads AND a stray
/// `GET_VERSION + JUMP(0x06)` — then ENABLE → `[82, 82]`.
///
/// PROVEN cause
/// (
/// commit fc4eef92): in the bosminer true-cold strace, between GET_VERSION(0x89)
/// and the ENABLE bosminer sends the `a lab unit` dsPIC **NOTHING** that can fault the
/// cold-engaged fw=0x89 app back to the fw=0x82 bootloader — no SetVoltage
/// (`0x10`), no `0x16` heartbeat, no re-JUMP (`0x06`), no second GET_VERSION. Its
/// only in-window traffic is `0x3B`/`0x3C` sensor passthrough, and even fully
/// un-serviced the chip stays fw=0x89 for >1.4 s. So the fix is to make DCENT's
/// window bosminer-minimal: GET_VERSION(0x89) → ENABLE(0x15), nothing in between.
///
/// When set to `"1"`, the `s19j_hybrid_mining.rs` Phase 3 call site tells
/// `DspicService::cold_boot_init_with_options` (via
/// `set_bosminer_minimal_enable`) to SKIP ALL of: the parser flush, the sanity
/// `0x16` heartbeat, the `0x30` LM75A pre-voltage read, any second
/// GET_VERSION/JUMP/re-verify, the `0x10` SetVoltage, and every keep-alive tick —
/// so the ONLY dsPIC wire traffic between the confirmed-fw=0x89 GET_VERSION (run
/// in the external warmup) and the ENABLE is the **byte-identical** ENABLE
/// (`0x15`) itself. The chip rail energizes via that unchanged ENABLE at the
/// dsPIC's power-on default voltage — bosminer-proven safe (bosminer never
/// SetVoltages and mines fine; the `a lab unit` input rail is the APW3 12.8 V
/// `psu_override`). This gate SUPERSEDES/implies `skip_setvoltage_keep_enable`
/// (it omits the `0x10` too) and renders the `0x16` keep-alive / `0x06` re-JUMP
/// moot (it omits both). Temps can be read AFTER the ENABLE if needed.
///
/// Only fires for the framed (fw=0x89) protocol; the proven BARE (fw=0x82)
/// `a lab unit`/ cold path — where SetVoltage/LM75-skip are load-bearing — is
/// left untouched (it falls through to the unchanged legacy path).
///
/// **Default-OFF + caller-fingerprinted.** The env is read at the
/// `s19j_hybrid_mining.rs` Phase 3 call site and AND-gated with the `a lab unit`
/// hardware fingerprint there (the platform fingerprint isn't visible from this
/// platform-agnostic crate), then handed to the service via
/// `DspicService::set_bosminer_minimal_enable`. With the env unset OR the
/// fingerprint not matching, the service field stays `false` and
/// `cold_boot_init_with_options` is byte-identical to today for the entire
/// fleet/handoff/legacy paths.
///
/// Source:
/// (the consolidated bosminer-minimal GET_VERSION→ENABLE window).
pub fn am2_dspic_bosminer_minimal_enable_enabled() -> bool {
    std::env::var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// ** (2026-05-25)** — emit the 17-transaction LM75A
/// passthrough sequence that bosminer issues to dsPIC slave 0x20
/// (`bosminer-i2c0-slave20.txt:33-203`). The hypothesis (per
/// ):
/// this LM75A polling sequence "warms" the dsPIC's I²C handler state
/// machine so subsequent commands (in particular the Loki spoof's
/// `0x83`/`0x86` SetVoltage opcodes — see `PHASE2B-APW12-PIC-PROTOCOL.md`)
/// actually engage the chip rail on cold-boot `a lab unit`-class hardware.
///
/// Read bytes are DISCARDED (we don't interpret LM75A temperatures).
/// Bus activity alone is the hypothesis under test.
///
/// ## Safety invariants
///
/// - **EEPROM denylist refusal** (`is_eeprom_denylist_addr`) — refuses
///   addresses in `0x50..=0x57` fail-closed before any wire byte.
/// - **Per-byte writes** via `WriteByteByByte` (matches bosminer
///   transport — one ioctl per byte).
/// - **Read drain** after every passthrough so the dsPIC TX FIFO
///   doesn't hold stale bytes that would corrupt the next read.
/// - **Best-effort per-transaction** — a single LM75A read failure is
///   non-fatal (the sensors may not be populated on `a lab unit`); the
///   function logs at WARN and continues. Only a hard bus failure on
///   the dsPIC address itself escalates to `Err`.
///
/// ## Errors
///
/// - `AsicError::Pic` if `addr` is in `0x50..=0x57` (EEPROM denylist).
/// - `AsicError::Pic` wrapping the underlying I²C error if MORE THAN
///   HALF the transactions fail (indicates the dsPIC itself is wedged,
///   not just absent sensors).
pub fn am2_dspic_lm75_passthrough_warmup(i2c: &I2cServiceHandle, addr: u8) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "Wave-55f LM75A passthrough warmup refused: addr 0x{:02X} is in \
                 the AM2 hashboard EEPROM write-denylist range (0x50..=0x57); the \
                 LM75A passthrough writes to a dsPIC address (0x20/0x21/0x22). \
                 Refusing fail-closed before any wire byte \
.",
                addr
            ),
        });
    }

    let transactions = build_lm75_passthrough_transactions();
    let total = transactions.len();
    let mut ok_count = 0usize;
    let mut last_err: Option<dcentrald_hal::HalError> = None;

    tracing::info!(
        target: "wave55f_lm75_passthrough_warmup",
        addr = format_args!("0x{:02X}", addr),
        total_transactions = total,
        sensors = format_args!("{:02X?}", LM75_SENSOR_ADDRS),
        "Wave-55f: emitting LM75A passthrough warmup chain — \
         bosminer-faithful dsPIC state-machine wake-up. Hypothesis: this \
         lets subsequent Loki SetVoltage(0x83) actually engage chip rail. \
         See PHASE2C-DSPIC-RAIL-FAILURE-RE.md."
    );

    for (idx, steps) in transactions.into_iter().enumerate() {
        // Decode the cmd/sensor/flag from the frame bytes inside the
        // transaction's WriteByteByByte step for the trace log.
        let (cmd_byte, sensor_byte, flag_byte) = steps
            .iter()
            .find_map(|s| {
                if let I2cTransactionStep::WriteByteByByte(b) = s {
                    if b.len() == 8 {
                        return Some((b[3], b[4], b[5]));
                    }
                }
                None
            })
            .unwrap_or((0xFF, 0xFF, 0xFF));

        tracing::trace!(
            target: "wave55f_lm75_passthrough_warmup",
            addr = format_args!("0x{:02X}", addr),
            tx_idx = idx + 1,
            total,
            cmd = format_args!("0x{:02X}", cmd_byte),
            sensor = format_args!("0x{:02X}", sensor_byte),
            flag = format_args!("0x{:02X}", flag_byte),
            "Wave-55f: LM75 passthrough tx {}/{} (cmd=0x{:02X} sensor=0x{:02X})",
            idx + 1, total, cmd_byte, sensor_byte
        );

        match i2c.transaction(addr, steps) {
            Ok(_reads) => {
                ok_count += 1;
            }
            Err(e) => {
                tracing::warn!(
                    target: "wave55f_lm75_passthrough_warmup",
                    addr = format_args!("0x{:02X}", addr),
                    tx_idx = idx + 1,
                    total,
                    cmd = format_args!("0x{:02X}", cmd_byte),
                    sensor = format_args!("0x{:02X}", sensor_byte),
                    error = %e,
                    "Wave-55f: LM75 passthrough tx {}/{} failed (non-fatal — \
                     sensors may not be populated on .25; continuing)",
                    idx + 1, total
                );
                last_err = Some(e);
            }
        }
    }

    let success_rate_pct = (ok_count * 100) / total.max(1);
    tracing::info!(
        target: "wave55f_lm75_passthrough_warmup",
        addr = format_args!("0x{:02X}", addr),
        total,
        ok_count,
        success_rate_pct,
        "Wave-55f: LM75A passthrough warmup COMPLETE — {}/{} transactions OK ({}%)",
        ok_count, total, success_rate_pct
    );

    // Escalate to error only if more than HALF failed (dsPIC itself
    // likely wedged). A few failures from absent/unresponsive LM75A
    // sensors on `a lab unit` is expected and non-fatal.
    if ok_count * 2 < total {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "Wave-55f LM75A passthrough warmup: only {}/{} transactions \
                 succeeded ({}%) — dsPIC at 0x{:02X} is likely wedged. \
                 Last error: {}",
                ok_count,
                total,
                success_rate_pct,
                addr,
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "no error captured".to_string())
            ),
        });
    }

    Ok(())
}

// ===========================================================================
//  Rung 2 — bounded JUMP-only re-verify (2026-06-07, `a lab unit` standalone)
// ===========================================================================
//
// LIVE evidence (`a lab unit` standalone, after commit 0a1bfa5a clean-path + 2-read
// ack drain + commit 8a9113b8 heartbeat keep-alive): the cold dsPIC 0x20
// REACHED fw=0x89 once (run 3), proving the `flush → RESET → JUMP → GET_VER`
// sequence works — but the JUMP→0x89 transition is INTERMITTENT. Subsequent
// cold-engages (incl. a fresh AC-cycle) read fw=0x82 (the JUMP didn't
// transition, or the chip drifted back to the bootloader before GET_VERSION).
//
// The documented fix is `MORNING-RUNBOOK.md` fallback rung 2 ("Bounded
// JUMP-only re-verify"): after the RESET→JUMP warmup, read GET_VER; if it is
// still 0x82, re-issue `flush → JUMP` ONLY (idempotent — **NEVER a second
// RESET**) up to N times before failing. SAFETY invariant (bible): never
// abandon a chip in 0x82 after a RESET without a confirmed JUMP-to-0x89; an
// abandoned RESET landing in 0x82 IS the destructive/downgrade case, whereas
// a JUMP-only retry is the safe recovery (and the chip was cold-0x82 to begin
// with, so re-JUMPing is non-destructive).
//
// Default-OFF + `a lab unit`-fingerprinted: the re-verify loop only runs when the
// caller in `s19j_hybrid_mining.rs` reads `am2_dspic_jump_reverify_max() > 0`
// AND the `a lab unit` hardware fingerprint matches. With the env unset, the loop is
// never entered and the fleet/handoff/legacy paths are byte-identical.
//
// Source:
// fallback rung 2.

/// **Bounded JUMP-only re-verify opt-in env helper** (2026-06-07, `a lab unit`
/// standalone cold-engage). Default-DISABLED.
///
/// Reads `DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX` as an unsigned integer — the
/// maximum number of `flush → framed-JUMP` re-verify attempts to make when
/// the cold-engage warmup leaves the dsPIC in fw=0x82 (bootloader) instead of
/// fw=0x89 (app). Returns `0` when the env is unset, empty, `"0"`, or
/// unparseable — `0` means the re-verify loop is DISABLED and the cold-engage
/// path is byte-identical to today's behavior.
///
/// The caller (`s19j_hybrid_mining.rs` Phase-1 fw determination) AND-gates this
/// with the `a lab unit` hardware fingerprint (the platform fingerprint is not visible
/// from this platform-agnostic crate) and clamps the returned value to a small
/// sane ceiling. Recommended value: `6` (set by
/// `run_wave56_25_STANDALONE_MINING.sh`), conservatively within the documented
/// 4–6 range. NEVER issues a second RESET — see
/// [`build_jump_only_reverify_transactions`] / [`am2_pic_jump_only_reverify`].
///
/// Source:
/// fallback rung 2.
pub fn am2_dspic_jump_reverify_max() -> u32 {
    std::env::var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Build the JUMP-only re-verify transaction list (rung 2).
///
/// Each top-level `Vec<I2cTransactionStep>` is **one** call to
/// `I2cServiceHandle::transaction(addr, ...)`. The order is:
///
/// 1..=`STRACE_SYNC_HEARTBEAT_COUNT`. Per-byte `0x00` sync heartbeat
///    (the parser flush — identical to the warmup chain).
/// last. Framed JUMP (`STRACE_START_APP_FRAME_FRAMED` = `[55 AA 04 06 00 0A]`)
///    with the  70 ms pre-Read settle + the 2-separate-reads ACK drain
///    (echo, then 0x01 status, `STRACE_INTER_ACK_READ_MS` apart) + the 500 ms
///    post-JUMP settle (`STRACE_START_APP_SETTLE_MS`).
///
/// This is BYTE-IDENTICAL to the flush + START_APP step of
/// [`build_strace_derived_prelude_transactions`] — it deliberately OMITS the
/// framed RESET (and the optional READ-CONFIG-LATCH). There is NO RESET in this
/// chain by construction: rung 2 is JUMP-only, the safe idempotent recovery for
/// a chip already cold-0x82.
///
/// Total = `STRACE_SYNC_HEARTBEAT_COUNT + 1` transactions.
pub fn build_jump_only_reverify_transactions() -> Vec<Vec<I2cTransactionStep>> {
    let mut txs: Vec<Vec<I2cTransactionStep>> = Vec::with_capacity(STRACE_SYNC_HEARTBEAT_COUNT + 1);

    // Flush: per-byte 0x00 sync heartbeats (separate transactions, exactly
    // like the warmup chain — each one's kernel ioctl creates the natural
    // ~6 ms inter-byte gap the dsPIC bootloader sync FSM relies on).
    for _ in 0..STRACE_SYNC_HEARTBEAT_COUNT {
        txs.push(vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(vec![0x00]),
        ]);
    }

    // Framed JUMP (START_APP) — byte-identical to the warmup's START_APP step.
    //  70 ms pre-Read settle + drain the framed echo+ack as TWO separate
    // 1-byte reads (cmd echo, then 0x01 status) STRACE_INTER_ACK_READ_MS apart
    // + 500 ms post-JUMP settle. NO RESET — JUMP-only (rung 2 safety invariant).
    txs.push(vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(STRACE_START_APP_FRAME_FRAMED.to_vec()),
        I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_INTER_ACK_READ_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_START_APP_SETTLE_MS),
    ]);

    debug_assert_eq!(txs.len(), STRACE_SYNC_HEARTBEAT_COUNT + 1);
    txs
}

/// Emit ONE bounded JUMP-only re-verify pass (rung 2) on `addr`.
///
/// Sends the flush (`STRACE_SYNC_HEARTBEAT_COUNT × 0x00`) then the framed JUMP
/// (`STRACE_START_APP_FRAME_FRAMED`, with the same 2-separate-reads ACK drain
/// and post-JUMP settle as the warmup). **NEVER issues a RESET.** The caller
/// re-reads GET_VERSION after this returns and repeats up to N times until the
/// dsPIC reports fw=0x89.
///
/// All other invariants match the warmup helpers:
/// - **EEPROM denylist refusal** (`is_eeprom_denylist_addr`) — refuses
///   addresses in `0x50..=0x57` fail-closed before any wire byte.
/// - **Per-byte writes** via `WriteByteByByte` (matches bosminer transport).
/// - **Single I²C owner** — every transaction goes through the shared
///   `I2cServiceHandle` (the same handle the warmup + cold_boot_init use).
/// - **No precondition fw probe** — unlike `am2_pic_reset_and_start_app_strace_derived`
///   (which probes to avoid re-RESETing an app-mode chip), there is no RESET
///   here, so the dangerous "downgrade an app-mode chip" case cannot occur; the
///   caller has already determined fw=0x82, so re-JUMPing is the intended action.
///
/// ## Errors
///
/// - `AsicError::Pic` if `addr` is in `0x50..=0x57` (EEPROM denylist).
/// - `AsicError::Pic` wrapping the underlying I²C error if any transaction
///   fails after the per-step retry budget is exhausted. The caller treats a
///   returned `Err` as non-fatal (logs + continues to the next attempt or to
///   the existing fail-closed path).
pub fn am2_pic_jump_only_reverify(i2c: &I2cServiceHandle, addr: u8) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "JUMP-only re-verify refused: addr 0x{:02X} is in the AM2 hashboard \
                 EEPROM write-denylist range (0x50..=0x57); the re-verify writes only \
                 to dsPIC addresses (0x20/0x21/0x22). Refusing fail-closed before any \
                 wire byte.",
                addr
            ),
        });
    }

    let transactions = build_jump_only_reverify_transactions();
    for (idx, steps) in transactions.into_iter().enumerate() {
        let label = if idx < STRACE_SYNC_HEARTBEAT_COUNT {
            format!(
                "reverify-sync-heartbeat[{}/{}]",
                idx + 1,
                STRACE_SYNC_HEARTBEAT_COUNT
            )
        } else {
            "reverify-FRAMED_JUMP[6B]+ACK+500ms".to_string()
        };

        let mut last_err: Option<dcentrald_hal::HalError> = None;
        let mut succeeded = false;
        for attempt in 1..=PRELUDE_TRANSACTION_RETRY_BUDGET.max(1) {
            match i2c.transaction(addr, steps.clone()) {
                Ok(reads) => {
                    let ack: Vec<u8> = reads.iter().flatten().copied().collect();
                    tracing::info!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label.as_str(),
                        attempt,
                        ack = format_args!("{:02X?}", ack),
                        "JUMP-only re-verify (rung 2): prelude ACK bytes (real JUMP ACK = \
                         [06, 01]; all-fw-byte echo = frame not recognized). NO RESET issued."
                    );
                    succeeded = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label.as_str(),
                        attempt,
                        error = %e,
                        "JUMP-only re-verify (rung 2): transaction failed; clean retry"
                    );
                    last_err = Some(e);
                }
            }
        }
        if !succeeded {
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "JUMP-only re-verify step {} ({}) failed after {} attempt(s): {}",
                    idx,
                    label,
                    PRELUDE_TRANSACTION_RETRY_BUDGET.max(1),
                    last_err
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "no error captured".to_string()),
                ),
            });
        }
    }

    tracing::info!(
        addr = format_args!("0x{:02X}", addr),
        sync_heartbeats = STRACE_SYNC_HEARTBEAT_COUNT,
        jump_settle_ms = STRACE_START_APP_SETTLE_MS,
        "JUMP-only re-verify (rung 2): flush + framed JUMP[55 AA 04 06 00 0A] re-issued \
         (NO RESET). Caller re-reads GET_VERSION to check for fw=0x89."
    );

    Ok(())
}

// ===========================================================================
//  Rung 2 (RESET→JUMP variant) — full flush→RESET→JUMP re-verify with a
//  configurable longer post-RESET dwell (2026-06-07, `a lab unit` standalone LIVE TEST 2)
// ===========================================================================
//
// LIVE TEST 2: the
// cold `a lab unit` selected-chain dsPIC 0x20 reached fw=0x89 standalone EXACTLY ONCE
// (run 3) — and that success was on a *warm* chip after TWO prior full
// RESET→JUMP cycles (runs 1-2). Fresh-cold (run 5, AC-cycled) and degraded
// (runs 4, 6) chips all read fw=0x82. Crucially, the rung-2 JUMP-only re-verify
// (`am2_pic_jump_only_reverify`, ×6) did NOT transition the chip — all 6
// attempts stayed fw=0x82 (run 6). So re-JUMPing alone does NOT prime the chip.
// The strongest hypothesis (SESSION.md §"NEXT-SESSION reliability frontier" #1)
// is that the chip needs MULTIPLE FULL RESET→JUMP cycles to transition, because
// the only 0x89 ever observed was effectively the 3rd RESET→JUMP cycle, and the
// JUMP-only re-verify can't prime because it omits the RESET.
//
// This helper does the full `flush → framed RESET → (longer) post-RESET dwell →
// framed JUMP → post-JUMP settle` so the caller can run N full RESET→JUMP cycles
// in one run (no AC-cycle between attempts — the chip degrades across AC cycles,
// so converging within one run is the reliability goal). The post-RESET dwell is
// configurable (`DCENT_AM2_DSPIC_RESET_DWELL_MS`, default
// [`RESET_JUMP_REVERIFY_DEFAULT_DWELL_MS`] = 1000 ms — the single-board test
// jig's ~1 s value; the warmup's `STRACE_RESET_TO_START_APP_DELAY_MS` = 500 ms
// may be too short to fully reset the cold chip into the JUMP-able bootloader,
// per SESSION.md §"reliability frontier" rung 1). Everything else is
// BYTE-IDENTICAL to the proven warmup RESET + JUMP steps (same 8×0x00 flush,
// same framed RESET/JUMP opcodes, same  70 ms pre-Read settle, same
// 2-separate-reads ACK drain, same 500 ms post-JUMP settle).
//
// ## SAFETY — why re-RESET during bring-up is NON-destructive here
//
// The bible's "never abandon a chip in 0x82 after a RESET" invariant (and the
// rung-2 JUMP-only "NEVER a 2nd RESET" comment) is about not DOWNGRADING a
// *working fw=0x89* chip back to 0x82. Our chip is **cold-0x82 to begin with**
// — that IS the bootloader state — so cycling RESET→JUMP to bring it UP to 0x89
// is non-destructive: an AC-cycle resets it the same way, and the caller only
// enters this path when GET_VERSION already read fw=0x82 (the bootloader). The
// runbook rung-2 "never a 2nd RESET" was written for the *jumped-then-fell-back*
// case (clean `[07 01]`/`[06 01]` ACKs but GET_VER=0x82), which is a DIFFERENT
// failure than our never-transitions case. See SESSION.md §"NEXT-SESSION
// reliability frontier" #1 for the reconciliation.
//
// Default-OFF + `a lab unit`-fingerprinted: only runs when the caller reads
// `am2_dspic_reset_jump_reverify_max() > 0` AND the `a lab unit` hardware fingerprint
// matches AND the cold-engage left the chip in fw=0x82. With the env unset, the
// loop is never entered and the fleet/handoff/legacy paths are byte-identical.
// Takes precedence over the JUMP-only re-verify at the call site when both envs
// are set (RESET→JUMP is the stronger lever per LIVE TEST 2).
//
// Source: .

/// Default post-RESET dwell for the RESET→JUMP re-verify (`a lab unit` standalone).
///
/// The single-board test jig holds ~1 s after RESET before JUMP; the warmup's
/// `STRACE_RESET_TO_START_APP_DELAY_MS` = 500 ms may be too short to fully reset
/// the cold chip into the JUMP-able bootloader (SESSION.md §"reliability
/// frontier" rung 1). 1000 ms is the conservative jig value.
pub const RESET_JUMP_REVERIFY_DEFAULT_DWELL_MS: u64 = 1000;

/// **RESET→JUMP re-verify post-RESET dwell env helper** (2026-06-07, `a lab unit`
/// standalone). Reads `DCENT_AM2_DSPIC_RESET_DWELL_MS` as a millisecond integer;
/// defaults to [`RESET_JUMP_REVERIFY_DEFAULT_DWELL_MS`] (1000) when unset, empty,
/// or unparseable. Clamped to `[100, 5000]` ms so a fat-fingered value can't
/// under-dwell below the chip's reset window or stall the bring-up for minutes
/// (the dwell holds the single-owner I²C service queue). Only consulted on the
/// `a lab unit`-fingerprinted RESET→JUMP path; the fleet/handoff/legacy paths never call
/// it.
pub fn am2_dspic_reset_dwell_ms() -> u64 {
    std::env::var("DCENT_AM2_DSPIC_RESET_DWELL_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(RESET_JUMP_REVERIFY_DEFAULT_DWELL_MS)
        .clamp(100, 5000)
}

/// **RESET→JUMP re-verify count env helper** (2026-06-07, `a lab unit` standalone
/// LIVE TEST 2). Default-DISABLED.
///
/// Reads `DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX` as an unsigned integer — the
/// maximum number of full `flush → RESET → (dwell) → JUMP` re-verify cycles to
/// run when the cold-engage warmup leaves the dsPIC in fw=0x82 (bootloader)
/// instead of fw=0x89 (app). Returns `0` when the env is unset, empty, `"0"`, or
/// unparseable — `0` means the RESET→JUMP re-verify loop is DISABLED and the
/// cold-engage path is byte-identical to today's behavior.
///
/// Takes PRECEDENCE over the JUMP-only re-verify (`am2_dspic_jump_reverify_max`)
/// at the `s19j_hybrid_mining.rs` call site when both envs are set (the full
/// RESET→JUMP cycle is the stronger lever per LIVE TEST 2 — the JUMP-only
/// re-verify ×6 did not transition the chip). The caller AND-gates this with the
/// `a lab unit` hardware fingerprint (the platform fingerprint is not visible from this
/// platform-agnostic crate) and clamps the returned value to a small sane
/// ceiling. Recommended value: `4` (set by `run_wave56_25_STANDALONE_MINING.sh`).
/// Unlike the JUMP-only re-verify, this DOES issue a RESET each cycle — which is
/// SAFE here because the chip is cold-0x82 to begin with (see the module-section
/// comment above for the bible-invariant reconciliation).
///
/// Source: .
pub fn am2_dspic_reset_jump_reverify_max() -> u32 {
    std::env::var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Build the full RESET→JUMP re-verify transaction list (rung 2, RESET→JUMP
/// variant) with a configurable post-RESET dwell.
///
/// Each top-level `Vec<I2cTransactionStep>` is **one** call to
/// `I2cServiceHandle::transaction(addr, ...)`. The order is:
///
/// 1..=`STRACE_SYNC_HEARTBEAT_COUNT`. Per-byte `0x00` sync heartbeat
///    (the parser flush — identical to the warmup chain).
/// next. Framed RESET (`STRACE_RESET_FRAME_FRAMED` = `[55 AA 04 07 00 0B]`) with
///    the  70 ms pre-Read settle + the 2-separate-reads ACK drain (echo,
///    then 0x01 status, `STRACE_INTER_ACK_READ_MS` apart) + the configurable
///    `reset_dwell_ms` post-RESET dwell (default 1000 ms, vs the warmup's 500 ms).
/// last. Framed JUMP (`STRACE_START_APP_FRAME_FRAMED` = `[55 AA 04 06 00 0A]`)
///    with the same 70 ms pre-Read settle + 2-separate-reads ACK drain + the
///    500 ms post-JUMP settle (`STRACE_START_APP_SETTLE_MS`).
///
/// This is BYTE-IDENTICAL to the flush + RESET + START_APP steps of
/// [`build_strace_derived_prelude_transactions`] (with READ-CONFIG-LATCH
/// disabled) EXCEPT the post-RESET dwell is `reset_dwell_ms` instead of the
/// hard-coded `STRACE_RESET_TO_START_APP_DELAY_MS` (500 ms). Pass 500 to get the
/// exact warmup chain back (the byte-identity is regression-pinned by a test).
///
/// Total = `STRACE_SYNC_HEARTBEAT_COUNT + 2` transactions (flush + RESET + JUMP).
pub fn build_reset_jump_reverify_transactions(reset_dwell_ms: u64) -> Vec<Vec<I2cTransactionStep>> {
    let mut txs: Vec<Vec<I2cTransactionStep>> = Vec::with_capacity(STRACE_SYNC_HEARTBEAT_COUNT + 2);

    // Flush: per-byte 0x00 sync heartbeats (separate transactions, exactly like
    // the warmup chain — each one's kernel ioctl creates the natural ~6 ms
    // inter-byte gap the dsPIC bootloader sync FSM relies on).
    for _ in 0..STRACE_SYNC_HEARTBEAT_COUNT {
        txs.push(vec![
            I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
            I2cTransactionStep::WriteByteByByte(vec![0x00]),
        ]);
    }

    // Framed RESET — byte-identical to the warmup's RESET step EXCEPT the post-
    // RESET dwell (reset_dwell_ms instead of STRACE_RESET_TO_START_APP_DELAY_MS).
    //  70 ms pre-Read settle + drain the framed echo+ack as TWO separate
    // 1-byte reads (cmd echo, then 0x01 status) STRACE_INTER_ACK_READ_MS apart +
    // the (longer, configurable) post-RESET dwell.
    txs.push(vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(STRACE_RESET_FRAME_FRAMED.to_vec()),
        I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_INTER_ACK_READ_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(reset_dwell_ms),
    ]);

    // Framed JUMP (START_APP) — byte-identical to the warmup's START_APP step.
    //  70 ms pre-Read settle + 2-separate-reads ACK drain + 500 ms
    // post-JUMP settle (STRACE_START_APP_SETTLE_MS).
    txs.push(vec![
        I2cTransactionStep::SetTimeout(PER_BYTE_TIMEOUT_JIFFIES),
        I2cTransactionStep::WriteByteByByte(STRACE_START_APP_FRAME_FRAMED.to_vec()),
        I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_INTER_ACK_READ_MS),
        I2cTransactionStep::Read(STRACE_FRAMED_ACK_LEN),
        I2cTransactionStep::SleepMs(STRACE_START_APP_SETTLE_MS),
    ]);

    debug_assert_eq!(txs.len(), STRACE_SYNC_HEARTBEAT_COUNT + 2);
    txs
}

/// Emit ONE full RESET→JUMP re-verify pass (rung 2, RESET→JUMP variant) on
/// `addr` with the given post-RESET dwell.
///
/// Sends the flush (`STRACE_SYNC_HEARTBEAT_COUNT × 0x00`), then the framed RESET
/// (`STRACE_RESET_FRAME_FRAMED` + 2-separate-reads ACK drain + `reset_dwell_ms`
/// post-RESET dwell), then the framed JUMP (`STRACE_START_APP_FRAME_FRAMED` +
/// 2-separate-reads ACK drain + 500 ms post-JUMP settle). The caller re-reads
/// GET_VERSION after this returns and repeats up to N times until the dsPIC
/// reports fw=0x89.
///
/// **This DOES issue a RESET — unlike [`am2_pic_jump_only_reverify`].** That is
/// SAFE here because the caller only enters this path when GET_VERSION already
/// read fw=0x82 (the cold bootloader state); cycling RESET→JUMP to bring a
/// cold-0x82 chip UP to 0x89 is non-destructive (an AC-cycle resets it the same
/// way). The dangerous case the bible forbids — downgrading a *working* fw=0x89
/// chip — cannot occur because we never enter this path with a fw=0x89 chip. See
/// the module-section comment above for the full reconciliation against SESSION.md.
///
/// All other invariants match the warmup helpers:
/// - **EEPROM denylist refusal** (`is_eeprom_denylist_addr`) — refuses
///   addresses in `0x50..=0x57` fail-closed before any wire byte.
/// - **Per-byte writes** via `WriteByteByByte` (matches bosminer transport).
/// - **Single I²C owner** — every transaction goes through the shared
///   `I2cServiceHandle` (the same handle the warmup + cold_boot_init use).
///
/// ## Errors
///
/// - `AsicError::Pic` if `addr` is in `0x50..=0x57` (EEPROM denylist).
/// - `AsicError::Pic` wrapping the underlying I²C error if any transaction
///   fails after the per-step retry budget is exhausted. The caller treats a
///   returned `Err` as non-fatal (logs + continues to the next attempt or to
///   the existing fail-closed path).
pub fn am2_pic_reset_jump_reverify(
    i2c: &I2cServiceHandle,
    addr: u8,
    reset_dwell_ms: u64,
) -> Result<()> {
    if is_eeprom_denylist_addr(addr) {
        return Err(AsicError::Pic {
            addr,
            detail: format!(
                "RESET→JUMP re-verify refused: addr 0x{:02X} is in the AM2 hashboard \
                 EEPROM write-denylist range (0x50..=0x57); the re-verify writes only \
                 to dsPIC addresses (0x20/0x21/0x22). Refusing fail-closed before any \
                 wire byte.",
                addr
            ),
        });
    }

    let transactions = build_reset_jump_reverify_transactions(reset_dwell_ms);
    for (idx, steps) in transactions.into_iter().enumerate() {
        let label = if idx < STRACE_SYNC_HEARTBEAT_COUNT {
            format!(
                "reset-jump-sync-heartbeat[{}/{}]",
                idx + 1,
                STRACE_SYNC_HEARTBEAT_COUNT
            )
        } else if idx == STRACE_SYNC_HEARTBEAT_COUNT {
            format!("reset-jump-FRAMED_RESET[6B]+ACK+{}ms", reset_dwell_ms)
        } else {
            "reset-jump-FRAMED_JUMP[6B]+ACK+500ms".to_string()
        };

        let mut last_err: Option<dcentrald_hal::HalError> = None;
        let mut succeeded = false;
        for attempt in 1..=PRELUDE_TRANSACTION_RETRY_BUDGET.max(1) {
            match i2c.transaction(addr, steps.clone()) {
                Ok(reads) => {
                    let ack: Vec<u8> = reads.iter().flatten().copied().collect();
                    tracing::info!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label.as_str(),
                        attempt,
                        ack = format_args!("{:02X?}", ack),
                        "RESET→JUMP re-verify (rung 2): prelude ACK bytes (real RESET ACK = \
                         [07, 01]; real JUMP ACK = [06, 01]; all-fw-byte echo = frame not \
                         recognized)."
                    );
                    succeeded = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "i2c_audit",
                        addr = format_args!("0x{:02X}", addr),
                        step = idx,
                        label = label.as_str(),
                        attempt,
                        error = %e,
                        "RESET→JUMP re-verify (rung 2): transaction failed; clean retry"
                    );
                    last_err = Some(e);
                }
            }
        }
        if !succeeded {
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "RESET→JUMP re-verify step {} ({}) failed after {} attempt(s): {}",
                    idx,
                    label,
                    PRELUDE_TRANSACTION_RETRY_BUDGET.max(1),
                    last_err
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "no error captured".to_string()),
                ),
            });
        }
    }

    tracing::info!(
        addr = format_args!("0x{:02X}", addr),
        sync_heartbeats = STRACE_SYNC_HEARTBEAT_COUNT,
        reset_dwell_ms,
        jump_settle_ms = STRACE_START_APP_SETTLE_MS,
        "RESET→JUMP re-verify (rung 2): flush + framed RESET[55 AA 04 07 00 0B] + {}ms dwell \
         + framed JUMP[55 AA 04 06 00 0A] re-issued. Caller re-reads GET_VERSION to check \
         for fw=0x89.",
        reset_dwell_ms
    );

    Ok(())
}

// ===========================================================================
//  Tests (structural — no live I²C)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Structural pin: the prelude MUST emit exactly three transactions in
    /// the canonical bosminer order. A future agent that strips the flush
    /// or reorders RESET/JUMP breaks this test.
    #[test]
    fn build_prelude_transactions_emits_canonical_three_step_chain() {
        let txs = build_prelude_transactions();
        assert_eq!(
            txs.len(),
            3,
            "bosminer prelude must be exactly 3 transactions (flush / RESET / JUMP)"
        );
    }

    /// Pin the parser flush byte sequence: `[0x55, 0xAA, 0x00]` + 16 × `0x00` = 19 bytes total.
    #[test]
    fn parser_flush_is_canonical_19_byte_payload() {
        let bytes = parser_flush_bytes();
        assert_eq!(
            bytes.len(),
            19,
            "flush must be 19 wire bytes (bosminer canonical)"
        );
        assert_eq!(bytes[0], 0x55, "byte 0 = PIC_COMMAND_1 magic");
        assert_eq!(bytes[1], 0xAA, "byte 1 = PIC_COMMAND_2 magic");
        assert_eq!(bytes[2], 0x00, "byte 2 = cmd=0x00 (no-op opcode)");
        for (i, b) in bytes.iter().enumerate().skip(3) {
            assert_eq!(*b, 0x00, "byte {} (data byte) must be zero", i);
        }
    }

    /// Pin step 0 = parser flush via `WriteByteByByte` (NOT `Write`).
    #[test]
    fn step_0_is_per_byte_parser_flush() {
        let txs = build_prelude_transactions();
        let flush_tx = &txs[0];
        assert!(
            matches!(flush_tx.first(), Some(I2cTransactionStep::SetTimeout(_))),
            "flush transaction must start with SetTimeout to bound per-byte hang time"
        );
        let mut saw_flush_write = false;
        for step in flush_tx {
            if let I2cTransactionStep::WriteByteByByte(bytes) = step {
                assert_eq!(bytes.len(), 19, "flush write must be 19 bytes");
                assert_eq!(bytes[0..3], [0x55, 0xAA, 0x00]);
                saw_flush_write = true;
            }
            // Crucial: the flush must NOT use Write (bulk single transaction).
            assert!(
                !matches!(step, I2cTransactionStep::Write(_)),
                "flush MUST be per-byte (WriteByteByByte), not bulk Write — \
                 bosminer canonical semantics require one ioctl per byte"
            );
        }
        assert!(
            saw_flush_write,
            "flush transaction must contain a WriteByteByByte step"
        );
    }

    /// Pin step 1 = RESET + 500 ms.
    #[test]
    fn step_1_is_reset_opcode_with_500ms_dwell() {
        let txs = build_prelude_transactions();
        let reset_tx = &txs[1];

        let mut saw_write = false;
        let mut saw_sleep = false;
        for step in reset_tx {
            match step {
                I2cTransactionStep::WriteByteByByte(bytes) => {
                    assert_eq!(
                        bytes.as_slice(),
                        &[0x55, 0xAA, 0x07],
                        "RESET frame must be exactly [0x55, 0xAA, 0x07]"
                    );
                    saw_write = true;
                }
                I2cTransactionStep::SleepMs(ms) => {
                    assert_eq!(
                        *ms, RESET_DELAY_MS,
                        "RESET dwell must be exactly 500 ms (RESET_DELAY)"
                    );
                    saw_sleep = true;
                }
                I2cTransactionStep::SetTimeout(_) => {}
                other => panic!("RESET transaction must not contain step: {:?}", other),
            }
        }
        assert!(saw_write, "RESET transaction must contain WriteByteByByte");
        assert!(saw_sleep, "RESET transaction must contain SleepMs(500)");
    }

    /// Pin step 2 = JUMP + 100 ms.
    #[test]
    fn step_2_is_jump_opcode_with_100ms_dwell() {
        let txs = build_prelude_transactions();
        let jump_tx = &txs[2];

        let mut saw_write = false;
        let mut saw_sleep = false;
        for step in jump_tx {
            match step {
                I2cTransactionStep::WriteByteByByte(bytes) => {
                    assert_eq!(
                        bytes.as_slice(),
                        &[0x55, 0xAA, 0x06],
                        "JUMP frame must be exactly [0x55, 0xAA, 0x06]"
                    );
                    saw_write = true;
                }
                I2cTransactionStep::SleepMs(ms) => {
                    assert_eq!(
                        *ms, JUMP_SETTLE_MS,
                        "JUMP dwell must be exactly 100 ms (BMMINER_DELAY)"
                    );
                    saw_sleep = true;
                }
                I2cTransactionStep::SetTimeout(_) => {}
                other => panic!("JUMP transaction must not contain step: {:?}", other),
            }
        }
        assert!(saw_write, "JUMP transaction must contain WriteByteByByte");
        assert!(saw_sleep, "JUMP transaction must contain SleepMs(100)");
    }

    /// Prelude MUST NEVER contain a Read step — it's a prelude, not a probe.
    #[test]
    fn prelude_contains_no_read_steps() {
        let txs = build_prelude_transactions();
        for (idx, tx) in txs.iter().enumerate() {
            for step in tx {
                assert!(
                    !matches!(
                        step,
                        I2cTransactionStep::Read(_)
                            | I2cTransactionStep::ReadFrame { .. }
                            | I2cTransactionStep::WriteRead { .. }
                    ),
                    "prelude transaction {} must not contain any read step (it is a prelude, \
                     not a probe — GET_VERSION is the caller's responsibility)",
                    idx,
                );
            }
        }
    }

    /// Belt-and-suspenders: addresses 0x50..=0x57 (AM2 EEPROM) must be refused
    /// without sending any wire byte. The HAL denylist is the load-bearing
    /// gate; this is a second line of defence so a future bug can't misroute
    /// the prelude to an EEPROM address.
    #[test]
    fn refuses_eeprom_denylist_addresses() {
        // We cannot actually call `am2_pic_reset_and_start_app_bosminer_faithful`
        // without a live I2cServiceHandle because the test handle blocks
        // forever on submit. So we test the gate predicate directly.
        for addr in 0x50u8..=0x57u8 {
            assert!(
                is_eeprom_denylist_addr(addr),
                "addr 0x{:02X} must be in the EEPROM denylist",
                addr
            );
        }
        assert!(!is_eeprom_denylist_addr(0x20), "dsPIC 0x20 must be allowed");
        assert!(!is_eeprom_denylist_addr(0x21), "dsPIC 0x21 must be allowed");
        assert!(!is_eeprom_denylist_addr(0x22), "dsPIC 0x22 must be allowed");
        assert!(
            !is_eeprom_denylist_addr(0x4F),
            "below denylist must be allowed"
        );
        assert!(
            !is_eeprom_denylist_addr(0x58),
            "above denylist must be allowed"
        );
    }

    // NOTE: a public-entry-point test that calls
    // `am2_pic_reset_and_start_app_bosminer_faithful(&handle, addr)` with a
    // mock service is intentionally NOT included here — `I2cServiceHandle::
    // for_unit_tests` is `#[cfg(test)]`-gated inside the `dcentrald-hal`
    // crate, so it is not visible from `dcentrald-asic` tests. The
    // `is_eeprom_denylist_addr` predicate test above covers the gate by
    // structural inspection. The integration-level fail-closed proof is in
    // `dcentrald` daemon-side tests where the handle constructor is in
    // scope and the wrapper's public entry point can be exercised end-to-end.

    // =======================================================================
    //   — strace-derived framed-protocol pins
    // =======================================================================

    /// Structural pin: the strace-derived prelude MUST emit exactly 10
    /// transactions (8 sync heartbeats + framed RESET + framed START_APP)
    /// when `DCENT_AM2_DSPIC_READ_CONFIG_LATCH` is unset (default behavior).
    ///
    /// RE-018 (2026-06-07): corrected from 9 → 10 because the TRUE-COLD
    /// `re018-cold-strace` (the authoritative AC-cold capture, decoded
    /// fd22=slave 0x20) shows exactly 8 leading `0x00` sync writes, not 7.
    /// The earlier  "7" was based on the dropped-byte
    /// `wave38-bosminer-truth` extract. See `STRACE_SYNC_HEARTBEAT_COUNT`.
    ///
    ///  Part 2: when the (now-known decode-artifact) READ-CONFIG-LATCH
    /// env var is set, the chain extends to 11 transactions — pinned
    /// separately in `wave55k_read_config_latch_inserts_between_reset_and_start_app`.
    #[test]
    fn wave28_build_strace_derived_emits_10_transactions() {
        // Defensive: the  Part 2 env gate could be leaked into
        // the test process from a prior test or shell. Ensure default-OFF.
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
        let txs = build_strace_derived_prelude_transactions();
        assert_eq!(
            txs.len(),
            10,
            "RE-018 strace-derived prelude must be exactly 10 transactions \
             (8 sync + RESET + START_APP) when DCENT_AM2_DSPIC_READ_CONFIG_LATCH is unset"
        );
        assert_eq!(
            STRACE_SYNC_HEARTBEAT_COUNT, 8,
            "RE-018: the true-cold re018-cold-strace shows 8 sync writes (corrects Wave-43's 7)"
        );
    }

    /// Pin steps 1..=8 = single-byte 0x00 sync heartbeats, each its own
    /// transaction. The true-cold `re018-cold-strace` (fd22 = slave 0x20)
    /// shows 8 separate `write(fd, "\x00", 1)` calls — the 6 ms
    /// inter-byte gap is the natural kernel ioctl latency, not an
    /// artificial sleep. (RE-018 corrected 7 → 8; the old `wave38` extract
    /// had dropped a leading byte.)
    #[test]
    fn wave28_sync_heartbeats_are_eight_separate_single_byte_writes() {
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
        let txs = build_strace_derived_prelude_transactions();
        for (i, tx) in txs.iter().enumerate().take(STRACE_SYNC_HEARTBEAT_COUNT) {
            // Each heartbeat transaction is SetTimeout + WriteByteByByte([0x00]).
            assert!(
                tx.iter()
                    .any(|s| matches!(s, I2cTransactionStep::SetTimeout(_))),
                "heartbeat transaction {} must start with SetTimeout",
                i
            );
            let writes: Vec<&Vec<u8>> = tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::WriteByteByByte(b) => Some(b),
                    _ => None,
                })
                .collect();
            assert_eq!(
                writes.len(),
                1,
                "heartbeat tx {} must have exactly 1 WriteByteByByte",
                i
            );
            assert_eq!(
                writes[0].as_slice(),
                &[0x00],
                "heartbeat tx {} must be single byte 0x00",
                i
            );
            // No Read / no Sleep on heartbeats — natural ioctl latency provides the gap.
            assert!(
                !tx.iter().any(|s| matches!(
                    s,
                    I2cTransactionStep::Read(_) | I2cTransactionStep::SleepMs(_)
                )),
                "heartbeat tx {} must not contain Read or SleepMs",
                i
            );
        }
    }

    /// Pin step 8 = framed RESET `[55 AA 04 07 00 0B]` +  70 ms
    /// pre-Read settle + Read(1) ACK + 500 ms post-Read dwell.
    /// Byte-exact match against the `a lab unit` strace evidence.
    /// ( corrected the step index from 9 → 8 by dropping one
    /// of the redundant sync heartbeats;  added the 70 ms
    /// pre-Read settle to match the strace's 66 ms write-to-ACK gap.)
    #[test]
    fn wave28_step_8_is_framed_reset_with_ack_and_500ms() {
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
        let txs = build_strace_derived_prelude_transactions();
        let reset_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];

        // Verify the exact wire bytes against the constant + strace.
        let mut saw_write = false;
        let mut saw_read = false;
        let mut saw_pre_read_sleep = false;
        let mut saw_post_read_sleep = false;
        for step in reset_tx {
            match step {
                I2cTransactionStep::WriteByteByByte(bytes) => {
                    assert_eq!(
                        bytes.as_slice(),
                        &[0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B],
                        "Wave-28 framed RESET wire bytes must be exactly \
                         [0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B] (LEN=4, CMD=0x07, \
                         PAYLOAD=0x00, CKSUM=LEN+CMD+PAYLOAD=0x0B)"
                    );
                    saw_write = true;
                }
                I2cTransactionStep::Read(n) => {
                    assert_eq!(*n, 1, "RESET ACK drain must be exactly 1 byte");
                    saw_read = true;
                }
                I2cTransactionStep::SleepMs(ms) => {
                    if *ms == STRACE_WRITE_TO_ACK_READ_DELAY_MS {
                        saw_pre_read_sleep = true;
                    } else if *ms == STRACE_RESET_TO_START_APP_DELAY_MS {
                        saw_post_read_sleep = true;
                    } else if *ms == STRACE_INTER_ACK_READ_MS {
                        // 2026-06-07 (commit 0a1bfa5a 2-read ACK drain): the
                        // inter-read settle between the two 1-byte ACK reads.
                    } else {
                        panic!(
                            "framed RESET transaction must only contain SleepMs(70) (pre-Read), \
                             SleepMs(7) (inter-ACK-read) or SleepMs(500) (post-Read), got SleepMs({})",
                            ms
                        );
                    }
                }
                I2cTransactionStep::SetTimeout(_) => {}
                other => panic!(
                    "framed RESET transaction must not contain step: {:?}",
                    other
                ),
            }
        }
        assert!(saw_write, "framed RESET tx must contain WriteByteByByte");
        assert!(
            saw_read,
            "framed RESET tx must contain Read(1) to drain ACK"
        );
        assert!(
            saw_pre_read_sleep,
            "Wave-44: framed RESET tx must contain SleepMs(70) BEFORE Read \
             (gives dsPIC 66 ms to prepare ACK byte per strace)"
        );
        assert!(
            saw_post_read_sleep,
            "framed RESET tx must contain SleepMs(500) AFTER Read"
        );
    }

    /// Pin step 9 = framed START_APP `[55 AA 04 06 00 0A]` +
    /// 70 ms pre-Read settle + Read(1) ACK + 500 ms post-Read settle.
    /// Byte-exact match against the `a lab unit` strace evidence.
    /// ( corrected the step index from 10 → 9;  added
    /// the 70 ms pre-Read settle.)
    #[test]
    fn wave28_step_9_is_framed_start_app_with_ack_and_500ms() {
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
        let txs = build_strace_derived_prelude_transactions();
        let start_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT + 1];

        let mut saw_write = false;
        let mut saw_read = false;
        let mut saw_pre_read_sleep = false;
        let mut saw_post_read_sleep = false;
        for step in start_tx {
            match step {
                I2cTransactionStep::WriteByteByByte(bytes) => {
                    assert_eq!(
                        bytes.as_slice(),
                        &[0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A],
                        "Wave-28 framed START_APP wire bytes must be exactly \
                         [0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A] (LEN=4, CMD=0x06, \
                         PAYLOAD=0x00, CKSUM=LEN+CMD+PAYLOAD=0x0A)"
                    );
                    saw_write = true;
                }
                I2cTransactionStep::Read(n) => {
                    assert_eq!(*n, 1, "START_APP ACK drain must be exactly 1 byte");
                    saw_read = true;
                }
                I2cTransactionStep::SleepMs(ms) => {
                    if *ms == STRACE_WRITE_TO_ACK_READ_DELAY_MS {
                        saw_pre_read_sleep = true;
                    } else if *ms == STRACE_START_APP_SETTLE_MS {
                        saw_post_read_sleep = true;
                    } else if *ms == STRACE_INTER_ACK_READ_MS {
                        // 2026-06-07 (commit 0a1bfa5a 2-read ACK drain): the
                        // inter-read settle between the two 1-byte ACK reads.
                    } else {
                        panic!(
                            "framed START_APP transaction must only contain SleepMs(70) (pre-Read), \
                             SleepMs(7) (inter-ACK-read) or SleepMs(500) (post-Read), got SleepMs({})",
                            ms
                        );
                    }
                }
                I2cTransactionStep::SetTimeout(_) => {}
                other => panic!(
                    "framed START_APP transaction must not contain step: {:?}",
                    other
                ),
            }
        }
        assert!(
            saw_write,
            "framed START_APP tx must contain WriteByteByByte"
        );
        assert!(
            saw_read,
            "framed START_APP tx must contain Read(1) to drain ACK"
        );
        assert!(
            saw_pre_read_sleep,
            "Wave-44: framed START_APP tx must contain SleepMs(70) BEFORE Read"
        );
        assert!(
            saw_post_read_sleep,
            "framed START_APP tx must contain SleepMs(500) AFTER Read"
        );
    }

    // =======================================================================
    //  COLD-BYTE-DIFF Fix B (2026-06-07) — single atomic transaction pins
    // =======================================================================

    /// `I2cTransactionStep` does not derive `PartialEq`, so this helper renders
    /// a step to its canonical `Debug` string for byte-and-sleep comparison.
    /// Two step lists with equal Debug strings are bit-identical on the wire.
    fn step_debug(step: &I2cTransactionStep) -> String {
        format!("{:?}", step)
    }

    /// THE load-bearing Fix B pin: the single-transaction warmup is the EXACT
    /// byte-and-sleep concatenation of the N-transaction warmup. If a future
    /// change touches one builder but not the other, the wire output would
    /// diverge — this test fails first. Pins identity for BOTH the default
    /// (READ-CONFIG-LATCH off) and the opt-in (READ-CONFIG-LATCH on) shapes.
    #[test]
    fn cold_byte_diff_single_transaction_equals_flattened_multi_transaction() {
        for latch in [false, true] {
            if latch {
                std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", "1");
            } else {
                std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
            }

            let multi = build_strace_derived_prelude_transactions();
            let flattened: Vec<I2cTransactionStep> = multi.into_iter().flatten().collect();
            let single = build_strace_derived_prelude_single_transaction();

            assert_eq!(
                single.len(),
                flattened.len(),
                "Fix B (latch={}): single-transaction step count must equal the \
                 flattened multi-transaction step count",
                latch
            );

            let single_dbg: Vec<String> = single.iter().map(step_debug).collect();
            let flat_dbg: Vec<String> = flattened.iter().map(step_debug).collect();
            assert_eq!(
                single_dbg, flat_dbg,
                "Fix B (latch={}): single-transaction warmup MUST be the exact \
                 byte-and-sleep concatenation of the N-transaction warmup — same \
                 bytes, same SleepMs dwells, same order. Only the transaction \
                 boundary may differ.",
                latch
            );
        }
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
    }

    /// Pin that the single-transaction warmup still carries the exact framed
    /// RESET + START_APP wire bytes (defence-in-depth beyond the concatenation
    /// equality — a corrupted flatten would be caught here even if the
    /// equality helper changed).
    #[test]
    fn cold_byte_diff_single_transaction_carries_reset_and_start_app_bytes() {
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
        let single = build_strace_derived_prelude_single_transaction();

        let writes: Vec<&Vec<u8>> = single
            .iter()
            .filter_map(|s| match s {
                I2cTransactionStep::WriteByteByByte(b) => Some(b),
                _ => None,
            })
            .collect();

        // 8 single-byte 0x00 flushes + framed RESET + framed START_APP.
        assert_eq!(
            writes.len(),
            STRACE_SYNC_HEARTBEAT_COUNT + 2,
            "single-transaction warmup must have 8 flush writes + RESET + START_APP"
        );
        for w in writes.iter().take(STRACE_SYNC_HEARTBEAT_COUNT) {
            assert_eq!(
                w.as_slice(),
                &[0x00],
                "leading writes must be single 0x00 flush bytes"
            );
        }
        assert_eq!(
            writes[STRACE_SYNC_HEARTBEAT_COUNT].as_slice(),
            &STRACE_RESET_FRAME_FRAMED,
            "framed RESET wire bytes unchanged in the single-transaction form"
        );
        assert_eq!(
            writes[STRACE_SYNC_HEARTBEAT_COUNT + 1].as_slice(),
            &STRACE_START_APP_FRAME_FRAMED,
            "framed START_APP wire bytes unchanged in the single-transaction form"
        );

        // Two ACK reads per framed opcode (the 0a1bfa5a 2-read drain) survive
        // the flatten: 4 Read steps total (2 RESET + 2 START_APP).
        let read_count = single
            .iter()
            .filter(|s| matches!(s, I2cTransactionStep::Read(_)))
            .count();
        assert_eq!(
            read_count, 4,
            "single-transaction warmup must keep the 2-separate-reads ACK drain \
             on both RESET and START_APP (4 Read steps total)"
        );
    }

    /// ** LOW-6 (2026-05-24, DCENT_EE swarm finding).** Pin the
    /// `STRACE_WRITE_TO_ACK_READ_DELAY_MS` constant to its minimum-safe
    /// floor (66 ms — the chip's actual ACK-byte prepare time per the
    /// `a lab unit` strace `wave38-bosminer-truth/bosminer-i2c0-slave20.txt`
    /// lines 17/22/28).  picked 70 ms (66 + 4 ms margin). A
    /// future "let's tighten the latency" change MUST stay above 66 ms
    /// or chips return garbage on the ACK read.
    #[test]
    fn wave55a_pre_read_settle_meets_chip_prepare_floor() {
        assert!(
            STRACE_WRITE_TO_ACK_READ_DELAY_MS >= 66,
            "Wave-55a LOW-6 regression: \
             STRACE_WRITE_TO_ACK_READ_DELAY_MS = {} is below the .25 \
             dsPIC's measured 66 ms ACK-byte prepare time. The chip \
             will return garbage on the ACK read. See \
             \
             WAVE44-PRE-READ-SETTLE.md and the strace lines 17/22/28 in \
             wave38-bosminer-truth/bosminer-i2c0-slave20.txt.",
            STRACE_WRITE_TO_ACK_READ_DELAY_MS
        );
    }

    /// ** LOW-7 (2026-05-24, DCENT_EE swarm finding).** Pin the
    /// no-jump warmup variant's structural invariant: the
    /// `am2_pic_reset_only_bosminer_faithful` flow truncates
    /// `build_prelude_transactions()` to 2 steps (flush + RESET) —
    /// dropping the JUMP_TO_APP step. This test asserts the JUMP
    /// opcode byte (0x06 — second byte of `JUMP_FRAME`) is NOT present
    /// anywhere in the truncated transaction list. Catches a future
    /// refactor that accidentally reintroduces the JUMP step into the
    /// no-jump variant.
    #[test]
    fn wave55a_no_jump_variant_does_not_emit_jump_opcode() {
        let mut transactions = build_prelude_transactions();
        transactions.truncate(2); // Matches am2_pic_reset_only_bosminer_faithful

        for (idx, steps) in transactions.iter().enumerate() {
            for (step_idx, step) in steps.iter().enumerate() {
                if let I2cTransactionStep::WriteByteByByte(bytes) = step {
                    // JUMP_FRAME = [0x55, 0xAA, 0x06] — full 3-byte
                    // signature. If any WriteByteByByte step in the
                    // no-jump variant equals JUMP_FRAME, that's a
                    // regression.
                    assert_ne!(
                        bytes.as_slice(),
                        &[0x55, 0xAA, 0x06],
                        "Wave-55a LOW-7 regression: no-jump variant tx[{}] \
                         step[{}] emits the JUMP frame [55 AA 06]. \
                         The no-jump variant must skip JUMP (Wave-22 BARE \
                         BREAK from earlier no-jump experiments). See \
                         am2_pic_reset_only_bosminer_faithful in \
                         bosminer_warmup.rs.",
                        idx,
                        step_idx
                    );
                    // Also assert the standalone JUMP CMD byte 0x06 is
                    // not the 3rd byte of any 3-byte preamble-led frame
                    // (would mean someone added a framed-JUMP path).
                    if bytes.len() == 3 && bytes[0] == 0x55 && bytes[1] == 0xAA {
                        assert_ne!(
                            bytes[2], 0x06,
                            "Wave-55a LOW-7: no-jump variant must not emit \
                             [55 AA 06] (the bare-form JUMP opcode)"
                        );
                    }
                }
            }
        }
    }

    /// Pin the framed-protocol checksum algorithm: `LEN + CMD + sum(PAYLOAD)`.
    /// Computed once from the constants and asserted against the wire bytes.
    /// Regression-protects against a checksum-formula drift in the constants.
    #[test]
    fn wave28_framed_checksum_algorithm_matches_strace_constants() {
        // RESET frame.
        let reset = STRACE_RESET_FRAME_FRAMED;
        let reset_len = reset[2];
        let reset_cmd = reset[3];
        let reset_payload = reset[4];
        let reset_cksum_expected: u8 = reset_len
            .wrapping_add(reset_cmd)
            .wrapping_add(reset_payload);
        assert_eq!(
            reset[5], reset_cksum_expected,
            "RESET checksum byte must equal LEN+CMD+PAYLOAD"
        );
        assert_eq!(
            reset_cksum_expected, 0x0B,
            "RESET checksum must be 0x0B (4+7+0)"
        );

        // START_APP frame.
        let start = STRACE_START_APP_FRAME_FRAMED;
        let start_len = start[2];
        let start_cmd = start[3];
        let start_payload = start[4];
        let start_cksum_expected: u8 = start_len
            .wrapping_add(start_cmd)
            .wrapping_add(start_payload);
        assert_eq!(
            start[5], start_cksum_expected,
            "START_APP checksum byte must equal LEN+CMD+PAYLOAD"
        );
        assert_eq!(
            start_cksum_expected, 0x0A,
            "START_APP checksum must be 0x0A (4+6+0)"
        );

        // Both frames must share the same 0x55/0xAA preamble and LEN=4.
        assert_eq!([reset[0], reset[1], reset[2]], [0x55, 0xAA, 0x04]);
        assert_eq!([start[0], start[1], start[2]], [0x55, 0xAA, 0x04]);
    }

    // =======================================================================
    //   — LM75A passthrough warmup structural pins
    // =======================================================================

    /// Pin transaction count = 17 exactly. Matches
    /// `bosminer-i2c0-slave20.txt:33-203`. A future drift to 16 or 18
    /// would mean the byte sequence no longer matches bosminer's
    /// ground-truth cold-boot trace.
    #[test]
    fn wave55f_lm75_passthrough_is_17_transactions() {
        let txs = build_lm75_passthrough_transactions();
        assert_eq!(
            txs.len(),
            LM75_PASSTHROUGH_TX_COUNT,
            "Wave-55f: LM75 passthrough chain must be exactly 17 transactions \
             (8 round-1 + 8 round-2 + 1 trailing 3B-to-0x48); matches the \
             byte-exact bosminer trace at bosminer-i2c0-slave20.txt:33-203"
        );
        assert_eq!(LM75_PASSTHROUGH_TX_COUNT, 17);
    }

    /// Pin frame builder: byte-exact for the first 4 sensors × 2
    /// directions from the trace. CKSUM must match
    /// `LEN + CMD + SENSOR + FLAG + 0x00`.
    #[test]
    fn wave55f_lm75_frame_builder_matches_trace_bytes_exact() {
        // 0x3B writes (FLAG=0x00). Source: bosminer-i2c0-slave20.txt
        // lines 33-40 (sensor 0x48), 55-62 (0x49), 74-80 (0x4A),
        // 95-102 (0x4B).
        let expected_write: [(u8, [u8; 8]); 4] = [
            (0x48, [0x55, 0xAA, 0x06, 0x3B, 0x48, 0x00, 0x00, 0x89]),
            (0x49, [0x55, 0xAA, 0x06, 0x3B, 0x49, 0x00, 0x00, 0x8A]),
            (0x4A, [0x55, 0xAA, 0x06, 0x3B, 0x4A, 0x00, 0x00, 0x8B]),
            (0x4B, [0x55, 0xAA, 0x06, 0x3B, 0x4B, 0x00, 0x00, 0x8C]),
        ];
        for (sensor, expected) in expected_write.iter() {
            let actual = build_lm75_passthrough_frame(LM75_PT_OPCODE_WRITE, *sensor, 0x00);
            assert_eq!(
                actual.as_slice(),
                expected.as_slice(),
                "Wave-55f: 0x3B write frame to sensor 0x{:02X} must match \
                 bosminer-i2c0-slave20.txt byte-for-byte",
                sensor
            );
        }
        // 0x3C reads (FLAG=0x02). Source: trace lines 42-48 (0x48),
        // 64-70 (0x49), 82-88 (0x4A), 104-110 (0x4B).
        let expected_read: [(u8, [u8; 8]); 4] = [
            (0x48, [0x55, 0xAA, 0x06, 0x3C, 0x48, 0x02, 0x00, 0x8C]),
            (0x49, [0x55, 0xAA, 0x06, 0x3C, 0x49, 0x02, 0x00, 0x8D]),
            (0x4A, [0x55, 0xAA, 0x06, 0x3C, 0x4A, 0x02, 0x00, 0x8E]),
            (0x4B, [0x55, 0xAA, 0x06, 0x3C, 0x4B, 0x02, 0x00, 0x8F]),
        ];
        for (sensor, expected) in expected_read.iter() {
            let actual = build_lm75_passthrough_frame(LM75_PT_OPCODE_READ, *sensor, 0x02);
            assert_eq!(
                actual.as_slice(),
                expected.as_slice(),
                "Wave-55f: 0x3C read frame from sensor 0x{:02X} must match \
                 bosminer-i2c0-slave20.txt byte-for-byte",
                sensor
            );
        }
    }

    /// Pin checksum algorithm = LEN + CMD + SENSOR + FLAG + 0x00 mod 256.
    /// Matches the existing FRAMED RESET/START_APP checksum.
    #[test]
    fn wave55f_lm75_checksum_matches_framed_algorithm() {
        for &sensor in &LM75_SENSOR_ADDRS {
            // 0x3B write CKSUM
            let frame_w = build_lm75_passthrough_frame(LM75_PT_OPCODE_WRITE, sensor, 0x00);
            let expected_w: u8 = LM75_FRAME_LEN_BYTE
                .wrapping_add(LM75_PT_OPCODE_WRITE)
                .wrapping_add(sensor)
                .wrapping_add(0x00)
                .wrapping_add(0x00);
            assert_eq!(
                frame_w[7], expected_w,
                "Wave-55f: 0x3B sensor 0x{:02X} CKSUM must equal LEN+CMD+SENSOR+FLAG+0x00",
                sensor
            );
            // 0x3C read CKSUM
            let frame_r = build_lm75_passthrough_frame(LM75_PT_OPCODE_READ, sensor, 0x02);
            let expected_r: u8 = LM75_FRAME_LEN_BYTE
                .wrapping_add(LM75_PT_OPCODE_READ)
                .wrapping_add(sensor)
                .wrapping_add(0x02)
                .wrapping_add(0x00);
            assert_eq!(
                frame_r[7], expected_r,
                "Wave-55f: 0x3C sensor 0x{:02X} CKSUM must equal LEN+CMD+SENSOR+FLAG+0x00",
                sensor
            );
        }
    }

    /// Pin transaction ordering: round 1 (8 tx) → round 2 (8 tx) →
    /// trailing tx (1). Each sensor visited in 0x48..0x4B order; within
    /// each sensor 0x3B WR precedes 0x3C RD.
    #[test]
    fn wave55f_lm75_transaction_order_matches_trace() {
        let txs = build_lm75_passthrough_transactions();
        let expected_order: Vec<(u8, u8)> = vec![
            // Round 1
            (LM75_PT_OPCODE_WRITE, 0x48),
            (LM75_PT_OPCODE_READ, 0x48),
            (LM75_PT_OPCODE_WRITE, 0x49),
            (LM75_PT_OPCODE_READ, 0x49),
            (LM75_PT_OPCODE_WRITE, 0x4A),
            (LM75_PT_OPCODE_READ, 0x4A),
            (LM75_PT_OPCODE_WRITE, 0x4B),
            (LM75_PT_OPCODE_READ, 0x4B),
            // Round 2
            (LM75_PT_OPCODE_WRITE, 0x48),
            (LM75_PT_OPCODE_READ, 0x48),
            (LM75_PT_OPCODE_WRITE, 0x49),
            (LM75_PT_OPCODE_READ, 0x49),
            (LM75_PT_OPCODE_WRITE, 0x4A),
            (LM75_PT_OPCODE_READ, 0x4A),
            (LM75_PT_OPCODE_WRITE, 0x4B),
            (LM75_PT_OPCODE_READ, 0x4B),
            // Trailing tx
            (LM75_PT_OPCODE_WRITE, 0x48),
        ];
        assert_eq!(txs.len(), expected_order.len());
        for (idx, ((expected_cmd, expected_sensor), tx)) in
            expected_order.iter().zip(txs.iter()).enumerate()
        {
            let bytes = tx
                .iter()
                .find_map(|s| {
                    if let I2cTransactionStep::WriteByteByByte(b) = s {
                        Some(b.clone())
                    } else {
                        None
                    }
                })
                .expect("tx must contain a WriteByteByByte step");
            assert_eq!(
                bytes.len(),
                8,
                "tx {} frame must be 8 bytes (55 AA 06 CMD SENSOR FLAG 00 CKSUM)",
                idx
            );
            assert_eq!(
                bytes[3], *expected_cmd,
                "tx {} CMD must be 0x{:02X}",
                idx, expected_cmd
            );
            assert_eq!(
                bytes[4], *expected_sensor,
                "tx {} SENSOR must be 0x{:02X}",
                idx, expected_sensor
            );
        }
    }

    /// Pin per-tx step shape: SetTimeout, WriteByteByByte (8B frame),
    /// SleepMs(70) pre-Read, Read(1 for 0x3B / 6 for 0x3C), SleepMs(6) settle.
    #[test]
    fn wave55f_lm75_tx_step_shape_is_canonical() {
        let txs = build_lm75_passthrough_transactions();
        for (idx, tx) in txs.iter().enumerate() {
            // Must contain exactly: SetTimeout, WriteByteByByte,
            // SleepMs(70), Read, SleepMs(6) — 5 steps.
            assert_eq!(
                tx.len(),
                5,
                "tx {} must have exactly 5 steps (SetTimeout + WriteByteByByte + \
                 SleepMs(70) + Read + SleepMs(6)); got {} steps",
                idx,
                tx.len()
            );
            // Verify step order.
            assert!(
                matches!(tx[0], I2cTransactionStep::SetTimeout(_)),
                "tx {} step 0 must be SetTimeout",
                idx
            );
            let write_bytes = match &tx[1] {
                I2cTransactionStep::WriteByteByByte(b) => b,
                other => panic!("tx {} step 1 must be WriteByteByByte, got {:?}", idx, other),
            };
            assert_eq!(
                write_bytes.len(),
                8,
                "tx {} write frame must be 8 bytes",
                idx
            );
            // Pre-Read 70 ms settle.
            assert!(
                matches!(tx[2], I2cTransactionStep::SleepMs(STRACE_WRITE_TO_ACK_READ_DELAY_MS)),
                "tx {} step 2 must be SleepMs(70) pre-Read settle (matches Wave-44 dsPIC ACK-prepare floor)",
                idx
            );
            let read_len = match &tx[3] {
                I2cTransactionStep::Read(n) => *n,
                other => panic!("tx {} step 3 must be Read, got {:?}", idx, other),
            };
            // 0x3B (CMD byte at idx 3 of frame) → 1-byte Read.
            // 0x3C → 6-byte Read.
            let cmd = write_bytes[3];
            if cmd == LM75_PT_OPCODE_WRITE {
                assert_eq!(read_len, 1, "tx {} 0x3B read must be 1 byte (ACK)", idx);
            } else if cmd == LM75_PT_OPCODE_READ {
                assert_eq!(
                    read_len, 6,
                    "tx {} 0x3C read must be 6 bytes (3C echo + up to 5 data)",
                    idx
                );
            } else {
                panic!("tx {} CMD byte 0x{:02X} is neither 0x3B nor 0x3C", idx, cmd);
            }
            // Inter-tx 6 ms settle.
            assert!(
                matches!(
                    tx[4],
                    I2cTransactionStep::SleepMs(LM75_PASSTHROUGH_INTER_TX_MS)
                ),
                "tx {} step 4 must be SleepMs(6) inter-tx settle",
                idx
            );
        }
    }

    /// EEPROM denylist must refuse the warmup. We can only test the
    /// gate predicate directly (no live I²C handle in unit tests); the
    /// public entry point's gate is structurally pinned.
    #[test]
    fn wave55f_lm75_refuses_eeprom_denylist_addresses() {
        for addr in 0x50u8..=0x57u8 {
            assert!(
                is_eeprom_denylist_addr(addr),
                "addr 0x{:02X} must be in the EEPROM denylist",
                addr
            );
        }
        // The dsPIC addresses must remain allowed.
        assert!(!is_eeprom_denylist_addr(0x20));
        assert!(!is_eeprom_denylist_addr(0x21));
        assert!(!is_eeprom_denylist_addr(0x22));
    }

    /// LM75A passthrough sensor addresses must be 0x48..0x4B (matches
    /// bosminer trace + datasheet — LM75A has 3 address pins giving 8
    /// possible addresses 0x48..0x4F, and bosminer uses the low 4 for
    /// the AM2 hashboard).
    #[test]
    fn wave55f_lm75_sensor_addrs_are_canonical() {
        assert_eq!(LM75_SENSOR_ADDRS, [0x48, 0x49, 0x4A, 0x4B]);
    }

    /// **Default-OFF safety pin.** The env helper must read
    /// `DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH` and return `false`
    /// when unset. A future drift to default-on would silently re-engage
    /// a never-live-tested warmup on every `a lab unit`-class boot.
    #[test]
    fn wave55f_lm75_passthrough_env_helper_default_off() {
        // Save + clear in case the test environment has it set.
        let prev = std::env::var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH");
        assert!(
            !am2_dspic_lm75_passthrough_enabled(),
            "Wave-55f safety regression: env helper must return false when \
             DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH is unset"
        );
        // Set to "1" and confirm.
        std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH", "1");
        assert!(am2_dspic_lm75_passthrough_enabled());
        // Set to "0" and confirm.
        std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH", "0");
        assert!(!am2_dspic_lm75_passthrough_enabled());
        // Restore.
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH"),
        }
    }

    // =======================================================================
    //   Part 2 — READ-CONFIG-LATCH opcode 0x00 inline structural pins
    // =======================================================================

    /// Pin the captured  bytes: `[0xAA, 0x04, 0x00, 0x0A]` — 4 bytes
    /// total, NO `0x55` preamble. Source: `bosminer-i2c0-slave20.txt:19-22`.
    /// A regression that adds the `0x55` preamble (or changes any byte)
    /// diverges from the live-proven bosminer cold-cold ground truth.
    #[test]
    fn wave55k_read_config_latch_constant_matches_bosminer_strace() {
        assert_eq!(
            STRACE_READ_CONFIG_LATCH_FRAMED,
            [0xAA, 0x04, 0x00, 0x0A],
            "Wave-55k Part 2: READ-CONFIG-LATCH wire bytes must match \
             bosminer-i2c0-slave20.txt:19-22 byte-for-byte (4 bytes total, \
             NO 0x55 preamble — the dsPIC parser stays in framed mode after \
             the RESET frame's preamble)."
        );
    }

    /// Pin the env helper's default-OFF safety contract.
    #[test]
    fn wave55k_read_config_latch_env_helper_default_off() {
        let prev = std::env::var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");
        assert!(
            !am2_dspic_read_config_latch_enabled(),
            "Wave-55k Part 2 safety regression: env helper must return false \
             when DCENT_AM2_DSPIC_READ_CONFIG_LATCH is unset — default behavior \
             MUST be byte-identical to the pre-Wave-55k bosminer-handoff path."
        );
        std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", "1");
        assert!(am2_dspic_read_config_latch_enabled());
        std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", "0");
        assert!(!am2_dspic_read_config_latch_enabled());
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH"),
        }
    }

    /// Post-JUMP heartbeat keep-alive (2026-06-07, `a lab unit` standalone
    /// cold-engage): the env helper MUST be default-OFF so the entire
    /// fleet/handoff/legacy `cold_boot_init_with_options` path stays
    /// byte-identical when the operator has not opted in. The caller AND-gates
    /// this with the `a lab unit` fingerprint, but the env helper itself is the
    /// primary default-OFF guarantee.
    #[test]
    fn postjump_keepalive_env_helper_default_off() {
        let prev = std::env::var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE");
        assert!(
            !am2_dspic_postjump_heartbeat_keepalive_enabled(),
            "post-JUMP keep-alive safety regression: env helper must return false \
             when DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE is unset — default \
             behavior MUST be byte-identical to the fleet/handoff/legacy path."
        );
        std::env::set_var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE", "1");
        assert!(am2_dspic_postjump_heartbeat_keepalive_enabled());
        std::env::set_var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE", "0");
        assert!(!am2_dspic_postjump_heartbeat_keepalive_enabled());
        // The new env is NOT one of the 4 forbidden `a lab unit` env vars, so it must
        // never collide with them.
        for forbidden in [
            "DCENT_AM2_PIC_RESET_AND_START_APP",
            "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
            "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
            "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
        ] {
            assert_ne!(forbidden, "DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE");
        }
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE"),
        }
    }

    /// Re-JUMP-before-ENABLE (2026-06-07, `a lab unit` standalone cold-engage): the env
    /// helper MUST be default-OFF so the entire fleet/handoff/legacy
    /// `cold_boot_init_with_options` path stays byte-identical (no extra
    /// GET_VERSION/JUMP) when the operator has not opted in. The caller
    /// AND-gates this with the `a lab unit` fingerprint, but the env helper itself is
    /// the primary default-OFF guarantee.
    #[test]
    fn rejump_before_enable_env_helper_default_off() {
        let prev = std::env::var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE");
        assert!(
            !am2_dspic_rejump_before_enable_enabled(),
            "re-JUMP-before-ENABLE safety regression: env helper must return false \
             when DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE is unset — default behavior \
             MUST be byte-identical to the fleet/handoff/legacy path."
        );
        std::env::set_var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE", "1");
        assert!(am2_dspic_rejump_before_enable_enabled());
        std::env::set_var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE", "0");
        assert!(!am2_dspic_rejump_before_enable_enabled());
        // The new env is NOT one of the 4 forbidden `a lab unit` env vars.
        for forbidden in [
            "DCENT_AM2_PIC_RESET_AND_START_APP",
            "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
            "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
            "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
        ] {
            assert_ne!(forbidden, "DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE");
        }
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE"),
        }
    }

    /// Skip-SetVoltage-keep-ENABLE (2026-06-07, `a lab unit` standalone cold-engage):
    /// the env helper MUST be default-OFF so the entire fleet/handoff/legacy
    /// `cold_boot_init_with_options` path stays byte-identical (the `0x10`
    /// SetVoltage still fires) when the operator has not opted in. The caller
    /// AND-gates this with the `a lab unit` fingerprint, but the env helper itself is
    /// the primary default-OFF guarantee.
    #[test]
    fn skip_setvoltage_keep_enable_env_helper_default_off() {
        let prev = std::env::var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE");
        assert!(
            !am2_dspic_skip_setvoltage_keep_enable_enabled(),
            "skip-SetVoltage-keep-ENABLE safety regression: env helper must return \
             false when DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE is unset — default \
             behavior MUST be byte-identical to the fleet/handoff/legacy path (the 0x10 \
             SetVoltage still fires)."
        );
        std::env::set_var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE", "1");
        assert!(am2_dspic_skip_setvoltage_keep_enable_enabled());
        std::env::set_var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE", "0");
        assert!(!am2_dspic_skip_setvoltage_keep_enable_enabled());
        // The new env is NOT one of the 4 forbidden `a lab unit` env vars, and it is
        // explicitly DISTINCT from the SENSOR_ONLY gate (which wrongly skips the
        // ENABLE too — ENABLE-DRIFT-DIFF.md proves bosminer DOES send 0x15).
        for forbidden in [
            "DCENT_AM2_PIC_RESET_AND_START_APP",
            "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
            "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
            "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
            "DCENT_AM2_DSPIC_SENSOR_ONLY",
        ] {
            assert_ne!(forbidden, "DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE");
        }
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE"),
        }
    }

    /// Bosminer-minimal ENABLE (2026-06-07, `a lab unit` standalone cold-engage): the
    /// env helper MUST be default-OFF so the entire fleet/handoff/legacy
    /// `cold_boot_init_with_options` path stays byte-identical (flush + sanity
    /// heartbeat + LM75A read + re-JUMP + SetVoltage all fire exactly as today)
    /// when the operator has not opted in. The caller AND-gates this with the
    /// `a lab unit` fingerprint, but the env helper itself is the primary default-OFF
    /// guarantee.
    #[test]
    fn bosminer_minimal_enable_env_helper_default_off() {
        let prev = std::env::var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE");
        assert!(
            !am2_dspic_bosminer_minimal_enable_enabled(),
            "bosminer-minimal-ENABLE safety regression: env helper must return false \
             when DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE is unset — default behavior \
             MUST be byte-identical to the fleet/handoff/legacy path (flush + heartbeat \
             + LM75A + re-JUMP + SetVoltage all still fire)."
        );
        std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE", "1");
        assert!(am2_dspic_bosminer_minimal_enable_enabled());
        std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE", "0");
        assert!(!am2_dspic_bosminer_minimal_enable_enabled());
        // The new env is NOT one of the 4 forbidden `a lab unit` env vars, and it is
        // explicitly DISTINCT from the SENSOR_ONLY gate (which wrongly skips the
        // ENABLE too — the bosminer-minimal window KEEPS the byte-identical 0x15).
        for forbidden in [
            "DCENT_AM2_PIC_RESET_AND_START_APP",
            "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
            "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
            "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
            "DCENT_AM2_DSPIC_SENSOR_ONLY",
        ] {
            assert_ne!(forbidden, "DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE");
        }
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE"),
        }
    }

    /// The keep-alive heartbeat interval defaults to 300 ms (well under the
    /// live-observed ~1.2 s fw=0x89→0x82 drift window), is tunable, and clamps
    /// garbage/out-of-range values to a sane `[50, 1000]` cadence.
    #[test]
    fn postjump_keepalive_interval_default_clamp_and_tunable() {
        let prev = std::env::var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS").ok();

        std::env::remove_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS");
        assert_eq!(
            am2_dspic_keepalive_interval_ms(),
            300,
            "keep-alive interval must default to 300 ms when the env is unset"
        );

        std::env::set_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS", "150");
        assert_eq!(
            am2_dspic_keepalive_interval_ms(),
            150,
            "tunable value honoured"
        );

        // 0 / garbage / negatives fall back to the default (no busy-loop).
        std::env::set_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS", "0");
        assert_eq!(
            am2_dspic_keepalive_interval_ms(),
            300,
            "0 falls back to default"
        );
        std::env::set_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS", "nope");
        assert_eq!(
            am2_dspic_keepalive_interval_ms(),
            300,
            "garbage falls back to default"
        );

        // Out-of-range clamps into [50, 1000].
        std::env::set_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS", "5");
        assert_eq!(
            am2_dspic_keepalive_interval_ms(),
            50,
            "too-small clamps up to 50"
        );
        std::env::set_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS", "9999");
        assert_eq!(
            am2_dspic_keepalive_interval_ms(),
            1000,
            "too-large clamps down to 1000"
        );

        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS"),
        }
    }

    /// Pin the gated chain emission order: when `DCENT_AM2_DSPIC_READ_CONFIG_LATCH=1`
    /// is set, the chain MUST be:
    ///   1..=7. sync heartbeats
    ///   8.    framed RESET
    ///   9.    framed READ-CONFIG-LATCH (NEW)
    ///   10.   framed START_APP
    /// A future refactor that drops the READ-CONFIG-LATCH step OR reorders
    /// it (e.g. before RESET, after START_APP) would re-introduce the
    /// chip-rail-not-engaged failure observed in  LIVE.
    #[test]
    fn wave55k_read_config_latch_inserts_between_reset_and_start_app() {
        let prev = std::env::var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH").ok();
        std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", "1");

        let txs = build_strace_derived_prelude_transactions();
        assert_eq!(
            txs.len(),
            11,
            "Wave-55k Part 2: with READ-CONFIG-LATCH enabled, chain must be \
             11 transactions (8 heartbeats + RESET + READ-CONFIG-LATCH + START_APP). \
             RE-018: 8 sync writes, not 7."
        );

        // Sync heartbeats unchanged.
        for (i, tx) in txs.iter().enumerate().take(STRACE_SYNC_HEARTBEAT_COUNT) {
            let writes: Vec<&Vec<u8>> = tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::WriteByteByByte(b) => Some(b),
                    _ => None,
                })
                .collect();
            assert_eq!(writes.len(), 1);
            assert_eq!(
                writes[0].as_slice(),
                &[0x00],
                "heartbeat {} must be 0x00",
                i
            );
        }

        // RESET at step 7 (index after the 7 heartbeats).
        let reset_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];
        let reset_bytes = reset_tx
            .iter()
            .find_map(|s| {
                if let I2cTransactionStep::WriteByteByByte(b) = s {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .expect("RESET tx must contain a write step");
        assert_eq!(
            reset_bytes.as_slice(),
            &[0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B],
            "step 8 must be framed RESET"
        );

        // READ-CONFIG-LATCH at step 8.
        let read_config_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT + 1];
        let read_config_bytes = read_config_tx
            .iter()
            .find_map(|s| {
                if let I2cTransactionStep::WriteByteByByte(b) = s {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .expect("READ-CONFIG-LATCH tx must contain a write step");
        assert_eq!(
            read_config_bytes.as_slice(),
            &[0xAA, 0x04, 0x00, 0x0A],
            "step 9 must be framed READ-CONFIG-LATCH (4-byte form, no 0x55 preamble)"
        );

        // START_APP at step 9.
        let start_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT + 2];
        let start_bytes = start_tx
            .iter()
            .find_map(|s| {
                if let I2cTransactionStep::WriteByteByByte(b) = s {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .expect("START_APP tx must contain a write step");
        assert_eq!(
            start_bytes.as_slice(),
            &[0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A],
            "step 10 must be framed START_APP (unchanged byte-form)"
        );

        // Restore env.
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH"),
        }
    }

    /// Pin the gated transaction shape for READ-CONFIG-LATCH:
    /// `[SetTimeout, WriteByteByByte(4B), SleepMs(70), Read(1), SleepMs(500)]`.
    /// Matches the  pre-Read settle pattern + the strace-derived
    /// post-Read settle.
    #[test]
    fn wave55k_read_config_latch_tx_shape_is_canonical() {
        let prev = std::env::var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH").ok();
        std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", "1");

        let txs = build_strace_derived_prelude_transactions();
        let read_config_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT + 1];

        let mut saw_set_timeout = false;
        let mut saw_write = false;
        let mut saw_pre_read_sleep = false;
        let mut saw_read = false;
        let mut saw_post_read_sleep = false;
        for step in read_config_tx {
            match step {
                I2cTransactionStep::SetTimeout(_) => saw_set_timeout = true,
                I2cTransactionStep::WriteByteByByte(b) => {
                    assert_eq!(
                        b.len(),
                        4,
                        "READ-CONFIG-LATCH write must be exactly 4 bytes"
                    );
                    saw_write = true;
                }
                I2cTransactionStep::SleepMs(ms) => {
                    if *ms == STRACE_WRITE_TO_ACK_READ_DELAY_MS {
                        saw_pre_read_sleep = true;
                    } else if *ms == STRACE_READ_CONFIG_LATCH_SETTLE_MS {
                        saw_post_read_sleep = true;
                    } else if *ms == STRACE_INTER_ACK_READ_MS {
                        // 2026-06-07 (commit 0a1bfa5a 2-read ACK drain): the
                        // inter-read settle between the two 1-byte ACK reads.
                    } else {
                        panic!(
                            "READ-CONFIG-LATCH tx must only contain SleepMs(70) (pre-Read), \
                             SleepMs(7) (inter-ACK-read) or SleepMs(500) (post-Read), got SleepMs({})",
                            ms
                        );
                    }
                }
                I2cTransactionStep::Read(n) => {
                    assert_eq!(*n, 1, "READ-CONFIG-LATCH ACK drain must be 1 byte");
                    saw_read = true;
                }
                other => panic!("READ-CONFIG-LATCH tx must not contain step: {:?}", other),
            }
        }
        assert!(
            saw_set_timeout,
            "READ-CONFIG-LATCH tx must contain SetTimeout"
        );
        assert!(
            saw_write,
            "READ-CONFIG-LATCH tx must contain WriteByteByByte"
        );
        assert!(
            saw_pre_read_sleep,
            "READ-CONFIG-LATCH tx must contain SleepMs(70) pre-Read settle"
        );
        assert!(
            saw_read,
            "READ-CONFIG-LATCH tx must contain Read(1) to drain ACK"
        );
        assert!(
            saw_post_read_sleep,
            "READ-CONFIG-LATCH tx must contain SleepMs(500) post-Read settle"
        );

        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH"),
        }
    }

    /// Pin the post-Read settle constant. The strace-measured wait between
    /// the READ-CONFIG-LATCH ACK and the GET_VERSION write is +521 ms;
    /// 500 ms is the tight matching value (extra ~21 ms in capture is
    /// scheduler overhead, not a chip requirement).
    #[test]
    fn wave55k_read_config_latch_settle_constant_matches_strace() {
        assert_eq!(
            STRACE_READ_CONFIG_LATCH_SETTLE_MS, 500,
            "Wave-55k Part 2: post-Read settle must be 500 ms (matches the \
             strace's +521 ms wait between READ-CONFIG-LATCH ACK and GET_VERSION \
             write at bosminer-i2c0-slave20.txt:23-24 minus scheduler overhead)"
        );
    }

    ///  wire bytes must NEVER use bare 3-byte opcodes (those are the
    /// S9-era variant). Catches a future agent accidentally collapsing the
    /// new variant onto the old one.
    #[test]
    fn wave28_framed_frames_are_never_3_bytes() {
        assert_eq!(
            STRACE_RESET_FRAME_FRAMED.len(),
            6,
            "RESET must be 6-byte framed"
        );
        assert_eq!(
            STRACE_START_APP_FRAME_FRAMED.len(),
            6,
            "START_APP must be 6-byte framed"
        );
        // The old bare forms must NOT match the new framed forms.
        assert_ne!(
            &STRACE_RESET_FRAME_FRAMED[..3],
            &[0x55, 0xAA, 0x07][..],
            "framed RESET must not begin with the bare 3-byte form"
        );
        assert_ne!(
            &STRACE_START_APP_FRAME_FRAMED[..3],
            &[0x55, 0xAA, 0x06][..],
            "framed START_APP must not begin with the bare 3-byte form"
        );
    }

    // =======================================================================
    //  Rung 2 — bounded JUMP-only re-verify (2026-06-07) structural pins
    // =======================================================================

    /// Default-DISABLED safety pin. The env helper MUST return `0` (= disabled)
    /// when `DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX` is unset, so the cold-engage
    /// path is byte-identical to today for the fleet/handoff/legacy paths.
    #[test]
    fn jump_reverify_max_env_helper_default_disabled() {
        let prev = std::env::var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX");
        assert_eq!(
            am2_dspic_jump_reverify_max(),
            0,
            "rung 2 safety regression: env helper must return 0 (disabled) when \
             DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX is unset — default behavior MUST be \
             byte-identical to the fleet/handoff/legacy cold-engage path."
        );
        std::env::set_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX", "6");
        assert_eq!(am2_dspic_jump_reverify_max(), 6);
        std::env::set_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX", "0");
        assert_eq!(
            am2_dspic_jump_reverify_max(),
            0,
            "explicit 0 must also disable"
        );
        // Garbage / empty must fail-safe to 0 (disabled), never panic.
        std::env::set_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX", "not-a-number");
        assert_eq!(am2_dspic_jump_reverify_max(), 0);
        std::env::set_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX", "");
        assert_eq!(am2_dspic_jump_reverify_max(), 0);
        std::env::set_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX", "  4  ");
        assert_eq!(
            am2_dspic_jump_reverify_max(),
            4,
            "trimmed whitespace parses"
        );
        // The new env is NOT one of the 4 forbidden `a lab unit` env vars.
        for forbidden in [
            "DCENT_AM2_PIC_RESET_AND_START_APP",
            "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
            "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
            "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
        ] {
            assert_ne!(forbidden, "DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX");
        }
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX"),
        }
    }

    /// The JUMP-only re-verify chain MUST be `STRACE_SYNC_HEARTBEAT_COUNT`
    /// per-byte 0x00 flush heartbeats followed by EXACTLY ONE framed JUMP — and
    /// MUST NEVER contain the framed RESET frame (the load-bearing safety
    /// invariant: rung 2 is JUMP-only, never a second RESET).
    #[test]
    fn jump_reverify_chain_has_no_reset_and_one_jump() {
        let txs = build_jump_only_reverify_transactions();
        assert_eq!(
            txs.len(),
            STRACE_SYNC_HEARTBEAT_COUNT + 1,
            "JUMP-only re-verify must be {} flush heartbeats + 1 framed JUMP",
            STRACE_SYNC_HEARTBEAT_COUNT
        );

        // Flush heartbeats unchanged: each is a single 0x00 byte write.
        for (i, tx) in txs.iter().enumerate().take(STRACE_SYNC_HEARTBEAT_COUNT) {
            let writes: Vec<&Vec<u8>> = tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::WriteByteByByte(b) => Some(b),
                    _ => None,
                })
                .collect();
            assert_eq!(writes.len(), 1, "heartbeat {} must have one write", i);
            assert_eq!(
                writes[0].as_slice(),
                &[0x00],
                "heartbeat {} must be 0x00",
                i
            );
        }

        // The final transaction MUST be the framed JUMP — never the RESET.
        let jump_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];
        let jump_bytes = jump_tx
            .iter()
            .find_map(|s| {
                if let I2cTransactionStep::WriteByteByByte(b) = s {
                    Some(b.clone())
                } else {
                    None
                }
            })
            .expect("JUMP tx must contain a write step");
        assert_eq!(
            jump_bytes.as_slice(),
            &[0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A],
            "rung 2 final step must be the framed JUMP/START_APP (0x06), byte-identical \
             to the warmup START_APP step"
        );

        // CRITICAL safety pin: NO transaction in the entire chain may write the
        // framed RESET frame [55 AA 04 07 00 0B]. A second RESET landing the
        // chip in 0x82 is the destructive/downgrade case the bible forbids.
        for (i, tx) in txs.iter().enumerate() {
            for step in tx {
                if let I2cTransactionStep::WriteByteByByte(b) = step {
                    assert_ne!(
                        b.as_slice(),
                        STRACE_RESET_FRAME_FRAMED.as_slice(),
                        "rung 2 SAFETY VIOLATION: transaction {} contains a framed RESET \
                         — JUMP-only re-verify must NEVER issue a second RESET",
                        i
                    );
                    // Also reject the bare 3-byte RESET form just in case.
                    assert_ne!(
                        b.as_slice(),
                        &[0x55, 0xAA, 0x07][..],
                        "rung 2 SAFETY VIOLATION: transaction {} contains a bare RESET",
                        i
                    );
                }
            }
        }
    }

    /// The framed-JUMP transaction in the re-verify chain MUST carry the same
    /// 2-separate-reads ACK drain (echo, then 0x01 status) + 500 ms post-JUMP
    /// settle as the warmup START_APP step. A single `Read(2)` would leave the
    /// chip mid-ack and the subsequent GET_VERSION goes all-0xFF (the very bug
    /// that commit 0a1bfa5a fixed).
    #[test]
    fn jump_reverify_jump_tx_has_two_read_ack_drain() {
        let txs = build_jump_only_reverify_transactions();
        let jump_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];

        let read_lens: Vec<usize> = jump_tx
            .iter()
            .filter_map(|s| match s {
                I2cTransactionStep::Read(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(
            read_lens.len(),
            STRACE_FRAMED_ACK_READS,
            "JUMP tx must drain the ACK as {} SEPARATE reads (cmd echo, then 0x01 status)",
            STRACE_FRAMED_ACK_READS
        );
        for n in &read_lens {
            assert_eq!(*n, STRACE_FRAMED_ACK_LEN, "each ACK read must be 1 byte");
        }

        let sleeps: Vec<u64> = jump_tx
            .iter()
            .filter_map(|s| match s {
                I2cTransactionStep::SleepMs(ms) => Some(*ms),
                _ => None,
            })
            .collect();
        assert!(
            sleeps.contains(&STRACE_WRITE_TO_ACK_READ_DELAY_MS),
            "JUMP tx must contain the Wave-44 70 ms pre-Read settle"
        );
        assert!(
            sleeps.contains(&STRACE_INTER_ACK_READ_MS),
            "JUMP tx must contain the inter-ACK-read settle between the two 1-byte reads"
        );
        assert!(
            sleeps.contains(&STRACE_START_APP_SETTLE_MS),
            "JUMP tx must contain the 500 ms post-JUMP settle"
        );
    }

    /// The JUMP-only re-verify chain MUST be structurally identical to the
    /// flush + START_APP step of the warmup chain (with READ-CONFIG-LATCH
    /// disabled). This proves rung 2 reuses the proven warmup wire shape and
    /// only drops the RESET — it never invents a divergent JUMP encoding.
    /// (`I2cTransactionStep` has no `PartialEq`, so we summarize each tx into
    /// comparable byte/read/sleep/timeout vectors.)
    #[test]
    fn jump_reverify_matches_warmup_flush_plus_start_app() {
        type TxSummary = (Vec<Vec<u8>>, Vec<usize>, Vec<u64>, Vec<u32>);
        fn summarize(tx: &[I2cTransactionStep]) -> TxSummary {
            let mut writes = Vec::new();
            let mut reads = Vec::new();
            let mut sleeps = Vec::new();
            let mut timeouts = Vec::new();
            for step in tx {
                match step {
                    I2cTransactionStep::WriteByteByByte(b) => writes.push(b.clone()),
                    I2cTransactionStep::Read(n) => reads.push(*n),
                    I2cTransactionStep::SleepMs(ms) => sleeps.push(*ms),
                    I2cTransactionStep::SetTimeout(j) => timeouts.push(*j),
                    _ => {}
                }
            }
            (writes, reads, sleeps, timeouts)
        }

        let prev = std::env::var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");

        let warmup = build_strace_derived_prelude_transactions();
        let reverify = build_jump_only_reverify_transactions();

        // Flush heartbeats are the first STRACE_SYNC_HEARTBEAT_COUNT txs in both.
        for i in 0..STRACE_SYNC_HEARTBEAT_COUNT {
            assert_eq!(
                summarize(&warmup[i]),
                summarize(&reverify[i]),
                "flush heartbeat {} must be byte-identical between warmup and re-verify",
                i
            );
        }
        // The warmup's LAST tx is START_APP (with READ-CONFIG-LATCH off the
        // warmup is 8 flush + RESET + START_APP). The re-verify's last tx is the
        // JUMP — they must be structurally identical.
        let warmup_start_app = warmup.last().expect("warmup has a START_APP tx");
        let reverify_jump = reverify.last().expect("re-verify has a JUMP tx");
        assert_eq!(
            summarize(warmup_start_app),
            summarize(reverify_jump),
            "rung 2 JUMP step must be byte-identical to the warmup START_APP step"
        );

        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH"),
        }
    }

    /// EEPROM denylist fail-closed: `am2_pic_jump_only_reverify` must refuse any
    /// address in `0x50..=0x57` before touching the bus (it can't actually open
    /// a bus here, but the denylist check returns Err before any I/O — we assert
    /// the guard predicate the function uses).
    #[test]
    fn jump_reverify_refuses_eeprom_denylist_addrs() {
        for addr in 0x50u8..=0x57 {
            assert!(
                is_eeprom_denylist_addr(addr),
                "0x{:02X} must be in the EEPROM denylist range the re-verify refuses",
                addr
            );
        }
        // dsPIC addresses are NOT denied.
        for addr in [0x20u8, 0x21, 0x22] {
            assert!(
                !is_eeprom_denylist_addr(addr),
                "dsPIC 0x{:02X} must NOT be denylisted",
                addr
            );
        }
    }

    // =======================================================================
    //  Rung 2 (RESET→JUMP variant) — full flush→RESET→JUMP re-verify
    //  structural pins (2026-06-07, `a lab unit` standalone LIVE TEST 2)
    // =======================================================================

    /// Default-DISABLED safety pin. The RESET→JUMP re-verify count env helper
    /// MUST return `0` (= disabled) when `DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX`
    /// is unset, so the cold-engage path is byte-identical to today for the
    /// fleet/handoff/legacy paths. With the env unset → helper returns 0 → the
    /// caller's loop is never entered → behavior unchanged.
    #[test]
    fn reset_jump_reverify_max_env_helper_default_disabled() {
        let prev = std::env::var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX");
        assert_eq!(
            am2_dspic_reset_jump_reverify_max(),
            0,
            "RESET→JUMP re-verify safety regression: env helper must return 0 \
             (disabled) when DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX is unset — \
             default behavior MUST be byte-identical to the fleet/handoff/legacy \
             cold-engage path."
        );
        std::env::set_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX", "4");
        assert_eq!(am2_dspic_reset_jump_reverify_max(), 4);
        std::env::set_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX", "0");
        assert_eq!(
            am2_dspic_reset_jump_reverify_max(),
            0,
            "explicit 0 must also disable"
        );
        // Garbage / empty must fail-safe to 0 (disabled), never panic.
        std::env::set_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX", "not-a-number");
        assert_eq!(am2_dspic_reset_jump_reverify_max(), 0);
        std::env::set_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX", "");
        assert_eq!(am2_dspic_reset_jump_reverify_max(), 0);
        std::env::set_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX", "  4  ");
        assert_eq!(
            am2_dspic_reset_jump_reverify_max(),
            4,
            "trimmed whitespace parses"
        );
        // The new env is NOT one of the 4 forbidden `a lab unit` env vars.
        for forbidden in [
            "DCENT_AM2_PIC_RESET_AND_START_APP",
            "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
            "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
            "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
        ] {
            assert_ne!(forbidden, "DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX");
        }
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX"),
        }
    }

    /// Post-RESET dwell env helper: default 1000 ms (the jig value), clamped to
    /// `[100, 5000]`, fail-safe to default on garbage/empty/unset.
    #[test]
    fn reset_dwell_ms_env_helper_default_1000_and_clamped() {
        let prev = std::env::var("DCENT_AM2_DSPIC_RESET_DWELL_MS").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_RESET_DWELL_MS");
        assert_eq!(
            am2_dspic_reset_dwell_ms(),
            1000,
            "default post-RESET dwell must be 1000 ms (the jig's ~1 s value; the \
             warmup's 500 ms may be too short to fully reset the cold chip)"
        );
        assert_eq!(RESET_JUMP_REVERIFY_DEFAULT_DWELL_MS, 1000);
        // Explicit values parse.
        std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", "1500");
        assert_eq!(am2_dspic_reset_dwell_ms(), 1500);
        std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", "500");
        assert_eq!(am2_dspic_reset_dwell_ms(), 500);
        // Clamp: below 100 → 100; above 5000 → 5000.
        std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", "10");
        assert_eq!(
            am2_dspic_reset_dwell_ms(),
            100,
            "under-dwell clamps up to 100"
        );
        std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", "999999");
        assert_eq!(
            am2_dspic_reset_dwell_ms(),
            5000,
            "over-dwell clamps down to 5000 (don't stall the bus for minutes)"
        );
        // Garbage / empty fail-safe to the default 1000.
        std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", "not-a-number");
        assert_eq!(am2_dspic_reset_dwell_ms(), 1000);
        std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", "");
        assert_eq!(am2_dspic_reset_dwell_ms(), 1000);
        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_RESET_DWELL_MS", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_RESET_DWELL_MS"),
        }
    }

    /// The RESET→JUMP re-verify chain MUST be `STRACE_SYNC_HEARTBEAT_COUNT`
    /// per-byte 0x00 flush heartbeats followed by EXACTLY ONE framed RESET and
    /// EXACTLY ONE framed JUMP (in that order). Counts the RESET/JUMP frames to
    /// prove there is exactly one of each.
    #[test]
    fn reset_jump_chain_has_exactly_one_reset_and_one_jump() {
        let txs = build_reset_jump_reverify_transactions(1000);
        assert_eq!(
            txs.len(),
            STRACE_SYNC_HEARTBEAT_COUNT + 2,
            "RESET→JUMP re-verify must be {} flush heartbeats + 1 RESET + 1 JUMP",
            STRACE_SYNC_HEARTBEAT_COUNT
        );

        // Flush heartbeats unchanged: each is a single 0x00 byte write.
        for (i, tx) in txs.iter().enumerate().take(STRACE_SYNC_HEARTBEAT_COUNT) {
            let writes: Vec<&Vec<u8>> = tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::WriteByteByByte(b) => Some(b),
                    _ => None,
                })
                .collect();
            assert_eq!(writes.len(), 1, "heartbeat {} must have one write", i);
            assert_eq!(
                writes[0].as_slice(),
                &[0x00],
                "heartbeat {} must be 0x00",
                i
            );
        }

        // Count RESET and JUMP frames across the WHOLE chain.
        let mut reset_count = 0usize;
        let mut jump_count = 0usize;
        for tx in &txs {
            for step in tx {
                if let I2cTransactionStep::WriteByteByByte(b) = step {
                    if b.as_slice() == STRACE_RESET_FRAME_FRAMED.as_slice() {
                        reset_count += 1;
                    }
                    if b.as_slice() == STRACE_START_APP_FRAME_FRAMED.as_slice() {
                        jump_count += 1;
                    }
                }
            }
        }
        assert_eq!(
            reset_count, 1,
            "RESET→JUMP chain must contain EXACTLY ONE framed RESET [55 AA 04 07 00 0B]"
        );
        assert_eq!(
            jump_count, 1,
            "RESET→JUMP chain must contain EXACTLY ONE framed JUMP [55 AA 04 06 00 0A]"
        );

        // Order: RESET is the tx at index STRACE_SYNC_HEARTBEAT_COUNT, JUMP is last.
        let reset_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];
        let reset_bytes = reset_tx
            .iter()
            .find_map(|s| match s {
                I2cTransactionStep::WriteByteByByte(b) => Some(b.clone()),
                _ => None,
            })
            .expect("RESET tx must contain a write step");
        assert_eq!(
            reset_bytes.as_slice(),
            &[0x55, 0xAA, 0x04, 0x07, 0x00, 0x0B],
            "second-to-last tx must be the framed RESET"
        );
        let jump_tx = txs.last().expect("chain has a JUMP tx");
        let jump_bytes = jump_tx
            .iter()
            .find_map(|s| match s {
                I2cTransactionStep::WriteByteByByte(b) => Some(b.clone()),
                _ => None,
            })
            .expect("JUMP tx must contain a write step");
        assert_eq!(
            jump_bytes.as_slice(),
            &[0x55, 0xAA, 0x04, 0x06, 0x00, 0x0A],
            "last tx must be the framed JUMP/START_APP"
        );
    }

    /// Both the RESET and JUMP transactions MUST carry the 2-separate-reads ACK
    /// drain (echo, then 0x01 status) + the  70 ms pre-Read settle. A
    /// single `Read(2)` would leave the chip mid-ack and the subsequent
    /// GET_VERSION goes all-0xFF (the very bug commit 0a1bfa5a fixed).
    #[test]
    fn reset_jump_reset_and_jump_have_two_read_ack_drain() {
        let txs = build_reset_jump_reverify_transactions(1000);
        for (which, tx) in [
            ("RESET", &txs[STRACE_SYNC_HEARTBEAT_COUNT]),
            ("JUMP", txs.last().expect("chain has a JUMP tx")),
        ] {
            let read_lens: Vec<usize> = tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::Read(n) => Some(*n),
                    _ => None,
                })
                .collect();
            assert_eq!(
                read_lens.len(),
                STRACE_FRAMED_ACK_READS,
                "{} tx must drain the ACK as {} SEPARATE reads (cmd echo, then 0x01 status)",
                which,
                STRACE_FRAMED_ACK_READS
            );
            for n in &read_lens {
                assert_eq!(
                    *n, STRACE_FRAMED_ACK_LEN,
                    "{} each ACK read must be 1 byte",
                    which
                );
            }
            let sleeps: Vec<u64> = tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::SleepMs(ms) => Some(*ms),
                    _ => None,
                })
                .collect();
            assert!(
                sleeps.contains(&STRACE_WRITE_TO_ACK_READ_DELAY_MS),
                "{} tx must contain the Wave-44 70 ms pre-Read settle",
                which
            );
            assert!(
                sleeps.contains(&STRACE_INTER_ACK_READ_MS),
                "{} tx must contain the inter-ACK-read settle between the two 1-byte reads",
                which
            );
        }

        // RESET tx ends with the configurable post-RESET dwell; JUMP tx ends
        // with the 500 ms post-JUMP settle.
        let reset_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];
        let reset_sleeps: Vec<u64> = reset_tx
            .iter()
            .filter_map(|s| match s {
                I2cTransactionStep::SleepMs(ms) => Some(*ms),
                _ => None,
            })
            .collect();
        assert!(
            reset_sleeps.contains(&1000),
            "RESET tx must contain the configurable post-RESET dwell (1000 ms here)"
        );
        let jump_tx = txs.last().unwrap();
        let jump_sleeps: Vec<u64> = jump_tx
            .iter()
            .filter_map(|s| match s {
                I2cTransactionStep::SleepMs(ms) => Some(*ms),
                _ => None,
            })
            .collect();
        assert!(
            jump_sleeps.contains(&STRACE_START_APP_SETTLE_MS),
            "JUMP tx must contain the 500 ms post-JUMP settle"
        );
    }

    /// The configurable post-RESET dwell MUST appear in the RESET transaction and
    /// MUST differ from the warmup's hard-coded 500 ms when a different value is
    /// passed (proves the dwell is actually wired, not ignored).
    #[test]
    fn reset_jump_reset_dwell_is_configurable() {
        for dwell in [600u64, 1000, 2000] {
            let txs = build_reset_jump_reverify_transactions(dwell);
            let reset_tx = &txs[STRACE_SYNC_HEARTBEAT_COUNT];
            // The LAST SleepMs in the RESET tx is the post-RESET dwell.
            let last_sleep = reset_tx
                .iter()
                .filter_map(|s| match s {
                    I2cTransactionStep::SleepMs(ms) => Some(*ms),
                    _ => None,
                })
                .last()
                .expect("RESET tx must contain a trailing SleepMs (the post-RESET dwell)");
            assert_eq!(
                last_sleep, dwell,
                "RESET tx post-RESET dwell must equal the passed reset_dwell_ms ({} ms)",
                dwell
            );
        }
    }

    /// When `reset_dwell_ms == STRACE_RESET_TO_START_APP_DELAY_MS` (500), the
    /// RESET→JUMP chain MUST be structurally BYTE-IDENTICAL to the warmup's
    /// flush + RESET + START_APP chain (READ-CONFIG-LATCH disabled). This proves
    /// the new helper reuses the proven warmup wire shape and only makes the
    /// post-RESET dwell configurable. (`I2cTransactionStep` has no `PartialEq`,
    /// so we summarize each tx into comparable byte/read/sleep/timeout vectors.)
    #[test]
    fn reset_jump_with_500ms_dwell_matches_warmup_chain() {
        type TxSummary = (Vec<Vec<u8>>, Vec<usize>, Vec<u64>, Vec<u32>);
        fn summarize(tx: &[I2cTransactionStep]) -> TxSummary {
            let mut writes = Vec::new();
            let mut reads = Vec::new();
            let mut sleeps = Vec::new();
            let mut timeouts = Vec::new();
            for step in tx {
                match step {
                    I2cTransactionStep::WriteByteByByte(b) => writes.push(b.clone()),
                    I2cTransactionStep::Read(n) => reads.push(*n),
                    I2cTransactionStep::SleepMs(ms) => sleeps.push(*ms),
                    I2cTransactionStep::SetTimeout(j) => timeouts.push(*j),
                    _ => {}
                }
            }
            (writes, reads, sleeps, timeouts)
        }

        let prev = std::env::var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH").ok();
        std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH");

        let warmup = build_strace_derived_prelude_transactions();
        let reset_jump = build_reset_jump_reverify_transactions(STRACE_RESET_TO_START_APP_DELAY_MS);

        assert_eq!(
            warmup.len(),
            reset_jump.len(),
            "with READ-CONFIG-LATCH off, the warmup (8 flush + RESET + START_APP) and the \
             RESET→JUMP chain (8 flush + RESET + JUMP) must have the same length"
        );
        for (i, (w, r)) in warmup.iter().zip(reset_jump.iter()).enumerate() {
            assert_eq!(
                summarize(w),
                summarize(r),
                "tx {} must be byte-identical between the warmup chain and the RESET→JUMP \
                 chain when reset_dwell_ms == 500 (the warmup's hard-coded value)",
                i
            );
        }

        match prev {
            Some(v) => std::env::set_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH", v),
            None => std::env::remove_var("DCENT_AM2_DSPIC_READ_CONFIG_LATCH"),
        }
    }

    /// EEPROM denylist fail-closed: `am2_pic_reset_jump_reverify` must refuse any
    /// address in `0x50..=0x57` before touching the bus (we assert the guard
    /// predicate the function uses, since no live handle is available in tests).
    #[test]
    fn reset_jump_reverify_refuses_eeprom_denylist_addrs() {
        for addr in 0x50u8..=0x57 {
            assert!(
                is_eeprom_denylist_addr(addr),
                "0x{:02X} must be in the EEPROM denylist range the RESET→JUMP re-verify refuses",
                addr
            );
        }
        for addr in [0x20u8, 0x21, 0x22] {
            assert!(
                !is_eeprom_denylist_addr(addr),
                "dsPIC 0x{:02X} must NOT be denylisted",
                addr
            );
        }
    }
}
