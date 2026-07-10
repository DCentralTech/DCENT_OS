//! PIC1704 voltage controller — short-form I2C register protocol.
//!
//! PIC1704 is a third PIC family used by DCENT_OS, distinct from:
//!
//! - `crate::pic` — PIC16F1704 on S9 hash boards (raw `[55 AA cmd ...]`
//!   preamble protocol, two firmware families: stock and BraiinsOS).
//! - `crate::dspic` — dsPIC33EP16GS202 on S17 / S19 / S19j Pro am2 hash
//!   boards (framed `[55 AA LEN CMD ... SUM]` protocol, multiple firmware
//!   revisions: 0x82 / 0x86 / 0x89 / 0x8A / 0xB9 / 0xFE).
//!
//! PIC1704 is the **short-form register-access** family used on:
//! - S19j Pro CV1835 (Sophgo CV1835 SoC, eMMC-rooted variant).
//! - AM335x BB (BeagleBone-class control board, S19j Pro variant).
//! - Amlogic S19j Pro variants.
//!
//! The wire protocol is plain I2C register access: write `[reg_addr,
//! data...]` to write, write `[reg_addr]` then read N bytes to read. No
//! preamble, no length byte, no checksum. Source of truth:
//! `DCENT_OS_DEVELOPMENT_KIT_FROMRE1/SOURCE_HAL/pic1704.{h,c}` from the
//! W2 RE deliverable.
//!
//! # Why this is a separate module
//!
//! Although PIC1704 shares the "voltage controller" role with `pic` and
//! `dspic`, the wire format and parser semantics are different enough
//! that mixing them in one module would obscure the per-family rules.
//! In particular, the 16-zero-byte parser flush mandated by
//!  is a **dsPIC-only** safety
//! rule — see the comment in `protocol.rs` for the full reasoning.
//!
//! # Public API
//!
//! - [`Pic1704State`] / [`classify_version`]: protocol primitives, host-safe.
//! - [`Pic1704Service`]: service-thread-backed runtime controller.
//! - [`service::Pic1704Authorized`] / [`service::platforms`]: sealed-trait
//!   construction whitelist (replaces with real `dcentrald-hal::platform::*`
//!   markers in wave A5).
//!
//! # Safety guarantees
//!
//! - **Sealed-trait construction gate.** `Pic1704Service::new` requires a
//!   marker type that implements [`service::Pic1704Authorized`]. Only
//!   the platforms in [`service::platforms`] satisfy that bound. Out-of-
//!   crate code cannot add new platforms because the trait is sealed.
//! - **No raw I2C.** The service routes all I/O through `I2cServiceHandle`,
//!   honoring the AM2 SINGLE-I2C-OWNER rule (one process owns
//!   `/dev/i2c-N`).
//! - **RESET is compile-gated.** `Pic1704Service::reset` only exists when
//!   the `recovery-tool` Cargo feature is enabled. The production
//!   `dcentrald` binary does not enable that feature, so calling
//!   `reset()` from the daemon is a compile error. See
//!   .
//! - **Programmer ops are recovery-tool ONLY.** The bootloader
//!   programmer surface (`pic_seek_1704`, `pic_erase_1704`,
//!   `pic_write_1704`, `pic_start_app_common`) lives in
//!   [`programmer`] and only exists when `recovery-tool` is enabled.
//!   Production `dcentrald` cannot link them — calling any programmer
//!   op from the daemon is a compile error. Each op also requires a
//!   [`programmer::ConfirmedBrickedToken`] (mintable only via the
//!   recovery binary's `--confirm-bricked` flow) and a runtime
//!   `version == VER_BOOTLOADER` check. See `programmer.rs` for the
//!   full safety contract.

pub mod protocol;
pub mod service;

/// Bootloader programmer ops — **recovery-tool ONLY**. Linked in only
/// when the `recovery-tool` Cargo feature is enabled. Production
/// `dcentrald` cannot reach these symbols. See module-level docs for
/// the layered safety contract (compile-time gate + sealed-trait
/// platform whitelist + `ConfirmedBrickedToken` runtime gate +
/// version-must-be-bootloader precondition).
#[cfg(feature = "recovery-tool")]
pub mod programmer;

/// PIC1704 framed-protocol programmer v2 (W14.C, R4 inferred). Distinct
/// from `programmer` (W11.7 BraiinsOS-shared register-style 0x01/0x05/0x09)
/// — uses framed REG_CMD ordinals 0x10-0x15 from the W4 handoff
/// `pic1704_v2.{c,h}`. Host-side wire-format helpers + collision-guard
/// against REG_VOLTAGE_L=0x10 in app mode. Recovery-tool feature gated;
/// production `dcentrald` cannot link.
///
/// Honest confidence label: framed protocol bytes are inferred from RE C
/// source; known-good CRC test vectors in handoff are still 0x????
/// placeholders. CLI uses `--i-acknowledge-pic1704-framed-inferred`,
/// NOT `--i-acknowledge-90-percent`.
#[cfg(feature = "recovery-tool")]
pub mod programmer_v2;

/// PIC1704 stock bmminer reflash protocol (W15.B, GHIDRA-EXTRACTED).
/// Decoded byte-exact from `_bitmain_pic_seek_1704.c`,
/// `_bitmain_pic_erase_1704.c`, `_bitmain_pic_write_1704.c`,
/// `_update_pic_app_program_1704.c`. Wire format:
/// `0x55` magic + additive-sum checksum + 2-phase write + 300 ms wait.
/// Distinct from W14.C V2 which uses REG_CMD 0x10-0x15 + CRC-ITU-T V.41
/// + single-phase write. Recovery-tool feature gated; production
/// `dcentrald` cannot link. Coexists with `programmer_v2`; routing
/// decision lives in [`reflash`].
#[cfg(feature = "recovery-tool")]
pub mod programmer_stock;

/// PIC1704 reflash auto-detection wrapper (W15.B3). Routes to either
/// [`programmer_stock`] (PRIMARY) or [`programmer_v2`] (fallback) based
/// on a stock-SEEK probe ACK pattern. Recovery-tool feature gated.
#[cfg(feature = "recovery-tool")]
pub mod reflash;

// W14.C re-exports: keep the framed-protocol surface available at the
// `pic1704::` prefix so CLI / pic-recovery callers don't need to
// reach into the submodule.
#[cfg(feature = "recovery-tool")]
pub use programmer_v2::{
    collision_guard, compute_crc_host, decode_verify_response, erase_steps_v2,
    read_version_step_v2, seek_steps_v2, start_app_steps_v2, write_steps_v2, FpError, BATCH_MAX,
    FLASH_APP_START, FLASH_MAX_WORDS, FLASH_PAGE_WORDS, FP_ERASE_PAGE, FP_READ_VERSION, FP_SEEK,
    FP_START_APP, FP_VERIFY_CRC, FP_WRITE_WORDS, POLL_MS, TIMEOUT_MS, VERSION_APP_88,
    VERSION_APP_89, VERSION_APP_8A, VERSION_BOOTLOADER,
};

// Re-export the canonical surface so call sites can write
// `crate::pic1704::Pic1704Service` / `crate::pic1704::Pic1704State`
// without reaching into the sub-modules.
pub use protocol::{
    classify_version, decode_le_word, enable_dc_dc_steps, heartbeat_steps, is_application_version,
    read_register_steps, start_app_steps, write_register_steps, Pic1704State, BL_CMD_JUMP,
    BL_MAGIC, CTRL_DC_DC_OFF, CTRL_DC_DC_ON, CTRL_HEARTBEAT, CTRL_RESET, HEARTBEAT_INTERVAL_MS,
    PIC1704_I2C_ADDR, POLL_INTERVAL_MS, REG_CONTROL, REG_CURRENT_H, REG_CURRENT_L, REG_STATUS,
    REG_TEMP, REG_TEMP_ALT, REG_VERSION, REG_VOLTAGE_H, REG_VOLTAGE_L, STATUS_APP_RUNNING,
    STATUS_DC_DC_ON, STATUS_FAULT, STATUS_OTP, VER_APPLICATION, VER_BOOTLOADER, VER_REV_A,
    VER_REV_B, WAIT_APP_TIMEOUT_MS,
};
pub use service::{Pic1704Authorized, Pic1704Service};
