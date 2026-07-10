//! APW12+ register-based PSU driver — I2C `/dev/i2c-N` @ 0x10.
//!
//! Used on **S21, S21 Pro, S21 XP** (Amlogic A113D, 4000W+ class). The S21
//! family is **NoPic**: chip voltage is set by TAS5782M audio DACs, NOT by
//! a PIC1704 or dsPIC. APW12+ controls the **rail-side power** (12 V bulk
//! supply, on/off, telemetry, fault clearing, AC power-limit ceiling). It
//! does NOT set per-chain ASIC voltage.
//!
//! Three PSU drivers coexist in `dcentrald-hal`. They are NOT
//! interchangeable — picking the wrong one will silently EIO at best and
//! corrupt PSU firmware state at worst:
//!
//! | Driver | Wire protocol | Platforms | Module |
//! |--------|----------------|-----------|--------|
//! | `Apw121215a` | `[55 AA LEN CMD ... SUM]` framed | Zynq am2 (S19 / S19j Pro / S19j XP) | [`crate::psu`] |
//! | `Apw12SmbusBackend` | SMBus opcode + payload (17 opcodes) | CV1835 / AM335x BB / Amlogic S19j Pro | [`crate::psu_apw12_smbus`] |
//! | `Apw12PlusBackend` | I2C register read/write (this module) | S21 / S21 Pro / S21 XP (Amlogic A113D) | THIS FILE |
//!
//! All three sit at slave address 0x10 — that's the trap. The slave address
//! alone tells you nothing about which protocol speaks behind it. The
//! authoritative gate is platform classification (subtype string + i2cdetect
//! probe) plus the sealed-trait whitelist on `Apw12PlusBackend::new`.
//!
//! # Reference
//!
//! - `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/DCENT_OS_HARDWARE_CATALOG.md`
//!   §5.1 line 475 — `APW12+ … 0x10 … Register-based (differs!) … 4000W+ …
//!   GPIO 907 … I2C register … S21, S21 Pro, S21 XP … CONFIRMED`
//! - `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/DCENT_OS_HARDWARE_CATALOG.md`
//!   §5.3 lines 505-527 — full register map (this is the authoritative
//!   table mirrored verbatim below).
//! - `DCENT_OS_ §6 Q10 — confirms S21 Amlogic is **NoPic**
//!   (TAS5782M DACs, no PIC1704), and §13.3 reiterates "register-based vs
//!   opcode-based" distinction at the protocol layer.
//!
//! # Wire protocol
//!
//! Plain SMBus-style register read/write at slave address 0x10:
//!
//! - **Read N bytes from register R**: `[Wr addr R][Sr addr Rd <N bytes>]`
//!   (i.e. write the register address byte, repeated START, read N bytes).
//! - **Write byte/word to register R**: `[Wr addr R <payload>]` (one
//!   transaction; payload is 1, 2, or 4 bytes depending on register).
//!
//! Word-sized register reads (VOUT, IOUT, POUT, VIN, IIN, PIN) are
//! **little-endian** by RE2's catalog convention. Endianness is documented
//! per-register; if a future RE pass disproves LE on any register, change
//! the helper for that register only.
//!
//! # Construction is sealed-trait gated
//!
//! [`Apw12PlusBackend::new`] requires a marker type implementing
//! [`Apw12PlusAuthorized`]. The trait is sealed and only the markers in
//! [`platforms`] satisfy it. This is defense-in-depth on top of the
//! runtime gate (`dcentrald_hal::platform::subtype::classify_with_probe`
//! → `VoltageControllerKind::NoPic` for S21 family).

use std::time::Duration;

use crate::i2c::{I2cServiceHandle, I2cTransactionStep};
use crate::HalError;
use crate::Result;

// ===========================================================================
//  Constants — RE2 §5.3 register catalog
// ===========================================================================

/// I2C slave address for APW12+ (RE2 §5.3 — `0x10`). Same nominal address as
/// APW12 SMBus and as `Apw121215a`. Address alone DOES NOT identify the
/// protocol; platform classification + sealed-trait gate is mandatory.
pub const APW12_PLUS_I2C_ADDR: u8 = 0x10;

/// Sysfs GPIO line for PSU enable on **S21 family** (RE2 §5.1 line 475 —
/// `GPIO 907`).
///
/// **Platform-specific.** S21 Amlogic boards route the PSU enable through
/// GPIO 907; that line is asserted by [`crate::psu_gpio_gate::PsuGpioGate`]
/// before APW12+ register I/O is attempted. This driver itself does not
/// touch GPIO — caller is responsible for GPIO sysfs export / direction /
/// value writes.
pub const GPIO_PSU_ENABLE: u32 = 907;

/// Settle delay between writing CONTROL=0x01 (power on) and the first
/// telemetry read. Conservative — RE2 does not specify, but APW12 SMBus
/// uses 250 ms and the same rail family is at play here.
pub const POWER_ON_SETTLE_MS: u64 = 250;

/// Maximum power-limit ceiling for [`Apw12PlusBackend::set_power_limit_w`].
/// RE2 §5.1 calls APW12+ a "4000W+ class" PSU; we cap at 4500 W as a
/// conservative envelope. Adjust upward only with hardware confirmation.
pub const POWER_LIMIT_MAX_W: u32 = 4500;

/// CONTROL register write payload to power the rail OFF.
///
/// **GUESS — verify on live S21.** RE2 §5.3 documents register 0x20 as
/// `CONTROL` with semantic `ON/OFF` but does not pin the specific byte
/// values. The 0x00/0x01 convention is inferred from BMC patterns and from
/// APW12 SMBus's PowerOff=0x00 / PowerOn=0x01 opcode pair. If a live S21
/// trace shows different values, fix here and in [`CONTROL_ON`].
///
/// **RE 2026-06-02 (Ghidra of the unstripped Bitmain S21 jig `single_board_test`) —
/// this register-0x20 model is the WRONG abstraction for the actual S21 APW121215f.**
/// The jig proves: (1) rail ENABLE is a **GPIO** (`bitmain_power_on` = `gpio_write(907,0)`
/// active-LOW on the jig; S21-Amlogic = PWR_EN gpio437 active-HIGH, already driven by
/// `PsuGpioGate`) — there is NO register-0x20 control word; (2) APW121215f control is the
/// `0x55 0xAA` **frame protocol** via I2C reg `0x11` (watchdog `0x81`, set-voltage frame/DAC-N,
/// 16-bit-sum checksum) — exactly `psu.rs::Apw121215a`. So the S21/AML PSU should route through
/// `Apw121215a` frame protocol + gpio437, NOT this register model. Full RE:
/// . These constants
/// are retained only for a genuinely register-mapped APW12+ variant (if one exists); do NOT
/// trust them for APW121215f. Live S21 routing change is the next (PSU-gated) step.
///
/// **CORROBORATED 2026-06-02 by an INDEPENDENT firmware (Ghidra of VNish v1.2.6
/// `awesome-s21-aml` `usr/bin/cgminer`, extracted from the firmware archive):** the VNish S21
/// PSU path uses the SAME `0x55 0xAA LEN CMD …` frame family over I2C (set-slave ioctl
/// `0x0703` → write reg-pointer → read/write N bytes), with `CMD 0x06` = read
/// calibration/telemetry (39–40-byte table). There is **NO register-0x20 control word** in the
/// VNish cgminer either — rail enable is GPIO (S11board drives PWR_EN gpio437 HIGH, no
/// `active_low`), exactly as the jig showed. New detail: the VNish cgminer verifies the
/// PSU **calibration table with CRC16/0xFFFF over 30 bytes** (`crc16(table[..30], init=0xFFFF)`),
/// distinct from the host→PSU command-frame SUM checksum the jig pinned. Two independent
/// firmwares (Bitmain jig + VNish cgminer) now agree on the GPIO-enable + `55 AA` frame model,
/// so the register-0x20 abstraction is confirmed-vestigial for APW121215f. See
/// .
///
/// **THIRD independent source, 2026-07-07 (W2-C — read-only RE of the APW12
/// PIC16F1704 firmware itself, ):**
/// the PSU's own PIC firmware (fw=0x71 `V71`, code-identical to `APW121215a-Good.dis`)
/// dispatches ONLY `55 AA` frames and builds ONLY `55 AA` replies — there is **no
/// register-0x20 / `CONTROL` byte anywhere in the command handler.** So the Bitmain
/// jig, VNish cgminer, AND the PSU firmware now agree: these register-0x20 constants
/// are vestigial for the real APW121215a/f. They are retained (still `GUESS`, do NOT
/// assign real values) only for a hypothetical genuinely-register-mapped APW12+ variant.
pub const CONTROL_OFF: u8 = 0x00;

/// CONTROL register write payload to power the rail ON.
///
/// **GUESS — verify on live S21.** See [`CONTROL_OFF`] for the rationale.
pub const CONTROL_ON: u8 = 0x01;

/// CLEAR_FAULTS register write payload (any non-zero byte clears).
///
/// **GUESS — verify on live S21.** RE2 §5.3 documents register 0x21 as
/// `CLEAR_FAULTS` with semantic `Clear` but does not pin the byte value.
/// 0x01 is conventional; some PSU firmwares accept 0xFF, some accept any
/// non-zero. Adjust after live capture.
pub const CLEAR_FAULTS_PAYLOAD: u8 = 0x01;

// ===========================================================================
//  Register-address enum — mirrors RE2 §5.3 verbatim
// ===========================================================================

/// APW12+ register addresses (RE2 §5.3 lines 505-527).
///
/// All values are bare 8-bit register pointers; on the wire these become
/// the first byte of a write, or the address-byte of a write-then-read
/// transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Apw12PlusReg {
    /// Manufacturer ID — ASCII, read-only. RE2 length not pinned; we read
    /// up to 16 bytes. (`0x00`)
    MfrId = 0x00,
    /// Model string — ASCII, read-only. (`0x01`)
    Model = 0x01,
    /// Revision — BCD u16 LE, read-only. (`0x02`)
    Revision = 0x02,
    /// Status bitfield — u32 LE, read-only. (`0x03`)
    Status = 0x03,
    /// Output voltage — u16 LE, units of 0.01 V. (`0x04`)
    Vout = 0x04,
    /// Output current — u16 LE, units of 0.1 A. (`0x05`)
    Iout = 0x05,
    /// Output power — u16 LE, watts. (`0x06`)
    Pout = 0x06,
    /// Temp sensor 1 — i8, °C. (`0x0A`)
    Temp1 = 0x0A,
    /// Temp sensor 2 — i8, °C. (`0x0B`)
    Temp2 = 0x0B,
    /// Temp sensor 3 — i8, °C. (`0x0C`)
    Temp3 = 0x0C,
    /// Fan 1 RPM low byte — u16 LE pair with [`Self::Fan1RpmHi`]. (`0x0D`)
    Fan1RpmLo = 0x0D,
    /// Fan 1 RPM high byte. (`0x0E`)
    Fan1RpmHi = 0x0E,
    /// Fan 2 RPM low byte — u16 LE pair with [`Self::Fan2RpmHi`]. (`0x0F`)
    Fan2RpmLo = 0x0F,
    /// Fan 2 RPM high byte. (`0x10`)
    Fan2RpmHi = 0x10,
    /// Input voltage — u16 LE, units of 0.01 V. (`0x11`)
    Vin = 0x11,
    /// Input current — u16 LE, units of 0.1 A. (`0x12`)
    Iin = 0x12,
    /// Input power — u16 LE, watts. (`0x13`)
    Pin = 0x13,
    /// Efficiency — u8, percent. (`0x14`)
    Efficiency = 0x14,
    /// Alarm bitfield — u32 LE, read-only. (`0x18`)
    Alarm = 0x18,
    /// Control — write-only, 1 byte (ON / OFF — see [`CONTROL_ON`] /
    /// [`CONTROL_OFF`]). (`0x20`)
    Control = 0x20,
    /// Clear faults — write-only, 1 byte (see [`CLEAR_FAULTS_PAYLOAD`]).
    /// (`0x21`)
    ClearFaults = 0x21,
    /// Set output power limit — write-only, u32 LE watts. (`0x22`)
    SetPowerLimit = 0x22,
}

impl Apw12PlusReg {
    /// Get the raw register-address byte.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

// ===========================================================================
//  Sealed trait — platform whitelist for `Apw12PlusBackend::new`
// ===========================================================================

mod sealed {
    pub trait Sealed {}
}

/// Sealed-trait whitelist for [`Apw12PlusBackend::new`].
///
/// Implementors are limited to the marker types in [`platforms`]. The
/// trait is sealed via `sealed::Sealed`, so adding a new platform requires
/// editing this module — there is no out-of-crate escape hatch.
///
/// # Why not share markers with `psu_apw12_smbus`?
///
/// Different families. APW12 SMBus markers are CV1835 / AM335x BB /
/// Amlogic S19j Pro (PIC1704 voltage controller). APW12+ markers are
/// S21 / S21 Pro / S21 XP (NoPic, TAS5782M voltage controller). The two
/// sets MUST NOT overlap — that's how we keep "different protocol at the
/// same address" mistakes off the live fleet.
pub trait Apw12PlusAuthorized: sealed::Sealed {}

/// Marker types for platforms that own APW12+-protocol PSUs.
///
/// All three markers are S21-family (NoPic) variants. `S21AmlogicNoPic` is
/// the actively shipped target (live fleet unit `s21` at .135);
/// `S21ProAmlogic` and `S21XpAmlogic` are forward-looking placeholders for
/// when those SKUs land on the fleet — register protocol is identical per
/// RE2 §5.3, so the markers exist to widen the whitelist with one
/// import-site change.
pub mod platforms {
    /// S21 base SKU on Amlogic A113D, NoPic (TAS5782M DACs).
    ///: never GPIO-reset S21
    /// chips — and this driver never writes GPIO at all.
    pub struct S21AmlogicNoPic;
    /// S21 Pro SKU on Amlogic A113D, NoPic. Forward-looking marker.
    pub struct S21ProAmlogic;
    /// S21 XP SKU on Amlogic A113D, NoPic. Forward-looking marker.
    pub struct S21XpAmlogic;
}

impl sealed::Sealed for platforms::S21AmlogicNoPic {}
impl Apw12PlusAuthorized for platforms::S21AmlogicNoPic {}

impl sealed::Sealed for platforms::S21ProAmlogic {}
impl Apw12PlusAuthorized for platforms::S21ProAmlogic {}

impl sealed::Sealed for platforms::S21XpAmlogic {}
impl Apw12PlusAuthorized for platforms::S21XpAmlogic {}

// ===========================================================================
//  Pure step-builders (host-testable, no I/O)
// ===========================================================================

/// Build a write-byte transaction `[Wr addr reg payload...]`.
fn write_steps(reg: u8, payload: &[u8]) -> Vec<I2cTransactionStep> {
    let mut buf = Vec::with_capacity(1 + payload.len());
    buf.push(reg);
    buf.extend_from_slice(payload);
    vec![I2cTransactionStep::Write(buf)]
}

/// Build a write-then-read transaction `[Wr addr reg][Sr addr Rd <N>]`.
fn read_steps(reg: u8, read_len: usize) -> Vec<I2cTransactionStep> {
    vec![I2cTransactionStep::WriteRead {
        write_data: vec![reg],
        read_len,
    }]
}

/// CONTROL=ON encoding (`[0x20, 0x01]`).
pub fn power_on_steps() -> Vec<I2cTransactionStep> {
    write_steps(Apw12PlusReg::Control.as_u8(), &[CONTROL_ON])
}

/// CONTROL=OFF encoding (`[0x20, 0x00]`).
pub fn power_off_steps() -> Vec<I2cTransactionStep> {
    write_steps(Apw12PlusReg::Control.as_u8(), &[CONTROL_OFF])
}

/// CLEAR_FAULTS encoding (`[0x21, 0x01]` by current convention).
pub fn clear_faults_steps() -> Vec<I2cTransactionStep> {
    write_steps(Apw12PlusReg::ClearFaults.as_u8(), &[CLEAR_FAULTS_PAYLOAD])
}

/// SET_POWER_LIMIT encoding `[0x22, w_le[0..4]]`. No bounds check here —
/// the public method [`Apw12PlusBackend::set_power_limit_w`] enforces
/// [`POWER_LIMIT_MAX_W`].
pub fn set_power_limit_steps(watts: u32) -> Vec<I2cTransactionStep> {
    let bytes = watts.to_le_bytes();
    write_steps(Apw12PlusReg::SetPowerLimit.as_u8(), &bytes)
}

// ===========================================================================
// APW121215f frame protocol (RE 2026-06-02, byte-exact from the unstripped S21
// jig `single_board_test`
// S21-APW-PMBUS-GHIDRA-RE.md). DISTINCT from the am2 APW121215a (`psu.rs`):
// APW121215f uses a **16-bit** checksum and `LEN = payload+4` (LEN counts itself +
// CMD + payload + the 2 checksum bytes), whereas am2 APW121215a uses an **8-bit**
// checksum with `LEN = payload+2`. Reusing the am2 8-bit frame on an APW121215f
// would fail the PSU's checksum. Frames are written byte-by-byte to I2C register
// 0x11 (`exec_power_cmd_v2`). Rail ENABLE is a GPIO (gpio437 active-HIGH on
// S21-AML), NOT a frame command. These builders are RE-exact but NOT yet wired
// into a live S21 cold-boot path — that drives a live PSU, so an operator live-A/B
// on `a lab unit` is owed before any default-on routing.
// ===========================================================================

/// APW121215f PMBus register-pointer: frames are written byte-by-byte to this I2C
/// register (RE: `exec_power_cmd_v2`; legacy `exec_power_cmd` uses 0x00 + a one-time 0x04).
pub const APW121215F_REG_POINTER: u8 = 0x11;

/// APW121215f watchdog opcode (frame CMD). RE: `_bitmain_set_watchdog`.
pub const APW121215F_CMD_WATCHDOG: u8 = 0x81;

/// Build a byte-exact APW121215f command frame
/// `[0x55, 0xAA, LEN, CMD, payload..., CKSUM_lo, CKSUM_hi]` where
/// `LEN = payload.len() + 4` and `CKSUM = LEN + CMD + sum(payload)` as a 16-bit
/// little-endian word. RE 2026-06-02 from the S21 jig.
pub fn build_apw121215f_frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() as u8).wrapping_add(4);
    let mut sum: u16 = u16::from(len).wrapping_add(u16::from(cmd));
    for &b in payload {
        sum = sum.wrapping_add(u16::from(b));
    }
    let mut frame = Vec::with_capacity(payload.len() + 6);
    frame.extend_from_slice(&[0x55, 0xAA, len, cmd]);
    frame.extend_from_slice(payload);
    frame.push((sum & 0xFF) as u8);
    frame.push((sum >> 8) as u8);
    frame
}

/// APW121215f watchdog frame for control byte `ctrl` (0 = disable/arm-off).
/// Disable (`ctrl=0`) = `[55 AA 06 81 00 00 87 00]` — RE-exact (S21 jig).
pub fn apw121215f_watchdog_frame(ctrl: u8) -> Vec<u8> {
    build_apw121215f_frame(APW121215F_CMD_WATCHDOG, &[ctrl, 0x00])
}

// ---------------------------------------------------------------------------
// APW float-frame SetVoltage (RE 2026-06-02, byte-exact from the HashSource
// S21 Pro jig `single_board_test.dec/bitmain_set_voltage@C90C8`). A SECOND,
// distinct SetVoltage encoding used by the higher-voltage APW power_versions
// 0x62/0x64/0x65/0x66/0x6A (17.0-21.6 V for the 0x62 class, 12-16 V for 0x6A),
// vs the DAC-byte form (`bitmain_convert_V_to_N` -> `build_apw121215f_frame(0x83,
// [N,0])`) used by 0x71/0x76 and friends. dcentrald's fleet PSUs are 0x71
// (am2 12.8 V) and 0x76 (S21) — both DAC-byte — so the float-frame is NOT a
// current-fleet path; it is shipped data-only to COMPLETE the APW SetVoltage
// protocol coverage for any future control board carrying a 0x62-class PSU.
// ---------------------------------------------------------------------------

/// APW power_versions that use the **float-frame** SetVoltage encoding (the jig
/// `bitmain_set_voltage` branch that builds an IEEE-754 cmd instead of calling
/// `bitmain_convert_V_to_N`). 0x62/0x64/0x65/0x66/0x6A.
pub fn apw_is_float_frame_version(power_version: u8) -> bool {
    matches!(power_version, 0x62 | 0x64 | 0x65 | 0x66 | 0x6A)
}

/// Build the byte-exact APW **float-frame** SetVoltage command (10 bytes):
/// `[0x55, 0xAA, 0x08, 0x83, <V as IEEE-754 f32 little-endian: 4 bytes>, CK_lo, CK_hi]`.
///
/// The checksum is a **16-bit word-sum** over the bytes *after* the `55 AA`
/// preamble taken as little-endian 16-bit words — i.e.
/// `CK = word(LEN,CMD) + word(f0,f1) + word(f2,f3)` where `word(lo,hi)=lo|hi<<8`
/// — which is DISTINCT from the DAC-byte frames' per-byte sum
/// ([`build_apw121215f_frame`]). RE-exact from the jig's inline loop
/// (`v10 += byte[2] + (byte[3] << 8)` over the 8 header+float bytes).
///
/// **DATA-ONLY / NO live-sending method** (mirrors the rest of the APW121215f
/// builders): not a current-fleet PSU; transport would still be
/// [`apw121215f_reg11_transaction`] + the GPIO rail enable, behind the same
/// operator live-A/B gate as the DAC-byte path.
pub fn build_apw_float_frame(voltage_v: f32) -> Vec<u8> {
    const LEN: u8 = 0x08;
    const CMD: u8 = 0x83;
    let f = voltage_v.to_le_bytes(); // [f0, f1, f2, f3] IEEE-754 LE
    let word = |lo: u8, hi: u8| (lo as u16) | ((hi as u16) << 8);
    let ck = word(LEN, CMD)
        .wrapping_add(word(f[0], f[1]))
        .wrapping_add(word(f[2], f[3]));
    vec![
        0x55,
        0xAA,
        LEN,
        CMD,
        f[0],
        f[1],
        f[2],
        f[3],
        (ck & 0xFF) as u8,
        (ck >> 8) as u8,
    ]
}

/// APW121215f transport: a frame is written to the PSU **one byte at a time**, each as a separate
/// `[0x11, frame_byte]` I2C write to the register-pointer (RE: `exec_power_cmd_v2`'s per-byte
/// `iic_write_reg(fd, &reg=0x11, 1, &frame[i], 1)` loop; reply is read back byte-by-byte from the same
/// register with a ~500 ms settle). This builds the ordered write-step list for one frame.
///
/// NOTE: this is the byte-exact TRANSPORT for the S21 APW121215f frame protocol, host-testable and
/// pure. It is NOT yet wired into the live S21/AML cold-boot dispatch — executing it asserts a real PSU
/// rail, so the cold-boot integration (assert gpio437 → version-detect → watchdog-disable → set-voltage
/// → enable) ships default-OFF and needs an operator live-A/B on `a lab unit` (plus the confirmed AML i2c
/// bus/slave-addr) before any default-on routing.
pub fn apw121215f_reg11_transaction(frame: &[u8]) -> Vec<I2cTransactionStep> {
    frame
        .iter()
        .map(|&b| I2cTransactionStep::Write(vec![APW121215F_REG_POINTER, b]))
        .collect()
}

// ===========================================================================
//  Decoders
// ===========================================================================

/// Decode a little-endian u16 from the first 2 bytes of `buf`.
pub fn decode_le_u16(buf: &[u8]) -> Option<u16> {
    if buf.len() < 2 {
        return None;
    }
    Some(u16::from(buf[0]) | (u16::from(buf[1]) << 8))
}

/// Decode a little-endian u32 from the first 4 bytes of `buf`.
pub fn decode_le_u32(buf: &[u8]) -> Option<u32> {
    if buf.len() < 4 {
        return None;
    }
    Some(
        u32::from(buf[0])
            | (u32::from(buf[1]) << 8)
            | (u32::from(buf[2]) << 16)
            | (u32::from(buf[3]) << 24),
    )
}

/// Decode a centivolt (×0.01 V) word into a float volts. `0x05DC` → 15.00 V.
pub fn decode_centivolt(buf: &[u8]) -> Option<f32> {
    decode_le_u16(buf).map(|raw| (raw as f32) * 0.01)
}

/// Decode a deciamp (×0.1 A) word into a float amps. `0x012C` → 30.0 A.
pub fn decode_deciamp(buf: &[u8]) -> Option<f32> {
    decode_le_u16(buf).map(|raw| (raw as f32) * 0.1)
}

// ===========================================================================
//  Apw12PlusBackend — runtime controller
// ===========================================================================

/// Service-thread-backed APW12+ register-protocol PSU controller for the
/// S21 family.
///
/// All I/O routes through a shared [`I2cServiceHandle`] (single-owner I2C
/// architecture). Construction is gated by [`Apw12PlusAuthorized`] at
/// compile time — only the markers in [`platforms`] satisfy that bound.
///
/// `M` is a phantom marker type carrying the platform proof; it is
/// optimised away.
pub struct Apw12PlusBackend<M: Apw12PlusAuthorized> {
    i2c: I2cServiceHandle,
    address: u8,
    /// Cached `output_on` flag, mirroring [`Self::power_on`] /
    /// [`Self::power_off`] cache semantics from `Apw12SmbusBackend`.
    output_on: bool,
    _marker: std::marker::PhantomData<M>,
}

impl<M: Apw12PlusAuthorized> Apw12PlusBackend<M> {
    /// Construct an APW12+ controller for one of the whitelisted S21
    /// markers. Address defaults to [`APW12_PLUS_I2C_ADDR`] (0x10); use
    /// [`Self::new_at`] to pin a different slave.
    pub fn new(handle: I2cServiceHandle, _marker: M) -> Self {
        Self::new_at(handle, APW12_PLUS_I2C_ADDR, _marker)
    }

    /// Construct at a specific slave address (rare — most boards use 0x10).
    pub fn new_at(handle: I2cServiceHandle, address: u8, _marker: M) -> Self {
        Self {
            i2c: handle,
            address,
            output_on: false,
            _marker: std::marker::PhantomData,
        }
    }

    /// Cached I2C slave address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Whether the PSU is currently powered ON, by our cached state.
    pub fn is_output_on(&self) -> bool {
        self.output_on
    }

    // -----------------------------------------------------------------------
    //  Reads — identity / status
    // -----------------------------------------------------------------------

    /// Read MFR_ID — ASCII bytes from register 0x00. RE2 does not pin the
    /// length; we read 16 bytes and let the caller trim.
    pub fn read_mfr_id(&mut self) -> Result<Vec<u8>> {
        self.run_read(read_steps(Apw12PlusReg::MfrId.as_u8(), 16))
    }

    /// Read MODEL — ASCII bytes from register 0x01.
    pub fn read_model(&mut self) -> Result<Vec<u8>> {
        self.run_read(read_steps(Apw12PlusReg::Model.as_u8(), 16))
    }

    /// Read REVISION — 2 bytes BCD from register 0x02. Returned as raw u16
    /// LE; caller decides BCD-vs-binary interpretation.
    pub fn read_revision(&mut self) -> Result<u16> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Revision.as_u8(), 2))?;
        decode_le_u16(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("REVISION returned {} bytes (need 2)", buf.len()))
        })
    }

    /// Read STATUS — u32 LE bitfield from register 0x03.
    pub fn read_status(&mut self) -> Result<u32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Status.as_u8(), 4))?;
        decode_le_u32(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("STATUS returned {} bytes (need 4)", buf.len()))
        })
    }

    /// Read ALARM — u32 LE bitfield from register 0x18.
    pub fn read_alarm(&mut self) -> Result<u32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Alarm.as_u8(), 4))?;
        decode_le_u32(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("ALARM returned {} bytes (need 4)", buf.len()))
        })
    }

    // -----------------------------------------------------------------------
    //  Reads — output telemetry (rail side)
    // -----------------------------------------------------------------------

    /// Read VOUT — output voltage in volts (×0.01 V scaling). Reg 0x04.
    pub fn read_vout_v(&mut self) -> Result<f32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Vout.as_u8(), 2))?;
        decode_centivolt(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("VOUT returned {} bytes (need 2)", buf.len()))
        })
    }

    /// Read IOUT — output current in amps (×0.1 A scaling). Reg 0x05.
    pub fn read_iout_a(&mut self) -> Result<f32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Iout.as_u8(), 2))?;
        decode_deciamp(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("IOUT returned {} bytes (need 2)", buf.len()))
        })
    }

    /// Read POUT — output power in watts (1 W scaling, u16 LE). Reg 0x06.
    pub fn read_pout_w(&mut self) -> Result<u32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Pout.as_u8(), 2))?;
        decode_le_u16(&buf).map(u32::from).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("POUT returned {} bytes (need 2)", buf.len()))
        })
    }

    // -----------------------------------------------------------------------
    //  Reads — input telemetry (AC side)
    // -----------------------------------------------------------------------

    /// Read VIN — input voltage in volts (×0.01 V scaling). Reg 0x11.
    pub fn read_vin_v(&mut self) -> Result<f32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Vin.as_u8(), 2))?;
        decode_centivolt(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("VIN returned {} bytes (need 2)", buf.len()))
        })
    }

    /// Read IIN — input current in amps (×0.1 A scaling). Reg 0x12.
    pub fn read_iin_a(&mut self) -> Result<f32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Iin.as_u8(), 2))?;
        decode_deciamp(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("IIN returned {} bytes (need 2)", buf.len()))
        })
    }

    /// Read PIN — input power in watts (1 W scaling, u16 LE). Reg 0x13.
    pub fn read_pin_w(&mut self) -> Result<u32> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Pin.as_u8(), 2))?;
        decode_le_u16(&buf).map(u32::from).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!("PIN returned {} bytes (need 2)", buf.len()))
        })
    }

    /// Read EFFICIENCY — u8 percent. Reg 0x14.
    pub fn read_efficiency_pct(&mut self) -> Result<u8> {
        let buf = self.run_read(read_steps(Apw12PlusReg::Efficiency.as_u8(), 1))?;
        if buf.is_empty() {
            return Err(HalError::PsuProtocolOwned(
                "EFFICIENCY returned empty buffer".into(),
            ));
        }
        Ok(buf[0])
    }

    // -----------------------------------------------------------------------
    //  Reads — temperature + fans
    // -----------------------------------------------------------------------

    /// Read all three temperature sensors (TEMP1..TEMP3, regs 0x0A-0x0C).
    /// Each is one signed byte °C. Returns `[T1, T2, T3]`.
    pub fn read_temps(&mut self) -> Result<[i8; 3]> {
        let t1 = self.read_one_temp(Apw12PlusReg::Temp1)?;
        let t2 = self.read_one_temp(Apw12PlusReg::Temp2)?;
        let t3 = self.read_one_temp(Apw12PlusReg::Temp3)?;
        Ok([t1, t2, t3])
    }

    fn read_one_temp(&mut self, reg: Apw12PlusReg) -> Result<i8> {
        let buf = self.run_read(read_steps(reg.as_u8(), 1))?;
        if buf.is_empty() {
            return Err(HalError::PsuProtocolOwned(format!(
                "TEMP@0x{:02X} returned empty buffer",
                reg.as_u8()
            )));
        }
        Ok(buf[0] as i8)
    }

    /// Read fan RPM by index (0 = FAN1, 1 = FAN2). Each fan reads as two
    /// consecutive registers (low, high) interpreted as u16 LE.
    ///
    /// Per RE2 §5.3, fan RPM occupies registers 0x0D-0x10 across two fans.
    /// We pair them as FAN1=[0x0D|0x0E], FAN2=[0x0F|0x10]. This pairing is
    /// **inferred** from the table layout — verify on live S21.
    pub fn read_fan_rpm(&mut self, idx: u8) -> Result<u16> {
        let lo_reg = match idx {
            0 => Apw12PlusReg::Fan1RpmLo,
            1 => Apw12PlusReg::Fan2RpmLo,
            _ => {
                return Err(HalError::PsuProtocolOwned(format!(
                    "fan idx {} out of range [0, 1]",
                    idx
                )));
            }
        };
        // Single 2-byte read starting at the low byte's register address.
        // Register auto-increment is APW12+'s expected behavior per RE2's
        // word-sized scaling for adjacent registers.
        let buf = self.run_read(read_steps(lo_reg.as_u8(), 2))?;
        decode_le_u16(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "FAN{}_RPM returned {} bytes (need 2)",
                idx + 1,
                buf.len()
            ))
        })
    }

    // -----------------------------------------------------------------------
    //  Writes — control / faults / power limit
    // -----------------------------------------------------------------------

    /// Power on the rail (write CONTROL=0x01 to register 0x20). Caches
    /// `output_on=true` and sleeps [`POWER_ON_SETTLE_MS`] before returning.
    pub fn power_on(&mut self) -> Result<()> {
        self.run(power_on_steps())?;
        std::thread::sleep(Duration::from_millis(POWER_ON_SETTLE_MS));
        self.output_on = true;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            settle_ms = POWER_ON_SETTLE_MS,
            "APW12+ POWER_ON"
        );
        Ok(())
    }

    /// Power off the rail (write CONTROL=0x00 to register 0x20). Caches
    /// `output_on=false`.
    pub fn power_off(&mut self) -> Result<()> {
        self.run(power_off_steps())?;
        self.output_on = false;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            "APW12+ POWER_OFF"
        );
        Ok(())
    }

    /// Clear faults (write CLEAR_FAULTS_PAYLOAD to register 0x21).
    pub fn clear_faults(&mut self) -> Result<()> {
        self.run(clear_faults_steps())?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            "APW12+ CLEAR_FAULTS"
        );
        Ok(())
    }

    /// Set output power limit in watts (write u32 LE to register 0x22).
    /// Bounds-checked against [`POWER_LIMIT_MAX_W`].
    pub fn set_power_limit_w(&mut self, watts: u32) -> Result<()> {
        if watts == 0 || watts > POWER_LIMIT_MAX_W {
            return Err(HalError::PsuProtocolOwned(format!(
                "set_power_limit_w: {} outside (0, {}]",
                watts, POWER_LIMIT_MAX_W
            )));
        }
        self.run(set_power_limit_steps(watts))?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            watts,
            "APW12+ SET_POWER_LIMIT"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    //  Internal: dispatch through I2C service
    // -----------------------------------------------------------------------

    fn run(&self, steps: Vec<I2cTransactionStep>) -> Result<()> {
        let _ = self.i2c.transaction(self.address, steps)?;
        Ok(())
    }

    fn run_read(&self, steps: Vec<I2cTransactionStep>) -> Result<Vec<u8>> {
        let mut reads = self.i2c.transaction(self.address, steps)?;
        reads
            .pop()
            .ok_or_else(|| HalError::PsuProtocolOwned("transaction returned no read result".into()))
    }
}

// ===========================================================================
//  Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::platforms::*;
    use super::*;
    use crate::i2c::I2cRequest;
    use std::sync::mpsc::Receiver;
    use std::thread;

    /// Compile-time proof that all platform marker types implement
    /// [`Apw12PlusAuthorized`]. If this fails to compile, the sealed-trait
    /// whitelist has lost a platform.
    fn _compile_assert_authorized<P: Apw12PlusAuthorized>() {}

    #[test]
    fn apw_float_frame_matches_jig_bitmain_set_voltage() {
        // RE byte-exact vs HashSource S21 Pro jig bitmain_set_voltage@C90C8
        // (the float-frame branch for power_version 0x62/0x64/0x65/0x66/0x6A):
        // [55 AA 08 83 <f32 LE> <word-sum16>], distinct CHECKSUM from the
        // DAC-byte frames (16-bit word-sum, not per-byte sum).
        assert!(apw_is_float_frame_version(0x62));
        assert!(apw_is_float_frame_version(0x65));
        assert!(apw_is_float_frame_version(0x6A));
        assert!(!apw_is_float_frame_version(0x71)); // DAC-byte (am2)
        assert!(!apw_is_float_frame_version(0x76)); // DAC-byte (S21)

        // Header + opcode + length pinned.
        let f = build_apw_float_frame(19.0);
        assert_eq!(f.len(), 10);
        assert_eq!(&f[0..4], &[0x55, 0xAA, 0x08, 0x83]);

        // 19.0 f32 = 0x4198_0000 -> LE bytes [00 00 98 41].
        assert_eq!(&f[4..8], &19.0f32.to_le_bytes());
        assert_eq!(&f[4..8], &[0x00, 0x00, 0x98, 0x41]);

        // word-sum CK = word(0x08,0x83) + word(00,00) + word(0x98,0x41)
        //             = 0x8308 + 0x0000 + 0x4198 = 0xC4A0.
        let ck = 0x8308u16.wrapping_add(0x0000).wrapping_add(0x4198);
        assert_eq!(ck, 0xC4A0);
        assert_eq!(f[8], (ck & 0xFF) as u8); // 0xA0
        assert_eq!(f[9], (ck >> 8) as u8); // 0xC4

        // The float-frame checksum is genuinely DIFFERENT from the per-byte sum
        // the DAC-byte builder would compute over the same LEN+CMD+payload —
        // pin that they diverge so the two encodings can't be conflated.
        let bytesum = build_apw121215f_frame(0x83, &19.0f32.to_le_bytes());
        // bytesum frame ends with the per-byte sum; the float-frame's last two
        // bytes (word-sum) must NOT equal the byte-sum frame's last two bytes.
        assert_ne!(
            &f[8..10],
            &bytesum[bytesum.len() - 2..],
            "float-frame word-sum must differ from DAC-frame byte-sum"
        );
    }

    #[test]
    fn apw121215f_frame_matches_s21_jig_re() {
        // RE 2026-06-02 byte-exact from the unstripped Bitmain S21 jig (single_board_test):
        // `_bitmain_set_watchdog` builds [55 AA 06 81 ctrl 00 CK16] (CK16 = LEN+CMD+payload,
        // 16-bit LE). Watchdog DISABLE (ctrl=0) = [55 AA 06 81 00 00 87 00] (CK16 = 0x06+0x81).
        assert_eq!(
            apw121215f_watchdog_frame(0x00),
            vec![0x55, 0xAA, 0x06, 0x81, 0x00, 0x00, 0x87, 0x00]
        );
        // ctrl=1 → CK16 = 0x87 + 0x01 = 0x88.
        assert_eq!(
            apw121215f_watchdog_frame(0x01),
            vec![0x55, 0xAA, 0x06, 0x81, 0x01, 0x00, 0x88, 0x00]
        );
        // LEN = payload + 4 (counts LEN + CMD + payload + 2 checksum bytes).
        let f = build_apw121215f_frame(0x83, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(f[0], 0x55);
        assert_eq!(f[1], 0xAA);
        assert_eq!(f[2], 0x0A, "LEN = payload(6) + 4");
        assert_eq!(f[3], 0x83);
        assert_eq!(f.len(), 6 + 6); // preamble(2)+len(1)+cmd(1)+payload(6)+cksum(2)
                                    // 16-bit checksum carries past 0xFF: 0x0A + 0x83 + 6*0xFF = 1671 = 0x0687.
        let ck = u16::from_le_bytes([f[f.len() - 2], f[f.len() - 1]]);
        assert_eq!(ck, 0x0A + 0x83 + 6 * 0xFF);
        assert_eq!(ck, 0x0687);
        // DISTINCT from the am2 APW121215a 8-bit frame: APW121215f watchdog is 8 bytes
        // (16-bit cksum, LEN=payload+4), vs am2's 6-byte [55 AA 03 81 00 84] (8-bit, LEN=payload+2).
        assert_eq!(apw121215f_watchdog_frame(0x00).len(), 8);
        assert_eq!(APW121215F_REG_POINTER, 0x11);
        assert_eq!(APW121215F_CMD_WATCHDOG, 0x81);

        // Transport: the frame is written one byte at a time, each as a [0x11, byte] I2C write
        // to the register-pointer (RE: exec_power_cmd_v2). Watchdog-disable → 8 such writes.
        let txn = apw121215f_reg11_transaction(&apw121215f_watchdog_frame(0x00));
        assert_eq!(txn.len(), 8);
        for (step, &fb) in txn
            .iter()
            .zip([0x55u8, 0xAA, 0x06, 0x81, 0x00, 0x00, 0x87, 0x00].iter())
        {
            match step {
                I2cTransactionStep::Write(buf) => assert_eq!(buf, &vec![0x11, fb]),
                other => panic!("expected Write([0x11, byte]), got {:?}", other),
            }
        }

        // SET-VOLTAGE (CMD 0x83, RE-ASK-BB-2): cross-validated against the STOCK Bitmain S19j-Pro-BB
        // bmminer (`_bitmain_set_DA_conversion_N`, RE 2026-06-02): the payload is a single DAC byte N
        // (N = convert_V_to_N(V), a per-version linear `offset - V*slope`), and the frame is
        // [55 AA 06 83 N 00 CK16] with CK16 = LEN+CMD+N = 0x89 + N. This is EXACTLY this builder, so
        // build_apw121215f_frame ALSO byte-exactly produces the BB APW121215f set-voltage frame.
        let set_v = build_apw121215f_frame(0x83, &[0x6C, 0x00]); // N=0x6C (e.g. ~13.7 V DAC)
        assert_eq!(set_v, vec![0x55, 0xAA, 0x06, 0x83, 0x6C, 0x00, 0xF5, 0x00]); // CK16 = 0x89+0x6C = 0xF5
                                                                                 // CK16 carry when N is large (N=0xFF): 0x89 + 0xFF = 0x0188 → lo 0x88, hi 0x01.
        let set_v_hi = build_apw121215f_frame(0x83, &[0xFF, 0x00]);
        assert_eq!(
            set_v_hi,
            vec![0x55, 0xAA, 0x06, 0x83, 0xFF, 0x00, 0x88, 0x01]
        );
    }

    #[test]
    fn markers_are_exhaustive() {
        // Each S21 marker must satisfy the sealed trait. No other types
        // can — `Apw12PlusAuthorized: sealed::Sealed` and `Sealed` is
        // crate-private with `impl`s confined to this module.
        _compile_assert_authorized::<S21AmlogicNoPic>();
        _compile_assert_authorized::<S21ProAmlogic>();
        _compile_assert_authorized::<S21XpAmlogic>();
    }

    #[test]
    fn register_address_constants_match_re2() {
        // Mirror RE2 §5.3 lines 505-527 verbatim.
        assert_eq!(Apw12PlusReg::MfrId.as_u8(), 0x00);
        assert_eq!(Apw12PlusReg::Model.as_u8(), 0x01);
        assert_eq!(Apw12PlusReg::Revision.as_u8(), 0x02);
        assert_eq!(Apw12PlusReg::Status.as_u8(), 0x03);
        assert_eq!(Apw12PlusReg::Vout.as_u8(), 0x04);
        assert_eq!(Apw12PlusReg::Iout.as_u8(), 0x05);
        assert_eq!(Apw12PlusReg::Pout.as_u8(), 0x06);
        assert_eq!(Apw12PlusReg::Temp1.as_u8(), 0x0A);
        assert_eq!(Apw12PlusReg::Temp2.as_u8(), 0x0B);
        assert_eq!(Apw12PlusReg::Temp3.as_u8(), 0x0C);
        assert_eq!(Apw12PlusReg::Fan1RpmLo.as_u8(), 0x0D);
        assert_eq!(Apw12PlusReg::Fan1RpmHi.as_u8(), 0x0E);
        assert_eq!(Apw12PlusReg::Fan2RpmLo.as_u8(), 0x0F);
        assert_eq!(Apw12PlusReg::Fan2RpmHi.as_u8(), 0x10);
        assert_eq!(Apw12PlusReg::Vin.as_u8(), 0x11);
        assert_eq!(Apw12PlusReg::Iin.as_u8(), 0x12);
        assert_eq!(Apw12PlusReg::Pin.as_u8(), 0x13);
        assert_eq!(Apw12PlusReg::Efficiency.as_u8(), 0x14);
        assert_eq!(Apw12PlusReg::Alarm.as_u8(), 0x18);
        assert_eq!(Apw12PlusReg::Control.as_u8(), 0x20);
        assert_eq!(Apw12PlusReg::ClearFaults.as_u8(), 0x21);
        assert_eq!(Apw12PlusReg::SetPowerLimit.as_u8(), 0x22);

        assert_eq!(APW12_PLUS_I2C_ADDR, 0x10);
        assert_eq!(GPIO_PSU_ENABLE, 907);
        assert_eq!(CONTROL_OFF, 0x00);
        assert_eq!(CONTROL_ON, 0x01);
    }

    #[test]
    #[allow(non_snake_case)]
    fn vout_decode_scales_by_001V() {
        // 0x05DC = 1500 → 15.00 V (typical 12V rail under load)
        let v = decode_centivolt(&[0xDC, 0x05]).expect("two bytes decodes");
        assert!((v - 15.0).abs() < 0.001, "got {}", v);

        // 0x04B0 = 1200 → 12.00 V (nominal)
        let v = decode_centivolt(&[0xB0, 0x04]).expect("two bytes decodes");
        assert!((v - 12.0).abs() < 0.001, "got {}", v);

        // Short buffer → None
        assert!(decode_centivolt(&[0xDC]).is_none());
    }

    #[test]
    #[allow(non_snake_case)]
    fn iout_decode_scales_by_01A() {
        // 0x012C = 300 → 30.0 A
        let i = decode_deciamp(&[0x2C, 0x01]).expect("two bytes decodes");
        assert!((i - 30.0).abs() < 0.001, "got {}", i);
    }

    #[test]
    fn power_on_writes_0x01_to_register_0x20() {
        let steps = power_on_steps();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &[0x20, 0x01], "[reg=Control, payload=ON]");
            }
            other => panic!("expected Write, got {:?}", other),
        }
    }

    #[test]
    fn power_off_writes_0x00_to_register_0x20() {
        let steps = power_off_steps();
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &[0x20, 0x00], "[reg=Control, payload=OFF]");
            }
            other => panic!("expected Write, got {:?}", other),
        }
    }

    #[test]
    fn clear_faults_targets_register_0x21() {
        match &clear_faults_steps()[0] {
            I2cTransactionStep::Write(buf) => assert_eq!(buf, &[0x21, 0x01]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn set_power_limit_encodes_le_u32_to_register_0x22() {
        // 4000 W = 0x00000FA0 → LE [0xA0, 0x0F, 0x00, 0x00]
        match &set_power_limit_steps(4000)[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &[0x22, 0xA0, 0x0F, 0x00, 0x00]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn read_steps_use_write_then_read() {
        // VOUT read → write 0x04, read 2 bytes
        match &read_steps(Apw12PlusReg::Vout.as_u8(), 2)[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &[0x04]);
                assert_eq!(*read_len, 2);
            }
            _ => unreachable!(),
        }
        // STATUS read → write 0x03, read 4 bytes
        match &read_steps(Apw12PlusReg::Status.as_u8(), 4)[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &[0x03]);
                assert_eq!(*read_len, 4);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn power_limit_bounds_rejected() {
        let (handle, _rx) = I2cServiceHandle::for_unit_tests();
        let mut psu = Apw12PlusBackend::new(handle, S21AmlogicNoPic);

        // Zero is rejected
        assert!(psu.set_power_limit_w(0).is_err());
        // Above ceiling is rejected
        assert!(psu.set_power_limit_w(POWER_LIMIT_MAX_W + 1).is_err());
    }

    #[test]
    fn fan_idx_out_of_range_rejected() {
        let (handle, _rx) = I2cServiceHandle::for_unit_tests();
        let mut psu = Apw12PlusBackend::new(handle, S21ProAmlogic);

        // idx 2 is invalid (only fan 0/1 exist on APW12+)
        let r = psu.read_fan_rpm(2);
        assert!(r.is_err());
    }

    // ----------------------- mock-worker test helpers -----------------------

    fn spawn_mock_worker(
        rx: Receiver<I2cRequest>,
        replies: Vec<Result<Vec<Vec<u8>>>>,
    ) -> thread::JoinHandle<Vec<(u8, Vec<I2cTransactionStep>)>> {
        thread::spawn(move || {
            let mut log = Vec::new();
            let mut replies = replies.into_iter();
            while let Ok(req) = rx.recv() {
                if let I2cRequest::Transaction {
                    addr,
                    steps,
                    reply_tx,
                } = req
                {
                    log.push((addr, steps));
                    let r = replies.next().unwrap_or_else(|| Ok(Vec::new()));
                    let _ = reply_tx.send(r);
                } else {
                    panic!("unexpected non-transaction request: {:?}", req);
                }
            }
            log
        })
    }

    /// `read_mfr_id` issues a [WriteRead] at register 0x00 and surfaces the
    /// ASCII bytes returned by the slave verbatim.
    #[test]
    fn read_mfr_id_returns_ascii_bytes() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_mock_worker(rx, vec![Ok(vec![b"BITMAIN\0\0\0\0\0\0\0\0\0".to_vec()])]);

        let mut psu = Apw12PlusBackend::new(handle, S21AmlogicNoPic);
        let mfr = psu.read_mfr_id().expect("transport");

        drop(psu);
        let log = worker.join().expect("worker");

        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, APW12_PLUS_I2C_ADDR);
        match &log[0].1[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &[0x00], "register byte = MFR_ID");
                assert_eq!(*read_len, 16, "default read length = 16");
            }
            other => panic!("expected WriteRead, got {:?}", other),
        }
        assert_eq!(&mfr[..7], b"BITMAIN");
    }

    /// `read_vout_v` decodes a centivolt word into volts.
    #[test]
    fn read_vout_decodes_centivolts() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let _worker = spawn_mock_worker(
            rx,
            vec![Ok(vec![vec![0xDC, 0x05]])], // 0x05DC = 1500 → 15.00 V
        );

        let mut psu = Apw12PlusBackend::new(handle, S21XpAmlogic);
        let v = psu.read_vout_v().expect("transport");
        assert!((v - 15.0).abs() < 0.001, "got {}", v);
    }

    /// `power_on` issues exactly one transaction with `[0x20, 0x01]`.
    #[test]
    fn power_on_runtime_writes_correct_bytes() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_mock_worker(rx, vec![Ok(Vec::new())]);

        let mut psu = Apw12PlusBackend::new(handle, S21AmlogicNoPic);
        psu.power_on().expect("power_on");

        drop(psu);
        let log = worker.join().expect("worker");

        assert_eq!(log.len(), 1);
        match &log[0].1[0] {
            I2cTransactionStep::Write(buf) => assert_eq!(buf, &[0x20, 0x01]),
            other => panic!("expected Write, got {:?}", other),
        }
    }

    /// `power_off` issues exactly one transaction with `[0x20, 0x00]`.
    #[test]
    fn power_off_runtime_writes_correct_bytes() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_mock_worker(rx, vec![Ok(Vec::new())]);

        let mut psu = Apw12PlusBackend::new(handle, S21AmlogicNoPic);
        psu.power_off().expect("power_off");

        drop(psu);
        let log = worker.join().expect("worker");

        assert_eq!(log.len(), 1);
        match &log[0].1[0] {
            I2cTransactionStep::Write(buf) => assert_eq!(buf, &[0x20, 0x00]),
            other => panic!("expected Write, got {:?}", other),
        }
    }
}
