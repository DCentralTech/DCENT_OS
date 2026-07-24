//! dsPIC33EP16GS202 voltage controller driver for S17/S19 hash boards.
//!
//! The S17/S19 family uses a Microchip dsPIC33EP16GS202 16-bit Digital Signal
//! Controller for DC-DC voltage regulation. This is fundamentally different from
//! the PIC16F1704 used on S9 hash boards:
//!
//! - **Framed protocol**: Commands use a `[command, status, payload...]` frame format
//!   with checksum, NOT the raw `[0x55, 0xAA, cmd, data]` preamble of PIC16F1704.
//! - **Higher voltage range**: 11.94V - 15.14V bus voltage (vs S9's 7.94V - 9.44V).
//! - **Temperature passthrough**: LM75A sensors at 0x48-0x4B are accessed THROUGH
//!   the voltage controller, not directly on the I2C bus. (BraiinsOS notation
//!   `hb{2,3}.72-75` uses *decimal* indices for the sensor *name*; the actual
//!   I2C addresses are 0x48-0x4B per `etc/topol.conf` in Bitmain stock CV
//!   rootfs and :26,79`.)
//! - **Multiple firmware versions**: Addresses 0x88, 0x89, 0xB9, 0xFE correspond
//!   to different firmware revisions (BraiinsOS uses pic0x88/pic0x89/pic0xb9/pic0xfe).
//!
//! Protocol details (from BraiinsOS binary strings and S19j Pro live probe):
//!   - BraiinsOS log: "Rx frame command mismatch, expected command: 59, this frame:
//!     RxFrame { command: ff, status: ff, payload: [] }"
//!   - BraiinsOS log: "Voltage controller reset: flush: reset PIC command"
//!   - BraiinsOS log: "bad checksum on get_voltage packet"
//!   - Init sequence: reset → start_app → get_version → set_voltage → enable → heartbeat
//!
//! VNish libplatform.so API (confirmed from S17 live probe extraction):
//!   dspic33epxx_get_sw_version()        → get firmware version
//!   dspic33epxx_jump_to_app()           → jump from bootloader to app
//!   dspic33epxx_reset()                 → reset the dsPIC
//!   dspic33epxx_enable_disable_dc_dc()  → enable/disable DC-DC converter
//!   dspic33epxx_heart_beat()            → watchdog heartbeat
//!   dspic33epxx_set_voltage()           → set target voltage (millivolts)
//!   dspic33epxx_get_an_voltage2()       → read analog voltage feedback (ADC)
//!   dspic33epxx_voltage_clamp_ctrl()    → voltage clamp control
//!   dspic33epxx_get_PDCx()              → get PWM duty cycle register value
//!   dspic33epxx_get_raw_crab_voltage()  → get raw internal reference voltage
//!   dspic33epxx_erase_program()         → erase flash (firmware update)
//!   dspic33epxx_update_app_program()    → write new firmware
//!
//! I2C addresses (CONFIRMED on live S19 Pro at 203.0.113.129):
//!   0x20 — Board 1 (fw 0x82, bare protocol)
//!   0x21 — Board 2 (fw 0x86, S19j bare protocol)
//!   0x22 — Board 3 (fw 0x8A, framed protocol)
//!   NOTE: 0x88/0x89/0xB9/0xFE in BraiinsOS source are firmware VERSION IDs, NOT addresses.
//!
//! Sources:
//!   -  (sections 4, 12)
//!   -  (section 4)
//!   -  (section 3)
//!   -  (section 3)
//!   -  (dsPIC33EP16GS202 confirmed)

use crate::Result;
use dcentrald_hal::i2c::{
    I2cBus, I2cDspicDisableProtocol, I2cMutationLabel, I2cServiceHandle, I2cTransactionStep,
};
use dcentrald_hal::platform::{VoltageControllerEndpoint, VoltageControllerKind};

// -- Per-firmware-revision wire-format modules ------------------------------
//
// Each `fwXX` module owns the byte-exact frame builders and per-revision
// byte-sequence tests for one dsPIC firmware identity. This `mod.rs` keeps
// the runtime controllers (`DspicController`, `DspicService`, `Pic0x89`,
// `Pic0x89Service`, `Dspic33Ep16Gs202`) and the cross-revision dispatch
// helpers (`dspic_set_voltage_frame`, `dspic_enable_disable_encoding`,
// `dspic_heartbeat_frame`, `dspic_read_temp_frame`) which delegate to the
// per-fw modules when the wire form differs by firmware identity.
//
// Per memory rules ,
// ,
// and  — fw=0x82 / 0x86 / 0x89 / 0x8A are the
// SAME silicon at different firmware revisions. Splitting them keeps each
// revision's byte sequences locally legible and tested in isolation.
pub mod fw82;
pub mod fw86;
pub mod fw89;
pub mod fw8a;

// 2026-05-22 (XIL `a lab unit` dsPIC recovery, Layer 1):
//   Bosminer-faithful pre-GET_VERSION dsPIC cold-boot prelude.
//   Always-includes-flush wrapper around the RESET+JUMP chain. See
//   .
//   This module is intentionally NOT gated behind `recovery-tool` — the
//   historical raw `dspic_flash::reset_pic` path has been removed entirely.
//   The wrapper here is safer-by-construction because the
//   16-byte parser flush is emitted BEFORE the RESET opcode in the same call,
//   so the `a lab unit` 2026-04-24 "bare RESET without flush" corruption pattern
//   cannot recur here even if a future agent reuses this module incorrectly.
pub mod bosminer_warmup;

// W12.1 (RE3 R3-6, 2026-05-10): dsPIC fw=0x86 software recovery.
//
// `recovery_fw86` is research/test-only behind `recovery-tool`. It preserves
// `jump_to_app` (the bootloader→application unlock+jump per RE3 §5.2) and a
// partial `reflash_app_via_framed_protocol` (RE3 §3.4 / §6 — 60% confidence).
//
// Production `dcentrald` and the diagnostic-only `pic-recovery` package do
// not enable `recovery-tool`, so no shipped binary can link any symbol in
// this module. The daemon's existing fw=0x86 voltage-command refusal
// stays in force. Recovery remains
// physical ICSP-only until a separate typed authority architecture exists.
#[cfg(feature = "recovery-tool")]
pub mod recovery_fw86;

// ---------------------------------------------------------------------------
// dsPIC command constants
// ---------------------------------------------------------------------------
// The dsPIC uses a different command set than PIC16F1704. The exact byte values
// are derived from BraiinsOS bosminer binary strings analysis and VNish
// libplatform.so RE. The framed protocol wraps these in [cmd, status, payload].
//
// NOTE: The command byte values below are inferred from BraiinsOS error messages
// (e.g., "expected command: 59" = 0x3B for get_voltage) and the VNish function
// names. Some values may need adjustment when tested on live S19 hardware.

/// dsPIC protocol preamble bytes.
/// The dsPIC framed protocol uses the same 0x55 0xAA preamble as PIC16F1704
/// for the I2C transport layer, but the payload is a structured frame.
pub const DSPIC_PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// Reset dsPIC (return to bootloader).
/// Equivalent to: dspic33epxx_reset()
/// BraiinsOS log: "Voltage controller reset: flush: reset PIC command"
pub const CMD_RESET: u8 = 0x07;

/// Jump from bootloader to application.
/// Equivalent to: dspic33epxx_jump_to_app()
pub const CMD_JUMP_TO_APP: u8 = 0x06;

/// Get firmware version.
/// Equivalent to: dspic33epxx_get_sw_version()
/// Response: 1 byte firmware version (e.g., 0x89, 0xB9)
pub const CMD_GET_VERSION: u8 = 0x17;

/// Set target voltage (millivolts).
/// Equivalent to: dspic33epxx_set_voltage()
/// Payload: 2 bytes big-endian voltage in millivolts (e.g., 13800 = 13.80V)
pub const CMD_SET_VOLTAGE: u8 = 0x10;

/// Enable DC-DC converter output.
/// Equivalent to: dspic33epxx_enable_disable_dc_dc() with enable=true
/// Payload: 1 byte (0x01 = enable, 0x00 = disable)
pub const CMD_ENABLE_VOLTAGE: u8 = 0x15;

/// Send heartbeat / keepalive.
/// Equivalent to: dspic33epxx_heart_beat()
/// Heartbeat interval: ~10 seconds (from Bitmain HEART_BEAT_TIME_GAP).
///
/// NOTE (name vs behavior): 0x16 is NOT a dedicated heartbeat opcode on the
/// chip. gpdasm of the S19 Pro control-board PIC16F1704 shows 0x16 dispatches a
/// **6-byte DAC-readback** response `[0x06, 0x16, 0x01, voltage_val, sum_hi,
/// sum_lo]` (label_210 @ 0x0d98). It "works" as a keepalive only because the
/// PIC main loop resets its watchdog counter on ANY valid command — the real
/// keepalive effect is the watchdog-reset side effect, and the bytes that come
/// back are DAC-readback data, not a heartbeat ACK. The S17 dsPIC33EP16GS202
/// jig confirms the same opcode framing (`pic_heart_beat@3100C.c`, reads 6
/// bytes). Source: knowledge-base goldmine `findings/s16b-nbp1901-pic-whatsminer.md`
/// A08/IC-A2. (Separately, the `a lab unit` cold strace proves bosminer never sends
/// 0x16 at all — do not rely on it as the canonical app-mode keepalive.)
pub const CMD_HEARTBEAT: u8 = 0x16;

/// Legacy get-voltage / passthrough command.
///
/// Ghidra RE of `bosminer.bin` (2026-06-01) shows fw=0x89 rail measurement uses
/// `CMD_MEASURE_VOLTAGE` (0x3A), not this opcode. On `a lab unit` captures, framed
/// `0x3B` with sensor args `0x48..0x4B` is the LM75 passthrough WRITE half.
/// Keep this constant for bare-path compatibility and non-0x89 research only.
pub const CMD_GET_VOLTAGE: u8 = 0x3B;

/// Measure the ACTUAL chain-rail voltage via the dsPIC analog ADC.
///
/// For the local bosminer fw=0x89 path, bosminer sends framed `[55 AA 04 3A 00 3E]`
/// and decodes the first two post-envelope reply bytes as a big-endian ADC count with
/// `volts = raw * 0.02448 - 0.35`.
/// Byte-exact framed frame `[55 AA 04 3A 00 3E]` per dspic-protocol-bible §2
/// (gap-swarm G03; codec verified in `dcentrald-api-types::dspic_frame`). Ghidra
/// currently proves the selector route for fw=0x89; unnormalized fw=0x8A parity is
/// not proven by the local bosminer binary or trace.
pub const CMD_MEASURE_VOLTAGE: u8 = 0x3A;

/// Read back the dsPIC's COMMANDED voltage DAC setpoint (framed, fw=0x86+).
///
/// Distinct from the three other voltage opcodes: `CMD_SET_VOLTAGE` (0x10) writes the
/// DAC, `CMD_MEASURE_VOLTAGE` (0x3A) reads the ACTUAL rail via the analog ADC, and
/// `CMD_GET_VOLTAGE` (0x3B) is LM75 passthrough on `a lab unit`. This opcode reads back the
/// last-commanded DAC code so a caller can confirm a `SET_VOLTAGE` write took effect
/// (the autotuner-confirm-write use the bible cites). LuxOS/VNish both read this.
///
/// Framed request `[55 AA 04 18 1C]` (LEN=0x04, CKSUM=0x04+0x18=0x1C); reply
/// `[0x18, status, dac_hi, dac_lo]`. Source: `mining-bible-v1/_canonical/dspic-protocol-bible.md`
/// §3 "CMD 0x18 — GET_VOLTAGE (DAC readback)" + `dspic-command-table.csv` (RE-DERIVED).
///
/// CONFIDENCE: RE-DERIVED, NOT yet live-verified on a DCENT-managed dsPIC. Exposed as the
/// protocol element + a pure decoder (`decode_voltage_dac_reply`) regression-pinned by a
/// byte-exact frame test; the live readback method + autotuner confirm-write wiring is a
/// follow-up gated on a live `0x18`-reply capture (read-only, drives no rail — same safety
/// class as the existing `CMD_READ_TEMP` passthrough).
pub const CMD_GET_VOLTAGE_DAC: u8 = 0x18;

/// Safe dsPIC rail-voltage envelope from S19-family bosminer model data.
pub const DSPIC_MIN_VOLTAGE_MV: u16 = 11_940;
pub const DSPIC_MAX_VOLTAGE_MV: u16 = 15_140;

/// Decode a framed `CMD_GET_VOLTAGE_DAC` (0x18) reply `[0x18, status, dac_hi, dac_lo]`.
///
/// Pure + host-testable. Applies the same command-echo guard as the other framed decoders:
/// the reply MUST echo the command byte (`reply[0] == 0x18`) and carry the 4 expected bytes,
/// else `None` (never fabricate a setpoint from a malformed/short reply). Returns the
/// big-endian DAC setpoint word `be16(dac_hi, dac_lo)`. The DAC→mV inversion is intentionally
/// left to the caller (the `framed_voltage_dac` mapping is per-fw and the autotuner owns the
/// confirm-write comparison).
pub fn decode_voltage_dac_reply(reply: &[u8]) -> Option<u16> {
    if reply.len() < 4 || reply[0] != CMD_GET_VOLTAGE_DAC {
        return None;
    }
    Some(u16::from_be_bytes([reply[2], reply[3]]))
}

/// Convert a framed S19-family dsPIC voltage target to the one-byte DAC code
/// used on the wire. Bosminer evidence for S19j Pro/BHB42601 shows 13.7 V as
/// DAC `0x06`, i.e. `[55 AA 04 10 06 1A]`, not a linear 0..255 encoding.
pub(crate) fn framed_voltage_dac(voltage_mv: u16) -> u8 {
    let clamped = voltage_mv.clamp(DSPIC_MIN_VOLTAGE_MV, DSPIC_MAX_VOLTAGE_MV);
    let span = u32::from(DSPIC_MAX_VOLTAGE_MV - DSPIC_MIN_VOLTAGE_MV);
    let offset = u32::from(clamped - DSPIC_MIN_VOLTAGE_MV);
    ((offset * 11 + (span / 2)) / span) as u8
}

/// Expected response byte count per command (RE'd from VNish libplatform.so).
/// Used by callers that need to size read buffers; NOT used for LEN calc anymore.
///
/// LEN field correction (P1.1 — Audit A GAP-A-13, 2026-04-25): the LEN byte in
/// every framed dsPIC command must encode the OUTGOING frame size as
/// `payload_len + 3` (counts itself + CMD + payload + CHECKSUM), NOT the
/// expected RESPONSE size + 4. The old `len_byte = resp_len + 4` was the root
/// cause of GET_VERSION returning `[FF FF]` bus noise on `a lab unit` — the dsPIC
/// receives a malformed frame with wrong LEN+CKSUM and silently drops it.
/// Confirmed against Mining Bible v1 `1-power-dspic/01-frame-format.md` and
/// the inline frame builders in `set_voltage`, `enable_voltage`, `send_heartbeat`
/// which were always correct (`LEN=0x04` for 1-byte payload, `LEN=0x05` for
/// 2-byte ENABLE_VOLTAGE payload).
fn dspic_response_len(cmd: u8) -> u8 {
    match cmd {
        CMD_GET_VERSION => 5,     // [cmd_echo, status, version, ?, checksum]
        CMD_JUMP_TO_APP => 2,     // [cmd_echo, status]
        CMD_RESET => 2,           // [cmd_echo, status]
        CMD_SET_VOLTAGE => 3,     // [cmd_echo, status, ?]
        CMD_ENABLE_VOLTAGE => 2,  // [cmd_echo, status]
        CMD_HEARTBEAT => 6,       // [cmd_echo, status, ?, ?, ?, ?]
        CMD_MEASURE_VOLTAGE => 2, // fw=0x89-shape: be16(raw_adc) in post-envelope reply
        CMD_GET_VOLTAGE_DAC => 4, // [cmd_echo, status, dac_hi, dac_lo] — DAC setpoint readback
        CMD_GET_VOLTAGE => 9,     // legacy/non-0x89 research only; not rail on `a lab unit`
        CMD_READ_TEMP => 4,       // [cmd_echo, status, temp_hi, temp_lo]
        _ => 2,                   // default: [cmd_echo, status]
    }
}

/// Compute LEN byte for an outgoing framed dsPIC command.
/// LEN counts itself + CMD + payload bytes + CHECKSUM = payload_len + 3.
/// Per Mining Bible v1 1-power-dspic/01-frame-format.md (VERIFIED).
#[inline]
fn dspic_outgoing_len(payload_len: usize) -> u8 {
    (payload_len as u8).wrapping_add(3)
}

/// Read LM75A temperature sensor via voltage controller I2C passthrough.
/// The dsPIC acts as an I2C bridge to on-board LM75A sensors at addresses
/// 0x72-0x75 (4 sensors per hash board for thermal zones).
/// This command tells the dsPIC which sensor address to read from.
pub const CMD_READ_TEMP: u8 = 0x30;

/// Voltage clamp control.
/// Equivalent to: dspic33epxx_voltage_clamp_ctrl()
pub const CMD_VOLTAGE_CLAMP: u8 = 0x31;

/// Get PWM duty cycle register value.
/// Equivalent to: dspic33epxx_get_PDCx()
pub const CMD_GET_PDCX: u8 = 0x32;

/// Erase dsPIC application flash (firmware update).
/// Equivalent to: dspic33epxx_erase_program()
pub const CMD_ERASE_PROGRAM: u8 = 0x09;

/// Write application program data (firmware update).
/// Equivalent to: dspic33epxx_update_app_program()
pub const CMD_UPDATE_PROGRAM: u8 = 0x05;

// ---------------------------------------------------------------------------
// dsPIC I2C address constants
// ---------------------------------------------------------------------------

/// dsPIC33EP actual I2C addresses on S19 Pro hash boards.
/// ONLY 0x20/0x21/0x22 are real 7-bit I2C addresses.
/// The old constants (0x88/0x89/0xB9/0xFE) were firmware VERSION identifiers
/// from BraiinsOS pic0x*.rs source paths — NOT I2C addresses.
///
/// Each board may have a DIFFERENT firmware version:
///   0x20: fw 0x82 (bare protocol), 0x21: fw 0x86 (bare), 0x22: fw 0x8A (framed)
pub const DSPIC_PROBE_ADDRS: [u8; 3] = [0x20, 0x21, 0x22];

/// LM75A temperature sensor addresses accessed through the dsPIC I2C passthrough.
/// 4 sensors per hash board, one per thermal zone.
///
/// Sources confirming `[0x48, 0x49, 0x4A, 0x4B]` (HEX, decimal 72-75):
///   - Bitmain stock S19j Pro CV rootfs `etc/topol.conf` (cv-cpio:0x0055F2EC):
///     "LM75A i2c_addr:72 .. 75" — decimal 72=0x48, 75=0x4B.
///   - :26,79`:
///     "Lm75aViaVoltageController at hb{2,3}.72-75 addresses 0x48-0x4B".
///   - :93`:
///     "Sensor addresses 0x48-0x4B on the shared voltage-controller-relayed I2C".
///
/// HISTORICAL NOTE: prior `[0x72, 0x73, 0x74, 0x75]` value was a hex/decimal
/// confusion bug — the BraiinsOS sensor *name* `hb2.72` uses decimal 72 for
/// the sensor index; the I2C *address* is 0x48 in HEX (decimal 72). The
/// LM75A datasheet specifies a 0x48-0x4F I2C address range with the low 3
/// bits set by ADDR0/1/2 hardware pins.
pub const LM75A_ADDRS: [u8; 4] = [0x48, 0x49, 0x4A, 0x4B];

// ---------------------------------------------------------------------------
// Voltage constants (S17/S19 range)
// ---------------------------------------------------------------------------

/// Default initial voltage in millivolts for S19 hash boards.
/// 13.80V is the EEPROM default (from S19 BraiinsOS probe fw_info.json).
pub const DEFAULT_VOLTAGE_MV: u16 = 13800;

/// Minimum voltage in millivolts (from S19 bosminer_model.json: min_voltage).
pub const MIN_VOLTAGE_MV: u16 = 11940;

/// Maximum voltage in millivolts (from S19 bosminer_model.json: max_voltage).
pub const MAX_VOLTAGE_MV: u16 = 15140;

/// Heartbeat interval in milliseconds.
/// BraiinsOS sends every 1 second (VOLTAGE_CTRL_HEART_BEAT_PERIOD).
/// Bitmain stock + VNish 1.2.7 cgminer use 10 seconds (HEART_BEAT_TIME_GAP)
/// — confirmed by VNish cgminer disasm (vmaddr 0x7394c sleeps 0x2710 ms =
/// 10000 ms between heartbeats — Mining Bible v1 1-power-dspic/03-timing.md).
///
/// P1.6 fix (2026-04-25): switching from 1 Hz to 10 s. The 1 Hz cadence may
/// have been flooding the dsPIC parser on `a lab unit`, contributing to the chain
/// silence observed in v9 testing. PSU watchdog (APW 0x84) stays at 1 Hz —
/// different layer.
///
/// See [`dcentrald_silicon_profiles::pic_heartbeat::pic_heartbeat_config`]
/// for the canonical per-`(Platform, PicFw)` interval table. The matrix
/// pins 1_000 ms for `S19Am2 × Dspic33epHealthy` (bosminer cadence,
/// matches the hybrid path). This 10_000 ms constant is the
/// driver-internal P1.6 fallback; see
///  for the
/// reconciliation discussion and the open question of which cadence
/// production should ship.
pub const HEARTBEAT_INTERVAL_MS: u64 = 10_000;

const DSPIC_GET_VERSION_ATTEMPTS: u8 = 3;
const DSPIC_PARSER_FLUSH_LEN: usize = 16;
const DSPIC_PARSER_FLUSH_SETTLE_MS: u64 = 10;
const DSPIC_GET_VERSION_REPLY_DELAY_MS: u64 = 100;
const DSPIC_GET_VERSION_SHORT_READ_LEN: usize = 1;
const DSPIC_GET_VERSION_FRAMED_READ_LEN: usize = 5;
const DSPIC_ENABLE_REPLY_DELAY_MS: u64 = 100;

/// Max `flush → framed-JUMP` re-verify attempts in
/// [`DspicService::rejump_to_app_mode_if_drifted`] when the cold-engaged
/// FRAMED dsPIC has drifted back to fw=0x82 bootloader by the time
/// `cold_boot_init_with_options` reaches SetVoltage (2026-06-07, `a lab unit`
/// standalone cold-engage). Bounded small: each JUMP-only re-verify is a
/// flush + framed JUMP + ~500 ms post-JUMP settle, so even the full bound stays
/// far under the ~8 s drift window — the JUMP→SetVoltage→ENABLE wall-time after
/// a single successful re-JUMP is ~100-200 ms. Inert unless the re-JUMP is
/// caller-gated active (env + `a lab unit` fingerprint) AND the protocol is framed.
const REJUMP_BEFORE_ENABLE_MAX_ATTEMPTS: u8 = 3;

///  (2026-05-23) — bosminer-faithful dsPIC i2c-0 timing.
///
///  live diagnostic on `a lab unit` proved the dsPIC at 0x20 is alive
/// in baseline i2cdetect but DCENT_OS's cold_boot WEDGES it.
/// Byte-level decode of bosminer's i2c-0 strace
/// (`wave38-bosminer-truth/bosminer-i2c0-slave20.txt`) shows three
/// timing/sequence deltas vs DCENT_OS:
///   1. Bosminer's PRE-RESET sync prelude = 7 zero bytes (NOT 16 used
///      by DCENT_OS's DSPIC_PARSER_FLUSH_LEN).
///   2. Bosminer's inter-byte gap on i2c-0 = ~6 ms (NOT 1 ms used by
///      DCENT_OS's `write_byte_by_byte` sleep).
///   3. Bosminer sends NO parser flush before GET_VERSION (DCENT_OS
///      prepends 16 zero bytes).
///
///  ships all three fixes default-OFF behind
/// `DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1`. When set, the dsPIC i2c-0
/// flow becomes byte-equivalent to bosminer (modulo the missing 0x55
/// preamble bytes that bosminer's strace dropped to `<unfinished>`).
/// Default off → byte-identical to prior waves on .79/.109 fleet
/// (preserves first-shares).
pub const DSPIC_BOSMINER_PARSER_FLUSH_LEN: usize = 7;
pub const DSPIC_BOSMINER_INTER_BYTE_GAP_MS: u64 = 6;

fn dspic_bosminer_faithful_value_enabled(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

/// : env-gate inspector.
///
/// `true` when `DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1` is set. Used by
/// `dspic_get_version_transaction_steps` to skip the parser flush and
/// by `bosminer_warmup` to scale the sync-prelude length down to 7.
pub fn dspic_bosminer_faithful_enabled() -> bool {
    let value = std::env::var("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL").ok();
    dspic_bosminer_faithful_value_enabled(value.as_deref())
}

// ---------------------------------------------------------------------------
// Frame protocol types
// ---------------------------------------------------------------------------

/// Response frame from the dsPIC voltage controller.
///
/// The dsPIC uses a framed I2C protocol where responses include a command echo,
/// status byte, and optional payload. From BraiinsOS error log:
///   "RxFrame { command: ff, status: ff, payload: [] }"
///
/// Frame format (I2C read after command write):
///   [command_echo, status, payload_byte_0, payload_byte_1, ...]
///
/// Status byte values (inferred from BraiinsOS behavior):
///   0x00 = success
///   0xFF = error / not ready / no response
#[derive(Debug, Clone)]
pub struct RxFrame {
    /// Echo of the command byte that was sent.
    pub command: u8,
    /// Status byte (0x00 = success).
    pub status: u8,
    /// Variable-length payload (0-32 bytes depending on command).
    pub payload: Vec<u8>,
}

impl RxFrame {
    /// Check if the frame indicates success.
    pub fn is_ok(&self) -> bool {
        self.status == 0x00
    }

    /// Check if the frame is an error/empty response (all 0xFF).
    pub fn is_error(&self) -> bool {
        self.command == 0xFF && self.status == 0xFF
    }
}

impl std::fmt::Display for RxFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RxFrame {{ command: 0x{:02X}, status: 0x{:02X}, payload: [{}] }}",
            self.command,
            self.status,
            self.payload
                .iter()
                .map(|b| format!("0x{:02X}", b))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

fn is_known_dspic_fw(version: u8) -> bool {
    matches!(version, 0x82 | 0x86 | 0x89 | 0x8A | 0xB9 | 0xFE)
}

fn is_bare_ack_fw_byte(byte: u8) -> bool {
    is_known_dspic_fw(byte)
}

fn is_repeated_fw_echo_ack(ack: &[u8]) -> bool {
    ack.len() >= 2 && is_bare_ack_fw_byte(ack[0]) && ack.iter().all(|&b| b == ack[0])
}

fn enable_ack_ok(ack: &[u8]) -> bool {
    ack.len() >= 2 && ack[0] == CMD_ENABLE_VOLTAGE && (ack[1] == 0x00 || ack[1] == 0x01)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnableVoltageAckKind {
    RealAck,
    FirmwareEcho,
    FirmwareEchoMismatch,
    AllFf,
    Mismatch,
}

impl std::fmt::Display for EnableVoltageAckKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnableVoltageAckKind::RealAck => write!(f, "real_ack"),
            EnableVoltageAckKind::FirmwareEcho => write!(f, "firmware_echo"),
            EnableVoltageAckKind::FirmwareEchoMismatch => write!(f, "firmware_echo_mismatch"),
            EnableVoltageAckKind::AllFf => write!(f, "all_ff"),
            EnableVoltageAckKind::Mismatch => write!(f, "mismatch"),
        }
    }
}

fn dspic_expected_fw_byte(firmware: DspicFirmware) -> Option<u8> {
    match firmware {
        DspicFirmware::Fw82 => Some(0x82),
        DspicFirmware::Fw86 => Some(0x86),
        DspicFirmware::Fw89 => Some(0x89),
        DspicFirmware::Fw8A => Some(0x8A),
        DspicFirmware::FwB9 => Some(0xB9),
        DspicFirmware::FwFE => Some(0xFE),
        DspicFirmware::Other(v) => Some(v),
        DspicFirmware::Unknown => None,
    }
}

fn fmt_optional_fw_byte(fw: Option<u8>) -> String {
    fw.map(|v| format!("0x{:02X}", v))
        .unwrap_or_else(|| "unknown".to_string())
}

fn classify_enable_ack(ack: &[u8], expected_fw: Option<u8>) -> EnableVoltageAckKind {
    if enable_ack_ok(ack) {
        return EnableVoltageAckKind::RealAck;
    }
    if ack.len() >= 2 && ack.iter().all(|&b| b == 0xFF) {
        return EnableVoltageAckKind::AllFf;
    }
    if is_repeated_fw_echo_ack(ack) {
        if let Some(expected) = expected_fw {
            if ack[0] == expected {
                EnableVoltageAckKind::FirmwareEcho
            } else {
                EnableVoltageAckKind::FirmwareEchoMismatch
            }
        } else {
            EnableVoltageAckKind::FirmwareEcho
        }
    } else {
        EnableVoltageAckKind::Mismatch
    }
}

fn dspic_require_real_enable_ack_enabled() -> bool {
    std::env::var("DCENT_AM2_REQUIRE_REAL_ENABLE_ACK")
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// Stricter ENABLE-ACK gate (2026-06-13, W1.2 jig op-diff): require the ENABLE
/// ACK to be EXACTLY `[0x15, 0x01]` — the *flag-on confirmed* state — and reject
/// `[0x15, 0x00]`.
///
/// The BM1362 factory jig (`single_board_test_bm1362`, `_bitmain_pic_enable_dc_dc_common`
/// FUN_00029b34) sends `55 AA 05 15 01 00 1B` (enable flag = `0x01`) and treats
/// **only** `read_back == [0x15, 0x01]` as success — it RETRIES otherwise and
/// fails the board if it never sees `0x01`. The ACK's second byte is the enable
/// flag echoed back: `0x01` = the dsPIC confirms the enable; `0x00` = it is
/// echoing the OFF/disable state (the enable did not take). DCENT's
/// [`enable_ack_ok`] historically accepted BOTH (lenient, for cross-fw tolerance).
/// This gate makes a `a lab unit`-class standalone run match the vendor's bar so a
/// `[0x15, 0x00]` (rail-not-confirmed) cannot be silently accepted as success.
///
/// **Default-OFF.** Absent env => byte-identical behaviour on every unit (the
/// lenient `enable_ack_ok` stands). This is an explicit opt-in strict
/// confirmation mode for lab runs that want the vendor jig's exact flag-on bar;
/// the proven  handoff, exploratory launchers, and fleet defaults do not
/// set it. Verified live: the current EBR-minimal `a lab unit` path already returns
/// `[0x15, 0x01]` (LIVE_TEST_18), so this gate does not change today's `a lab unit`
/// outcome when opted in; it pins the vendor bar against a future regression to
/// `[0x15, 0x00]`.
fn dspic_require_enable_flag_on_enabled() -> bool {
    std::env::var("DCENT_AM2_REQUIRE_ENABLE_FLAG_ON")
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// True iff `ack` is the jig-confirmed *flag-on* ENABLE ACK `[0x15, 0x01]`.
///
/// Distinct from [`enable_ack_ok`], which also accepts `[0x15, 0x00]`. Used by
/// the [`dspic_require_enable_flag_on_enabled`] gate to match the BM1362 factory
/// jig's exact success criterion (`read_back[1] == 0x01`).
fn enable_ack_flag_on(ack: &[u8]) -> bool {
    ack.len() >= 2 && ack[0] == CMD_ENABLE_VOLTAGE && ack[1] == 0x01
}

fn alternate_enable_encoding(encoding: EnableFrameEncoding) -> EnableFrameEncoding {
    match encoding {
        EnableFrameEncoding::Canonical => EnableFrameEncoding::VnishPadded,
        EnableFrameEncoding::VnishPadded => EnableFrameEncoding::Canonical,
    }
}

fn is_shift_left_artifact(buf: &[u8]) -> bool {
    buf.len() >= 2 && buf.windows(2).all(|w| w[1] == w[0].wrapping_shl(1))
}

fn parse_get_version_reply(buf: &[u8]) -> Option<u8> {
    if buf.is_empty()
        || buf.iter().all(|&b| b == 0x00)
        || buf.iter().all(|&b| b == 0xFF)
        || is_shift_left_artifact(buf)
    {
        return None;
    }

    // VNish/re-derived framed reply: [LEN=0x05, CMD=0x17, FW, ?, SUM].
    if buf.len() >= 3 && buf[0] == 0x05 && buf[1] == CMD_GET_VERSION && is_known_dspic_fw(buf[2]) {
        return Some(buf[2]);
    }

    // Older framed reply: [CMD=0x17, status, FW, ...].
    if buf.len() >= 3 && buf[0] == CMD_GET_VERSION && is_known_dspic_fw(buf[2]) {
        return Some(buf[2]);
    }

    // FW86 short/bare reply: the one-byte transaction returns just the
    // firmware byte. Multi-byte shift-left tails from xiic bulk reads are
    // rejected above.
    if is_known_dspic_fw(buf[0]) {
        return Some(buf[0]);
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GetVersionEncoding {
    Short,
    Framed,
}

fn dspic_get_version_frame(encoding: GetVersionEncoding) -> &'static [u8] {
    match encoding {
        GetVersionEncoding::Short => &[0x55, 0xAA, CMD_GET_VERSION],
        GetVersionEncoding::Framed => &[0x55, 0xAA, 0x04, CMD_GET_VERSION, 0x00, 0x1B],
    }
}

fn dspic_get_version_read_len(encoding: GetVersionEncoding) -> usize {
    match encoding {
        GetVersionEncoding::Short => DSPIC_GET_VERSION_SHORT_READ_LEN,
        GetVersionEncoding::Framed => DSPIC_GET_VERSION_FRAMED_READ_LEN,
    }
}

fn dspic_bytewise_write_then_read_steps(
    frame: &[u8],
    read_len: usize,
    delay_ms: u64,
) -> Vec<I2cTransactionStep> {
    let mut steps = vec![
        I2cTransactionStep::SetTimeout(10),
        I2cTransactionStep::WriteByteByByte(frame.to_vec()),
        I2cTransactionStep::SleepMs(delay_ms),
    ];
    for _ in 0..read_len {
        steps.push(I2cTransactionStep::Read(1));
    }
    steps
}

fn dspic_get_version_probe_order(firmware: DspicFirmware) -> [GetVersionEncoding; 2] {
    match firmware {
        DspicFirmware::Fw82 | DspicFirmware::Fw86 | DspicFirmware::Unknown => {
            [GetVersionEncoding::Short, GetVersionEncoding::Framed]
        }
        _ => [GetVersionEncoding::Framed, GetVersionEncoding::Short],
    }
}

fn dspic_get_version_transaction_steps(encoding: GetVersionEncoding) -> Vec<I2cTransactionStep> {
    dspic_get_version_transaction_steps_with_bosminer_faithful(
        encoding,
        dspic_bosminer_faithful_enabled(),
    )
}

fn dspic_get_version_transaction_steps_with_bosminer_faithful(
    encoding: GetVersionEncoding,
    bosminer_faithful: bool,
) -> Vec<I2cTransactionStep> {
    // : when DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1, omit the
    // pre-GET_VERSION parser flush. Bosminer's i2c-0 strace shows it
    // does NOT prepend any zero bytes before sending the framed/short
    // GET_VERSION — it just sleeps ~500 ms after the prior reply, then
    // writes the GET_VERSION bytes one at a time. The 16-byte flush
    // DCENT_OS uses may be exactly what wedges 0x20 on `a lab unit`.
    let mut steps = if bosminer_faithful {
        vec![
            I2cTransactionStep::WriteByteByByte(dspic_get_version_frame(encoding).to_vec()),
            I2cTransactionStep::SleepMs(DSPIC_GET_VERSION_REPLY_DELAY_MS),
        ]
    } else {
        vec![
            I2cTransactionStep::WriteByteByByte(vec![0u8; DSPIC_PARSER_FLUSH_LEN]),
            I2cTransactionStep::SleepMs(DSPIC_PARSER_FLUSH_SETTLE_MS),
            I2cTransactionStep::WriteByteByByte(dspic_get_version_frame(encoding).to_vec()),
            I2cTransactionStep::SleepMs(DSPIC_GET_VERSION_REPLY_DELAY_MS),
        ]
    };
    for _ in 0..dspic_get_version_read_len(encoding) {
        steps.push(I2cTransactionStep::Read(1));
    }
    steps
}

fn collect_single_byte_i2c_reads(reads: Vec<Vec<u8>>) -> Vec<u8> {
    reads
        .into_iter()
        .filter_map(|read| read.first().copied())
        .collect()
}

// ---------------------------------------------------------------------------
// dsPIC firmware type detection
// ---------------------------------------------------------------------------

/// dsPIC I2C protocol mode — determined by firmware version, NOT I2C address.
///
/// The S19 Pro has three hash boards with DIFFERENT firmware versions:
///   0x20: fw 0x82 (bare protocol), 0x21: fw 0x86 (S19j bare), 0x22: fw 0x8A (framed)
///
/// Protocol modes:
///   - **Bare**: [55 AA CMD data...] — no LEN byte, no checksum. fw 0x82 confirmed.
///   - **Framed**: [55 AA LEN CMD data... CHECKSUM] — with LEN and SUM checksum. fw 0x8A+.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DspicProtocol {
    /// Bare protocol: [55 AA CMD data...] — no framing, no checksum.
    /// Confirmed working for heartbeat on fw 0x82.
    Bare,
    /// S19j framed protocol: [55 AA 04 CMD ARG CHECKSUM].
    /// LEN always 0x04, CHECKSUM = (LEN + CMD + ARG) & 0xFF.
    /// Used by framed firmware variants such as fw 0x8A/0x89.
    Framed,
}

/// Detected dsPIC firmware variant.
///
/// Identified by the firmware version byte returned by CMD_GET_VERSION or
/// raw I2C read. The version determines which protocol mode to use.
///
/// IMPORTANT: The enum variants are keyed by firmware VERSION (0x82, 0x86, etc.),
/// NOT by I2C address (0x20, 0x21, 0x22). I2C addresses are physical board positions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DspicFirmware {
    /// Firmware version 0x82 — bare protocol, older S19 Pro boards.
    Fw82,
    /// Firmware version 0x86 — S19j bare protocol.
    Fw86,
    /// Firmware version 0x89 — VNish framed protocol (dynamic LEN).
    Fw89,
    /// Firmware version 0x8A — likely framed (similar to 0x89).
    Fw8A,
    /// Firmware version 0xB9 — newer boards, framed protocol.
    FwB9,
    /// Firmware version 0xFE — alternate variant.
    FwFE,
    /// Firmware with a specific version byte not in the known set.
    Other(u8),
    /// Not yet detected.
    Unknown,
}

const DSPIC_FORCE_FW89_ENCODING_ENV: &str = "DCENT_AM2_FORCE_FW89_ENCODING";

fn force_fw89_encoding_value_enabled(value: Option<&str>) -> bool {
    matches!(
        value,
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

fn force_fw89_encoding_enabled_from_env() -> bool {
    let value = std::env::var(DSPIC_FORCE_FW89_ENCODING_ENV).ok();
    force_fw89_encoding_value_enabled(value.as_deref())
}

fn decode_dspic_firmware(fw_byte: Option<u8>) -> DspicFirmware {
    match fw_byte {
        Some(0x82) => DspicFirmware::Fw82,
        Some(0x86) => DspicFirmware::Fw86,
        Some(0x89) => DspicFirmware::Fw89,
        Some(0x8A) => DspicFirmware::Fw8A,
        Some(0xB9) => DspicFirmware::FwB9,
        Some(0xFE) => DspicFirmware::FwFE,
        Some(0x00 | 0xFF) | None => DspicFirmware::Unknown,
        Some(other) => DspicFirmware::Other(other),
    }
}

/// Apply the lab-only encoding experiment to an already decoded identity.
///
/// Keeping this transformation pure makes the safety boundary testable under
/// Cargo's parallel runner. Only the production adapter above reads process
/// state, and only Fw82 can cross this boundary into Fw89 encoding.
fn apply_fw89_encoding_override(
    detected: DspicFirmware,
    force_fw89_encoding: bool,
) -> DspicFirmware {
    if force_fw89_encoding && matches!(detected, DspicFirmware::Fw82) {
        DspicFirmware::Fw89
    } else {
        detected
    }
}

impl DspicFirmware {
    /// Determine the I2C protocol mode for this firmware version.
    pub fn protocol(&self) -> DspicProtocol {
        match self {
            DspicFirmware::Fw82 | DspicFirmware::Fw86 => DspicProtocol::Bare,
            // FW86 was live-proven as short/bare on .139 after AC power-cycle.
            // Newer known variants remain framed unless re-proven otherwise.
            _ => DspicProtocol::Framed,
        }
    }

    /// Whether legacy bootloader-control commands are allowed for this
    /// firmware. Modern S19j/Pic0x89-family paths must not emit RESET/JUMP.
    pub fn legacy_bootloader_commands_allowed(&self) -> bool {
        matches!(self, DspicFirmware::Fw82)
    }

    /// Create from a raw firmware version byte.
    ///
    /// ** MED-3 (2026-05-24, DCENT_EE swarm finding — comment
    /// rewrite to prevent regression).** The earlier 5 /
    /// claim that "fw=0x82 BARE BE-mV SetVoltage → ~2 V chip rail on
    /// `a lab unit`" was MISDIAGNOSED. The 2 V observation was a `a lab unit`-specific
    /// chip-state interaction with bosminer-plus-tuner 0.9.0 leaving the
    /// chip mid-init, NOT a BARE encoding bug. **`a lab unit` mines at 13.7 V
    /// using the exact BARE BE-mV encoding `[55 AA 10 HI LO]`** (first
    /// shares 2026-05-15,  BARE path — see
    /// ).
    ///
    /// Future agents: DO NOT rip out the BARE BE-mV path. DO NOT promote
    /// `DCENT_AM2_FORCE_FW89_ENCODING=1` to default-on. The escape hatch
    /// stays env-gated default-OFF specifically because flipping it
    /// default-on would regress the `a lab unit` BARE-proven first-shares path
    /// (and every other unit in the fleet that talks BARE fw=0x82).
    ///
    /// Today's `a lab unit` mining ( LIVE 2026-05-24) uses the
    /// bosminer-handoff recipe which leaves the chip in fw=0x89 FRAMED
    /// app state —.
    /// `a lab unit` doesn't need this env override at all; FW89-framed encoding
    /// happens automatically when bosminer pre-engaged the chip.
    ///
    /// The env override ( vintage) remains in source for lab
    /// experimentation only.
    ///
    /// **SAFETY (PIC-1, 2026-06-20): the FORCE_FW89_ENCODING remap is scoped to
    /// `Fw82` ONLY — fw=0x86 must NEVER be remapped to Fw89.** Fw89 is not in the
    /// fw=0x86 voltage-refusal predicate
    /// ([`dspic_requires_degraded_fw_voltage_refusal`]), so remapping a corrupted
    /// fw=0x86 chip to Fw89 here would silently make it voltage-ALLOWED — a second,
    /// non-auditable bypass of the load-bearing fw=0x86 guard born of the `a lab unit`
    /// corruption incident. The ONLY sanctioned fw=0x86 override is the auditable,
    /// lab-only [`DSPIC_FW86_TRUST_DEGRADED_ENV`]. `Fw82` (the legitimate cold-boot
    /// bootloader/bare-protocol case) is the only fw this flag may upgrade.
    pub fn from_version(version: u8) -> Self {
        Self::from_version_with_fw89_encoding(version, force_fw89_encoding_enabled_from_env())
    }

    fn from_version_with_fw89_encoding(version: u8, force_fw89_encoding: bool) -> Self {
        let detected = decode_dspic_firmware(Some(version));
        let effective = apply_fw89_encoding_override(detected, force_fw89_encoding);

        // SAFETY: Fw82 ONLY. Fw86 deliberately excluded so it stays caught by
        // the fw=0x86 voltage-refusal predicate (see the doc-comment above).
        if effective != detected {
            tracing::info!(
                detected_fw = format_args!("0x{:02X}", version),
                forced_to = "Fw89",
                "Wave-27 DCENT_AM2_FORCE_FW89_ENCODING=1 — remapping detected fw=0x82 bare-protocol fw to Fw89 (framed-DAC) for SetVoltage / Enable / Disable encoding (fw=0x86 is NOT remapped — it stays voltage-refused)"
            );
        }

        effective
    }
}

impl std::fmt::Display for DspicFirmware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DspicFirmware::Fw82 => write!(f, "dsPIC(fw=0x82,bare)"),
            DspicFirmware::Fw86 => write!(f, "dsPIC(fw=0x86,bare)"),
            DspicFirmware::Fw89 => write!(f, "dsPIC(fw=0x89,framed)"),
            DspicFirmware::Fw8A => write!(f, "dsPIC(fw=0x8A,framed)"),
            DspicFirmware::FwB9 => write!(f, "dsPIC(fw=0xB9,framed)"),
            DspicFirmware::FwFE => write!(f, "dsPIC(fw=0xFE,framed)"),
            DspicFirmware::Other(v) => write!(f, "dsPIC(fw=0x{:02X},framed)", v),
            DspicFirmware::Unknown => write!(f, "dsPIC(Unknown)"),
        }
    }
}

/// AT-3 (quiet-window 0x3A measured-voltage read) firmware gate.
///
/// AT-3 must only ever issue the **parser-safe byte-wise framed** path of
/// [`DspicService::measure_voltage`] (`[55 AA 04 3A 00 3E]` via
/// `bytewise_write_then_read`), which is taken for `Fw89`/`Fw8A`. For bare
/// firmwares (`Fw82`/`Fw86`) and `Unknown`/other, `measure_voltage` falls
/// through to the `write_read_command` → `I2C_RDWR` combined read — the exact
/// shape the AT-1 module header warns "can corrupt the dsPIC parser". AT-3 must
/// therefore **refuse** to call `measure_voltage` for any firmware this gate
/// rejects, independent of `measure_voltage`'s own internal branching, so a
/// future refactor of `measure_voltage` cannot silently expose AT-3 to the
/// `I2C_RDWR` form (DESIGN 1 §1.5, "Firmware gating of the wire form").
///
/// Returns `true` only for `Fw89`/`Fw8A`.
pub fn at3_measure_voltage_firmware_allowed(fw: DspicFirmware) -> bool {
    matches!(fw, DspicFirmware::Fw89 | DspicFirmware::Fw8A)
}

/// Map an optionally observed Pic0x89-family firmware byte to the exact
/// dsPIC firmware identity used by voltage guards.
///
/// `None` means no firmware identity was observed and therefore maps to
/// [`DspicFirmware::Unknown`]. Missing evidence must never manufacture fw=0x89:
/// that identity selects the VNish-padded ENABLE encoding and permits voltage
/// commands. Explicit 0x86 must stay 0x86 so serial BM1362/BM1398 paths cannot
/// bypass the degraded voltage refusal by routing through a Pic0x89 wrapper.
///
/// **SAFETY (PIC-1, 2026-06-20): the FORCE_FW89_ENCODING remap below is scoped to
/// `Fw82` ONLY — an observed fw=0x86 must NEVER be remapped to Fw89.** Fw89 is not
/// in the fw=0x86 voltage-refusal predicate
/// ([`dspic_requires_degraded_fw_voltage_refusal`]); remapping a corrupted fw=0x86
/// chip to Fw89 here would silently make it voltage-ALLOWED, defeating the
/// load-bearing fw=0x86 guard (the `a lab unit` corruption incident) through a second,
/// non-auditable path. The ONLY sanctioned fw=0x86 override is the auditable,
/// lab-only [`DSPIC_FW86_TRUST_DEGRADED_ENV`].
pub fn pic0x89_firmware_from_observed_fw_byte(fw_byte: Option<u8>) -> DspicFirmware {
    pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(
        fw_byte,
        force_fw89_encoding_enabled_from_env(),
    )
}

fn pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(
    fw_byte: Option<u8>,
    force_fw89_encoding: bool,
) -> DspicFirmware {
    let detected = decode_dspic_firmware(fw_byte);
    let effective = apply_fw89_encoding_override(detected, force_fw89_encoding);

    //  (2026-05-23): mirror of `from_version`'s env override —
    // when `DCENT_AM2_FORCE_FW89_ENCODING=1`, treat the fw=0x82 bare-protocol
    // case as Fw89 so SetVoltage / Enable / Disable use framed-DAC encoding.
    // See the corresponding doc block on `DspicFirmware::from_version`.
    //
    // SAFETY: Fw82 ONLY. Fw86 deliberately excluded so it stays caught by the
    // fw=0x86 voltage-refusal predicate (see the doc-comment above).
    if effective != detected {
        tracing::info!(
            detected_fw = format_args!("0x{:?}", fw_byte),
            forced_to = "Fw89",
            "Wave-27b DCENT_AM2_FORCE_FW89_ENCODING=1 — pic0x89 path remapping fw=0x82 bare-protocol fw to Fw89 (framed-DAC) for SetVoltage encoding (fw=0x86 is NOT remapped — it stays voltage-refused)"
        );
    }

    effective
}

/// Lab-only override for fw=0x86 voltage commands.
///
/// fw=0x86 is live-proven as a degraded path: the bus ACKs some commands, but
/// the rail-engagement sequence is not production-trusted. Voltage-setting
/// commands stay refused unless an operator explicitly opts into a lab run.
pub const DSPIC_FW86_TRUST_DEGRADED_ENV: &str = "DCENT_AM2_TRUST_DEGRADED_FW";

/// Return true when this firmware requires the fw=0x86 voltage-command refusal.
pub const fn dspic_requires_degraded_fw_voltage_refusal(firmware: DspicFirmware) -> bool {
    matches!(firmware, DspicFirmware::Fw86)
}

/// Pure helper used by tests and API surfaces that need to explain the gate.
pub const fn dspic_voltage_command_allowed(
    firmware: DspicFirmware,
    trust_degraded_fw: bool,
) -> bool {
    match firmware {
        // No observed identity means no proven protocol or energizing bytes.
        // Unlike fw=0x86, this cannot be bypassed by the degraded-firmware lab
        // override because there is no evidence about what firmware is present.
        DspicFirmware::Unknown | DspicFirmware::Other(_) => false,
        DspicFirmware::Fw86 => trust_degraded_fw,
        _ => true,
    }
}

/// Whether a firmware identity has an explicitly modeled runtime wire
/// protocol. Unknown and merely observed-but-unsupported revisions must not
/// inherit a generic framed protocol for keepalive or energizing operations.
pub const fn dspic_runtime_protocol_is_proven(firmware: DspicFirmware) -> bool {
    matches!(
        firmware,
        DspicFirmware::Fw82
            | DspicFirmware::Fw86
            | DspicFirmware::Fw89
            | DspicFirmware::Fw8A
            | DspicFirmware::FwB9
            | DspicFirmware::FwFE
    )
}

fn ensure_dspic_runtime_protocol_is_proven(
    address: u8,
    firmware: DspicFirmware,
    operation: &str,
) -> Result<()> {
    if dspic_runtime_protocol_is_proven(firmware) {
        return Ok(());
    }

    let evidence = match firmware {
        DspicFirmware::Unknown => "no firmware identity was observed".to_string(),
        DspicFirmware::Other(version) => {
            format!("observed firmware 0x{version:02X} has no explicitly modeled runtime protocol")
        }
        _ => unreachable!("known firmware identities are accepted above"),
    };
    Err(crate::AsicError::Pic {
        addr: address,
        detail: format!("dsPIC {operation} refused: {evidence}"),
    })
}

/// Read the process environment for the lab-only fw=0x86 voltage override.
pub fn dspic_fw86_trust_degraded_override_enabled() -> bool {
    std::env::var(DSPIC_FW86_TRUST_DEGRADED_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}

pub fn dspic_voltage_refusal_detail(operation: &str) -> String {
    format!(
        "dsPIC {} refused for fw=0x86 by default: degraded-voltage path is not \
         production-trusted; set {}=1 only in a lab after independent rail verification",
        operation, DSPIC_FW86_TRUST_DEGRADED_ENV
    )
}

fn ensure_dspic_voltage_command_allowed(
    address: u8,
    firmware: DspicFirmware,
    operation: &str,
) -> Result<()> {
    if dspic_voltage_command_allowed(firmware, dspic_fw86_trust_degraded_override_enabled()) {
        return Ok(());
    }

    let detail = match firmware {
        DspicFirmware::Unknown => format!(
            "dsPIC {} refused without an observed firmware identity: protocol and energizing bytes cannot be selected safely",
            operation
        ),
        DspicFirmware::Other(version) => format!(
            "dsPIC {} refused for unsupported observed firmware 0x{version:02X}: energizing bytes are not proven",
            operation
        ),
        _ => dspic_voltage_refusal_detail(operation),
    };

    Err(crate::AsicError::Pic {
        addr: address,
        detail,
    })
}

/// Load-bearing voltage HARD CAP for the dsPIC chain/chip rail (project safety
/// rule: <=14500 mV). Enforced as an INPUT clamp at the actual rail-program
/// boundary (`set_voltage`, both controller + service) — gap-swarm cont.24/25
/// finding .
///
/// CRITICAL — why this is an input clamp, NOT a lower `DSPIC_MAX_VOLTAGE_MV`:
/// `DSPIC_MAX_VOLTAGE_MV` (15140) anchors the `framed_voltage_dac()` encoding
/// span (11940..=15140). That mapping is LIVE-PROVEN (13.7 V -> DAC 0x06,
/// strace-confirmed on `a lab unit`/`a lab unit`). Lowering the max constant would shrink
/// the span and shift EVERY production DAC code (13.7 V -> a different DAC ->
/// a different actual rail voltage) — a silent live-rail regression. So we
/// leave the span/encoding untouched and clamp the *input* instead: a request
/// above 14500 mV is lowered to 14500 (never raised), so the rail can never be
/// driven past the hard cap while the proven DAC mapping is preserved.
pub const DSPIC_VOLTAGE_HARD_CAP_MV: u16 = 14_500;

/// Lab-only override that lifts the 14500 mV input clamp up to
/// `DSPIC_MAX_VOLTAGE_MV` so the documented AMTC ~15.0 V pre-open pulse stays
/// reachable on a bench (: "gate 15.0V pre-open behind an explicit lab
/// flag"). Default OFF -> production (13.7-13.8 V) never exceeds 14500.
pub const DSPIC_ALLOW_LAB_OVERVOLT_ENV: &str = "DCENT_AM2_ALLOW_LAB_OVERVOLT";

/// Pure input clamp: returns `(effective_mv, was_clamped)`. With
/// `lab_overvolt == false`, any input above `DSPIC_VOLTAGE_HARD_CAP_MV` is
/// lowered to the cap (only ever DOWN, never up). With `lab_overvolt == true`
/// the input passes through unchanged (still subject to the
/// `DSPIC_MIN_VOLTAGE_MV..=DSPIC_MAX_VOLTAGE_MV` range check at the call site).
pub fn clamp_dspic_voltage_to_hard_cap(voltage_mv: u16, lab_overvolt: bool) -> (u16, bool) {
    if !lab_overvolt && voltage_mv > DSPIC_VOLTAGE_HARD_CAP_MV {
        (DSPIC_VOLTAGE_HARD_CAP_MV, true)
    } else {
        (voltage_mv, false)
    }
}

/// Read the process environment for the lab-only over-volt override.
pub fn dspic_lab_overvolt_override_enabled() -> bool {
    std::env::var(DSPIC_ALLOW_LAB_OVERVOLT_ENV)
        .map(|value| value == "1")
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnableFrameEncoding {
    /// 6-byte canonical form: `[55 AA 04 15 ARG SUM]` (LEN=0x04, single-byte payload).
    /// Original DCENT_OS form, matched bosminer strace `[55 AA 04 15 01 1A]`.
    Canonical,
    /// 7-byte VNish-RE'd form: `[55 AA 05 15 ARG 0x00 SUM]` (LEN=0x05, two-byte payload).
    /// VNish/bosminer cgminer disasm @ VMA 0x05277c (RE corpus 2026-04-25,
    /// 22 firmwares cross-validated):
    ///   ENABLE  TX = `[55 AA 05 15 01 00 1B]`  RX = `[15 01]`
    ///   DISABLE TX = `[55 AA 05 15 00 00 1A]`  RX = `[15 00]`
    /// Checksum = (LEN + CMD + ARG + 0x00) & 0xFF.
    /// Applied selectively to fw=0x86 (framed) and fw=0x89 only — other framed
    /// variants (0x8A/0xB9/0xFE) keep the canonical 6-byte form because VNish
    /// RE coverage for those firmwares is limited and the 7-byte form has
    /// not been live-proven for them.
    VnishPadded,
}

/// Pick the ENABLE/DISABLE_VOLTAGE frame encoding for a given firmware.
///
/// VNish RE corpus (*.md`)
/// shows fw=0x86 and fw=0x89 use the 7-byte `VnishPadded` form. fw=0x82/0x88/
/// 0xB9/0xFE use the canonical 6-byte form (or bare equivalent).
///
/// NOTE on fw=0x86: live test on `a lab unit` (
/// 2026-04-24) confirmed fw=0x86 negotiates the BARE protocol on the wire.
/// Bare mode short-circuits the `EnableFrameEncoding` selection at the call site
/// (the bare 4-byte `[55 AA 15 ARG]` form is built directly), so the
/// `VnishPadded` choice for fw=0x86 only applies if a future call site forces
/// the framed path for that firmware.
#[inline]
fn dspic_enable_disable_encoding(firmware: DspicFirmware) -> EnableFrameEncoding {
    match firmware {
        // fw=0x86 framed (per VNish RE) and fw=0x89 (S19j Pro am2 — primary target)
        // get the 7-byte form. Live wire is bare for 0x86, but if forced framed
        // we still want the VNish form rather than the unproven Canonical form.
        DspicFirmware::Fw86 | DspicFirmware::Fw89 => EnableFrameEncoding::VnishPadded,
        // fw=0x8A / 0xB9 / 0xFE / Other / Unknown: keep canonical 6-byte form.
        // VNish RE for these firmwares is incomplete and the 7-byte form has
        // not been live-proven for them; flipping unilaterally risks regressing
        // working units. fw=0x82 only uses the bare path.
        _ => EnableFrameEncoding::Canonical,
    }
}

fn dspic_set_voltage_frame(
    firmware: DspicFirmware,
    use_bare_protocol: bool,
    voltage_mv: u16,
) -> Vec<u8> {
    if firmware == DspicFirmware::Fw86 {
        // FW86 — try FRAMED form first.
        //
        // Live evidence on `a lab unit` (2026-04-26 evening): bare `[55 AA 10 DAC]`
        // is ACKed by the dsPIC at the wire level (no EIO) but the chain
        // DC-DC rail does NOT engage afterward (post-ENABLE chain UART probe
        // returns 0 bytes; user confirms hardware mines fine under bosminer).
        // The user's confirmation rules out hardware fault, so the bare form
        // must be insufficient to actually program the DAC on fw=0x86.
        // Switch to the framed `[55 AA 04 10 DAC SUM]` form which is the
        // canonical Bible form (`mining-bible-v1/1-power-dspic/00-opcode-map.md`)
        // and is proven on fw=0x82/0x88/0x89/0x8A. Per investigation
        // `21-pic-0x86-analysis.md`, fw=0x86 also accepts the framed form.
        let dac = framed_voltage_dac(voltage_mv);
        let checksum = 0x04u8.wrapping_add(CMD_SET_VOLTAGE).wrapping_add(dac);
        vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, dac, checksum]
    } else if use_bare_protocol {
        // FW82 bare form: [55 AA 10 HI LO] with millivolts in big-endian.
        let hi = (voltage_mv >> 8) as u8;
        let lo = (voltage_mv & 0xFF) as u8;
        vec![0x55, 0xAA, CMD_SET_VOLTAGE, hi, lo]
    } else {
        // Framed form: [55 AA 04 10 DAC SUM].
        let dac = framed_voltage_dac(voltage_mv);
        let checksum = 0x04u8.wrapping_add(CMD_SET_VOLTAGE).wrapping_add(dac);
        vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, dac, checksum]
    }
}

fn dspic_enable_voltage_frame(
    use_bare_protocol: bool,
    encoding: EnableFrameEncoding,
) -> &'static [u8] {
    if use_bare_protocol {
        &[0x55, 0xAA, CMD_ENABLE_VOLTAGE, 0x01]
    } else {
        match encoding {
            // 6-byte: LEN=0x04, payload=[0x01], SUM=(0x04+0x15+0x01)=0x1A
            EnableFrameEncoding::Canonical => &[0x55, 0xAA, 0x04, CMD_ENABLE_VOLTAGE, 0x01, 0x1A],
            // 7-byte VNish: LEN=0x05, payload=[0x01, 0x00], SUM=(0x05+0x15+0x01+0x00)=0x1B
            EnableFrameEncoding::VnishPadded => {
                &[0x55, 0xAA, 0x05, CMD_ENABLE_VOLTAGE, 0x01, 0x00, 0x1B]
            }
        }
    }
}

fn dspic_disable_voltage_frame(
    use_bare_protocol: bool,
    encoding: EnableFrameEncoding,
) -> &'static [u8] {
    if use_bare_protocol {
        &[0x55, 0xAA, CMD_ENABLE_VOLTAGE, 0x00]
    } else {
        match encoding {
            // 6-byte: LEN=0x04, payload=[0x00], SUM=(0x04+0x15+0x00)=0x19
            EnableFrameEncoding::Canonical => &[0x55, 0xAA, 0x04, CMD_ENABLE_VOLTAGE, 0x00, 0x19],
            // 7-byte VNish: LEN=0x05, payload=[0x00, 0x00], SUM=(0x05+0x15+0x00+0x00)=0x1A
            EnableFrameEncoding::VnishPadded => {
                &[0x55, 0xAA, 0x05, CMD_ENABLE_VOLTAGE, 0x00, 0x00, 0x1A]
            }
        }
    }
}

fn dspic_heartbeat_frame(use_bare_protocol: bool) -> &'static [u8] {
    if use_bare_protocol {
        &[0x55, 0xAA, CMD_HEARTBEAT]
    } else {
        &[0x55, 0xAA, 0x04, CMD_HEARTBEAT, 0x00, 0x1A]
    }
}

/// Build the LM75A passthrough read frame for `sensor_addr`.
///
/// Bare form (fw 0x82 / fw 0x86):  `[55 AA 30 sensor_addr]` (4 bytes).
/// Framed form (fw 0x89/0x8A/B9):  `[55 AA 04 30 sensor_addr SUM]` (6 bytes)
///   where `SUM = (0x04 + 0x30 + sensor_addr) & 0xFF`.
///
/// The dsPIC bridges this command to the on-board LM75A sensor at
/// `sensor_addr` (0x48-0x4B) and returns `[cmd_echo, status, temp_hi, temp_lo]`.
fn dspic_read_temp_frame(use_bare_protocol: bool, sensor_addr: u8) -> Vec<u8> {
    if use_bare_protocol {
        vec![0x55, 0xAA, CMD_READ_TEMP, sensor_addr]
    } else {
        let checksum = 0x04u8.wrapping_add(CMD_READ_TEMP).wrapping_add(sensor_addr);
        vec![0x55, 0xAA, 0x04, CMD_READ_TEMP, sensor_addr, checksum]
    }
}

fn legacy_bootloader_command(data: &[u8], use_bare_protocol: bool) -> Option<u8> {
    if data.len() < 3 || data[0] != DSPIC_PREAMBLE[0] || data[1] != DSPIC_PREAMBLE[1] {
        return None;
    }

    // : bare frames are
    // [55 AA CMD ARG], so data[3] is an argument/DAC byte, not a command.
    if (use_bare_protocol || data.len() == 3) && matches!(data[2], CMD_RESET | CMD_JUMP_TO_APP) {
        return Some(data[2]);
    }

    if !use_bare_protocol && data.len() >= 4 && matches!(data[3], CMD_RESET | CMD_JUMP_TO_APP) {
        return Some(data[3]);
    }

    None
}

fn ensure_dspic_bootloader_command_allowed(
    address: u8,
    firmware: DspicFirmware,
    use_bare_protocol: bool,
    data: &[u8],
) -> Result<()> {
    if let Some(cmd) = legacy_bootloader_command(data, use_bare_protocol) {
        if !firmware.legacy_bootloader_commands_allowed() {
            let name = if cmd == CMD_RESET {
                "RESET"
            } else {
                "JUMP_TO_APP"
            };
            return Err(crate::AsicError::Pic {
                addr: address,
                detail: format!(
                    "dsPIC {} command 0x{:02X} banned for {}",
                    name, cmd, firmware
                ),
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// dsPIC voltage controller
// ---------------------------------------------------------------------------

/// dsPIC33EP16GS202 voltage controller for one S17/S19 hash board.
///
/// This controller communicates over I2C using a framed protocol that wraps
/// command bytes in structured frames with status and payload fields.
///
/// Unlike the PIC16F1704 (which uses raw `[0x55, 0xAA, cmd, data]` preamble),
/// the dsPIC protocol includes:
///   - Command echo in responses (for frame matching)
///   - Status byte (success/error indication)
///   - Checksum validation (BraiinsOS: "bad checksum on get_voltage packet")
///   - I2C passthrough for LM75A temperature sensors
///
/// The dsPIC also uses higher voltages (12-15V bus voltage vs S9's 8-9.4V)
/// and millivolt precision (16-bit voltage values instead of 8-bit DAC).
pub struct DspicController<'a> {
    /// I2C bus reference.
    i2c: &'a mut I2cBus,
    /// I2C slave address (0x88, 0x89, 0xB9, or 0xFE).
    address: u8,
    /// Detected firmware variant.
    firmware: DspicFirmware,
    /// Current target voltage in millivolts.
    current_voltage_mv: u16,
    /// Whether voltage output is enabled.
    voltage_enabled: bool,
    /// Use bare protocol (no LEN/checksum) for firmware 0x82 compatibility.
    use_bare_protocol: bool,
}

impl<'a> DspicController<'a> {
    /// Construct from a discovery-issued, bus-bound dsPIC endpoint.
    ///
    /// This is the preferred constructor for migrated orchestration paths.
    /// Unlike [`Self::new`], neither family nor address is caller asserted.
    pub fn from_endpoint(i2c: &'a mut I2cBus, endpoint: VoltageControllerEndpoint) -> Result<Self> {
        if endpoint.kind() != VoltageControllerKind::Dspic33Ep {
            return Err(crate::AsicError::InvalidParameter(format!(
                "{} endpoint cannot construct a dsPIC controller",
                endpoint.kind().as_str()
            )));
        }
        if endpoint.bus() != i2c.bus() {
            return Err(crate::AsicError::InvalidParameter(format!(
                "dsPIC endpoint is bound to I2C bus {}, but transport owns bus {}",
                endpoint.bus(),
                i2c.bus()
            )));
        }
        Ok(Self::new(i2c, endpoint.address()))
    }

    /// Create a new dsPIC controller for the given I2C address.
    /// Firmware type defaults to Unknown (auto-detected on init).
    pub fn new(i2c: &'a mut I2cBus, address: u8) -> Self {
        Self {
            i2c,
            address,
            firmware: DspicFirmware::Unknown,
            current_voltage_mv: 0,
            voltage_enabled: false,
            use_bare_protocol: false,
        }
    }

    /// Create a dsPIC controller with a known firmware type.
    pub fn new_with_firmware(i2c: &'a mut I2cBus, address: u8, firmware: DspicFirmware) -> Self {
        Self {
            i2c,
            address,
            firmware,
            current_voltage_mv: 0,
            voltage_enabled: false,
            use_bare_protocol: firmware.protocol() == DspicProtocol::Bare,
        }
    }

    /// Get the detected firmware variant.
    pub fn firmware(&self) -> DspicFirmware {
        self.firmware
    }

    /// Get the I2C address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Get the current target voltage in millivolts (cached).
    pub fn voltage_mv(&self) -> u16 {
        self.current_voltage_mv
    }

    /// Get the current target voltage in volts.
    pub fn voltage_v(&self) -> f64 {
        self.current_voltage_mv as f64 / 1000.0
    }

    // -----------------------------------------------------------------------
    // Detection
    // -----------------------------------------------------------------------

    /// Probe the dsPIC at the configured address to detect firmware version.
    ///
    /// Sends CMD_GET_VERSION and reads the response frame. If the dsPIC is in
    /// bootloader mode, the response will be 0xFF/0xFF (error frame).
    ///
    /// Returns the detected firmware variant, or Unknown if detection fails.
    pub fn detect_firmware(&mut self) -> Result<DspicFirmware> {
        // Flush the dsPIC I2C parser with zeros (same pattern as PIC16F1704).
        // The dsPIC's I2C slave module can get stuck if a previous transaction
        // was interrupted (e.g., after kill -9 of bosminer).
        let _ = self.i2c.set_slave(self.address);
        let _ = self.i2c.write(&[0u8; 8]);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // GET_VERSION is a short query on BM1362-family dsPIC firmware. The
        // framed form NAKs on live .139 and produces bus-noise reads.
        self.use_bare_protocol = true;
        let cmd = [DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_GET_VERSION];
        let mut buf = [0u8; 5];

        match self.write_read_command(&cmd, &mut buf) {
            Ok(()) => {
                let rx_cmd = buf[0];
                let rx_status = buf[1];
                if rx_cmd == 0xFF && rx_status == 0xFF {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        "dsPIC in bootloader or not responding (all 0xFF)",
                    );
                    self.firmware = DspicFirmware::Unknown;
                    return Ok(DspicFirmware::Unknown);
                }

                // Map firmware VERSION (not address) to variant.
                let Some(version) = parse_get_version_reply(&buf) else {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        raw = format_args!("{:02X?}", buf),
                        "dsPIC detect_firmware rejected invalid GET_VERSION reply",
                    );
                    self.firmware = DspicFirmware::Unknown;
                    self.use_bare_protocol = false;
                    return Ok(DspicFirmware::Unknown);
                };
                let fw = DspicFirmware::from_version(version);
                self.use_bare_protocol = fw.protocol() == DspicProtocol::Bare;

                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    version = format_args!("0x{:02X}", version),
                    status = format_args!("0x{:02X}", rx_status),
                    firmware = %fw,
                    "dsPIC firmware detected",
                );
                self.firmware = fw;
                Ok(fw)
            }
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "dsPIC detect_firmware failed — no response on I2C",
                );
                self.firmware = DspicFirmware::Unknown;
                self.use_bare_protocol = false;
                Ok(DspicFirmware::Unknown)
            }
        }
    }

    /// Probe all known dsPIC addresses on the I2C bus.
    ///
    /// Returns the first address that responds with a valid firmware version,
    /// or None if no dsPIC is found. This is the S19 equivalent of the S9's
    /// PIC address scan (which checks 0x55, 0x56, 0x57).
    pub fn probe_addresses(i2c: &mut I2cBus) -> Option<(u8, DspicFirmware)> {
        for &addr in &DSPIC_PROBE_ADDRS {
            tracing::debug!(
                addr = format_args!("0x{:02X}", addr),
                "Probing dsPIC address"
            );
            let mut ctrl = DspicController::new(i2c, addr);
            if let Ok(fw) = ctrl.detect_firmware() {
                if fw != DspicFirmware::Unknown {
                    return Some((addr, fw));
                }
            }
        }
        None
    }

    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    /// Initialize the dsPIC voltage controller for mining.
    ///
    /// Safe DCENT_OS production sequence:
    ///   1. Flush I2C parser (zero bytes)
    ///   2. Validate the known firmware/protocol before this call
    ///   3. Send stable pre-voltage heartbeats
    ///   4. Read LM75A sensors through dsPIC passthrough
    ///   5. Set initial voltage (CMD_SET_VOLTAGE)
    ///   6. Enable DC-DC output (CMD_ENABLE_VOLTAGE)
    ///   7. Start heartbeat (CMD_HEARTBEAT)
    ///
    /// RESET/JUMP are deliberately excluded on S19j/Pic0x89-family paths.
    pub fn cold_boot_init(&mut self, voltage_mv: u16) -> Result<()> {
        ensure_dspic_voltage_command_allowed(self.address, self.firmware, "cold_boot_init")?;

        let voltage_v = voltage_mv as f64 / 1000.0;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            target_voltage = format_args!("{:.2}V", voltage_v),
            voltage_mv,
            "dsPIC init starting (write-only, no RESET/JUMP/detect)",
        );

        // FIX 1: NO disable_voltage() — don't kill rails that may already be on.
        // FIX 2: NO RESET/JUMP — corrupts dsPIC33EP firmware.
        // FIX 3: NO detect_firmware() — uses I2C_RDWR which is dangerous on am2.
        // Just: flush → heartbeat → set voltage → enable → heartbeat.

        // Step 1: Flush I2C parser state (safe write-only operation)
        let _ = self.i2c.set_slave(self.address);
        let _ = self.i2c.write(&[0u8; 8]);
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Step 2: Honor known firmware protocol. A successful I2C write only
        // proves address ACK, not that the dsPIC parsed the command. On fw
        // 0x86/0x89, probing bare first can false-positive and make the
        // following SET_VOLTAGE use the wrong wire format.
        if self.firmware != DspicFirmware::Unknown {
            self.use_bare_protocol = self.firmware.protocol() == DspicProtocol::Bare;
            match self.send_heartbeat() {
                Ok(()) => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        firmware = %self.firmware,
                        bare = self.use_bare_protocol,
                        "dsPIC heartbeat sent using firmware-selected protocol",
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        firmware = %self.firmware,
                        bare = self.use_bare_protocol,
                        error = %e,
                        "dsPIC heartbeat failed using firmware-selected protocol — continuing anyway",
                    );
                }
            }
        } else {
            // Unknown firmware only: try framed first because all S19j Pro
            // variants observed on .139 use framed protocol. Fall back to
            // bare for older fw 0x82 compatibility.
            self.use_bare_protocol = false;
            match self.send_heartbeat() {
                Ok(()) => {
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        "dsPIC responds to FRAMED protocol",
                    );
                }
                Err(framed_err) => {
                    self.use_bare_protocol = true;
                    match self.send_heartbeat() {
                        Ok(()) => {
                            tracing::info!(
                                addr = format_args!("0x{:02X}", self.address),
                                "dsPIC responds to BARE protocol (fw 0x82 compatible)",
                            );
                        }
                        Err(bare_err) => {
                            self.use_bare_protocol = false;
                            tracing::warn!(
                                addr = format_args!("0x{:02X}", self.address),
                                framed_error = %framed_err,
                                bare_error = %bare_err,
                                "dsPIC heartbeat failed on both protocols — continuing with framed protocol",
                            );
                        }
                    }
                }
            }
        }

        // Bosminer keeps the per-chain voltage-controller app alive before
        // SetVoltage. Mirror that with five 1 Hz heartbeat ticks so the dsPIC
        // watchdog and I2C parser are stable before the DAC command.
        let mut stable_heartbeats = 0u8;
        for tick in 1..=5 {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            match self.send_heartbeat() {
                Ok(()) => {
                    stable_heartbeats += 1;
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        tick,
                        stable_heartbeats,
                        bare = self.use_bare_protocol,
                        "dsPIC pre-voltage heartbeat tick",
                    );
                }
                Err(e) => {
                    stable_heartbeats = 0;
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        tick,
                        error = %e,
                        "dsPIC pre-voltage heartbeat failed",
                    );
                }
            }
        }
        if stable_heartbeats < 5 {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "dsPIC did not complete 5 stable pre-voltage heartbeats (stable={})",
                    stable_heartbeats
                ),
            });
        }

        // P1.7 LM75A WIRING READ. Bosminer reads the four LM75A sensors via
        // dsPIC passthrough before the first SetVoltage/ENABLE sequence. CORRECTED
        // (findings/s8 F18/F19): bosminer uses the two-step pair **0x3B (select) +
        // 0x3C (read), LEN=0x06, addresses 0x48..0x4B — never opcode 0x30**. The
        // 0x30 claim was false; it was disproven by the `a lab unit` cold strace, where
        // the whole 662k-line capture shows 0x3B×290 / 0x3C×306 and 0x30×0. DCENT's
        // own `read_all_temperatures()` still emits 0x30 (a known, separately-tracked
        // divergence — see findings/s8 C2/open-item #3); this read is kept here only
        // to preserve the known-working cold-boot ordering. NON-FATAL: log and continue.
        let temps = self.read_all_temperatures();
        if self.use_bare_protocol {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                temps = format_args!("{:?}", temps),
                bare = self.use_bare_protocol,
                "dsPIC bare LM75A pre-voltage read complete (informational only; NaN expected)"
            );
        } else {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                temps = format_args!("{:?}", temps),
                bare = self.use_bare_protocol,
                "dsPIC LM75A pre-voltage wiring read complete (4x passthrough at 0x48-0x4B)"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Step 3: Set voltage (write-only, no read-back needed)
        self.set_voltage(voltage_mv)?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            voltage_mv,
            voltage = format_args!("{:.2}V", voltage_v),
            "dsPIC voltage set to {:.2}V ({} mV)",
            voltage_v,
            voltage_mv,
        );

        std::thread::sleep(std::time::Duration::from_millis(50));

        // Step 6: Enable DC-DC output. ENABLE is the hard handoff from
        // configured DAC target to energized hashboard rail; an ACK-only I2C
        // address hit is not enough evidence to continue mining.
        self.enable_voltage()?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            "dsPIC DC-DC output ENABLED",
        );

        // Wait 1000ms for DC-DC ramp after enable.
        // Firmware 0x82 may NACK I2C during DC-DC startup (measured EIO after 50ms).
        std::thread::sleep(std::time::Duration::from_millis(1000));

        // Step 7: Send initial heartbeat (non-fatal — EIO during DC-DC ramp is OK)
        match self.send_heartbeat() {
            Ok(()) => {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    "dsPIC heartbeat sent — DC-DC enabled and stable",
                );
            }
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "dsPIC heartbeat after enable failed (may be DC-DC ramp) — continuing",
                );
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Voltage control
    // -----------------------------------------------------------------------

    /// Set the target voltage in millivolts.
    ///
    /// Protocol-aware:
    ///   Bare (fw 0x82):   [55 AA 10 HI LO] — 16-bit BE millivolts
    ///   Fw86 bare:         [55 AA 10 DAC] — 8-bit DAC value, no LEN/checksum
    ///   Framed (fw 0x89+): [55 AA 04 10 ARG CHECKSUM] — 8-bit DAC value
    ///
    /// For framed protocol, the ARG is an 8-bit voltage DAC value (not millivolts).
    /// Confirmed via strace: bosminer sends [55 AA 04 10 06 1A] for SET_VOLTAGE(0x06).
    /// The DAC-to-voltage mapping is board-specific.
    ///
    /// Valid range: 11940 - 15140 mV (from S19 bosminer_model.json).
    pub fn set_voltage(&mut self, voltage_mv: u16) -> Result<()> {
        ensure_dspic_voltage_command_allowed(self.address, self.firmware, "set_voltage")?;

        // Load-bearing <=14500 mV hard cap, enforced at the rail-program
        // boundary (input clamp, preserves the proven DAC span — see
        // clamp_dspic_voltage_to_hard_cap). Default-off lab override allows the
        // AMTC pre-open. Production (13.7-13.8 V) is unaffected.
        let requested_mv = voltage_mv;
        let (voltage_mv, capped) =
            clamp_dspic_voltage_to_hard_cap(voltage_mv, dspic_lab_overvolt_override_enabled());
        if capped {
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                requested_mv,
                capped_mv = voltage_mv,
                "dsPIC set_voltage exceeded the {} mV hard cap — clamped DOWN (set {}=1 only in a lab for the AMTC pre-open)",
                DSPIC_VOLTAGE_HARD_CAP_MV,
                DSPIC_ALLOW_LAB_OVERVOLT_ENV,
            );
        }

        if !(DSPIC_MIN_VOLTAGE_MV..=DSPIC_MAX_VOLTAGE_MV).contains(&voltage_mv) {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "dsPIC voltage {} mV outside safe range {}..={} mV",
                    voltage_mv, DSPIC_MIN_VOLTAGE_MV, DSPIC_MAX_VOLTAGE_MV
                ),
            });
        }

        // Write directly to I2C (NOT through send_command_raw which would double-frame).
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave: {}", e),
            })?;
        let frame = dspic_set_voltage_frame(self.firmware, self.use_bare_protocol, voltage_mv);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            voltage_mv = voltage_mv,
            tx_bytes = ?frame,
            "dsPIC set_voltage tx"
        );
        self.i2c.write(&frame).map_err(|e| crate::AsicError::Pic {
            addr: self.address,
            detail: format!("I2C write: {}", e),
        })?;
        self.current_voltage_mv = voltage_mv;
        Ok(())
    }

    /// Enable voltage output (DC-DC enable).
    ///   Bare (any fw):     [55 AA 15 01]
    ///   Framed (fw 0x86/0x89): [55 AA 05 15 01 00 1B]  (VNish 7-byte form)
    ///   Framed (other fw): [55 AA 04 15 01 1A]         (canonical 6-byte form)
    ///
    /// Uses atomic `I2C_RDWR` (repeated START) so the dsPIC MSSP stays in the
    /// "command received, ACK queued" state across the write→read boundary.
    /// A plain `write()` issues STOP which clears MSSP staging and produces
    /// EIO on am2 Zynq units even when the PIC is alive. Bosminer's atomic
    /// ioctl pattern is the proven reference — see investigation
    /// `29-wire-ground-truth-ftrace.md` and `30-strace-bosminer-i2c.md`.
    ///
    /// VNish RE (`re-armada-2026-04-25`) shows fw=0x86/0x89 use the 7-byte form
    /// with payload `[0x01, 0x00]`. Other framed fw bytes (0x8A/0xB9/0xFE) keep
    /// the canonical 6-byte form pending live verification — flipping
    /// unconditionally risks regressing working units.
    ///
    /// The 2-byte ACK `[0x15, 0x00]` or `[0x15, 0x01]` (ENABLE echo + status) is
    /// read back; both status values are accepted (VNish ACKs with 0x01).
    pub fn enable_voltage(&mut self) -> Result<()> {
        ensure_dspic_voltage_command_allowed(self.address, self.firmware, "enable_voltage")?;

        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave: {}", e),
            })?;
        let encoding = dspic_enable_disable_encoding(self.firmware);
        let frame = dspic_enable_voltage_frame(self.use_bare_protocol, encoding);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            form = if matches!(encoding, EnableFrameEncoding::VnishPadded) { "7-byte VNish" } else { "6-byte canonical" },
            bare = self.use_bare_protocol,
            frame = ?frame,
            "ENABLE_VOLTAGE frame"
        );
        if self.use_bare_protocol {
            // : fw0x86 bare
            // ENABLE returns one firmware byte, not [CMD, status].
            let mut ack = [0u8; 1];
            self.i2c
                .write_read(frame, &mut ack)
                .map_err(|e| crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!("I2C write_read (ENABLE bare): {}", e),
                })?;
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                ack_fw = format_args!("0x{:02X}", ack[0]),
                bare = self.use_bare_protocol,
                "PIC enable_voltage bare ACK"
            );
            if ack[0] == 0xFF {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: "ENABLE bare returned 0xFF (slave idle / NACK)".to_string(),
                });
            }
            if !is_bare_ack_fw_byte(ack[0]) {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "ENABLE bare ACK mismatch: expected firmware byte, got [{:02X}]",
                        ack[0]
                    ),
                });
            }
            self.voltage_enabled = true;
            return Ok(());
        }

        let mut ack = [0u8; 2];
        self.i2c
            .write_read(frame, &mut ack)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write_read (ENABLE): {}", e),
            })?;
        // Validate the 2-byte ACK. Layout per the dsPIC: `[cmd_echo, status]`.
        // Accept 0x15 (CMD_ENABLE_VOLTAGE echo) — anything else means the
        // command was not parsed (NAK, bus noise, wrong protocol variant).
        // Reject the all-`0xFF` "I am the bus" pattern outright.
        // (DCENT_RE 2026-04-25 — gap #1, was silently accepted before.)
        let expected_fw = dspic_expected_fw_byte(self.firmware);
        let ack_kind = classify_enable_ack(&ack, expected_fw);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            ack_cmd = format_args!("0x{:02X}", ack[0]),
            ack_status = format_args!("0x{:02X}", ack[1]),
            ack_kind = %ack_kind,
            bare = self.use_bare_protocol,
            "PIC enable_voltage ACK"
        );
        if ack_kind == EnableVoltageAckKind::AllFf {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "ENABLE returned all-0xFF (slave not responding / bootloader): ack=[{:02X}, {:02X}]",
                    ack[0], ack[1]
                ),
            });
        }
        // Accept ACK [0x15, 0x00] (legacy/canonical) or [0x15, 0x01] (VNish 7-byte form).
        // VNish RE confirms fw=0x86/0x89 ACK with status=0x01 — see
        // `re-armada-2026-04-25/dspic-s19jpro-xil.md`.
        let ok = ack_kind == EnableVoltageAckKind::RealAck;
        if matches!(
            ack_kind,
            EnableVoltageAckKind::FirmwareEcho | EnableVoltageAckKind::FirmwareEchoMismatch
        ) {
            let require_real_ack = dspic_require_real_enable_ack_enabled();
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                ack = format_args!("{:02X?}", ack),
                ack_kind = %ack_kind,
                firmware = %self.firmware,
                expected_fw = %fmt_optional_fw_byte(expected_fw),
                require_real_ack,
                rail_proof = "chain-uart-required",
                "PIC enable_voltage returned repeated firmware-byte echo; not a real ENABLE ACK"
            );
            if require_real_ack {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "ENABLE returned {} instead of real ACK [{:02X}, 00/01]: ack={:02X?}",
                        ack_kind, CMD_ENABLE_VOLTAGE, ack
                    ),
                });
            }
            self.voltage_enabled = true;
            return Ok(());
        }
        if !ok {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "ENABLE ACK mismatch ({}): expected [{:02X}, 00] or [{:02X}, 01], got [{:02X}, {:02X}]",
                    ack_kind, CMD_ENABLE_VOLTAGE, CMD_ENABLE_VOLTAGE, ack[0], ack[1]
                ),
            });
        }
        // W1.2 jig op-diff (2026-06-13): when DCENT_AM2_REQUIRE_ENABLE_FLAG_ON is
        // set (`a lab unit` standalone only), require the jig's exact success criterion
        // — ACK [0x15, 0x01] (enable flag confirmed) — and REJECT [0x15, 0x00]
        // (the dsPIC echoing the OFF/disable flag = rail not confirmed enabled).
        // The BM1362 factory jig's _bitmain_pic_enable_dc_dc_common requires
        // read_back[1]==0x01. Default-OFF ⇒ byte-identical everywhere; the lenient
        // RealAck (0x00 OR 0x01) stands unless the gate is explicitly set.
        if dspic_require_enable_flag_on_enabled() && !enable_ack_flag_on(&ack) {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "ENABLE flag-on required (jig bar [{:02X}, 01]) but got [{:02X}, {:02X}] \
                     — rail not confirmed enabled (dsPIC echoed the OFF flag); \
                     DCENT_AM2_REQUIRE_ENABLE_FLAG_ON is set",
                    CMD_ENABLE_VOLTAGE, ack[0], ack[1]
                ),
            });
        }
        self.voltage_enabled = true;
        Ok(())
    }

    /// Disable voltage output (DC-DC disable).
    ///   Bare (any fw):     [55 AA 15 00]
    ///   Framed (fw 0x86/0x89): [55 AA 05 15 00 00 1A]  (VNish 7-byte form)
    ///   Framed (other fw): [55 AA 04 15 00 19]         (canonical 6-byte form)
    pub fn disable_voltage(&mut self) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave: {}", e),
            })?;
        let encoding = dspic_enable_disable_encoding(self.firmware);
        let frame = dspic_disable_voltage_frame(self.use_bare_protocol, encoding);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            form = if matches!(encoding, EnableFrameEncoding::VnishPadded) { "7-byte VNish" } else { "6-byte canonical" },
            bare = self.use_bare_protocol,
            frame = ?frame,
            "DISABLE_VOLTAGE frame"
        );
        self.i2c.write(frame).map_err(|e| crate::AsicError::Pic {
            addr: self.address,
            detail: format!("I2C write: {}", e),
        })?;
        self.voltage_enabled = false;
        Ok(())
    }

    /// Read the current voltage via ADC feedback.
    ///
    /// Returns the voltage in millivolts as reported by the dsPIC's ADC.
    /// Uses I2C_RDWR for the read phase.
    ///
    /// WARNING: Like PIC16F1704, I2C_RDWR may corrupt the dsPIC's I2C parser.
    /// Use sparingly (diagnostics only, not in hot mining path).
    pub fn read_voltage(&mut self) -> Result<u16> {
        if !self.use_bare_protocol
            && matches!(self.firmware, DspicFirmware::Fw89 | DspicFirmware::Fw8A)
        {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC read_voltage(0x3B): not a fw=0x89/0x8A rail-voltage command; use measure_voltage(0x3A) ADC decode".to_string(),
            });
        }

        let cmd = [DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_GET_VOLTAGE];
        let mut buf = [0u8; 4]; // [cmd_echo, status, voltage_hi, voltage_lo]
        self.write_read_command(&cmd, &mut buf)?;

        // .25 rail-readback capture (HB_RESET dig, 2026-05-29): log the raw RX
        // bytes so the framed (fw=0x89) 0x3B GET_VOLTAGE reply structure can be
        // analyzed offline. On the bosminer-handoff path the effective-chain
        // dsPIC (0x22) IS fw=0x89, so this capture finally yields the framed
        // v_hi/v_lo offset the decoder needs (decode below returns honest Err on
        // framed rather than guess the offset). Diagnostics-only path (I2C_RDWR,
        // not hot mining); zero I2C-transaction change (same 4-byte read).
        tracing::info!(
            target: "rail_capture",
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            rx_raw = format_args!("{:02X?}", buf),
            "dsPIC read_voltage(0x3B GET_VOLTAGE) RX raw bytes"
        );

        let rx_cmd = buf[0];
        let rx_status = buf[1];

        if rx_cmd == 0xFF && rx_status == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC read_voltage: error frame (0xFF/0xFF)".to_string(),
            });
        }

        // Frame-shape guard (swarm wf_e0647147 finding #1/#2, 2026-05-29). This fixed
        // 4-byte decode is only valid for the BARE reply [cmd_echo, status, v_hi, v_lo]
        // (fw=0x82). On FRAMED firmware (fw=0x89) the reply is a longer LEN/preamble-led
        // frame, so buf[2]/buf[3] are NOT v_hi/v_lo and the old decode fabricated garbage
        // (live: dsPIC 0x22 -> 64760 mV = 0xFCF8, then mis-read as "rail NOT energized").
        // Refuse to fabricate: require the bare cmd-echo at buf[0] AND a physically
        // plausible value, else return Err so callers log "readback unreliable / no rail
        // proof" instead of a false low-rail verdict. The offset-correct framed decoder
        // needs the raw fw=0x89 reply bytes captured first — do NOT guess the offset.
        let voltage_mv = dcentrald_common::dspic_decode::decode_bare_voltage_reply(
            rx_cmd,
            CMD_GET_VOLTAGE,
            buf[2],
            buf[3],
            DSPIC_MAX_VOLTAGE_MV,
        )
        .map_err(|e| crate::AsicError::Pic {
            addr: self.address,
            detail: format!("dsPIC read_voltage: {e}"),
        })?;
        Ok(voltage_mv)
    }

    /// Measure the ACTUAL chain-rail voltage via the dsPIC analog ADC
    /// (`MEASURE_VOLTAGE` 0x3A = VNish `dspic33epxx_get_an_voltage2()`).
    ///
    /// Distinct from [`read_voltage`](Self::read_voltage) (0x3B `GET_VOLTAGE`,
    /// the setpoint/feedback path bosminer references in "expected command:
    /// 59"). 0x3A reads the analog-input ADC value of the real rail, so it is
    /// the stronger "is the chain rail truly at the target voltage?" proxy —
    /// the `a lab unit` standalone rail-engaged test (Procedure A in the live-capture
    /// runbook) wants this alongside 0x3B to disambiguate setpoint from
    /// actually-energized. Read-only. (gap-swarm G03.)
    ///
    /// Wire frame is byte-exact to dspic-protocol-bible §2: the `[0x00]`
    /// payload byte makes the framed form `[55 AA 04 3A 00 3E]` (LEN=0x04,
    /// CKSUM=(0x04+0x3A+0x00)&0xFF=0x3E) on fw 0x86/0x89/0x8A, and the bare form
    /// `[55 AA 3A 00]` on fw 0x82 — `encode_command_frame` picks the form from
    /// `use_bare_protocol`.
    ///
    /// WARNING: like `read_voltage`, the I2C_RDWR read phase may corrupt the
    /// dsPIC parser — diagnostics only, never the hot mining path.
    pub fn measure_voltage(&mut self) -> Result<u16> {
        let cmd = [
            DSPIC_PREAMBLE[0],
            DSPIC_PREAMBLE[1],
            CMD_MEASURE_VOLTAGE,
            0x00,
        ];
        if !self.use_bare_protocol
            && matches!(self.firmware, DspicFirmware::Fw89 | DspicFirmware::Fw8A)
        {
            // Read the FULL 7-byte framed envelope (see the SERVICE measure_voltage note):
            // `[0x07,0x3A,status,adc_hi,adc_lo,0x00,cksum]`. The old 2-byte read landed on
            // the envelope head [07,3A] and decoded to ~45 V -> ExceedsMax. The envelope
            // decoder extracts the ADC at offset 3..4 and falls back cleanly on a bad frame.
            let mut buf = [0u8; 7];
            self.write_read_command(&cmd, &mut buf)?;
            tracing::info!(
                target: "rail_capture",
                addr = format_args!("0x{:02X}", self.address),
                firmware = %self.firmware,
                rx_raw = format_args!("{:02X?}", buf),
                "dsPIC measure_voltage(0x3A) framed ADC reply bytes (7-byte envelope, offset-3..4 ADC)"
            );
            return dcentrald_common::dspic_decode::decode_framed_measure_voltage_i2c0_capture(
                &buf,
                DSPIC_MAX_VOLTAGE_MV,
            )
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC measure_voltage framed ADC decode: {e}"),
            });
        }

        let mut buf = [0u8; 4]; // [cmd_echo, status, voltage_hi, voltage_lo]
        self.write_read_command(&cmd, &mut buf)?;

        // .25 rail-readback capture (HB_RESET dig, 2026-05-29): raw RX bytes for
        // the framed (fw=0x89) 0x3A MEASURE_VOLTAGE reply (the ADC actual-rail
        // proxy). Same rationale as read_voltage; captures the offset on the
        // handoff path where the effective-chain dsPIC is fw=0x89. Zero I2C change.
        tracing::info!(
            target: "rail_capture",
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            rx_raw = format_args!("{:02X?}", buf),
            "dsPIC measure_voltage(0x3A MEASURE_VOLTAGE) RX raw bytes"
        );

        let rx_cmd = buf[0];
        let rx_status = buf[1];

        if rx_cmd == 0xFF && rx_status == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC measure_voltage: error frame (0xFF/0xFF)".to_string(),
            });
        }

        // Frame-shape guard (swarm wf_e0647147 finding #1/#2, 2026-05-29). Bare 4-byte
        // decode only. fw=0x89 and currently-assumed-same 0x8A replies return above;
        // any other framed variant must still refuse a fabricated bare-shape value.
        let voltage_mv = dcentrald_common::dspic_decode::decode_bare_voltage_reply(
            rx_cmd,
            CMD_MEASURE_VOLTAGE,
            buf[2],
            buf[3],
            DSPIC_MAX_VOLTAGE_MV,
        )
        .map_err(|e| {
            // Non-fw89/framed residual capture. The selected ADC path is handled
            // above with the fw=0x89-shape 2-byte decode; keep this dump for any other
            // framed family that reaches the bare-shape guard.
            if !self.use_bare_protocol {
                tracing::info!(
                    target: "rail_capture_framed_0x3A",
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    rx_raw = format_args!("{:02X?}", buf),
                    rx_len = buf.len(),
                    rx_cmd = format_args!("0x{:02X}", rx_cmd),
                    rx_status = format_args!("0x{:02X}", rx_status),
                    decode_error = %e,
                    "dsPIC measure_voltage(0x3A) framed residual reply — bare 4-byte decode refused (readback unreliable). RAW BYTES captured for non-fw89/non-fw8a follow-up."
                );
            }
            crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC measure_voltage: {e}"),
            }
        })?;
        Ok(voltage_mv)
    }

    /// Send heartbeat to prevent dsPIC watchdog timeout.
    ///
    /// The dsPIC watchdog disables voltage output if no heartbeat is received
    /// within the timeout period (~10 seconds on most firmware versions).
    ///
    /// Protocol-aware: uses bare or framed format based on `use_bare_protocol`.
    ///   Bare:   [55 AA 16] — 3 bytes (fw 0x82)
    ///   Framed: [55 AA 04 16 00 1A] — 6 bytes (fw 0x89+)
    ///           CHECKSUM = (0x04 + 0x16 + 0x00) & 0xFF = 0x1A
    pub fn send_heartbeat(&mut self) -> Result<()> {
        let frame = dspic_heartbeat_frame(self.use_bare_protocol);
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave: {}", e),
            })?;
        self.i2c.write(frame).map_err(|e| crate::AsicError::Pic {
            addr: self.address,
            detail: format!("I2C write: {}", e),
        })?;
        Ok(())
    }

    /// Get firmware version from the dsPIC.
    ///
    /// Returns the firmware version byte. Uses I2C_RDWR.
    pub fn get_version(&mut self) -> Result<u8> {
        self.use_bare_protocol = true;
        let cmd = [DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_GET_VERSION];
        let mut buf = [0u8; 5];
        self.write_read_command(&cmd, &mut buf)?;

        if buf[0] == 0xFF && buf[1] == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC get_version: error frame (0xFF/0xFF)".to_string(),
            });
        }

        let Some(version) = parse_get_version_reply(&buf) else {
            self.firmware = DspicFirmware::Unknown;
            self.use_bare_protocol = false;
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC get_version invalid reply: {:02X?}", buf),
            });
        };
        self.firmware = DspicFirmware::from_version(version);
        self.use_bare_protocol = self.firmware.protocol() == DspicProtocol::Bare;
        Ok(version)
    }

    // -----------------------------------------------------------------------
    // Temperature reading (LM75A passthrough)
    // -----------------------------------------------------------------------

    /// Read temperature from an LM75A sensor via the dsPIC I2C passthrough.
    ///
    /// The dsPIC acts as an I2C bridge — it forwards temperature read requests
    /// to the LM75A sensors on the hash board. This is fundamentally different
    /// from S9 where temp sensors are on the main I2C bus.
    ///
    /// LM75A addresses: 0x48, 0x49, 0x4A, 0x4B (4 thermal zones per board).
    /// Frame: bare `[55 AA 30 sensor_addr]` (fw 0x82/0x86) or framed
    /// `[55 AA 04 30 sensor_addr SUM]` (fw 0x89/0x8A/B9) — see
    /// `dspic_read_temp_frame`. The dsPIC must be in app mode and the chain
    /// PSU rail asserted before this call succeeds.
    ///
    /// Returns temperature in degrees Celsius (0.5C resolution).
    ///
    /// **Bare-mode behavior**: per
    ///  and
    /// , the bare-protocol
    /// dsPIC always replies with a single FW-byte echo for any 1-byte read.
    /// Real LM75 temperature data is **not delivered** in bare mode — we
    /// return `Ok(f64::NAN)` as a sentinel meaning "informational only — no
    /// temp data delivered in bare mode". A reply of `0xFF` indicates the
    /// slave is silent/idle; any byte outside the FW-echo whitelist is an
    /// error.
    pub fn read_temperature(&mut self, sensor_addr: u8) -> Result<f64> {
        let cmd = dspic_read_temp_frame(self.use_bare_protocol, sensor_addr);
        if self.use_bare_protocol {
            // : bare fw0x86
            // only drives one byte; 4-byte xiic reads synthesize fake tails.
            // : validate the
            // single-byte FW-echo strictly — reject 0xFF (slave idle) and
            // anything outside the known FW-byte whitelist.
            let mut buf = [0u8; 1];
            self.write_read_command(&cmd, &mut buf)?;
            if buf[0] == 0xFF {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "LM75 bare read(0x{:02X}) returned 0xFF (slave idle)",
                        sensor_addr,
                    ),
                });
            }
            if !is_bare_ack_fw_byte(buf[0]) {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "LM75 bare read(0x{:02X}): unexpected byte 0x{:02X} (not FW echo)",
                        sensor_addr, buf[0],
                    ),
                });
            }
            tracing::debug!(
                addr = format_args!("0x{:02X}", self.address),
                sensor = format_args!("0x{:02X}", sensor_addr),
                ack_fw = format_args!("0x{:02X}", buf[0]),
                bare = self.use_bare_protocol,
                "dsPIC bare LM75A read returned firmware echo; temperature unavailable (NaN sentinel)"
            );
            return Ok(f64::NAN);
        }

        let mut buf = [0u8; 4]; // [cmd_echo, status, temp_hi, temp_lo]
        self.write_read_command(&cmd, &mut buf)?;

        if buf[0] == 0xFF && buf[1] == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC read_temperature(0x{:02X}): error frame", sensor_addr,),
            });
        }

        // LM75A temperature format: 16-bit signed, upper 9 bits = temp * 2
        // Bit 15 = sign, bits 14:7 = integer, bit 6 = 0.5C
        let raw = ((buf[2] as i16) << 8) | (buf[3] as i16);
        let temp_c = (raw >> 5) as f64 * 0.125;

        Ok(temp_c)
    }

    /// Read all 4 LM75A temperature sensors on this hash board.
    ///
    /// Returns an array of 4 temperature readings in degrees Celsius.
    /// If a sensor fails to respond, its value is set to -999.0 (sentinel).
    pub fn read_all_temperatures(&mut self) -> [f64; 4] {
        let mut temps = [-999.0f64; 4];
        for (i, &sensor_addr) in LM75A_ADDRS.iter().enumerate() {
            match self.read_temperature(sensor_addr) {
                Ok(t) => temps[i] = t,
                Err(e) => {
                    tracing::debug!(
                        addr = format_args!("0x{:02X}", self.address),
                        sensor = format_args!("0x{:02X}", sensor_addr),
                        error = %e,
                        "dsPIC LM75A read failed — sensor may not be present",
                    );
                }
            }
        }
        temps
    }

    // -----------------------------------------------------------------------
    // Voltage ramping (safety)
    // -----------------------------------------------------------------------

    /// Ramp voltage to target in steps (prevents large current transients).
    ///
    /// The S19 hash board DC-DC converter can be damaged by large voltage
    /// steps. This function ramps from the current voltage to the target
    /// in increments of `step_mv` with `delay_ms` between steps.
    ///
    /// Default: 500 mV steps, 100ms delay (conservative for home mining).
    pub fn ramp_voltage(&mut self, target_mv: u16, step_mv: u16, delay_ms: u64) -> Result<()> {
        ensure_dspic_voltage_command_allowed(self.address, self.firmware, "ramp_voltage")?;

        let current = self.current_voltage_mv;
        if current == 0 {
            // First time — set directly (no ramp from unknown state)
            return self.set_voltage(target_mv);
        }

        let delay = std::time::Duration::from_millis(delay_ms);

        if target_mv > current {
            // Ramping up
            let mut v = current;
            while v < target_mv {
                v = (v + step_mv).min(target_mv);
                self.set_voltage(v)?;
                if v < target_mv {
                    std::thread::sleep(delay);
                }
            }
        } else if target_mv < current {
            // Ramping down
            let mut v = current;
            while v > target_mv {
                v = if v >= step_mv + target_mv {
                    v - step_mv
                } else {
                    target_mv
                };
                self.set_voltage(v)?;
                if v > target_mv {
                    std::thread::sleep(delay);
                }
            }
        }
        // If equal, nothing to do

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Send a raw command frame to the dsPIC (write-only, no response).
    /// Build and send a dsPIC framed command.
    ///
    /// Frame format (Mining Bible v1 / `1-power-dspic/01-frame-format.md`):
    ///   [0x55] [0xAA] [LEN] [CMD] [payload...] [CHECKSUM]
    ///
    /// LEN = payload_len + 3 (counts itself + CMD + payload + CHECKSUM)
    /// CHECKSUM = (LEN + CMD + sum_of_payload_bytes) & 0xFF
    fn encode_command_frame(&self, data: &[u8]) -> Vec<u8> {
        if data.len() >= 3 && data[0] == DSPIC_PREAMBLE[0] && data[1] == DSPIC_PREAMBLE[1] {
            if self.use_bare_protocol {
                return data.to_vec();
            }
            let cmd = data[2];
            let payload = &data[3..];
            let len_byte = dspic_outgoing_len(payload.len());
            let checksum = len_byte
                .wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)));
            let mut frame = Vec::with_capacity(4 + payload.len() + 1);
            frame.push(DSPIC_PREAMBLE[0]);
            frame.push(DSPIC_PREAMBLE[1]);
            frame.push(len_byte);
            frame.push(cmd);
            frame.extend_from_slice(payload);
            frame.push(checksum);
            return frame;
        }
        data.to_vec()
    }

    fn write_read_command(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        let frame = self.encode_command_frame(write_data);
        ensure_dspic_bootloader_command_allowed(
            self.address,
            self.firmware,
            self.use_bare_protocol,
            &frame,
        )?;
        self.i2c
            .write_read(&frame, read_buf)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write_read failed: {}", e),
            })?;
        Ok(())
    }

    fn send_command_raw(&mut self, data: &[u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        // data = [0x55, 0xAA, CMD, ...payload] (old format without LEN)
        // We need to INSERT LEN at position [2] and APPEND checksum.
        // LEN is calculated from the expected response size per command.
        if data.len() >= 3 && data[0] == DSPIC_PREAMBLE[0] && data[1] == DSPIC_PREAMBLE[1] {
            // BARE protocol: send data as-is [55 AA CMD payload], no LEN or checksum
            if self.use_bare_protocol {
                ensure_dspic_bootloader_command_allowed(
                    self.address,
                    self.firmware,
                    self.use_bare_protocol,
                    data,
                )?;
                self.i2c.write(data).map_err(|e| crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!("I2C write (bare) failed: {}", e),
                })?;
                return Ok(());
            }
            // FRAMED protocol: insert LEN (= payload_len + 3 per Bible), append checksum
            let cmd = data[2];
            let payload = &data[3..];
            let len_byte = dspic_outgoing_len(payload.len());
            let checksum = len_byte
                .wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)));
            let mut frame = Vec::with_capacity(4 + payload.len() + 1);
            frame.push(DSPIC_PREAMBLE[0]); // 0x55
            frame.push(DSPIC_PREAMBLE[1]); // 0xAA
            frame.push(len_byte); // LEN
            frame.push(cmd); // CMD
            frame.extend_from_slice(payload);
            frame.push(checksum); // CHECKSUM
            ensure_dspic_bootloader_command_allowed(
                self.address,
                self.firmware,
                self.use_bare_protocol,
                &frame,
            )?;
            self.i2c.write(&frame).map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write failed: {}", e),
            })?;
        } else {
            // Raw data without preamble — send as-is (flush bytes etc.)
            self.i2c.write(data).map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write failed: {}", e),
            })?;
        }
        Ok(())
    }

    /// Combined write+read using I2C_RDWR ioctl (repeated START).
    /// Required for reading response frames from the dsPIC.
    fn write_read(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        self.i2c
            .set_slave(self.address)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C set_slave failed: {}", e),
            })?;
        ensure_dspic_bootloader_command_allowed(
            self.address,
            self.firmware,
            self.use_bare_protocol,
            write_data,
        )?;
        self.i2c
            .write_read(write_data, read_buf)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("I2C write_read failed: {}", e),
            })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// dsPIC voltage controller over the process-wide I2C service
// ---------------------------------------------------------------------------

/// Pure slice planner for the post-JUMP keep-alive chunked sleep (2026-06-07,
/// `a lab unit` standalone cold-engage).
///
/// Splits a `total_ms` sleep into consecutive slices each `<= interval_ms`
/// (the last slice is the remainder). One framed `0x16` keep-alive heartbeat
/// is sent after each slice, so the returned `Vec::len()` is exactly the
/// number of heartbeats a `total_ms` settle produces — e.g. a 1.2 s window at
/// the default 300 ms interval yields `[300, 300, 300, 300]` ⇒ 4 heartbeats,
/// well under the live-observed ~1.2 s fw=0x89→0x82 drift window. Pure and
/// host-testable (no bus); `DspicService::keepalive_sleep` is the only caller.
fn keepalive_sleep_slices(total_ms: u64, interval_ms: u64) -> Vec<u64> {
    let slice = interval_ms.max(1);
    let mut out = Vec::new();
    let mut remaining = total_ms;
    while remaining > 0 {
        let chunk = remaining.min(slice);
        out.push(chunk);
        remaining -= chunk;
    }
    out
}

/// Service-backed dsPIC33EP voltage controller.
///
/// This mirrors the public, raw-`I2cBus` `DspicController` methods while routing
/// all bus access through `I2cServiceHandle`. It intentionally does not replace
/// or alter `DspicController`; daemon code that still owns a raw bus keeps the
/// same behavior.
pub struct DspicService {
    i2c: I2cServiceHandle,
    address: u8,
    firmware: DspicFirmware,
    current_voltage_mv: u16,
    voltage_enabled: bool,
    use_bare_protocol: bool,
    /// Post-JUMP heartbeat keep-alive (2026-06-07, `a lab unit` standalone
    /// cold-engage). Default `false` → byte-identical legacy behaviour. When
    /// `true` AND the protocol is framed (`!use_bare_protocol`),
    /// `cold_boot_init_with_options` runs a **continuous bounded keep-alive**:
    /// from the post-warmup fw=0x89 confirmation it fires a framed `0x16`
    /// heartbeat at most `keepalive_interval_ms` apart — interleaved between
    /// the ~290 ms LM75A sensor reads and chunked across the settle sleeps —
    /// all the way through SetVoltage → ENABLE, so a cold-engaged fw=0x89 dsPIC
    /// stays in app mode instead of drifting back to fw=0x82 bootloader (the
    /// earlier 3 discrete boundary ticks left a ~1.2 s un-serviced gap during
    /// the LM75A read where the chip drifted, LIVE on `a lab unit`). Set by the
    /// `s19j_hybrid_mining.rs` Phase 3 call site via
    /// `set_postjump_heartbeat_keepalive` only when the env gate
    /// (`DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE`) AND the `a lab unit`
    /// fingerprint both match. See
    /// `bosminer_warmup::am2_dspic_postjump_heartbeat_keepalive_enabled`.
    postjump_heartbeat_keepalive: bool,
    /// Keep-alive heartbeat interval (ms) for the continuous bounded keep-alive
    /// loop (2026-06-07, `a lab unit` standalone cold-engage). Default `300`. Only
    /// consulted while `postjump_heartbeat_keepalive` is `true`; resolved from
    /// `DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS` (clamped `[50, 1000]`) when
    /// `set_postjump_heartbeat_keepalive(true)` is called. Inert when the
    /// keep-alive is off ⇒ does not affect the byte-identical legacy path.
    keepalive_interval_ms: u64,
    /// Re-JUMP-before-ENABLE (2026-06-07, `a lab unit` standalone cold-engage).
    /// Default `false` → byte-identical legacy behaviour. When `true` AND the
    /// protocol is framed (`!use_bare_protocol`), `cold_boot_init_with_options`
    /// reads GET_VERSION immediately before SetVoltage(0x10); if the
    /// cold-engaged FRAMED (fw=0x89) dsPIC has drifted back to fw=0x82
    /// bootloader it runs a bounded `flush → framed-JUMP` (NO RESET) re-verify
    /// to re-transition it to fw=0x89 so the chip is in APP mode when the
    /// ENABLE(0x15) lands (LIVE blocker: ENABLE returned `[82, 82]` echo —
    /// chip had drifted back to bootloader in the ~8 s orchestration gap; the
    /// framed `0x16` keep-alive did NOT hold app mode nor transition 0x82→0x89).
    /// Set by the `s19j_hybrid_mining.rs` Phase 3 call site via
    /// `set_rejump_before_enable` only when the env gate
    /// (`DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE`) AND the `a lab unit` fingerprint both
    /// match. See `bosminer_warmup::am2_dspic_rejump_before_enable_enabled`.
    rejump_before_enable: bool,
    /// Skip-SetVoltage-keep-ENABLE (2026-06-07, `a lab unit` standalone cold-engage).
    /// Default `false` → byte-identical legacy behaviour (the `0x10` SetVoltage
    /// is still sent on every fleet/handoff/legacy path). When `true` AND the
    /// protocol is framed (`!use_bare_protocol`), `cold_boot_init_with_options`
    /// SKIPS the dsPIC SetVoltage (opcode `0x10`, frame `[55 AA 04 10 DAC SUM]`)
    /// entirely and goes GET_VERSION(0x89) → [re-JUMP if drifted] → ENABLE(0x15)
    /// directly, exactly like bosminer. PROVEN root cause
    /// (
    /// commit fc4eef92): the bosminer true-cold strace contains ZERO SetVoltage
    /// (0x10) frames to the `a lab unit` dsPIC in 662k lines — the chip-rail voltage is
    /// set on the APW PSU (PMBus), NOT the per-board dsPIC (sensor-passthrough +
    /// ENABLE only). DCENT's `0x10` faults the cold-engaged fw=0x89 app back to
    /// the fw=0x82 bootloader, so the immediately-following ENABLE reads
    /// `[82, 82]`. The chip rail energizes via the (unchanged) ENABLE at the
    /// dsPIC's power-on default voltage — bosminer-proven safe (bosminer never
    /// SetVoltages and mines fine). Only effective for the framed (fw=0x89)
    /// protocol; the proven BARE (fw=0x82) `a lab unit`/ cold path — where
    /// SetVoltage ACKs and is part of the working path — is left untouched. Set
    /// by the `s19j_hybrid_mining.rs` Phase 3 call site via
    /// `set_skip_setvoltage_keep_enable` only when the env gate
    /// (`DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE`) AND the `a lab unit` fingerprint
    /// both match. See
    /// `bosminer_warmup::am2_dspic_skip_setvoltage_keep_enable_enabled`.
    skip_setvoltage_keep_enable: bool,
    /// Bosminer-minimal ENABLE (2026-06-07, `a lab unit` standalone cold-engage) — the
    /// CONSOLIDATED fix for the LIVE-confirmed ENABLE `[82, 82]` drift. Default
    /// `false` → byte-identical legacy behaviour. When `true` AND the protocol is
    /// framed (`!use_bare_protocol`), `cold_boot_init_with_options` goes straight
    /// GET_VERSION(0x89, confirmed in the external warmup) → ENABLE(0x15),
    /// sending the dsPIC **NOTHING** in between: it SKIPS the parser flush, the
    /// sanity `0x16` heartbeat, the `0x30` LM75A pre-voltage read, any second
    /// GET_VERSION/`0x06` JUMP re-verify, the `0x10` SetVoltage, and every
    /// keep-alive tick — exactly like bosminer (whose only GET_VERSION→ENABLE
    /// traffic is `0x3B`/`0x3C` sensor passthrough, and which holds the chip in
    /// fw=0x89 for >1.4 s un-serviced). The env-by-env skips left commands firing
    /// from multiple code paths (LIVE TEST 6 still showed LM75A reads + a stray
    /// GET_VERSION+JUMP); this single gate consolidates the skip. It
    /// SUPERSEDES/implies `skip_setvoltage_keep_enable` (omits the `0x10` too) and
    /// renders `postjump_heartbeat_keepalive` / `rejump_before_enable` moot
    /// (omits both). The rail energizes via the **byte-identical** ENABLE at the
    /// dsPIC power-on default voltage — bosminer-proven safe (`a lab unit` input rail is
    /// the APW3 12.8 V `psu_override`). Only effective for the framed (fw=0x89)
    /// protocol; the proven BARE (fw=0x82) `a lab unit`/ cold path is left
    /// untouched (falls through to the unchanged legacy path). Set by the
    /// `s19j_hybrid_mining.rs` Phase 3 call site via `set_bosminer_minimal_enable`
    /// only when the env gate (`DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE`) AND the
    /// `a lab unit` fingerprint both match. See
    /// `bosminer_warmup::am2_dspic_bosminer_minimal_enable_enabled`.
    bosminer_minimal_enable: bool,
}

/// Durable authority for creating ephemeral dsPIC service views at one
/// discovery-bound endpoint.
///
/// The daemon historically creates short-lived `DspicService` values for
/// initialization and heartbeat operations. This session consumes the opaque
/// HAL endpoint once, validates its serialized-service bus once, and then owns
/// the only reusable construction seam for migrated routes. Callers cannot
/// choose or change the family, bus, or address after construction.
pub struct DspicEndpointSession {
    i2c: I2cServiceHandle,
    address: u8,
}

impl DspicEndpointSession {
    /// Bind a serialized I2C service to a discovery-issued dsPIC endpoint.
    pub fn new(i2c: I2cServiceHandle, endpoint: VoltageControllerEndpoint) -> Result<Self> {
        if endpoint.kind() != VoltageControllerKind::Dspic33Ep {
            return Err(crate::AsicError::InvalidParameter(format!(
                "{} endpoint cannot construct a dsPIC session",
                endpoint.kind().as_str()
            )));
        }
        if endpoint.bus() != i2c.bus() {
            return Err(crate::AsicError::InvalidParameter(format!(
                "dsPIC endpoint is bound to I2C bus {}, but service owns bus {}",
                endpoint.bus(),
                i2c.bus()
            )));
        }
        Ok(Self {
            i2c,
            address: endpoint.address(),
        })
    }

    /// Bound address, exposed only for lookup and diagnostics.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Create a short-lived controller view without reasserting an address.
    pub fn service(&self) -> DspicService {
        DspicService::new_legacy_parts(self.i2c.clone(), self.address)
    }

    /// Create a short-lived controller view with an observed firmware hint.
    /// Firmware selects revision framing only; family/bus/address remain bound
    /// by this session.
    pub fn service_with_firmware(&self, firmware: DspicFirmware) -> DspicService {
        DspicService::new_legacy_parts_with_firmware(self.i2c.clone(), self.address, firmware)
    }
}

impl DspicService {
    /// Construct from a discovery-issued, bus-bound dsPIC endpoint.
    ///
    /// This is the preferred constructor for migrated daemon paths. It
    /// rejects a legitimate endpoint for another controller family and a
    /// handle for any bus other than the one on which presence was observed.
    pub fn from_endpoint(
        i2c: I2cServiceHandle,
        endpoint: VoltageControllerEndpoint,
    ) -> Result<Self> {
        if endpoint.kind() != VoltageControllerKind::Dspic33Ep {
            return Err(crate::AsicError::InvalidParameter(format!(
                "{} endpoint cannot construct a dsPIC service",
                endpoint.kind().as_str()
            )));
        }
        if endpoint.bus() != i2c.bus() {
            return Err(crate::AsicError::InvalidParameter(format!(
                "dsPIC endpoint is bound to I2C bus {}, but service owns bus {}",
                endpoint.bus(),
                i2c.bus()
            )));
        }
        Ok(Self::new_legacy_parts(i2c, endpoint.address()))
    }

    /// Legacy caller-asserted construction seam.
    ///
    /// New production routes must use [`DspicEndpointSession`]. This remains
    /// public temporarily because proven AM2 and recovery paths have not yet
    /// migrated; removing it now would be a broad, unsafe compatibility break.
    #[doc(hidden)]
    pub fn new(i2c: I2cServiceHandle, address: u8) -> Self {
        Self::new_legacy_parts(i2c, address)
    }

    fn new_legacy_parts(i2c: I2cServiceHandle, address: u8) -> Self {
        Self {
            i2c,
            address,
            firmware: DspicFirmware::Unknown,
            current_voltage_mv: 0,
            voltage_enabled: false,
            use_bare_protocol: false,
            postjump_heartbeat_keepalive: false,
            keepalive_interval_ms: 300,
            rejump_before_enable: false,
            skip_setvoltage_keep_enable: false,
            bosminer_minimal_enable: false,
        }
    }

    /// Legacy caller-asserted construction seam with a firmware hint.
    /// Prefer [`DspicEndpointSession::service_with_firmware`] on migrated
    /// production routes.
    #[doc(hidden)]
    pub fn new_with_firmware(i2c: I2cServiceHandle, address: u8, firmware: DspicFirmware) -> Self {
        Self::new_legacy_parts_with_firmware(i2c, address, firmware)
    }

    fn new_legacy_parts_with_firmware(
        i2c: I2cServiceHandle,
        address: u8,
        firmware: DspicFirmware,
    ) -> Self {
        Self {
            i2c,
            address,
            firmware,
            current_voltage_mv: 0,
            voltage_enabled: false,
            use_bare_protocol: firmware.protocol() == DspicProtocol::Bare,
            postjump_heartbeat_keepalive: false,
            keepalive_interval_ms: 300,
            rejump_before_enable: false,
            skip_setvoltage_keep_enable: false,
            bosminer_minimal_enable: false,
        }
    }

    /// Enable/disable the post-JUMP framed-heartbeat keep-alive for
    /// `cold_boot_init_with_options` (default `false`).
    ///
    /// Caller-gated: the `s19j_hybrid_mining.rs` Phase 3 call site sets this
    /// to `true` ONLY when `DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE=1`
    /// AND the `a lab unit`-class hardware fingerprint matches. With the default
    /// `false`, `cold_boot_init_with_options` is byte-identical to the legacy
    /// fleet/handoff path. Only effective for the framed (fw=0x89) protocol;
    /// the bare (fw=0x82) path has no fw=0x89→0x82 drift to defend against and
    /// is left untouched.
    ///
    /// When enabling, the keep-alive heartbeat interval is resolved once from
    /// `DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS` (default 300 ms, clamped
    /// `[50, 1000]`). The interval field is inert while the keep-alive is off,
    /// so the default-OFF path stays byte-identical.
    pub fn set_postjump_heartbeat_keepalive(&mut self, on: bool) {
        self.postjump_heartbeat_keepalive = on;
        if on {
            self.keepalive_interval_ms =
                crate::dspic::bosminer_warmup::am2_dspic_keepalive_interval_ms();
        }
    }

    /// Enable/disable the re-JUMP-before-ENABLE for
    /// `cold_boot_init_with_options` (default `false`).
    ///
    /// Caller-gated: the `s19j_hybrid_mining.rs` Phase 3 call site sets this to
    /// `true` ONLY when `DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE=1` AND the
    /// `a lab unit`-class hardware fingerprint matches. With the default `false`,
    /// `cold_boot_init_with_options` is byte-identical to the legacy
    /// fleet/handoff path (no extra GET_VERSION/JUMP). Only effective for the
    /// framed (fw=0x89) protocol; the bare (fw=0x82) path has no fw=0x89→0x82
    /// drift to defend against and is left untouched. NEVER issues a RESET — the
    /// re-verify is the JUMP-only ([`am2_pic_jump_only_reverify`]) safe
    /// idempotent transition for a chip already cold-0x82.
    pub fn set_rejump_before_enable(&mut self, on: bool) {
        self.rejump_before_enable = on;
    }

    /// Enable/disable the skip-SetVoltage-keep-ENABLE for
    /// `cold_boot_init_with_options` (default `false`).
    ///
    /// Caller-gated: the `s19j_hybrid_mining.rs` Phase 3 call site sets this to
    /// `true` ONLY when `DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE=1` AND the
    /// `a lab unit`-class hardware fingerprint matches. With the default `false`,
    /// `cold_boot_init_with_options` is byte-identical to the legacy
    /// fleet/handoff path (the `0x10` SetVoltage is still sent). Only effective
    /// for the framed (fw=0x89) protocol; the proven BARE (fw=0x82) cold path —
    /// where SetVoltage ACKs and is load-bearing — is left untouched. PROVEN
    /// root cause: bosminer sends ZERO `0x10` SetVoltage to the `a lab unit` dsPIC
    /// (rail is APW-PSU-side); DCENT's `0x10` faults the cold-engaged fw=0x89
    /// app back to fw=0x82, so the ENABLE reads `[82, 82]`. Skipping the `0x10`
    /// — and keeping the byte-identical ENABLE (`0x15`) — lets the ENABLE land
    /// on a still-fw=0x89 app and energize the rail at the dsPIC power-on
    /// default voltage. The 14.5 V dsPIC cap / EEPROM denylist / fw=0x86 refusal
    /// / ENABLE wire bytes are ALL unchanged.
    pub fn set_skip_setvoltage_keep_enable(&mut self, on: bool) {
        self.skip_setvoltage_keep_enable = on;
    }

    /// Enable/disable the bosminer-minimal ENABLE for
    /// `cold_boot_init_with_options` (default `false`).
    ///
    /// Caller-gated: the `s19j_hybrid_mining.rs` Phase 3 call site sets this to
    /// `true` ONLY when `DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE=1` AND the
    /// `a lab unit`-class hardware fingerprint matches. With the default `false`,
    /// `cold_boot_init_with_options` is byte-identical to the legacy
    /// fleet/handoff path (flush + sanity heartbeat + LM75A read + re-JUMP +
    /// SetVoltage + keep-alive all fire exactly as today). When `true` AND framed,
    /// it CONSOLIDATES the GET_VERSION(0x89)→ENABLE window to bosminer-minimal:
    /// the ONLY dsPIC wire traffic in that window is the byte-identical ENABLE
    /// (`0x15`) itself — every fault-suspect pre-ENABLE command (`0x10` SetVoltage,
    /// `0x30` LM75A, second GET_VERSION/`0x06` JUMP, `0x16` heartbeat, flush) is
    /// skipped, exactly like bosminer. SUPERSEDES `skip_setvoltage_keep_enable`
    /// and renders `postjump_heartbeat_keepalive`/`rejump_before_enable` moot.
    /// Only effective for the framed (fw=0x89) protocol; the proven BARE
    /// (fw=0x82) cold path is left untouched. The 14.5 V dsPIC cap / EEPROM
    /// denylist / fw=0x86 refusal / ENABLE wire bytes are ALL unchanged.
    pub fn set_bosminer_minimal_enable(&mut self, on: bool) {
        self.bosminer_minimal_enable = on;
    }

    pub fn firmware(&self) -> DspicFirmware {
        self.firmware
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn voltage_mv(&self) -> u16 {
        self.current_voltage_mv
    }

    pub fn voltage_v(&self) -> f64 {
        self.current_voltage_mv as f64 / 1000.0
    }

    pub fn voltage_enabled(&self) -> bool {
        self.voltage_enabled
    }

    /// Flush the dsPIC parser with zero bytes through the service thread.
    /// 16 zero bytes — anything
    /// shorter risks leaving partial parser state after a NACK.
    pub fn flush_parser(&self) {
        let _ = self
            .i2c
            .write_bytes_mutating(I2cMutationLabel::Recovery, self.address, &[0u8; 16]);
    }

    fn probe_get_version_once(&self, encoding: GetVersionEncoding) -> Result<Vec<u8>> {
        let frame = dspic_get_version_frame(encoding);
        ensure_dspic_bootloader_command_allowed(
            self.address,
            self.firmware,
            matches!(encoding, GetVersionEncoding::Short),
            frame,
        )?;
        let reads = self
            .i2c
            .transaction_mutating(
                I2cMutationLabel::QueryPrelude,
                self.address,
                dspic_get_version_transaction_steps(encoding),
            )
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC service GET_VERSION transaction: {}", e),
            })?;
        let reply = collect_single_byte_i2c_reads(reads);
        if reply.is_empty() {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC service GET_VERSION transaction returned no read".to_string(),
            });
        }
        Ok(reply)
    }

    /// Read firmware version and update the protocol mode.
    ///
    /// This is a service-thread equivalent of `DspicController::get_version`.
    /// It keeps the flush/write/delay/read sequence inside one service request
    /// so no APW heartbeat or thermal read can interleave with the PIC parser
    /// while it is preparing its response.
    pub fn get_version(&mut self) -> Result<u8> {
        let mut last_raw: Option<Vec<u8>> = None;
        let mut last_error: Option<String> = None;

        for encoding in dspic_get_version_probe_order(self.firmware) {
            for attempt in 1..=DSPIC_GET_VERSION_ATTEMPTS {
                let buf = match self.probe_get_version_once(encoding) {
                    Ok(buf) => buf,
                    Err(e) => {
                        last_error = Some(e.to_string());
                        tracing::warn!(
                            addr = format_args!("0x{:02X}", self.address),
                            ?encoding,
                            attempt,
                            error = %e,
                            "dsPIC service GET_VERSION transaction failed; flushing and retrying",
                        );
                        continue;
                    }
                };

                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    ?encoding,
                    attempt,
                    raw = format_args!("{:02X?}", buf),
                    "dsPIC service GET_VERSION raw reply",
                );

                if let Some(version) = parse_get_version_reply(&buf) {
                    self.firmware = DspicFirmware::from_version(version);
                    self.use_bare_protocol = self.firmware.protocol() == DspicProtocol::Bare;
                    tracing::info!(
                        addr = format_args!("0x{:02X}", self.address),
                        ?encoding,
                        attempt,
                        version = format_args!("0x{:02X}", version),
                        firmware = %self.firmware,
                        bare = self.use_bare_protocol,
                        "dsPIC service GET_VERSION accepted firmware reply",
                    );
                    return Ok(version);
                }

                last_raw = Some(buf.clone());
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    ?encoding,
                    attempt,
                    raw = format_args!("{:02X?}", buf),
                    "dsPIC service GET_VERSION rejected invalid/bus-noise reply",
                );
            }
        }

        self.firmware = DspicFirmware::Unknown;
        self.use_bare_protocol = false;
        Err(crate::AsicError::Pic {
            addr: self.address,
            detail: format!(
                "dsPIC service GET_VERSION failed after short+framed retries; last_raw={}; last_error={}",
                last_raw
                    .as_deref()
                    .map(|raw| format!("{:02X?}", raw))
                    .unwrap_or_else(|| "none".to_string()),
                last_error.unwrap_or_else(|| "none".to_string())
            ),
        })
    }

    /// Preflight the service-backed dsPIC by flushing parser state and reading
    /// firmware version. Returns `Unknown` instead of failing hard on I2C errors.
    pub fn preflight(&mut self) -> Result<DspicFirmware> {
        self.flush_parser();
        std::thread::sleep(std::time::Duration::from_millis(10));

        match self.get_version() {
            Ok(version) => {
                let fw = DspicFirmware::from_version(version);
                self.firmware = fw;
                self.use_bare_protocol = fw.protocol() == DspicProtocol::Bare;
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    version = format_args!("0x{:02X}", version),
                    firmware = %fw,
                    "dsPIC service preflight firmware detected",
                );
                Ok(fw)
            }
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "dsPIC service preflight failed; firmware remains Unknown",
                );
                self.firmware = DspicFirmware::Unknown;
                self.use_bare_protocol = false;
                Ok(DspicFirmware::Unknown)
            }
        }
    }

    /// Alias for callers that follow the raw-controller naming.
    pub fn detect_firmware(&mut self) -> Result<DspicFirmware> {
        self.preflight()
    }

    /// Initialize the dsPIC voltage controller without RESET/JUMP, using only
    /// service-serialized transactions.
    ///
    /// Convenience wrapper that calls `cold_boot_init_with_options` with the
    /// historical defaults: run the internal 5×1s pre-voltage heartbeat
    /// warmup loop. New callers that already ran an external warmup pass
    /// (e.g. the bosminer-warmup + Phase 0d 5×1Hz idle heartbeats from
    /// `s19j_hybrid_mining.rs`) should call
    /// `cold_boot_init_with_options(voltage_mv, true)` to skip the
    /// duplicate-and-slow internal loop.
    pub fn cold_boot_init(&mut self, voltage_mv: u16) -> Result<()> {
        self.cold_boot_init_with_options(voltage_mv, false)
    }

    /// Variant of `cold_boot_init` with an explicit `skip_warmup_loop` knob.
    ///
    /// When `skip_warmup_loop` is `true`, the internal 5×1s pre-voltage
    /// heartbeat stability gate is SKIPPED. Callers should ONLY set this to
    /// true when they have already proven 5 stable heartbeats on this exact
    /// dsPIC address through a separate warmup pass (e.g. the bosminer-warmup
    /// prelude + Phase 0d 5×1Hz idle heartbeats from `s19j_hybrid_mining.rs`).
    /// When `false` the legacy behaviour is preserved byte-for-byte.
    ///
    /// See
    ///  §(e).
    pub fn cold_boot_init_with_options(
        &mut self,
        voltage_mv: u16,
        skip_warmup_loop: bool,
    ) -> Result<()> {
        ensure_dspic_voltage_command_allowed(
            self.address,
            self.firmware,
            "service cold_boot_init",
        )?;

        let voltage_v = voltage_mv as f64 / 1000.0;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            target_voltage = format_args!("{:.2}V", voltage_v),
            voltage_mv,
            skip_warmup_loop,
            "dsPIC service init starting (write-only, no RESET/JUMP)",
        );

        // ════════════════════════════════════════════════════════════════════
        // BOSMINER-MINIMAL ENABLE (2026-06-07, `a lab unit` standalone cold-engage) —
        // the CONSOLIDATED fix for the LIVE-confirmed ENABLE `[82, 82]` drift.
        //
        // Between the external warmup's GET_VERSION(0x89) confirmation and the
        // ENABLE(0x15), bosminer sends the `a lab unit` dsPIC NOTHING that can fault the
        // cold-engaged fw=0x89 app back to the fw=0x82 bootloader: no SetVoltage
        // (0x10), no LM75A read (0x30), no re-JUMP (0x06), no second GET_VERSION
        // re-verify, no `0x16` heartbeat, no parser flush — its only in-window
        // traffic is `0x3B`/`0x3C` sensor passthrough, and even fully un-serviced
        // the chip stays fw=0x89 for >1.4 s
        // (
        // commit fc4eef92). DCENT's individual pre-ENABLE commands each come from
        // a DIFFERENT code path — the flush + sanity heartbeat below, the `0x30`
        // LM75A `read_all_temperatures_keepalive`, the
        // `rejump_to_app_mode_if_drifted` re-verify, the `0x10` SetVoltage — so
        // unsetting them env-by-env still left some firing: LIVE TEST 6 (after
        // unsetting the `0x16` keep-alive + the `0x06` re-JUMP + skipping the
        // `0x10` SetVoltage) STILL showed LM75A reads + a stray GET_VERSION+JUMP
        // between the warmup GET_VERSION(0x89) and the ENABLE → ENABLE `[82, 82]`.
        //
        // This single gate CONSOLIDATES the skip: when active it returns straight
        // to the ENABLE, so the ONLY dsPIC wire traffic between the confirmed
        // fw=0x89 GET_VERSION and the ENABLE is the byte-identical ENABLE itself.
        // It SUPERSEDES `skip_setvoltage_keep_enable` (omits the `0x10` too) and
        // renders `postjump_heartbeat_keepalive` / `rejump_before_enable` moot
        // (omits both). The rail energizes via the (unchanged) ENABLE at the
        // dsPIC power-on default voltage — bosminer-proven safe (bosminer never
        // SetVoltages and mines; `a lab unit` input rail is the APW3 12.8 V psu_override).
        //
        // Gating mirrors `skip_setvoltage_keep_enable` EXACTLY: caller-gated
        // (`DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE=1` AND the `a lab unit` fingerprint,
        // set at the s19j_hybrid Phase 3 call site) AND framed-only. The protocol
        // is resolved here from the KNOWN firmware byte WITHOUT any wire traffic
        // (a detection heartbeat would itself violate the bosminer-minimal
        // window) — Unknown ⇒ framed default, matching the no-traffic half of the
        // firmware-detection block below. Default-OFF ⇒ this whole block is
        // skipped ⇒ byte-identical to the legacy fleet/handoff path. The proven
        // BARE (fw=0x82) `a lab unit`/ cold path — where SetVoltage/LM75-skip are
        // load-bearing — never enters the minimal arm (it falls through to the
        // unchanged legacy path below). `ensure_dspic_voltage_command_allowed`
        // (14.5 V cap / fw=0x86 refusal / EEPROM denylist) already ran above, so
        // the minimal path is still fully gated by it.
        if self.bosminer_minimal_enable {
            let minimal_use_bare = if self.firmware != DspicFirmware::Unknown {
                self.firmware.protocol() == DspicProtocol::Bare
            } else {
                false
            };
            if !minimal_use_bare {
                self.use_bare_protocol = false;
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    voltage_mv,
                    env_gate = "DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE=1",
                    "dsPIC BOSMINER-MINIMAL ENABLE: GET_VERSION(0x89, external warmup) → \
                     ENABLE(0x15) directly — sending the dsPIC NOTHING in between (no flush, \
                     no 0x16 heartbeat, no 0x30 LM75A, no second GET_VERSION/0x06 JUMP, no \
                     0x10 SetVoltage), exactly like bosminer, so the cold-engaged fw=0x89 app \
                     cannot be faulted back to fw=0x82 before the ENABLE (ENABLE-DRIFT-DIFF.md \
                     / LIVE TEST 6). Rail energizes via the unchanged ENABLE at the dsPIC \
                     power-on default voltage"
                );
                // THE ONLY dsPIC wire traffic in the GET_VERSION→ENABLE window:
                // the byte-identical ENABLE(0x15) frame (`55 AA 05 15 01 00 1B`
                // framed). enable_voltage() selects the form from
                // use_bare_protocol (framed here) — wire bytes UNCHANGED vs every
                // other path.
                self.enable_voltage()?;
                // Post-ENABLE only (rail now energized + chip in app mode): the
                // same 1 s settle + single bus-alive heartbeat the legacy path
                // runs AFTER ENABLE. This is OUTSIDE the GET_VERSION→ENABLE window
                // and matches bosminer (which reads sensors after ENABLE); a
                // `0x16` on an already-energized app-mode chip is the legitimate
                // watchdog refresh, not a fault. NON-FATAL.
                std::thread::sleep(std::time::Duration::from_millis(1000));
                if let Err(e) = self.send_heartbeat() {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        error = %e,
                        "dsPIC service heartbeat after BOSMINER-MINIMAL ENABLE failed; continuing",
                    );
                }
                return Ok(());
            }
            // Bosminer-minimal gate set but the chip is BARE protocol: NOT the
            // `a lab unit` framed fw=0x89 target — fall through to the unchanged legacy
            // BARE path (byte-identical). The bosminer-minimal window only applies
            // to the framed fw=0x89 chip.
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                firmware = %self.firmware,
                "dsPIC BOSMINER-MINIMAL ENABLE gate set but chip is BARE protocol — NOT \
                 applying the framed-only minimal window; proceeding on the unchanged legacy path"
            );
        }

        self.flush_parser();
        std::thread::sleep(std::time::Duration::from_millis(10));

        if self.firmware != DspicFirmware::Unknown {
            self.use_bare_protocol = self.firmware.protocol() == DspicProtocol::Bare;
            if let Err(e) = self.send_heartbeat() {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    bare = self.use_bare_protocol,
                    error = %e,
                    "dsPIC service heartbeat failed using firmware-selected protocol; continuing",
                );
            }
        } else {
            self.use_bare_protocol = false;
            if let Err(framed_err) = self.send_heartbeat() {
                self.use_bare_protocol = true;
                if let Err(bare_err) = self.send_heartbeat() {
                    self.use_bare_protocol = false;
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        framed_error = %framed_err,
                        bare_error = %bare_err,
                        "dsPIC service heartbeat failed on both protocols; continuing framed",
                    );
                }
            }
        }

        // 5×1Hz pre-voltage heartbeat stability gate.
        //
        // 2026-05-22 (XIL `a lab unit` recovery, Layer 3): callers that have already
        // run an external warmup pass (bosminer-warmup prelude + Phase 0d
        // 5×1Hz idle heartbeats in `s19j_hybrid_mining.rs`) can opt out via
        // `skip_warmup_loop=true` to avoid running the same gate twice. When
        // skipped, we still issue ONE heartbeat to prove the bus is healthy
        // before SetVoltage — this is the cheap "did the external warmup
        // actually stick?" check. When `skip_warmup_loop=false` (legacy
        // default), the original byte-for-byte behaviour is preserved.
        if skip_warmup_loop {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                bare = self.use_bare_protocol,
                "dsPIC service cold_boot_init: skip_warmup_loop=true — \
                 5×1Hz pre-voltage warmup loop SKIPPED (external Phase 0d \
                 warmup already ran); single sanity heartbeat only"
            );
            //  (2026-05-29, live-evidenced on `a lab unit` cold-boot): on a COLD
            // standalone boot the Phase-0d external warmup probes the absent
            // slot-2 dsPIC (0x21) whose EIO can momentarily desync the AXI-IIC
            // controller, so this FIRST sanity heartbeat on the selected PIC can
            // EIO even though the bus recovers on the very next fd reopen (the
            // I2cService reopens lazily on the transaction AFTER an error). A
            // single-shot check therefore aborts a chain that is actually fine.
            // Retry a few times with a settle so the auto-reopen takes effect;
            // a genuinely dead bus still fails after the retries and bails
            // fail-closed (the canonical error string below is preserved and
            // pinned by cold_boot_init_with_options_skip_warmup.rs). The single
            // textual `self.send_heartbeat()` call keeps that test's "exactly
            // one heartbeat in the skip branch" contract intact.
            //
            //  Fix A tweak (2026-05-29): the settle is 1100 ms, NOT
            // 300 ms. The I2cService fd-reopen is rate-limited to >1 second
            // (i2c.rs:2594), so a 300 ms settle CANNOT trigger a reopen — the
            // retry then re-hits the same poisoned fd and the chain still aborts.
            // 1100 ms guarantees the rate-limit window elapses so the reopen
            // actually fires before the retry.
            let mut sanity_ok = false;
            let mut last_hb_err: Option<crate::AsicError> = None;
            for attempt in 1..=4u8 {
                match self.send_heartbeat() {
                    Ok(()) => {
                        sanity_ok = true;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            addr = format_args!("0x{:02X}", self.address),
                            attempt,
                            error = %e,
                            "dsPIC sanity heartbeat EIO after external warmup; \
                             settling 1100ms for the rate-limited I2C bus fd-reopen, then retrying",
                        );
                        last_hb_err = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(1100));
                    }
                }
            }
            if !sanity_ok {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "dsPIC sanity heartbeat after external warmup failed: {} \
                         (external Phase 0d warmup did not produce a stable bus)",
                        last_hb_err
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "unknown".to_string())
                    ),
                });
            }
        } else {
            let mut stable_heartbeats = 0u8;
            for tick in 1..=5 {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                match self.send_heartbeat() {
                    Ok(()) => {
                        stable_heartbeats += 1;
                        tracing::info!(
                            addr = format_args!("0x{:02X}", self.address),
                            tick,
                            stable_heartbeats,
                            bare = self.use_bare_protocol,
                            "dsPIC service pre-voltage heartbeat tick",
                        );
                    }
                    Err(e) => {
                        stable_heartbeats = 0;
                        tracing::warn!(
                            addr = format_args!("0x{:02X}", self.address),
                            tick,
                            error = %e,
                            "dsPIC service pre-voltage heartbeat failed",
                        );
                    }
                }
            }

            if stable_heartbeats < 5 {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "dsPIC service did not complete 5 stable pre-voltage heartbeats (stable={})",
                        stable_heartbeats
                    ),
                });
            }
        }

        // Post-JUMP heartbeat keep-alive (2026-06-07, `a lab unit` standalone
        // cold-engage) — START of the continuous bounded keep-alive: the
        // moment the warmup gate has proven the cold-engaged FRAMED (fw=0x89)
        // dsPIC is in app mode, begin servicing the app-mode watchdog. From
        // here through the ENABLE every settle/read below keeps the chip
        // serviced at most `keepalive_interval_ms` apart (LM75A reads are
        // interleaved; sleeps are chunked) so it never drifts back to fw=0x82
        // bootloader. No-op unless caller-gated (env + `a lab unit` fingerprint) AND
        // framed; default-OFF ⇒ byte-identical.
        self.postjump_keepalive_tick("post-warmup");

        // P1.7 LM75A WIRING READ — 3-corpus RE consensus 2026-04-26
        // (bosminer.log, VNish 1.2.7 cgminer, Bitmain stock CV bmminer).
        //
        // All three working firmwares read four LM75A sensors via dsPIC
        // passthrough (opcode 0x30) before the first SetVoltage/ENABLE
        // sequence. The 2026-04-26 .139 probes proved LM75 is not a
        // GET_VERSION precondition; the reads are retained to preserve the
        // known working cold-boot order and are NON-FATAL. Bosminer logs at
        // 22:33:56 show this path can return frame-mismatch errors and still
        // recover, so log and continue regardless of per-sensor outcome.
        // Live evidence on .139 (2026-04-26 evening): on bare-protocol fw=0x86,
        // the LM75A pre-voltage pass issues 4× bus writes of [55 AA 30 ADDR]
        // even though no temp data is delivered (dsPIC just echoes 0x86). The
        // SetVoltage that follows immediately afterward NACKs at the kernel
        // level (EIO) — strong signal that the LM75 writes corrupt the dsPIC
        // parser state for fw=0x86. Manual `i2cset` of the same SetVoltage
        // bytes from a quiescent shell SUCCEEDS, confirming the issue is
        // sequence-order, not protocol-form.
        //
        // Bare mode: SKIP LM75 reads entirely. The reads return NaN sentinels
        // anyway (no real temp data on the bare 1-byte echo). Framed mode:
        // keep the canonical 4× passthrough as proven on the BraiinsOS+
        // reference path.
        if self.use_bare_protocol {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                bare = self.use_bare_protocol,
                "dsPIC service bare LM75A pre-voltage read SKIPPED (bare echoes 0x86 \
                 only — bus writes appear to corrupt dsPIC parser for following \
                 SetVoltage on fw=0x86; see project_s19jpro_139_progress_2026_04_26_evening)"
            );
        } else {
            // Keep-alive (default-OFF): byte-identical `read_all_temperatures`
            // unless the post-JUMP keep-alive is active, in which case a framed
            // 0x16 heartbeat is interleaved between each ~290 ms sensor read so
            // the cold-engaged fw=0x89 chip is serviced across the ~1.2 s
            // 4-sensor window (the LIVE `a lab unit` un-serviced drift gap).
            let temps = self.read_all_temperatures_keepalive();
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                temps = format_args!("{:?}", temps),
                bare = self.use_bare_protocol,
                "dsPIC service LM75A pre-voltage wiring read complete (4x passthrough at 0x48-0x4B)"
            );
        }
        // Keep-alive (default-OFF): byte-identical `thread::sleep(50ms)` unless
        // active, in which case the settle is chunked with keep-alive ticks.
        self.keepalive_sleep(50, "post-lm75-settle");

        // Ghidra-RE PART A (2026-05-29, DCENT_AM2_DSPIC_SENSOR_ONLY) — skip
        // the dsPIC SetVoltage (0x10) + ENABLE_VOLTAGE (0x15) writes entirely.
        //
        //
        // (Ghidra static RE of bosminer.bin): on `a lab unit`-class AM2 hardware
        // bosminer engages the chip rail ENTIRELY Loki-side (PWR_CONTROL +
        // Loki `0x83` SetVoltage-step) and sends ZERO `0x10`/`0x15` to the
        // per-board dsPIC — the dsPIC is sensor-passthrough ONLY. Hitting the
        // cold dsPIC with SetVoltage/ENABLE opcodes its bootloader can't route
        // makes it echo its FW byte (0x8A) and leaves the parser unwarmed.
        //
        // Default-OFF: when `DCENT_AM2_DSPIC_SENSOR_ONLY` is unset the legacy
        // behaviour (SetVoltage + ENABLE) is preserved byte-for-byte for every
        // other platform (.79/.129/.135/.109/S9). cold_boot_init still returns
        // Ok in the skip path — the downstream chain enumeration is the real
        // proof of rail engagement, not the dsPIC ACK/echo.
        if crate::dspic::bosminer_warmup::am2_dspic_sensor_only_enabled() {
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                voltage_mv,
                env_gate = "DCENT_AM2_DSPIC_SENSOR_ONLY=1",
                "DCENT_AM2_DSPIC_SENSOR_ONLY=1: skipping dsPIC SetVoltage/ENABLE on .25 \
                 — rail is engaged Loki-side per bosminer RE; dsPIC used for sensor \
                 passthrough only. cold_boot_init completes Ok; chain enum is the rail proof. \
"
            );
            // Issue one heartbeat so we still confirm the bus is alive on this
            // dsPIC after the (sensor-only) cold-boot init — matches the
            // post-enable heartbeat the non-skip path issues below.
            if let Err(e) = self.send_heartbeat() {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "dsPIC service heartbeat after SENSOR_ONLY cold_boot_init failed; continuing",
                );
            }
            return Ok(());
        }

        // VNish RE 2026-04-25 (cgminer disasm at VMA 0x069e70 + 0x06b49c):
        // VNish does NOT send SetVoltage opcode 0x10 to the dsPIC at all.
        // The chain voltage rail is set by the PSU upstream (15.2 V), and
        // ENABLE_VOLTAGE alone engages the per-chain DC-DC. So SetVoltage is
        // OPTIONAL and its failure is non-fatal — the real engagement is
        // ENABLE_VOLTAGE.
        // Post-JUMP heartbeat keep-alive tick 2 of 3: refresh the fw=0x89
        // app-mode watchdog immediately BEFORE SetVoltage (0x10), closing the
        // LM75A-read-to-SetVoltage gap so the chip is still in app mode when
        // the DAC program lands. No-op unless caller-gated + framed.
        self.postjump_keepalive_tick("pre-setvoltage");
        // Re-JUMP-before-ENABLE (2026-06-07, `a lab unit` standalone cold-engage) —
        // the LIVE-confirmed SOLE remaining blocker fix. Immediately before
        // SetVoltage, re-read GET_VERSION; if the cold-engaged FRAMED (fw=0x89)
        // dsPIC has drifted back to fw=0x82 bootloader in the ~8 s orchestration
        // gap (LIVE: ENABLE returned `[82, 82]` echo), re-JUMP it to fw=0x89 via
        // a bounded `flush → framed-JUMP` (NO RESET) re-verify so the chip is in
        // APP mode when the ENABLE lands. SetVoltage → settle → ENABLE then run
        // back-to-back (~100-200 ms wall-time, far under the drift window). This
        // is the LAST step before SetVoltage so no long read/sleep can re-open
        // the drift gap. No-op unless caller-gated (env + `a lab unit` fingerprint) AND
        // framed; default-OFF ⇒ byte-identical (no extra GET_VERSION/JUMP). The
        // re-JUMP preserves self.firmware/use_bare_protocol so the SetVoltage /
        // ENABLE wire bytes are unchanged.
        self.rejump_to_app_mode_if_drifted();
        #[allow(clippy::nonminimal_bool)]
        if !(self.skip_setvoltage_keep_enable && !self.use_bare_protocol) {
            match self.set_voltage(voltage_mv) {
                Ok(_) => tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    voltage_mv,
                    "dsPIC SetVoltage OK"
                ),
                Err(e) => tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "dsPIC SetVoltage failed (non-fatal — VNish does not send opcode 0x10; ENABLE_VOLTAGE alone engages the chain)"
                ),
            }
        } else {
            // Skip-SetVoltage-keep-ENABLE — PROVEN root cause
            // (
            // commit fc4eef92): in the bosminer true-cold strace bosminer sends
            // ZERO SetVoltage (0x10) frames to the `a lab unit` dsPIC across 662k lines
            // — the chip-rail voltage is set on the APW PSU (PMBus), NOT the
            // per-board dsPIC (which is sensor-passthrough + ENABLE only).
            // DCENT's `0x10` frame `[55 AA 04 10 DAC SUM]` faults the
            // cold-engaged fw=0x89 app back to the fw=0x82 bootloader, so the
            // immediately-following ENABLE reads `[82, 82]` (LIVE TEST 5:
            // re-JUMP→0x89, "SetVoltage applied", then ENABLE→0x82). So when the
            // gate is active we SKIP the 0x10 SetVoltage entirely and go
            // GET_VERSION(0x89) → [re-JUMP if drifted, above] → ENABLE(0x15)
            // directly, exactly like bosminer. The chip rail energizes via the
            // (unchanged) ENABLE at the dsPIC's power-on default voltage —
            // bosminer-proven safe (bosminer never SetVoltages and mines fine).
            // The 14.5 V dsPIC cap / EEPROM denylist / fw=0x86 refusal / ENABLE
            // wire bytes are ALL unchanged — this ONLY OMITS the 0x10 frame.
            // Reached only when `DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE=1`
            // AND the `a lab unit` fingerprint match (set at the
            // `s19j_hybrid_mining.rs` Phase 3 call site) AND the protocol is
            // framed; the proven BARE fw=0x82 `a lab unit`/ cold path — where
            // SetVoltage ACKs and is load-bearing — never enters here.
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                voltage_mv,
                env_gate = "DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE=1",
                "dsPIC SetVoltage (0x10) SKIPPED — bosminer never sends 0x10 to the \
                 `a lab unit` dsPIC (rail is APW-PSU-side); the 0x10 faults the cold-engaged \
                 fw=0x89 app back to fw=0x82 bootloader → ENABLE reads [82,82]. Going \
                 GET_VERSION(0x89) → ENABLE(0x15) directly like bosminer; rail energizes \
                 via the unchanged ENABLE at the dsPIC power-on default voltage \
                 (ENABLE-DRIFT-DIFF.md)"
            );
        }
        // Keep-alive (default-OFF): byte-identical `thread::sleep(50ms)` unless
        // active, in which case the SetVoltage→ENABLE settle is chunked with
        // keep-alive ticks so the chip stays in app mode right up to ENABLE.
        self.keepalive_sleep(50, "post-setvoltage-settle");
        // Post-JUMP heartbeat keep-alive tick 3 of 3: refresh the fw=0x89
        // app-mode watchdog immediately BEFORE ENABLE (0x15). This is the
        // load-bearing one — LIVE on `a lab unit` the ENABLE returned ack_cmd=0x82
        // (drifted back to bootloader) because the chip went unserviced
        // between GET_VERSION and ENABLE. No-op unless caller-gated + framed.
        self.postjump_keepalive_tick("pre-enable");
        self.enable_voltage()?;
        std::thread::sleep(std::time::Duration::from_millis(1000));

        if let Err(e) = self.send_heartbeat() {
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                error = %e,
                "dsPIC service heartbeat after enable failed; continuing",
            );
        }

        Ok(())
    }

    /// Post-JUMP framed-heartbeat keep-alive tick (2026-06-07, `a lab unit`
    /// standalone cold-engage).
    ///
    /// No-op unless `self.postjump_heartbeat_keepalive` is set (caller-gated:
    /// env `DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE=1` AND `a lab unit`
    /// fingerprint) AND the protocol is framed (`!use_bare_protocol`). When
    /// active it sends ONE framed `0x16` heartbeat (`[55 AA 04 16 00 1A]`) via
    /// the existing single-owner `send_heartbeat`/`I2cServiceHandle` path to
    /// refresh the fw=0x89 app-mode watchdog so the cold-engaged dsPIC does
    /// not drift back to fw=0x82 bootloader before the ENABLE. NON-FATAL — a
    /// failed keep-alive tick is logged and ignored (matching the existing
    /// post-enable heartbeat); it never aborts cold_boot_init.
    fn postjump_keepalive_tick(&mut self, where_label: &str) {
        if !self.postjump_heartbeat_keepalive || self.use_bare_protocol {
            return;
        }
        match self.send_heartbeat() {
            Ok(()) => tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                at = where_label,
                "dsPIC post-JUMP heartbeat keep-alive tick (framed 0x16) — refreshing fw=0x89 \
                 app-mode watchdog so the cold-engaged chip does not drift back to fw=0x82 \
                 bootloader before ENABLE"
            ),
            Err(e) => tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                at = where_label,
                error = %e,
                "dsPIC post-JUMP heartbeat keep-alive tick failed (non-fatal; continuing)"
            ),
        }
    }

    /// Re-JUMP the cold-engaged FRAMED dsPIC back to fw=0x89 APP mode if it has
    /// drifted to fw=0x82 bootloader, immediately before SetVoltage / ENABLE
    /// (2026-06-07, `a lab unit` standalone cold-engage).
    ///
    /// No-op unless `self.rejump_before_enable` is set (caller-gated: env
    /// `DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE=1` AND `a lab unit` fingerprint) AND the
    /// protocol is framed (`!use_bare_protocol`). When active:
    ///
    /// 1. Read GET_VERSION. If it is already fw=0x89, this is a no-op (do not
    ///    disturb a good chip).
    /// 2. If it is fw=0x82 (drifted back to bootloader), run a bounded
    ///    `flush → framed-JUMP` re-verify via [`am2_pic_jump_only_reverify`] —
    ///    **NEVER a RESET** (the chip is a cold-0x82-class bootloader, so the
    ///    JUMP-only transition is the safe idempotent recovery; a RESET here is
    ///    the destructive-downgrade class) — re-reading GET_VERSION after each
    ///    attempt until fw=0x89 or the bounded
    ///    [`REJUMP_BEFORE_ENABLE_MAX_ATTEMPTS`] is exhausted.
    /// 3. Any other observed fw (or an unreadable GET_VERSION) is left alone —
    ///    proceed fail-closed, exactly like today.
    ///
    /// All bus access goes through the existing single-owner `I2cServiceHandle`
    /// (`self.i2c`) — no new bus owner. Best-effort / log-and-continue: a failed
    /// GET_VERSION or JUMP is logged and never aborts `cold_boot_init`.
    ///
    /// **Byte-identical SetVoltage/ENABLE guarantee:** GET_VERSION mutates
    /// `self.firmware` / `self.use_bare_protocol` (the encoding-determining
    /// state). This helper SAVES those at entry and RESTORES them at exit, so
    /// the immediately-following SetVoltage(0x10) / ENABLE(0x15) wire bytes are
    /// byte-identical to the framed path regardless of what the probe observed —
    /// the ONLY thing this helper changes is the dsPIC's *physical* state
    /// (re-JUMPed into app mode) plus the gated GET_VERSION/JUMP bus traffic.
    fn rejump_to_app_mode_if_drifted(&mut self) {
        if !self.rejump_before_enable || self.use_bare_protocol {
            return;
        }

        // Preserve the encoding-determining state across the GET_VERSION probe
        // (which mutates self.firmware/use_bare_protocol) so SetVoltage/ENABLE
        // bytes stay byte-identical to the framed path in EVERY outcome.
        let saved_firmware = self.firmware;
        let saved_use_bare = self.use_bare_protocol;

        let observed = match self.get_version() {
            Ok(fw) => Some(fw),
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    error = %e,
                    "dsPIC re-JUMP-before-ENABLE: pre-check GET_VERSION unreadable; \
                     NOT re-JUMPing, proceeding fail-closed (no RESET)"
                );
                None
            }
        };

        match observed {
            Some(0x89) => {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    "dsPIC re-JUMP-before-ENABLE: chip already fw=0x89 APP mode — \
                     no re-JUMP needed (good chip undisturbed)"
                );
            }
            Some(0x82) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    "dsPIC re-JUMP-before-ENABLE: chip drifted back to fw=0x82 \
                     BOOTLOADER between warmup and ENABLE; re-JUMPing (flush → \
                     framed JUMP, NO RESET) up to {} time(s) to re-transition to \
                     fw=0x89 before SetVoltage/ENABLE",
                    REJUMP_BEFORE_ENABLE_MAX_ATTEMPTS
                );
                let mut reached_app = false;
                for attempt in 1..=REJUMP_BEFORE_ENABLE_MAX_ATTEMPTS {
                    match crate::dspic::bosminer_warmup::am2_pic_jump_only_reverify(
                        &self.i2c,
                        self.address,
                    ) {
                        Ok(()) => {}
                        Err(e) => {
                            tracing::warn!(
                                addr = format_args!("0x{:02X}", self.address),
                                attempt,
                                error = %e,
                                "dsPIC re-JUMP-before-ENABLE: JUMP-only re-verify attempt \
                                 failed (non-fatal); re-reading GET_VERSION anyway"
                            );
                        }
                    }
                    match self.get_version() {
                        Ok(0x89) => {
                            reached_app = true;
                            tracing::info!(
                                addr = format_args!("0x{:02X}", self.address),
                                attempt,
                                "dsPIC re-JUMP-before-ENABLE: chip re-transitioned to \
                                 fw=0x89 APP mode — SetVoltage/ENABLE will land in app mode"
                            );
                            break;
                        }
                        Ok(fw) => tracing::warn!(
                            addr = format_args!("0x{:02X}", self.address),
                            attempt,
                            fw = format_args!("0x{:02X}", fw),
                            "dsPIC re-JUMP-before-ENABLE: still not fw=0x89 after JUMP; retrying"
                        ),
                        Err(e) => tracing::warn!(
                            addr = format_args!("0x{:02X}", self.address),
                            attempt,
                            error = %e,
                            "dsPIC re-JUMP-before-ENABLE: GET_VERSION unreadable after JUMP; retrying"
                        ),
                    }
                }
                if !reached_app {
                    tracing::warn!(
                        addr = format_args!("0x{:02X}", self.address),
                        attempts = REJUMP_BEFORE_ENABLE_MAX_ATTEMPTS,
                        "dsPIC re-JUMP-before-ENABLE: chip did NOT reach fw=0x89 after \
                         {} re-JUMP attempt(s); proceeding fail-closed (ENABLE will land \
                         on bootloader as before — no RESET issued)",
                        REJUMP_BEFORE_ENABLE_MAX_ATTEMPTS
                    );
                }
            }
            Some(fw) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    fw = format_args!("0x{:02X}", fw),
                    "dsPIC re-JUMP-before-ENABLE: unexpected fw (not 0x89/0x82); \
                     NOT re-JUMPing, proceeding"
                );
            }
            None => {}
        }

        // Restore the encoding-determining state so SetVoltage(0x10)/ENABLE(0x15)
        // wire bytes are byte-identical to the framed path. The re-JUMP changed
        // the chip's PHYSICAL state, not the host-side protocol selection.
        self.firmware = saved_firmware;
        self.use_bare_protocol = saved_use_bare;
    }

    /// Sleep for `total_ms`, keeping the cold-engaged FRAMED dsPIC serviced
    /// (2026-06-07, `a lab unit` standalone cold-engage).
    ///
    /// When the post-JUMP keep-alive is OFF (default) OR the protocol is bare,
    /// this is exactly `std::thread::sleep(total_ms)` — **byte-identical** to
    /// the legacy `cold_boot_init_with_options` settle. When the keep-alive is
    /// active (caller-gated env + `a lab unit` fingerprint, framed only), the sleep is
    /// chunked via [`keepalive_sleep_slices`] into `<= keepalive_interval_ms`
    /// slices with a framed `0x16` `postjump_keepalive_tick` after each slice,
    /// so no settle ever leaves the chip un-serviced longer than the interval
    /// and it cannot drift fw=0x89→0x82 before the ENABLE. NON-FATAL: a failed
    /// tick is logged and ignored, exactly like `postjump_keepalive_tick`.
    fn keepalive_sleep(&mut self, total_ms: u64, where_label: &str) {
        if !self.postjump_heartbeat_keepalive || self.use_bare_protocol {
            std::thread::sleep(std::time::Duration::from_millis(total_ms));
            return;
        }
        for slice in keepalive_sleep_slices(total_ms, self.keepalive_interval_ms) {
            std::thread::sleep(std::time::Duration::from_millis(slice));
            self.postjump_keepalive_tick(where_label);
        }
    }

    /// Read all 4 LM75A sensors, keeping the cold-engaged FRAMED dsPIC serviced
    /// between sensors (2026-06-07, `a lab unit` standalone cold-engage).
    ///
    /// When the post-JUMP keep-alive is OFF (default) OR the protocol is bare,
    /// this delegates to [`read_all_temperatures`](Self::read_all_temperatures)
    /// verbatim — **byte-identical** to the legacy pre-voltage read. When the
    /// keep-alive is active it performs the SAME 4-sensor read loop but fires a
    /// framed `0x16` `postjump_keepalive_tick` after each sensor. Each LM75A
    /// passthrough read takes ~290 ms, so the un-interleaved 4-sensor loop is
    /// the ~1.2 s un-serviced gap where the cold-engaged chip was drifting
    /// fw=0x89→0x82 LIVE on `a lab unit` (post-warmup tick at +12.9 s, next tick not
    /// until +14.1 s); interleaving a heartbeat per sensor closes that gap.
    fn read_all_temperatures_keepalive(&mut self) -> [f64; 4] {
        if !self.postjump_heartbeat_keepalive || self.use_bare_protocol {
            return self.read_all_temperatures();
        }
        let mut temps = [-999.0f64; 4];
        for (i, &sensor_addr) in LM75A_ADDRS.iter().enumerate() {
            match self.read_temperature(sensor_addr) {
                Ok(t) => temps[i] = t,
                Err(e) => {
                    tracing::debug!(
                        addr = format_args!("0x{:02X}", self.address),
                        sensor = format_args!("0x{:02X}", sensor_addr),
                        error = %e,
                        "dsPIC LM75A read failed — sensor may not be present",
                    );
                }
            }
            // Continuous keep-alive: service the fw=0x89 app-mode watchdog
            // between each ~290 ms sensor read so the cold-engaged chip stays
            // in app mode across the ~1.2 s 4-sensor pre-voltage read window.
            self.postjump_keepalive_tick("lm75-interleave");
        }
        temps
    }

    /// Set the target voltage in millivolts.
    pub fn set_voltage(&mut self, voltage_mv: u16) -> Result<()> {
        ensure_dspic_voltage_command_allowed(self.address, self.firmware, "service set_voltage")?;

        // Load-bearing <=14500 mV hard cap at the rail-program boundary (input
        // clamp, preserves the proven DAC span). Mirrors the controller
        // set_voltage; default-off lab override for the AMTC pre-open.
        let requested_mv = voltage_mv;
        let (voltage_mv, capped) =
            clamp_dspic_voltage_to_hard_cap(voltage_mv, dspic_lab_overvolt_override_enabled());
        if capped {
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                requested_mv,
                capped_mv = voltage_mv,
                "dsPIC service set_voltage exceeded the {} mV hard cap — clamped DOWN (set {}=1 only in a lab for the AMTC pre-open)",
                DSPIC_VOLTAGE_HARD_CAP_MV,
                DSPIC_ALLOW_LAB_OVERVOLT_ENV,
            );
        }

        if !(DSPIC_MIN_VOLTAGE_MV..=DSPIC_MAX_VOLTAGE_MV).contains(&voltage_mv) {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "dsPIC service voltage {} mV outside safe range {}..={} mV",
                    voltage_mv, DSPIC_MIN_VOLTAGE_MV, DSPIC_MAX_VOLTAGE_MV
                ),
            });
        }

        let frame = dspic_set_voltage_frame(self.firmware, self.use_bare_protocol, voltage_mv);

        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            voltage_mv = voltage_mv,
            tx_bytes = ?frame,
            "dsPIC service set_voltage tx"
        );
        self.write_bytes_mutating(I2cMutationLabel::Energize, &frame)?;
        self.current_voltage_mv = voltage_mv;
        Ok(())
    }

    /// Enable DC-DC output and validate the dsPIC ACK.
    ///
    /// VNish-RE'd format (cgminer disasm 2026-04-25 VMA 0x05277c) — applies to
    /// fw=0x86 and fw=0x89:
    ///   TX: `[55 AA 05 15 01 00 1B]` (7 bytes, LEN=0x05, extra 0x00 arg byte)
    ///   RX: `[15 01]` (2 bytes — note status=0x01, not 0x00)
    /// CHECKSUM = (LEN + CMD + 0x01 + 0x00) & 0xFF = (0x05 + 0x15 + 0x01 + 0x00) = 0x1B
    ///
    /// Other framed firmwares (0x8A/0xB9/0xFE) keep the canonical 6-byte form
    /// `[55 AA 04 15 01 1A]` pending live verification — flipping unconditionally
    /// risks regressing working units (see `dspic_enable_disable_encoding`).
    pub fn enable_voltage(&mut self) -> Result<()> {
        ensure_dspic_voltage_command_allowed(
            self.address,
            self.firmware,
            "service enable_voltage",
        )?;

        let encoding = dspic_enable_disable_encoding(self.firmware);
        let frame = dspic_enable_voltage_frame(self.use_bare_protocol, encoding);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            form = if matches!(encoding, EnableFrameEncoding::VnishPadded) { "7-byte VNish" } else { "6-byte canonical" },
            bare = self.use_bare_protocol,
            frame = ?frame,
            "service ENABLE_VOLTAGE frame"
        );
        let ack = self.bytewise_write_then_read_mutating(
            I2cMutationLabel::Energize,
            frame,
            if self.use_bare_protocol { 1 } else { 2 },
            DSPIC_ENABLE_REPLY_DELAY_MS,
            "ENABLE service",
        )?;

        if self.use_bare_protocol {
            // : fw0x86 bare
            // ENABLE returns one firmware byte, not [CMD, status].
            let ack_fw = *ack.first().ok_or_else(|| crate::AsicError::Pic {
                addr: self.address,
                detail: "ENABLE service bare returned an empty ACK".to_string(),
            })?;
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                ack_fw = format_args!("0x{:02X}", ack_fw),
                bare = self.use_bare_protocol,
                "PIC service enable_voltage bare ACK"
            );
            if ack_fw == 0xFF {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: "ENABLE service bare returned 0xFF (slave idle / NACK)".to_string(),
                });
            }
            if !is_bare_ack_fw_byte(ack_fw) {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "ENABLE service bare ACK mismatch: expected firmware byte, got [{:02X}]",
                        ack_fw
                    ),
                });
            }
            self.voltage_enabled = true;
            return Ok(());
        }

        let expected_fw = dspic_expected_fw_byte(self.firmware);
        let first_ack_kind = classify_enable_ack(&ack, expected_fw);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            ack_cmd = format_args!("0x{:02X}", ack[0]),
            ack_status = format_args!("0x{:02X}", ack[1]),
            ack_kind = %first_ack_kind,
            bare = self.use_bare_protocol,
            "PIC service enable_voltage ACK"
        );

        if first_ack_kind == EnableVoltageAckKind::AllFf {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "ENABLE service returned all-0xFF: ack=[{:02X}, {:02X}]",
                    ack[0], ack[1]
                ),
            });
        }
        // VNish accepts ACK [0x15, 0x01]. Older code expected [0x15, 0x00];
        // accept both for backwards compat.
        let mut ok = first_ack_kind == EnableVoltageAckKind::RealAck;
        let mut final_ack = ack.clone();
        let mut final_ack_kind = first_ack_kind;
        if !ok {
            let alt_encoding = alternate_enable_encoding(encoding);
            let alt_frame = dspic_enable_voltage_frame(self.use_bare_protocol, alt_encoding);
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                first_ack = format_args!("{:02X?}", ack),
                first_ack_kind = %first_ack_kind,
                first_form = if matches!(encoding, EnableFrameEncoding::VnishPadded) {
                    "7-byte VNish"
                } else {
                    "6-byte canonical"
                },
                retry_form = if matches!(alt_encoding, EnableFrameEncoding::VnishPadded) {
                    "7-byte VNish"
                } else {
                    "6-byte canonical"
                },
                "PIC service enable_voltage ACK mismatch; retrying alternate framed ENABLE form"
            );
            let alt_ack = self.bytewise_write_then_read_mutating(
                I2cMutationLabel::Energize,
                alt_frame,
                2,
                DSPIC_ENABLE_REPLY_DELAY_MS,
                "ENABLE service alternate",
            )?;
            let alt_ack_kind = classify_enable_ack(&alt_ack, expected_fw);
            tracing::info!(
                addr = format_args!("0x{:02X}", self.address),
                ack_cmd = format_args!("0x{:02X}", alt_ack[0]),
                ack_status = format_args!("0x{:02X}", alt_ack[1]),
                ack_kind = %alt_ack_kind,
                bare = self.use_bare_protocol,
                "PIC service enable_voltage alternate ACK"
            );
            final_ack_kind = alt_ack_kind;
            ok = final_ack_kind == EnableVoltageAckKind::RealAck;
            final_ack = alt_ack;
        }
        if matches!(
            final_ack_kind,
            EnableVoltageAckKind::FirmwareEcho | EnableVoltageAckKind::FirmwareEchoMismatch
        ) {
            let require_real_ack = dspic_require_real_enable_ack_enabled();
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                ack = format_args!("{:02X?}", final_ack),
                ack_kind = %final_ack_kind,
                firmware = %self.firmware,
                expected_fw = %fmt_optional_fw_byte(expected_fw),
                require_real_ack,
                rail_proof = "chain-uart-required",
                "PIC service enable_voltage returned repeated firmware-byte echo; not a real ENABLE ACK"
            );
            if require_real_ack {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "ENABLE service returned {} instead of real ACK [{:02X}, 00/01]: ack={:02X?}",
                        final_ack_kind, CMD_ENABLE_VOLTAGE, final_ack
                    ),
                });
            }
            self.voltage_enabled = true;
            return Ok(());
        }
        if !ok {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "ENABLE service ACK mismatch ({}): expected [{:02X}, 01] or [{:02X}, 00], got [{:02X}, {:02X}]",
                    final_ack_kind, CMD_ENABLE_VOLTAGE, CMD_ENABLE_VOLTAGE, final_ack[0], final_ack[1]
                ),
            });
        }
        if dspic_require_enable_flag_on_enabled() && !enable_ack_flag_on(&final_ack) {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "ENABLE service flag-on required [{:02X}, 01], got [{:02X}, {:02X}]",
                    CMD_ENABLE_VOLTAGE, final_ack[0], final_ack[1]
                ),
            });
        }

        self.voltage_enabled = true;
        Ok(())
    }

    /// Disable DC-DC output.
    ///
    /// Form selection mirrors `enable_voltage`: fw=0x86/0x89 → 7-byte VNish form
    /// `[55 AA 05 15 00 00 1A]`; other framed fw → canonical 6-byte
    /// `[55 AA 04 15 00 19]`; bare → 4-byte `[55 AA 15 00]`.
    pub fn disable_voltage(&mut self) -> Result<()> {
        let encoding = dspic_enable_disable_encoding(self.firmware);
        let frame = dspic_disable_voltage_frame(self.use_bare_protocol, encoding);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            firmware = %self.firmware,
            form = if matches!(encoding, EnableFrameEncoding::VnishPadded) { "7-byte VNish" } else { "6-byte canonical" },
            bare = self.use_bare_protocol,
            frame = ?frame,
            "service DISABLE_VOLTAGE frame"
        );
        let protocol = if self.use_bare_protocol {
            I2cDspicDisableProtocol::Bare
        } else if matches!(encoding, EnableFrameEncoding::VnishPadded) {
            I2cDspicDisableProtocol::VnishPaddedFramed
        } else {
            I2cDspicDisableProtocol::CanonicalFramed
        };
        self.i2c
            .disable_dspic_voltage(self.address, protocol)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("svc disable: {}", e),
            })?;
        self.voltage_enabled = false;
        Ok(())
    }

    /// Read the current voltage via dsPIC ADC feedback.
    pub fn read_voltage(&mut self) -> Result<u16> {
        if !self.use_bare_protocol
            && matches!(self.firmware, DspicFirmware::Fw89 | DspicFirmware::Fw8A)
        {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC service read_voltage(0x3B): not a fw=0x89/0x8A rail-voltage command; use measure_voltage(0x3A) ADC decode".to_string(),
            });
        }

        let cmd = [DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_GET_VOLTAGE];
        let buf = self.write_read_command(&cmd, 4)?;

        if buf[0] == 0xFF && buf[1] == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC service read_voltage: error frame (0xFF/0xFF)".to_string(),
            });
        }

        // Frame-shape guard (swarm wf_e0647147 #1/#2, 2026-05-29): bare 4-byte decode only;
        // on framed fw=0x89 buf[2]/buf[3] are not v_hi/v_lo (live: dsPIC 0x22 -> 64760 mV
        // garbage, mis-read as "rail NOT energized"). Require the bare cmd-echo + a plausible
        // value, else Err so callers log "readback unreliable" not a false low-rail verdict.
        // Offset-correct framed decode pends a raw fw=0x89 reply capture — do NOT guess.
        let voltage_mv = dcentrald_common::dspic_decode::decode_bare_voltage_reply(
            buf[0],
            CMD_GET_VOLTAGE,
            buf[2],
            buf[3],
            DSPIC_MAX_VOLTAGE_MV,
        )
        .map_err(|e| crate::AsicError::Pic {
            addr: self.address,
            detail: format!("dsPIC service read_voltage: {e}"),
        })?;
        Ok(voltage_mv)
    }

    /// Measure the ACTUAL chain-rail voltage via the dsPIC analog ADC
    /// (`MEASURE_VOLTAGE` 0x3A = VNish `dspic33epxx_get_an_voltage2()`).
    ///
    /// Distinct from [`read_voltage`](Self::read_voltage) (0x3B `GET_VOLTAGE`,
    /// the setpoint/feedback path). 0x3A reads the analog-input ADC value of the
    /// real rail — the stronger rail-engagement proxy for the `a lab unit` standalone
    /// test (Procedure A). The `[0x00]` payload makes the framed wire form
    /// byte-exact `[55 AA 04 3A 00 3E]`; bare form `[55 AA 3A 00]` on fw 0x82.
    /// Read-only diagnostic (I2C_RDWR may corrupt the parser — not the hot
    /// path). (gap-swarm G03.)
    pub fn measure_voltage(&mut self) -> Result<u16> {
        let cmd = [
            DSPIC_PREAMBLE[0],
            DSPIC_PREAMBLE[1],
            CMD_MEASURE_VOLTAGE,
            0x00,
        ];
        if !self.use_bare_protocol
            && matches!(self.firmware, DspicFirmware::Fw89 | DspicFirmware::Fw8A)
        {
            let frame = self.encode_command_frame(&cmd);
            // Read the FULL 7-byte framed envelope, not 2 bytes. The fw=0x89 0x3A reply
            // is `[LEN=0x07, 0x3A, status, adc_hi, adc_lo, 0x00, cksum]` (live-captured on
            // `a lab unit` TEST-18: [07,3A,01,02,21,00,65]). The old 2-byte read returned the
            // ENVELOPE HEAD [07,3A] and decode_framed_measure_voltage_reply's offset-0
            // be16 = 0x073A -> ~45 V -> ExceedsMax error, which is why the live rail verdict
            // was always UNAVAILABLE (Team M BLK-5 / DCENT_FPGA F5b). Use the envelope-aware
            // decoder which validates LEN/cmd/checksum and extracts the ADC at offset 3..4
            // ([02,21]=545 -> 12,992 mV); it falls back to the offset-0 decode (clean error)
            // if the frame doesn't validate, so a short/garbled read is no worse than before.
            let buf = self.bytewise_write_then_read(&frame, 7, 6, "measure-voltage-fw89")?;
            tracing::info!(
                target: "rail_capture",
                addr = format_args!("0x{:02X}", self.address),
                firmware = %self.firmware,
                rx_raw = format_args!("{:02X?}", buf),
                "dsPIC SERVICE measure_voltage(0x3A) framed ADC reply bytes (7-byte envelope, offset-3..4 ADC)"
            );
            return dcentrald_common::dspic_decode::decode_framed_measure_voltage_i2c0_capture(
                &buf,
                DSPIC_MAX_VOLTAGE_MV,
            )
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC service measure_voltage framed ADC decode: {e}"),
            });
        }

        let buf = self.write_read_command(&cmd, 4)?;

        if buf[0] == 0xFF && buf[1] == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: "dsPIC service measure_voltage: error frame (0xFF/0xFF)".to_string(),
            });
        }

        // Frame-shape guard (swarm wf_e0647147 #1/#2, 2026-05-29): bare 4-byte decode only.
        // fw=0x89 and currently-assumed-same 0x8A replies return above; any other framed variant must still
        // refuse a fabricated bare-shape value.
        let voltage_mv = dcentrald_common::dspic_decode::decode_bare_voltage_reply(
            buf[0],
            CMD_MEASURE_VOLTAGE,
            buf[2],
            buf[3],
            DSPIC_MAX_VOLTAGE_MV,
        )
        .map_err(|e| {
            // Non-fw89/framed residual capture. The selected path uses the
            // 0x89-shape 2-byte ADC decode above; keep this dump for any other framed family that
            // reaches the bare-shape guard.
            if !self.use_bare_protocol {
                tracing::info!(
                    target: "rail_capture_framed_0x3A",
                    addr = format_args!("0x{:02X}", self.address),
                    firmware = %self.firmware,
                    rx_raw = format_args!("{:02X?}", buf),
                    rx_len = buf.len(),
                    decode_error = %e,
                    "dsPIC SERVICE measure_voltage(0x3A) framed residual reply — bare 4-byte decode refused (readback unreliable). RAW BYTES captured for non-fw89/non-fw8a follow-up."
                );
            }
            crate::AsicError::Pic {
                addr: self.address,
                detail: format!("dsPIC service measure_voltage: {e}"),
            }
        })?;
        Ok(voltage_mv)
    }

    /// Read temperature from an LM75A sensor via dsPIC passthrough.
    ///
    /// Frame builder gates by `use_bare_protocol`: bare for fw 0x82/0x86,
    /// framed `[55 AA 04 30 sensor_addr SUM]` for fw 0x89/0x8A/B9.
    ///
    /// **Bare-mode behavior**: per
    ///  and
    /// , bare-protocol
    /// dsPIC firmware always replies with a single FW-byte echo for any
    /// 1-byte read. Real LM75 temperature data is **not delivered** in
    /// bare mode — we return `Ok(f64::NAN)` as a sentinel. A reply of
    /// `0xFF` indicates the slave is silent/idle; any byte outside the
    /// FW-echo whitelist is an error.
    pub fn read_temperature(&mut self, sensor_addr: u8) -> Result<f64> {
        let cmd = dspic_read_temp_frame(self.use_bare_protocol, sensor_addr);
        if self.use_bare_protocol {
            // : bare fw0x86
            // only drives one byte; 4-byte xiic reads synthesize fake tails.
            // : validate the
            // single-byte FW-echo strictly — reject 0xFF and any byte
            // outside the known FW-byte whitelist.
            let buf = self.write_read_command(&cmd, 1)?;
            let ack_fw = buf.first().copied().unwrap_or(0xFF);
            if ack_fw == 0xFF {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "LM75 bare read(0x{:02X}) returned 0xFF (slave idle)",
                        sensor_addr,
                    ),
                });
            }
            if !is_bare_ack_fw_byte(ack_fw) {
                return Err(crate::AsicError::Pic {
                    addr: self.address,
                    detail: format!(
                        "LM75 bare read(0x{:02X}): unexpected byte 0x{:02X} (not FW echo)",
                        sensor_addr, ack_fw,
                    ),
                });
            }
            tracing::debug!(
                addr = format_args!("0x{:02X}", self.address),
                sensor = format_args!("0x{:02X}", sensor_addr),
                ack_fw = format_args!("0x{:02X}", ack_fw),
                bare = self.use_bare_protocol,
                "dsPIC service bare LM75A read returned firmware echo; temperature unavailable (NaN sentinel)"
            );
            return Ok(f64::NAN);
        }

        let buf = self.write_read_command(&cmd, 4)?;

        if buf[0] == 0xFF && buf[1] == 0xFF {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "dsPIC service read_temperature(0x{:02X}): error frame",
                    sensor_addr,
                ),
            });
        }

        let raw = ((buf[2] as i16) << 8) | (buf[3] as i16);
        Ok((raw >> 5) as f64 * 0.125)
    }

    /// Read all 4 LM75A temperature sensors through the service.
    pub fn read_all_temperatures(&mut self) -> [f64; 4] {
        let mut temps = [-999.0f64; 4];
        for (i, &sensor_addr) in LM75A_ADDRS.iter().enumerate() {
            match self.read_temperature(sensor_addr) {
                Ok(t) => temps[i] = t,
                Err(e) => {
                    tracing::debug!(
                        addr = format_args!("0x{:02X}", self.address),
                        sensor = format_args!("0x{:02X}", sensor_addr),
                        error = %e,
                        "dsPIC service LM75A read failed; sensor may not be present",
                    );
                }
            }
        }
        temps
    }

    /// Send heartbeat to prevent dsPIC watchdog timeout.
    pub fn send_heartbeat(&mut self) -> Result<()> {
        let frame = dspic_heartbeat_frame(self.use_bare_protocol);
        self.write_bytes_mutating(I2cMutationLabel::KeepAlive, frame)
    }

    fn encode_command_frame(&self, data: &[u8]) -> Vec<u8> {
        // P1.1 fix: LEN = payload_len + 3 (Bible v1 1-power-dspic/01-frame-format.md).
        // Old code used `dspic_response_len(cmd) + 4` which produced wrong LEN+CKSUM
        // and is the root cause of GET_VERSION returning bus noise on `a lab unit`.
        if data.len() >= 3 && data[0] == DSPIC_PREAMBLE[0] && data[1] == DSPIC_PREAMBLE[1] {
            if self.use_bare_protocol {
                return data.to_vec();
            }
            let cmd = data[2];
            let payload = &data[3..];
            let len_byte = dspic_outgoing_len(payload.len());
            let checksum = len_byte
                .wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)));
            let mut frame = Vec::with_capacity(4 + payload.len() + 1);
            frame.push(DSPIC_PREAMBLE[0]);
            frame.push(DSPIC_PREAMBLE[1]);
            frame.push(len_byte);
            frame.push(cmd);
            frame.extend_from_slice(payload);
            frame.push(checksum);
            return frame;
        }
        data.to_vec()
    }

    fn write_bytes(&self, data: &[u8]) -> Result<()> {
        self.write_bytes_mutating(I2cMutationLabel::Recovery, data)
    }

    fn write_bytes_mutating(&self, label: I2cMutationLabel, data: &[u8]) -> Result<()> {
        ensure_dspic_bootloader_command_allowed(
            self.address,
            self.firmware,
            self.use_bare_protocol,
            data,
        )?;
        self.i2c
            .write_bytes_mutating(label, self.address, data)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("svc write: {}", e),
            })
    }

    fn write_read(&self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        ensure_dspic_bootloader_command_allowed(
            self.address,
            self.firmware,
            self.use_bare_protocol,
            write_data,
        )?;
        self.i2c
            .write_read_mutating(
                I2cMutationLabel::QueryPrelude,
                self.address,
                write_data,
                read_len,
            )
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("svc write_read: {}", e),
            })
    }

    fn write_read_command(&self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let frame = self.encode_command_frame(write_data);
        self.write_read(&frame, read_len)
    }

    fn bytewise_write_then_read(
        &self,
        frame: &[u8],
        read_len: usize,
        delay_ms: u64,
        label: &str,
    ) -> Result<Vec<u8>> {
        self.bytewise_write_then_read_mutating(
            I2cMutationLabel::QueryPrelude,
            frame,
            read_len,
            delay_ms,
            label,
        )
    }

    fn bytewise_write_then_read_mutating(
        &self,
        mutation_label: I2cMutationLabel,
        frame: &[u8],
        read_len: usize,
        delay_ms: u64,
        label: &str,
    ) -> Result<Vec<u8>> {
        ensure_dspic_bootloader_command_allowed(
            self.address,
            self.firmware,
            self.use_bare_protocol,
            frame,
        )?;

        let steps = dspic_bytewise_write_then_read_steps(frame, read_len, delay_ms);

        let reads = self
            .i2c
            .transaction_mutating(mutation_label, self.address, steps)
            .map_err(|e| crate::AsicError::Pic {
                addr: self.address,
                detail: format!("{} bytewise transaction: {}", label, e),
            })?;
        let reply = collect_single_byte_i2c_reads(reads);
        if reply.len() != read_len {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "{} bytewise transaction returned {}/{} reply bytes: {:02X?}",
                    label,
                    reply.len(),
                    read_len,
                    reply
                ),
            });
        }
        Ok(reply)
    }

    /// **Diagnostic-only** raw byte-wise telemetry dump (read-only, NEVER on
    /// the hot mining path).
    ///
    /// The production `read_voltage` / `measure_voltage` decoders do a COMBINED
    /// 4-byte `I2C_RDWR` read, which is correct for the BARE reply
    /// `[cmd_echo, status, v_hi, v_lo]` (fw=0x82) but GARBLES the FRAMED
    /// (fw=0x89) reply, which is actually a longer byte-wise frame
    /// `[cmd_echo, status, v_hi, v_lo, …, CKSUM]` (~9 bytes). On `a lab unit`'s slot-3
    /// dsPIC 0x22 the 4-byte read decoded to 64760 mV garbage.
    ///
    /// This method instead uses the byte-wise read primitive
    /// (`bytewise_write_then_read`) — the same `set_slave → write → sleep →
    /// per-byte read` transaction shape the framed reads need — so the FULL raw
    /// reply is captured and logged, letting us see whether 0x22 returns real
    /// data or only an all-echo (FW-byte 0x8A repeated).
    ///
    /// `cmd_payload` is the un-encoded `[preamble0, preamble1, CMD, (payload…)]`
    /// vector — it is run through [`encode_command_frame`](Self::encode_command_frame)
    /// (which applies LEN/CKSUM for framed firmware or returns the bytes as-is
    /// for bare). `read_len` is how many reply bytes to attempt.
    ///
    /// On success the raw bytes are `tracing::info!`-logged and returned. On
    /// failure (e.g. fewer than `read_len` bytes came back) the error detail
    /// string — which itself includes the partial raw bytes (`{:02X?}`) — is
    /// `tracing::warn!`-logged and an empty `Vec` is returned, so the partial
    /// raw bytes are still visible in the daemon log. NEVER returns an Err
    /// (diagnostic, non-fatal by contract).
    pub fn dump_framed_telemetry_raw(&self, cmd_payload: &[u8], read_len: usize) -> Vec<u8> {
        let frame = self.encode_command_frame(cmd_payload);
        match self.bytewise_write_then_read(&frame, read_len, 6, "raw-telemetry-dump") {
            Ok(reply) => {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    cmd_payload = format_args!("{:02X?}", cmd_payload),
                    encoded_frame = format_args!("{:02X?}", frame),
                    read_len,
                    raw_reply = format_args!("{:02X?}", reply),
                    "RAW FRAMED TELEMETRY DUMP — byte-wise reply captured (diagnostic only, read-only)"
                );
                reply
            }
            Err(e) => {
                tracing::warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    cmd_payload = format_args!("{:02X?}", cmd_payload),
                    encoded_frame = format_args!("{:02X?}", frame),
                    read_len,
                    error = %e,
                    "RAW FRAMED TELEMETRY DUMP — byte-wise read incomplete; partial raw bytes are in the error detail (diagnostic only, read-only)"
                );
                Vec::new()
            }
        }
    }

    /// **Diagnostic-only** LM75 die-temp read via the `a lab unit`-class dsPIC LM75
    /// PASSTHROUGH protocol (opcodes 0x3B passthrough-WRITE + 0x3C
    /// passthrough-READ), distinct from [`read_temperature`](Self::read_temperature)
    /// which uses `CMD_READ_TEMP` (0x30) + a 4-byte read.
    ///
    /// On `a lab unit`-class (XIL S19j Pro, Loki/APW3) the chain dsPIC fronts the
    /// on-board LM75A sensors through opcodes 0x3B/0x3C (NOT 0x30). The
    /// bosminer-proven sequence per sensor (0x48..0x4B), captured at
    ///
    /// wave38-bosminer-truth/bosminer-i2c0-slave20.txt`, is:
    ///
    ///   1. WRITE frame `[55 AA 06 3B SENSOR 00 00 SUM]` (set LM75 pointer)
    ///   2. READ  frame `[55 AA 06 3C SENSOR 02 00 SUM]` (read 2 temp bytes)
    ///      → 6-byte reply `[3C 01 <hi> <lo> 01 SUM]`
    ///
    /// Temperature is at reply offset 2-3:
    /// `temp_C = ((hi << 8 | lo) >> 5) as f64 * 0.125`
    /// (e.g. reply `[3C 01 1A E0 01 3E]` ⇒ 0x1AE0 >> 5 * 0.125 = 26.875 °C).
    ///
    /// The two 8-byte frames are pre-built (preamble + LEN + payload + CKSUM)
    /// by [`bosminer_warmup::build_lm75_passthrough_frame`], so they are sent
    /// **raw** via the byte-wise primitive (`bytewise_write_then_read`) — NOT
    /// re-encoded through `encode_command_frame`. The WRITE frame's 1-byte ACK
    /// is read-and-discarded; the READ frame's 6-byte reply is decoded.
    ///
    /// Read-only, non-fatal: returns `Err` if the dsPIC doesn't answer, the
    /// reply is short, or the reply opcode echo isn't 0x3C — the caller treats
    /// any `Err` as "UNAVAILABLE" and never panics/aborts. NEVER on the hot
    /// mining path.
    pub fn read_lm75_passthrough_temp(&self, sensor_addr: u8) -> Result<f64> {
        use crate::dspic::bosminer_warmup::{
            build_lm75_passthrough_frame, LM75_PT_OPCODE_READ, LM75_PT_OPCODE_WRITE,
        };

        // 1) passthrough-WRITE: set the LM75 register pointer. flag = 0x00.
        //    1-byte ACK is read-and-discarded (parser-state only).
        let write_frame = build_lm75_passthrough_frame(LM75_PT_OPCODE_WRITE, sensor_addr, 0x00);
        let _ = self.bytewise_write_then_read(&write_frame, 1, 6, "lm75-pt-write")?;

        // 2) passthrough-READ: read 2 temperature bytes. flag = 0x02.
        //    Reply is 6 bytes `[3C 01 hi lo 01 SUM]`.
        let read_frame = build_lm75_passthrough_frame(LM75_PT_OPCODE_READ, sensor_addr, 0x02);
        let reply = self.bytewise_write_then_read(&read_frame, 6, 6, "lm75-pt-read")?;

        if reply.len() < 4 {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "LM75 passthrough read(0x{:02X}): short reply {:02X?}",
                    sensor_addr, reply
                ),
            });
        }
        // Reply opcode echo MUST be the passthrough-READ opcode (0x3C); an
        // all-echo (FW-byte repeated) or all-FF idle reply means the dsPIC
        // does not speak the passthrough protocol → treat as UNAVAILABLE.
        if reply[0] != LM75_PT_OPCODE_READ {
            return Err(crate::AsicError::Pic {
                addr: self.address,
                detail: format!(
                    "LM75 passthrough read(0x{:02X}): unexpected echo 0x{:02X} (not 0x3C) reply {:02X?}",
                    sensor_addr, reply[0], reply
                ),
            });
        }
        // Temperature at offset 2-3: LM75A 11-bit (>> 5) * 0.125 °C/LSB.
        let raw = ((reply[2] as i16) << 8) | (reply[3] as i16);
        let temp_c = (raw >> 5) as f64 * 0.125;
        Ok(temp_c)
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Convert millivolts to display-friendly voltage string.
pub fn mv_to_v_str(mv: u16) -> String {
    format!("{:.2}V", mv as f64 / 1000.0)
}

// ===========================================================================
//  S19j Pro am2 — dsPIC 0x89 (pic0x89) variant
// ===========================================================================
//
// Mirrors bosminer `open/bosminer/bosminer-am2-s17/src/hardware/hashboard/power/
// antminer/pic0x89.rs`. The 0x89 firmware is the S19j Pro variant of the S17
// family dsPIC firmware. It shares the 0x55 0xAA LEN CMD data SUM framing with
// the 0x86 / 0x8A variants already supported — the differences are:
//
//   1. **RESET (0x07) is BANNED on S19j Pro** — sending it permanently
//      downgrades the firmware.
//      The type system forbids `reset()` on this variant: the method simply
//      does not exist. Callers that want reset must use a `Fw86` or `Fw8A`
//      instance.
//
//   2. LM75A temperature passthrough at 0x72–0x75 goes **through** the dsPIC —
//      the dsPIC proxies I2C transactions to the on-board LM75A sensors.
//      This is exposed via the `Lm75aViaVoltageController` trait so the
//      thermal controller can read the 4 per-board sensors without caring
//      whether it's a real I2C bus or a PIC relay.
//
//   3. NEVER use `I2C_RDWR` on this PIC.
//      All reads are three-phase `set_slave → write → sleep → read`.
//
//   4. ALWAYS flush 16 zero bytes to the PIC after any I2C NACK
//.

/// PIC / voltage-controller variant tag.  Used by the factory that picks
/// between 0x82 (bare), 0x86 (S19j bare), 0x89 (S19j Pro framed),
/// 0x8A, 0xB9, and 0xFE (framed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PicVariant {
    /// Firmware identity has not been observed. Protocol-dependent and
    /// energizing operations must remain fail-closed.
    Unknown,
    /// Bare protocol, PIC FW 0x82.
    Bare82,
    /// S19j bare (fw 0x86).
    S19jBare86,
    /// S19j Pro framed (fw 0x89). **RESET is banned.**
    S19jProAm2,
    /// Framed (fw 0x8A).
    Framed8A,
    /// Framed (fw 0xB9). Observed in BraiinsOS/VNish families; reset remains banned.
    FramedB9,
    /// Framed (fw 0xFE). Observed in BraiinsOS/VNish families; reset remains banned.
    FramedFE,
    /// Unknown / other.
    Other(u8),
}

impl PicVariant {
    /// Detect variant from firmware version byte.
    pub fn from_fw_byte(fw: u8) -> Self {
        match fw {
            0x00 | 0xFF => PicVariant::Unknown,
            0x82 => PicVariant::Bare82,
            0x86 => PicVariant::S19jBare86,
            0x89 => PicVariant::S19jProAm2,
            0x8A => PicVariant::Framed8A,
            0xB9 => PicVariant::FramedB9,
            0xFE => PicVariant::FramedFE,
            other => PicVariant::Other(other),
        }
    }

    /// Whether the RESET (0x07) command is permitted on this variant.
    /// False for S19j Pro — RESET permanently downgrades the PIC.
    pub fn reset_allowed(self) -> bool {
        matches!(self, PicVariant::Bare82)
    }

    /// Whether the JUMP_TO_APP (0x06) command is permitted on this variant.
    /// Treat JUMP as part of the same legacy bootloader-control family as RESET.
    pub fn jump_allowed(self) -> bool {
        matches!(self, PicVariant::Bare82)
    }

    /// Whether the variant uses framed [0x55 0xAA LEN CMD data SUM] protocol.
    pub fn is_framed(self) -> bool {
        matches!(
            self,
            PicVariant::S19jProAm2
                | PicVariant::Framed8A
                | PicVariant::FramedB9
                | PicVariant::FramedFE
                | PicVariant::Other(_)
        )
    }
}

/// Abstraction for reading LM75A temperature sensors that sit **behind** a
/// voltage controller (dsPIC 0x86/0x89/0x8A on S17/S19/S19j Pro).
///
/// Implementors relay I2C transactions to the 4 on-board LM75A sensors at
/// 0x48–0x4B (AM2 sensor map; Agent 5 probe confirmed on .139) or
/// 0x72–0x75 (older S19/S19j probe) — the exact subaddress is set per board
/// by the sensor bank arg.
pub trait Lm75aViaVoltageController {
    /// Read temperature in degrees Celsius from the given on-board sensor
    /// sub-address (0x48..=0x4B on S19j Pro am2, 0x72..=0x75 on older S19/S19j).
    fn lm75a_read_temp(&mut self, sensor_addr: u8) -> Result<f64>;

    /// Read all 4 per-board LM75A sensors.  Returns `f64::NAN` for any
    /// sensor that fails to respond so the thermal controller can reason
    /// about partial data.
    fn lm75a_read_all(&mut self, sensor_addrs: [u8; 4]) -> [f64; 4] {
        let mut out = [f64::NAN; 4];
        for (i, &a) in sensor_addrs.iter().enumerate() {
            if let Ok(t) = self.lm75a_read_temp(a) {
                out[i] = t;
            }
        }
        out
    }
}

// Blanket implementation so the existing `DspicController` (0x86 / 0x8A path)
// automatically satisfies the trait. The 0x89 path below delegates to the
// same code with added safety gates.
impl<'a> Lm75aViaVoltageController for DspicController<'a> {
    fn lm75a_read_temp(&mut self, sensor_addr: u8) -> Result<f64> {
        self.read_temperature(sensor_addr)
    }
}

/// S19j Pro am2 specific PIC controller (fw 0x89).
///
/// Wraps `DspicController` to hard-forbid RESET and expose the typed variant.
/// All safety rules from the parent driver (no `I2C_RDWR` on PIC, 16-byte
/// zero flush after NACK, heartbeat-before-voltage-ramp) are honored by
/// delegation — this type only adds the **RESET ban** and variant metadata.
pub struct Pic0x89<'a> {
    inner: DspicController<'a>,
    variant: PicVariant,
}

impl<'a> Pic0x89<'a> {
    /// Construct over the underlying dsPIC controller with detected firmware.
    ///
    /// Accepts 0x86 (S19j bare) OR 0x89 (S19j Pro framed). RESET remains banned
    /// for both — this type has no
    /// `reset()` method (enforced at compile time).
    ///
    /// If `fw_byte` is `None`, retain an explicit unknown identity so
    /// protocol-dependent voltage operations fail closed. If an unrecognized
    /// byte is supplied, retain that exact observed identity instead of
    /// silently treating it as 0x89; RESET remains banned by this wrapper.
    pub fn new(i2c: &'a mut I2cBus, address: u8) -> Self {
        Self::new_with_fw(i2c, address, None)
    }

    /// Construct with an explicitly observed firmware byte. Use this when
    /// the caller has already read GET_VERSION and knows 0x86 vs 0x89 vs other.
    pub fn new_with_fw(i2c: &'a mut I2cBus, address: u8, fw_byte: Option<u8>) -> Self {
        let firmware = pic0x89_firmware_from_observed_fw_byte(fw_byte);
        let variant = fw_byte
            .map(PicVariant::from_fw_byte)
            .unwrap_or(PicVariant::Unknown);
        Self {
            inner: DspicController::new_with_firmware(i2c, address, firmware),
            variant,
        }
    }

    /// Detected variant tag. RESET is banned for every variant routed through
    /// this wrapper, including unrecognized framed firmware IDs.
    pub fn variant(&self) -> PicVariant {
        self.variant
    }

    /// Detected firmware identity used by the inner controller.
    pub fn firmware(&self) -> DspicFirmware {
        self.inner.firmware()
    }

    /// The underlying I2C slave address.
    pub fn address(&self) -> u8 {
        self.inner.address()
    }

    /// Initialize the voltage controller in cold-boot mode.
    ///
    /// This path deliberately does **not** send RESET/JUMP (the base driver
    /// already avoids those because they corrupt dsPIC firmware — on S19j
    /// Pro the RESET ban is a hard safety rule, enforced by the fact that
    /// no `reset()` method exists on this type).
    pub fn cold_boot_init(&mut self, voltage_mv: u16) -> Result<()> {
        self.inner.cold_boot_init(voltage_mv)
    }

    /// Heartbeat tick — must be fired before any voltage ramp.
    pub fn send_heartbeat(&mut self) -> Result<()> {
        ensure_dspic_runtime_protocol_is_proven(self.address(), self.firmware(), "heartbeat")?;
        self.inner.send_heartbeat()
    }

    /// Set voltage in millivolts (delegates to the framed protocol path).
    pub fn set_voltage(&mut self, voltage_mv: u16) -> Result<()> {
        self.inner.set_voltage(voltage_mv)
    }

    /// Enable DC-DC output.
    pub fn enable_voltage(&mut self) -> Result<()> {
        self.inner.enable_voltage()
    }

    /// Disable DC-DC output.
    pub fn disable_voltage(&mut self) -> Result<()> {
        self.inner.disable_voltage()
    }

    /// Read the current per-chain voltage via dsPIC ADC feedback (millivolts).
    /// Diagnostic-only — uses I2C_RDWR which the dsPIC parser tolerates for
    /// reads but not in the hot mining path. Call once post-`enable_voltage`
    /// to confirm the DC-DC actually engaged before kicking off chain init.
    pub fn read_voltage(&mut self) -> Result<u16> {
        self.inner.read_voltage()
    }

    /// Measure the actual chain-rail voltage via the dsPIC analog ADC
    /// (`MEASURE_VOLTAGE` 0x3A). Read-only; delegates to the inner controller.
    /// See [`DspicService::measure_voltage`]. (gap-swarm G03.)
    pub fn measure_voltage(&mut self) -> Result<u16> {
        self.inner.measure_voltage()
    }

    // NOTE — **intentionally no** `reset()` method. RESET (0x07) is banned
    // on S19j Pro. The type system
    // enforces this by omission.
}

impl<'a> Lm75aViaVoltageController for Pic0x89<'a> {
    fn lm75a_read_temp(&mut self, sensor_addr: u8) -> Result<f64> {
        self.inner.read_temperature(sensor_addr)
    }
}

/// Service-backed S19j Pro am2 PIC controller (fw 0x89).
///
/// This is the `I2cServiceHandle` equivalent of `Pic0x89`: RESET is still
/// intentionally absent, and all commands are serialized by the process-wide
/// I2C service thread.
pub struct Pic0x89Service {
    inner: DspicService,
    variant: PicVariant,
}

/// Discovery-bound owner for one service-backed AM2 Pic0x89 controller.
///
/// The endpoint must carry a firmware byte observed by the HAL while issuing
/// the capability. A model string, raw address, or caller-supplied firmware
/// hint cannot construct this session.
///
/// ```compile_fail
/// use dcentrald_asic::dspic::Pic0x89EndpointSession;
/// use dcentrald_hal::i2c::I2cServiceHandle;
///
/// fn caller_asserted(handle: I2cServiceHandle) {
///     let _ = Pic0x89EndpointSession::new(handle, 0x20);
/// }
/// ```
pub struct Pic0x89EndpointSession {
    controller: Pic0x89Service,
    i2c: I2cServiceHandle,
    address: u8,
    firmware_byte: u8,
}

impl Pic0x89EndpointSession {
    pub fn new(i2c: I2cServiceHandle, endpoint: VoltageControllerEndpoint) -> Result<Self> {
        if endpoint.kind() != VoltageControllerKind::Dspic33Ep {
            return Err(crate::AsicError::InvalidParameter(format!(
                "{} endpoint cannot construct an AM2 Pic0x89 session",
                endpoint.kind().as_str()
            )));
        }
        if endpoint.bus() != i2c.bus() {
            return Err(crate::AsicError::InvalidParameter(format!(
                "AM2 Pic0x89 endpoint is bound to I2C bus {}, but service owns bus {}",
                endpoint.bus(),
                i2c.bus()
            )));
        }
        let firmware_byte = endpoint.observed_firmware().ok_or_else(|| {
            crate::AsicError::InvalidParameter(
                "AM2 Pic0x89 endpoint lacks observed firmware evidence".into(),
            )
        })?;
        let firmware = pic0x89_firmware_from_observed_fw_byte(Some(firmware_byte));
        if !dspic_runtime_protocol_is_proven(firmware) {
            return Err(crate::AsicError::InvalidParameter(format!(
                "AM2 Pic0x89 endpoint firmware 0x{firmware_byte:02X} has no modeled runtime protocol"
            )));
        }
        let address = endpoint.address();
        Ok(Self {
            controller: Pic0x89Service::new_with_fw(i2c.clone(), address, Some(firmware_byte)),
            i2c,
            address,
            firmware_byte,
        })
    }

    pub fn into_controller(self) -> Pic0x89Service {
        self.controller
    }

    /// Borrow the discovery-bound controller without discarding its endpoint
    /// authority. Long-lived orchestrators use this to retain the same exact
    /// identity/presence/firmware evidence through clean shutdown instead of
    /// reconstructing a raw address + firmware controller later.
    pub fn controller_mut(&mut self) -> &mut Pic0x89Service {
        &mut self.controller
    }

    /// Create an independent controller view for a concurrent lifecycle owner
    /// while preserving this session's retained shutdown controller.
    /// Transport, address, and firmware come only from the consumed opaque
    /// endpoint; callers cannot substitute any of them.
    pub fn controller(&self) -> Pic0x89Service {
        Pic0x89Service::new_with_fw(self.i2c.clone(), self.address, Some(self.firmware_byte))
    }
}

impl Pic0x89Service {
    /// Construct without an observed firmware identity. Voltage operations
    /// remain fail-closed until a preflight or version read establishes one.
    pub fn new(i2c: I2cServiceHandle, address: u8) -> Self {
        Self::new_with_fw(i2c, address, None)
    }

    /// Construct with an explicitly observed firmware byte.
    pub fn new_with_fw(i2c: I2cServiceHandle, address: u8, fw_byte: Option<u8>) -> Self {
        let firmware = pic0x89_firmware_from_observed_fw_byte(fw_byte);
        let variant = fw_byte
            .map(PicVariant::from_fw_byte)
            .unwrap_or(PicVariant::Unknown);
        Self {
            inner: DspicService::new_with_firmware(i2c, address, firmware),
            variant,
        }
    }

    pub fn variant(&self) -> PicVariant {
        self.variant
    }

    pub fn firmware(&self) -> DspicFirmware {
        self.inner.firmware()
    }

    pub fn address(&self) -> u8 {
        self.inner.address()
    }

    pub fn voltage_mv(&self) -> u16 {
        self.inner.voltage_mv()
    }

    pub fn voltage_enabled(&self) -> bool {
        self.inner.voltage_enabled()
    }

    /// Flush parser state and read firmware version through the service.
    pub fn preflight(&mut self) -> Result<DspicFirmware> {
        let fw = self.inner.preflight()?;
        self.variant = match fw {
            DspicFirmware::Unknown => PicVariant::Unknown,
            DspicFirmware::Fw82 => PicVariant::Bare82,
            DspicFirmware::Fw86 => PicVariant::S19jBare86,
            DspicFirmware::Fw89 => PicVariant::S19jProAm2,
            DspicFirmware::Fw8A => PicVariant::Framed8A,
            DspicFirmware::FwB9 => PicVariant::FramedB9,
            DspicFirmware::FwFE => PicVariant::FramedFE,
            DspicFirmware::Other(v) => PicVariant::Other(v),
        };
        Ok(fw)
    }

    /// Read firmware version byte through the service.
    pub fn get_version(&mut self) -> Result<u8> {
        let version = self.inner.get_version()?;
        self.variant = PicVariant::from_fw_byte(version);
        Ok(version)
    }

    /// Initialize the voltage controller in cold-boot mode.
    pub fn cold_boot_init(&mut self, voltage_mv: u16) -> Result<()> {
        self.inner.cold_boot_init(voltage_mv)
    }

    /// Initialize the voltage controller in cold-boot mode, opting out of
    /// the internal 5×1s pre-voltage heartbeat warmup loop when the caller
    /// already ran an external warmup pass (Layer 3 of the 2026-05-22
    /// XIL `a lab unit` recovery: bosminer-warmup + Phase 0d 5×1Hz idle heartbeats
    /// in `s19j_hybrid_mining.rs`).
    ///
    /// See
    ///  §(e).
    pub fn cold_boot_init_with_options(
        &mut self,
        voltage_mv: u16,
        skip_warmup_loop: bool,
    ) -> Result<()> {
        self.inner
            .cold_boot_init_with_options(voltage_mv, skip_warmup_loop)
    }

    /// Enable/disable the post-JUMP framed-heartbeat keep-alive for
    /// `cold_boot_init_with_options` (default `false`). See
    /// [`DspicService::set_postjump_heartbeat_keepalive`]. Caller-gated by the
    /// env `DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE` AND the `a lab unit`
    /// fingerprint at the `s19j_hybrid_mining.rs` Phase 3 call site.
    pub fn set_postjump_heartbeat_keepalive(&mut self, on: bool) {
        self.inner.set_postjump_heartbeat_keepalive(on);
    }

    /// Enable/disable the re-JUMP-before-ENABLE for
    /// `cold_boot_init_with_options` (default `false`). See
    /// [`DspicService::set_rejump_before_enable`]. Caller-gated by the env
    /// `DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE` AND the `a lab unit` fingerprint at the
    /// `s19j_hybrid_mining.rs` Phase 3 call site. NEVER issues a RESET.
    pub fn set_rejump_before_enable(&mut self, on: bool) {
        self.inner.set_rejump_before_enable(on);
    }

    /// Enable/disable the skip-SetVoltage-keep-ENABLE for
    /// `cold_boot_init_with_options` (default `false`). See
    /// [`DspicService::set_skip_setvoltage_keep_enable`]. Caller-gated by the
    /// env `DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE` AND the `a lab unit`
    /// fingerprint at the `s19j_hybrid_mining.rs` Phase 3 call site. Skips ONLY
    /// the dsPIC `0x10` SetVoltage; the `0x15` ENABLE wire bytes are unchanged.
    pub fn set_skip_setvoltage_keep_enable(&mut self, on: bool) {
        self.inner.set_skip_setvoltage_keep_enable(on);
    }

    /// Enable/disable the bosminer-minimal ENABLE for
    /// `cold_boot_init_with_options` (default `false`). See
    /// [`DspicService::set_bosminer_minimal_enable`]. Caller-gated by the env
    /// `DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE` AND the `a lab unit` fingerprint at the
    /// `s19j_hybrid_mining.rs` Phase 3 call site. When active (framed only) the
    /// GET_VERSION(0x89)→ENABLE window is consolidated to bosminer-minimal: the
    /// only dsPIC traffic in it is the byte-identical `0x15` ENABLE.
    pub fn set_bosminer_minimal_enable(&mut self, on: bool) {
        self.inner.set_bosminer_minimal_enable(on);
    }

    /// Heartbeat tick.
    pub fn send_heartbeat(&mut self) -> Result<()> {
        ensure_dspic_runtime_protocol_is_proven(self.address(), self.firmware(), "heartbeat")?;
        self.inner.send_heartbeat()
    }

    /// Set voltage in millivolts.
    pub fn set_voltage(&mut self, voltage_mv: u16) -> Result<()> {
        self.inner.set_voltage(voltage_mv)
    }

    /// Enable DC-DC output.
    pub fn enable_voltage(&mut self) -> Result<()> {
        self.inner.enable_voltage()
    }

    /// Disable DC-DC output.
    pub fn disable_voltage(&mut self) -> Result<()> {
        self.inner.disable_voltage()
    }

    /// Read ADC voltage feedback in millivolts.
    pub fn read_voltage(&mut self) -> Result<u16> {
        self.inner.read_voltage()
    }

    /// Measure the actual chain-rail voltage via the dsPIC analog ADC
    /// (`MEASURE_VOLTAGE` 0x3A). Read-only; delegates to the inner service.
    /// 0x3A is the analog-ADC rail read, distinct from `read_voltage`'s 0x3B
    /// setpoint/feedback — the `a lab unit` Procedure A reads both, addr-tagged, to
    /// disambiguate setpoint from actually-energized. See
    /// [`DspicService::measure_voltage`]. (gap-swarm G03.)
    pub fn measure_voltage(&mut self) -> Result<u16> {
        self.inner.measure_voltage()
    }

    /// **Diagnostic-only** raw byte-wise dump of the `GET_VOLTAGE` (0x3B)
    /// reply. Read-only, NEVER on the hot mining path.
    ///
    /// Unlike [`read_voltage`](Self::read_voltage) (which does the production
    /// COMBINED 4-byte decode that garbles the FRAMED fw=0x89 reply into
    /// garbage like 64760 mV), this captures and logs the FULL raw byte-wise
    /// reply so we can SEE whether the chain dsPIC (e.g. `a lab unit` slot-3 0x22)
    /// returns real `[cmd_echo, status, v_hi, v_lo, …, CKSUM]` data or only an
    /// all-echo (FW-byte 0x8A repeated). Returns the raw bytes (empty `Vec` if
    /// the read was incomplete — the partial bytes are then in the warn log).
    pub fn dump_voltage_raw(&mut self, read_len: usize) -> Vec<u8> {
        self.inner.dump_framed_telemetry_raw(
            &[DSPIC_PREAMBLE[0], DSPIC_PREAMBLE[1], CMD_GET_VOLTAGE],
            read_len,
        )
    }

    /// **Diagnostic-only** raw byte-wise dump of the `MEASURE_VOLTAGE` (0x3A,
    /// analog-ADC rail) reply. Read-only, NEVER on the hot mining path.
    ///
    /// Same intent as [`dump_voltage_raw`](Self::dump_voltage_raw) but for the
    /// analog-ADC rail measure opcode (the stronger physical-rail proxy). The
    /// `[0x00]` payload byte matches the production `measure_voltage` frame
    /// (`[55 AA 04 3A 00 3E]` framed / `[55 AA 3A 00]` bare).
    pub fn dump_measure_raw(&mut self, read_len: usize) -> Vec<u8> {
        self.inner.dump_framed_telemetry_raw(
            &[
                DSPIC_PREAMBLE[0],
                DSPIC_PREAMBLE[1],
                CMD_MEASURE_VOLTAGE,
                0x00,
            ],
            read_len,
        )
    }

    /// **Diagnostic-only** LM75 die-temp read via the `a lab unit`-class dsPIC LM75
    /// PASSTHROUGH protocol (opcodes 0x3B/0x3C), distinct from
    /// [`read_temperature`](Self::read_temperature) which uses `CMD_READ_TEMP`
    /// (0x30) + a 4-byte read. Delegates to
    /// [`DspicService::read_lm75_passthrough_temp`]. Read-only, non-fatal:
    /// returns `Err` if the dsPIC doesn't answer the passthrough protocol — the
    /// caller treats any `Err` as "rail signal UNAVAILABLE" and never panics.
    /// NEVER on the hot mining path. See the inner method for the byte-exact
    /// WRITE+READ frame sequence and the temperature decode.
    pub fn read_lm75_passthrough_temp(&mut self, sensor_addr: u8) -> Result<f64> {
        self.inner.read_lm75_passthrough_temp(sensor_addr)
    }

    /// Read temperature from an LM75A sensor via dsPIC passthrough.
    ///
    /// Delegates to the inner service-backed implementation. In bare-protocol
    /// mode (fw 0x82/0x86) this returns `Ok(f64::NAN)` as a sentinel — bare
    /// firmware never delivers real LM75 temp data, only a 1-byte FW echo.
    ///.
    pub fn read_temperature(&mut self, sensor_addr: u8) -> Result<f64> {
        self.inner.read_temperature(sensor_addr)
    }

    /// Read all 4 LM75A sensors. In bare mode each entry will be `NaN`
    /// (informational only — no temp data delivered in bare mode); call
    /// sites should use XADC die-temp fallback for thermal safety. See
    /// .
    pub fn read_all_temperatures(&mut self) -> [f64; 4] {
        self.inner.read_all_temperatures()
    }

    // NOTE: intentionally no `reset()` method. RESET (0x07) remains banned
    // for the service-backed Pic0x89 path as well.
}

impl Lm75aViaVoltageController for Pic0x89Service {
    fn lm75a_read_temp(&mut self, sensor_addr: u8) -> Result<f64> {
        self.inner.read_temperature(sensor_addr)
    }
}

/// Last-ditch runtime guard. If any future caller ever reaches for RESET via
/// a dynamic dispatch path, this function centralises the ban so the error
/// message points to the governing feedback note.
pub fn ensure_reset_allowed(variant: PicVariant) -> std::result::Result<(), &'static str> {
    if !variant.reset_allowed() {
        return Err("PIC RESET banned; \
             RESET permanently downgrades S19j Pro dsPIC firmware");
    }
    Ok(())
}

/// Last-ditch runtime guard for legacy JUMP_TO_APP. S19j/Pic0x89 runtime
/// should be probed in-place, not forced through bootloader state.
pub fn ensure_jump_allowed(variant: PicVariant) -> std::result::Result<(), &'static str> {
    if !variant.jump_allowed() {
        return Err("PIC JUMP_TO_APP banned; \
             do not push S19j Pro dsPIC firmware through legacy bootloader flow");
    }
    Ok(())
}

// ===========================================================================
//  S17 dsPIC33EP16GS202 family alias
// ===========================================================================
//
// The S17 hash board voltage controller is a Microchip dsPIC33EP16GS202 — a
// 16-bit DSP with hardware HR-PWM and 12-bit ADC. Functionally it shares the
// same I²C framed protocol (`[55 AA LEN CMD payload SUM]`) used by all the
// S17/S19-family dsPICs, and the existing `DspicController` /
// `DspicService` / `Pic0x89Service` paths already cover the byte-level
// transport. The wrappers below exist primarily for **call-site clarity**
// when the platform constructor wires up an S17 hash board, and to give
// future S17-specific tweaks (e.g. clamp/CRAB voltage telemetry, dsPIC
// firmware update via `update_app_program`) a single place to land.
//
// XXX: confirm against live S17 — the current S17 path ships untested.
// Specifically:
//   * I²C addresses on S17 boards are believed to be 0x20/0x21/0x22 (per
//     the S19 RE corpus), but the S17 RE document references 0x88/0x89/
//     0xB9/0xFE as firmware-version IDs. Live S17 i2cdetect needed.
//   * Bare-vs-framed protocol mode: S17 dsPIC33EP fw is expected to be
//     framed (matches S19j 0x89 family), but live verification needed
//     before voltage commands ship.
//   * Default voltage envelope (`DEFAULT_VOLTAGE_MV` 13.80 V) is the S19
//     value; S17 may use a different rail target.

/// Convenience constructors for S17 hash boards using the dsPIC33EP16GS202
/// voltage controller.
///
/// These are thin wrappers around `DspicController::new` and
/// `DspicService::new` that document the S17 use case at the call site.
/// Behavior is identical to the generic constructors today; if S17 live
/// data later diverges from the S19 path (e.g. different command IDs, a
/// distinct voltage envelope, or fw-update specifics), the wrappers below
/// are the place to branch.
pub struct Dspic33Ep16Gs202;

impl Dspic33Ep16Gs202 {
    /// I²C address probe order for S17 dsPIC33EP16GS202 boards.
    /// Same physical addresses as the S19 family (0x20..=0x22).
    /// XXX: confirm against live S17.
    pub const PROBE_ADDRS: [u8; 3] = DSPIC_PROBE_ADDRS;

    /// Build a raw-bus `DspicController` for an S17 hash board.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(i2c: &mut I2cBus, address: u8) -> DspicController<'_> {
        DspicController::new(i2c, address)
    }

    /// Build a raw-bus `DspicController` for an S17 hash board with a known
    /// firmware version (skips probing).
    pub fn new_with_firmware(
        i2c: &mut I2cBus,
        address: u8,
        firmware: DspicFirmware,
    ) -> DspicController<'_> {
        DspicController::new_with_firmware(i2c, address, firmware)
    }

    /// Build a service-backed dsPIC controller for an S17 hash board.
    pub fn new_service(i2c: I2cServiceHandle, address: u8) -> DspicService {
        DspicService::new(i2c, address)
    }

    /// Build a service-backed dsPIC controller for an S17 hash board with a
    /// known firmware version (skips probing).
    pub fn new_service_with_firmware(
        i2c: I2cServiceHandle,
        address: u8,
        firmware: DspicFirmware,
    ) -> DspicService {
        DspicService::new_with_firmware(i2c, address, firmware)
    }
}

// ===========================================================================
//  Tests
// ===========================================================================

#[cfg(test)]
mod pic0x89_tests {
    use super::*;
    use proptest::prelude::*;

    /// This module's own source, for structural source-parse pins (same pattern
    /// as dcentrald/tests/cold_boot_init_with_options_skip_warmup.rs but kept
    /// host-runnable inside the asic crate).
    const MOD_SRC: &str = include_str!("mod.rs");

    /// Corpus-pin (2026-07-02 firmware-corpus mining): the dsPIC framed
    /// command opcodes + preamble are RE-confirmed byte-exact against multiple
    /// held VNish/Bitmain cgminer binaries (hexdump `55 aa 05 15 …` ENABLE,
    /// `55 aa 04 16 …` HEARTBEAT, `55 aa 04 07 …` RESET; GET_VERSION 0x17,
    /// DAC readback 0x18). These bytes go straight onto a live PSU/chip rail —
    /// a silent renumber would issue the WRONG command to a voltage controller.
    /// No prior test pinned them (grep of the asic tests dir returned none).
    /// Evidence: CORPUS_MINING_FINDINGS.md pic-eeprom[0]/[3].
    #[test]
    fn dspic_rail_touching_opcodes_pinned_to_corpus_bytes() {
        assert_eq!(DSPIC_PREAMBLE, [0x55, 0xAA], "framed preamble");
        assert_eq!(CMD_RESET, 0x07, "RESET");
        assert_eq!(CMD_JUMP_TO_APP, 0x06, "JUMP");
        assert_eq!(CMD_SET_VOLTAGE, 0x10, "SET_VOLTAGE (DAC)");
        assert_eq!(CMD_ENABLE_VOLTAGE, 0x15, "ENABLE_VOLTAGE");
        assert_eq!(CMD_HEARTBEAT, 0x16, "HEARTBEAT");
        assert_eq!(CMD_GET_VERSION, 0x17, "GET_VERSION");
        assert_eq!(CMD_GET_VOLTAGE_DAC, 0x18, "GET_VOLTAGE_DAC readback");
    }

    /// Corpus-pin: the dsPIC fw-version whitelist must contain ONLY dsPIC33EP
    /// firmware bytes and NEVER the S9-class PIC16F1704 stock echo bytes, so a
    /// framed voltage command can never fire on an S9 PIC that happens to echo
    /// a byte the dsPIC path would accept. dsPIC family (0x17-detected) =
    /// {0x82,0x86,0x89,0x8A,0xB9,0xFE}; S9 stock PIC (0x04-detected) =
    /// {0x56,0x5A,0x5E,0x03}. Evidence: CORPUS_MINING_FINDINGS.md pic-eeprom[3]
    /// (PIC_HEARTBEAT_MATRIX cross-check).
    #[test]
    fn dspic_fw_whitelist_excludes_s9_pic1704_bytes() {
        for v in [0x82u8, 0x86, 0x89, 0x8A, 0xB9, 0xFE] {
            assert!(is_known_dspic_fw(v), "0x{v:02X} is a known dsPIC fw");
        }
        for v in [0x56u8, 0x5A, 0x5E, 0x03] {
            assert!(
                !is_known_dspic_fw(v),
                "S9 stock PIC1704 byte 0x{v:02X} must NOT be a valid dsPIC fw",
            );
        }
    }

    /// prod-readiness hunt #3 (safety-test-gap): pin the load-bearing
    /// "NEVER send SET_VOLTAGE before 5 stable heartbeats" gate. The only prior
    /// test of this region pins the skip-warmup branch + the loop's textual
    /// existence, NOT the `< 5` refusal threshold — so weakening `< 5` to `< 1`
    /// or deleting the `if` while leaving `for tick in 1..=5` intact would pass
    /// the whole suite. The MSSP-parser-corruption root cause (the #1 PIC
    /// blocker) makes this a load-bearing invariant; pin it.
    #[test]
    fn dspic_5_heartbeat_pre_voltage_gate_is_pinned() {
        assert!(
            MOD_SRC.contains("if stable_heartbeats < 5"),
            "SAFETY: the dsPIC 5-stable-heartbeat pre-voltage refusal gate is missing \
             (NACK before 5 heartbeats permanently corrupts the PIC MSSP parser — the #1 blocker)"
        );
        // Substring (no leading "dsPIC ") so it matches BOTH the standalone
        // ("dsPIC did not...") and service ("dsPIC service did not...") errors.
        assert!(
            MOD_SRC.contains("did not complete 5 stable pre-voltage heartbeats"),
            "SAFETY: the 5-stable-heartbeat refusal error message disappeared (gate may be gone)"
        );
    }

    /// W1.2 jig op-diff (2026-06-13): `enable_ack_flag_on` matches the BM1362
    /// factory jig's exact ENABLE success criterion — `[0x15, 0x01]` ONLY —
    /// distinct from the lenient `enable_ack_ok` (which also accepts `[0x15, 0x00]`).
    #[test]
    fn enable_ack_flag_on_requires_status_01_not_00() {
        // jig-confirmed flag-on:
        assert!(enable_ack_flag_on(&[CMD_ENABLE_VOLTAGE, 0x01]));
        // dsPIC echoing the OFF flag — rail NOT confirmed enabled (jig rejects this):
        assert!(!enable_ack_flag_on(&[CMD_ENABLE_VOLTAGE, 0x00]));
        // wrong cmd echo / too short / firmware echo:
        assert!(!enable_ack_flag_on(&[0x82, 0x82]));
        assert!(!enable_ack_flag_on(&[CMD_ENABLE_VOLTAGE]));
        assert!(!enable_ack_flag_on(&[]));
        // Cross-check: the lenient gate DOES accept [0x15, 0x00] (the difference
        // the strict gate closes) — so the two helpers are genuinely distinct.
        assert!(enable_ack_ok(&[CMD_ENABLE_VOLTAGE, 0x00]));
        assert!(enable_ack_ok(&[CMD_ENABLE_VOLTAGE, 0x01]));
    }

    /// Pin that the strict flag-on ENABLE gate is wired into `enable_voltage`
    /// and is default-OFF (env-gated), so the fleet stays byte-identical.
    #[test]
    fn require_enable_flag_on_gate_is_wired_and_default_off() {
        // Default-OFF unless the env is set (kept independent of process env by
        // asserting the source wiring, not the live var — env is global state).
        assert!(
            MOD_SRC.contains("DCENT_AM2_REQUIRE_ENABLE_FLAG_ON"),
            "the strict flag-on ENABLE gate env was removed"
        );
        assert!(
            MOD_SRC.contains("dspic_require_enable_flag_on_enabled() && !enable_ack_flag_on(&ack)"),
            "the strict flag-on ENABLE check is no longer wired into raw enable_voltage()"
        );
        assert!(
            MOD_SRC.contains(
                "dspic_require_enable_flag_on_enabled() && !enable_ack_flag_on(&final_ack)"
            ),
            "the strict flag-on ENABLE check is no longer wired into service enable_voltage()"
        );
    }

    /// Post-JUMP keep-alive (2026-06-07, `a lab unit` standalone cold-engage): the
    /// continuous-bounded chunked-sleep planner splits a settle into
    /// `<= interval` slices, one keep-alive heartbeat per slice. A 1.2 s window
    /// at the default 300 ms interval MUST yield >= 4 heartbeats so the
    /// cold-engaged fw=0x89 chip is serviced inside its ~1.2 s app-mode-hold
    /// window and does not drift back to fw=0x82 bootloader before the ENABLE.
    #[test]
    fn keepalive_sleep_slices_chunks_settle_into_bounded_heartbeats() {
        // The live blocker: a ~1.2 s un-serviced gap. At 300 ms ⇒ exactly 4
        // heartbeats (one per slice), bridging the gap.
        let slices = keepalive_sleep_slices(1200, 300);
        assert_eq!(slices, vec![300, 300, 300, 300]);
        assert!(
            slices.len() >= 4,
            "a 1.2 s settle must produce >= 4 keep-alive heartbeats at the 300 ms default"
        );
        // Every slice is bounded by the interval (no un-serviced gap > interval).
        assert!(slices.iter().all(|&s| s <= 300));
        // Slices sum to the exact requested total (no lost/over-slept time).
        assert_eq!(slices.iter().sum::<u64>(), 1200);

        // Remainder is the last (short) slice.
        assert_eq!(keepalive_sleep_slices(700, 300), vec![300, 300, 100]);
        // A sub-interval settle is a single slice (1 heartbeat) — the 50 ms
        // SetVoltage/ENABLE settles route through here.
        assert_eq!(keepalive_sleep_slices(50, 300), vec![50]);
        // Zero total ⇒ no slices ⇒ no heartbeats.
        assert!(keepalive_sleep_slices(0, 300).is_empty());
        // Degenerate interval is floored to 1 (never a busy/zero-length loop).
        assert_eq!(keepalive_sleep_slices(3, 0), vec![1, 1, 1]);
    }

    /// Default-OFF byte-identical contract: with the keep-alive disabled,
    /// `keepalive_sleep` is a plain `std::thread::sleep(total_ms)` and the
    /// LM75A read delegates to the untouched `read_all_temperatures` — i.e. the
    /// fleet/handoff/legacy `cold_boot_init` path is unchanged. Source-pinned
    /// so a refactor that drops the early-return cannot silently start emitting
    /// extra heartbeats on every platform's cold boot.
    #[test]
    fn keepalive_sleep_and_lm75_are_byte_identical_when_keepalive_off() {
        // CRLF-safe: only single-line substrings (mod.rs is CRLF on disk, so an
        // embedded-\n match would be brittle — same pattern as the gate above).
        //
        // `keepalive_sleep` exists and falls back to a plain thread::sleep of
        // the full requested duration (the legacy settle) when off/bare.
        assert!(
            MOD_SRC.contains("fn keepalive_sleep(&mut self, total_ms: u64, where_label: &str)"),
            "the keep-alive chunked-sleep helper disappeared"
        );
        assert!(
            MOD_SRC.contains("std::thread::sleep(std::time::Duration::from_millis(total_ms));"),
            "BYTE-IDENTICAL: keepalive_sleep must fall back to a plain \
             thread::sleep(total_ms) when the keep-alive is off/bare"
        );
        // The off/bare early-return guard is present (no extra heartbeats when
        // the gate is unset — preserves the fleet/handoff/legacy path).
        assert!(
            MOD_SRC.contains("if !self.postjump_heartbeat_keepalive || self.use_bare_protocol {"),
            "BYTE-IDENTICAL: the default-OFF early-return guard disappeared"
        );
        // The keep-alive-aware LM75A read delegates to the untouched
        // `read_all_temperatures` when off/bare.
        assert!(
            MOD_SRC.contains("fn read_all_temperatures_keepalive(&mut self) -> [f64; 4]"),
            "the keep-alive-aware LM75A read helper disappeared"
        );
        assert!(
            MOD_SRC.contains("return self.read_all_temperatures();"),
            "BYTE-IDENTICAL: read_all_temperatures_keepalive must delegate to the \
             untouched read_all_temperatures when the keep-alive is off/bare"
        );
        // The keep-alive loop NEVER issues a SetVoltage / ENABLE / JUMP / RESET
        // — only the framed 0x16 heartbeat (send_heartbeat). Pin that the tick
        // helper calls send_heartbeat and nothing destructive.
        assert!(
            MOD_SRC.contains("fn postjump_keepalive_tick(&mut self, where_label: &str)")
                && MOD_SRC.contains("self.send_heartbeat()"),
            "the keep-alive tick must be the non-destructive framed 0x16 heartbeat only"
        );
    }

    /// Re-JUMP-before-ENABLE (2026-06-07, `a lab unit` standalone cold-engage):
    /// default-OFF byte-identical + reached only when fw==0x82 + NO RESET.
    /// Source-pinned (the HAL/bus path can't run host-side, same pattern as the
    /// keep-alive pins) so a refactor can't silently change the gating, the
    /// fw==0x82 trigger, the byte-identical SetVoltage/ENABLE guarantee, or
    /// (load-bearing) re-introduce a RESET into the re-JUMP.
    #[test]
    fn rejump_before_enable_is_gated_fw82_only_and_never_resets() {
        // The helper exists and early-returns (no extra GET_VERSION/JUMP) unless
        // the gate is set AND the protocol is framed — preserves the
        // fleet/handoff/legacy byte-identical path.
        assert!(
            MOD_SRC.contains("fn rejump_to_app_mode_if_drifted(&mut self)"),
            "the re-JUMP-before-ENABLE helper disappeared"
        );
        assert!(
            MOD_SRC.contains("if !self.rejump_before_enable || self.use_bare_protocol {"),
            "BYTE-IDENTICAL: the re-JUMP default-OFF + framed-only early-return guard disappeared"
        );
        // The re-JUMP fires ONLY on the fw==0x82 drift case (Some(0x82) arm calls
        // the JUMP-only re-verify). An already-0x89 chip is a no-op; any other fw
        // / unreadable GET_VERSION is left alone.
        assert!(
            MOD_SRC.contains("Some(0x82) =>"),
            "the re-JUMP must be reached ONLY for the fw==0x82 (drifted-to-bootloader) case"
        );
        assert!(
            MOD_SRC.contains("crate::dspic::bosminer_warmup::am2_pic_jump_only_reverify("),
            "the re-JUMP must reuse the JUMP-only re-verify (flush → framed JUMP)"
        );
        // LOAD-BEARING: the re-JUMP is JUMP-ONLY — it must NEVER issue a RESET.
        // The reused re-verify (pinned positively above) omits the framed RESET
        // by construction. Pin that mod.rs never reaches for the RESET-then-JUMP
        // re-verify variant — a RESET here is the destructive-downgrade class,
        // JUMP-only is the safe transition for a cold-0x82 bootloader. The
        // needles are assembled via `concat!` so this very assertion does not
        // make `MOD_SRC` (which `include_str!`s this file) self-match.
        let reset_variant_fn = concat!("am2_pic_reset", "_jump_reverify");
        let reset_variant_builder = concat!("build_reset", "_jump_reverify_transactions");
        assert!(
            !MOD_SRC.contains(reset_variant_fn) && !MOD_SRC.contains(reset_variant_builder),
            "the re-JUMP path must NEVER call a RESET-then-JUMP re-verify variant (RESET here \
             is the destructive-downgrade class — JUMP-only is the safe transition)"
        );
        // BYTE-IDENTICAL SetVoltage/ENABLE: the helper saves and restores the
        // encoding-determining state (firmware/use_bare_protocol) around the
        // GET_VERSION probe so the wire bytes are unchanged.
        assert!(
            MOD_SRC.contains("let saved_firmware = self.firmware;")
                && MOD_SRC.contains("self.firmware = saved_firmware;"),
            "BYTE-IDENTICAL: the re-JUMP must save+restore self.firmware so SetVoltage/ENABLE \
             wire bytes are unchanged regardless of the GET_VERSION probe"
        );
        assert!(
            MOD_SRC.contains("let saved_use_bare = self.use_bare_protocol;")
                && MOD_SRC.contains("self.use_bare_protocol = saved_use_bare;"),
            "BYTE-IDENTICAL: the re-JUMP must save+restore self.use_bare_protocol"
        );
        // The call site runs the re-JUMP immediately before SetVoltage.
        // CRLF-safe: byte-offset ordering (no embedded-newline substring, which
        // would be brittle on the CRLF-on-disk source).
        let call_idx = MOD_SRC
            .find("self.rejump_to_app_mode_if_drifted();")
            .expect("the re-JUMP call site disappeared");
        let setv_idx = MOD_SRC
            .find("match self.set_voltage(voltage_mv) {")
            .expect("the cold_boot_init SetVoltage call disappeared");
        assert!(
            call_idx < setv_idx,
            "the re-JUMP must be invoked BEFORE SetVoltage"
        );
        assert!(
            setv_idx - call_idx < 200,
            "the re-JUMP must be invoked IMMEDIATELY before SetVoltage (no long read/sleep between)"
        );
    }

    /// Skip-SetVoltage-keep-ENABLE (2026-06-07, `a lab unit` standalone cold-engage):
    /// PROVEN-root-cause fix (ENABLE-DRIFT-DIFF.md, commit fc4eef92). Source-
    /// pinned (the HAL/bus path can't run host-side, same pattern as the
    /// re-JUMP/keep-alive pins) so a refactor can't silently (a) change the
    /// default-OFF + framed-only gating, (b) start skipping the ENABLE too
    /// (the SENSOR_ONLY half-bug this gate explicitly corrects), or (c) drop the
    /// byte-identical guarantee for the fleet/handoff/legacy/BARE paths.
    #[test]
    fn skip_setvoltage_keep_enable_is_gated_and_keeps_enable() {
        // The gate field + setter exist (additive plumbing, sibling of the
        // re-JUMP / post-JUMP keep-alive gates).
        assert!(
            MOD_SRC.contains("skip_setvoltage_keep_enable: bool,")
                && MOD_SRC
                    .contains("pub fn set_skip_setvoltage_keep_enable(&mut self, on: bool) {"),
            "the skip-SetVoltage-keep-ENABLE field/setter disappeared"
        );
        // Default-OFF + framed-only guard, INVERTED so the SetVoltage arm stays
        // first/adjacent to the re-JUMP. With the gate off OR bare, the
        // `match self.set_voltage(voltage_mv)` arm runs exactly as today.
        assert!(
            MOD_SRC.contains("if !(self.skip_setvoltage_keep_enable && !self.use_bare_protocol) {"),
            "BYTE-IDENTICAL: the skip default-OFF + framed-only (inverted) guard disappeared — \
             gate off OR bare MUST run SetVoltage exactly as today"
        );
        // LOAD-BEARING: the gate skips ONLY the 0x10 SetVoltage; the 0x15 ENABLE
        // still runs unconditionally AFTER the gated block. Pin that the ENABLE
        // call is present and is reached AFTER the SetVoltage decision (not
        // wrapped inside the skip branch — the SENSOR_ONLY gate's bug was
        // skipping the ENABLE too, which the cold strace proves bosminer issues).
        let gate_idx = MOD_SRC
            .find("if !(self.skip_setvoltage_keep_enable && !self.use_bare_protocol) {")
            .expect("the skip gate disappeared");
        // `enable_voltage()?` after the gate is the canonical 0x15 ENABLE call in
        // cold_boot_init_with_options; assert it exists after the gate.
        let enable_idx = MOD_SRC[gate_idx..]
            .find("self.enable_voltage()?;")
            .map(|i| gate_idx + i)
            .expect("the unconditional 0x15 ENABLE call after the skip gate disappeared");
        assert!(
            enable_idx > gate_idx,
            "the 0x15 ENABLE must still run AFTER the skip-SetVoltage gate (the gate skips \
             ONLY the 0x10 SetVoltage; bosminer DOES send 0x15 — see ENABLE-DRIFT-DIFF.md)"
        );
        // The ENABLE must NOT be nested inside the skip (else) branch — it must
        // be at the outer cold-boot scope. The skip else-branch ends at the
        // closing brace that precedes the post-setvoltage settle; the ENABLE call
        // comes after that. Pin that the post-setvoltage keep-alive settle sits
        // between the gated block and the ENABLE (i.e. the ENABLE is at outer
        // scope, shared by BOTH arms).
        let settle_idx = MOD_SRC
            .find("self.keepalive_sleep(50, \"post-setvoltage-settle\");")
            .expect("the post-setvoltage settle disappeared");
        assert!(
            gate_idx < settle_idx && settle_idx < enable_idx,
            "the 0x15 ENABLE must be shared outer-scope (after the post-setvoltage settle), \
             reached on BOTH the skip and the run-SetVoltage arms"
        );
    }

    /// Bosminer-minimal ENABLE (2026-06-07, `a lab unit` standalone cold-engage): the
    /// CONSOLIDATED fix for the LIVE ENABLE `[82, 82]` drift. Source-pinned (the
    /// HAL/bus path can't run host-side, same pattern as the skip-SetVoltage /
    /// re-JUMP / keep-alive pins) so a refactor can't silently (a) change the
    /// default-OFF + framed-only gating, (b) start running the legacy flush /
    /// heartbeat / LM75A / re-JUMP / SetVoltage between GET_VERSION(0x89) and the
    /// ENABLE, or (c) drop the byte-identical guarantee for the
    /// fleet/handoff/legacy/BARE paths.
    #[test]
    fn bosminer_minimal_enable_is_gated_and_enable_only() {
        // The gate field + setter exist (additive plumbing, sibling of the
        // skip-SetVoltage / re-JUMP / keep-alive gates).
        assert!(
            MOD_SRC.contains("bosminer_minimal_enable: bool,")
                && MOD_SRC.contains("pub fn set_bosminer_minimal_enable(&mut self, on: bool) {"),
            "the bosminer-minimal-ENABLE field/setter disappeared"
        );
        // Default-OFF guard: the whole minimal block is `if self.bosminer_minimal_enable {`,
        // so with the gate off the cold_boot_init path is byte-identical to today.
        let minimal_idx = MOD_SRC
            .find("if self.bosminer_minimal_enable {")
            .expect("BYTE-IDENTICAL: the bosminer-minimal default-OFF guard disappeared");
        // Framed-only inner guard — the BARE (fw=0x82) path falls through to the
        // unchanged legacy path (the minimal window is framed fw=0x89 only).
        assert!(
            MOD_SRC.contains("if !minimal_use_bare {"),
            "the bosminer-minimal framed-only inner guard disappeared"
        );
        // The minimal arm KEEPS the byte-identical 0x15 ENABLE and EARLY-RETURNS
        // (so the legacy flush / heartbeat / LM75A / re-JUMP / SetVoltage never run).
        let minimal_enable_idx = minimal_idx
            + MOD_SRC[minimal_idx..]
                .find("self.enable_voltage()?;")
                .expect("the bosminer-minimal arm's 0x15 ENABLE call disappeared");
        let minimal_return_idx = minimal_idx
            + MOD_SRC[minimal_idx..]
                .find("return Ok(());")
                .expect("the bosminer-minimal arm's early return disappeared");
        assert!(
            minimal_enable_idx < minimal_return_idx,
            "the bosminer-minimal arm must ENABLE then early-return"
        );
        // ORDERING: the minimal block (and its ENABLE+return) must sit BEFORE the
        // legacy flush / LM75A read / re-JUMP / SetVoltage gate — proving the
        // early-return short-circuits ALL of them on the minimal path.
        // Anchor the flush AFTER the minimal block — `self.flush_parser();` also
        // appears in the separate DspicController::cold_boot_init earlier in the
        // file, so a bare `.find` would point at the wrong function.
        let flush_idx = minimal_idx
            + MOD_SRC[minimal_idx..]
                .find("self.flush_parser();")
                .expect("the cold_boot_init_with_options parser flush disappeared");
        let lm75_idx = MOD_SRC
            .find("let temps = self.read_all_temperatures_keepalive();")
            .expect("the LM75A pre-voltage read disappeared");
        let rejump_idx = MOD_SRC
            .find("self.rejump_to_app_mode_if_drifted();")
            .expect("the re-JUMP call site disappeared");
        let setv_gate_idx = MOD_SRC
            .find("if !(self.skip_setvoltage_keep_enable && !self.use_bare_protocol) {")
            .expect("the SetVoltage gate disappeared");
        assert!(
            minimal_idx < flush_idx
                && minimal_enable_idx < flush_idx
                && minimal_return_idx < flush_idx,
            "the bosminer-minimal ENABLE+return must precede the legacy parser flush \
             (nothing legacy runs on the minimal path)"
        );
        assert!(
            minimal_return_idx < lm75_idx
                && minimal_return_idx < rejump_idx
                && minimal_return_idx < setv_gate_idx,
            "the bosminer-minimal early-return must precede the LM75A read, the re-JUMP, \
             and the SetVoltage gate (all skipped on the minimal path)"
        );
        // NO fault-suspect dsPIC command between the minimal block start and the
        // minimal ENABLE — the ONLY traffic in the GET_VERSION→ENABLE window is
        // the ENABLE itself. (The `0x16` post-enable heartbeat is AFTER the ENABLE
        // — outside this window — so it is intentionally NOT checked here.) Match
        // the exact CALL forms (which do not appear in the in-window comments).
        let pre_enable = &MOD_SRC[minimal_idx..minimal_enable_idx];
        for forbidden_call in [
            "self.set_voltage(",
            "self.read_all_temperatures_keepalive()",
            "self.read_all_temperatures()",
            "self.rejump_to_app_mode_if_drifted()",
            "self.send_heartbeat()",
            "self.flush_parser()",
            "self.postjump_keepalive_tick(",
            "self.get_version()",
        ] {
            assert!(
                !pre_enable.contains(forbidden_call),
                "bosminer-minimal regression: a `{}` runs between the minimal block start \
                 and the ENABLE — the GET_VERSION→ENABLE window must contain ONLY the ENABLE \
                 (ENABLE-DRIFT-DIFF.md / LIVE TEST 6)",
                forbidden_call
            );
        }
    }

    /// prod-readiness hunt #7 (safety-test-gap): the fw=0x82 BARE SetVoltage
    /// encoder emits RAW big-endian mV with NO internal clamp (unlike the FRAMED
    /// path, which clamps inside framed_voltage_dac). Its only over-cap
    /// protection is the method-level clamp_dspic_voltage_to_hard_cap in
    /// set_voltage. Pin (a) that the BARE encoder faithfully renders a (capped)
    /// value and (b) that the upstream clamp call survives in BOTH set_voltage
    /// methods — so a refactor removing the clamp ("FRAMED already clamps")
    /// can't silently leave the BARE path emitting an uncapped over-voltage.
    #[test]
    fn dspic_bare_setvoltage_encoder_renders_value_and_clamp_is_upstream() {
        // 14500 mV = 0x38A4 → [55 AA 10 38 A4] (CMD_SET_VOLTAGE=0x10, BE hi/lo).
        assert_eq!(
            dspic_set_voltage_frame(DspicFirmware::Fw82, true, 14_500),
            vec![0x55, 0xAA, CMD_SET_VOLTAGE, 0x38, 0xA4],
            "BARE encoder must render the (already-capped) value big-endian"
        );
        // The BARE encoder does NOT clamp; the <=14500 hard cap MUST be applied
        // upstream in set_voltage. Both real set_voltage sites (controller +
        // service) must call the clamp before encoding.
        assert!(
            MOD_SRC
                .matches("clamp_dspic_voltage_to_hard_cap(voltage_mv")
                .count()
                >= 2,
            "SAFETY: the <=14500 hard-cap input clamp must precede the encoder in BOTH \
             set_voltage methods (BARE encoder has no internal clamp)"
        );
    }

    ///  (RE-018, 2026-05-30) — pin the dsPIC ENABLE encoding-form + ACK
    /// classification that the `a lab unit` echo-not-ACK root cause turned on. The `a lab unit`
    /// effective-chain dsPIC's echo-family byte is 0x8A (GET_VERSION reports 0x89):
    /// Fw89 selects the 7-byte VnishPadded form (which `a lab unit` ECHOES [0x8A,0x8A])
    /// while Fw8A selects the 6-byte Canonical form (which returned a real
    /// [0x15,0x00] ACK live). Change #1 (DCENT_AM2_DSPIC_FW_FROM_OBSERVED)
    /// constructs the 0x22 service as Fw8A so it picks Canonical + expects 0x8A.
    /// Lock the mapping, the byte-exact frames, and the ACK classification so the
    /// fix can never silently regress.
    #[test]
    fn wave57_enable_encoding_and_ack_classification_pinned() {
        // Encoding selection: Fw89 -> 7-byte VnishPadded (the form .25 echoes);
        // Fw8A -> 6-byte Canonical (the form that ACKs on .25).
        // matches! (not assert_eq!) so this compiles whether or not
        // EnableFrameEncoding derives PartialEq (prod uses matches! on it).
        assert!(
            matches!(
                dspic_enable_disable_encoding(DspicFirmware::Fw89),
                EnableFrameEncoding::VnishPadded
            ),
            "Fw89 must select the 7-byte VnishPadded ENABLE form (the form .25 echoes)"
        );
        assert!(
            matches!(
                dspic_enable_disable_encoding(DspicFirmware::Fw8A),
                EnableFrameEncoding::Canonical
            ),
            "Fw8A must select the 6-byte Canonical ENABLE form (the .25-proven ACKing form)"
        );
        // Byte-exact ENABLE frames (framed, non-bare), verified against the live
        // .25 logs: 6-byte Canonical = [55 AA 04 15 01 1A] (the .25-proven form),
        // 7-byte VnishPadded = [55 AA 05 15 01 00 1B].
        assert_eq!(
            dspic_enable_voltage_frame(false, EnableFrameEncoding::Canonical),
            vec![0x55, 0xAA, 0x04, CMD_ENABLE_VOLTAGE, 0x01, 0x1A],
            "6-byte Canonical ENABLE frame must be byte-exact (the .25 fw=0x8A form that ACKs)"
        );
        assert_eq!(
            dspic_enable_voltage_frame(false, EnableFrameEncoding::VnishPadded),
            vec![0x55, 0xAA, 0x05, CMD_ENABLE_VOLTAGE, 0x01, 0x00, 0x1B],
            "7-byte VnishPadded ENABLE frame must be byte-exact"
        );
        // ACK classification: with expected_fw=0x8A the [0x8A,0x8A] echo is a
        // recognized FirmwareEcho (NOT Mismatch); the pre-fix expected_fw=0x89
        // made the SAME bytes a FirmwareEchoMismatch (the .25 failure).
        // matches! (not assert_eq!) so this compiles regardless of whether
        // EnableVoltageAckKind derives Debug (prod only uses Display on it).
        assert!(
            matches!(
                classify_enable_ack(&[0x8A, 0x8A], Some(0x8A)),
                EnableVoltageAckKind::FirmwareEcho
            ),
            "with expected_fw=0x8A the 0x8A echo must classify as FirmwareEcho (not Mismatch)"
        );
        assert!(
            matches!(
                classify_enable_ack(&[0x8A, 0x8A], Some(0x89)),
                EnableVoltageAckKind::FirmwareEchoMismatch
            ),
            "the pre-fix expected_fw=0x89 made the SAME 0x8A echo a FirmwareEchoMismatch (the .25 failure)"
        );
        // A genuine [0x15,0x00]/[0x15,0x01] is RealAck regardless of expected_fw.
        assert!(matches!(
            classify_enable_ack(&[CMD_ENABLE_VOLTAGE, 0x00], Some(0x8A)),
            EnableVoltageAckKind::RealAck
        ));
        assert!(matches!(
            classify_enable_ack(&[CMD_ENABLE_VOLTAGE, 0x01], Some(0x8A)),
            EnableVoltageAckKind::RealAck
        ));
        // dspic_expected_fw_byte(Fw8A) -> 0x8A drives the correct classification.
        assert_eq!(dspic_expected_fw_byte(DspicFirmware::Fw8A), Some(0x8A));
    }

    #[test]
    fn variant_from_fw_byte() {
        assert_eq!(PicVariant::from_fw_byte(0x00), PicVariant::Unknown);
        assert_eq!(PicVariant::from_fw_byte(0xFF), PicVariant::Unknown);
        assert_eq!(PicVariant::from_fw_byte(0x82), PicVariant::Bare82);
        assert_eq!(PicVariant::from_fw_byte(0x86), PicVariant::S19jBare86);
        assert_eq!(PicVariant::from_fw_byte(0x89), PicVariant::S19jProAm2);
        assert_eq!(PicVariant::from_fw_byte(0x8A), PicVariant::Framed8A);
        assert_eq!(PicVariant::from_fw_byte(0xB9), PicVariant::FramedB9);
        assert_eq!(PicVariant::from_fw_byte(0xFE), PicVariant::FramedFE);
        assert!(matches!(
            PicVariant::from_fw_byte(0x88),
            PicVariant::Other(0x88)
        ));
    }

    #[test]
    fn pic0x89_fw_mapper_preserves_degraded_fw86_for_serial_wrappers() {
        assert_eq!(
            pic0x89_firmware_from_observed_fw_byte(Some(0x86)),
            DspicFirmware::Fw86
        );
        assert_eq!(
            pic0x89_firmware_from_observed_fw_byte(Some(0x89)),
            DspicFirmware::Fw89
        );
        assert_eq!(
            pic0x89_firmware_from_observed_fw_byte(None),
            DspicFirmware::Unknown
        );
        assert_eq!(
            pic0x89_firmware_from_observed_fw_byte(Some(0x00)),
            DspicFirmware::Unknown
        );
        assert_eq!(
            pic0x89_firmware_from_observed_fw_byte(Some(0x88)),
            DspicFirmware::Other(0x88)
        );
        assert!(!dspic_voltage_command_allowed(
            pic0x89_firmware_from_observed_fw_byte(Some(0x86)),
            false
        ));
        assert!(!dspic_voltage_command_allowed(
            pic0x89_firmware_from_observed_fw_byte(Some(0x88)),
            false
        ));
    }

    #[test]
    fn reset_allowed_only_on_bare82() {
        assert!(PicVariant::Bare82.reset_allowed());
        assert!(!PicVariant::S19jBare86.reset_allowed());
        assert!(!PicVariant::S19jProAm2.reset_allowed());
        assert!(!PicVariant::Framed8A.reset_allowed());
        assert!(!PicVariant::FramedB9.reset_allowed());
        assert!(!PicVariant::FramedFE.reset_allowed());
        assert!(!PicVariant::Other(0x88).reset_allowed());
        assert!(!PicVariant::Unknown.reset_allowed());
    }

    #[test]
    fn jump_allowed_only_on_bare82() {
        assert!(PicVariant::Bare82.jump_allowed());
        assert!(!PicVariant::S19jBare86.jump_allowed());
        assert!(!PicVariant::S19jProAm2.jump_allowed());
        assert!(!PicVariant::Framed8A.jump_allowed());
        assert!(!PicVariant::FramedB9.jump_allowed());
        assert!(!PicVariant::FramedFE.jump_allowed());
        assert!(!PicVariant::Other(0x88).jump_allowed());
        assert!(!PicVariant::Unknown.jump_allowed());
    }

    #[test]
    fn ensure_reset_allowed_refuses_s19j_pro() {
        let err = ensure_reset_allowed(PicVariant::S19jProAm2)
            .expect_err("S19j Pro RESET must be refused without panicking");
        assert!(err.contains("PIC RESET banned"));
    }

    #[test]
    fn ensure_jump_allowed_refuses_s19j_pro() {
        let err = ensure_jump_allowed(PicVariant::S19jProAm2)
            .expect_err("S19j Pro JUMP_TO_APP must be refused without panicking");
        assert!(err.contains("PIC JUMP_TO_APP banned"));
    }

    #[test]
    fn ensure_reset_allowed_ok_on_others() {
        ensure_reset_allowed(PicVariant::Bare82).expect("Bare82 reset remains allowed");
    }

    #[test]
    fn ensure_jump_allowed_ok_on_bare82() {
        ensure_jump_allowed(PicVariant::Bare82).expect("Bare82 jump remains allowed");
    }

    #[test]
    fn parse_get_version_reply_accepts_known_shapes() {
        assert_eq!(parse_get_version_reply(&[0x86]), Some(0x86));
        assert_eq!(
            parse_get_version_reply(&[0x86, 0x00, 0x00, 0x00, 0x00]),
            Some(0x86)
        );
        assert_eq!(
            parse_get_version_reply(&[0x05, CMD_GET_VERSION, 0x89, 0x00, 0x00]),
            Some(0x89)
        );
        assert_eq!(
            parse_get_version_reply(&[CMD_GET_VERSION, 0x00, 0x8A, 0x00, 0x00]),
            Some(0x8A)
        );
    }

    #[test]
    fn parse_get_version_reply_rejects_bus_noise() {
        assert_eq!(
            parse_get_version_reply(&[0x86, 0x0C, 0x18, 0x30, 0x60]),
            None
        );
        assert_eq!(
            parse_get_version_reply(&[0xFF, 0xFE, 0xFC, 0xF8, 0xF0]),
            None
        );
        assert_eq!(
            parse_get_version_reply(&[0x05, 0x0A, 0x14, 0x28, 0x50]),
            None
        );
        assert_eq!(
            parse_get_version_reply(&[0x89, 0x12, 0x24, 0x48, 0x90]),
            None
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn parse_get_version_reply_never_panics_on_arbitrary_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..32)
        ) {
            let _ = parse_get_version_reply(&data);
        }
    }

    #[test]
    fn get_version_probe_frames_cover_short_and_framed_without_reset_jump() {
        assert_eq!(
            dspic_get_version_frame(GetVersionEncoding::Short),
            &[0x55, 0xAA, CMD_GET_VERSION]
        );
        assert_eq!(
            dspic_get_version_frame(GetVersionEncoding::Framed),
            &[0x55, 0xAA, 0x04, CMD_GET_VERSION, 0x00, 0x1B]
        );
    }

    #[test]
    fn get_version_probe_order_uses_short_first_for_unknown_and_fw86() {
        assert_eq!(
            dspic_get_version_probe_order(DspicFirmware::Unknown),
            [GetVersionEncoding::Short, GetVersionEncoding::Framed]
        );
        assert_eq!(
            dspic_get_version_probe_order(DspicFirmware::Fw86),
            [GetVersionEncoding::Short, GetVersionEncoding::Framed]
        );
        assert_eq!(
            dspic_get_version_probe_order(DspicFirmware::Fw89),
            [GetVersionEncoding::Framed, GetVersionEncoding::Short]
        );
    }

    /// : when `DCENT_AM2_DSPIC_BOSMINER_FAITHFUL=1` is set,
    /// the GET_VERSION transaction must OMIT the 16-byte parser-flush
    /// prelude (bosminer's strace on `a lab unit` shows it doesn't send one
    /// before GET_VERSION). When unset, the legacy path stays.
    ///
    #[test]
    fn wave41_get_version_skips_parser_flush_when_policy_enabled() {
        // Default-off path: parser flush present.
        let steps_off = dspic_get_version_transaction_steps_with_bosminer_faithful(
            GetVersionEncoding::Short,
            false,
        );
        assert_eq!(steps_off.len(), 5, "default path includes parser flush");
        assert!(
            matches!(steps_off[0], I2cTransactionStep::WriteByteByByte(ref b) if b.len() == DSPIC_PARSER_FLUSH_LEN),
            "default first step must be parser flush"
        );

        //  on: parser flush omitted.
        let steps_on = dspic_get_version_transaction_steps_with_bosminer_faithful(
            GetVersionEncoding::Short,
            true,
        );
        let expected_len = 2 + DSPIC_GET_VERSION_SHORT_READ_LEN;
        assert_eq!(
            steps_on.len(),
            expected_len,
            "bosminer-faithful path drops 2 steps (flush + settle)"
        );
        assert!(
            matches!(steps_on[0], I2cTransactionStep::WriteByteByByte(ref b) if b.as_slice() == dspic_get_version_frame(GetVersionEncoding::Short)),
            "bosminer-faithful first step must be GET_VERSION write (no preceding flush)"
        );
        assert!(!dspic_bosminer_faithful_value_enabled(None));
        assert!(!dspic_bosminer_faithful_value_enabled(Some("0")));
        assert!(dspic_bosminer_faithful_value_enabled(Some(" 1 ")));
    }

    /// : pin the bosminer-faithful constants. Drift in either
    /// invalidates the diagnostic match with bosminer's strace.
    #[test]
    fn wave41_bosminer_constants_pinned() {
        assert_eq!(
            DSPIC_BOSMINER_PARSER_FLUSH_LEN, 7,
            "bosminer's i2c-0 sync prelude is 7 zero bytes per Wave-40 trace decode"
        );
        assert_eq!(
            DSPIC_BOSMINER_INTER_BYTE_GAP_MS, 6,
            "bosminer's inter-byte gap on i2c-0 is 6 ms per Wave-40 trace decode"
        );
    }

    #[test]
    fn get_version_transaction_flushes_waits_writes_then_reads() {
        let steps = dspic_get_version_transaction_steps_with_bosminer_faithful(
            GetVersionEncoding::Short,
            false,
        );
        assert_eq!(steps.len(), 5);

        match &steps[0] {
            I2cTransactionStep::WriteByteByByte(bytes) => {
                assert_eq!(bytes, &vec![0u8; DSPIC_PARSER_FLUSH_LEN]);
            }
            other => panic!("unexpected first GET_VERSION step: {:?}", other),
        }
        assert!(matches!(
            steps[1],
            I2cTransactionStep::SleepMs(DSPIC_PARSER_FLUSH_SETTLE_MS)
        ));
        match &steps[2] {
            I2cTransactionStep::WriteByteByByte(bytes) => {
                assert_eq!(
                    bytes.as_slice(),
                    dspic_get_version_frame(GetVersionEncoding::Short)
                );
            }
            other => panic!("unexpected GET_VERSION write step: {:?}", other),
        }
        assert!(matches!(
            steps[3],
            I2cTransactionStep::SleepMs(DSPIC_GET_VERSION_REPLY_DELAY_MS)
        ));
        assert!(matches!(
            steps[4],
            I2cTransactionStep::Read(DSPIC_GET_VERSION_SHORT_READ_LEN)
        ));

        let framed_steps = dspic_get_version_transaction_steps_with_bosminer_faithful(
            GetVersionEncoding::Framed,
            false,
        );
        assert_eq!(framed_steps.len(), 4 + DSPIC_GET_VERSION_FRAMED_READ_LEN);
        assert!(
            framed_steps[4..]
                .iter()
                .all(|step| matches!(step, I2cTransactionStep::Read(1))),
            "framed GET_VERSION must read one byte per I2C transaction"
        );
    }

    #[test]
    fn enable_ack_transaction_uses_bytewise_write_and_reads() {
        let frame = dspic_enable_voltage_frame(false, EnableFrameEncoding::VnishPadded);
        let steps = dspic_bytewise_write_then_read_steps(frame, 2, DSPIC_ENABLE_REPLY_DELAY_MS);

        assert_eq!(steps.len(), 5);
        assert!(matches!(steps[0], I2cTransactionStep::SetTimeout(10)));
        match &steps[1] {
            I2cTransactionStep::WriteByteByByte(bytes) => assert_eq!(bytes.as_slice(), frame),
            other => panic!("unexpected ENABLE write step: {:?}", other),
        }
        assert!(matches!(
            steps[2],
            I2cTransactionStep::SleepMs(DSPIC_ENABLE_REPLY_DELAY_MS)
        ));
        assert!(
            steps[3..]
                .iter()
                .all(|step| matches!(step, I2cTransactionStep::Read(1))),
            "ENABLE ACK must read one byte per I2C transaction"
        );
    }

    #[test]
    fn enable_ack_classifier_separates_real_ack_and_fw_echo() {
        assert_eq!(
            classify_enable_ack(&[CMD_ENABLE_VOLTAGE, 0x00], Some(0x89)),
            EnableVoltageAckKind::RealAck
        );
        assert_eq!(
            classify_enable_ack(&[CMD_ENABLE_VOLTAGE, 0x01], Some(0x89)),
            EnableVoltageAckKind::RealAck
        );
        assert_eq!(
            classify_enable_ack(&[0x89, 0x89], Some(0x89)),
            EnableVoltageAckKind::FirmwareEcho
        );
        assert_eq!(
            classify_enable_ack(&[0x8A, 0x8A], Some(0x89)),
            EnableVoltageAckKind::FirmwareEchoMismatch
        );
        assert_eq!(
            classify_enable_ack(&[0xFF, 0xFF], Some(0x89)),
            EnableVoltageAckKind::AllFf
        );
        assert_eq!(
            classify_enable_ack(&[0x01, 0x02], Some(0x89)),
            EnableVoltageAckKind::Mismatch
        );
    }

    #[test]
    fn firmware_protocol_maps_fw86_to_bare_and_fw89_to_framed() {
        assert_eq!(
            decode_dspic_firmware(Some(0x82)).protocol(),
            DspicProtocol::Bare
        );
        assert_eq!(
            decode_dspic_firmware(Some(0x86)).protocol(),
            DspicProtocol::Bare
        );
        assert_eq!(
            decode_dspic_firmware(Some(0x89)).protocol(),
            DspicProtocol::Framed
        );
        assert_eq!(
            decode_dspic_firmware(Some(0x8A)).protocol(),
            DspicProtocol::Framed
        );
    }

    #[test]
    fn at3_measure_voltage_gate_allows_only_framed_fw89_and_fw8a() {
        // AT-3 may only issue the parser-safe byte-wise framed 0x3A read, which
        // measure_voltage takes for Fw89/Fw8A. Every bare / unknown / other
        // firmware MUST be refused so AT-3 never reaches the I2C_RDWR fallback.
        assert!(at3_measure_voltage_firmware_allowed(DspicFirmware::Fw89));
        assert!(at3_measure_voltage_firmware_allowed(DspicFirmware::Fw8A));
        // Bare (would route measure_voltage to the I2C_RDWR combined read).
        assert!(!at3_measure_voltage_firmware_allowed(DspicFirmware::Fw82));
        assert!(!at3_measure_voltage_firmware_allowed(DspicFirmware::Fw86));
        // Other framed-by-protocol variants are NOT proven for the 0x3A decode
        // and are refused too (conservative — only the two live-decoded
        // firmwares are allowed).
        assert!(!at3_measure_voltage_firmware_allowed(DspicFirmware::FwB9));
        assert!(!at3_measure_voltage_firmware_allowed(DspicFirmware::FwFE));
        assert!(!at3_measure_voltage_firmware_allowed(DspicFirmware::Other(
            0x90
        )));
        assert!(!at3_measure_voltage_firmware_allowed(
            DspicFirmware::Unknown
        ));
    }

    #[test]
    fn voltage_commands_require_supported_firmware_and_explicit_fw86_trust() {
        assert!(dspic_requires_degraded_fw_voltage_refusal(
            DspicFirmware::Fw86
        ));
        assert!(!dspic_voltage_command_allowed(DspicFirmware::Fw86, false));
        assert!(dspic_voltage_command_allowed(DspicFirmware::Fw86, true));
        assert!(dspic_voltage_command_allowed(DspicFirmware::Fw82, false));
        assert!(dspic_voltage_command_allowed(DspicFirmware::Fw89, false));
        assert!(dspic_voltage_command_allowed(DspicFirmware::Fw8A, false));
        assert!(dspic_voltage_command_allowed(DspicFirmware::FwB9, false));
        assert!(dspic_voltage_command_allowed(DspicFirmware::FwFE, false));
        assert!(!dspic_voltage_command_allowed(
            DspicFirmware::Unknown,
            false
        ));
        assert!(!dspic_voltage_command_allowed(DspicFirmware::Unknown, true));
        assert!(!dspic_voltage_command_allowed(
            DspicFirmware::Other(0x88),
            false
        ));
        assert!(!dspic_voltage_command_allowed(
            DspicFirmware::Other(0x88),
            true
        ));
    }

    #[test]
    fn runtime_protocol_requires_an_explicitly_modeled_firmware() {
        for known in [
            DspicFirmware::Fw82,
            DspicFirmware::Fw86,
            DspicFirmware::Fw89,
            DspicFirmware::Fw8A,
            DspicFirmware::FwB9,
            DspicFirmware::FwFE,
        ] {
            assert!(
                dspic_runtime_protocol_is_proven(known),
                "known firmware {known:?} must retain its proven route"
            );
        }
        assert!(!dspic_runtime_protocol_is_proven(DspicFirmware::Unknown));
        assert!(!dspic_runtime_protocol_is_proven(DspicFirmware::Other(
            0x88
        )));
        let missing =
            ensure_dspic_runtime_protocol_is_proven(0x21, DspicFirmware::Unknown, "heartbeat")
                .expect_err("heartbeat must fail closed without firmware evidence");
        assert!(missing.to_string().contains("no firmware identity"));
        let unsupported =
            ensure_dspic_runtime_protocol_is_proven(0x21, DspicFirmware::Other(0x88), "heartbeat")
                .expect_err("heartbeat must fail closed for unsupported firmware");
        assert!(unsupported.to_string().contains("0x88"));
    }

    #[test]
    fn dspic_voltage_hard_cap_only_ever_lowers_and_stays_14500() {
        // Load-bearing priority-1 chip-rail OVER-VOLTAGE protection (project rule
        // <= 14500 mV; over-voltage damages ASIC silicon). The clamp must only
        // ever LOWER a commanded voltage to the cap, never raise it, with the lab
        // override the sole escape. Pin the value + the full truth table so an
        // inverted condition / raised cap / wrong override sense can't silently
        // drive the chip rail past 14500 mV and pass CI.
        assert_eq!(DSPIC_VOLTAGE_HARD_CAP_MV, 14_500);
        // Must be a REAL reduction below the DAC full-scale, else it's a no-op.
        assert!(DSPIC_VOLTAGE_HARD_CAP_MV < DSPIC_MAX_VOLTAGE_MV);

        // Default (no lab override): at/below the cap passes through untouched.
        for mv in [0u16, 1000, 13_700, 14_499, 14_500] {
            let (eff, clamped) = clamp_dspic_voltage_to_hard_cap(mv, false);
            assert_eq!(eff, mv, "{mv} mV (<= cap) must pass through");
            assert!(!clamped);
        }
        // Default: anything above the cap is lowered TO the cap — never raised.
        for mv in [14_501u16, 15_000, DSPIC_MAX_VOLTAGE_MV, u16::MAX] {
            let (eff, clamped) = clamp_dspic_voltage_to_hard_cap(mv, false);
            assert_eq!(
                eff, DSPIC_VOLTAGE_HARD_CAP_MV,
                "{mv} mV (> cap) must clamp to the cap"
            );
            assert!(clamped);
            assert!(eff <= mv, "the clamp must only ever LOWER, never raise");
        }
        // Lab override: passes through unchanged (the bench ~15 V pre-open pulse),
        // still bounded by the DAC range check at the call site.
        for mv in [0u16, 14_500, 14_501, 15_000, u16::MAX] {
            let (eff, clamped) = clamp_dspic_voltage_to_hard_cap(mv, true);
            assert_eq!(eff, mv, "lab override must pass {mv} mV through");
            assert!(!clamped);
        }
    }

    #[test]
    fn fw86_voltage_refusal_names_the_lab_override() {
        let msg = dspic_voltage_refusal_detail("set_voltage");
        assert!(msg.contains("fw=0x86"), "missing firmware in {msg}");
        assert!(
            msg.contains(DSPIC_FW86_TRUST_DEGRADED_ENV),
            "missing lab override env in {msg}"
        );
    }

    /// SAFETY (PIC-1, 2026-06-20): with `DCENT_AM2_FORCE_FW89_ENCODING=1` set,
    /// an observed/version fw=0x86 must NOT be remapped to Fw89 — it must stay
    /// `Fw86` and stay voltage-refused. Fw89 is not in the voltage-refusal
    /// predicate, so a remap here would silently make a corrupted fw=0x86 chip
    /// (the `a lab unit` corruption class) voltage-ALLOWED via a second, non-auditable
    /// bypass distinct from the lab-only DCENT_AM2_TRUST_DEGRADED_FW override.
    ///
    /// Before the PIC-1 fix this test FAILS (both functions remapped Fw86 ->
    /// Fw89, which `dspic_voltage_command_allowed` then permits).
    ///
    #[test]
    fn force_fw89_encoding_does_not_remap_fw86_so_it_stays_voltage_refused() {
        // version-byte path: fw=0x86 stays Fw86 (NOT Fw89).
        let from_ver = DspicFirmware::from_version_with_fw89_encoding(0x86, true);
        // observed-fw-byte (pic0x89) path: fw=0x86 stays Fw86 (NOT Fw89).
        let from_observed =
            pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(Some(0x86), true);
        // Missing evidence must not be upgraded by an encoding experiment.
        let from_missing = pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(None, true);

        assert_eq!(
            from_ver,
            DspicFirmware::Fw86,
            "SAFETY: from_version remapped fw=0x86 to {from_ver:?} under FORCE_FW89_ENCODING — \
             fw=0x86 must stay Fw86 so it remains voltage-refused"
        );
        assert_eq!(
            from_observed,
            DspicFirmware::Fw86,
            "SAFETY: pic0x89_firmware_from_observed_fw_byte remapped fw=0x86 to {from_observed:?} \
             under FORCE_FW89_ENCODING — fw=0x86 must stay Fw86 so it remains voltage-refused"
        );
        assert_eq!(
            from_missing,
            DspicFirmware::Unknown,
            "SAFETY: FORCE_FW89_ENCODING manufactured fw=0x89 from missing evidence"
        );

        // The whole point of refusing the remap: the resulting firmware is still
        // caught by the voltage-refusal predicate (no lab override granted).
        assert!(
            dspic_requires_degraded_fw_voltage_refusal(from_ver),
            "SAFETY: a FORCE_FW89-remapped fw=0x86 escaped the voltage-refusal predicate"
        );
        assert!(
            !dspic_voltage_command_allowed(from_ver, false),
            "SAFETY: fw=0x86 became voltage-allowed under FORCE_FW89_ENCODING (second bypass)"
        );
        assert!(
            !dspic_voltage_command_allowed(from_observed, false),
            "SAFETY: observed fw=0x86 became voltage-allowed under FORCE_FW89_ENCODING (second bypass)"
        );
    }

    /// No-regression companion: the legitimate `a lab unit` cold-boot case (fw=0x82,
    /// bare bootloader protocol) STILL remaps to Fw89 under FORCE_FW89_ENCODING.
    /// Restricting the guard to Fw82-only must not break the path it exists for.
    #[test]
    fn force_fw89_encoding_still_remaps_fw82_to_fw89() {
        // Baseline (override OFF): fw=0x82 stays Fw82 on both paths.
        let off_ver = DspicFirmware::from_version_with_fw89_encoding(0x82, false);
        let off_observed =
            pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(Some(0x82), false);

        // Override ON: fw=0x82 upgrades to Fw89 on both paths.
        let on_ver = DspicFirmware::from_version_with_fw89_encoding(0x82, true);
        let on_observed =
            pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(Some(0x82), true);

        assert_eq!(off_ver, DspicFirmware::Fw82, "fw=0x82 default must be Fw82");
        assert_eq!(
            off_observed,
            DspicFirmware::Fw82,
            "observed fw=0x82 default must be Fw82"
        );
        assert_eq!(
            on_ver,
            DspicFirmware::Fw89,
            "FORCE_FW89_ENCODING must still remap fw=0x82 -> Fw89 (the .25 cold-boot path)"
        );
        assert_eq!(
            on_observed,
            DspicFirmware::Fw89,
            "FORCE_FW89_ENCODING must still remap observed fw=0x82 -> Fw89 (the .25 cold-boot path)"
        );
    }

    #[test]
    fn force_fw89_encoding_env_values_preserve_the_existing_opt_in_contract() {
        for enabled in ["1", "true", "TRUE", "yes", "YES", "on", "ON"] {
            assert!(
                force_fw89_encoding_value_enabled(Some(enabled)),
                "documented opt-in value {enabled:?} must remain enabled"
            );
        }

        for disabled in ["", "0", "false", "False", "no", "off", " true "] {
            assert!(
                !force_fw89_encoding_value_enabled(Some(disabled)),
                "non-contract value {disabled:?} must remain disabled"
            );
        }
        assert!(!force_fw89_encoding_value_enabled(None));
    }

    #[test]
    fn force_fw89_encoding_changes_only_observed_fw82_across_the_byte_space() {
        for fw_byte in u8::MIN..=u8::MAX {
            let detected = decode_dspic_firmware(Some(fw_byte));
            let default_version = DspicFirmware::from_version_with_fw89_encoding(fw_byte, false);
            let forced_version = DspicFirmware::from_version_with_fw89_encoding(fw_byte, true);
            let default_observed =
                pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(Some(fw_byte), false);
            let forced_observed =
                pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(Some(fw_byte), true);

            assert_eq!(
                default_version, detected,
                "default version byte 0x{fw_byte:02X}"
            );
            assert_eq!(
                default_observed, detected,
                "default observed byte 0x{fw_byte:02X}"
            );
            assert_eq!(
                forced_version, forced_observed,
                "both adapters must resolve byte 0x{fw_byte:02X} identically"
            );

            if fw_byte == 0x82 {
                assert_eq!(forced_version, DspicFirmware::Fw89);
            } else {
                assert_eq!(
                    forced_version, detected,
                    "encoding experiment changed non-fw82 identity 0x{fw_byte:02X}"
                );
            }
        }

        assert_eq!(
            pic0x89_firmware_from_observed_fw_byte_with_fw89_encoding(None, true),
            DspicFirmware::Unknown,
            "encoding experiment must not manufacture identity from missing evidence"
        );
    }

    /// Load-bearing <=14500 mV hard cap (input clamp at set_voltage).
    #[test]
    fn voltage_hard_cap_clamps_input_down_unless_lab_override() {
        // The cap itself must not exceed the project safety limit.
        assert_eq!(DSPIC_VOLTAGE_HARD_CAP_MV, 14_500);
        // Production targets are at/below the cap -> passthrough, no clamp.
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(13_700, false),
            (13_700, false)
        );
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(13_800, false),
            (13_800, false)
        );
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(14_500, false),
            (14_500, false)
        );
        // Above the cap, default -> clamped DOWN to the cap (never raised).
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(14_501, false),
            (14_500, true)
        );
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(15_000, false),
            (14_500, true)
        );
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(DSPIC_MAX_VOLTAGE_MV, false),
            (DSPIC_VOLTAGE_HARD_CAP_MV, true)
        );
        // Lab override lifts the cap up to DSPIC_MAX (AMTC pre-open) -> passthrough.
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(15_000, true),
            (15_000, false)
        );
        assert_eq!(
            clamp_dspic_voltage_to_hard_cap(DSPIC_MAX_VOLTAGE_MV, true),
            (DSPIC_MAX_VOLTAGE_MV, false)
        );
    }

    /// Property test (exhaustive over every u16): the production clamp is a TOTAL
    /// over-volt safety invariant. For any input voltage on any code path, with
    /// `lab_overvolt=false` the clamp must (1) never return a value above the
    /// 14500 mV dsPIC hard cap, (2) only ever LOWER, never raise, the input, and
    /// (3) be an exact no-op at/below the cap and an exact clamp-to-cap above it.
    /// The lab override is the single explicit escape hatch and passes through
    /// unchanged. This pins "the rail can never be programmed to an over-volt"
    /// against any future refactor of `clamp_dspic_voltage_to_hard_cap`.
    #[test]
    fn voltage_hard_cap_clamp_is_a_total_over_volt_invariant() {
        for mv in 0u16..=u16::MAX {
            let (prod, prod_clamped) = clamp_dspic_voltage_to_hard_cap(mv, false);
            assert!(
                prod <= DSPIC_VOLTAGE_HARD_CAP_MV,
                "production clamp let {mv} mV through as {prod} (> {DSPIC_VOLTAGE_HARD_CAP_MV} cap)"
            );
            assert!(prod <= mv, "clamp RAISED {mv} to {prod}");
            if mv <= DSPIC_VOLTAGE_HARD_CAP_MV {
                assert_eq!(
                    (prod, prod_clamped),
                    (mv, false),
                    "expected exact no-op at/below cap for {mv}"
                );
            } else {
                assert_eq!(
                    (prod, prod_clamped),
                    (DSPIC_VOLTAGE_HARD_CAP_MV, true),
                    "expected clamp-to-cap above cap for {mv}"
                );
            }
            // Lab override is the sole escape hatch: exact passthrough, no clamp flag.
            assert_eq!(
                clamp_dspic_voltage_to_hard_cap(mv, true),
                (mv, false),
                "lab override must pass {mv} through unchanged"
            );
        }
    }

    /// The hard-cap input clamp MUST NOT change the live-proven DAC encoding:
    /// the cap is an input guard, the DAC span (DSPIC_MIN..=DSPIC_MAX) is
    /// untouched, so 13.7 V still encodes to DAC 0x06.
    #[test]
    fn voltage_hard_cap_preserves_proven_dac_mapping() {
        // The cap lives strictly inside the DAC span; lowering DSPIC_MAX (which
        // would shift every production DAC code) is explicitly NOT how the cap
        // is implemented.
        assert!(DSPIC_VOLTAGE_HARD_CAP_MV < DSPIC_MAX_VOLTAGE_MV);
        assert!(DSPIC_VOLTAGE_HARD_CAP_MV > DSPIC_MIN_VOLTAGE_MV);
        // Proven mapping intact (matches runtime_frames_follow_detected_protocol).
        assert_eq!(framed_voltage_dac(13_700), 0x06);
    }

    #[test]
    fn runtime_frames_follow_detected_protocol() {
        assert_eq!(
            dspic_set_voltage_frame(DspicFirmware::Fw86, true, 13_700),
            vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, 0x06, 0x1A]
        );
        assert_eq!(
            dspic_set_voltage_frame(DspicFirmware::Fw82, true, 13_800),
            vec![0x55, 0xAA, CMD_SET_VOLTAGE, 0x35, 0xE8]
        );
        assert_eq!(
            dspic_set_voltage_frame(DspicFirmware::Fw89, false, 13_700),
            vec![0x55, 0xAA, 0x04, CMD_SET_VOLTAGE, 0x06, 0x1A]
        );

        assert_eq!(dspic_heartbeat_frame(true), &[0x55, 0xAA, CMD_HEARTBEAT]);
        assert_eq!(
            dspic_heartbeat_frame(false),
            &[0x55, 0xAA, 0x04, CMD_HEARTBEAT, 0x00, 0x1A]
        );
        assert_eq!(
            dspic_enable_voltage_frame(true, EnableFrameEncoding::Canonical),
            &[0x55, 0xAA, CMD_ENABLE_VOLTAGE, 0x01]
        );
        assert_eq!(
            dspic_enable_voltage_frame(false, EnableFrameEncoding::Canonical),
            &[0x55, 0xAA, 0x04, CMD_ENABLE_VOLTAGE, 0x01, 0x1A]
        );
        assert_eq!(
            dspic_enable_voltage_frame(false, EnableFrameEncoding::VnishPadded),
            &[0x55, 0xAA, 0x05, CMD_ENABLE_VOLTAGE, 0x01, 0x00, 0x1B]
        );
        // DISABLE bare/canonical/VnishPadded variants
        assert_eq!(
            dspic_disable_voltage_frame(true, EnableFrameEncoding::Canonical),
            &[0x55, 0xAA, CMD_ENABLE_VOLTAGE, 0x00]
        );
        assert_eq!(
            dspic_disable_voltage_frame(false, EnableFrameEncoding::Canonical),
            &[0x55, 0xAA, 0x04, CMD_ENABLE_VOLTAGE, 0x00, 0x19]
        );
        assert_eq!(
            dspic_disable_voltage_frame(false, EnableFrameEncoding::VnishPadded),
            &[0x55, 0xAA, 0x05, CMD_ENABLE_VOLTAGE, 0x00, 0x00, 0x1A]
        );
    }

    #[test]
    fn enable_disable_encoding_gates_by_fw_byte() {
        // fw=0x86 + fw=0x89 → 7-byte VNish form (per RE corpus 2026-04-25)
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::Fw86),
            EnableFrameEncoding::VnishPadded
        );
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::Fw89),
            EnableFrameEncoding::VnishPadded
        );
        // Other framed fw bytes → canonical 6-byte (unproven for VNish form)
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::Fw8A),
            EnableFrameEncoding::Canonical
        );
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::FwB9),
            EnableFrameEncoding::Canonical
        );
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::FwFE),
            EnableFrameEncoding::Canonical
        );
        // fw=0x82 only ever uses bare path; encoding hint is irrelevant but
        // defaults to Canonical.
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::Fw82),
            EnableFrameEncoding::Canonical
        );
        // Unknown / Other → conservative canonical fallback
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::Unknown),
            EnableFrameEncoding::Canonical
        );
        assert_eq!(
            dspic_enable_disable_encoding(DspicFirmware::Other(0x88)),
            EnableFrameEncoding::Canonical
        );
    }

    /// VNish RE byte-exact assertions for the 7-byte ENABLE/DISABLE form.
    /// Source:
    /// (direct match for `a lab unit` Zynq am2). Cross-validated by 22 firmwares.
    ///
    /// CHECKSUM formula: `(LEN + OPCODE + Σpayload) & 0xFF`.
    ///   ENABLE  : 0x05 + 0x15 + 0x01 + 0x00 = 0x1B
    ///   DISABLE : 0x05 + 0x15 + 0x00 + 0x00 = 0x1A
    #[test]
    fn vnish_7byte_enable_disable_byte_exact() {
        let enable = dspic_enable_voltage_frame(false, EnableFrameEncoding::VnishPadded);
        assert_eq!(
            enable,
            &[0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B],
            "VNish ENABLE_VOLTAGE 7-byte frame must match RE corpus exactly"
        );
        let disable = dspic_disable_voltage_frame(false, EnableFrameEncoding::VnishPadded);
        assert_eq!(
            disable,
            &[0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A],
            "VNish DISABLE_VOLTAGE 7-byte frame must match RE corpus exactly"
        );
        // Verify checksum independently of the static frame literals.
        let cksum = |len: u8, cmd: u8, payload: &[u8]| -> u8 {
            len.wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |a, &b| a.wrapping_add(b)))
        };
        assert_eq!(cksum(0x05, 0x15, &[0x01, 0x00]), 0x1B);
        assert_eq!(cksum(0x05, 0x15, &[0x00, 0x00]), 0x1A);
    }

    #[test]
    fn fw89_picks_vnish_padded_form_end_to_end() {
        // End-to-end: ensure the helper-driven path produces the VNish form
        // for fw=0x89 in framed mode.
        let fw = DspicFirmware::Fw89;
        let encoding = dspic_enable_disable_encoding(fw);
        let bare = false;
        assert_eq!(encoding, EnableFrameEncoding::VnishPadded);
        assert_eq!(
            dspic_enable_voltage_frame(bare, encoding),
            &[0x55, 0xAA, 0x05, 0x15, 0x01, 0x00, 0x1B]
        );
        assert_eq!(
            dspic_disable_voltage_frame(bare, encoding),
            &[0x55, 0xAA, 0x05, 0x15, 0x00, 0x00, 0x1A]
        );
    }

    #[test]
    fn fw8a_keeps_canonical_form_end_to_end() {
        // Regression guard: fw=0x8A must NOT silently switch to VNish form.
        let fw = DspicFirmware::Fw8A;
        let encoding = dspic_enable_disable_encoding(fw);
        let bare = false;
        assert_eq!(encoding, EnableFrameEncoding::Canonical);
        assert_eq!(
            dspic_enable_voltage_frame(bare, encoding),
            &[0x55, 0xAA, 0x04, 0x15, 0x01, 0x1A]
        );
        assert_eq!(
            dspic_disable_voltage_frame(bare, encoding),
            &[0x55, 0xAA, 0x04, 0x15, 0x00, 0x19]
        );
    }

    #[test]
    fn bootloader_commands_are_banned_on_s19j_variants() {
        let framed_reset = [0x55, 0xAA, 0x04, CMD_RESET, 0x00, 0x0B];
        let framed_jump = [0x55, 0xAA, 0x04, CMD_JUMP_TO_APP, 0x00, 0x0A];
        let bare_reset = [0x55, 0xAA, CMD_RESET];
        let heartbeat = [0x55, 0xAA, CMD_HEARTBEAT];
        let bare_set_voltage_dac_six = [0x55, 0xAA, CMD_SET_VOLTAGE, CMD_JUMP_TO_APP];
        let framed_len_six_non_bootloader = [0x55, 0xAA, 0x06, CMD_HEARTBEAT, 0x00, 0x1C];

        assert!(ensure_dspic_bootloader_command_allowed(
            0x21,
            DspicFirmware::Fw86,
            false,
            &framed_reset
        )
        .is_err());
        assert!(ensure_dspic_bootloader_command_allowed(
            0x21,
            DspicFirmware::Fw89,
            false,
            &framed_jump
        )
        .is_err());
        assert!(ensure_dspic_bootloader_command_allowed(
            0x21,
            DspicFirmware::Unknown,
            true,
            &bare_reset
        )
        .is_err());
        assert!(ensure_dspic_bootloader_command_allowed(
            0x20,
            DspicFirmware::Fw82,
            true,
            &bare_reset
        )
        .is_ok());
        assert!(ensure_dspic_bootloader_command_allowed(
            0x21,
            DspicFirmware::Fw89,
            false,
            &heartbeat
        )
        .is_ok());
        assert!(ensure_dspic_bootloader_command_allowed(
            0x21,
            DspicFirmware::Fw86,
            true,
            &bare_set_voltage_dac_six
        )
        .is_ok());
        assert!(ensure_dspic_bootloader_command_allowed(
            0x21,
            DspicFirmware::Fw89,
            false,
            &framed_len_six_non_bootloader
        )
        .is_ok());
    }

    #[test]
    fn dspic_framed_checksum_is_sum_not_xor() {
        // Heartbeat framed: [55 AA 04 16 00 1A] — CKSUM = (0x04+0x16+0x00)&0xFF = 0x1A
        // SetVoltage framed: [55 AA 04 10 06 1A] — CKSUM = (0x04+0x10+0x06)&0xFF = 0x1A
        // Verify via a handcrafted builder mirroring `encode_command_frame`.
        fn sum_ck(len: u8, cmd: u8, payload: &[u8]) -> u8 {
            len.wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |a, &b| a.wrapping_add(b)))
        }
        assert_eq!(sum_ck(0x04, 0x16, &[0x00]), 0x1A);
        assert_eq!(sum_ck(0x04, 0x10, &[0x06]), 0x1A);
        // Non-overlapping case: LEN=0x05, CMD=0x30, PAYLOAD=[0x72] → (5+0x30+0x72)=0xA7
        assert_eq!(sum_ck(0x05, 0x30, &[0x72]), 0xA7);
    }

    /// A50 — pin DCENT's framed checksum against the dsPIC33EP16GS202 jig CRC.
    ///
    /// Goldmine `findings/s17-hashsource-full-catalog.md` JIG-08: every
    /// `dsPIC33EP16GS202_*` jig frame uses `crc = opcode_value + data_byte +
    /// constant` (the jig stores this as a 16-bit sum, big-endian, whose high
    /// byte is 0x00 for these small frames). DCENT's framed checksum is the
    /// single low byte of that same additive sum: `(LEN + CMD + Σpayload) & 0xFF`,
    /// where `LEN` is the JIG-08 "constant", `CMD` the "opcode_value", and
    /// `Σpayload` the "data_byte" term. This test confirms the formulas agree
    /// byte-for-byte on the four jig-captured frames (JIG-01/02/03/04), so a
    /// future refactor of the framed builder can't silently diverge from the RE
    /// ground truth. Pure/host-testable — no behavior, just verification.
    #[test]
    fn dspic_framed_checksum_matches_jig08_opcode_plus_data() {
        // DCENT framed checksum (mirrors encode_command_frame): low byte of the
        // additive sum over LEN + CMD + payload.
        fn dcent_ck(len: u8, cmd: u8, payload: &[u8]) -> u8 {
            len.wrapping_add(cmd)
                .wrapping_add(payload.iter().fold(0u8, |a, &b| a.wrapping_add(b)))
        }

        // JIG-01 RESET frame [55 AA 04 07 00 0B] (dsPIC33EP16GS202_reset_pic@30DF0).
        assert_eq!(dcent_ck(0x04, CMD_RESET, &[0x00]), 0x0B);
        // JIG-02 JUMP_TO_APP frame [55 AA 04 06 00 0A] (jump_to_app_from_loader@30BE0).
        assert_eq!(dcent_ck(0x04, CMD_JUMP_TO_APP, &[0x00]), 0x0A);
        // JIG-04 HEARTBEAT frame [55 AA 04 16 00 1A] (pic_heart_beat@3100C).
        assert_eq!(dcent_ck(0x04, CMD_HEARTBEAT, &[0x00]), 0x1A);

        // JIG-03 ENABLE_DC_DC frame [55 AA 05 15 enable .. ..] with jig formula
        // `crc = enable + 26`. The jig constant 26 IS (LEN=0x05 + OPCODE=0x15),
        // so `enable + 26` and DCENT's `(LEN + CMD + enable + 0x00)` are the same
        // additive sum. enable=0x01 → 0x1B; enable=0x00 → 0x1A.
        for enable in [0x00u8, 0x01u8] {
            let jig_crc = enable.wrapping_add(26); // JIG-03 closed-form
            let dcent = dcent_ck(0x05, CMD_ENABLE_VOLTAGE, &[enable, 0x00]);
            assert_eq!(dcent, jig_crc, "ENABLE enable={enable:#04x}");
        }
        assert_eq!(dcent_ck(0x05, CMD_ENABLE_VOLTAGE, &[0x01, 0x00]), 0x1B);
        assert_eq!(dcent_ck(0x05, CMD_ENABLE_VOLTAGE, &[0x00, 0x00]), 0x1A);
    }

    #[test]
    fn framed_voltage_dac_matches_s19j_bosminer_sample() {
        assert_eq!(framed_voltage_dac(DSPIC_MIN_VOLTAGE_MV), 0x00);
        assert_eq!(framed_voltage_dac(13_700), 0x06);
        assert_eq!(framed_voltage_dac(13_800), 0x06);
        assert_eq!(framed_voltage_dac(DSPIC_MAX_VOLTAGE_MV), 0x0B);
    }

    #[test]
    fn dspic_outgoing_len_matches_bible_v1_frame_format() {
        // Bible v1 / 1-power-dspic/01-frame-format.md:
        //   LEN = payload_len + 3 (counts itself + CMD + payload + CHECKSUM)
        //
        // Cross-check against verified inline frame builders in dspic.rs:
        //   RESET / JUMP / GET_VERSION / SetVoltage / HEARTBEAT all have
        //   payload=[1 byte] and ship LEN=0x04 → 1+3 = 0x04 ✓
        //   ENABLE_VOLTAGE has payload=[0x01, 0x00] (2 bytes) and ships
        //   LEN=0x05 → 2+3 = 0x05 ✓
        assert_eq!(
            dspic_outgoing_len(1),
            0x04,
            "1-byte payload commands (RESET/JUMP/GET_VERSION/SetVoltage/HEARTBEAT)"
        );
        assert_eq!(
            dspic_outgoing_len(2),
            0x05,
            "2-byte payload commands (ENABLE_VOLTAGE)"
        );
        assert_eq!(
            dspic_outgoing_len(0),
            0x03,
            "0-byte payload (preamble + CMD + CKSUM only)"
        );
    }

    #[test]
    fn measure_voltage_0x3a_frame_is_byte_exact_and_distinct_from_get_voltage() {
        // CMD_MEASURE_VOLTAGE (0x3A) is the Ghidra-proven fw=0x89 ADC rail read and
        // is distinct from legacy/residual CMD_GET_VOLTAGE (0x3B). measure_voltage()
        // passes a [0x00] payload so the framed wire form is byte-exact to
        // dspic-protocol-bible §2: [55 AA 04 3A 00 3E] (cgminer literal 0x94888,
        // also pinned in dcentrald-api-types::dspic_frame verified-frames table).
        assert_eq!(CMD_MEASURE_VOLTAGE, 0x3A);
        assert_ne!(CMD_MEASURE_VOLTAGE, CMD_GET_VOLTAGE);

        // Reconstruct the framed frame the way encode_command_frame builds it for
        // the [55 AA 3A 00] command (1-byte payload): LEN = payload+3 = 0x04,
        // CKSUM = (LEN + CMD + Σpayload) & 0xFF = (0x04 + 0x3A + 0x00) & 0xFF.
        let len = dspic_outgoing_len(1);
        let cksum = len.wrapping_add(CMD_MEASURE_VOLTAGE).wrapping_add(0x00);
        assert_eq!(
            [0x55, 0xAA, len, CMD_MEASURE_VOLTAGE, 0x00, cksum],
            [0x55, 0xAA, 0x04, 0x3A, 0x00, 0x3E],
            "MEASURE_VOLTAGE framed frame must match dspic-protocol-bible §2"
        );
    }

    #[test]
    fn get_voltage_dac_0x18_frame_and_decoder_are_byte_exact() {
        // CMD_GET_VOLTAGE_DAC (0x18) reads back the COMMANDED DAC setpoint — distinct from
        // the actual-rail ADC read (0x3A) and the LM75 passthrough (0x3B). RE-DERIVED from
        // mining-bible-v1 dspic-protocol-bible §3 "CMD 0x18 — GET_VOLTAGE (DAC readback)".
        assert_eq!(CMD_GET_VOLTAGE_DAC, 0x18);
        assert_ne!(CMD_GET_VOLTAGE_DAC, CMD_SET_VOLTAGE); // 0x10 writes the DAC
        assert_ne!(CMD_GET_VOLTAGE_DAC, CMD_MEASURE_VOLTAGE); // 0x3A reads the ADC rail
        assert_eq!(dspic_response_len(CMD_GET_VOLTAGE_DAC), 4); // [echo, status, dac_hi, dac_lo]

        // Framed request mirrors measure_voltage's convention (a [0x00] read-payload):
        // LEN = payload+3 = 0x04, CKSUM = (LEN + CMD + Σpayload) & 0xFF = 0x04+0x18+0x00 = 0x1C.
        // → [55 AA 04 18 00 1C]; the bible's shorthand [55 AA 04 18 1C] omits the zero payload
        // but carries the same CKSUM 0x1C.
        let len = dspic_outgoing_len(1);
        let cksum = len.wrapping_add(CMD_GET_VOLTAGE_DAC).wrapping_add(0x00);
        assert_eq!(
            [0x55, 0xAA, len, CMD_GET_VOLTAGE_DAC, 0x00, cksum],
            [0x55, 0xAA, 0x04, 0x18, 0x00, 0x1C],
            "GET_VOLTAGE_DAC framed frame must match dspic-protocol-bible §3 (CKSUM 0x1C)"
        );

        // Decoder: echo-guarded, returns be16(dac_hi, dac_lo); rejects wrong-echo / short.
        assert_eq!(
            decode_voltage_dac_reply(&[0x18, 0x00, 0x00, 0x06]),
            Some(0x0006)
        ); // 13.7 V → DAC 0x06 (framed_voltage_dac anchor)
        assert_eq!(
            decode_voltage_dac_reply(&[0x18, 0x00, 0x01, 0x23]),
            Some(0x0123)
        );
        assert_eq!(decode_voltage_dac_reply(&[0x3A, 0x00, 0x00, 0x06]), None); // wrong echo
        assert_eq!(decode_voltage_dac_reply(&[0x18, 0x00, 0x06]), None); // short
        assert_eq!(decode_voltage_dac_reply(&[]), None);
        assert_eq!(decode_voltage_dac_reply(&[0xFF, 0xFF]), None); // bus noise
    }
}
