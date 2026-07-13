//! APW12 SMBus PSU driver — opcode-based I2C protocol on `/dev/i2c-N` @ 0x10.
//!
//! Used on **CV1835 / AM335x BB / Amlogic S19j Pro** with PIC1704 voltage
//! controllers. **NOT for Zynq am2** — that uses the [`crate::psu::Apw121215a`]
//! struct (legacy `[55 AA LEN CMD ... SUM]` framed protocol).
//!
//! # Reference
//!
//! - `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/SOURCE_HAL/apw12.h`
//!   — opcode + bound + GPIO definitions (108 LOC)
//! - `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/SOURCE_HAL/apw12.c`
//!   — Linux SMBus ioctl helpers + init / shutdown sequencers (396 LOC)
//!
//! # Wire protocol
//!
//! All transactions are SMBus byte / word / block at slave address 0x10:
//!
//! | Opcode | Direction        | Data           |
//! |-------:|------------------|----------------|
//! | `0x00` | write byte       | POWER_OFF       |
//! | `0x01` | write byte       | POWER_ON        |
//! | `0x02` | write word LE    | SET_VOLTAGE mV  |
//! | `0x03` | read word LE     | READ_VOLTAGE mV |
//! | `0x04` | read word LE     | GET_FW_VERSION  |
//! | `0x05` | read block (32B) | READ_TELEMETRY  |
//! | `0x06` | write word LE    | ENABLE_WDOG ms  |
//! | `0x07` | write word LE=0  | DISABLE_WDOG    |
//! | `0x08` | read word LE     | GET_HW_VERSION  |
//! | `0x09` | read byte        | CALIB_STATUS    |
//! | `0x0A` | read word LE     | GET_AC_POWER    |
//! | `0x0B` | read word LE     | READ_ERROR_CODE |
//! | `0x0C` | read block       | READ_ERROR_DATA |
//! | `0x0D` | read word LE     | GET_UPDATE_TIME |
//! | `0x0E` | read byte        | GET_RESET_CAUSE |
//! | `0x0F` | write byte       | CLEAR_FAULTS    |
//! | `0x10` | read block       | GET_POWER_SN    |
//!
//! SMBus over kernel I2C is implemented via "register-as-opcode" reads/writes:
//! the SMBus opcode lives in the I²C register-address byte, payload is the
//! SMBus data byte/word. We build that wire pattern as
//! [`crate::i2c::I2cTransactionStep::Write`] (write opcode + payload) and
//! [`crate::i2c::I2cTransactionStep::WriteRead`] (write opcode, repeated
//! START, read N bytes), all routed through [`I2cServiceHandle`] per the
//! AM2 SINGLE-I2C-OWNER architecture rule.
//!
//! # Construction is sealed-trait gated
//!
//! [`Apw12SmbusBackend::new`] requires a marker type implementing
//! [`Apw12SmbusAuthorized`]. The trait is sealed and only the platform
//! markers in [`platforms`] satisfy it. Out-of-crate code cannot register
//! a rogue platform.
//!
//! These markers are **parallel** to (not re-exported from)
//! `dcentrald_asic::pic1704::service::platforms`. We can't depend on that
//! crate from `dcentrald-hal` without forming a dependency cycle
//! (`dcentrald-asic -> dcentrald-hal`). The runtime gate is provided by
//! `dcentrald_hal::platform::subtype::classify_with_probe` returning
//! `VoltageControllerKind::Pic1704`; these compile-time markers are an
//! independent defense-in-depth check.
//!
//! # Cold-boot orchestration
//!
//! See [`Apw12SmbusBackend::cold_boot_sequence_5_step`] for the five-step
//! init flow lifted from RE2 §3 (`apw12.c::apw12_init_sequence`):
//! POWER_ON → fw probe → SET_VOLTAGE → telemetry confirm → ENABLE_WDOG.

use std::time::{Duration, Instant};

use crate::i2c::{I2cOperationIntent, I2cServiceHandle, I2cTransactionStep};
use crate::HalError;
use crate::Result;

// ===========================================================================
//  Constants — mirror apw12.h verbatim
// ===========================================================================

/// SMBus slave address for APW12 (RE2 `APW12_I2C_ADDR`).
pub const APW12_I2C_ADDR: u8 = 0x10;

/// Voltage envelope (RE2 `APW12_VOLTAGE_MIN_MV` / `APW12_VOLTAGE_MAX_MV`).
pub const VOLTAGE_MIN_MV: u16 = 1200;
pub const VOLTAGE_MAX_MV: u16 = 1600;

/// Block-data length for `READ_TELEMETRY` opcode (RE2 `APW12_TELEMETRY_BLOCK_LEN`).
pub const TELEMETRY_BLOCK_LEN: usize = 32;

/// Watchdog timeout bounds (RE2 `APW12_WDOG_MIN_MS` / `APW12_WDOG_MAX_MS`).
pub const WDOG_MIN_MS: u16 = 100;
pub const WDOG_MAX_MS: u16 = 60_000;

/// Sysfs GPIO line for PSU enable on **CV1835 S19j Pro** (RE2
/// `APW12_GPIO_PSU_ENABLE = 412`).
///
/// **Platform-specific.** Only meaningful on CV1835. AM335x BB and Amlogic
/// S19j Pro use different GPIO maps and do not gate the PSU on this line.
/// Caller is responsible for GPIO sysfs export / direction / value writes —
/// this driver does not touch GPIO directly.
pub const GPIO_PSU_ENABLE: u32 = 412;

/// Expected firmware version word for S19j Pro APW12 (RE2 `APW12_EXPECTED_FW_VER`).
pub const EXPECTED_FW_VER: u16 = 0x0103;

/// Delay after POWER_ON before any further opcode is issued (RE2 50 ms +
/// our extra 200 ms guard for slow-rail SMBus PSUs). RE2 `apw12_init_sequence`
/// uses `usleep(50000)`; we extend to 250 ms because the AM2/CV1835 rail
/// settling time has been observed up to ~200 ms on bench units.
pub const POWER_ON_SETTLE_MS: u64 = 250;

/// Default cold-boot watchdog timeout (RE2 `apw12_init_sequence` step 5
/// uses 5000 ms).
pub const DEFAULT_COLD_BOOT_WDOG_MS: u16 = 5000;

/// Default cold-boot voltage (RE2 `apw12_init_sequence` step 3 uses 1420 mV
/// "typical for S19j Pro").
pub const DEFAULT_COLD_BOOT_VOLTAGE_MV: u16 = 1420;

/// Calibration-ready poll budget. RE2 `apw12_is_calibrated` polls 50 × 2 ms
/// = 100 ms. We extend to 2 minutes because the dev-kit init banner says
/// "Initializing the power, please wait, this may take up to 2 minutes...".
pub const CALIBRATION_POLL_TIMEOUT_MS: u64 = 120_000;
pub const CALIBRATION_POLL_INTERVAL_MS: u64 = 100;

// ===========================================================================
//  Opcode enum
// ===========================================================================

/// APW12 SMBus opcodes (mirrors `APW12_CMD_*` from `apw12.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Apw12Cmd {
    PowerOff = 0x00,
    PowerOn = 0x01,
    SetVoltage = 0x02,
    ReadVoltage = 0x03,
    GetFwVersion = 0x04,
    ReadTelemetry = 0x05,
    EnableWdog = 0x06,
    DisableWdog = 0x07,
    GetHwVersion = 0x08,
    CalibStatus = 0x09,
    GetAcPower = 0x0A,
    ReadErrorCode = 0x0B,
    ReadErrorData = 0x0C,
    GetUpdateTime = 0x0D,
    GetResetCause = 0x0E,
    ClearFaults = 0x0F,
    GetPowerSn = 0x10,
}

impl Apw12Cmd {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

// ===========================================================================
//  Sealed trait — platform whitelist for `Apw12SmbusBackend::new`
// ===========================================================================

mod sealed {
    /// Sealed marker — out-of-crate code cannot implement
    /// [`super::Apw12SmbusAuthorized`].
    pub trait Sealed {}
}

/// Sealed-trait whitelist for [`Apw12SmbusBackend::new`].
///
/// Implementors are limited to the marker types in [`platforms`]. The
/// trait is sealed via `sealed::Sealed`, so adding a new platform requires
/// editing this module — there's no out-of-crate escape hatch.
///
/// # Why the marker types are parallel to `pic1704`
///
/// `dcentrald-hal` cannot depend on `dcentrald-asic` (that would form a
/// dep cycle: `dcentrald-asic -> dcentrald-hal -> dcentrald-asic`).
/// `pic1704` markers live in `dcentrald-asic`, so we cannot import them
/// here. We therefore re-declare equivalent zero-sized markers in this
/// module. The runtime gate
/// (`dcentrald_hal::platform::subtype::classify_with_probe`) is the
/// authoritative platform classifier; these compile-time markers are
/// belt-and-suspenders.
pub trait Apw12SmbusAuthorized: sealed::Sealed {}

/// Marker types for platforms that own APW12-protocol PSUs.
///
/// All SHIP markers — `Cv1835S19jPro`, `Am335xBbS19jPro`, `AmlogicS19jPro` —
/// match the PIC1704 platform whitelist exactly. `Cv1835S19`, `Cv1835S19i`,
/// and `Cv1835S19XP` are forward-looking placeholders for the W11C
/// cross-platform broadening pass; their PIC1704 counterparts are still
/// pending in `pic1704/service.rs`.
pub mod platforms {
    /// CV1835 S19j Pro — Sophgo CV1835 SoC, eMMC-rooted variant.
    pub struct Cv1835S19jPro;
    /// AM335x BB S19j Pro — BeagleBone-class control board.
    pub struct Am335xBbS19jPro;
    /// Amlogic S19j Pro variants. NOT S19k Pro (BHB56xxx) and NOT S21
    /// NoPic — those families do not use APW12 SMBus.
    pub struct AmlogicS19jPro;
    /// CV1835 S19 — placeholder for W11C cross-platform broadening.
    pub struct Cv1835S19;
    /// CV1835 S19i — placeholder for W11C cross-platform broadening.
    pub struct Cv1835S19i;
    /// CV1835 S19 XP — placeholder for W11C cross-platform broadening.
    pub struct Cv1835S19XP;
}

impl sealed::Sealed for platforms::Cv1835S19jPro {}
impl Apw12SmbusAuthorized for platforms::Cv1835S19jPro {}

impl sealed::Sealed for platforms::Am335xBbS19jPro {}
impl Apw12SmbusAuthorized for platforms::Am335xBbS19jPro {}

impl sealed::Sealed for platforms::AmlogicS19jPro {}
impl Apw12SmbusAuthorized for platforms::AmlogicS19jPro {}

impl sealed::Sealed for platforms::Cv1835S19 {}
impl Apw12SmbusAuthorized for platforms::Cv1835S19 {}

impl sealed::Sealed for platforms::Cv1835S19i {}
impl Apw12SmbusAuthorized for platforms::Cv1835S19i {}

impl sealed::Sealed for platforms::Cv1835S19XP {}
impl Apw12SmbusAuthorized for platforms::Cv1835S19XP {}

// ===========================================================================
//  Pure step-builders (host-testable, no I/O)
// ===========================================================================

/// Build a one-step write transaction for an SMBus "write byte" form
/// (opcode + payload byte). Some APW12 opcodes take no payload (POWER_ON,
/// POWER_OFF, CLEAR_FAULTS) — pass an empty payload for those.
///
/// SMBus encodes the opcode as the first byte on the wire, followed by the
/// payload (1 byte for write_byte, 2 bytes LE for write_word).
fn write_steps(opcode: u8, payload: &[u8]) -> Vec<I2cTransactionStep> {
    let mut buf = Vec::with_capacity(1 + payload.len());
    buf.push(opcode);
    buf.extend_from_slice(payload);
    vec![I2cTransactionStep::Write(buf)]
}

/// Build a write+read transaction (SMBus "read byte/word"): write the
/// opcode, repeated START, read `read_len` bytes.
fn read_steps(opcode: u8, read_len: usize) -> Vec<I2cTransactionStep> {
    vec![I2cTransactionStep::WriteRead {
        write_data: vec![opcode],
        read_len,
    }]
}

/// Build the SET_VOLTAGE write step. `mv` must be in [VOLTAGE_MIN_MV,
/// VOLTAGE_MAX_MV]; the caller is responsible for the bounds check (this
/// helper is reused by tests that need to encode out-of-range values to
/// confirm the bounds gate works).
pub fn set_voltage_steps(mv: u16) -> Vec<I2cTransactionStep> {
    let lo = (mv & 0xFF) as u8;
    let hi = ((mv >> 8) & 0xFF) as u8;
    write_steps(Apw12Cmd::SetVoltage.as_u8(), &[lo, hi])
}

/// Build the ENABLE_WDOG write step. Same bounds-check caveat as
/// [`set_voltage_steps`].
pub fn enable_watchdog_steps(timeout_ms: u16) -> Vec<I2cTransactionStep> {
    let lo = (timeout_ms & 0xFF) as u8;
    let hi = ((timeout_ms >> 8) & 0xFF) as u8;
    write_steps(Apw12Cmd::EnableWdog.as_u8(), &[lo, hi])
}

/// Build the DISABLE_WDOG write step (write word LE = 0x0000).
pub fn disable_watchdog_steps() -> Vec<I2cTransactionStep> {
    write_steps(Apw12Cmd::DisableWdog.as_u8(), &[0x00, 0x00])
}

/// POWER_ON write step (no payload).
pub fn power_on_steps() -> Vec<I2cTransactionStep> {
    write_steps(Apw12Cmd::PowerOn.as_u8(), &[])
}

/// POWER_OFF write step (no payload).
pub fn power_off_steps() -> Vec<I2cTransactionStep> {
    write_steps(Apw12Cmd::PowerOff.as_u8(), &[])
}

/// READ_VOLTAGE read step (returns 2 bytes LE mV).
pub fn read_voltage_steps() -> Vec<I2cTransactionStep> {
    read_steps(Apw12Cmd::ReadVoltage.as_u8(), 2)
}

/// GET_FW_VERSION read step (returns 2 bytes LE).
pub fn get_fw_version_steps() -> Vec<I2cTransactionStep> {
    read_steps(Apw12Cmd::GetFwVersion.as_u8(), 2)
}

/// READ_TELEMETRY read step (returns 32 bytes block).
pub fn read_telemetry_steps() -> Vec<I2cTransactionStep> {
    read_steps(Apw12Cmd::ReadTelemetry.as_u8(), TELEMETRY_BLOCK_LEN)
}

/// CALIB_STATUS read step (returns 1 byte).
pub fn read_calibration_steps() -> Vec<I2cTransactionStep> {
    read_steps(Apw12Cmd::CalibStatus.as_u8(), 1)
}

// ===========================================================================
//  Decoders
// ===========================================================================

/// Decode a little-endian u16 from the first 2 bytes of `buf`. Returns
/// `None` if the buffer is too short.
pub fn decode_le_word(buf: &[u8]) -> Option<u16> {
    if buf.len() < 2 {
        return None;
    }
    Some(u16::from(buf[0]) | (u16::from(buf[1]) << 8))
}

/// Parsed APW12 telemetry block (RE2 `apw12_telemetry_t`).
///
/// All fields are unsigned because the PSU never reports negative values;
/// undefined fields read 0 when the response is short (mirrors RE2
/// `apw12_read_telemetry` behavior of `memset(tel, 0, sizeof(*tel))` for
/// short reads `< 9`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Apw12Telemetry {
    pub dc_volt_mv: u16,
    pub set_volt_mv: u16,
    pub current_ma: u16,
    pub power_w: u16,
    pub status_bits: u8,
}

impl Apw12Telemetry {
    /// Parse the raw 32-byte telemetry block. RE2 layout:
    ///
    /// | Offset | Field           | Encoding |
    /// |-------:|-----------------|----------|
    /// |    0-1 | `dc_volt_mv`    | u16 LE   |
    /// |    2-3 | `set_volt_mv`   | u16 LE   |
    /// |    4-5 | `current_ma`    | u16 LE   |
    /// |    6-7 | `power_w`       | u16 LE   |
    /// |      8 | `status_bits`   | u8       |
    ///
    /// Returns the all-zero telemetry on a short read (`<9` bytes), per
    /// RE2 `apw12_read_telemetry`.
    pub fn parse(raw: &[u8]) -> Self {
        if raw.len() < 9 {
            return Self::default();
        }
        Self {
            dc_volt_mv: u16::from(raw[0]) | (u16::from(raw[1]) << 8),
            set_volt_mv: u16::from(raw[2]) | (u16::from(raw[3]) << 8),
            current_ma: u16::from(raw[4]) | (u16::from(raw[5]) << 8),
            power_w: u16::from(raw[6]) | (u16::from(raw[7]) << 8),
            status_bits: raw[8],
        }
    }
}

// ===========================================================================
//  Apw12SmbusBackend — the runtime controller
// ===========================================================================

/// Best locally known result of APW12 output-control commands.
///
/// This is command-completion evidence, not an independent rail measurement.
/// `Unknown` is used on attach and whenever a submitted command's outcome is
/// not known. Callers must not treat `On` or `Off` as physical telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsuOutputState {
    Unknown,
    On,
    Off,
}

/// Service-thread-backed APW12 SMBus PSU controller.
///
/// All I/O goes through a shared [`I2cServiceHandle`] — never raw
/// `I2cBus::open(...)`. Construction is gated by [`Apw12SmbusAuthorized`]
/// at compile time.
pub struct Apw12SmbusBackend {
    i2c: I2cServiceHandle,
    address: u8,
    /// Cached firmware version word (0 until [`Self::get_fw_version`] runs).
    fw_version: u16,
    /// Best locally known power-command outcome. This deliberately starts and
    /// returns to `Unknown` when transport completion is not observable.
    output_state: PsuOutputState,
    /// Cached watchdog-armed flag — APW12 has no readable watchdog state,
    /// so we track it locally.
    watchdog_armed: bool,
    /// Last accepted watchdog timeout, retained for shutdown compensation.
    watchdog_timeout_ms: Option<u16>,
}

/// Arms one APW12 power rollback for the lifetime of a boot attempt.
///
/// Ordinary errors are handled explicitly by [`run_with_power_rollback`] so
/// both the primary and rollback results can be returned. In unwind-enabled
/// builds, `Drop` is a best-effort panic fallback. Release firmware uses
/// `panic = "abort"`, so its process-wide platform panic hook is the only
/// software panic cutoff; this guard's `Drop` does not run there.
/// `rollback_attempted` is set before submitting the worker-owned plan so an
/// unwind cannot enqueue a duplicate plan after an unknown receipt outcome.
struct Apw12PowerRollbackGuard<'a> {
    psu: &'a mut Apw12SmbusBackend,
    context: &'static str,
    committed: bool,
    rollback_attempted: bool,
}

impl Apw12PowerRollbackGuard<'_> {
    fn attempt_rollback(&mut self) -> crate::PowerRollbackOutcome {
        debug_assert!(!self.rollback_attempted);
        self.rollback_attempted = true;
        match self.psu.power_off_with_disarm() {
            Ok(()) => crate::PowerRollbackOutcome::Completed,
            Err(error) => crate::PowerRollbackOutcome::Failed(Box::new(error)),
        }
    }
}

impl Drop for Apw12PowerRollbackGuard<'_> {
    fn drop(&mut self) {
        if self.committed || self.rollback_attempted {
            return;
        }
        self.rollback_attempted = true;
        let rollback = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.psu.power_off_with_disarm()
        }));
        match rollback {
            Ok(Ok(())) => tracing::warn!(
                context = self.context,
                "APW12 unwind-only rollback completed (release panic=abort uses the platform panic hook instead)"
            ),
            Ok(Err(error)) => tracing::error!(
                context = self.context,
                %error,
                "APW12 unwind-only rollback failed; no duplicate plan will be submitted"
            ),
            Err(_) => tracing::error!(
                context = self.context,
                "APW12 unwind-only rollback panicked; no duplicate plan will be submitted"
            ),
        }
    }
}

/// Execute work while APW12 output may be energized.
///
/// On ordinary failure, exactly one worker-owned disarm + POWER_OFF plan is
/// attempted and its typed result is attached without replacing `primary`.
/// The guard is armed before `body` begins, covering even a POWER_ON request
/// whose transport result is unknown after the hardware may have accepted it.
pub(crate) fn run_with_power_rollback<T>(
    psu: &mut Apw12SmbusBackend,
    context: &'static str,
    body: impl FnOnce(&mut Apw12SmbusBackend) -> Result<T>,
) -> Result<T> {
    let mut guard = Apw12PowerRollbackGuard {
        psu,
        context,
        committed: false,
        rollback_attempted: false,
    };
    match body(&mut *guard.psu) {
        Ok(value) => {
            guard.committed = true;
            Ok(value)
        }
        Err(primary) => {
            let rollback = guard.attempt_rollback();
            guard.committed = true;
            Err(HalError::PartialBootRollback {
                context,
                primary: Box::new(primary),
                rollback,
            })
        }
    }
}

impl Apw12SmbusBackend {
    /// Construct an APW12 SMBus controller for one of the whitelisted
    /// platforms.
    ///
    /// The `_marker: P` argument is a compile-time-only proof of platform
    /// authorization (zero-sized, optimized away). Address defaults to
    /// [`APW12_I2C_ADDR`] (0x10); use [`Self::new_at`] for a custom slave.
    pub fn new<P: Apw12SmbusAuthorized>(handle: I2cServiceHandle, _marker: P) -> Self {
        Self::new_at(handle, APW12_I2C_ADDR, _marker)
    }

    /// Construct at a specific slave address (rare — most boards use 0x10).
    pub fn new_at<P: Apw12SmbusAuthorized>(
        handle: I2cServiceHandle,
        address: u8,
        _marker: P,
    ) -> Self {
        Self {
            i2c: handle,
            address,
            fw_version: 0,
            output_state: PsuOutputState::Unknown,
            watchdog_armed: false,
            watchdog_timeout_ms: None,
        }
    }

    /// Cached I2C slave address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Last observed firmware version word (`0` until first
    /// [`Self::get_fw_version`] call).
    pub fn fw_version(&self) -> u16 {
        self.fw_version
    }

    /// Best locally known output-command state.
    ///
    /// This is never physical rail telemetry. It is `Unknown` until a command
    /// completes and returns to `Unknown` before any mutation whose outcome
    /// may become unobservable.
    pub fn output_state(&self) -> PsuOutputState {
        self.output_state
    }

    /// Whether the watchdog is currently armed, by our cached state.
    pub fn is_watchdog_armed(&self) -> bool {
        self.watchdog_armed
    }

    // -----------------------------------------------------------------------
    //  Power control
    // -----------------------------------------------------------------------

    /// Send POWER_ON (RE2 `apw12_power_on`) and sleep [`POWER_ON_SETTLE_MS`].
    ///
    /// RE2's `apw12_init_sequence` does the settle inside a `usleep(50000)`
    /// after `apw12_power_on`. We bake the sleep into this method so callers
    /// don't have to remember it.
    pub fn power_on(&mut self) -> Result<()> {
        self.output_state = PsuOutputState::Unknown;
        self.run_with_intent(I2cOperationIntent::Energize, power_on_steps())?;
        std::thread::sleep(Duration::from_millis(POWER_ON_SETTLE_MS));
        self.output_state = PsuOutputState::On;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            settle_ms = POWER_ON_SETTLE_MS,
            "APW12 POWER_ON"
        );
        Ok(())
    }

    /// **DEPRECATED for external use** — prefer [`Self::power_off_with_disarm`].
    ///
    /// Raw POWER_OFF (RE2 `apw12_power_off`). Does NOT disarm the watchdog
    /// first — if the watchdog is armed when this fires, the PSU may
    /// auto-restart on watchdog-elapse. RE2 §3 explicitly orders watchdog
    /// disable before power-off; honour that ordering by calling
    /// [`Self::power_off_with_disarm`] instead.
    #[doc(hidden)]
    pub fn power_off_raw(&mut self) -> Result<()> {
        self.output_state = PsuOutputState::Unknown;
        self.run_with_intent(I2cOperationIntent::SafeOff, power_off_steps())?;
        self.output_state = PsuOutputState::Off;
        Ok(())
    }

    /// Disarm watchdog (`0x06` with timeout=0) THEN POWER_OFF (`0x00`).
    ///
    /// Mirrors RE2 `apw12_shutdown_sequence` ordering. If the PSU has no
    /// armed watchdog this still issues both opcodes — APW12 hardware
    /// accepts a redundant disarm gracefully.
    ///
    /// If POWER_OFF fails after watchdog disarm may have succeeded, the driver
    /// makes one bounded compensating watchdog-arm attempt. Cache changes and
    /// return values describe completed transport commands only; APW12 exposes
    /// no independent physical rail-off observation here.
    pub fn power_off_with_disarm(&mut self) -> Result<()> {
        let (compensation_timeout, compensation_policy) = match self.watchdog_timeout_ms {
            Some(timeout) => (timeout, "restore-known-prior-timeout"),
            None => (
                WDOG_MIN_MS,
                "emergency-minimum-timeout-prior-configuration-unknown",
            ),
        };
        self.output_state = PsuOutputState::Unknown;
        let outcome = self.i2c.conditional_safe_off_plan(
            self.address,
            disable_watchdog_steps(),
            power_off_steps(),
            enable_watchdog_steps(compensation_timeout),
        )?;

        if outcome.primary.completed() {
            self.output_state = PsuOutputState::Off;
            if outcome.prelude.completed() || outcome.prelude_retry.completed() {
                self.watchdog_armed = false;
                tracing::info!(
                    addr = format_args!("0x{:02X}", self.address),
                    retried_disarm = !outcome.prelude.completed(),
                    "APW12 worker-owned POWER_OFF plan completed in a safe final command state; physical rail-off was not independently observed"
                );
                return Ok(());
            }
            return Err(HalError::PsuProtocolOwned(format!(
                "APW12 POWER_OFF completed, but watchdog disarm and post-off re-disarm failed (initial={:?}, retry={:?}); physical watchdog state is unknown",
                outcome.prelude, outcome.prelude_retry
            )));
        }

        if outcome.compensation.completed() {
            self.watchdog_armed = true;
            self.watchdog_timeout_ms = Some(compensation_timeout);
        } else if outcome.prelude.completed() {
            self.watchdog_armed = false;
        }
        Err(HalError::PsuProtocolOwned(format!(
            "APW12 POWER_OFF failed ({:?}); initial watchdog disarm={:?}; compensating watchdog arm={:?} at {} ms (policy={}); output state is unknown",
            outcome.primary,
            outcome.prelude,
            outcome.compensation,
            compensation_timeout,
            compensation_policy
        )))
    }

    // -----------------------------------------------------------------------
    //  Voltage
    // -----------------------------------------------------------------------

    /// SET_VOLTAGE in millivolts (RE2 `apw12_set_voltage_mv`). Bounds checked
    /// against [`VOLTAGE_MIN_MV`] / [`VOLTAGE_MAX_MV`].
    pub fn set_voltage_mv(&mut self, mv: u16) -> Result<()> {
        if !(VOLTAGE_MIN_MV..=VOLTAGE_MAX_MV).contains(&mv) {
            return Err(HalError::PsuProtocolOwned(format!(
                "set_voltage_mv: {} outside [{}, {}]",
                mv, VOLTAGE_MIN_MV, VOLTAGE_MAX_MV
            )));
        }
        self.run_with_intent(I2cOperationIntent::Energize, set_voltage_steps(mv))?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            mv,
            "APW12 SET_VOLTAGE"
        );
        Ok(())
    }

    /// READ_VOLTAGE (RE2 `apw12_read_voltage_mv` per spec). Returns mV LE.
    pub fn read_voltage_mv(&mut self) -> Result<u16> {
        let buf = self.run_read(read_voltage_steps())?;
        decode_le_word(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "READ_VOLTAGE returned {} bytes (need 2)",
                buf.len()
            ))
        })
    }

    // -----------------------------------------------------------------------
    //  Version / status
    // -----------------------------------------------------------------------

    /// GET_FW_VERSION (RE2 `apw12_get_fw_version`). Bails with
    /// [`HalError::PsuUnsupported`] if the version is not [`EXPECTED_FW_VER`]
    /// (0x0103). Caller can override with [`Self::get_fw_version_unchecked`].
    pub fn get_fw_version(&mut self) -> Result<u16> {
        let ver = self.get_fw_version_unchecked()?;
        if ver != EXPECTED_FW_VER {
            return Err(HalError::PsuUnsupported(format!(
                "APW12 fw=0x{:04X}, expected 0x{:04X}",
                ver, EXPECTED_FW_VER
            )));
        }
        Ok(ver)
    }

    /// Same as [`Self::get_fw_version`] but does not enforce the
    /// [`EXPECTED_FW_VER`] match. Useful for diagnostic dumps and bring-up.
    pub fn get_fw_version_unchecked(&mut self) -> Result<u16> {
        let buf = self.run_read(get_fw_version_steps())?;
        let ver = decode_le_word(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "GET_FW_VERSION returned {} bytes (need 2)",
                buf.len()
            ))
        })?;
        self.fw_version = ver;
        Ok(ver)
    }

    /// GET_HW_VERSION read (RE2 `apw12_get_hw_version`). Diagnostic only —
    /// no driver gating depends on the value.
    pub fn get_hw_version(&mut self) -> Result<u16> {
        let buf = self.run_read(read_steps(Apw12Cmd::GetHwVersion.as_u8(), 2))?;
        decode_le_word(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "GET_HW_VERSION returned {} bytes (need 2)",
                buf.len()
            ))
        })
    }

    /// READ_TELEMETRY block read with timeout-fallback semantics
    /// (RE2 `apw12_read_telemetry` returns short reads as zero-fill; we
    /// translate that to `Ok(None)` for callers that want to distinguish
    /// "PSU said nothing" from "PSU said all zeros").
    ///
    /// Returns:
    /// - `Ok(Some(t))` — fresh, parseable telemetry block
    /// - `Ok(None)` — short read or NACK (PSU is alive but does not yet
    ///   support telemetry on this firmware revision; caller should treat
    ///   as "telemetry unavailable")
    /// - `Err(...)` — bus / transport failure
    pub fn read_telemetry(&mut self) -> Result<Option<Apw12Telemetry>> {
        let buf = match self.run_read(read_telemetry_steps()) {
            Ok(b) => b,
            Err(HalError::PsuTelemetryUnavailable(_)) => return Ok(None),
            Err(e) => {
                // Treat any I2C-level NACK as Ok(None). RE2's apw12_read_telemetry
                // returns short reads as zero-fill; we mirror that and let
                // higher layers decide.
                if matches!(&e, HalError::I2c { .. }) {
                    tracing::debug!(
                        addr = format_args!("0x{:02X}", self.address),
                        error = %e,
                        "APW12 READ_TELEMETRY NACK — treating as unavailable",
                    );
                    return Ok(None);
                }
                return Err(e);
            }
        };
        if buf.len() < 9 {
            return Ok(None);
        }
        Ok(Some(Apw12Telemetry::parse(&buf)))
    }

    // -----------------------------------------------------------------------
    //  Watchdog
    // -----------------------------------------------------------------------

    /// ENABLE_WDOG with timeout in ms. Bounds checked against
    /// [`WDOG_MIN_MS`] / [`WDOG_MAX_MS`].
    pub fn enable_watchdog(&mut self, timeout_ms: u16) -> Result<()> {
        if !(WDOG_MIN_MS..=WDOG_MAX_MS).contains(&timeout_ms) {
            return Err(HalError::PsuProtocolOwned(format!(
                "enable_watchdog: {} ms outside [{}, {}]",
                timeout_ms, WDOG_MIN_MS, WDOG_MAX_MS
            )));
        }
        self.run_with_intent(
            I2cOperationIntent::NeutralControl,
            enable_watchdog_steps(timeout_ms),
        )?;
        self.watchdog_armed = true;
        self.watchdog_timeout_ms = Some(timeout_ms);
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            timeout_ms,
            "APW12 ENABLE_WDOG"
        );
        Ok(())
    }

    /// DISABLE_WDOG (RE2 `apw12_disable_watchdog`).
    pub fn disable_watchdog(&mut self) -> Result<()> {
        // Disarm alone removes a hardware cutoff and is neutral control, not
        // privileged SafeOff. The coordinated power-off plan owns the
        // reserved disarm phase.
        self.run_with_intent(I2cOperationIntent::NeutralControl, disable_watchdog_steps())?;
        self.watchdog_armed = false;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            "APW12 DISABLE_WDOG"
        );
        Ok(())
    }

    /// CALIB_STATUS read (RE2 `apw12_is_calibrated`). Returns `true` when
    /// the PSU's calibration EEPROM is loaded and ready for SET_VOLTAGE.
    ///
    /// RE2's polling logic is in [`Self::wait_for_calibration`] — this is
    /// the single-shot read.
    pub fn read_calibration_status(&mut self) -> Result<bool> {
        let buf = self.run_read(read_calibration_steps())?;
        if buf.is_empty() {
            return Err(HalError::PsuProtocolOwned(
                "CALIB_STATUS returned empty buffer".into(),
            ));
        }
        Ok(buf[0] != 0)
    }

    /// Poll CALIB_STATUS until ready or [`CALIBRATION_POLL_TIMEOUT_MS`]
    /// elapses. RE2's polling is bounded at ~100 ms; the dev-kit init
    /// banner says boot can take up to 2 minutes, so we extend.
    pub fn wait_for_calibration(&mut self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(CALIBRATION_POLL_TIMEOUT_MS);
        let interval = Duration::from_millis(CALIBRATION_POLL_INTERVAL_MS);
        loop {
            match self.read_calibration_status() {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!(
                        addr = format_args!("0x{:02X}", self.address),
                        error = %e,
                        "APW12 CALIB_STATUS poll error — retrying",
                    );
                }
            }
            if Instant::now() >= deadline {
                return Err(HalError::PsuProtocolOwned(format!(
                    "CALIB_STATUS poll timed out after {} ms",
                    CALIBRATION_POLL_TIMEOUT_MS
                )));
            }
            std::thread::sleep(interval);
        }
    }

    // -----------------------------------------------------------------------
    //  Cold-boot orchestration
    // -----------------------------------------------------------------------

    /// Five-step cold-boot sequence per RE2 §3 / `apw12_init_sequence`:
    ///
    /// 1. POWER_ON + 250 ms settle
    /// 2. GET_FW_VERSION + check vs [`EXPECTED_FW_VER`]
    /// 3. wait for calibration ready (poll CALIB_STATUS, up to 2 min)
    /// 4. SET_VOLTAGE to `target_mv`
    /// 5. READ_TELEMETRY confirm (DC volt non-zero), then ENABLE_WDOG
    ///
    /// This method honors RE2's ordering exactly. Deterministic parameter
    /// errors are rejected before POWER_ON. Any later failure triggers one
    /// worker-owned watchdog-disarm + POWER_OFF plan and returns
    /// [`HalError::PartialBootRollback`] containing both typed outcomes;
    /// callers must not proceed to mining.
    pub fn cold_boot_sequence_5_step(&mut self, target_mv: u16, wdog_ms: u16) -> Result<()> {
        // Reject deterministic configuration errors before POWER_ON. These
        // bounds are also enforced by the individual commands, but checking
        // here prevents an invalid final-stage watchdog value or voltage from
        // leaving an otherwise healthy PSU energized.
        if !(VOLTAGE_MIN_MV..=VOLTAGE_MAX_MV).contains(&target_mv) {
            return Err(HalError::PsuProtocolOwned(format!(
                "cold_boot target voltage {} outside [{}, {}]",
                target_mv, VOLTAGE_MIN_MV, VOLTAGE_MAX_MV
            )));
        }
        if !(WDOG_MIN_MS..=WDOG_MAX_MS).contains(&wdog_ms) {
            return Err(HalError::PsuProtocolOwned(format!(
                "cold_boot watchdog {} ms outside [{}, {}]",
                wdog_ms, WDOG_MIN_MS, WDOG_MAX_MS
            )));
        }

        run_with_power_rollback(self, "APW12 five-step cold boot", |psu| {
            psu.cold_boot_sequence_5_step_energized(target_mv, wdog_ms)
        })
    }

    /// Five-step body with no local rollback wrapper. Platform orchestrators
    /// use the public method and then immediately arm their own continuation
    /// guard through the final platform phase.
    fn cold_boot_sequence_5_step_energized(&mut self, target_mv: u16, wdog_ms: u16) -> Result<()> {
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            target_mv,
            wdog_ms,
            "APW12 cold_boot_sequence_5_step: starting",
        );

        // Step 1: POWER_ON.
        self.power_on()?;

        // Step 2: GET_FW_VERSION + check.
        let fw = self.get_fw_version()?;
        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            fw = format_args!("0x{:04X}", fw),
            "APW12 cold-boot step 2: fw OK",
        );

        // Step 3: wait for calibration ready (RE2's "this may take up to 2 minutes").
        self.wait_for_calibration()?;

        // Step 4: SET_VOLTAGE.
        self.set_voltage_mv(target_mv)?;

        // Step 5: telemetry confirm + ENABLE_WDOG.
        match self.read_telemetry()? {
            Some(tel) if tel.dc_volt_mv == 0 => {
                return Err(HalError::PsuProtocolOwned(
                    "APW12 cold-boot telemetry reports DC volt = 0".into(),
                ));
            }
            Some(_) | None => {
                // Some(_): DC volt nonzero, good. None: telemetry-unavailable
                // is acceptable on this firmware revision — RE2 falls through
                // to ENABLE_WDOG anyway when the read short-returns.
            }
        }
        self.enable_watchdog(wdog_ms)?;

        tracing::info!(
            addr = format_args!("0x{:02X}", self.address),
            "APW12 cold_boot_sequence_5_step: complete",
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    //  Diagnostic stubs for opcodes 0x0A-0x10
    // -----------------------------------------------------------------------

    /// GET_AC_POWER (RE2 `APW12_CMD_GET_AC_POWER` = 0x0A). Returns AC input
    /// power in watts (LE u16). Diagnostic only.
    pub fn get_ac_power_w(&mut self) -> Result<u16> {
        let buf = self.run_read(read_steps(Apw12Cmd::GetAcPower.as_u8(), 2))?;
        decode_le_word(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "GET_AC_POWER returned {} bytes (need 2)",
                buf.len()
            ))
        })
    }

    /// READ_ERROR_CODE (RE2 0x0B). Returns the LE u16 fault code. Diagnostic.
    pub fn read_error_code(&mut self) -> Result<u16> {
        let buf = self.run_read(read_steps(Apw12Cmd::ReadErrorCode.as_u8(), 2))?;
        decode_le_word(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "READ_ERROR_CODE returned {} bytes (need 2)",
                buf.len()
            ))
        })
    }

    /// READ_ERROR_DATA (RE2 0x0C). Returns up to 32 bytes of fault context.
    /// Diagnostic only.
    pub fn read_error_data(&mut self) -> Result<Vec<u8>> {
        self.run_read(read_steps(Apw12Cmd::ReadErrorData.as_u8(), 32))
    }

    /// GET_UPDATE_TIME (RE2 0x0D). LE u16 (semantics PSU-firmware-defined).
    pub fn get_update_time(&mut self) -> Result<u16> {
        let buf = self.run_read(read_steps(Apw12Cmd::GetUpdateTime.as_u8(), 2))?;
        decode_le_word(&buf).ok_or_else(|| {
            HalError::PsuProtocolOwned(format!(
                "GET_UPDATE_TIME returned {} bytes (need 2)",
                buf.len()
            ))
        })
    }

    /// GET_RESET_CAUSE (RE2 0x0E). One byte: PSU reset reason code.
    pub fn get_reset_cause(&mut self) -> Result<u8> {
        let buf = self.run_read(read_steps(Apw12Cmd::GetResetCause.as_u8(), 1))?;
        if buf.is_empty() {
            return Err(HalError::PsuProtocolOwned(
                "GET_RESET_CAUSE returned empty buffer".into(),
            ));
        }
        Ok(buf[0])
    }

    /// CLEAR_FAULTS (RE2 0x0F). Write byte; no payload.
    pub fn clear_faults(&mut self) -> Result<()> {
        self.run_with_intent(
            I2cOperationIntent::Recovery,
            write_steps(Apw12Cmd::ClearFaults.as_u8(), &[]),
        )?;
        Ok(())
    }

    /// GET_POWER_SN (RE2 0x10). Returns up to 32 bytes of PSU serial number.
    /// Diagnostic only.
    pub fn get_power_sn(&mut self) -> Result<Vec<u8>> {
        self.run_read(read_steps(Apw12Cmd::GetPowerSn.as_u8(), 32))
    }

    // -----------------------------------------------------------------------
    //  Internal: dispatch a write/read transaction through the I2C service
    // -----------------------------------------------------------------------

    /// Run a typed write-only transaction (returns nothing).
    fn run_with_intent(
        &self,
        intent: I2cOperationIntent,
        steps: Vec<I2cTransactionStep>,
    ) -> Result<()> {
        if intent == I2cOperationIntent::SafeOff && self.i2c.has_reserved_safe_off_lane() {
            if let [I2cTransactionStep::Write(data)] = steps.as_slice() {
                return self.i2c.write_bytes_with_intent(intent, self.address, data);
            }
        }
        let _ = self
            .i2c
            .transaction_with_intent(intent, self.address, steps)?;
        Ok(())
    }

    /// Run a transaction that contains exactly one read step and return
    /// the resulting bytes. Fails if zero reads are returned.
    fn run_read(&self, steps: Vec<I2cTransactionStep>) -> Result<Vec<u8>> {
        let mut reads =
            self.i2c
                .transaction_with_intent(I2cOperationIntent::ReadOnly, self.address, steps)?;
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
    /// [`Apw12SmbusAuthorized`]. If this fails to compile, the sealed-trait
    /// whitelist has lost a platform.
    fn _compile_assert_authorized<P: Apw12SmbusAuthorized>() {}

    #[test]
    fn whitelisted_platforms_are_authorized() {
        _compile_assert_authorized::<Cv1835S19jPro>();
        _compile_assert_authorized::<Am335xBbS19jPro>();
        _compile_assert_authorized::<AmlogicS19jPro>();
        _compile_assert_authorized::<Cv1835S19>();
        _compile_assert_authorized::<Cv1835S19i>();
        _compile_assert_authorized::<Cv1835S19XP>();
    }

    #[test]
    fn opcode_values_match_apw12_h() {
        assert_eq!(Apw12Cmd::PowerOff.as_u8(), 0x00);
        assert_eq!(Apw12Cmd::PowerOn.as_u8(), 0x01);
        assert_eq!(Apw12Cmd::SetVoltage.as_u8(), 0x02);
        assert_eq!(Apw12Cmd::ReadVoltage.as_u8(), 0x03);
        assert_eq!(Apw12Cmd::GetFwVersion.as_u8(), 0x04);
        assert_eq!(Apw12Cmd::ReadTelemetry.as_u8(), 0x05);
        assert_eq!(Apw12Cmd::EnableWdog.as_u8(), 0x06);
        assert_eq!(Apw12Cmd::DisableWdog.as_u8(), 0x07);
        assert_eq!(Apw12Cmd::GetHwVersion.as_u8(), 0x08);
        assert_eq!(Apw12Cmd::CalibStatus.as_u8(), 0x09);
        assert_eq!(Apw12Cmd::GetAcPower.as_u8(), 0x0A);
        assert_eq!(Apw12Cmd::ReadErrorCode.as_u8(), 0x0B);
        assert_eq!(Apw12Cmd::ReadErrorData.as_u8(), 0x0C);
        assert_eq!(Apw12Cmd::GetUpdateTime.as_u8(), 0x0D);
        assert_eq!(Apw12Cmd::GetResetCause.as_u8(), 0x0E);
        assert_eq!(Apw12Cmd::ClearFaults.as_u8(), 0x0F);
        assert_eq!(Apw12Cmd::GetPowerSn.as_u8(), 0x10);
    }

    #[test]
    fn constants_match_apw12_h() {
        assert_eq!(APW12_I2C_ADDR, 0x10);
        assert_eq!(VOLTAGE_MIN_MV, 1200);
        assert_eq!(VOLTAGE_MAX_MV, 1600);
        assert_eq!(TELEMETRY_BLOCK_LEN, 32);
        assert_eq!(WDOG_MIN_MS, 100);
        assert_eq!(WDOG_MAX_MS, 60_000);
        assert_eq!(GPIO_PSU_ENABLE, 412);
        assert_eq!(EXPECTED_FW_VER, 0x0103);
    }

    /// 1420 mV LE = bytes [0x8C, 0x05].
    #[test]
    fn set_voltage_encoding() {
        let steps = set_voltage_steps(1420);
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &[0x02, 0x8C, 0x05], "[opcode, lo, hi]");
            }
            other => panic!("expected Write, got {:?}", other),
        }
    }

    /// Min-bound (1200 mV) and max-bound (1600 mV) encode correctly.
    #[test]
    fn set_voltage_bounds_encode_correctly() {
        // 1200 = 0x04B0 → [B0, 04]
        let s = set_voltage_steps(1200);
        match &s[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x02, 0xB0, 0x04]),
            _ => unreachable!(),
        }
        // 1600 = 0x0640 → [40, 06]
        let s = set_voltage_steps(1600);
        match &s[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x02, 0x40, 0x06]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn enable_watchdog_encoding() {
        // 5000 ms = 0x1388 → [88, 13]
        let s = enable_watchdog_steps(5000);
        match &s[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x06, 0x88, 0x13]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn disable_watchdog_encoding() {
        let s = disable_watchdog_steps();
        match &s[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x07, 0x00, 0x00]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn power_on_off_encoding() {
        match &power_on_steps()[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x01]),
            _ => unreachable!(),
        }
        match &power_off_steps()[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x00]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn read_steps_use_write_read() {
        match &read_voltage_steps()[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &[0x03]);
                assert_eq!(*read_len, 2);
            }
            _ => unreachable!(),
        }
        match &get_fw_version_steps()[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &[0x04]);
                assert_eq!(*read_len, 2);
            }
            _ => unreachable!(),
        }
        match &read_telemetry_steps()[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &[0x05]);
                assert_eq!(*read_len, 32);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn telemetry_parses_layout() {
        let raw: [u8; 32] = [
            0x84, 0x05, // dc_volt = 0x0584 = 1412
            0x8C, 0x05, // set_volt = 0x058C = 1420
            0x10, 0x27, // current = 0x2710 = 10000 (10A)
            0xC8, 0x00, // power = 0x00C8 = 200W
            0x01, // status_bits = 0x01
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let t = Apw12Telemetry::parse(&raw);
        assert_eq!(t.dc_volt_mv, 1412);
        assert_eq!(t.set_volt_mv, 1420);
        assert_eq!(t.current_ma, 10000);
        assert_eq!(t.power_w, 200);
        assert_eq!(t.status_bits, 0x01);
    }

    #[test]
    fn telemetry_short_read_zero_fills() {
        let t = Apw12Telemetry::parse(&[0x84, 0x05, 0x8C]); // 3 bytes
        assert_eq!(t, Apw12Telemetry::default());
    }

    /// Voltage bounds gate: 1199 and 1601 must error before any I/O.
    #[test]
    fn voltage_bounds_rejected() {
        let (handle, _rx) = I2cServiceHandle::for_unit_tests();
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        let r = psu.set_voltage_mv(1199);
        assert!(r.is_err(), "1199 mV must be rejected");
        let r = psu.set_voltage_mv(1601);
        assert!(r.is_err(), "1601 mV must be rejected");
    }

    /// Watchdog bounds gate: 99 ms and 60001 ms must error before any I/O.
    #[test]
    fn wdog_bounds_rejected() {
        let (handle, _rx) = I2cServiceHandle::for_unit_tests();
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        let r = psu.enable_watchdog(99);
        assert!(r.is_err(), "99 ms must be rejected");
        let r = psu.enable_watchdog(60_001);
        assert!(r.is_err(), "60001 ms must be rejected");
    }

    // ----------------------- mock-worker test helpers -----------------------

    /// Drain `I2cRequest::Transaction` calls from the test channel into a
    /// `Vec<(addr, steps)>`. Spawns a worker that replies to each request
    /// with the next reply produced by `replies` (one entry per
    /// transaction). Used to verify call ordering without requiring a real
    /// I2C bus.
    fn spawn_mock_worker(
        rx: Receiver<I2cRequest>,
        replies: Vec<Result<Vec<Vec<u8>>>>,
    ) -> thread::JoinHandle<Vec<(u8, Vec<I2cTransactionStep>)>> {
        thread::spawn(move || {
            let mut log = Vec::new();
            let mut replies = replies.into_iter();
            while let Ok(req) = rx.recv() {
                match req {
                    I2cRequest::Transaction {
                        addr,
                        steps,
                        reply_tx,
                    } => {
                        log.push((addr, steps));
                        let r = replies.next().unwrap_or_else(|| Ok(Vec::new()));
                        let _ = reply_tx.send(r);
                    }
                    other => panic!("unexpected non-transaction request: {:?}", other),
                }
            }
            log
        })
    }

    fn spawn_conditional_worker(
        rx: Receiver<I2cRequest>,
        outcome: crate::i2c::I2cConditionalSafeOffOutcome,
    ) -> thread::JoinHandle<(
        Vec<I2cTransactionStep>,
        Vec<I2cTransactionStep>,
        Vec<I2cTransactionStep>,
    )> {
        thread::spawn(move || match rx.recv().expect("conditional plan request") {
            I2cRequest::ConditionalSafeOffPlan {
                prelude,
                primary,
                compensation,
                reply_tx,
                ..
            } => {
                let _ = reply_tx.send(Ok(outcome));
                (prelude, primary, compensation)
            }
            other => panic!("unexpected non-conditional request: {:?}", other),
        })
    }

    #[derive(Debug)]
    struct BootWorkerLog {
        transactions: Vec<Vec<I2cTransactionStep>>,
        rollback_plans: usize,
    }

    /// Script normal boot transactions and one worker-owned rollback plan on
    /// the same raw service seam. A second plan is a test failure: dropping
    /// its reply sender also prevents a duplicate caller from hanging.
    fn spawn_boot_rollback_worker(
        rx: Receiver<I2cRequest>,
        replies: Vec<Result<Vec<Vec<u8>>>>,
        rollback_reply: Result<crate::i2c::I2cConditionalSafeOffOutcome>,
    ) -> thread::JoinHandle<BootWorkerLog> {
        thread::spawn(move || {
            let mut transactions = Vec::new();
            let mut replies = replies.into_iter();
            let mut rollback_reply = Some(rollback_reply);
            let mut rollback_plans = 0;
            while let Ok(request) = rx.recv() {
                match request {
                    I2cRequest::Transaction {
                        steps, reply_tx, ..
                    } => {
                        transactions.push(steps);
                        let reply = replies
                            .next()
                            .unwrap_or_else(|| panic!("unexpected boot transaction"));
                        let _ = reply_tx.send(reply);
                    }
                    I2cRequest::ConditionalSafeOffPlan { reply_tx, .. } => {
                        rollback_plans += 1;
                        let Some(reply) = rollback_reply.take() else {
                            drop(reply_tx);
                            panic!("duplicate rollback plan");
                        };
                        let _ = reply_tx.send(reply);
                    }
                    other => panic!("unexpected boot request: {other:?}"),
                }
            }
            BootWorkerLog {
                transactions,
                rollback_plans,
            }
        })
    }

    fn phase_completed() -> crate::i2c::I2cSafeOffPhaseOutcome {
        crate::i2c::I2cSafeOffPhaseOutcome::Completed
    }

    fn phase_not_attempted() -> crate::i2c::I2cSafeOffPhaseOutcome {
        crate::i2c::I2cSafeOffPhaseOutcome::NotAttempted
    }

    fn phase_failed(detail: &str) -> crate::i2c::I2cSafeOffPhaseOutcome {
        crate::i2c::I2cSafeOffPhaseOutcome::Failed(detail.into())
    }

    fn successful_rollback_outcome() -> crate::i2c::I2cConditionalSafeOffOutcome {
        crate::i2c::I2cConditionalSafeOffOutcome {
            prelude: phase_completed(),
            primary: phase_completed(),
            compensation: phase_not_attempted(),
            prelude_retry: phase_not_attempted(),
        }
    }

    #[test]
    fn output_command_state_starts_unknown() {
        let (handle, _rx) = I2cServiceHandle::for_unit_tests();
        let psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);
        assert_eq!(psu.output_state(), PsuOutputState::Unknown);
    }

    #[test]
    fn failed_power_on_cannot_leave_a_false_off_cache() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_mock_worker(
            rx,
            vec![Err(HalError::I2c {
                bus: 0,
                addr: APW12_I2C_ADDR,
                detail: "injected unobserved POWER_ON outcome".into(),
            })],
        );
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        assert!(psu.power_on().is_err());
        assert_eq!(psu.output_state(), PsuOutputState::Unknown);

        drop(psu);
        assert_eq!(worker.join().unwrap().len(), 1);
    }

    #[test]
    fn cold_boot_prevalidates_voltage_and_watchdog_before_power_on() {
        for (target_mv, watchdog_ms) in [(VOLTAGE_MIN_MV - 1, WDOG_MIN_MS), (1420, 0)] {
            let (handle, rx) = I2cServiceHandle::for_unit_tests();
            let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);
            assert!(psu
                .cold_boot_sequence_5_step(target_mv, watchdog_ms)
                .is_err());
            assert!(
                rx.try_recv().is_err(),
                "invalid boot parameters must issue no POWER_ON or rollback I/O"
            );
        }
    }

    #[test]
    fn cold_boot_preserves_primary_fw_failure_and_completed_rollback() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_boot_rollback_worker(
            rx,
            vec![
                Ok(Vec::new()),
                Ok(vec![vec![0x02, 0x01]]), // unsupported FW 0x0102
            ],
            Ok(successful_rollback_outcome()),
        );
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        let error = psu.cold_boot_sequence_5_step(1420, 100).unwrap_err();
        let source = std::error::Error::source(&error)
            .expect("PartialBootRollback must expose its initiating failure");
        assert!(source.to_string().contains("PSU unsupported"));
        match error {
            HalError::PartialBootRollback {
                context,
                primary,
                rollback: crate::PowerRollbackOutcome::Completed,
            } => {
                assert_eq!(context, "APW12 five-step cold boot");
                assert!(matches!(*primary, HalError::PsuUnsupported(_)));
            }
            other => panic!("unexpected structured boot error: {other:?}"),
        }
        assert_eq!(psu.output_state(), PsuOutputState::Off);

        drop(psu);
        let log = worker.join().unwrap();
        assert_eq!(log.rollback_plans, 1);
        assert_eq!(log.transactions.len(), 2);
    }

    #[test]
    fn cold_boot_rollback_outcome_unknown_is_structured_and_not_duplicated() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_boot_rollback_worker(
            rx,
            vec![Err(HalError::I2c {
                bus: 0,
                addr: APW12_I2C_ADDR,
                detail: "injected POWER_ON outcome unknown".into(),
            })],
            Err(HalError::I2cSafeOffOutcomeUnknown {
                bus: 0,
                addr: APW12_I2C_ADDR,
                detail: "accepted rollback receipt timed out".into(),
            }),
        );
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        let error = psu.cold_boot_sequence_5_step(1420, 100).unwrap_err();
        match error {
            HalError::PartialBootRollback {
                primary,
                rollback: crate::PowerRollbackOutcome::Failed(rollback),
                ..
            } => {
                assert!(matches!(*primary, HalError::I2c { .. }));
                assert!(matches!(
                    *rollback,
                    HalError::I2cSafeOffOutcomeUnknown { .. }
                ));
            }
            other => panic!("unexpected structured boot error: {other:?}"),
        }
        assert_eq!(psu.output_state(), PsuOutputState::Unknown);

        drop(psu);
        let log = worker.join().unwrap();
        assert_eq!(log.rollback_plans, 1);
        assert_eq!(log.transactions.len(), 1);
    }

    #[test]
    #[cfg(panic = "unwind")]
    fn unwind_profile_fallback_submits_exactly_one_rollback() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_boot_rollback_worker(rx, Vec::new(), Ok(successful_rollback_outcome()));
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> = run_with_power_rollback(&mut psu, "panic test", |_psu| {
                panic!("injected boot panic")
            });
        }));
        assert!(panic.is_err());

        drop(psu);
        let log = worker.join().unwrap();
        assert_eq!(log.rollback_plans, 1);
        assert!(log.transactions.is_empty());
    }

    #[test]
    fn post_power_scope_preserves_platform_failure_structurally() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_boot_rollback_worker(rx, Vec::new(), Ok(successful_rollback_outcome()));
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        let error = run_with_power_rollback(&mut psu, "injected platform phase", |_psu| {
            Err::<(), _>(HalError::Gpio("injected reset failure".into()))
        })
        .unwrap_err();
        match error {
            HalError::PartialBootRollback {
                context,
                primary,
                rollback: crate::PowerRollbackOutcome::Completed,
            } => {
                assert_eq!(context, "injected platform phase");
                assert!(matches!(*primary, HalError::Gpio(_)));
            }
            other => panic!("unexpected structured platform error: {other:?}"),
        }

        drop(psu);
        let log = worker.join().unwrap();
        assert_eq!(log.rollback_plans, 1);
        assert!(log.transactions.is_empty());
    }

    #[test]
    fn successful_post_power_scope_commits_without_rollback() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_boot_rollback_worker(rx, Vec::new(), Ok(successful_rollback_outcome()));
        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);

        run_with_power_rollback(&mut psu, "successful platform phase", |_psu| Ok(())).unwrap();

        drop(psu);
        let log = worker.join().unwrap();
        assert_eq!(log.rollback_plans, 0);
        assert!(log.transactions.is_empty());
    }

    /// power_off_with_disarm sends DISABLE_WDOG (0x07) BEFORE POWER_OFF (0x00).
    #[test]
    fn power_off_disarms_watchdog_first() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        // Need an unbounded staging channel — the for_unit_tests channel is
        // sync_channel(1), so rotate by spawning the worker first.
        let worker = spawn_conditional_worker(
            rx,
            crate::i2c::I2cConditionalSafeOffOutcome {
                prelude: phase_completed(),
                primary: phase_completed(),
                compensation: phase_not_attempted(),
                prelude_retry: phase_not_attempted(),
            },
        );

        let mut psu = Apw12SmbusBackend::new(handle, Am335xBbS19jPro);
        psu.power_off_with_disarm()
            .expect("power_off_with_disarm should succeed");

        // Drop the handle (and the psu, which holds a clone) so the worker exits.
        drop(psu);
        let log = worker.join().expect("worker thread");

        let (prelude, primary, compensation) = log;
        // First call: DISABLE_WDOG (opcode 0x07, payload 0x00 0x00)
        match &prelude[0] {
            I2cTransactionStep::Write(b) => {
                assert_eq!(b[0], 0x07, "first opcode must be DISABLE_WDOG (0x07)");
            }
            _ => panic!("expected Write step"),
        }
        // Second call: POWER_OFF (opcode 0x00, no payload)
        match &primary[0] {
            I2cTransactionStep::Write(b) => {
                assert_eq!(b, &[0x00], "second opcode must be POWER_OFF (0x00)");
            }
            _ => panic!("expected Write step"),
        }
        assert_eq!(
            compensation[0],
            I2cTransactionStep::Write(vec![0x06, 0x64, 0x00])
        );
    }

    #[test]
    fn power_off_success_does_not_hide_watchdog_disarm_failure() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_conditional_worker(
            rx,
            crate::i2c::I2cConditionalSafeOffOutcome {
                prelude: phase_failed("injected disarm failure"),
                primary: phase_completed(),
                compensation: phase_not_attempted(),
                prelude_retry: phase_failed("injected retry failure"),
            },
        );
        let mut psu = Apw12SmbusBackend::new(handle, Am335xBbS19jPro);
        psu.output_state = PsuOutputState::On;

        let rendered = psu.power_off_with_disarm().unwrap_err().to_string();
        assert!(rendered.contains("POWER_OFF completed"), "{rendered}");
        assert_eq!(psu.output_state(), PsuOutputState::Off);
        drop(psu);
        let _ = worker.join().unwrap();
    }

    #[test]
    fn power_off_failure_after_disarm_attempts_watchdog_compensation() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_conditional_worker(
            rx,
            crate::i2c::I2cConditionalSafeOffOutcome {
                prelude: phase_completed(),
                primary: phase_failed("injected power-off failure"),
                compensation: phase_completed(),
                prelude_retry: phase_not_attempted(),
            },
        );
        let mut psu = Apw12SmbusBackend::new(handle, Am335xBbS19jPro);
        psu.output_state = PsuOutputState::On;
        psu.watchdog_armed = true;
        psu.watchdog_timeout_ms = Some(5_000);

        let rendered = psu.power_off_with_disarm().unwrap_err().to_string();
        assert!(
            rendered.contains("compensating watchdog arm=Completed"),
            "{rendered}"
        );
        assert!(
            matches!(psu.output_state(), PsuOutputState::Unknown),
            "failed POWER_OFF must invalidate the command-history cache"
        );
        assert!(psu.watchdog_armed);
        drop(psu);

        let (_, _, compensation) = worker.join().unwrap();
        match &compensation[0] {
            I2cTransactionStep::Write(bytes) => {
                assert_eq!(bytes[0], 0x06, "compensation must re-arm watchdog");
            }
            _ => panic!("expected compensation Write step"),
        }
    }

    /// get_fw_version with a mismatched response (0x0102) returns
    /// `HalError::PsuUnsupported`.
    #[test]
    fn expected_fw_version_mismatch_errors() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let _worker = spawn_mock_worker(
            rx,
            vec![Ok(vec![vec![0x02, 0x01]])], // 0x0102 LE
        );

        let mut psu = Apw12SmbusBackend::new(handle, AmlogicS19jPro);
        let r = psu.get_fw_version();
        assert!(r.is_err(), "fw 0x0102 must mismatch expected 0x0103");
        match r {
            Err(HalError::PsuUnsupported(_)) => {}
            other => panic!("expected PsuUnsupported, got {:?}", other),
        }
        // Cached fw_version is still updated by get_fw_version_unchecked
        // (the inner call), so it should reflect what we observed.
        assert_eq!(psu.fw_version(), 0x0102);
    }

    /// READ_TELEMETRY short response (≤8 bytes) returns Ok(None).
    #[test]
    fn telemetry_unavailable_returns_ok_none() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let _worker = spawn_mock_worker(
            rx,
            vec![Ok(vec![vec![0x84, 0x05, 0x8C]])], // 3 bytes < 9
        );

        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);
        let r = psu.read_telemetry().expect("transport must succeed");
        assert!(r.is_none(), "<9 byte response must yield Ok(None)");
    }

    /// cold_boot_sequence_5_step orders POWER_ON → GET_FW → CALIB poll →
    /// SET_VOLTAGE → READ_TELEMETRY → ENABLE_WDOG with the right opcodes.
    /// Wdog timeout=100ms is the boundary — minimum legal value.
    #[test]
    fn cold_boot_sequence_5_step_orders_correctly() {
        let (handle, rx) = I2cServiceHandle::for_unit_tests();
        let worker = spawn_mock_worker(
            rx,
            vec![
                Ok(Vec::new()),                        // 1) POWER_ON
                Ok(vec![vec![0x03, 0x01]]),            // 2) GET_FW_VERSION → 0x0103
                Ok(vec![vec![0x01]]),                  // 3) CALIB_STATUS → ready
                Ok(Vec::new()),                        // 4) SET_VOLTAGE
                Ok(vec![telemetry_block(1412, 1420)]), // 5) READ_TELEMETRY → DC=1412
                Ok(Vec::new()),                        // 6) ENABLE_WDOG
            ],
        );

        let mut psu = Apw12SmbusBackend::new(handle, Cv1835S19jPro);
        psu.cold_boot_sequence_5_step(1420, 100)
            .expect("cold boot should succeed");

        drop(psu);
        let log = worker.join().expect("worker thread");

        assert_eq!(log.len(), 6, "exactly six transactions");
        let opcodes: Vec<u8> = log
            .iter()
            .map(|(_, steps)| match &steps[0] {
                I2cTransactionStep::Write(b) => b[0],
                I2cTransactionStep::WriteRead { write_data, .. } => write_data[0],
                _ => 0xFF,
            })
            .collect();
        assert_eq!(
            opcodes,
            vec![0x01, 0x04, 0x09, 0x02, 0x05, 0x06],
            "ordering: POWER_ON, GET_FW, CALIB_STATUS, SET_VOLTAGE, READ_TELEMETRY, ENABLE_WDOG",
        );

        // Confirm the SET_VOLTAGE payload is 1420 mV LE.
        match &log[3].1[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x02, 0x8C, 0x05]),
            _ => unreachable!(),
        }
        // Confirm ENABLE_WDOG payload is 100 ms LE = [0x64, 0x00].
        match &log[5].1[0] {
            I2cTransactionStep::Write(b) => assert_eq!(b, &[0x06, 0x64, 0x00]),
            _ => unreachable!(),
        }
    }

    fn telemetry_block(dc_mv: u16, set_mv: u16) -> Vec<u8> {
        let mut v = vec![
            (dc_mv & 0xFF) as u8,
            (dc_mv >> 8) as u8,
            (set_mv & 0xFF) as u8,
            (set_mv >> 8) as u8,
            0x00,
            0x00, // current
            0x00,
            0x00, // power
            0x00, // status
        ];
        v.resize(TELEMETRY_BLOCK_LEN, 0);
        v
    }
}
