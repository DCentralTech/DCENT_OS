//! `Pic1704Service` — service-thread-backed PIC1704 controller.
//!
//! Mirrors the shape of `crate::dspic::Pic0x89Service` (S19j Pro am2 dsPIC):
//! a thin struct holding an `I2cServiceHandle`, an address, cached state,
//! and a heartbeat throttle. All I/O is serialized through the process-wide
//! I2C service thread per the AM2 SINGLE-I2C-OWNER architecture rule —
//! callers MUST NOT bypass the service with raw `I2cBus::open(0)`.
//!
//! # Construction is sealed-trait-gated
//!
//! `Pic1704Service::new` requires a `_marker: P` argument where
//! `P: Pic1704Authorized`. The trait is sealed (cannot be implemented
//! outside this crate) and is only implemented for the platform marker
//! types whitelisted in [`platforms`]. This is the platform-isolation
//! guarantee from the W2 RE deliverable spec.
//!
//! The marker types live in this crate (NOT `dcentrald-hal`) because
//! `dcentrald-asic` depends on `dcentrald-hal` — moving the markers to
//! the hal side would form a dep cycle. A5's wave-2A.2 closure pinned
//! this decision; future agents must not "fix" the apparent placeholder
//! TODO by relocating them..
//!
//! # RESET is research/test-only
//!
//! The `reset()` method only exists when the `recovery-tool` Cargo feature
//! is enabled. No shipped package enables that feature, so neither `dcentrald`
//! nor the diagnostic-only `pic-recovery` package can link it. See
//! . A future reset executor requires separate,
//! typed controller-recovery authority; this method's feature gate alone is
//! not sufficient authorization.

use std::time::{Duration, Instant};

use dcentrald_hal::i2c::{I2cMutationLabel, I2cServiceHandle};
use dcentrald_hal::platform::{VoltageControllerEndpoint, VoltageControllerKind};
use tracing::{debug, info, warn};

use crate::AsicError;
use crate::Result;

use super::protocol::{
    classify_version, decode_le_word, enable_dc_dc_steps, heartbeat_steps, is_application_version,
    read_register_steps, start_app_steps, Pic1704State, HEARTBEAT_INTERVAL_MS, POLL_INTERVAL_MS,
    REG_CURRENT_L, REG_STATUS, REG_TEMP, REG_TEMP_ALT, REG_VERSION, REG_VOLTAGE_L,
};

// ===========================================================================
//  Sealed trait — platform whitelist for `Pic1704Service::new`
// ===========================================================================

/// Sealed-trait sub-module — see Rust API guidelines C-SEALED.
mod sealed {
    /// Sealed marker. Cannot be implemented outside this crate, which
    /// pins `Pic1704Authorized` impls to the whitelisted set in
    /// `super::platforms`.
    pub trait Sealed {}
}

/// Platform marker types allowed to construct a `Pic1704Service`.
///
/// Implementors are constrained to the placeholder types in [`platforms`]
/// pending wave A5 (where they will be replaced with real
/// `dcentrald_hal::platform::*` markers). The trait is sealed via
/// `sealed::Sealed`, so no downstream / out-of-crate code can sneak a
/// rogue platform in.
pub trait Pic1704Authorized: sealed::Sealed {}

/// Marker types for the platforms that own PIC1704 hashboards.
///
/// W2A.2 closure (2026-05-09): These markers stay in `dcentrald-asic`
/// rather than being re-exported from `dcentrald-hal`, because
/// `dcentrald-asic` depends ON `dcentrald-hal` — re-exporting from hal
/// would create a circular dependency. Instead, the platform layer
/// constructs the marker locally at the call site:
///
/// ```ignore
/// use dcentrald_asic::pic1704::service::platforms::Cv1835S19jPro;
/// use dcentrald_asic::pic1704::Pic1704Service;
///
/// let service = Pic1704Service::new(i2c_handle, 0x20, Cv1835S19jPro);
/// ```
///
/// The hal-side `VoltageControllerKind::Pic1704` classification (from
/// `dcentrald_hal::platform::subtype::classify_with_probe`) is the
/// runtime gate; these zero-sized markers are the compile-time gate.
/// Together they form the "subtype + ACK + sealed-trait" defense in
/// depth that prevents a stray PIC1704 instantiation on a dsPIC unit.
pub mod platforms {
    /// S19j Pro CV1835 (Sophgo CV1835 SoC). Subtype = `CVCtrl_BHB42XXX`.
    pub struct Cv1835S19jPro;
    /// AM335x BB (BeagleBone-class control board, S19j Pro variant).
    /// Subtype = `BBCtrl_BHB42XXX`.
    pub struct Am335xBbS19jPro;
    /// Amlogic S19j Pro variants. Subtype = `AMLCtrl_BHB42XXX`.
    /// **NOT** S19k Pro (`AMLCtrl_BHB56xxx`) or S21 NoPic — those stay
    /// on the existing dsPIC / NoPic paths.
    pub struct AmlogicS19jPro;
    /// Antminer S19 (Standard) on Cvitek CV183x. PIC1704 at I²C 0x20 per
    /// chain. Subtype mirrors the S19j Pro pattern (`CVCtrl_BHB42XXX`)
    /// because RE2's hardware catalog confirms all CVCtrl-family hashboards
    /// share the BHB42XXX pattern at the `/etc/subtype` level.
    /// Source: `DCENT_OS_HARDWARE_CATALOG.md` §2.5 (Antminer S19) +
    /// §6.1 (PIC MCU comparison row — PIC1704 confirmed for S19).
    pub struct Cv1835S19;
    /// Antminer S19i on Cvitek CV183x. PIC1704 at I²C 0x20 per chain.
    /// Same `CVCtrl_BHB42XXX` subtype family.
    /// Source: `DCENT_OS_HARDWARE_CATALOG.md` §2.5 (Antminer S19i) +
    /// §6.1 PIC MCU row — PIC1704 confirmed for S19i.
    pub struct Cv1835S19i;
    /// Antminer S19 XP (Hydra) on Cvitek CV183x. PIC1704 at I²C 0x20 per
    /// chain. Same `CVCtrl_BHB42XXX` subtype family. Note the spelling
    /// `S19XP` (no underscore between "S19" and "XP") matches Bitmain's
    /// internal sku notation.
    /// Source: `DCENT_OS_HARDWARE_CATALOG.md` §2.5 (Antminer S19 XP) +
    /// §6.1 PIC MCU row — PIC1704 confirmed for S19 XP.
    /// **NOT** the Amlogic S905 hydro variant of S19 XP — that hardware
    /// path is not yet covered by a marker; add a separate
    /// `AmlogicS19XP` marker when (a) RE2 ground-truths the subtype
    /// string AND (b) live hardware is acquired.
    pub struct Cv1835S19XP;
    /// Antminer T19 on Cvitek CV183x. PIC1704 at I²C 0x20 per chain.
    /// Same `CVCtrl_BHB42XXX` subtype family + APW12 SMBus PSU as the
    /// rest of the BM1362-family CV183x line. T19 is the lower-bin
    /// BM1368 (~84 TH/s, ~3150W) variant of the S19 generation.
    /// Source: RE3 `DCENT_OS_HARDWARE_CATALOG.md` §2.5 (Antminer T19 —
    /// "Control board: Cvitek CV183x", "PSU: APW12") + §5.1 (PSU table
    /// confirms APW12 used in "S19j Pro, S19, T19") + §6.1 (PIC1704
    /// register map identical across the entire BM1362-family line).
    /// **NOT** AM335x BB or Amlogic — RE3 §2.5 lists only Cvitek CV183x
    /// as the T19 carrier. If a future T19 SKU is RE-confirmed to ship
    /// on BB or AML carrier (with subtype evidence + live i2cdetect 0x20
    /// ACK), add `Am335xBbT19` / `AmlogicT19` markers in a follow-up
    /// wave; do not back-door them here.
    pub struct Cv1835T19;
}

impl sealed::Sealed for platforms::Cv1835S19jPro {}
impl Pic1704Authorized for platforms::Cv1835S19jPro {}

impl sealed::Sealed for platforms::Am335xBbS19jPro {}
impl Pic1704Authorized for platforms::Am335xBbS19jPro {}

impl sealed::Sealed for platforms::AmlogicS19jPro {}
impl Pic1704Authorized for platforms::AmlogicS19jPro {}

// W11.3 expansion (2026-05-09): three additional CV183x SKUs share the
// PIC1704 register map per RE2 §6.1. They all live behind the same
// `CVCtrl_BHB42XXX` subtype + 0x20 ACK probe gate as the existing
// CV1835 S19j Pro entry — no platform-specific construction-time
// behavior changes; only the compile-time whitelist grows.
impl sealed::Sealed for platforms::Cv1835S19 {}
impl Pic1704Authorized for platforms::Cv1835S19 {}

impl sealed::Sealed for platforms::Cv1835S19i {}
impl Pic1704Authorized for platforms::Cv1835S19i {}

impl sealed::Sealed for platforms::Cv1835S19XP {}
impl Pic1704Authorized for platforms::Cv1835S19XP {}

// W12 expansion (RE3 §2.5 + §5.1): T19 carrier addition.
// RE3 lists ONLY Cvitek CV183x for T19 — no AM335x BB or Amlogic
// variants are documented. Adding only `Cv1835T19`.
impl sealed::Sealed for platforms::Cv1835T19 {}
impl Pic1704Authorized for platforms::Cv1835T19 {}

// NOTE: NO `S21*` PIC1704 marker exists by design.
//  root corruption-prevention guarantee #2 + memory rule
//  lock S21 Amlogic to NoPic
// (TAS5782M kernel-managed DAC). RE2 §2.6 lists S21 with PIC1704 at
// I²C 0x20, but the live S21 unit at .135 has been proven NoPic and the
// 0x55-0x57 EEPROM denylist is intentionally NOT registered on S21 for
// that reason. Adding an S21 PIC1704 marker would create a footgun:
// dcentrald could attempt to talk PIC1704 register protocol to a
// platform that does not implement it. If a future S21 SKU is RE-confirmed
// to ship a real PIC1704 (with subtype evidence + live i2cdetect 0x20
// ACK on a non-`a lab unit` unit), add the marker in a follow-up wave with
// fresh `feedback_*` rules; do not back-door it here.

// ===========================================================================
//  Pic1704Service
// ===========================================================================

/// Service-thread-backed PIC1704 voltage / status controller.
///
/// Construction is gated by `Pic1704Authorized`; runtime safety is enforced
/// by routing every I2C operation through the shared `I2cServiceHandle`
/// rather than opening `/dev/i2c-N` directly.
pub struct Pic1704Service {
    i2c: I2cServiceHandle,
    address: u8,
    state: Pic1704State,
    fw_version: u8,
    last_heartbeat: Option<Instant>,
}

impl Pic1704Service {
    /// Construct from a discovery-issued, bus-bound PIC1704 endpoint.
    ///
    /// This is the preferred constructor for production orchestration. The
    /// opaque endpoint replaces caller-selected platform markers and raw
    /// addresses with exact system identity plus presence evidence.
    pub fn from_endpoint(
        handle: I2cServiceHandle,
        endpoint: VoltageControllerEndpoint,
    ) -> Result<Self> {
        if endpoint.kind() != VoltageControllerKind::Pic1704 {
            return Err(AsicError::InvalidParameter(format!(
                "{} endpoint cannot construct a PIC1704 service",
                endpoint.kind().as_str()
            )));
        }
        if endpoint.bus() != handle.bus() {
            return Err(AsicError::InvalidParameter(format!(
                "PIC1704 endpoint is bound to I2C bus {}, but service owns bus {}",
                endpoint.bus(),
                handle.bus()
            )));
        }
        Ok(Self::from_parts(handle, endpoint.address()))
    }

    /// Construct a `Pic1704Service` for one of the whitelisted platforms.
    ///
    /// The `_marker: P` argument is consumed only at compile time: it
    /// proves the caller is an authorized platform via the sealed
    /// `Pic1704Authorized` trait. There is no runtime cost.
    pub fn new<P: Pic1704Authorized>(handle: I2cServiceHandle, address: u8, _marker: P) -> Self {
        Self::from_parts(handle, address)
    }

    fn from_parts(handle: I2cServiceHandle, address: u8) -> Self {
        Self {
            i2c: handle,
            address,
            state: Pic1704State::Unknown,
            fw_version: 0,
            last_heartbeat: None,
        }
    }

    /// Current cached I2C address (typically `PIC1704_I2C_ADDR` = 0x20).
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Last classified runtime state (Bootloader / Application / Unknown / Error).
    pub fn state(&self) -> Pic1704State {
        self.state
    }

    /// Last observed firmware byte from `REG_VERSION`. `0` until first read.
    pub fn fw_version(&self) -> u8 {
        self.fw_version
    }

    // -----------------------------------------------------------------------
    //  Read-only operations
    // -----------------------------------------------------------------------

    /// Read `REG_VERSION` and update cached state.
    ///
    /// Equivalent to `pic1704.c::pic1704_read_version` plus the version
    /// classification block from `pic1704.c::pic1704_open` (lines 130-136).
    pub fn read_version(&mut self) -> Result<u8> {
        let buf = self.read_register(REG_VERSION, 1)?;
        let ver = buf[0];
        self.fw_version = ver;
        self.state = classify_version(ver);
        debug!(
            addr = format_args!("0x{:02X}", self.address),
            version = format_args!("0x{:02X}", ver),
            state = ?self.state,
            "PIC1704 version read",
        );
        Ok(ver)
    }

    /// Poll `REG_VERSION` until an application revision (0x88/0x89/0x8A)
    /// appears or `timeout` elapses.
    ///
    /// Mirrors `pic1704.c::pic1704_wait_for_app` (poll every 100 ms).
    pub fn wait_for_app(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let poll = Duration::from_millis(POLL_INTERVAL_MS);
        loop {
            match self.read_version() {
                Ok(v) if is_application_version(v) => {
                    info!(
                        addr = format_args!("0x{:02X}", self.address),
                        version = format_args!("0x{:02X}", v),
                        "PIC1704 entered application firmware",
                    );
                    return Ok(());
                }
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                warn!(
                    addr = format_args!("0x{:02X}", self.address),
                    timeout_ms = timeout.as_millis() as u64,
                    "PIC1704 wait_for_app timed out",
                );
                return Err(AsicError::Pic {
                    addr: self.address,
                    detail: format!("wait_for_app timed out after {} ms", timeout.as_millis(),),
                });
            }
            std::thread::sleep(poll);
        }
    }

    /// Read `REG_VOLTAGE` (LE u16, mV).
    pub fn read_voltage_mv(&mut self) -> Result<u16> {
        let buf = self.read_register(REG_VOLTAGE_L, 2)?;
        decode_le_word(&buf).ok_or_else(|| AsicError::Pic {
            addr: self.address,
            detail: format!("voltage read returned {} bytes (need 2)", buf.len()),
        })
    }

    /// Read `REG_CURRENT` (LE u16, mA).
    pub fn read_current_ma(&mut self) -> Result<u16> {
        let buf = self.read_register(REG_CURRENT_L, 2)?;
        decode_le_word(&buf).ok_or_else(|| AsicError::Pic {
            addr: self.address,
            detail: format!("current read returned {} bytes (need 2)", buf.len()),
        })
    }

    /// Read temperature in tenths of degrees Celsius. Tries `REG_TEMP`,
    /// falls back to `REG_TEMP_ALT` if the primary register returns the
    /// `0x00` / `0xFF` "no reading" sentinels.
    ///
    /// Mirrors `pic1704.c::pic1704_read_temp_tenthc`.
    pub fn read_temp_tenthc(&mut self) -> Result<i16> {
        let primary = self.read_register(REG_TEMP, 1)?;
        let raw = primary[0];
        let final_raw = if raw == 0x00 || raw == 0xFF {
            let alt = self.read_register(REG_TEMP_ALT, 1)?;
            alt[0]
        } else {
            raw
        };
        Ok(i16::from(final_raw as i8))
    }

    /// Read `REG_STATUS` byte (DC_DC_ON / APP_RUNNING / FAULT / OTP).
    pub fn read_status(&mut self) -> Result<u8> {
        let buf = self.read_register(REG_STATUS, 1)?;
        Ok(buf[0])
    }

    // -----------------------------------------------------------------------
    //  Write operations
    // -----------------------------------------------------------------------

    /// Trigger bootloader → application transition.
    ///
    /// Writes `0x5A` to `REG_VERSION` then `0x01` to `REG_CONTROL` in one
    /// service transaction so no other I2C client can interleave between
    /// the two writes. After this returns, callers should poll with
    /// `wait_for_app` (the PIC takes ~100 ms to reset and re-enumerate).
    ///
    /// If the chip is already in application mode, this is a no-op and
    /// returns `Ok(())` (matches `pic1704.c` lines 207-209).
    pub fn start_app(&mut self) -> Result<()> {
        if matches!(self.state, Pic1704State::Application) {
            debug!(
                addr = format_args!("0x{:02X}", self.address),
                "PIC1704 start_app: already in application mode (no-op)",
            );
            return Ok(());
        }
        if !matches!(self.state, Pic1704State::Bootloader | Pic1704State::Unknown) {
            return Err(AsicError::Pic {
                addr: self.address,
                detail: format!("start_app: invalid state {:?}", self.state),
            });
        }

        info!(
            addr = format_args!("0x{:02X}", self.address),
            "PIC1704 start_app: writing 0x5A→VERSION, 0x01→CONTROL",
        );
        self.i2c
            .transaction_mutating(I2cMutationLabel::Recovery, self.address, start_app_steps())
            .map_err(|e| AsicError::Pic {
                addr: self.address,
                detail: format!("start_app transaction: {}", e),
            })?;

        // The PIC resets and comes back as one of the application revs.
        self.state = Pic1704State::Unknown;
        Ok(())
    }

    /// Enable or disable the DC-DC converter.
    ///
    /// Per `pic1704.c::pic1704_enable_dc_dc`, this is only effective in
    /// application mode — but unlike that C code, we don't gate it here:
    /// the daemon's higher-level boot orchestrator must call
    /// `wait_for_app` first. We keep this method permissive so that
    /// recovery / lab tooling can attempt it from any state.
    pub fn enable_dc_dc(&mut self, enable: bool) -> Result<()> {
        info!(
            addr = format_args!("0x{:02X}", self.address),
            enable, "PIC1704 enable_dc_dc",
        );
        let result = if enable {
            self.i2c.transaction_mutating(
                I2cMutationLabel::Energize,
                self.address,
                enable_dc_dc_steps(true),
            )
        } else {
            self.i2c
                .disable_pic1704_dc_dc(self.address)
                .map(|_| Vec::new())
        };
        result.map_err(|e| AsicError::Pic {
            addr: self.address,
            detail: format!("enable_dc_dc transaction: {}", e),
        })?;
        Ok(())
    }

    /// Send a heartbeat tick. Rate-limited to one write per
    /// `HEARTBEAT_INTERVAL_MS` (2 s) — extra calls return `Ok(())` without
    /// touching the bus, matching `pic1704.c::pic1704_heartbeat`.
    pub fn heartbeat(&mut self) -> Result<()> {
        let now = Instant::now();
        if let Some(last) = self.last_heartbeat {
            if now.duration_since(last) < Duration::from_millis(HEARTBEAT_INTERVAL_MS) {
                // Rate-limited: caller probably called us in a tight loop.
                return Ok(());
            }
        }
        self.last_heartbeat = Some(now);
        self.i2c
            .transaction_mutating(I2cMutationLabel::KeepAlive, self.address, heartbeat_steps())
            .map_err(|e| AsicError::Pic {
                addr: self.address,
                detail: format!("heartbeat transaction: {}", e),
            })?;
        Ok(())
    }

    /// Internal accessor for the I²C service handle, gated to the
    /// research/test-only `recovery-tool` feature. No shipped package enables
    /// it. Used by [`super::programmer`] host tests and protocol research.
    #[cfg(feature = "recovery-tool")]
    pub(super) fn i2c_handle(&self) -> &I2cServiceHandle {
        &self.i2c
    }

    /// **Destructive** hardware RESET retained for protocol research. It only
    /// exists behind `recovery-tool`, which no shipped package enables.
    ///
    /// Even though PIC1704 is a different chip family than the S19j Pro
    /// dsPIC that originally motivated the no-RESET rule, the
    /// safety rule is the same: no runtime consumer may call this without a
    /// separate controller-recovery authority architecture. See
    /// .
    #[cfg(feature = "recovery-tool")]
    pub fn reset(&mut self) -> Result<()> {
        warn!(
            addr = format_args!("0x{:02X}", self.address),
            "PIC1704 RESET (recovery-tool only) — DESTRUCTIVE",
        );
        self.i2c
            .transaction_mutating(
                I2cMutationLabel::Recovery,
                self.address,
                super::protocol::write_register_steps(
                    super::protocol::REG_CONTROL,
                    super::protocol::CTRL_RESET,
                ),
            )
            .map_err(|e| AsicError::Pic {
                addr: self.address,
                detail: format!("reset transaction: {}", e),
            })?;
        self.state = Pic1704State::Unknown;
        self.fw_version = 0;
        Ok(())
    }

    // -----------------------------------------------------------------------
    //  Internal helpers
    // -----------------------------------------------------------------------

    fn read_register(&self, reg: u8, len: usize) -> Result<Vec<u8>> {
        let mut reads = self
            .i2c
            .transaction_mutating(
                I2cMutationLabel::QueryPrelude,
                self.address,
                read_register_steps(reg, len),
            )
            .map_err(|e| AsicError::Pic {
                addr: self.address,
                detail: format!("read_register 0x{:02X}: {}", reg, e),
            })?;
        reads.pop().ok_or_else(|| AsicError::Pic {
            addr: self.address,
            detail: format!("read_register 0x{:02X}: no read result", reg),
        })
    }
}

impl Drop for Pic1704Service {
    /// Best-effort: nothing destructive on drop.
    ///
    /// The reference C code (`pic1704.c::pic1704_close`) closes the I2C
    /// fd, but we don't own the fd — the service thread does. We
    /// intentionally do NOT disable the DC-DC converter here, because a
    /// drop can happen mid-shutdown and we don't want to interfere with
    /// any orchestrated power-down sequence the daemon is running.
    fn drop(&mut self) {
        debug!(
            addr = format_args!("0x{:02X}", self.address),
            state = ?self.state,
            "Pic1704Service dropped",
        );
    }
}

// ===========================================================================
//  Build-time sealed-trait verification (no runtime cost)
// ===========================================================================
//
// The following compile-fail check would prove that an out-of-crate type
// cannot satisfy `Pic1704Authorized`. We can't actually run it from inside
// the same crate (it would defeat the purpose), but we leave the
// trybuild-style assertion notes here for A5 reference:
//
//   ```compile_fail
//   struct Rogue;
//   impl dcentrald_asic::pic1704::service::Pic1704Authorized for Rogue {}
//   ```
//
//   ```compile_fail
//   struct Rogue;
//   impl dcentrald_asic::pic1704::service::sealed::Sealed for Rogue {}
//   ```
//
// In-crate, the integration test in `tests/pic1704_protocol.rs` uses one
// of the `platforms::*` types to confirm the authorized path compiles.

#[cfg(test)]
mod tests {
    use super::platforms::*;
    use super::*;

    /// Compile-time proof that the platform marker types implement
    /// `Pic1704Authorized`. If this stops compiling, the sealed-trait
    /// whitelist has lost a platform.
    fn _compile_assert_authorized<P: Pic1704Authorized>() {}

    #[test]
    fn whitelisted_platforms_are_authorized() {
        _compile_assert_authorized::<Cv1835S19jPro>();
        _compile_assert_authorized::<Am335xBbS19jPro>();
        _compile_assert_authorized::<AmlogicS19jPro>();
        // W11.3 expansion (RE2 §2.5 + §6.1) — additional CV183x SKUs.
        _compile_assert_authorized::<Cv1835S19>();
        _compile_assert_authorized::<Cv1835S19i>();
        _compile_assert_authorized::<Cv1835S19XP>();
        // W12 expansion (RE3 §2.5 + §5.1) — T19 on CV183x (only carrier
        // RE3 documents for T19; no BB/AML variants).
        _compile_assert_authorized::<Cv1835T19>();
    }

    /// Confirms the sealed-trait pattern still rejects out-of-crate /
    /// rogue authorization. The actual compile-fail proof lives in the
    /// trybuild-style notes above the test module; this in-crate test
    /// only verifies the new T19 marker satisfies `Sealed` so the trait
    /// remains usable from inside the crate.
    #[test]
    fn t19_marker_is_sealed_and_authorized() {
        // If `Cv1835T19` ever loses either `sealed::Sealed` or
        // `Pic1704Authorized`, this fails to compile.
        _compile_assert_authorized::<Cv1835T19>();
    }

    #[test]
    fn heartbeat_interval_is_2s() {
        assert_eq!(HEARTBEAT_INTERVAL_MS, 2_000);
    }
}
