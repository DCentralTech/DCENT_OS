//! APW121215f UART-tunnel PSU driver — framed protocol on `/dev/i2c-N` @ 0x10.
//!
//! **Used on the AM335x BeagleBone S19j Pro on `S19J_IO_BOARD_V2_0` (the
//! `a lab unit` unit).** This is a *different* protocol from [`crate::psu_apw12_smbus`]
//! (bare SMBus opcodes 0x00-0x10 on CV1835 / Amlogic S19j Pro) — do NOT use
//! one driver for the other board.
//!
//! ## Status — wire frame format LIVE-CONFIRMED (2026-05-12 ftrace on `a lab unit`)
//!
//! A kernel `ftrace` capture on `a lab unit` (events `i2c:i2c_write/read/reply`,
//! `smbus:*`) caught luxminer's actual `/dev/i2c-1 @ 0x10` traffic — see
//!  §"R7-1".
//! The frame layout + the additive checksum + the mandatory write→read delay
//! below are all reproduced byte-exact from that trace. A later Ghidra pass on
//! `luxminer` confirmed the calibration read and watchdog-disable opcodes. What
//! is *still* not known: the **set-voltage opcode/payload** and echo-class
//! opcode `0x83/[0x00,0x00]` — those stay `// TODO` until a deeper Ghidra pass
//! on `antminer_set_voltage_single` / a cold-boot trace on `a lab unit`.
//! The chain *rail* voltage on this board is actually driven by the hashboard
//! dsPIC (i2c-0 @ 0x20/0x21/0x22, cmd 0x10) — the existing am2 dsPIC path —
//! NOT by the APW, so a missing APW set-voltage opcode is not on the critical
//! path; the APW just needs identity + (ideally) watchdog-disable + gpio59.
//!
//! ## Wire protocol (LIVE-CONFIRMED — luxos-wire-capture.md §R7-1)
//!
//! **Request (host → PSU), single I²C write of `4 + params.len() + 3` bytes:**
//!
//! ```text
//!   [0x11, 0x55, 0xAA, LEN, OPCODE, <params…>, CKSUM, 0x00]
//!   LEN   = 4 + params.len()                       (counts LEN..0x00 inclusive)
//!   CKSUM = (LEN + OPCODE + Σ params) & 0xFF        (additive 8-bit sum)
//! ```
//!
//! `0x11` = host-command transport marker (NOT in the reply); `0x55 0xAA` =
//! magic preamble; trailing `0x00` is a constant (high byte of a 16-bit
//! checksum that is always zero in every observed frame — treat as fixed).
//!
//! **Response (PSU → host), a SEPARATE I²C read issued ≥ ~400 ms after the
//! write** (≥ ~1 s for the calibration-block read — the APW is slow). luxminer
//! reads a *fixed* size and the APW pads anything past its actual reply with
//! `0xF5`, so trim trailing `0xF5` before parsing:
//!
//! ```text
//!   [0x55, 0xAA, RLEN, OPCODE_echo, <data…>, CKSUM, 0x00]   (then 0xF5 padding)
//!   RLEN  = 4 + data.len()
//!   CKSUM = (RLEN + OPCODE_echo + Σ data) & 0xFF
//! ```
//!
//! Worked examples from the `a lab unit` trace:
//! - `[0x11,0x55,0xAA,0x04,0x02,0x06,0x00]` (read HW type) → `[0x55,0xAA,0x06,0x02,0x76,0x00,0x7E,0x00]` → HW = `0x76`, `0x06+0x02+0x76+0x00 = 0x7E` ✓
//! - `[0x11,0x55,0xAA,0x04,0x01,0x05,0x00]` (read FW)      → `[0x55,0xAA,0x06,0x01,0x17,0x00,0x1E,0x00]` → FW = `0x17`, `0x06+0x01+0x17+0x00 = 0x1E` ✓
//! - `[0x11,0x55,0xAA,0x04,0x03,0x07,0x00]` (read 0x03)    → `[0x55,0xAA,0x06,0x03,0x23,0x00,0x2C,0x00,0xF5,0xF5]` → value = `0x0023`, `0x06+0x03+0x23+0x00 = 0x2C` ✓
//! - `[0x11,0x55,0xAA,0x06,0x06,0x40,0x20,0x6C,0x00]` (read cal block, params `[0x40,0x20]`) → 40-byte reply `[0x55,0xAA,0x25,0x06,0x40,0xFF×32,0x4B,0x20,0xF5]` (mostly uninitialised on this unit) — `0x06+0x06+0x40+0x20 = 0x6C` ✓
//! - `[0x11,0x55,0xAA,0x06,0x81,0x00,0x00,0x87,0x00]` — watchdog-disable (`0x06+0x81 = 0x87` ✓).
//! - `[0x11,0x55,0xAA,0x06,0x83,0x00,0x00,0x89,0x00]` — echo-replied "set"-class command; semantics still unresolved (`0x06+0x83 = 0x89` ✓).
//!
//! ## PSU init sequence (per analysis/C §7 luxminer strings)
//!
//! 1. read HW type (expect `0x76` = APW121215f) — [`ApwUartTunnel::read_hw_type`]
//! 2. read FW (expect `0x17`) — [`ApwUartTunnel::read_fw_version`]
//! 3. send calibration message (`Sending read calibration message:`) — opcode
//!    `0x06`, params `[0x40,0x20]` (Ghidra-confirmed from `luxminer`
//!    `FUN_0061c4a8`, 2026-05-31)
//! 4. send watchdog-disable message (`Sending PSU watchdog disable message:`) —
//!    opcode `0x81`, params `[0x00,0x00]` (Ghidra-confirmed from `luxminer`
//!    `FUN_0061cd48`, 2026-05-31)
//! 5. **NO PowerOn opcode** — the chain rail comes up via `gpio59` (or is already up); luxminer does `Set power supply pin to ON` via the GPIO, not an APW opcode (matches ).
//! 6. (optional) set chain-rail voltage via the dsPIC, not the APW.
//!
//! ## Host-testable transport
//!
//! All actual I/O goes through the [`ApwUartTunnelBus`] trait so this module
//! is testable with a mock — same pattern as `dcentrald_asic::bm1362::uart_transport`.
//! The runtime impl ([`I2cServiceApwBus`]) routes through
//! [`crate::i2c::I2cServiceHandle`] per the SINGLE-I2C-OWNER architecture; on
//! `a lab unit` the PSU lives on the bit-banged i2c-gpio bus (bus 1).

use crate::HalError;
use crate::Result;
use std::time::Duration;

// ===========================================================================
//  Constants — LIVE-CONFIRMED per luxos-wire-capture.md §R7-1
// ===========================================================================

/// I²C slave address of the APW121215f on `a lab unit` (bus 1, bit-banged i2c-gpio).
pub const APW_UART_TUNNEL_I2C_ADDR: u8 = 0x10;

/// Host-command transport marker — the leading `0x11` of every request frame.
pub const TUNNEL_REG_BYTE: u8 = 0x11;

/// Tunnel magic preamble (`0x55 0xAA`), same for request and response.
pub const TUNNEL_PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// Constant trailing byte of every frame (the always-zero high byte of the
/// 16-bit checksum field; only the low byte — [`checksum`] — is meaningful).
pub const TUNNEL_TRAILER: u8 = 0x00;

/// Largest parameter block representable by the tunnel's 8-bit LEN field.
/// LEN counts itself, OPCODE, checksum, and trailer.
pub const TUNNEL_MAX_PARAMS_LEN: usize = u8::MAX as usize - 4;

/// Pad byte the APW returns for any read bytes past its actual reply length.
pub const TUNNEL_PAD_BYTE: u8 = 0xF5;

/// Minimum delay between the request write and the reply read, in ms. luxminer
/// waits ~250 ms for the I²C `result` then another ~300-500 ms before reading;
/// reading too soon yields all-`0xF5` (the original BEST-GUESS bug). The
/// calibration-block read needs ~1 s — see [`APW_CAL_READ_DELAY_MS`].
pub const APW_READ_DELAY_MS: u64 = 500;

/// Delay before reading back the calibration block (opcode [`OP_READ_CAL_BLOCK`]) —
/// the APW takes ~1.7 s end-to-end for that one in the trace.
pub const APW_CAL_READ_DELAY_MS: u64 = 1200;

/// Expected HW type byte for the APW121215f (live `a lab unit` trace + analysis/C §3).
pub const EXPECTED_HW_TYPE: u8 = 0x76;

/// Expected FW byte for the APW121215f on `a lab unit` (live `a lab unit` trace + analysis/C §3).
pub const EXPECTED_FW_VERSION: u8 = 0x17;

/// Read FW version (`0x01`). Reply value = FW byte (low byte of a u16).
pub const OP_READ_FW_VERSION: u8 = 0x01;

/// Read HW type (`0x02`). Reply value = HW byte (low byte of a u16).
pub const OP_READ_HW_TYPE: u8 = 0x02;

/// Read status word `0x03` — luxminer reads this during init; value `0x0023`
/// (=35) on the `a lab unit` capture. Exact meaning (temp? current? a status code?)
/// not yet RE'd. Reply value = low byte of a u16.
pub const OP_READ_STATUS_0X03: u8 = 0x03;

/// Read calibration block (`0x06`, params `[0x40, 0x20]` = addr/len on the
/// `a lab unit` trace; 34-byte data reply, mostly `0xFF` on that uninitialised unit).
/// Corresponds to luxminer's `Sending read calibration message:`.
pub const OP_READ_CAL_BLOCK: u8 = 0x06;

/// Calibration-block read params seen on `a lab unit` (`[0x40, 0x20]`).
pub const CAL_BLOCK_PARAMS: [u8; 2] = [0x40, 0x20];

// Opcode catalog: the Ghidra pass on 2026-05-31 confirmed `0x06/[0x40,0x20]`
// as the calibration-block read and `0x81/[0x00,0x00]` as watchdog-disable.
// SET-VOLTAGE (`0x83`) is now RE'd byte-exact (RE-ASK-BB-2, 2026-06-02,
// cross-validated against stock BB bmminer `_bitmain_set_DA_conversion_N`):
// the payload is a single DAC byte `N = convert_V_to_N(V)` (per-version linear
// offset - V*slope), the frame is `[55 AA 06 83 N 00 CK16]`, CK16 = 0x89 + N,
// and it is produced byte-exactly by
// `crate::psu_apw12_plus::build_apw121215f_frame(0x83, &[N, 0])` (pinned by
// `apw121215f_frame_matches_s21_jig_re`). So the earlier "value-payload
// encoding unresolved (centivolt vs raw mV vs DAC)" note is SETTLED: it is a
// DAC code. The remaining work for 0x83 is purely the LIVE WIRING (operator
// A/B), not RE — and on this board the dsPIC path below is the preferred
// rail-set anyway.
//
// TODO: opcode catalog incomplete — set-voltage (0x83) is now RE'd (above),
// but two opcodes remain un-RE'd: the `0x03` status-word MEANING (the
// `0x0023`-on-`a lab unit` value's field layout) and the read-voltage opcode
// (`read_voltage_mv`, never exercised by the `a lab unit` warm-restart trace). A
// cold-boot ftrace on `a lab unit` or a deeper Ghidra pass settles those.
// Note: on this board the chain RAIL voltage is set via the hashboard dsPIC
// (i2c-0 @ 0x20/0x21/0x22, cmd 0x10) — the existing am2 dsPIC path — so a
// missing APW set-voltage opcode is NOT on the critical path.

/// Watchdog-control opcode (`0x81`). With params `[0x00,0x00]`, luxminer uses
/// it as watchdog-disable on APW121215f (`FUN_0061cd48`).
pub const OP_WATCHDOG_CONTROL: u8 = 0x81;

/// Watchdog-disable params seen in luxminer and the `a lab unit` ftrace.
pub const WATCHDOG_DISABLE_PARAMS: [u8; 2] = [0x00, 0x00];

// ===========================================================================
//  Pure helpers — host-safe, no I/O
// ===========================================================================

/// Additive 8-bit checksum over `[LEN, OPCODE, params…]` (live-confirmed).
pub fn checksum(len: u8, opcode: u8, params: &[u8]) -> u8 {
    params
        .iter()
        .copied()
        .fold(len.wrapping_add(opcode), u8::wrapping_add)
}

/// Build a request frame for `(opcode, params)`:
/// `[0x11, 0x55, 0xAA, LEN, OPCODE, params…, CKSUM, 0x00]` where
/// `LEN = 4 + params.len()`. Host-safe.
///
/// Returns an error before allocating when `params` cannot be represented by
/// the wire format's 8-bit LEN field.
pub fn build_request(opcode: u8, params: &[u8]) -> Result<Vec<u8>> {
    if params.len() > TUNNEL_MAX_PARAMS_LEN {
        return Err(HalError::PsuProtocolOwned(format!(
            "APW UART-tunnel request parameters are {} bytes; maximum is {}",
            params.len(),
            TUNNEL_MAX_PARAMS_LEN
        )));
    }
    let len = u8::try_from(params.len() + 4).expect("TUNNEL_MAX_PARAMS_LEN proves LEN fits in u8");
    let mut f = Vec::with_capacity(3 + len as usize);
    f.push(TUNNEL_REG_BYTE);
    f.extend_from_slice(&TUNNEL_PREAMBLE);
    f.push(len);
    f.push(opcode);
    f.extend_from_slice(params);
    f.push(checksum(len, opcode, params));
    f.push(TUNNEL_TRAILER);
    Ok(f)
}

/// Convenience: a no-param request frame.
/// Returns the same fallible frame-builder result as [`build_request`].
pub fn build_request_noparams(opcode: u8) -> Result<Vec<u8>> {
    build_request(opcode, &[])
}

/// Parsed APW121215f tunnel response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApwTunnelResponse {
    /// Echoed opcode (response byte index 3).
    pub opcode_echo: u8,
    /// Data bytes (between the echoed opcode and the checksum).
    pub data: Vec<u8>,
}

impl ApwTunnelResponse {
    /// The data interpreted as a little-endian u16 (the common "value" reply
    /// shape — `[lo, hi]`). `0` if `data` is empty; the low byte alone if
    /// `data.len() == 1`.
    pub fn value_le_u16(&self) -> u16 {
        match self.data.len() {
            0 => 0,
            1 => self.data[0] as u16,
            _ => u16::from_le_bytes([self.data[0], self.data[1]]),
        }
    }
}

/// Strip trailing [`TUNNEL_PAD_BYTE`] (`0xF5`) bytes from a raw read.
fn strip_pad(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    while end > 0 && buf[end - 1] == TUNNEL_PAD_BYTE {
        end -= 1;
    }
    &buf[..end]
}

/// Parse + validate a tunnel response. Strips trailing `0xF5` padding, checks
/// the `0x55 0xAA` preamble, the `RLEN` byte (= `4 + data.len()`), and the
/// additive checksum. Returns the echoed opcode + the data bytes.
///
/// Live-confirmed against the `a lab unit` trace's HW/FW/0x03 replies; the
/// calibration-block reply (opcode `0x06`) on that unit has an odd trailing
/// shape (mostly-`0xFF` uninitialised data) — the checksum may not validate
/// there, so the caller can fall back to [`parse_response_lenient`].
pub fn parse_response(buf: &[u8]) -> Result<ApwTunnelResponse> {
    let buf = strip_pad(buf);
    if buf.len() < 6 {
        return Err(HalError::PsuProtocolOwned(format!(
            "APW UART-tunnel response: only {} bytes after stripping 0xF5 padding (need ≥ 6)",
            buf.len()
        )));
    }
    if buf[0] != TUNNEL_PREAMBLE[0] || buf[1] != TUNNEL_PREAMBLE[1] {
        return Err(HalError::PsuProtocolOwned(format!(
            "APW UART-tunnel response: bad preamble {:02X?} (want {:02X?})",
            &buf[0..2],
            TUNNEL_PREAMBLE
        )));
    }
    let rlen = buf[2];
    // RLEN counts RLEN..0x00 inclusive: RLEN(1) + OPCODE(1) + data + CKSUM(1) + TRAILER(1).
    if (rlen as usize) < 4 || (rlen as usize) + 1 > buf.len() {
        return Err(HalError::PsuProtocolOwned(format!(
            "APW UART-tunnel response: RLEN=0x{:02X} doesn't fit in {} bytes",
            rlen,
            buf.len()
        )));
    }
    let opcode_echo = buf[3];
    let data_len = (rlen as usize) - 4;
    let data = buf[4..4 + data_len].to_vec();
    let want_cksum = checksum(rlen, opcode_echo, &data);
    let got_cksum = buf[4 + data_len];
    if got_cksum != want_cksum {
        return Err(HalError::PsuProtocolOwned(format!(
            "APW UART-tunnel response: bad checksum 0x{:02X} (want 0x{:02X}) for \
             opcode_echo=0x{:02X} data={:02X?}",
            got_cksum, want_cksum, opcode_echo, data
        )));
    }
    Ok(ApwTunnelResponse { opcode_echo, data })
}

/// Lenient parse: preamble + RLEN only, no checksum check. For replies whose
/// trailing structure is odd (the `a lab unit` calibration block).
pub fn parse_response_lenient(buf: &[u8]) -> Result<ApwTunnelResponse> {
    let buf = strip_pad(buf);
    if buf.len() < 4 || buf[0] != TUNNEL_PREAMBLE[0] || buf[1] != TUNNEL_PREAMBLE[1] {
        return Err(HalError::PsuProtocolOwned(format!(
            "APW UART-tunnel response (lenient): bad preamble / too short ({:02X?})",
            buf
        )));
    }
    let rlen = buf[2] as usize;
    let opcode_echo = buf[3];
    let data_end = (4 + rlen.saturating_sub(4)).min(buf.len());
    let data = buf[4..data_end].to_vec();
    Ok(ApwTunnelResponse { opcode_echo, data })
}

// ===========================================================================
//  Bus trait — host-testable transport
// ===========================================================================

/// Abstract I²C transport for the APW UART-tunnel PSU.
///
/// The runtime impl ([`I2cServiceApwBus`]) routes through
/// [`crate::i2c::I2cServiceHandle`]. Tests use a mock that records the frames
/// written and replies with canned responses.
///
/// **Important: the APW requires a write, a caller-controlled delay, and then
/// a separate read** — it needs ≥ ~400 ms to produce a reply (see
/// [`APW_READ_DELAY_MS`]). Service-backed implementations retain worker
/// ownership across that sequence, but must not collapse it into a
/// repeated-START WriteRead (read-too-soon → all `0xF5`).
pub trait ApwUartTunnelBus {
    /// Write `frame` to the PSU at `addr`.
    fn write_frame(&mut self, addr: u8, frame: &[u8]) -> Result<()>;
    /// Read `read_len` bytes from the PSU at `addr`.
    fn read_reply(&mut self, addr: u8, read_len: usize) -> Result<Vec<u8>>;
    /// Execute one correlated request/delay/reply exchange. The default keeps
    /// mocks and non-service transports simple; the service-backed runtime
    /// overrides this to retain single-worker ownership across the delay.
    fn exchange(
        &mut self,
        addr: u8,
        frame: &[u8],
        read_len: usize,
        delay: Duration,
    ) -> Result<Vec<u8>> {
        self.write_frame(addr, frame)?;
        self.delay(delay);
        self.read_reply(addr, read_len)
    }
    /// Sleep for `dur` (overridable in tests so they don't actually wait).
    fn delay(&mut self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

#[cfg(feature = "recovery-tool")]
mod i2c_service_bus {
    //! Runtime [`super::ApwUartTunnelBus`] impl over a real I²C service handle.
    //! Gated by the `recovery-tool` feature — not yet exercised on production
    //! hardware (no live `a lab unit` DCENT_OS install).

    use super::{ApwUartTunnelBus, Result};
    use crate::i2c::{I2cOperationIntent, I2cServiceHandle, I2cTransactionStep};

    /// [`ApwUartTunnelBus`] backed by the shared I²C service.
    pub struct I2cServiceApwBus {
        handle: I2cServiceHandle,
    }

    impl I2cServiceApwBus {
        pub fn new(handle: I2cServiceHandle) -> Self {
            Self { handle }
        }
    }

    fn validated_request(frame: &[u8]) -> Option<(u8, &[u8])> {
        if frame.len() < 7
            || frame[0] != super::TUNNEL_REG_BYTE
            || frame[1..3] != super::TUNNEL_PREAMBLE
            || frame.last() != Some(&0x00)
            || frame.len() != usize::from(frame[3]) + 3
        {
            return None;
        }
        let opcode = frame[4];
        let params = &frame[5..frame.len() - 2];
        (frame[frame.len() - 2] == super::checksum(frame[3], opcode, params))
            .then_some((opcode, params))
    }

    fn frame_intent(frame: &[u8]) -> I2cOperationIntent {
        match validated_request(frame) {
            // Watchdog disable is standalone neutral control: it removes a
            // cutoff without itself moving the rail safe.
            Some((opcode, params))
                if opcode == super::OP_WATCHDOG_CONTROL
                    && params == super::WATCHDOG_DISABLE_PARAMS =>
            {
                I2cOperationIntent::NeutralControl
            }
            // The gated voltage opcode can raise the rail if this recovery
            // transport is ever enabled for it.
            Some((0x83, _)) => I2cOperationIntent::Energize,
            Some((opcode, params))
                if params.is_empty()
                    && matches!(
                        opcode,
                        super::OP_READ_HW_TYPE
                            | super::OP_READ_FW_VERSION
                            | super::OP_READ_STATUS_0X03
                    ) =>
            {
                I2cOperationIntent::ReadOnly
            }
            Some((opcode, params))
                if opcode == super::OP_READ_CAL_BLOCK && params == super::CAL_BLOCK_PARAMS =>
            {
                I2cOperationIntent::ReadOnly
            }
            // The opcode catalog is intentionally incomplete. Unknown
            // commands must never inherit read-only terminal privilege.
            _ => I2cOperationIntent::UnclassifiedMutation,
        }
    }

    impl ApwUartTunnelBus for I2cServiceApwBus {
        fn write_frame(&mut self, addr: u8, frame: &[u8]) -> Result<()> {
            let intent = frame_intent(frame);
            self.handle.transaction_with_intent(
                intent,
                addr,
                vec![I2cTransactionStep::Write(frame.to_vec())],
            )?;
            Ok(())
        }

        fn read_reply(&mut self, addr: u8, read_len: usize) -> Result<Vec<u8>> {
            let mut reads = self.handle.transaction_with_intent(
                I2cOperationIntent::ReadOnly,
                addr,
                vec![I2cTransactionStep::Read(read_len)],
            )?;
            reads.pop().ok_or_else(|| {
                crate::HalError::PsuProtocolOwned(
                    "APW UART-tunnel: read transaction returned no result".into(),
                )
            })
        }

        fn exchange(
            &mut self,
            addr: u8,
            frame: &[u8],
            read_len: usize,
            delay: std::time::Duration,
        ) -> Result<Vec<u8>> {
            let intent = frame_intent(frame);
            let delay_ms = u64::try_from(delay.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX);
            let mut reads = self.handle.transaction_with_intent(
                intent,
                addr,
                vec![
                    I2cTransactionStep::Write(frame.to_vec()),
                    I2cTransactionStep::SleepMs(delay_ms),
                    I2cTransactionStep::Read(read_len),
                ],
            )?;
            reads.pop().ok_or_else(|| {
                crate::HalError::PsuProtocolOwned(
                    "APW UART-tunnel: exchange returned no read result".into(),
                )
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn classifies_only_byte_exact_known_requests_as_privileged() {
            assert_eq!(
                frame_intent(
                    &super::super::build_request(super::super::OP_READ_HW_TYPE, &[]).unwrap()
                ),
                I2cOperationIntent::ReadOnly
            );
            assert_eq!(
                frame_intent(
                    &super::super::build_request(
                        super::super::OP_READ_CAL_BLOCK,
                        &super::super::CAL_BLOCK_PARAMS
                    )
                    .unwrap()
                ),
                I2cOperationIntent::ReadOnly
            );
            assert_eq!(
                frame_intent(
                    &super::super::build_request(
                        super::super::OP_WATCHDOG_CONTROL,
                        &super::super::WATCHDOG_DISABLE_PARAMS
                    )
                    .unwrap()
                ),
                I2cOperationIntent::NeutralControl
            );
            assert_eq!(
                frame_intent(&super::super::build_request(0x83, &[0x2A, 0x00]).unwrap()),
                I2cOperationIntent::Energize
            );
        }

        #[test]
        fn malformed_or_semantically_unknown_requests_are_unclassified_mutations() {
            let valid_read =
                super::super::build_request(super::super::OP_READ_HW_TYPE, &[]).unwrap();

            let mut bad_checksum = valid_read.clone();
            bad_checksum[5] ^= 0x01;
            assert_eq!(
                frame_intent(&bad_checksum),
                I2cOperationIntent::UnclassifiedMutation
            );

            let mut bad_length = valid_read.clone();
            bad_length[3] = bad_length[3].saturating_add(1);
            assert_eq!(
                frame_intent(&bad_length),
                I2cOperationIntent::UnclassifiedMutation
            );

            assert_eq!(
                frame_intent(&super::super::build_request(0x7F, &[]).unwrap()),
                I2cOperationIntent::UnclassifiedMutation
            );
            assert_eq!(
                frame_intent(
                    &super::super::build_request(super::super::OP_READ_HW_TYPE, &[0x00]).unwrap()
                ),
                I2cOperationIntent::UnclassifiedMutation
            );
            assert_eq!(
                frame_intent(
                    &super::super::build_request(super::super::OP_WATCHDOG_CONTROL, &[0x01, 0x00])
                        .unwrap()
                ),
                I2cOperationIntent::UnclassifiedMutation
            );
        }
    }
}

#[cfg(feature = "recovery-tool")]
pub use i2c_service_bus::I2cServiceApwBus;

// ===========================================================================
//  ApwUartTunnel — the runtime controller
// ===========================================================================

/// APW121215f UART-tunnel PSU controller for the AM335x BB S19j Pro.
///
/// Generic over [`ApwUartTunnelBus`] so it is host-testable. The runtime path
/// constructs it with an [`I2cServiceApwBus`] (feature `recovery-tool`) bound
/// to bus 1, address [`APW_UART_TUNNEL_I2C_ADDR`].
pub struct ApwUartTunnel<B: ApwUartTunnelBus> {
    bus: B,
    address: u8,
    hw_type: u8,
    fw_version: u8,
}

impl<B: ApwUartTunnelBus> ApwUartTunnel<B> {
    /// Construct at the default address ([`APW_UART_TUNNEL_I2C_ADDR`]).
    pub fn new(bus: B) -> Self {
        Self::new_at(bus, APW_UART_TUNNEL_I2C_ADDR)
    }

    /// Construct at a specific slave address (rare).
    pub fn new_at(bus: B, address: u8) -> Self {
        Self {
            bus,
            address,
            hw_type: 0,
            fw_version: 0,
        }
    }

    /// Cached I²C slave address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Last observed HW type byte (`0` until [`Self::read_hw_type`]).
    pub fn hw_type(&self) -> u8 {
        self.hw_type
    }

    /// Last observed FW version byte (`0` until [`Self::read_fw_version`]).
    pub fn fw_version(&self) -> u8 {
        self.fw_version
    }

    /// Low-level: write a `(opcode, params)` frame, wait `delay`, read
    /// `read_len` bytes, strip `0xF5` padding, parse + checksum-verify.
    fn command(
        &mut self,
        opcode: u8,
        params: &[u8],
        read_len: usize,
        delay: Duration,
    ) -> Result<ApwTunnelResponse> {
        let frame = build_request(opcode, params)?;
        let raw = self.bus.exchange(self.address, &frame, read_len, delay)?;
        let resp = parse_response(&raw)?;
        if resp.opcode_echo != opcode {
            return Err(HalError::PsuProtocolOwned(format!(
                "APW UART-tunnel: opcode echo mismatch (sent 0x{:02X}, got 0x{:02X})",
                opcode, resp.opcode_echo
            )));
        }
        Ok(resp)
    }

    /// Read the PSU HW type byte. Expect [`EXPECTED_HW_TYPE`] (`0x76`).
    pub fn read_hw_type(&mut self) -> Result<u8> {
        let resp = self.command(
            OP_READ_HW_TYPE,
            &[],
            8,
            Duration::from_millis(APW_READ_DELAY_MS),
        )?;
        let v = resp.value_le_u16() as u8;
        self.hw_type = v;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            hw = format_args!("0x{:02X}", v),
            "APW UART-tunnel: HW type read"
        );
        Ok(v)
    }

    /// Read the PSU FW version byte. Expect [`EXPECTED_FW_VERSION`] (`0x17`).
    pub fn read_fw_version(&mut self) -> Result<u8> {
        let resp = self.command(
            OP_READ_FW_VERSION,
            &[],
            8,
            Duration::from_millis(APW_READ_DELAY_MS),
        )?;
        let v = resp.value_le_u16() as u8;
        self.fw_version = v;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            fw = format_args!("0x{:02X}", v),
            "APW UART-tunnel: FW version read"
        );
        Ok(v)
    }

    /// Read the opcode-`0x03` status word (meaning not yet RE'd; `0x0023` on
    /// the `a lab unit` capture). Returned as a raw little-endian u16.
    pub fn read_status_0x03(&mut self) -> Result<u16> {
        let resp = self.command(
            OP_READ_STATUS_0X03,
            &[],
            10,
            Duration::from_millis(APW_READ_DELAY_MS),
        )?;
        Ok(resp.value_le_u16())
    }

    /// Probe HW + FW and confirm they match the expected APW121215f signature
    /// (cold-boot step 3 (a)/(b)). HW mismatch is fatal; FW mismatch logs a
    /// warning and continues (a different APW revision may be fine).
    pub fn probe_identity(&mut self) -> Result<()> {
        let hw = self.read_hw_type()?;
        if hw != EXPECTED_HW_TYPE {
            return Err(HalError::PsuUnsupported(format!(
                "APW UART-tunnel: HW type 0x{:02X}, expected 0x{:02X} (APW121215f)",
                hw, EXPECTED_HW_TYPE
            )));
        }
        let fw = self.read_fw_version()?;
        if fw != EXPECTED_FW_VERSION {
            tracing::warn!(
                addr = format_args!("0x{:02X}", self.address),
                fw = format_args!("0x{:02X}", fw),
                expected = format_args!("0x{:02X}", EXPECTED_FW_VERSION),
                "APW UART-tunnel: FW version differs from expected — continuing"
            );
        }
        Ok(())
    }

    /// Read the calibration block (opcode [`OP_READ_CAL_BLOCK`], params
    /// [`CAL_BLOCK_PARAMS`]). Uses the lenient parser (the `a lab unit` reply has an
    /// odd trailing shape). Returns the raw data bytes — mostly `0xFF`
    /// (uninitialised) on the `a lab unit` unit; on a calibrated PSU this is the
    /// voltage/current trim table. Best-effort: never fatal.
    pub fn read_calibration_block(&mut self) -> Result<Vec<u8>> {
        let frame = build_request(OP_READ_CAL_BLOCK, &CAL_BLOCK_PARAMS)?;
        let raw = self.bus.exchange(
            self.address,
            &frame,
            40,
            Duration::from_millis(APW_CAL_READ_DELAY_MS),
        )?;
        let resp = parse_response_lenient(&raw)?;
        if resp.opcode_echo != OP_READ_CAL_BLOCK {
            return Err(HalError::PsuProtocolOwned(format!(
                "APW UART-tunnel cal block: opcode echo 0x{:02X} (want 0x{:02X})",
                resp.opcode_echo, OP_READ_CAL_BLOCK
            )));
        }
        Ok(resp.data)
    }

    /// Send the calibration message (cold-boot step 3 (c)).
    ///
    /// luxminer's `Sending read calibration message:` is the
    /// [`OP_READ_CAL_BLOCK`] read (Ghidra-confirmed `FUN_0061c4a8`). This keeps
    /// the call best-effort because the `a lab unit` calibration block has an odd
    /// mostly-`0xFF` shape and calibration data is not critical to the board
    /// reset/ASIC bring-up path.
    pub fn send_calibration_message(&mut self) -> Result<()> {
        match self.read_calibration_block() {
            Ok(data) => {
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    bytes = data.len(),
                    all_ff = data.iter().all(|&b| b == 0xFF),
                    "APW UART-tunnel: calibration block read (best-effort)"
                );
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "APW UART-tunnel: calibration-block read failed — continuing (best-effort)"
                );
                Ok(())
            }
        }
    }

    /// Send the watchdog-disable message (cold-boot step 3 (d)).
    ///
    /// Ghidra-confirmed from luxminer `FUN_0061cd48`: opcode
    /// [`OP_WATCHDOG_CONTROL`] with [`WATCHDOG_DISABLE_PARAMS`]. The caller may
    /// still choose to treat failures as non-fatal on `a lab unit` because the APW rail
    /// is normally already enabled and the watchdog interval is minutes-scale.
    pub fn send_watchdog_disable_message(&mut self) -> Result<()> {
        self.command(
            OP_WATCHDOG_CONTROL,
            &WATCHDOG_DISABLE_PARAMS,
            8,
            Duration::from_millis(APW_READ_DELAY_MS),
        )?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            opcode = format_args!("0x{:02X}", OP_WATCHDOG_CONTROL),
            "APW UART-tunnel: watchdog-disable acknowledged"
        );
        Ok(())
    }

    /// Set the chain-rail voltage in millivolts.
    ///
    /// **Intentional gated stub — and on this board NOT the right place.** On
    /// `S19J_IO_BOARD_V2_0` the chain *rail* voltage is set via the hashboard
    /// dsPIC (i2c-0 @ 0x20/0x21/0x22, cmd 0x10) — the existing am2 dsPIC path —
    /// not via the APW. luxminer's APW `Sending message to write voltage:` is
    /// the "Loki bypass" path (only used when the dsPIC voltage controllers are
    /// absent). Its frame is now RE'd byte-exact (RE-ASK-BB-2): opcode `0x83`,
    /// payload a single DAC byte `N`, `[55 AA 06 83 N 00 CK16]`,
    /// `CK16 = 0x89 + N` — see `crate::psu_apw12_plus::build_apw121215f_frame`.
    /// This stub stays `PsuUnsupported` ON PURPOSE: wiring it drives a live PSU
    /// rail, so it is operator-A/B-gated, and the dsPIC path is preferred on
    /// this board. Returns [`HalError::PsuUnsupported`] so a caller that reaches
    /// here by mistake fails loudly rather than driving the APW rail unbidden.
    pub fn set_voltage_mv(&mut self, mv: u16) -> Result<()> {
        Err(HalError::PsuUnsupported(format!(
            "APW UART-tunnel set_voltage_mv({}): intentionally unwired — on S19J_IO_BOARD_V2_0 the \
             chain rail is set via the hashboard dsPIC (i2c-0 0x20/0x21/0x22 cmd 0x10), not the \
             APW. The APW set-voltage frame IS RE'd (0x83 DAC-byte, build_apw121215f_frame); \
             wiring it is an operator-gated live step, not a missing-RE blocker.",
            mv,
        )))
    }

    /// Read back the current chain-rail voltage in millivolts.
    ///
    /// **Stub — opcode not yet RE'd.** (Same caveat as [`Self::set_voltage_mv`]:
    /// rail telemetry on this board is more naturally read from the dsPIC.)
    pub fn read_voltage_mv(&mut self) -> Result<u16> {
        Err(HalError::PsuUnsupported(
            "APW UART-tunnel read_voltage_mv: not implemented — opcode not yet RE'd (the `a lab unit` \
             warm-restart trace didn't exercise it). See the `// TODO` opcode catalog."
                .into(),
        ))
    }
}

// ===========================================================================
//  Tests — all host-safe (no real I/O; mock bus skips the delays)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock bus: records the frames written, replies with canned responses,
    /// and skips the inter-message delay so tests don't actually sleep.
    struct MockBus {
        replies: std::collections::VecDeque<Result<Vec<u8>>>,
        writes: Vec<(u8, Vec<u8>)>,
        reads: Vec<(u8, usize)>,
        delays: Vec<Duration>,
    }

    impl MockBus {
        fn new(replies: Vec<Result<Vec<u8>>>) -> Self {
            Self {
                replies: replies.into_iter().collect(),
                writes: Vec::new(),
                reads: Vec::new(),
                delays: Vec::new(),
            }
        }
    }

    impl ApwUartTunnelBus for MockBus {
        fn write_frame(&mut self, addr: u8, frame: &[u8]) -> Result<()> {
            self.writes.push((addr, frame.to_vec()));
            Ok(())
        }
        fn read_reply(&mut self, addr: u8, read_len: usize) -> Result<Vec<u8>> {
            self.reads.push((addr, read_len));
            self.replies.pop_front().unwrap_or(Ok(vec![0; read_len]))
        }
        fn delay(&mut self, dur: Duration) {
            self.delays.push(dur); // record, don't sleep
        }
    }

    /// Build a valid reply frame for `(opcode_echo, data)` and optionally pad
    /// it out to `pad_to` bytes with `0xF5` (mimics the APW's fixed-size read).
    fn reply_frame(opcode_echo: u8, data: &[u8], pad_to: usize) -> Vec<u8> {
        let rlen = (4 + data.len()) as u8;
        let mut f = vec![TUNNEL_PREAMBLE[0], TUNNEL_PREAMBLE[1], rlen, opcode_echo];
        f.extend_from_slice(data);
        f.push(checksum(rlen, opcode_echo, data));
        f.push(TUNNEL_TRAILER);
        while f.len() < pad_to {
            f.push(TUNNEL_PAD_BYTE);
        }
        f
    }

    #[test]
    fn constants_match_capture() {
        assert_eq!(APW_UART_TUNNEL_I2C_ADDR, 0x10);
        assert_eq!(TUNNEL_REG_BYTE, 0x11);
        assert_eq!(TUNNEL_PREAMBLE, [0x55, 0xAA]);
        assert_eq!(TUNNEL_TRAILER, 0x00);
        assert_eq!(TUNNEL_PAD_BYTE, 0xF5);
        assert_eq!(EXPECTED_HW_TYPE, 0x76);
        assert_eq!(EXPECTED_FW_VERSION, 0x17);
        assert_eq!(OP_READ_FW_VERSION, 0x01);
        assert_eq!(OP_READ_HW_TYPE, 0x02);
        assert_eq!(OP_READ_CAL_BLOCK, 0x06);
        assert_eq!(CAL_BLOCK_PARAMS, [0x40, 0x20]);
        assert_eq!(OP_WATCHDOG_CONTROL, 0x81);
        assert_eq!(WATCHDOG_DISABLE_PARAMS, [0x00, 0x00]);
    }

    #[test]
    fn build_request_byte_exact_against_dot79_trace() {
        // luxminer's actual frames from luxos-wire-capture.md §R7-1:
        assert_eq!(
            build_request(0x02, &[]).unwrap(),
            vec![0x11, 0x55, 0xAA, 0x04, 0x02, 0x06, 0x00]
        );
        assert_eq!(
            build_request(0x01, &[]).unwrap(),
            vec![0x11, 0x55, 0xAA, 0x04, 0x01, 0x05, 0x00]
        );
        assert_eq!(
            build_request(0x03, &[]).unwrap(),
            vec![0x11, 0x55, 0xAA, 0x04, 0x03, 0x07, 0x00]
        );
        assert_eq!(
            build_request(0x06, &[0x40, 0x20]).unwrap(),
            vec![0x11, 0x55, 0xAA, 0x06, 0x06, 0x40, 0x20, 0x6C, 0x00]
        );
        assert_eq!(
            build_request(0x81, &[0x00, 0x00]).unwrap(),
            vec![0x11, 0x55, 0xAA, 0x06, 0x81, 0x00, 0x00, 0x87, 0x00]
        );
        assert_eq!(
            build_request(0x83, &[0x00, 0x00]).unwrap(),
            vec![0x11, 0x55, 0xAA, 0x06, 0x83, 0x00, 0x00, 0x89, 0x00]
        );
    }

    #[test]
    fn checksum_additive_matches_trace() {
        assert_eq!(checksum(0x04, 0x02, &[]), 0x06);
        assert_eq!(checksum(0x04, 0x01, &[]), 0x05);
        assert_eq!(checksum(0x04, 0x03, &[]), 0x07);
        assert_eq!(checksum(0x06, 0x06, &[0x40, 0x20]), 0x6C);
        assert_eq!(checksum(0x06, 0x81, &[0x00, 0x00]), 0x87);
        // wrap test
        assert_eq!(checksum(0xFF, 0xFF, &[0x02]), 0x00);

        // Public checksum input is not length-constrained. Its arithmetic is
        // explicitly modulo 256 even for inputs larger than any valid frame.
        let long = vec![0xFF; 1_024];
        let expected = long
            .iter()
            .copied()
            .fold(0xFEu8, |sum, byte| sum.wrapping_add(byte));
        assert_eq!(checksum(0xFF, 0xFF, &long), expected);
    }

    #[test]
    fn build_request_enforces_u8_length_boundary() {
        let params = vec![0xFF; TUNNEL_MAX_PARAMS_LEN];
        let frame = build_request(0xFF, &params).unwrap();
        assert_eq!(frame[3], u8::MAX);
        assert_eq!(frame.len(), TUNNEL_MAX_PARAMS_LEN + 7);
        assert_eq!(frame[frame.len() - 2], checksum(u8::MAX, 0xFF, &params));
        assert_eq!(frame[frame.len() - 1], TUNNEL_TRAILER);

        let oversized = vec![0xFF; TUNNEL_MAX_PARAMS_LEN + 1];
        assert!(build_request(0xFF, &oversized).is_err());
    }

    #[test]
    fn parse_response_against_dot79_trace() {
        // [55 aa 06 02 76 00 7e 00] — HW = 0x76
        let r = parse_response(&[0x55, 0xAA, 0x06, 0x02, 0x76, 0x00, 0x7E, 0x00]).unwrap();
        assert_eq!(r.opcode_echo, 0x02);
        assert_eq!(r.data, vec![0x76, 0x00]);
        assert_eq!(r.value_le_u16(), 0x0076);
        // [55 aa 06 01 17 00 1e 00] — FW = 0x17
        let r = parse_response(&[0x55, 0xAA, 0x06, 0x01, 0x17, 0x00, 0x1E, 0x00]).unwrap();
        assert_eq!(r.value_le_u16(), 0x0017);
        // [55 aa 06 03 23 00 2c 00 f5 f5] — opcode 0x03, value 0x0023, padded
        let r =
            parse_response(&[0x55, 0xAA, 0x06, 0x03, 0x23, 0x00, 0x2C, 0x00, 0xF5, 0xF5]).unwrap();
        assert_eq!(r.opcode_echo, 0x03);
        assert_eq!(r.value_le_u16(), 0x0023);
        // echo-class set command [55 aa 06 81 00 00 87 00]
        let r = parse_response(&[0x55, 0xAA, 0x06, 0x81, 0x00, 0x00, 0x87, 0x00]).unwrap();
        assert_eq!(r.opcode_echo, 0x81);
        assert_eq!(r.data, vec![0x00, 0x00]);
    }

    #[test]
    fn parse_response_strips_pad_and_rejects_garbage() {
        // all-0xF5 (the "read too soon" bug symptom) → error, not a silent ""
        assert!(parse_response(&[0xF5; 8]).is_err());
        assert!(parse_response(&[]).is_err());
        // bad preamble
        assert!(parse_response(&[0xDE, 0xAD, 0x06, 0x02, 0x76, 0x00, 0x7E, 0x00]).is_err());
        // bad checksum
        assert!(parse_response(&[0x55, 0xAA, 0x06, 0x02, 0x76, 0x00, 0x99, 0x00]).is_err());
    }

    #[test]
    fn parse_response_lenient_handles_cal_block() {
        // [55 aa 25 06 40 ff×32 4b 20 f5] — the .79 calibration block
        let mut raw = vec![0x55, 0xAA, 0x25, 0x06, 0x40];
        raw.extend(std::iter::repeat(0xFF).take(32));
        raw.extend_from_slice(&[0x4B, 0x20, 0xF5]);
        let r = parse_response_lenient(&raw).unwrap();
        assert_eq!(r.opcode_echo, 0x06);
        // RLEN=0x25=37 → data_len=33; data = [0x40, 0xFF×32]
        assert_eq!(r.data.len(), 33);
        assert_eq!(r.data[0], 0x40);
        assert!(r.data[1..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn read_hw_type_round_trips_and_waits() {
        let mut psu =
            ApwUartTunnel::new(MockBus::new(vec![Ok(reply_frame(0x02, &[0x76, 0x00], 8))]));
        assert_eq!(psu.read_hw_type().unwrap(), 0x76);
        assert_eq!(psu.hw_type(), 0x76);
        // The command must have written the frame, delayed, then read.
        assert_eq!(psu.bus.writes.len(), 1);
        assert_eq!(psu.bus.writes[0].1, build_request(0x02, &[]).unwrap());
        assert_eq!(
            psu.bus.delays,
            vec![Duration::from_millis(APW_READ_DELAY_MS)]
        );
        assert_eq!(psu.bus.reads, vec![(0x10, 8)]);
    }

    #[test]
    fn read_fw_version_round_trips() {
        let mut psu =
            ApwUartTunnel::new(MockBus::new(vec![Ok(reply_frame(0x01, &[0x17, 0x00], 8))]));
        assert_eq!(psu.read_fw_version().unwrap(), 0x17);
        assert_eq!(psu.fw_version(), 0x17);
    }

    #[test]
    fn probe_identity_accepts_expected_signature() {
        let mut psu = ApwUartTunnel::new(MockBus::new(vec![
            Ok(reply_frame(0x02, &[0x76, 0x00], 8)),
            Ok(reply_frame(0x01, &[0x17, 0x00], 8)),
        ]));
        psu.probe_identity().expect("APW121215f signature");
    }

    #[test]
    fn probe_identity_rejects_wrong_hw() {
        let mut psu =
            ApwUartTunnel::new(MockBus::new(vec![Ok(reply_frame(0x02, &[0x99, 0x00], 8))]));
        assert!(psu.probe_identity().is_err());
    }

    #[test]
    fn command_rejects_opcode_echo_mismatch() {
        let mut psu =
            ApwUartTunnel::new(MockBus::new(vec![Ok(reply_frame(0xFF, &[0x76, 0x00], 8))]));
        assert!(psu.read_hw_type().is_err());
    }

    #[test]
    fn calibration_is_best_effort_nonfatal() {
        // calibration: even if the read errors, send_calibration_message returns Ok
        let mut psu = ApwUartTunnel::new(MockBus::new(vec![Err(HalError::PsuProtocolOwned(
            "x".into(),
        ))]));
        assert!(psu.send_calibration_message().is_ok());
    }

    #[test]
    fn watchdog_disable_uses_ghidra_confirmed_frame() {
        let mut psu = ApwUartTunnel::new(MockBus::new(vec![Ok(reply_frame(
            OP_WATCHDOG_CONTROL,
            &[0x00, 0x00],
            8,
        ))]));
        assert!(psu.send_watchdog_disable_message().is_ok());
        assert_eq!(psu.bus.writes.len(), 1);
        assert_eq!(
            psu.bus.writes[0].1,
            vec![0x11, 0x55, 0xAA, 0x06, 0x81, 0x00, 0x00, 0x87, 0x00]
        );
        assert_eq!(
            psu.bus.delays,
            vec![Duration::from_millis(APW_READ_DELAY_MS)]
        );
        assert_eq!(psu.bus.reads, vec![(0x10, 8)]);
    }

    #[test]
    fn calibration_block_read_uses_long_delay_and_lenient_parse() {
        let mut raw = vec![0x55, 0xAA, 0x25, 0x06, 0x40];
        raw.extend(std::iter::repeat(0xFF).take(32));
        raw.extend_from_slice(&[0x4B, 0x20, 0xF5]);
        let mut psu = ApwUartTunnel::new(MockBus::new(vec![Ok(raw)]));
        let data = psu.read_calibration_block().unwrap();
        assert_eq!(data.len(), 33);
        assert_eq!(
            psu.bus.delays,
            vec![Duration::from_millis(APW_CAL_READ_DELAY_MS)]
        );
    }

    #[test]
    fn set_voltage_and_read_voltage_are_dspic_not_apw_stubs() {
        let mut psu = ApwUartTunnel::new(MockBus::new(vec![]));
        assert!(psu.set_voltage_mv(15000).is_err());
        assert!(psu.read_voltage_mv().is_err());
    }

    /// Regression-pin: the opcode catalog is still incomplete, so the source
    /// MUST carry the `// TODO: opcode catalog incomplete` marker. As of
    /// RE-ASK-BB-2 (2026-06-02) the set-voltage opcode `0x83` IS RE'd
    /// (DAC-byte frame, `build_apw121215f_frame`), so the marker now scopes to
    /// the genuinely-unresolved opcodes: the `0x03` status-word meaning and the
    /// read-voltage opcode. The frame format, calibration read, watchdog, and
    /// set-voltage frame are confirmed; the status/read-voltage semantics are not.
    #[test]
    fn opcode_catalog_todo_present() {
        let src = include_str!("psu_apw_uart_tunnel.rs");
        let test_mod_start = src.find("#[cfg(test)]").expect("test module start");
        let content_only = &src[..test_mod_start];
        assert!(
            content_only.contains("TODO: opcode catalog incomplete"),
            "the opcode-catalog-incomplete TODO must stay in source until the status-word (0x03) \
             and read-voltage opcode semantics are RE'd"
        );
        // RE-ASK-BB-2 closed the set-voltage opcode — assert the doc records it
        // (so a future edit doesn't regress it back to an un-RE'd guess).
        assert!(
            content_only.contains("RE-ASK-BB-2"),
            "the doc must record that set-voltage (0x83) is RE'd per RE-ASK-BB-2"
        );
        // The framing is now LIVE-CONFIRMED — assert the doc says so (so a
        // future edit doesn't accidentally re-label it a guess).
        assert!(
            content_only.contains("LIVE-CONFIRMED"),
            "the module doc must record that the frame format is live-confirmed from the .79 ftrace"
        );
    }
}
