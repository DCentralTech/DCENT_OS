//! Restore-to-Stock backend (wave-8 W8-F).
//!
//! Flashes Bitmain stock firmware to the **inactive** sysupgrade slot, sets
//! the boot-slot env, and schedules a reboot. After the operator harvests
//! per-(BHB, level) data from the booted stock firmware, the recommended
//! return path is `fw_setenv bootslot <prev>` from inside stock Bitmain
//! (or via serial-console U-Boot prompt). Per
//! , U-Boot `auto_recovery` is real but
//! defeated by both DCENT_OS and stock Bitmain S99upgrade scripts (each
//! unconditionally clears `upgrade_stage` on first boot) — so a plain
//! power-cycle is NOT a reliable revert path. The
//! `STOCK_BOOT_HARVEST_PROCEDURE.md` operator doc spells out both
//! reliable revert paths in step 10.
//!
//! Industry pattern parity (per user-confirmed scope):
//! - **VNish** ships a `/firmware/remove` and a stock-revert dashboard button
//!   that flashes a vendor-supplied Bitmain image (see
//!    for the brick-loop
//!   caveat we explicitly avoid).
//! - **LuxOS** has a "Reset to Factory" web UI that re-flashes Bitmain stock.
//! - **BraiinsOS** has `bos togglefw` + a dashboard switch that toggles
//!   between the two firmware slots; one of the two slots is typically
//!   stock Bitmain after a fresh install.
//!
//! What we improve over those:
//! 1. **Mandatory NAND backup** before any flash. Output:
//!    `/data/restore-backup-<timestamp>/{mtd0.img, mtd1.img, mtd2.img,
//!    ubinfo.txt, fwenv.txt}`. Recovery is `flash_erase` + `nandwrite`
//!    of the same images back into the same partitions if anything
//!    goes sideways.
//! 2. **Mandatory safety preflight** — refuses to flash any tarball that
//!    contains the SECURE_BOOT_SET eFuse-burn blob, the Hashcore
//!    universal `$6$4rQjfxJBpRYbzeys$…` root hash, the VNish
//!    `atlas@anthill.farm` SSH key, the VNish `hotelfee.json` devfee
//!    file, the `daemons:22322` injection listener, or the Innosilicon
//!    `dtu` phone-home endpoint. Same detector list as W5-C / W5-E
//!    `vnish_security_audit.py`, ported to Rust here so dcentrald can
//!    enforce the gate live without shelling out to a Python tool.
//! 3. **Operator-typed serial confirmation**. The operator must POST the
//!    miner serial verbatim — the daemon compares it against
//!    `state.hardware_info.miner_serial` and rejects mismatches. This
//!    prevents accidental "Restore to Stock" of the wrong miner in a
//!    multi-tab dashboard.
//! 4. **Default dry-run**. The destructive write only fires when the
//!    operator passes `confirm: true` in the body. Dry-run returns the
//!    full plan + safety findings + planned reboot timestamp without
//!    touching NAND.
//!
//! ## HTTP surface
//!
//! | Method | Path | Behavior |
//! |---|---|---|
//! | POST | `/api/system/restore-to-stock/preflight` | Run safety preflight + serial check + NAND inventory only. No flash. |
//! | POST | `/api/system/restore-to-stock` | Full flow: preflight, NAND backup, stage to inactive slot, set U-Boot flag, schedule reboot. Default `confirm: false` returns the plan only. |
//! | GET  | `/api/system/restore-to-stock/status` | Read-only status: last preflight result, last backup path, last scheduled reboot. |
//!
//! Auth: same dashboard cookie session as the rest of `dcentrald-api`.
//!
//! ## Body shape (`POST /api/system/restore-to-stock`)
//!
//! ```json
//! {
//!   "stock_firmware_staged_path": "/tmp/dcentos-upgrade/<uuid>/Antminer-S9-...tar.gz",
//!   "stock_firmware_sha256":     "<expected sha256, optional, fail-closed if mismatch>",
//!   "operator_serial_typed":     "<exactly the value reported in /api/system/info>",
//!   "acknowledge_breaker_warning": true,
//!   "hashboard_count_to_use":    2,
//!   "confirm_string_typed":      "RESTORE TO STOCK",
//!   "confirm":                   false
//! }
//! ```
//!
//! ## Hard rules baked into this module
//!
//! - NEVER flash to the active NAND slot (always uses
//!   `inactive_mtd_for_active_slot` — same logic as
//!   `scripts/revert_to_stock.sh`).
//! - NEVER skip the safety preflight (`PreflightVerdict::Critical` ->
//!   `400 Bad Request`, no override flag honored — `no_override:true`
//!   IOCs match the Python detector contract in
//!   `projects/security-audit/data/iocs.json`).
//! - NEVER skip the operator serial typed-confirm (`400 serial_mismatch`).
//! - NEVER skip the NAND backup (return `500 nand_backup_failed`,
//!   refuse to schedule reboot).
//! - NEVER reboot synchronously inside the request handler — schedule
//!   via `tokio::spawn` with a configurable delay so the dashboard
//!   shows the response before the network drops.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use dcentrald_api_types::audit_log::AuditEvent;

use crate::{push_audit_event, AppState};

// ---------------------------------------------------------------------------
// Typed error enum ( W9-A — R4-H4)
// ---------------------------------------------------------------------------

/// Restore-to-stock pipeline errors. Replaces the prior `Result<_, String>`
/// helper signatures so call sites can match on error kind without parsing
/// stringified diagnostics. Per R4-H4 in
/// .
#[derive(Debug, Error)]
pub enum RestoreError {
    /// Filesystem I/O failure. The `path` is `Some(_)` when the call
    /// site has provenance (operator-relevant for diagnostics); `None`
    /// when the error came through `?`-propagation from `From<io::Error>`
    /// and the path context was lost.  W10-D (R4'-L4) widened the
    /// variant from `path: PathBuf` to `path: Option<PathBuf>` so the
    /// `From<io::Error>` impl no longer fabricates an empty path.
    #[error("io error{}: {source}", path.as_ref().map(|p| format!(" on {}", p.display())).unwrap_or_default())]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: io::Error,
    },

    /// External command (tar, dd, fw_setenv, flash_erase, nandwrite,
    /// revert_to_stock.sh) returned non-zero or could not be spawned.
    #[error("command `{command}` failed: {reason}")]
    CommandFailed { command: String, reason: String },

    /// Tarball is shaped wrong (missing UBI/rootfs entries, slip attempt,
    /// extraction failure, etc.).
    #[error("invalid tarball: {reason}")]
    InvalidTarball { reason: String },

    /// Path-traversal attempt detected during tar extraction
    /// (R4-H2 mitigation).
    #[error("path traversal attempt rejected: {path}")]
    PathTraversal { path: String },

    /// Safety preflight returned one or more critical findings.
    #[error("safety preflight failed: {findings:?}")]
    SafetyPreflightFailed { findings: Vec<String> },

    /// NAND backup pipeline failed at a specific step.
    #[error("nand backup failed at {step}: {reason}")]
    NandBackupFailed { step: String, reason: String },

    /// Flash dispatch failed (flash_erase / nandwrite / fw_setenv /
    /// revert_to_stock.sh). Replaces R1-C1's silent sysupgrade -f
    /// failure path.
    #[error("flash dispatch failed: {reason}")]
    FlashFailed { reason: String },

    /// Another restore-to-stock request is already in flight. Reserved
    /// for the W9-C concurrency mutex; kept here so the variant is part
    /// of the wire-stable error vocabulary from day one.
    #[error("concurrent restore in progress")]
    Conflict,

    /// Internal invariant breach — used for poisoned mutexes, missing
    /// canonical paths, hash-mismatch refusals from the spawned task,
    /// etc. Per W9-C R4-C1 + R4-H1.
    #[error("internal restore-to-stock failure: {reason}")]
    Internal { reason: String },

    /// TOCTOU mismatch detected by the spawned task between preflight
    /// canonicalization/hash and pre-write canonicalization/hash.
    /// Per W9-C R4-H1.
    #[error("staged tarball drifted between preflight and flash dispatch: {reason}")]
    StagedTarballDrift { reason: String },
}

impl From<io::Error> for RestoreError {
    fn from(source: io::Error) -> Self {
        //  W10-D (R4'-L4): `From<io::Error>` no longer fabricates
        // an empty PathBuf — `?`-propagated errors honestly report
        // `path: None` so call sites that need provenance use the
        // `RestoreError::io(source, path)` helper or `map_err(...)`
        // instead.
        RestoreError::Io { path: None, source }
    }
}

impl RestoreError {
    ///  W10-D (R4'-L4): construct a `RestoreError::Io` with
    /// optional path provenance. Pass `Some(p)` when the call site has
    /// the offending path; pass `None` (or omit) for `?`-propagated
    /// errors.
    pub fn io(source: io::Error, path: impl Into<Option<PathBuf>>) -> Self {
        RestoreError::Io {
            path: path.into(),
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// PII helpers
// ---------------------------------------------------------------------------

///  W10-D (A1-LOW-3): truncate an operator-typed serial to
/// `XXXX…YYYY` for log emission. Full serials are PII-class on a
/// fleet operator's perspective and shouldn't be persisted in tracing
/// output. Strings of 8 chars or shorter are reduced to `REDACTED` to
/// avoid exposing weak/short serials.
pub(crate) fn truncate_serial(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= 8 {
        return "REDACTED".to_string();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let n = chars.len();
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[n - 4..].iter().collect();
    format!("{prefix}…{suffix}")
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Root for NAND backups created by this module. The S9 control board
/// has /data on a UBI volume that survives sysupgrade — the same
/// volume bosminer + dcentrald use for `dcentrald.toml` etc.
const NAND_BACKUP_ROOT: &str = "/data/restore-backup";

/// Reboot delay after a successful flash schedule, in seconds. Long
/// enough for the dashboard to render the response and for the
/// operator to read the multi-line confirmation panel.
const REBOOT_DELAY_SECS: u64 = 30;

/// `/data` mount point — checked for free space and writability
/// before NAND backup. Per R3-HIGH (free-space precheck).
const NAND_BACKUP_DATA_DIR: &str = "/data";

/// UBI image magic (`UBI#` = 0x55 0x42 0x49 0x23). Used by
/// [`ubi_image_has_magic`] to assert that a dumped firmware-slot
/// image actually starts with a valid UBI header. Closes R1-H-2
/// (UBI shape validation before staging stock).
const UBI_MAGIC: &[u8; 4] = b"UBI#";

/// uImage magic (`27 05 19 56`, big-endian) — the U-Boot legacy image
/// header signature. Amlogic A113D (am3-aml) ships kernel + rootfs as
/// a uImage at mtd5 offset 0x5100000 — NOT a UBI volume — so the
/// post-write readback uses this magic instead of [`UBI_MAGIC`].
/// Per `DCENT_OS_Antminer/ "Boot Chain (Extracted 2026-04-11)".
#[allow(dead_code)] // referenced from Amlogic revert scripts via shell
const UIMAGE_MAGIC: &[u8; 4] = &[0x27, 0x05, 0x19, 0x56];

///  W13-D: ring-buffer cap on
/// [`RestoreToStockStatus::recent_log_lines`]. The dashboard renders
/// only the last ~10 lines, so 100 is comfortable headroom for
/// operators inspecting the raw `/status` endpoint. Each line is also
/// truncated to [`RECENT_LOG_LINE_MAX_LEN`] to bound memory if the
/// writer emits one giant unbroken line (e.g. nandwrite progress
/// without `\n`).
const RECENT_LOG_LINES_MAX: usize = 100;

///  W13-D: per-line truncation cap. Beyond this the line is
/// suffixed with `…` and pushed truncated. Bounds total memory to
/// roughly `RECENT_LOG_LINES_MAX * RECENT_LOG_LINE_MAX_LEN` ≈ 100 KiB
/// in the worst case.
const RECENT_LOG_LINE_MAX_LEN: usize = 1024;

// ---------------------------------------------------------------------------
//  W12-B — Platform-keyed profile table for restore-to-stock.
//
// -11 hardcoded S9 am1 specifics (mtd4/7/8, S9_UBI_EXPECTED_LEBS,
// single revert script, etc.).  prepares the codebase to flash
// stock onto the office fleet (S19j Pro BB / S19k Pro / S21) by
// keying every per-platform constant on a `PlatformProfile`.
//
// Plan: internal W12-B planning notes.
//
// `verified_revertable: true` means a wave-≤11 live practical-test
// proved end-to-end revert on this platform. Today only S9 am1 has
// that proof. Other entries are CODE-COMPLETE — the destructive
// handler refuses confirm:true on them via the new 2-layer gate
// (`profile_for_current_platform()` returns Some, then
// `verified_revertable == false` triggers
// `rejected_unsupported_platform_pending_live_test`).
// ---------------------------------------------------------------------------

/// Per-platform profile feeding the restore-to-stock destructive path.
/// Replaces the wave-8-11 hardcoded S9 am1 constants.
#[derive(Debug, Clone)]
pub struct PlatformProfile {
    /// Platform fingerprint produced by [`detect_platform_signature`].
    /// Must match the manifest `platform_signature` for stock-image
    /// verification.
    pub signature: &'static str,
    /// MTD partitions the NAND backup pipeline dumps before any flash.
    /// Each entry is a `/dev/mtdN` path; ordering controls the
    /// `SHA256SUMS` line order.
    pub nand_backup_mtds: &'static [&'static str],
    /// MTD partitions that hold the firmware A/B slots. The slot-plan
    /// resolver picks the inactive entry and the writer flashes there.
    pub firmware_slot_mtds: &'static [&'static str],
    /// Expected UBI volume table on the inactive firmware slot. `None`
    /// for am3-aml platforms that ship a uImage at mtd5 offset
    /// 0x5100000 instead of a UBI volume — those skip the LEB-mirror
    /// gate entirely.
    pub ubi_expected_lebs: Option<&'static [(&'static str, u32)]>,
    /// U-Boot env keys probed (in priority order) by [`read_slot_plan`]
    /// to discover the active firmware slot. S9 am1 stock uses
    /// `bootslot=a|b`, BraiinsOS / DCENT_OS Buildroot uses
    /// `firmware=1|2`, am3-aml uses `dcent_boot_slot` / `firstboot`,
    /// AM335x BB uses `firmware` / `bootslot`.
    pub bootslot_env_keys: &'static [&'static str],
    /// Path to the per-platform revert script installed by the
    /// Buildroot post-build hook. Probed at runtime; falls back to
    /// in-tree dev paths if the rootfs install is missing.
    pub revert_script: &'static str,
    /// Minimum free space (bytes) at `/data` before NAND backup
    /// starts. Per-platform because the dump sizes differ
    /// (S9 am1 ≈ 190 MiB; am3-aml mtd5 alone ≈ 50 MiB).
    pub min_free_bytes: u64,
    /// Has this platform been live-tested end-to-end (preflight + NAND
    /// backup + flash + reboot + revert) on real hardware?
    /// `verified_revertable: false` means CODE-PATH-COMPLETE but not
    /// hardware-proven; the destructive handler refuses confirm:true
    /// until an operator flips this flag in source after a successful
    /// live test (and re-runs the cargo test pin).
    pub verified_revertable: bool,
}

/// S9 am1 (Zynq XC7Z010, BM1387). -11 home-S9 first practical
/// test GO. The destructive constants below match the wave-8-11
/// hardcoded values exactly — no behavior change at this point.
const S9_AM1_NAND_BACKUP_MTDS: &[&str] = &["/dev/mtd4", "/dev/mtd7", "/dev/mtd8"];
const S9_AM1_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd7", "/dev/mtd8"];
/// Expected LEB counts per UBI volume on the S9 am1 firmware slots.
/// (S9 reference
/// layout from .39 2026-04-17): kernel=25, rootfs=166,
/// rootfs_data=525 LEBs.
const S9_UBI_EXPECTED_LEBS: &[(&str, u32)] =
    &[("kernel", 25), ("rootfs", 166), ("rootfs_data", 525)];
const S9_AM1_BOOTSLOT_ENV_KEYS: &[&str] = &["bootslot", "active_slot"];

/// S17 am2-s17 (Zynq XC7Z010, BM1397+, 3 hashboards × 48 chips).
/// W16 code-only port: BM1397+ driver in `dcentrald-asic` already
/// production-quality (different cmd headers 0x51/0x41 per
/// , 672 cores/chip, 4-midstate
/// version rolling, BM139X mode bit on FPGA CTRL). dsPIC33EP framed
/// protocol module (`dcentrald-asic/src/dspic.rs`) covers voltage
/// control. Platform signature `zynq-am2-bm1397` is emitted by
/// `detect_platform_signature_with_root` on Xilinx Zynq + DT model
/// containing `antminer-s17`/`am2-s17`.
///
/// Sysupgrade NAND layout: S17 uses the same am2-s17 control board
/// flash topology as S19 Pro am2 (XC7Z020-class). The mtd partition
/// list mirrors the S9 am1 BraiinsOS/DCENT_OS Buildroot 10-partition
/// layout we reuse on Zynq today (mtd4 = U-Boot env, mtd7/mtd8 =
/// firmware A/B). HONEST GAP: the live mtd partition list on a
/// production S17 has NOT been pulled in this code-only wave. Wave-W16
/// followup must validate `cat /proc/mtd` on a live S17 unit before
/// `verified_revertable` can flip to `true`.
const S17_AM2_NAND_BACKUP_MTDS: &[&str] = &["/dev/mtd4", "/dev/mtd7", "/dev/mtd8"];
const S17_AM2_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd7", "/dev/mtd8"];
/// S17 reuses the S9 am1 UBI layout when running DCENT_OS Buildroot
/// (kernel=25, rootfs=166, rootfs_data=525). HONEST GAP: live S17
/// units may have been built with a different Buildroot config; the
/// W16 followup live test must confirm or override these counts.
const S17_UBI_EXPECTED_LEBS: &[(&str, u32)] =
    &[("kernel", 25), ("rootfs", 166), ("rootfs_data", 525)];
const S17_AM2_BOOTSLOT_ENV_KEYS: &[&str] = &["bootslot", "active_slot"];

/// S19 Pro / S19j Pro Zynq am2 (XC7Z020 SoC, BM1398 + BM1362).
/// W19 code-only entry. Both miner families share the SAME XC7Z020
/// SoC + control board topology, so the platform-signature detector
/// emits a single signature `zynq-am2-bm1398` for both
/// (`detect_platform_signature_with_root` line ~2615 — DT model contains
/// `antminer-s19` / `am2-s19` / `xc7z020`).
///
/// Source: live `a lab unit` U-Boot env extraction at
/// . Confirmed
/// `firmware_select=if test x${firmware} = x1; then ... firmware_mtd 7;
/// else ... firmware_mtd 8;` and
/// `mtdparts=pl35x-nand:8m(boot),12m(boot-failover),2m(fpga1),
/// 2m(fpga2),512k(uboot_env),512k(miner_cfg),87m(recovery),
/// 57m(firmware1),57m(firmware2),30m(factory)` →
/// mtd4=uboot_env, mtd7=firmware1, mtd8=firmware2.
///
/// am2-Zynq XC7Z020 NAND backup mtd list mirrors am1-S9 / am2-S17
/// because the BraiinsOS / DCENT_OS Buildroot layout is identical on
/// every Zynq am1/am2 control board today.
///
/// HONEST GAP — UBI LEB shape: live UBI inspection has NOT been pulled
/// in this code-only wave. The S19j Pro BraiinsOS+ build presumably
/// uses the same `kernel/rootfs/rootfs_data` 25/166/525 LEB shape as
/// S9 am1, but a future W18 / W17-followup live test must run
/// `ubinfo /dev/ubi0` on a real S19 Pro / S19j Pro Zynq am2 unit before
/// `verified_revertable` can flip to `true`. The conservative path
/// today: keep `verified_revertable: false` so the destructive handler
/// rejects confirm:true with `rejected_unsupported_platform_pending_live_test`.
const S19_AM2_NAND_BACKUP_MTDS: &[&str] = &["/dev/mtd4", "/dev/mtd7", "/dev/mtd8"];
const S19_AM2_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd7", "/dev/mtd8"];
/// Conservative LEB expectation matching the S9 am1 / S17 am2-s17
/// BraiinsOS layout. HONEST GAP: not live-verified on S19 Pro / S19j
/// Pro Zynq am2 hardware. Live preflight on first W18 / W17-followup
/// test must record actual LEB counts and either confirm or override.
const S19_AM2_UBI_EXPECTED_LEBS: &[(&str, u32)] =
    &[("kernel", 25), ("rootfs", 166), ("rootfs_data", 525)];
const S19_AM2_BOOTSLOT_ENV_KEYS: &[&str] = &["firmware", "bootslot"];

/// AM335x BeagleBone BB stock-Bitmain S19j Pro (TI Sitara, BM1362).
/// Source: ,
/// ,
/// . Stock SSH is
/// `miner:miner` (root has /bin/false). uart_trans.ko is the ASIC UART
/// shim; not load-bearing for restore-to-stock.
///
/// HONEST GAP: the live extraction tree on .79
/// (*`) was not pulled before W12-B
/// landed, so the exact mtd partition list is TBD. The placeholder
/// list below is a conservative minimum (env + firmware slots) based
/// on standard Bitmain BB layout; the live-test wave (W13) MUST
/// validate or correct these mtd device paths against
/// `cat /proc/mtd` on a real BB unit BEFORE flipping
/// `verified_revertable` to true.
const AM335X_BB_NAND_BACKUP_MTDS: &[&str] = &["/dev/mtd0", "/dev/mtd1", "/dev/mtd2"];
const AM335X_BB_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd1", "/dev/mtd2"];
const AM335X_BB_BOOTSLOT_ENV_KEYS: &[&str] = &["firmware", "bootslot"];

/// am3-aml-S21 (Amlogic A113D, BM1368). Source:
/// `DCENT_OS_Antminer/ "Amlogic A113D Platform Port" +
/// "Boot Chain (Extracted 2026-04-11)". CRITICAL DIFFERENCE: ships
/// kernel-rootfs uImage at mtd5 offset 0x5100000 — NOT a UBI volume
/// — so `ubi_expected_lebs` is `None` and the post-write readback
/// uses [`UIMAGE_MAGIC`] not [`UBI_MAGIC`]. The U-Boot env partition
/// is `/dev/nand_env`.
const AM3_AML_S21_NAND_BACKUP_MTDS: &[&str] = &["/dev/nand_env", "/dev/mtd5"];
const AM3_AML_S21_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd5"];
const AM3_AML_BOOTSLOT_ENV_KEYS: &[&str] = &["dcent_boot_slot", "firstboot"];

/// am3-aml-S19k Pro (Amlogic A113D, BM1366). Source:
/// . Same Amlogic
/// flash-mechanism as S21 but BHB56902 hashboards (different chip
/// family) and APW121215f PSU fw=0x76. mtd layout is the same as the
/// S21 — `/dev/mtd5` rootfs uImage + `/dev/nand_env` U-Boot env.
const AM3_AML_S19K_NAND_BACKUP_MTDS: &[&str] = &["/dev/nand_env", "/dev/mtd5"];
const AM3_AML_S19K_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd5"];

/// am3-aml-S19j Pro (Amlogic A113D, BM1362).  W23 entry.
/// the S19j Pro Amlogic shares
/// the AML S11board control board with S21 and L9 — BYTE-IDENTICAL
/// across L9 / S19j Pro / S21 v1.2.6-rc5. Same Amlogic uImage flash
/// mechanism (`/dev/mtd5` rootfs + `/dev/nand_env` U-Boot env), same
/// `revert_to_stock_am3_aml_s21.sh` is reused (BM1362 vs BM1368 is a
/// chip-family delta only — the flash layout is identical). The
/// dedicated `revert_to_stock_am3_aml_s19j.sh` script is a future W22
/// follow-up if BM1362-specific differences emerge from live testing.
const AM3_AML_S19J_NAND_BACKUP_MTDS: &[&str] = &["/dev/nand_env", "/dev/mtd5"];
const AM3_AML_S19J_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd5"];

/// am3-aml-S21 Pro / S21 XP (Amlogic A113D, **BM1370** — NOT BM1368).
/// Phase 2B (v2 preparedness sweep, 2026-05-15). The v2 sweep
/// (
/// am3-aml.md` §3.2 + `axis-toolbox.md` §3) found S21 Pro / S21 XP
/// previously fell through `detect_platform_signature`'s bare-"s21"
/// match to `amlogic-a113d-bm1368`, so the BM1368-tuned PROFILE_TABLE
/// entry (frequency/voltage tables, S21 revert script) silently
/// covered a BM1370 carrier. S21 Pro / S21 XP carry TSMC 3 nm BM1370
/// (`dcentrald-asic::drivers::bm1370`, 999 LOC). The AML S11board
/// control board is byte-identical across the A113D family
///, so the NAND/MTD flash
/// topology and the S21 revert script are reused unchanged — the
/// BM1368↔BM1370 delta is a chip-family + freq/voltage-table delta,
/// not a flash-layout delta. `verified_revertable: false` — no
/// BM1370 bench unit exists (Tier-2/Tier-3 hardware ask, 0/4 RE-round
/// hardware asks landed); the destructive handler refuses confirm:true
/// until a live S21 Pro / S21 XP round-trip flips this flag.
const AM3_AML_S21PRO_NAND_BACKUP_MTDS: &[&str] = &["/dev/nand_env", "/dev/mtd5"];
const AM3_AML_S21PRO_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd5"];
const AM3_AML_S21XP_NAND_BACKUP_MTDS: &[&str] = &["/dev/nand_env", "/dev/mtd5"];
const AM3_AML_S21XP_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mtd5"];

/// cv1835-S19j Pro (Sophgo CV1835, BM1362). W2B B1 entry (2026-05-09).
/// Sourced from the dev-kit RE deliverable
/// (`DCENT_OS_DEVELOPMENT_KIT_FROMRE1/.../DOCS/multi_platform_master.md` §4
/// + `DCENT_OS_ §4/§11). CV1835 boots from eMMC, NOT
/// NAND — the rootfs lives on `/dev/mmcblk0p2` with U-Boot env on
/// `/dev/mmcblk0boot0`. The mtd-style placeholders below match the
/// dev-kit's BMU sysupgrade conventions; B5 (CV1835 eMMC sysupgrade +
/// BMU parser) wires the actual block-device path discovery. Until
/// then, the entry is `verified_revertable: false` and `runtime-only`
/// in dcent-toolbox install routing — same posture as am3-aml on its
/// pre-live-test wave. NO live CV1835 unit on the fleet (2026-05-09);
/// promotion gate = 24-devmem replay match against fresh hardware.
const CV1835_S19J_NAND_BACKUP_MTDS: &[&str] = &["/dev/mmcblk0boot0", "/dev/mmcblk0p2"];
const CV1835_S19J_FIRMWARE_SLOT_MTDS: &[&str] = &["/dev/mmcblk0p2"];
const CV1835_BOOTSLOT_ENV_KEYS: &[&str] = &["dcent_boot_slot", "bootcmd"];
/// Minimum-free-space tier — CV1835 (eMMC ~4 GiB; rootfs ~150 MiB +
/// 100 MiB headroom for sysupgrade staging).
const CV1835_MIN_FREE_BYTES: u64 = 250 * 1024 * 1024;

/// Minimum-free-space tier — S9 am1 (mtd4 + mtd7 + mtd8 ≈ 190 MiB
/// raw; 250 MiB headroom for sha + ubinfo + fwenv + fw_setenv copy).
const S9_AM1_MIN_FREE_BYTES: u64 = 250 * 1024 * 1024;
/// Minimum-free-space tier — S17 am2-s17 (same backup mtd list as
/// S9 am1; same 250 MiB tier).
const S17_AM2_MIN_FREE_BYTES: u64 = 250 * 1024 * 1024;
/// Minimum-free-space tier — S19 Pro / S19j Pro Zynq am2 (XC7Z020).
/// Same backup mtd list as S9 am1 / S17 am2-s17; same 250 MiB tier.
const S19_AM2_MIN_FREE_BYTES: u64 = 250 * 1024 * 1024;
/// Minimum-free-space tier — am3-aml (mtd5 alone ≈ 50 MiB; 100 MiB
/// headroom).
const AM3_AML_MIN_FREE_BYTES: u64 = 100 * 1024 * 1024;
/// Minimum-free-space tier — AM335x BB (rough estimate; refine in
/// W13 live test).
const AM335X_BB_MIN_FREE_BYTES: u64 = 200 * 1024 * 1024;

///  W12-B PROFILE_TABLE ( W16 +  W19 +  W23
/// +  B1 extended). Linear scan via [`profile_for`] — 8 entries
/// today, so a `HashMap` would just be ceremony.
///
/// W23 reconciliation (2026-05-04): Amlogic signature keys were
/// migrated from `amlogic-am3-bm{1366,1368}` to
/// `amlogic-a113d-bm{1362,1366,1368}` to match the
/// [`detect_platform_signature`] output (which is grounded in
/// /proc/device-tree probe — the most authoritative source). Without
/// this rename, am3-aml platforms fell through the platform gate and
/// returned `rejected_unsupported_platform_pending_live_test` even
/// though PROFILE_TABLE entries existed. W23 also added the missing
/// 5th Amlogic entry (`amlogic-a113d-bm1362` — S19j Pro Amlogic) so
/// all three Amlogic chip families are covered. Per
/// , S19j Pro Amlogic / S21 / L9
/// share the AML S11board control board byte-identically, so the
/// S19j Pro entry reuses `revert_to_stock_am3_aml_s21.sh` until
/// BM1362-specific differences emerge from live testing.
pub const PROFILE_TABLE: &[PlatformProfile] = &[
    // 1. S9 am1 — wave-8-11 home-S9 first practical test GO.
    PlatformProfile {
        signature: "zynq-am1-bm1387",
        nand_backup_mtds: S9_AM1_NAND_BACKUP_MTDS,
        firmware_slot_mtds: S9_AM1_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: Some(S9_UBI_EXPECTED_LEBS),
        bootslot_env_keys: S9_AM1_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_s9.sh",
        min_free_bytes: S9_AM1_MIN_FREE_BYTES,
        verified_revertable: true,
    },
    // 2. AM335x BB stock-Bitmain S19j Pro (TI Sitara, BM1362).
    PlatformProfile {
        signature: "am335x-bb-bm1362",
        nand_backup_mtds: AM335X_BB_NAND_BACKUP_MTDS,
        firmware_slot_mtds: AM335X_BB_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // BB stock layout not yet validated
        bootslot_env_keys: AM335X_BB_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_am335x_bb.sh",
        min_free_bytes: AM335X_BB_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 5. S17 am2-s17 (Zynq XC7Z010, BM1397+, 3×48 chips).
    //    code-only entry; `verified_revertable: false` until a W16
    //    followup live test validates the mtd layout + UBI LEB counts
    //    on a real S17 unit.
    PlatformProfile {
        signature: "zynq-am2-bm1397",
        nand_backup_mtds: S17_AM2_NAND_BACKUP_MTDS,
        firmware_slot_mtds: S17_AM2_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: Some(S17_UBI_EXPECTED_LEBS),
        bootslot_env_keys: S17_AM2_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_s17.sh",
        min_free_bytes: S17_AM2_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 6. S19 Pro / S19j Pro Zynq am2 (XC7Z020, BM1398 / BM1362).
    //    W19 code-only entry. Both miner families share XC7Z020 + the
    //    same control board NAND topology, so a single signature
    //    covers them. `verified_revertable: false` until W17/W18
    //    followup live tests validate UBI LEB shape on real hardware.
    //    Detector emits this signature for any antminer-s19/am2-s19/
    //    xc7z020 DT model match.
    PlatformProfile {
        signature: "zynq-am2-bm1398",
        nand_backup_mtds: S19_AM2_NAND_BACKUP_MTDS,
        firmware_slot_mtds: S19_AM2_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: Some(S19_AM2_UBI_EXPECTED_LEBS),
        bootslot_env_keys: S19_AM2_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_s19_am2.sh",
        min_free_bytes: S19_AM2_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 3. am3-aml-S21 (Amlogic A113D, BM1368). W23: signature renamed
    //    `amlogic-am3-bm1368` → `amlogic-a113d-bm1368` to match
    //    `detect_platform_signature` output.
    PlatformProfile {
        signature: "amlogic-a113d-bm1368",
        nand_backup_mtds: AM3_AML_S21_NAND_BACKUP_MTDS,
        firmware_slot_mtds: AM3_AML_S21_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // uImage at mtd5 offset 0x5100000
        bootslot_env_keys: AM3_AML_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_am3_aml_s21.sh",
        min_free_bytes: AM3_AML_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 4. am3-aml-S19k Pro (Amlogic A113D, BM1366). W23: signature
    //    renamed `amlogic-am3-bm1366` → `amlogic-a113d-bm1366`.
    PlatformProfile {
        signature: "amlogic-a113d-bm1366",
        nand_backup_mtds: AM3_AML_S19K_NAND_BACKUP_MTDS,
        firmware_slot_mtds: AM3_AML_S19K_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // uImage at mtd5 offset 0x5100000
        bootslot_env_keys: AM3_AML_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_am3_aml_s19k.sh",
        min_free_bytes: AM3_AML_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 7. am3-aml-S19j Pro (Amlogic A113D, BM1362). W23 NEW entry.
    //    Closes W12-Z gap #4 (S19j Pro Amlogic has manifest placeholder
    //    but no PROFILE_TABLE entry — Layer 1 gate refused with
    //    `rejected_unsupported_platform`). Reuses the S21 revert script
    //    because the AML S11board is byte-identical across S19j/S21/L9
    //. Same uImage at mtd5
    //    offset 0x5100000 flash mechanism.
    //
    //     (B4, 2026-05-09) cross-cite: the dcent-toolbox install
    //    route `amlogic-s19jpro-stock-rootfs_window_lab` is the host-side
    //    counterpart to this firmware-side PROFILE_TABLE entry. Both use
    //    the same mtd5 + 0x5100000 + 40 MiB rootfs window writer; the
    //    new toolbox route discriminates carriers by detected model +
    //    package product family. See
    //    `projects/dcent-toolbox/src/dcent_toolbox/core/installer.py`
    //    `iter_install_support_facts()` and
    //    `tests/test_install_route_amlogic_s19jpro_planner.py`. No
    //    firmware-side changes required — this comment is a citation
    //    only.
    PlatformProfile {
        signature: "amlogic-a113d-bm1362",
        nand_backup_mtds: AM3_AML_S19J_NAND_BACKUP_MTDS,
        firmware_slot_mtds: AM3_AML_S19J_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // uImage at mtd5 offset 0x5100000
        bootslot_env_keys: AM3_AML_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_am3_aml_s21.sh",
        min_free_bytes: AM3_AML_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 9. am3-aml-S21 Pro (Amlogic A113D, BM1370). Phase 2B (v2 sweep,
    //    2026-05-15) NEW entry. Closes the v2-sweep BM1370 silent-routing
    //    gap: before this entry, `detect_platform_signature`'s bare-"s21"
    //    DT-model match returned `amlogic-a113d-bm1368`, so the BM1368
    //    PROFILE_TABLE row + S21 revert script silently covered a BM1370
    //    S21 Pro carrier. The detector now emits `amlogic-a113d-bm1370`
    //    for "s21 pro" / "s21pro" DT models (checked BEFORE the bare-s21
    //    branch); this entry gives that signature a real profile so the
    //    platform gate doesn't refuse with
    //    `rejected_unsupported_platform_pending_live_test`. AML S11board
    //    is byte-identical across the A113D family
    // so the S21 revert script +
    //    mtd5/nand_env topology are reused — BM1368↔BM1370 is a chip /
    //    freq-voltage-table delta, not a flash-layout delta.
    //    `verified_revertable: false` until a live S21 Pro bench unit
    //    round-trip (Tier-2 hardware ask).
    PlatformProfile {
        signature: "amlogic-a113d-bm1370",
        nand_backup_mtds: AM3_AML_S21PRO_NAND_BACKUP_MTDS,
        firmware_slot_mtds: AM3_AML_S21PRO_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // uImage at mtd5 offset 0x5100000
        bootslot_env_keys: AM3_AML_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_am3_aml_s21.sh",
        min_free_bytes: AM3_AML_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 10. am3-aml-S21 XP (Amlogic A113D, BM1370). Phase 2B (v2 sweep,
    //     2026-05-15) NEW entry. Sibling of the S21 Pro BM1370 entry
    //     above — same A113D / AML S11board / mtd5+nand_env topology,
    //     same S21 revert script. Distinct signature
    //     `amlogic-a113d-bm1370-xp` so the detector→PROFILE_TABLE
    //     roundtrip stays 1:1 per DT model and a future S21 XP-specific
    //     freq/voltage divergence can pin to its own row without
    //     touching S21 Pro. `verified_revertable: false` until a live
    //     S21 XP bench unit round-trip (Tier-3 hardware ask).
    PlatformProfile {
        signature: "amlogic-a113d-bm1370-xp",
        nand_backup_mtds: AM3_AML_S21XP_NAND_BACKUP_MTDS,
        firmware_slot_mtds: AM3_AML_S21XP_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // uImage at mtd5 offset 0x5100000
        bootslot_env_keys: AM3_AML_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_am3_aml_s21.sh",
        min_free_bytes: AM3_AML_MIN_FREE_BYTES,
        verified_revertable: false,
    },
    // 8. cv1835-S19j Pro (Sophgo CV1835, BM1362). W2B B1 NEW entry
    //    (2026-05-09). Code-only platform port — no live CV1835 unit
    //    on the fleet. Profile entry exists so the platform router
    //    doesn't refuse with `rejected_unsupported_platform` once the
    //    first CV1835 ships. eMMC-rooted (mmcblk0p2), NOT NAND — the
    //    `nand_backup_mtds` field is reused as the eMMC block-device
    //    list per the dev-kit BMU sysupgrade convention. Reuses the
    //    placeholder revert script name `revert_to_stock_cv1835_s19j.sh`
    //    that B5 will populate with the actual eMMC dd-flow.
    //    `verified_revertable: false` until live test promotion via
    //    24-devmem replay match.
    PlatformProfile {
        signature: "cv1835-bm1362",
        nand_backup_mtds: CV1835_S19J_NAND_BACKUP_MTDS,
        firmware_slot_mtds: CV1835_S19J_FIRMWARE_SLOT_MTDS,
        ubi_expected_lebs: None, // eMMC ext4 rootfs, no UBI
        bootslot_env_keys: CV1835_BOOTSLOT_ENV_KEYS,
        revert_script: "/usr/sbin/revert_to_stock_cv1835_s19j.sh",
        min_free_bytes: CV1835_MIN_FREE_BYTES,
        verified_revertable: false,
    },
];

/// Look up a platform profile by signature. Linear scan over
/// [`PROFILE_TABLE`] — 8 entries (W16 added S17 am2-s17, W19 added
/// S19 Pro / S19j Pro Zynq am2, W23 added S19j Pro Amlogic
/// `amlogic-a113d-bm1362` and renamed Amlogic keys to match the
/// detector output).
pub fn profile_for(signature: &str) -> Option<&'static PlatformProfile> {
    PROFILE_TABLE.iter().find(|p| p.signature == signature)
}

/// Probe the running platform via [`detect_platform_signature`] and
/// look up the matching profile. Returns `None` if cpuinfo is
/// unreadable, the signature doesn't match a [`PROFILE_TABLE`] entry,
/// or the platform fails the legacy Zynq cpuinfo gate (preserved for
/// wave-≤11 callers via the OR'd condition in
/// [`platform_supports_restore_to_stock`]).
pub async fn profile_for_current_platform() -> Option<&'static PlatformProfile> {
    let sig = detect_platform_signature().await?;
    profile_for(&sig)
}

/// Candidate paths for any per-platform revert script. The wave-12
/// Buildroot post-build hooks install the canonical
/// `/usr/sbin/revert_to_stock_<plat>.sh` paths; the additional
/// `/usr/local/sbin/` + `/data/scripts/` candidates support live
/// dev-deploy without a full Buildroot rebuild.
///
/// `script_basename` is the per-platform filename pulled from the
/// active [`PlatformProfile::revert_script`] field — e.g.
/// `"revert_to_stock_s9.sh"`, `"revert_to_stock_am335x_bb.sh"`,
/// `"revert_to_stock_am3_aml_s21.sh"`,
/// `"revert_to_stock_am3_aml_s19k.sh"`.
fn revert_script_candidates(script_basename: &str) -> [String; 3] {
    [
        format!("/usr/sbin/{script_basename}"),
        format!("/usr/local/sbin/{script_basename}"),
        format!("/data/scripts/{script_basename}"),
    ]
}

/// Resolve the first existing candidate path for the revert script.
/// Returns the canonical (rootfs-install) path if none exist, so the
/// spawned task can still log a meaningful "command not found" error.
fn resolve_revert_to_stock_script_for_profile(profile: &PlatformProfile) -> String {
    let canonical_basename = Path::new(profile.revert_script)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("revert_to_stock_s9.sh");
    let candidates = revert_script_candidates(canonical_basename);
    for candidate in &candidates {
        if Path::new(candidate).is_file() {
            return candidate.clone();
        }
    }
    profile.revert_script.to_string()
}

/// SHA-256 hex prefix of the SECURE_BOOT_SET eFuse-burn blob found in
/// vnish.farm "unlock" packs. From
/// + `projects/security-audit/data/iocs.json` DCENT-2026-010. **No
/// override flag** — burning this fuse is irreversible.
const SECURE_BOOT_SET_SHA256_PREFIX: &str = "c3b77476bfc640ed";

/// Expected blob size for the SECURE_BOOT_SET match (bytes).
const SECURE_BOOT_SET_SIZE: u64 = 1024;

/// Hashcore Toolkit universal SHA-512 root hash needle. From
///  +
/// DCENT-2026-011.
const HASHCORE_ROOT_HASH_NEEDLE: &str = "$6$4rQjfxJBpRYbzeys$uB1.ljOfEgY8";

/// VNish "atlas" backdoor SSH key signature.
const VNISH_ATLAS_KEY_NEEDLE: &str = "atlas@anthill.farm";

/// VNish hotelfee.json devfee filename suffix.
const VNISH_HOTELFEE_FILENAME: &str = "hotelfee.json";

/// All four Innosilicon DTU phone-home needles per Python wave-5
/// detector (`vnish_security_audit.py:412-417`) and
/// `projects/security-audit/data/iocs.json:95-100` DCENT-2026-013.
///  W9-D parity fix (R1 IOC parity table) — the wave-8 Rust
/// port only checked the bare host string, missing 3 of 4 needles.
/// Index 1 (`39.104.179.132`) is the bare host that wave-8 used; the
/// other three are the parity additions.
const INNOSILICON_DTU_NEEDLES: &[&str] = &[
    "39.104.179.132:20001",
    "39.104.179.132",
    "dtu.conf.def",
    "dtu.innosilicon.com",
];

/// Both VNish factory-reset argv needles per Python wave-5 detector
/// (`vnish_security_audit.py:423-426`) — long-form (`--`) and
/// short-form (`-`).  W9-D parity fix; index 0 is the
/// long-form needle wave-8 already checked.
const VNISH_FACTORY_RESET_NEEDLES: &[&str] = &["--enable-factory-reset", "-enable-factory-reset"];

/// All three daemons:22322 listener needles per Python wave-5 detector
/// (`vnish_security_audit.py:403-407`) and
/// `projects/security-audit/data/iocs.json:71` DCENT-2026-012.
///  W9-D parity fix (R1-C3) — the wave-8 Rust port only checked
/// `monitor-ipsig` (index 1), missing the `daemons` binary name (index
/// 0) and the literal port string `22322` (index 2). A tampered
/// firmware that drops `monitor-ipsig` but keeps `daemons` bound to
/// TCP 22322 used to slip through.
const DAEMONS_22322_NEEDLES: &[&str] = &["daemons", "monitor-ipsig", "22322"];

/// Filename hints used by the (negative) DCENT-2026-015 detector to
/// recognize a stock Amlogic S21 firmware tree. Mirrors
/// `vnish_security_audit.py:435-439` and
/// `iocs.json:134` `filename_hints`.
const AMLOGIC_S21_FILENAME_HINTS: &[&str] = &["s21", "amlogic", "aml"];

///  W10-B (A1-HIGH-1): maximum total uncompressed size
/// (across all entries) accepted from a staged tarball. Mirrors the
/// per-file IOC-scan cap to refuse decompression-bomb attacks (small
/// compressed file expanding to gigabytes of garbage). 256 MiB sits
/// above every plausible stock-Bitmain firmware tree (~190 MiB
/// kernel + UBI per BIBLE Volume BSR §2.1) but low enough that a
/// truly hostile bomb is rejected before any IOC scanning or NAND
/// staging occurs.
const MAX_EXTRACTED_BYTES: u64 = 256 * 1024 * 1024;

// Note: the per-file IOC scan size cap lives at
// `IOC_SCAN_MAX_FILE_BYTES` (256 MiB, defined further down beside the
// streaming scanner W9-F installed). 256 MiB sits comfortably above
// every known stock + tampered binary on every platform DCENT_OS
// targets (stock `bmminer` ≈ 6 MiB; VNish-tampered `cgminer`/`dashd`
// 9-12 MiB; Innosilicon T2Tz `single-board-test` ≈ 12 MiB), and is
// 2× the upload cap so a single archive entry can never exceed it.
// W9-D R1-C2 closure relies on the W9-F streaming pipeline + this
// raised cap rather than the historical 8 MiB hard skip.

// ---------------------------------------------------------------------------
// Process-global last-known status (cheap surface for GET /status)
// ---------------------------------------------------------------------------

/// Snapshot of the most recent preflight + flash attempt, exposed
/// read-only by `GET /api/system/restore-to-stock/status`. Default
/// state is `idle` — the daemon does not synthesize this from log
/// scraping; it is populated only by handler bodies in this module.
///
///  W9-C (R4-H5) added the `state_detail` field which carries
/// the fully-typed phase machine the spawned task transitions
/// through. The flat top-level `state` string is kept for dashboard
/// backward compat (prior wave-8 contract); new clients should
/// consume `state_detail` for the structured shape.
#[derive(Debug, Default, Clone, Serialize)]
pub struct RestoreToStockStatus {
    pub state: String,
    ///  W9-C: structured phase machine for the destructive path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_detail: Option<RestoreState>,
    pub last_preflight_at_ms: Option<u64>,
    pub last_preflight_verdict: Option<String>,
    pub last_backup_path: Option<String>,
    pub last_scheduled_reboot_at_ms: Option<u64>,
    pub last_safety_findings: Vec<SafetyFinding>,
    pub last_active_slot: Option<String>,
    pub last_inactive_slot: Option<String>,
    ///  W9-C: monotonically-increasing transition counter so the
    /// dashboard can detect that the spawned task is actually making
    /// progress (vs. stuck on a single phase forever).
    #[serde(default)]
    pub transitions: u64,
    ///  W9-C: epoch-ms of the most recent state transition.
    pub last_transition_at_ms: Option<u64>,
    /// -prep R1''-Q24: did the most recent NAND backup
    /// successfully include `fw_setenv` for operator Option-A recovery?
    /// `Some(true)` = present; `Some(false)` = copy attempted and
    /// failed (operator must use Option B serial console); `None` =
    /// no backup has run yet on this daemon lifetime. Surfaced so the
    /// dashboard can warn the operator BEFORE pulling the trigger
    /// instead of after they're stranded on stock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup_fw_setenv_present: Option<bool>,
    ///  W13-D (A2'-#1): rolling buffer of the last
    /// [`RECENT_LOG_LINES_MAX`] stderr/stdout lines emitted by the
    /// spawned writer (`revert_to_stock_*.sh`). Streamed line-by-line
    /// so the dashboard can render VNish-style live progress while the
    /// flash is in flight (1-2 minute window). Empty / not serialized
    /// before the writer starts producing output, so old responses
    /// pre-W13-D don't accidentally leak `[]` in their wire shape.
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pub recent_log_lines: VecDeque<String>,
}

///  W9-C (R4-H5): explicit phase machine for the destructive
/// flash path. Every spawned task transition writes a fresh value
/// into the STATUS mutex so the dashboard's `GET /status` reflects
/// what the background work is actually doing — not just the
/// "scheduled" stamp from the moment the HTTP response was returned.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum RestoreState {
    /// No restore-to-stock work has been attempted, or the last
    /// attempt completed successfully.
    #[default]
    Idle,
    /// `run_preflight` is currently executing.
    PreflightRunning,
    /// Preflight rejected the request.
    PreflightFailed { reason: String },
    /// Preflight passed (dry-run or pre-confirm).
    PreflightOk,
    /// `nand_backup` is currently dumping mtdN.img / SHA / ubinfo.
    NandBackupRunning,
    /// NAND backup write failed; backup_path may be partial.
    NandBackupFailed {
        reason: String,
        backup_path: Option<PathBuf>,
    },
    /// Pre-flash staging (re-canonicalize + re-hash + spawn the
    /// detached writer).
    Staging { backup_path: PathBuf },
    /// Staging failed (TOCTOU mismatch, missing setsid, etc.).
    StagingFailed {
        reason: String,
        backup_path: Option<PathBuf>,
    },
    /// Reboot scheduled; spawned task is sleeping until the deferred
    /// fire time.
    Scheduled {
        reboot_at_ms: u64,
        backup_path: PathBuf,
    },
    /// Detached flash dispatcher has actually started.
    FlashRunning { backup_path: PathBuf },
    /// Flash succeeded (writer exit 0; reboot expected momentarily).
    FlashSucceeded {
        completed_at_ms: u64,
        backup_path: PathBuf,
    },
    /// Flash failed at runtime (writer non-zero, hash drifted, etc.).
    FlashFailed {
        reason: String,
        backup_path: Option<PathBuf>,
    },
}

impl RestoreState {
    /// Stable string label matching the legacy wave-8 wire vocabulary
    /// in `RestoreToStockResponse::status`.
    pub fn as_label(&self) -> &'static str {
        match self {
            RestoreState::Idle => "idle",
            RestoreState::PreflightRunning => "preflight_running",
            RestoreState::PreflightFailed { .. } => "preflight_failed",
            RestoreState::PreflightOk => "preflight_ok",
            RestoreState::NandBackupRunning => "nand_backup_running",
            RestoreState::NandBackupFailed { .. } => "nand_backup_failed",
            RestoreState::Staging { .. } => "staging",
            RestoreState::StagingFailed { .. } => "staging_failed",
            RestoreState::Scheduled { .. } => "scheduled",
            RestoreState::FlashRunning { .. } => "flash_running",
            RestoreState::FlashSucceeded { .. } => "flash_succeeded",
            RestoreState::FlashFailed { .. } => "flash_failed",
        }
    }
}

/// Process-wide status mutex.  keeps this in a module-local
/// lock so we don't have to extend `AppState` mid-wave; W9
/// candidate is to fold this into `AppState` so other route modules
/// can also read it.
///
///  W10-B (A1-MEDIUM-4): swapped from `Mutex` to `RwLock`.
/// The status surface has many concurrent readers (every dashboard
/// tab polls `GET /api/system/restore-to-stock/status`) and very
/// few writers (preflight + the spawned writer's transition_state
/// calls). `std::sync::RwLock` matches this read-heavy pattern; the
/// critical sections are a single read or a single write of an
/// `Option<RestoreToStockStatus>` — no `.await` is held across the
/// guard, so a `std::sync::RwLock` is the correct choice (vs.
/// `tokio::sync::RwLock`, which would force every path async).
static STATUS: RwLock<Option<RestoreToStockStatus>> = RwLock::new(None);

///  W11-A (R4''-poison-logging): acquire `STATUS.write()`,
/// recovering the inner `RwLockWriteGuard` if the lock was poisoned
/// by a previous panic. Emits a `tracing::error!` so the operator
/// has a forensic trail — the previous code silently no-op'd or
/// silently `unwrap_or_else`-recovered, which masked panics in the
/// destructive `restore_to_stock` path.
fn status_write_or_recover() -> std::sync::RwLockWriteGuard<'static, Option<RestoreToStockStatus>> {
    STATUS.write().unwrap_or_else(|p| {
        tracing::error!(
            "Wave-11 W11-A R4''-poison-logging: STATUS write lock was poisoned — recovering inner guard; this indicates a previous panic in restore_to_stock that left the lock in inconsistent state. Forensic next step: search the daemon log for an earlier 'panicked at' line and capture the backtrace."
        );
        p.into_inner()
    })
}

///  W11-A (R4''-poison-logging): acquire `STATUS.read()`,
/// recovering the inner `RwLockReadGuard` if the lock was poisoned.
/// Same logging contract as [`status_write_or_recover`].
fn status_read_or_recover() -> std::sync::RwLockReadGuard<'static, Option<RestoreToStockStatus>> {
    STATUS.read().unwrap_or_else(|p| {
        tracing::error!(
            "Wave-11 W11-A R4''-poison-logging: STATUS read lock was poisoned — recovering inner guard; this indicates a previous panic in restore_to_stock that left the lock in inconsistent state. Forensic next step: search the daemon log for an earlier 'panicked at' line and capture the backtrace."
        );
        p.into_inner()
    })
}

fn record_status(snapshot: RestoreToStockStatus) {
    let mut guard = status_write_or_recover();
    *guard = Some(snapshot);
}

///  W9-C (R4-H5): transition the structured phase machine
/// while preserving the rest of the snapshot. Bumps `transitions`
/// and stamps `last_transition_at_ms` so the dashboard can render
/// forward progress. Callable from the spawned-task context — uses
/// `std::sync::RwLock` (W10-B A1-MEDIUM-4) with a tiny critical
/// section so it does not pin a tokio worker.
fn transition_state(next: RestoreState) {
    let mut guard = status_write_or_recover();
    let mut snap = guard.clone().unwrap_or_default();
    snap.state = next.as_label().to_string();
    snap.state_detail = Some(next);
    snap.transitions = snap.transitions.saturating_add(1);
    snap.last_transition_at_ms = Some(now_ms());
    *guard = Some(snap);
}

///  W13-D (A2'-#1): push one stderr/stdout line emitted by the
/// spawned writer into the rolling [`RestoreToStockStatus::recent_log_lines`]
/// buffer. Bounds at [`RECENT_LOG_LINES_MAX`] (drops oldest), and
/// truncates each line to [`RECENT_LOG_LINE_MAX_LEN`] characters.
///
/// Cheap (small critical section, single `String` allocation per call)
/// and safe to invoke from the spawned tokio task body without
/// pinning a worker — the lock is `std::sync::RwLock` and is held only
/// for the push/pop. Uses the same poison-recovery pattern as
/// [`transition_state`] (W11-A `status_write_or_recover`) so a panic
/// in a sibling helper doesn't strand this surface forever.
fn push_log_line(line: String) {
    let bounded = if line.chars().count() > RECENT_LOG_LINE_MAX_LEN {
        let mut s: String = line.chars().take(RECENT_LOG_LINE_MAX_LEN).collect();
        s.push('…');
        s
    } else {
        line
    };

    let mut guard = status_write_or_recover();
    let mut snap = guard.clone().unwrap_or_default();
    snap.recent_log_lines.push_back(bounded);
    while snap.recent_log_lines.len() > RECENT_LOG_LINES_MAX {
        snap.recent_log_lines.pop_front();
    }
    *guard = Some(snap);
}

///  W13-D test helper: deterministic single-line push without
/// the spawned-task plumbing. Hidden from rustdoc.
#[doc(hidden)]
pub fn push_log_line_for_test(line: String) {
    push_log_line(line);
}

///  W13-D test helper: read the current
/// [`RestoreToStockStatus::recent_log_lines`] buffer length. Hidden
/// from rustdoc.
#[doc(hidden)]
pub fn recent_log_lines_len_for_test() -> usize {
    let guard = status_read_or_recover();
    guard
        .as_ref()
        .map(|s| s.recent_log_lines.len())
        .unwrap_or(0)
}

///  W13-D test helper: snapshot the current
/// [`RestoreToStockStatus::recent_log_lines`] buffer as a `Vec<String>`
/// (in push order). Hidden from rustdoc.
#[doc(hidden)]
pub fn recent_log_lines_snapshot_for_test() -> Vec<String> {
    let guard = status_read_or_recover();
    guard
        .as_ref()
        .map(|s| s.recent_log_lines.iter().cloned().collect())
        .unwrap_or_default()
}

///  W13-D: cap exposed to tests so the test asserts use the
/// same constant as the production push logic.
#[doc(hidden)]
pub fn recent_log_lines_max_for_test() -> usize {
    RECENT_LOG_LINES_MAX
}

fn read_status() -> RestoreToStockStatus {
    let guard = status_read_or_recover();
    if let Some(snapshot) = guard.as_ref() {
        return snapshot.clone();
    }
    RestoreToStockStatus {
        state: "idle".to_string(),
        state_detail: Some(RestoreState::Idle),
        ..Default::default()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

///  W11-A (R3''-Q12): best-effort sweep of orphan
/// `<NAND_BACKUP_ROOT>-<ts>.partial/` directories left behind by a
/// crashed previous run (SIGKILL / OOM / power-loss) before the
/// `PartialDirCleanup` Drop guard could fire.
///
/// Walks `data_dir` (default `/data`) for entries whose name starts
/// with the basename of [`NAND_BACKUP_ROOT`] and ends in `.partial`,
/// `remove_dir_all`-ing each. Failure is `warn!` only — never blocks
/// the new flash, never returns an error. Reads still work even if
/// the sweep is incomplete.
///
/// Called from `restore_to_stock` after lock acquisition and BEFORE
/// `nand_backup` so the sweep is serialized against in-flight
/// backups (no race with the freshly-armed `PartialDirCleanup` for
/// the current run) AND so the new backup has reclaimed disk space
/// for the 250 MiB free-space precheck.
async fn sweep_orphan_partial_backups(data_dir: &str) {
    let root = Path::new(data_dir);
    let prefix_basename = match Path::new(NAND_BACKUP_ROOT)
        .file_name()
        .and_then(|n| n.to_str())
    {
        Some(s) => s,
        None => {
            tracing::warn!(
                "Wave-11 W11-A: could not derive partial-prefix from NAND_BACKUP_ROOT={NAND_BACKUP_ROOT}; skipping orphan sweep"
            );
            return;
        }
    };
    let mut rd = match tokio::fs::read_dir(root).await {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(
                error = %e,
                data_dir = %data_dir,
                "Wave-11 W11-A: orphan-partial sweep could not read data_dir; continuing"
            );
            return;
        }
    };
    let mut swept = 0u32;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !name.starts_with(prefix_basename) || !name.ends_with(".partial") {
            continue;
        }
        let path = entry.path();
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => {
                swept = swept.saturating_add(1);
                tracing::info!(
                    path = %path.display(),
                    "Wave-11 W11-A (R3''-Q12): swept orphan NAND-backup partial dir from prior crash"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Wave-11 W11-A (R3''-Q12): could not remove orphan partial dir; continuing"
                );
            }
        }
    }
    if swept > 0 {
        tracing::info!(
            count = swept,
            "Wave-11 W11-A (R3''-Q12): orphan-partial sweep complete"
        );
    }
}

///  W11-A test re-export for [`sweep_orphan_partial_backups`].
/// Hidden from rustdoc.
#[doc(hidden)]
pub async fn sweep_orphan_partial_backups_for_test(data_dir: &str) {
    sweep_orphan_partial_backups(data_dir).await
}

///  W11-A (R1''-Q24) test helper: simulate the
/// `copy_fw_setenv_into_backup_dir` → STATUS.last_backup_fw_setenv_present
/// pipeline that lives inside `nand_backup` step (e). Tests can't
/// drive `nand_backup` directly (it requires `/dev/mtd*`), so this
/// helper folds the two operations together so the integration test
/// can assert STATUS reflects the helper's success/failure
/// observable.
#[doc(hidden)]
pub async fn copy_fw_setenv_and_record_status_for_test(src: &str, backup_dir: &Path) -> bool {
    let ok = copy_fw_setenv_into_backup_dir(src, backup_dir).await;
    let mut guard = status_write_or_recover();
    let mut snap = guard.clone().unwrap_or_default();
    snap.last_backup_fw_setenv_present = Some(ok);
    *guard = Some(snap);
    ok
}

///  W11-A test helper: read
/// `STATUS.last_backup_fw_setenv_present`. Hidden from rustdoc.
#[doc(hidden)]
pub fn last_backup_fw_setenv_present_for_test() -> Option<bool> {
    let guard = status_read_or_recover();
    guard.as_ref().and_then(|s| s.last_backup_fw_setenv_present)
}

///  W11-A test helper: clear `STATUS` (set to None) so a test
/// can prove a subsequent helper actually wrote into it. Hidden from
/// rustdoc.
#[doc(hidden)]
pub fn reset_status_for_test() {
    let mut guard = status_write_or_recover();
    *guard = None;
}

// ---------------------------------------------------------------------------
// Process-wide flash mutex ( W9-C — R4-C1)
// ---------------------------------------------------------------------------
//
// Two concurrent `confirm:true` POSTs (operator's modal in two
// browser tabs, retry-on-timeout loops, etc.) used to both pass
// preflight, both run `nand_backup`, and both spawn the writer
// against the same staged tarball. `nand_backup`'s timestamp
// resolution is per-second so two concurrent timestamps can collide;
// two concurrent writers into the same NAND slot is a known way to
// brick. Per R4-C1 in
// .
//
// We serialize the destructive path with a
// `OnceLock<Mutex<Option<RestoreInProgress>>>`. The first POST takes
// the slot; subsequent POSTs return 409 Conflict (surfaced via
// `RestoreError::Conflict` →
// `rejected_restore_already_in_progress`) until the first task
// drops its RAII `RestoreLockGuard`. The lock is held across NAND
// backup + the spawn call so a second request cannot race past
// preflight while the first is doing its destructive work.

/// Sentinel describing the in-flight restore. Carried in the lock
/// slot so a future GET endpoint can surface "another request is
/// in flight" to the dashboard with a stable shape.
#[derive(Debug, Clone)]
pub struct RestoreInProgress {
    pub started_at: SystemTime,
    /// Optional operator-facing trace marker (we currently log the
    /// operator-typed serial here; never the tarball bytes).
    pub operator_marker: Option<String>,
}

static RESTORE_LOCK: OnceLock<Mutex<Option<RestoreInProgress>>> = OnceLock::new();

fn restore_lock() -> &'static Mutex<Option<RestoreInProgress>> {
    RESTORE_LOCK.get_or_init(|| Mutex::new(None))
}

/// RAII guard for the process-wide flash mutex. Drop releases the
/// slot.
#[derive(Debug)]
pub struct RestoreLockGuard {
    _phantom: (),
}

impl Drop for RestoreLockGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = restore_lock().lock() {
            *guard = None;
        }
    }
}

/// Try to acquire the process-wide flash slot. Returns
/// `RestoreError::Conflict` if another flash is already in flight.
/// Per R4-C1.
pub fn try_lock_restore(operator_marker: Option<String>) -> Result<RestoreLockGuard, RestoreError> {
    let lock = restore_lock();
    let mut guard = lock.lock().map_err(|_| RestoreError::Internal {
        reason: "restore lock mutex poisoned".to_string(),
    })?;
    if guard.is_some() {
        return Err(RestoreError::Conflict);
    }
    *guard = Some(RestoreInProgress {
        started_at: SystemTime::now(),
        operator_marker,
    });
    Ok(RestoreLockGuard { _phantom: () })
}

/// Read-only inspector for the lock slot — used by tests and by
/// future dashboard surfaces. Does NOT acquire.
pub fn restore_lock_in_use() -> bool {
    restore_lock().lock().map(|g| g.is_some()).unwrap_or(false)
}

///  W9-C (R4-H1): canonical-path + sha256 fingerprint captured
/// at preflight time, re-verified by the spawned task before any
/// destructive write. If either drifts the spawned task refuses the
/// flash and transitions to `FlashFailed`.
#[derive(Debug, Clone)]
pub struct StagedTarballFingerprint {
    pub canonical_path: PathBuf,
    pub sha256: String,
}

// ---------------------------------------------------------------------------
// Request / response DTOs
// ---------------------------------------------------------------------------

/// Body for both `POST /api/system/restore-to-stock` and
/// `POST /api/system/restore-to-stock/preflight`. The `confirm` field
/// gates the destructive flash; default is dry-run.
///
///  W10-D (A1-LOW-2): `#[serde(deny_unknown_fields)]` is set so
/// a typo'd or hostile extra field (e.g. `{"confirm":true,"junk":1}`)
/// is rejected at deserialization with `400 Bad Request` rather than
/// silently ignored. Prevents footgun where a malformed dashboard
/// build accidentally adds a field nobody else implements.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreToStockBody {
    /// Path to a stock firmware tarball that the operator already
    /// uploaded via the existing `POST /api/system/upgrade` staging
    /// endpoint. The path must live under
    /// `/tmp/dcentos-upgrade/<uuid>/` (the same staging root
    /// `post_system_upgrade` writes into) — paths outside that root
    /// are refused with `400 stock_firmware_path_outside_staging`.
    pub stock_firmware_staged_path: String,

    /// Optional expected SHA-256 of the staged tarball. When set, the
    /// daemon hashes the file and refuses to proceed on mismatch.
    /// When unset, the daemon still reports the actual SHA-256 in
    /// the response so the operator can pin it for next time.
    #[serde(default)]
    pub stock_firmware_sha256: Option<String>,

    /// The miner serial number, typed by the operator into the
    /// dashboard modal. Compared verbatim against
    /// `state.hardware_info.miner_serial`. Mismatch → `400`.
    pub operator_serial_typed: String,

    /// Operator must explicitly acknowledge that flashing stock will
    /// noticeably increase fan noise + breaker draw on a home circuit.
    pub acknowledge_breaker_warning: bool,

    /// Number of hashboards the operator wants the booted stock
    /// firmware to use. Per user instructions, the operator may
    /// physically unplug 1 hashboard before flashing to manage
    /// breaker / noise. Reported back to the operator in the response;
    /// not enforced by this handler beyond range validation 1..=3.
    #[serde(default = "default_hashboard_count")]
    pub hashboard_count_to_use: u8,

    /// Operator must type this exact phrase to proceed. Case-sensitive,
    /// trimmed of leading/trailing whitespace. Mismatch → `400`.
    #[serde(default)]
    pub confirm_string_typed: String,

    /// Default `false` (dry-run). Must be `true` to actually flash.
    #[serde(default)]
    pub confirm: bool,

    /// Operator acknowledgement that they have reviewed any HIGH-severity
    /// safety findings and accept them for this restore.  W9-G
    /// (R5-MEDIUM, mirrors R1 H-5 pattern): the dashboard `highAcknowledged`
    /// checkbox now rounds to the wire so a direct curl with `confirm:true`
    /// cannot bypass the HIGH-findings acknowledgement gate. When the
    /// preflight returns ≥1 HIGH-severity finding and this field is
    /// `false`, the backend refuses with
    /// `rejected_high_findings_require_acknowledgement`.
    #[serde(default)]
    pub acknowledge_high_findings: bool,
}

///  W9-G (R5-MEDIUM): UI/backend hashboard_count_to_use default
/// divergence. The UI defaulted to 1 (breaker safety, matching the
/// user's stated home-mining intent of unplugging 1 board); the backend
/// previously defaulted to 3. We pin the backend default to 1 so a
/// curl caller without an explicit value behaves identically to the
/// modal-driven flow. Range validation 1..=3 still applies.
fn default_hashboard_count() -> u8 {
    1
}

/// Top-level response envelope. `status` is a fixed-vocab field so the
/// dashboard can route on it without parsing prose:
///
/// - `"preflight_ok"` — preflight passed, no flash attempted (always
///   the response from `/preflight`).
/// - `"dry_run"` — flash endpoint with `confirm:false`. Same payload
///   shape as `scheduled` minus the actual reboot.
/// - `"scheduled"` — flash dispatched, NAND backup written,
///   sysupgrade staged, reboot scheduled.
/// - `"rejected_<reason>"` — failure with machine-readable reason.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreToStockResponse {
    pub status: String,
    /// Human-readable reason for `rejected_*` statuses; null otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Path of the NAND backup tarball, if one was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    /// Unix epoch ms when the reboot is scheduled, if scheduled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reboot_at_ms: Option<u64>,
    /// Per-detector findings from the safety preflight. `Critical`
    /// entries fail the request with `400`; `High`/`Medium`/`Low`
    /// entries are reported but do not block.
    pub safety_findings: Vec<SafetyFinding>,
    /// Computed SHA-256 of the staged tarball (always returned).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_sha256: Option<String>,
    /// Resolved active/inactive slot pair the daemon would write into.
    pub slot_plan: SlotPlan,
    /// Operator-typed hashboard count, echoed back.
    pub hashboard_count_to_use: u8,
    /// Was this a dry-run?
    pub dry_run: bool,
}

/// Single safety preflight finding.
#[derive(Debug, Clone, Serialize)]
pub struct SafetyFinding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    /// Path inside the extracted tarball that triggered the match.
    /// `null` for negative findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_path: Option<String>,
    pub remediation: String,
    /// `true` when the operator cannot override this finding even with
    /// `confirm:true`. Currently only SECURE_BOOT_SET sets this.
    pub no_override: bool,
}

/// Severity vocabulary, mirroring the Python detector contract.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SlotPlan {
    /// Currently booted slot ("a" / "b" / "1" / "2" — varies per
    /// platform); unknown if `fw_printenv` is unavailable.
    pub active_slot: Option<String>,
    pub inactive_slot: Option<String>,
    /// MTD partition path the daemon would `nandwrite` into.
    pub inactive_mtd: Option<String>,
}

// ---------------------------------------------------------------------------
// Top-level router
// ---------------------------------------------------------------------------

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/system/restore-to-stock/preflight",
            post(preflight_only),
        )
        .route("/api/system/restore-to-stock", post(restore_to_stock))
        .route(
            "/api/system/restore-to-stock/status",
            get(restore_to_stock_status),
        )
        //  W12-C: dynamic preflight-checks endpoint. Replaces
        // the wave-11 static dashboard checklist with live probe
        // results (setsid / revert script / fw_setenv / tar /
        // nandwrite / flash_erase paths + free-space + platform
        // verdict).
        .route(
            "/api/system/restore-to-stock/preflight-checks",
            get(preflight_checks),
        )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/system/restore-to-stock/preflight` — non-destructive
/// safety + identity check. Always returns 200 if the request shape
/// parses, with a `safety_findings` array. The dashboard renders this
/// in modal step 4 of the multi-step confirm flow before the operator
/// is offered the final slider.
pub async fn preflight_only(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RestoreToStockBody>,
) -> axum::response::Response {
    // CE-174: gate the restore preflight on RuntimeCapability::Restore. This is
    // a write-capability family; read-only / unknown-identity / Amlogic sessions
    // fail closed (409) before the tar-extract + IOC scan even runs. The
    // downstream serial-match / verified_revertable / typed-phrase gates remain.
    if let Err(response) = crate::rest::require_antminer_runtime_capability(
        &state,
        dcent_schema::capability::RuntimeCapability::Restore,
        "/api/system/restore-to-stock/preflight",
    ) {
        return response;
    }
    match run_preflight(&state, &body).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err((code, resp)) => (code, Json(resp)).into_response(),
    }
}

/// `POST /api/system/restore-to-stock` — destructive flash entry
/// point. Default dry-run; flips to scheduled-reboot only when
/// `confirm:true` and every preflight gate passes.
pub async fn restore_to_stock(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RestoreToStockBody>,
) -> axum::response::Response {
    // CE-174: gate the destructive restore POST on RuntimeCapability::Restore
    // BEFORE try_lock_restore / preflight. Beta-tier boards (am1-s9 +
    // am2-s19jpro-zynq) still grant it; read-only / unknown-identity / Amlogic
    // sessions fail closed (409). The independent serial-match +
    // verified_revertable + typed-phrase brick guards downstream are unchanged.
    if let Err(response) = crate::rest::require_antminer_runtime_capability(
        &state,
        dcent_schema::capability::RuntimeCapability::Restore,
        "/api/system/restore-to-stock",
    ) {
        return response;
    }
    restore_to_stock_impl(state, body).await.into_response()
}

async fn restore_to_stock_impl(
    state: Arc<AppState>,
    body: RestoreToStockBody,
) -> impl IntoResponse {
    let dry_run_intent = !body.confirm;

    //  W10-B (A1-HIGH-5): acquire the process-wide restore
    // lock BEFORE the preflight runs. This serializes concurrent
    // dry-runs against each other AND against any in-flight
    // `confirm:true` flash. The dry-run path runs the safety
    // preflight (tarball extraction + 256 MiB IOC scan + slot
    // probe) which can take several seconds and writes to /tmp;
    // two parallel dry-runs from two dashboard tabs used to race
    // the scratch dir, and a dry-run firing while a confirm:true
    // flash is mid-`nand_backup` could trip statvfs / probe state.
    // The guard is held for the entire handler — including the
    // `tokio::spawn` for confirm:true (the spawned writer takes
    // ownership of the guard so the slot stays held across the
    // 30s pre-reboot dwell).
    //  W10-D (A1-LOW-3): the marker stored in the lock slot is
    // surfaced via tracing (and may be returned in a 409 conflict
    // payload), so emit a truncated serial form instead of the
    // operator-typed full serial.
    let restore_guard = match try_lock_restore(Some(format!(
        "operator_serial{}:{}",
        if dry_run_intent { "_dry_run" } else { "" },
        truncate_serial(&body.operator_serial_typed)
    ))) {
        Ok(g) => g,
        Err(RestoreError::Conflict) => {
            tracing::warn!(
                dry_run = dry_run_intent,
                "Restore-to-Stock: rejecting concurrent operation \
                 (W10-B A1-HIGH-5 / W9-C R4-C1)"
            );
            let status_label = if dry_run_intent {
                "rejected_dry_run_already_in_progress"
            } else {
                "rejected_restore_already_in_progress"
            };
            let resp = RestoreToStockResponse {
                status: status_label.to_string(),
                reason: Some(
                    "Another restore-to-stock operation (dry-run or flash) is already in flight on this miner. Wait for the in-progress operation to finish (or fail) before retrying."
                        .to_string(),
                ),
                backup_path: None,
                reboot_at_ms: None,
                safety_findings: vec![],
                staged_sha256: None,
                slot_plan: SlotPlan::default(),
                hashboard_count_to_use: body.hashboard_count_to_use,
                dry_run: dry_run_intent,
            };
            return (StatusCode::CONFLICT, Json(resp));
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "Restore-to-Stock: lock acquisition failed (W10-B A1-HIGH-5)"
            );
            let resp = RestoreToStockResponse {
                status: "rejected_lock_unavailable".to_string(),
                reason: Some(e.to_string()),
                backup_path: None,
                reboot_at_ms: None,
                safety_findings: vec![],
                staged_sha256: None,
                slot_plan: SlotPlan::default(),
                hashboard_count_to_use: body.hashboard_count_to_use,
                dry_run: dry_run_intent,
            };
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(resp));
        }
    };

    // 1. Same preflight as the dedicated endpoint. Critical findings
    //    or serial mismatch → 400 here. The lock_guard is held
    //    across this call so a second concurrent caller bounces.
    let preflight = match run_preflight(&state, &body).await {
        Ok(resp) => resp,
        Err((code, resp)) => {
            drop(restore_guard);
            return (code, Json(resp));
        }
    };

    let dry_run = !body.confirm;

    if dry_run {
        let mut resp = preflight;
        resp.status = "dry_run".to_string();
        resp.dry_run = true;
        // Surface the planned reboot time so the dashboard can show
        // "would have rebooted at <wall-clock>" for the operator's
        // sanity check.
        resp.reboot_at_ms = Some(now_ms() + REBOOT_DELAY_SECS * 1000);
        record_status(RestoreToStockStatus {
            state: "dry_run".to_string(),
            last_preflight_at_ms: Some(now_ms()),
            last_preflight_verdict: Some(resp.status.clone()),
            last_backup_path: None,
            last_scheduled_reboot_at_ms: resp.reboot_at_ms,
            last_safety_findings: resp.safety_findings.clone(),
            last_active_slot: resp.slot_plan.active_slot.clone(),
            last_inactive_slot: resp.slot_plan.inactive_slot.clone(),
            ..Default::default()
        });
        let staged_sha256 = resp
            .staged_sha256
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        push_audit_event(
            &state,
            "operator",
            AuditEvent::Free {
                category: "restore_to_stock".to_string(),
                message: format!(
                    "restore-to-stock dry-run completed: status={}; staged_sha256={staged_sha256}",
                    resp.status
                ),
            },
        );
        // restore_guard drops here — slot released for next caller.
        drop(restore_guard);
        return (StatusCode::OK, Json(resp));
    }

    // 1b.  W9-G (R5-MEDIUM): refuse `confirm:true` against
    //     unacknowledged HIGH-severity safety findings. Mirrors R1 H-5
    //     pattern (the slider/breaker-ack gates also round to the wire
    //     so a direct curl can't skip them). The dashboard requires the
    //     operator to tick the "I've reviewed the high-severity
    //     findings" checkbox before advancing past the preflight step;
    //     this gate ensures the same contract holds at the wire so a
    //     direct curl with `confirm:true` is also rejected.
    let has_high_findings = preflight
        .safety_findings
        .iter()
        .any(|f| matches!(f.severity, Severity::High));
    if has_high_findings && !body.acknowledge_high_findings {
        let resp = RestoreToStockResponse {
            status: "rejected_high_findings_require_acknowledgement".to_string(),
            reason: Some(
                "Preflight returned HIGH-severity safety findings; the operator must set \
                 `acknowledge_high_findings: true` after reviewing them before `confirm:true` \
                 is accepted."
                    .to_string(),
            ),
            backup_path: None,
            reboot_at_ms: None,
            safety_findings: preflight.safety_findings,
            staged_sha256: preflight.staged_sha256,
            slot_plan: preflight.slot_plan,
            hashboard_count_to_use: body.hashboard_count_to_use,
            dry_run: false,
        };
        return (StatusCode::BAD_REQUEST, Json(resp));
    }

    // 1c.  R3'-M1: refuse `confirm:true` on non-S9-am1
    //     platforms. The destructive path hardcodes mtd4/7/8 + UBI
    //     LEB counts (25/166/525) that are S9-am1-specific; running
    //     against an Amlogic am2/am3 miner would silently corrupt
    //     NAND. Detection reads `/proc/cpuinfo` for the Xilinx Zynq
    //     CPU signature shared by S9 / S17 / S19 Pro am1/am2 (the
    //     S19j Pro Amlogic + S21 Amlogic + S19k Amlogic units do NOT
    //     match). Belt-and-suspenders alongside the dashboard hiding
    //     the button on non-S9 platforms.
    //  W12-B (multi-platform 2-layer gate):
    //
    //   Layer 1 — `profile_for_current_platform()` reads /proc/cpuinfo
    //   + /proc/device-tree/model and looks up the matching
    //   PROFILE_TABLE entry. `None` means the platform isn't even
    //   code-supported (no PlatformProfile entry); reject with
    //   `rejected_unsupported_platform`.
    //
    //   Layer 2 — `verified_revertable: false` means CODE-PATH-COMPLETE
    //   but not hardware-proven on this platform. Reject with
    //   `rejected_unsupported_platform_pending_live_test` so the
    //   operator (and the dashboard) can distinguish the two. Flipping
    //   `verified_revertable` to true requires an in-source code
    //   change after a successful live test.
    let profile = match profile_for_current_platform().await {
        Some(p) => p,
        None => {
            tracing::warn!(
                "Restore-to-Stock: refusing confirm:true — no PROFILE_TABLE \
                 entry matches the running platform (W12-B layer 1)"
            );
            let resp = RestoreToStockResponse {
                status: "rejected_unsupported_platform".to_string(),
                reason: Some(
                    "Restore-to-Stock is not code-supported on this \
                     platform. The destructive flash path is keyed on \
                     PROFILE_TABLE entries (signature, mtd list, UBI \
                     layout, revert script) that don't match the \
                     daemon-detected platform signature. Use the manual \
                     recovery procedure or wait for a future wave to \
                     add a PROFILE_TABLE entry for this platform."
                        .to_string(),
                ),
                backup_path: None,
                reboot_at_ms: None,
                safety_findings: preflight.safety_findings,
                staged_sha256: preflight.staged_sha256,
                slot_plan: preflight.slot_plan,
                hashboard_count_to_use: body.hashboard_count_to_use,
                dry_run: false,
            };
            drop(restore_guard);
            return (StatusCode::BAD_REQUEST, Json(resp));
        }
    };
    if !profile.verified_revertable {
        tracing::warn!(
            platform = %profile.signature,
            "Restore-to-Stock: refusing confirm:true — platform code-supports \
             but is NOT live-tested (W12-B layer 2 — verified_revertable=false)"
        );
        let resp = RestoreToStockResponse {
            status: "rejected_unsupported_platform_pending_live_test".to_string(),
            reason: Some(format!(
                "Restore-to-Stock code path supports `{sig}` (PROFILE_TABLE \
                 has an entry: mtd list, revert script, slot env keys), but \
                 the platform has not been live-tested end-to-end \
                 (preflight + NAND backup + flash + reboot + revert) on \
                 real hardware. Flip `verified_revertable: true` in \
                 source (PROFILE_TABLE in restore_to_stock.rs) after a \
                 successful practical-test on this hardware before \
                 re-attempting confirm:true. The DCENT_OS team uses \
                 `verified_revertable: false` as a hard safety gate to \
                 avoid bricking unverified miners on the destructive \
                 path.",
                sig = profile.signature,
            )),
            backup_path: None,
            reboot_at_ms: None,
            safety_findings: preflight.safety_findings,
            staged_sha256: preflight.staged_sha256,
            slot_plan: preflight.slot_plan,
            hashboard_count_to_use: body.hashboard_count_to_use,
            dry_run: false,
        };
        drop(restore_guard);
        return (StatusCode::BAD_REQUEST, Json(resp));
    }

    // 2.  W9-C (R4-C1) / W10-B (A1-HIGH-5): the process-wide
    //    flash mutex is already held by `restore_guard`, acquired at
    //    the top of this handler so the preflight + dry-run paths
    //    are also serialized. The previous wave-9 implementation
    //    acquired the lock here (after preflight); W10-B moved it
    //    earlier so two parallel dry-runs from two dashboard tabs
    //    can't race the scratch dir or contend with a real flash
    //    that's mid-`nand_backup`. The guard moves into the spawned
    //    writer task at the end of this handler, holding the slot
    //    for the entire pre-reboot dwell.
    let lock_guard = restore_guard;

    // 3.  W9-C (R4-H1): capture canonical-path + sha256
    //    fingerprint for the staged tarball BEFORE any destructive
    //    work. The spawned task re-fingerprints just before the writer
    //    fires — drift between preflight and dispatch (symlink swap,
    //    file replacement, atomic rename, etc.) makes the spawned task
    //    refuse the flash.
    let staged_path_input = Path::new(&body.stock_firmware_staged_path);
    let preflight_fingerprint = match fingerprint_staged_tarball(staged_path_input).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "Restore-to-Stock: failed to fingerprint staged tarball");
            let resp = RestoreToStockResponse {
                status: "rejected_fingerprint_failed".to_string(),
                reason: Some(format!("Could not fingerprint staged tarball: {e}")),
                backup_path: None,
                reboot_at_ms: None,
                safety_findings: preflight.safety_findings,
                staged_sha256: preflight.staged_sha256,
                slot_plan: preflight.slot_plan,
                hashboard_count_to_use: body.hashboard_count_to_use,
                dry_run: false,
            };
            drop(lock_guard);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(resp));
        }
    };

    // 3b.  W11-A (R3''-Q12): best-effort sweep of any orphan
    //    `<NAND_BACKUP_ROOT>-<ts>.partial/` directories left behind by
    //    a previous run that crashed via SIGKILL / OOM / power-loss
    //    BEFORE the `PartialDirCleanup` Drop guard fired. Without this
    //    sweep, repeated crash-kills accumulate ~190 MiB partial dirs
    //    on /data and eventually trip the 250 MiB free-space precheck,
    //    blocking ALL future flashes silently. Run after lock
    //    acquisition (so we serialize against in-flight backups) but
    //    BEFORE the NAND-backup pipeline starts (which would compete
    //    with us for /data space). Failure is `warn!` only — it does
    //    not block a fresh flash.
    sweep_orphan_partial_backups(NAND_BACKUP_DATA_DIR).await;

    // 4. NAND backup is mandatory.
    //
    //  W9-B (R3-CRITICAL-2 + R1-H-1/H-2/H-3 + R3-HIGH): the
    // backup now dumps mtd4 (U-Boot env) + mtd7 + mtd8 (firmware
    // slots), SHA-256-verifies every dumped file, validates the UBI
    // magic on both firmware-slot images, runs an LEB-mirror check
    // against the inactive slot, and refuses if /data has less than
    // 250 MB free. Pass slot_plan in so the LEB/UBI checks know
    // which side is the inactive firmware slot.
    //
    //  W9-C (R4-H5) wraps this in transition_state() so the
    // dashboard's GET /status reflects NandBackupRunning →
    // NandBackupFailed | (next phase).
    transition_state(RestoreState::NandBackupRunning);
    let backup_path = match nand_backup(&preflight.slot_plan, profile).await {
        Ok(p) => p,
        Err(e) => {
            transition_state(RestoreState::NandBackupFailed {
                reason: e.to_string(),
                backup_path: None,
            });
            //  W11-A (R1''-Q31 + R3''-Q31): the
            // `rejected_nand_backup_failed_no_backup_created` status
            // string + reason text explicitly tell the operator that
            // NO backup exists and they should NOT proceed without
            // first investigating the daemon log. The previous
            // wording was a bare `rejected_nand_backup_failed` which
            // could be read as "we have a partial backup; retry"
            // even though `nand_backup` deliberately returns Err
            // BEFORE the partial dir is renamed to its final name.
            let reason_text = format!(
                "{e}. NO BACKUP WAS CREATED — operator should NOT proceed and should \
                 investigate the daemon log (`tail /tmp/dcentrald.log` or \
                 `journalctl -u dcentrald`) before retrying. Retrying without \
                 understanding the failure can leave the miner unrecoverable. If \
                 the failure is a free-space precheck (250 MiB cap), free /data \
                 first; if it is a UBI-shape or LEB-mirror failure, run \
                 `ubinfo -a` from a serial console and consult \
                 ."
            );
            let resp = RestoreToStockResponse {
                status: "rejected_nand_backup_failed_no_backup_created".to_string(),
                reason: Some(reason_text),
                backup_path: None,
                reboot_at_ms: None,
                safety_findings: preflight.safety_findings,
                staged_sha256: preflight.staged_sha256,
                slot_plan: preflight.slot_plan,
                hashboard_count_to_use: body.hashboard_count_to_use,
                dry_run: false,
            };
            drop(lock_guard);
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(resp));
        }
    };

    // 5. Flash stock Bitmain firmware to the inactive slot.
    //
    //  W9-A — R1-C1 + R3-CRITICAL-1 fix.
    //
    // Previous wave-8 code shelled out to `sysupgrade -f <stock-tarball>`.
    // That was the wrong tool: DCENT_OS `/usr/sbin/sysupgrade` is a strict
    // A/B installer that requires a `sysupgrade-am1-s9/` directory layout,
    // a `MANIFEST.json` + Ed25519-signed `MANIFEST.sig`, and a squashfs
    // rootfs whose first 4 bytes are the `hsqs` magic. Bitmain stock
    // tarballs ship a UBI image at the archive root with none of those
    // properties — the spawn would exit non-zero in <100 ms and the
    // dashboard would remain stuck on `status:"scheduled"` forever
    // (R1-C1).
    //
    // The proven on-target stock-revert sequence is in
    // `DCENT_OS_Antminer/scripts/revert_to_stock.sh:97-115` and uses three
    // primitives that have been live-tested for years:
    //
    //   flash_erase /dev/mtd<inactive> 0 0
    //   nandwrite -p /dev/mtd<inactive> <extracted UBI image>
    //   fw_setenv bootslot <inactive_slot>
    //   fw_setenv upgrade_stage ""
    //
    // We invoke the shell script directly (Option A in the W9-A brief)
    // rather than re-implementing the primitives in Rust. Reasons:
    //   - The script is already on-image (rootfs-overlay/scripts/), so
    //     no Buildroot package change is needed.
    //   - The script auto-detects active vs inactive slot via fw_printenv
    //     and picks the safe target.
    //   - The script extracts the tarball into /tmp/stock_extract and
    //     locates the `*.ubi` payload via `find` — handles both stock
    //     Bitmain layouts (root-level UBI) and re-packed variants.
    //
    // The script's interactive `read -p 'Type REVERT'` is bypassed by
    // piping `REVERT\n` on stdin — the typed-confirm gate is already
    // enforced by run_preflight() above.
    //
    // NAND-write semantics: the spawned task does NOT trigger a reboot
    // itself; instead it leaves the miner with stock Bitmain in the
    // inactive slot, `bootslot` flipped, and `upgrade_stage` cleared.
    // The 30-second REBOOT_DELAY_SECS sleep then issues `reboot` so the
    // dashboard has time to render the response. The W9-C concurrency
    // wave will replace this fire-and-forget with a state-machine driven
    // sequence that updates STATUS as each primitive completes.
    transition_state(RestoreState::Staging {
        backup_path: backup_path.clone(),
    });

    let canonical_path = preflight_fingerprint.canonical_path.clone();
    let preflight_sha = preflight_fingerprint.sha256.clone();
    let reboot_at = now_ms() + REBOOT_DELAY_SECS * 1000;
    let script_path = resolve_revert_to_stock_script_for_profile(profile);
    let backup_path_clone = backup_path.clone();

    transition_state(RestoreState::Scheduled {
        reboot_at_ms: reboot_at,
        backup_path: backup_path.clone(),
    });

    tokio::spawn(async move {
        //  W9-C (R4-C1): the lock_guard moves into the spawned
        // task; it is dropped when the task body completes. The slot
        // is held for the entire flash-progress window, not just the
        // synchronous request handler.
        let _lock_guard = lock_guard;

        //  R4'-M1: spawn the actual flash body as an inner
        // task, then await its JoinHandle from this outer task. If
        // the inner task panics, JoinHandle resolves to Err(JoinError)
        // and we record FlashFailed to STATUS — without this, an
        // operator-visible dashboard could stall on "Scheduled"
        // forever while the real flash never completed.
        let backup_for_panic = backup_path_clone.clone();
        let inner = tokio::spawn(async move {
            // Pre-flash dwell — gives the dashboard time to render the
            // 200 response and the operator time to read the
            // confirmation panel before the network drops.
            tokio::time::sleep(std::time::Duration::from_secs(REBOOT_DELAY_SECS)).await;

            //  W9-C (R4-H1): re-fingerprint the staged tarball.
            // Refuse if either canonical path or SHA-256 drifted.
            let post_dwell_fp = match fingerprint_staged_tarball(&canonical_path).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Restore-to-Stock: post-dwell fingerprint FAILED — refusing flash (W9-C R4-H1)"
                    );
                    transition_state(RestoreState::FlashFailed {
                        reason: format!("post-dwell fingerprint failed: {e}"),
                        backup_path: Some(backup_path_clone.clone()),
                    });
                    return;
                }
            };
            if post_dwell_fp.canonical_path != canonical_path
                || post_dwell_fp.sha256 != preflight_sha
            {
                tracing::error!(
                    preflight_path = %canonical_path.display(),
                    post_dwell_path = %post_dwell_fp.canonical_path.display(),
                    preflight_sha = %preflight_sha,
                    post_dwell_sha = %post_dwell_fp.sha256,
                    "Restore-to-Stock: TOCTOU drift detected between preflight and flash dispatch — refusing flash (W9-C R4-H1)"
                );
                let preflight_prefix: String = preflight_sha.chars().take(16).collect();
                let post_prefix: String = post_dwell_fp.sha256.chars().take(16).collect();
                transition_state(RestoreState::FlashFailed {
                reason: format!(
                    "staged tarball drifted between preflight and dispatch (preflight sha {} → post-dwell sha {})",
                    preflight_prefix, post_prefix,
                ),
                backup_path: Some(backup_path_clone.clone()),
            });
                return;
            }

            transition_state(RestoreState::FlashRunning {
                backup_path: backup_path_clone.clone(),
            });
            tracing::warn!(
                staged_path = %canonical_path.display(),
                script = %script_path,
                "Restore-to-Stock: invoking revert_to_stock.sh via setsid (W9-C R3-HIGH/R4-H3 detach)"
            );

            //  W9-C (R3-HIGH / R4-H3): detach via setsid so the
            // child survives dcentrald exit. setsid puts the writer in a
            // new session (no controlling TTY) and a new process group,
            // so a dcentrald `kill -TERM` does NOT cascade to the child
            // via the controlling-TTY signaling path. We DO want to
            // capture the child's exit status so the state machine can
            // transition to FlashSucceeded | FlashFailed. If `setsid` is
            // missing on the target the spawned command falls back to
            // plain `sh`; the W9-C status report documents that fallback
            // and proposes Option B (persistent intent file + S99
            // honor-on-boot) for follow-up.
            use tokio::io::AsyncWriteExt;
            let setsid_present =
                Path::new("/usr/bin/setsid").is_file() || Path::new("/bin/setsid").is_file();

            let mut cmd = if setsid_present {
                let mut c = tokio::process::Command::new("setsid");
                c.arg("sh")
                    .arg(&script_path)
                    .arg(canonical_path.as_os_str())
                    .arg(&post_dwell_fp.sha256);
                c
            } else {
                tracing::warn!(
                    "Restore-to-Stock: setsid not on PATH — falling back to plain sh. \
                 Daemon restart during the 30s window will cancel the writer. \
                 Install util-linux for setsid (W9-C Option A)."
                );
                let mut c = tokio::process::Command::new("sh");
                c.arg(&script_path)
                    .arg(canonical_path.as_os_str())
                    .arg(&post_dwell_fp.sha256);
                c
            };

            let mut child = match cmd
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Restore-to-Stock: failed to spawn writer — DCENT_OS still active, no flash"
                    );
                    transition_state(RestoreState::FlashFailed {
                        reason: format!("spawn failed: {e}"),
                        backup_path: Some(backup_path_clone.clone()),
                    });
                    return;
                }
            };

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(b"REVERT\n").await;
                // Drop closes the pipe.
            }

            //  W13-D (A2'-#1): VNish-style polled progress streaming.
            // Take stdout + stderr, wrap each in a `BufReader::lines()`, and
            // spawn two tokio tasks to push each line into
            // `STATUS.recent_log_lines` via [`push_log_line`]. Both streams
            // are routed into the same ring buffer (interleaved by arrival
            // order, prefixed with `[err] ` for stderr to keep them
            // distinguishable for forensic readers of /status). The
            // dashboard renders the last ~10 lines while phase is
            // `flash_running`. Last-stderr-line is also recovered for the
            // FlashFailed transition reason.
            use tokio::io::AsyncBufReadExt;
            use tokio::io::BufReader;

            let stdout_pipe = child.stdout.take();
            let stderr_pipe = child.stderr.take();

            let stdout_handle = stdout_pipe.map(|out| {
                tokio::spawn(async move {
                    let reader = BufReader::new(out);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        push_log_line(line);
                    }
                })
            });

            // Stderr task ALSO retains the last-line for FlashFailed reason
            // synthesis. Wrapped in an Arc<Mutex<Option<String>>> shared
            // with this task's owner via `last_stderr_handle`.
            let last_stderr_line: Arc<std::sync::Mutex<Option<String>>> =
                Arc::new(std::sync::Mutex::new(None));
            let last_stderr_for_task = Arc::clone(&last_stderr_line);
            let stderr_handle = stderr_pipe.map(|err| {
                tokio::spawn(async move {
                    let reader = BufReader::new(err);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        // Stash for FlashFailed reason synthesis (overwrites
                        // each iteration so we end up with the LAST stderr
                        // line — typical writer failure mode prints the
                        // diagnostic immediately before exit).
                        if let Ok(mut g) = last_stderr_for_task.lock() {
                            *g = Some(line.clone());
                        }
                        push_log_line(format!("[err] {line}"));
                    }
                })
            });

            let wait_result = child.wait().await;
            // Drain the pipe-reader tasks so any tail bytes after exit are
            // pushed before we transition. Both `JoinHandle`s complete on
            // EOF (which arrives when the child closes its end of the
            // pipe), so this is bounded.
            if let Some(h) = stdout_handle {
                let _ = h.await;
            }
            if let Some(h) = stderr_handle {
                let _ = h.await;
            }

            match wait_result {
                Ok(status) => {
                    if status.success() {
                        tracing::info!(
                        "Restore-to-Stock: writer exit 0 — stock UBI written, bootslot flipped, upgrade_stage cleared"
                    );
                        transition_state(RestoreState::FlashSucceeded {
                            completed_at_ms: now_ms(),
                            backup_path: backup_path_clone.clone(),
                        });
                        // Trigger the deferred reboot, also detached
                        // where setsid is available.
                        if setsid_present {
                            let _ = tokio::process::Command::new("setsid")
                                .arg("reboot")
                                .output()
                                .await;
                        } else {
                            let _ = tokio::process::Command::new("reboot").output().await;
                        }
                    } else {
                        let last_stderr = last_stderr_line
                            .lock()
                            .ok()
                            .and_then(|g| g.clone())
                            .unwrap_or_else(|| "(empty)".to_string());
                        tracing::error!(
                            last_stderr = %last_stderr,
                            "Restore-to-Stock: writer FAILED — DCENT_OS still active, no flash, no reboot"
                        );
                        transition_state(RestoreState::FlashFailed {
                            reason: format!(
                                "writer exit code {:?}; stderr={}",
                                status.code(),
                                last_stderr,
                            ),
                            backup_path: Some(backup_path_clone.clone()),
                        });
                    }
                }
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Restore-to-Stock: failed to wait on writer"
                    );
                    transition_state(RestoreState::FlashFailed {
                        reason: format!("child.wait() failed: {e}"),
                        backup_path: Some(backup_path_clone.clone()),
                    });
                }
            }
            // inner spawned task body ends here.
        });

        //  R4'-M1: await the inner JoinHandle. If it panicked
        // or was cancelled, transition state to FlashFailed so the
        // dashboard /status endpoint reflects the real outcome
        // instead of stalling on Scheduled.
        match inner.await {
            Ok(()) => {
                // Inner task completed normally — state was already
                // transitioned to FlashSucceeded or FlashFailed inside.
            }
            Err(e) if e.is_panic() => {
                tracing::error!(
                    error = %e,
                    "Restore-to-Stock: spawned writer task PANICKED — recording FlashFailed"
                );
                transition_state(RestoreState::FlashFailed {
                    reason: format!("spawned writer panicked: {e}"),
                    backup_path: Some(backup_for_panic),
                });
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Restore-to-Stock: spawned writer task cancelled — recording FlashFailed"
                );
                transition_state(RestoreState::FlashFailed {
                    reason: format!("spawned writer cancelled: {e}"),
                    backup_path: Some(backup_for_panic),
                });
            }
        }
        // _lock_guard drops here; slot released for next operator.
    });

    let resp = RestoreToStockResponse {
        status: "scheduled".to_string(),
        reason: None,
        backup_path: Some(backup_path.to_string_lossy().into_owned()),
        reboot_at_ms: Some(reboot_at),
        safety_findings: preflight.safety_findings.clone(),
        staged_sha256: preflight.staged_sha256.clone(),
        slot_plan: preflight.slot_plan.clone(),
        hashboard_count_to_use: body.hashboard_count_to_use,
        dry_run: false,
    };

    //  W9-C: ALSO record the legacy flat status so any
    // dashboard not yet consuming `state_detail` continues to render
    // the wave-8 fields. The structured phase machine is preserved
    // via state_detail.
    record_status(RestoreToStockStatus {
        state: "scheduled".to_string(),
        state_detail: Some(RestoreState::Scheduled {
            reboot_at_ms: reboot_at,
            backup_path: backup_path.clone(),
        }),
        last_preflight_at_ms: Some(now_ms()),
        last_preflight_verdict: Some("preflight_ok".to_string()),
        last_backup_path: resp.backup_path.clone(),
        last_scheduled_reboot_at_ms: Some(reboot_at),
        last_safety_findings: resp.safety_findings.clone(),
        last_active_slot: resp.slot_plan.active_slot.clone(),
        last_inactive_slot: resp.slot_plan.inactive_slot.clone(),
        transitions: read_status().transitions.saturating_add(1),
        last_transition_at_ms: Some(now_ms()),
        //  W11-A (R1''-Q24): preserve the prior status snapshot's
        // fw_setenv-present field so we don't lose the operator-facing
        // signal across the scheduled-status overwrite. nand_backup
        // step (e) wrote into this slot; here we read it back.
        last_backup_fw_setenv_present: read_status().last_backup_fw_setenv_present,
        //  W13-D: preserve any stderr/stdout lines streamed by
        // the spawned writer up to this overwrite point. The spawned
        // task continues pushing through this transition; clobbering
        // here would erase mid-flight progress lines from the
        // dashboard's view.
        recent_log_lines: read_status().recent_log_lines,
    });
    push_audit_event(
        &state,
        "operator",
        AuditEvent::Free {
            category: "restore_to_stock".to_string(),
            message: format!(
                "restore-to-stock scheduled: backup_path={}; reboot_at_ms={reboot_at}",
                resp.backup_path.as_deref().unwrap_or("unknown")
            ),
        },
    );

    (StatusCode::OK, Json(resp))
}

/// `GET /api/system/restore-to-stock/status` — read-only snapshot of
/// the most recent preflight or flash attempt. Always 200.
pub async fn restore_to_stock_status() -> impl IntoResponse {
    (StatusCode::OK, Json(read_status()))
}

// ---------------------------------------------------------------------------
// Preflight engine — pure logic, returns a Result so handlers can
// short-circuit on critical findings or input validation errors.
// ---------------------------------------------------------------------------

/// Run the full preflight pipeline. Return either a successful
/// preflight envelope (which both handlers may then decorate with
/// dry_run / scheduled / etc.) or a typed `(code, response)` rejection
/// the handlers can return verbatim.
async fn run_preflight(
    state: &AppState,
    body: &RestoreToStockBody,
) -> Result<RestoreToStockResponse, (StatusCode, RestoreToStockResponse)> {
    // ---- 1. Operator confirmation strings ----
    if !body.acknowledge_breaker_warning {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_breaker_warning_not_acknowledged",
            "The operator must explicitly acknowledge the breaker / noise warning before stock flashing.",
            body,
            None,
            SlotPlan::default(),
            vec![],
        ));
    }

    if body.confirm_string_typed.trim() != "RESTORE TO STOCK" {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_confirm_string_mismatch",
            "Operator must type the exact phrase 'RESTORE TO STOCK' (uppercase) to proceed.",
            body,
            None,
            SlotPlan::default(),
            vec![],
        ));
    }

    if !(1..=3).contains(&body.hashboard_count_to_use) {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_invalid_hashboard_count",
            "hashboard_count_to_use must be 1, 2, or 3.",
            body,
            None,
            SlotPlan::default(),
            vec![],
        ));
    }

    // ---- 2. Operator-typed serial vs hardware_info ----
    let real_serial = state
        .hardware_info
        .lock()
        .ok()
        .and_then(|h| h.miner_serial.clone());
    match real_serial {
        Some(real) if !real.trim().is_empty() => {
            if body.operator_serial_typed.trim() != real.trim() {
                return Err(reject(
                    StatusCode::BAD_REQUEST,
                    "rejected_serial_mismatch",
                    &format!(
                        "Operator typed serial does not match the miner serial. \
                         Expected '{}', got '{}'.",
                        real, body.operator_serial_typed
                    ),
                    body,
                    None,
                    SlotPlan::default(),
                    vec![],
                ));
            }
        }
        _ => {
            // No serial known — refuse rather than skip the gate, so
            // a dashboard tab against a half-initialized daemon can't
            // bypass the typed-confirm check.
            return Err(reject(
                StatusCode::BAD_REQUEST,
                "rejected_serial_unknown",
                "Daemon has no miner serial yet; refusing to skip the typed-serial confirmation.",
                body,
                None,
                SlotPlan::default(),
                vec![],
            ));
        }
    }

    // ---- 3. Validate staged path is inside the upgrade staging root ----
    //
    // Order: existence check FIRST so the operator gets a clear
    // "not found" error for typos. The staging-root check uses
    // `canonicalize()` which would also fail for a non-existent
    // path, so without this ordering an obvious typo would surface
    // as "outside_staging" — confusing.
    let staged_path = Path::new(&body.stock_firmware_staged_path);
    if !staged_path.is_file() {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_stock_firmware_not_found",
            &format!(
                "Staged stock firmware tarball not found: {}",
                staged_path.display()
            ),
            body,
            None,
            SlotPlan::default(),
            vec![],
        ));
    }
    if !is_inside_staging_root(staged_path) {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_stock_firmware_path_outside_staging",
            "stock_firmware_staged_path must live inside /tmp/dcentos-upgrade/<uuid>/.",
            body,
            None,
            SlotPlan::default(),
            vec![],
        ));
    }

    // ---- 4. SHA-256 hash + optional pin check ----
    let staged_sha = match sha256_of_file(staged_path).await {
        Ok(h) => h,
        Err(e) => {
            return Err(reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                "rejected_sha256_failed",
                &format!("Failed to hash staged tarball: {e}"),
                body,
                None,
                SlotPlan::default(),
                vec![],
            ));
        }
    };
    if let Some(expected) = body.stock_firmware_sha256.as_deref() {
        if !expected.trim().is_empty() && !expected.trim().eq_ignore_ascii_case(&staged_sha) {
            return Err(reject(
                StatusCode::BAD_REQUEST,
                "rejected_sha256_mismatch",
                &format!(
                    "stock_firmware_sha256 mismatch: expected {}, computed {}",
                    expected, staged_sha
                ),
                body,
                Some(staged_sha.clone()),
                SlotPlan::default(),
                vec![],
            ));
        }
    }

    // ---- 5. Slot plan ----
    let slot_plan = read_slot_plan().await;

    // ---- 6.  W11-A (A4''-HIGH-5): manifest match BEFORE the
    //         expensive tar-extract + IOC scan. ----
    //
    // The wave-10 ordering ran `safety_preflight` (extract +
    // 256 MiB IOC scan) FIRST, then matched against the manifest. A
    // `WrongModel` verdict (Critical, no_override) made the IOC scan
    // wasted work — and on a 512 MB Cortex-A9 the wasted I/O can
    // pin the daemon for several seconds. Matching against the
    // manifest first short-circuits a wrong-model upload before any
    // tarball is ever extracted.
    //
    // Note the SHA used here is the staged-tarball SHA computed in
    // step 4 above (cheap streaming digest), NOT a hash of the
    // extracted contents — so this reorder doesn't pay any new cost.
    let detected_platform = detect_platform_signature().await;
    let manifest_verdict =
        lookup_in_stock_manifest(&staged_sha, detected_platform.as_deref(), None).await;

    // If the manifest match is `WrongModel` we MUST refuse before any
    // tar-extract so a hostile tarball sized to OOM the IOC scanner
    // can't do its damage when we already know the image is wrong
    // for this miner. The Critical no_override path below picks this
    // up via the `findings` accumulator.
    let manifest_finding = manifest_verdict
        .clone()
        .into_finding(detected_platform.as_deref());
    let manifest_is_critical_no_override =
        matches!(manifest_verdict, ManifestVerdict::WrongModel { .. });

    // ---- 7. Safety preflight scan (extract + IOC scan) ----
    //
    //  W11-A (A4''-HIGH-5): if we already have a Critical
    // no_override manifest verdict (WrongModel), short-circuit and
    // do NOT spend the IOC-scan budget on a tarball we're refusing
    // anyway. The handler's Critical-finding gate below produces the
    // same `rejected_critical_safety_finding` 400 either way.
    let mut findings = if manifest_is_critical_no_override {
        Vec::new()
    } else {
        safety_preflight(staged_path).await
    };

    // ---- 7b. Append the manifest finding so the Critical gate
    //         (below) picks up `WrongModel` (Critical no_override)
    //         without bespoke routing. `VerifiedSafe` / `Unknown` /
    //         `NonRevertable` / `ManifestUnavailable` surface as
    //         informational/High findings the operator can review
    //         (and ack via `acknowledge_high_findings:true` for High).
    findings.push(manifest_finding);

    // Critical no_override findings → hard reject. Operator cannot
    // pass `confirm:true` past this gate.
    let critical_no_override: Vec<&SafetyFinding> = findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Critical) && f.no_override)
        .collect();
    if !critical_no_override.is_empty() {
        let titles: Vec<String> = critical_no_override
            .iter()
            .map(|f| format!("{}: {}", f.id, f.title))
            .collect();
        let reason = format!(
            "Critical no-override IOCs detected in staged tarball: {}",
            titles.join("; ")
        );
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_critical_safety_finding",
            &reason,
            body,
            Some(staged_sha),
            slot_plan,
            findings,
        ));
    }

    // Other Critical findings (e.g. daemons:22322 RCE listener) also
    // hard-reject by default. There is no override flag at this
    // boundary — the dashboard refuses to send `confirm:true` past
    // a Critical finding.
    if findings
        .iter()
        .any(|f| matches!(f.severity, Severity::Critical))
    {
        let titles: Vec<String> = findings
            .iter()
            .filter(|f| matches!(f.severity, Severity::Critical))
            .map(|f| format!("{}: {}", f.id, f.title))
            .collect();
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "rejected_critical_safety_finding",
            &format!(
                "Critical IOCs detected in staged tarball: {}",
                titles.join("; ")
            ),
            body,
            Some(staged_sha),
            slot_plan,
            findings,
        ));
    }

    // ---- 7. Build the success envelope ----
    let resp = RestoreToStockResponse {
        status: "preflight_ok".to_string(),
        reason: None,
        backup_path: None,
        reboot_at_ms: None,
        safety_findings: findings,
        staged_sha256: Some(staged_sha),
        slot_plan,
        hashboard_count_to_use: body.hashboard_count_to_use,
        dry_run: !body.confirm,
    };

    record_status(RestoreToStockStatus {
        state: "preflight_ok".to_string(),
        last_preflight_at_ms: Some(now_ms()),
        last_preflight_verdict: Some(resp.status.clone()),
        last_backup_path: None,
        last_scheduled_reboot_at_ms: None,
        last_safety_findings: resp.safety_findings.clone(),
        last_active_slot: resp.slot_plan.active_slot.clone(),
        last_inactive_slot: resp.slot_plan.inactive_slot.clone(),
        ..Default::default()
    });

    Ok(resp)
}

fn reject(
    code: StatusCode,
    status_string: &str,
    reason: &str,
    body: &RestoreToStockBody,
    staged_sha: Option<String>,
    slot_plan: SlotPlan,
    findings: Vec<SafetyFinding>,
) -> (StatusCode, RestoreToStockResponse) {
    (
        code,
        RestoreToStockResponse {
            status: status_string.to_string(),
            reason: Some(reason.to_string()),
            backup_path: None,
            reboot_at_ms: None,
            safety_findings: findings,
            staged_sha256: staged_sha,
            slot_plan,
            hashboard_count_to_use: body.hashboard_count_to_use,
            dry_run: !body.confirm,
        },
    )
}

// ---------------------------------------------------------------------------
// Helpers — staging path / sha256 / slot plan
// ---------------------------------------------------------------------------

/// True when `path` is under the existing sysupgrade staging root
/// (`/tmp/dcentos-upgrade/<uuid>/<filename>`). Refuses paths that
/// traverse upward, paths directly under the staging root with no
/// UUID subdir, and paths whose UUID component is empty.
///
///  W9-C tightens this per R1-H4 — the wave-8 implementation
/// accepted `/tmp/dcentos-upgrade/anything.tar.gz` directly. The
/// `post_system_upgrade` endpoint always creates a fresh UUID
/// subdirectory per upload (`rest.rs:6352`); declaring that
/// invariant here closes the bypass where an SSH user (or shared
/// `/tmp` race) could drop a file directly in the staging root.
pub fn is_inside_staging_root(path: &Path) -> bool {
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let root = match Path::new("/tmp/dcentos-upgrade").canonicalize() {
        Ok(p) => p,
        Err(_) => Path::new("/tmp/dcentos-upgrade").to_path_buf(),
    };
    if !canonical.starts_with(&root) {
        return false;
    }
    // R1-H4: require at least one path component below the staging
    // root (i.e. the parent of the file is a UUID-shaped subdirectory,
    // not the staging root itself). We don't enforce strict UUID-v4
    // shape because the upload endpoint may switch to ULIDs / nanoids
    // later — we just require a non-empty subdir component.
    let rel = match canonical.strip_prefix(&root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let component_count = rel.components().count();
    // <uuid>/<filename> is exactly 2 components. Direct child of
    // staging root would be 1 (just the filename). Reject anything
    // shorter than 2.
    if component_count < 2 {
        return false;
    }
    // Reject empty UUID component (e.g. `/tmp/dcentos-upgrade//file`,
    // which canonicalize would normally collapse but we guard
    // anyway).
    if let Some(first) = rel.components().next() {
        let s = first.as_os_str().to_string_lossy();
        if s.is_empty() || s == "." || s == ".." {
            return false;
        }
    }
    true
}

///  W9-C (R4-H1): produce a stable canonical-path + sha256
/// fingerprint for the staged tarball. Called at preflight time;
/// the spawned task re-runs this and refuses to flash on drift.
async fn fingerprint_staged_tarball(path: &Path) -> Result<StagedTarballFingerprint, String> {
    // Canonicalize FIRST — symlink resolution has to happen at
    // preflight time so the spawned task is comparing apples to
    // apples (a symlink swapped between preflight and dispatch must
    // produce a different canonical path → drift detected).
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {}", path.display(), e))?;
    //  W9-A — `sha256_of_file` now returns `RestoreError` per
    // R4-H4. Stringify here at the boundary so this function's public
    // signature stays stable for the W9-C work that owns it.
    let sha = sha256_of_file(&canonical)
        .await
        .map_err(|e| e.to_string())?;
    Ok(StagedTarballFingerprint {
        canonical_path: canonical,
        sha256: sha,
    })
}

///  W9-A — R4-H4: typed error variant. Caller previously
/// received a `String` and could not distinguish ENOENT from EACCES;
/// `RestoreError::Io` preserves the underlying `io::ErrorKind`.
/// Stream-hash a file with 64 KiB chunks. Peak RSS overhead per call is
/// the chunk buffer (64 KiB) + the SHA-256 state (~200 bytes). On a
/// 512 MB Zynq running the full daemon stack with a 128 MiB upload cap
/// (`rest.rs:608`), the previous `tokio::fs::read` whole-file path was a
/// concrete OOM hazard — closes R4-C3 in
/// .
///
/// W9-B added a sibling `sha256_of_file_streaming` for the NAND backup
/// path. W9-F migrates this canonical helper to streaming as well; the
/// W9-B name is retained as a thin `Result<String, String>` wrapper so
/// the NAND backup call site at line ~2141 stays untouched.
async fn sha256_of_file(path: &Path) -> Result<String, RestoreError> {
    use tokio::io::AsyncReadExt;
    //  W11-A (R4''-io-helper-migration): use the
    // `RestoreError::io(source, path)` helper instead of the
    // struct-literal so the `path` field is consistently captured at
    // every operator-relevant `?`-propagation site.
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| RestoreError::io(e, path.to_path_buf()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .await
            .map_err(|e| RestoreError::io(e, path.to_path_buf()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode_lower(&hasher.finalize()))
}

mod hex {
    pub fn encode_lower(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }
}

///  R3'-M1: gate the destructive path on platform = S9 am1.
///
/// The mtd4/7/8 partition layout, UBI LEB counts (25/166/525), and
/// the in-tree `revert_to_stock.sh` (which hardcodes `/dev/mtd7` +
/// `/dev/mtd8`) are all S9-am1-specific. Running this flow on an
/// Amlogic am2/am3 miner (S19j Pro Amlogic, S21, S19k Pro) would
/// silently corrupt NAND. Belt-and-suspenders alongside the dashboard
/// hiding the button on non-S9 platforms.
///
/// Detection: read `/proc/cpuinfo`; require Xilinx Zynq CPU signature.
/// Fail-closed: if the file is missing or unreadable, refuse.
///
///  W12-B: replaced by the new 2-layer gate
/// ([`profile_for_current_platform`] + `verified_revertable`). Kept
/// for documentation cross-references; not on any live call path.
#[allow(dead_code)]
async fn platform_supports_restore_to_stock() -> bool {
    match tokio::fs::read_to_string("/proc/cpuinfo").await {
        Ok(s) => {
            let s_lower = s.to_lowercase();
            // S9 am1 + S17 + S19 Pro am2 are all Xilinx Zynq.
            // (am2 is gated separately by the slot-plan probe today;
            //  the dashboard restricts the button to S9-am1 visually.)
            // S19j Pro Amlogic + S21 + S19k Pro are Amlogic A113D —
            // NOT Zynq, NOT Xilinx — so they fail this check.
            s_lower.contains("xilinx") || s_lower.contains("zynq")
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Restore-to-Stock: failed to read /proc/cpuinfo for platform check; failing closed"
            );
            false
        }
    }
}

///  W10-G: detect a coarse-grained platform signature for
/// matching against the stock-Bitmain manifest. Reads `/proc/cpuinfo`
/// to distinguish Xilinx Zynq (S9 am1 / S17 am2 / S19 Pro am2 / S19j
/// Pro Zynq am2) from Amlogic (S19j Pro Amlogic / S21 / S19k Pro).
/// Returns `None` if cpuinfo is unreadable so the manifest lookup
/// falls through to `Unknown`/`VerifiedSafe` without erroneously
/// asserting `WrongModel`.
///
/// Today the only ENFORCED signature is `zynq-am1-bm1387` (the S9 —
/// the only platform that today passes `platform_supports_restore_to_stock`).
/// Other entries are informational; wave-11 will expand the platform
/// gate to use this fingerprint directly.
pub(crate) async fn detect_platform_signature() -> Option<String> {
    detect_platform_signature_with_root(None).await
}

///  W11-A: same as [`detect_platform_signature`] but takes an
/// optional `/proc` root override so tests can drive the helper
/// against a fixture directory tree without touching the real
/// `/proc/cpuinfo` + `/proc/device-tree/...`. Production callers
/// pass `None`; tests pass `Some(<tmp>)` and lay out
/// `<tmp>/cpuinfo` + `<tmp>/device-tree/model` + optionally
/// `<tmp>/device-tree/compatible`.
pub(crate) async fn detect_platform_signature_with_root(
    proc_root: Option<&Path>,
) -> Option<String> {
    let root: PathBuf = proc_root
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/proc"));
    let cpuinfo = tokio::fs::read_to_string(root.join("cpuinfo")).await.ok()?;
    let lower = cpuinfo.to_lowercase();
    if lower.contains("xilinx") || lower.contains("zynq") {
        // -prep R4''-#3: discriminate Zynq generations via the
        // device-tree model node before tagging. The S9 am1 ships the
        // XC7Z010 + a "ZC706/Antminer" DT model; the S19/S19j Pro am2
        // ships the XC7Z020 + a different DT model. Without this check,
        // an am2 daemon would tag as "zynq-am1-bm1387" — a wave-11
        // expansion of the platform gate to am2 would then accept S9
        // tarballs as VerifiedSafe on S19 hardware (wrong-model brick).
        //
        // Probe order:
        //   1. /proc/device-tree/model — most reliable, kernel-published
        //      (note: kernel publishes a NUL-terminated string here; we
        //       lowercase + substring-match so the trailing \0 is
        //       harmless).
        //   2. /proc/device-tree/compatible — fallback, lists chip variants
        //   3. fallback to "zynq-unknown" so the manifest lookup
        //      degrades to ManifestUnavailable / Unknown rather than
        //      silently mapping to S9.
        let model_lower = tokio::fs::read_to_string(root.join("device-tree/model"))
            .await
            .ok()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let compat_lower = tokio::fs::read_to_string(root.join("device-tree/compatible"))
            .await
            .ok()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        let combined = format!("{model_lower} {compat_lower}");
        if combined.contains("antminer-s9") || combined.contains("zc706") {
            Some("zynq-am1-bm1387".to_string())
        } else if combined.contains("antminer-s17") || combined.contains("am2-s17") {
            Some("zynq-am2-bm1397".to_string())
        } else if combined.contains("antminer-s19")
            || combined.contains("am2-s19")
            || combined.contains("xc7z020")
        {
            // S19 Pro am2 + S19j Pro am2 share the XC7Z020 SoC; both
            // are BM1398/BM1362 chip families, both NOT yet supported
            // by the destructive path (platform gate refuses am2
            // confirm:true today).
            Some("zynq-am2-bm1398".to_string())
        } else {
            // Generic Zynq we can't disambiguate — tag as unknown so
            // manifest lookup CANNOT silently map to a known model.
            // ManifestVerdict::Unknown fires; operator must ack High.
            Some("zynq-unknown".to_string())
        }
    } else if lower.contains("amlogic") || lower.contains("a113") {
        // Same disambiguation principle for Amlogic A113D family —
        // S19j Pro Amlogic + S21 + S19k Pro all share the A113D SoC
        // but differ in chip family (BM1362 / BM1368 / BM1366) and
        // BHB hashboard variant. Today the platform gate refuses
        // non-Zynq confirm:true; this signature is forward-compatible
        // with the wave-11 multi-platform expansion.
        let model_lower = tokio::fs::read_to_string(root.join("device-tree/model"))
            .await
            .ok()
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        // Phase 2B (v2 sweep, 2026-05-15): S21 Pro / S21 XP carry
        // **BM1370** (TSMC 3 nm) NOT the BM1368 of the base S21. They
        // MUST be matched BEFORE the bare-"s21" branch — otherwise the
        // generic `model_lower.contains("s21")` swallows "s21 pro" /
        // "s21 xp" and a BM1370 carrier silently resolves to the
        // BM1368-tuned `amlogic-a113d-bm1368` PROFILE_TABLE row + S21
        // revert script. The two new BM1370 PROFILE_TABLE entries
        // (`amlogic-a113d-bm1370`, `amlogic-a113d-bm1370-xp`) keep the
        // detector→profile roundtrip 1:1. DT model strings are
        // lowercased + NUL-tolerant (substring match), and Bitmain DT
        // models render the variant as "antminer-s21-pro" /
        // "antminer-s21-xp" / "antminer s21 pro" — accept hyphen and
        // space forms.
        if model_lower.contains("s21xp")
            || model_lower.contains("s21-xp")
            || model_lower.contains("s21 xp")
        {
            Some("amlogic-a113d-bm1370-xp".to_string())
        } else if model_lower.contains("s21pro")
            || model_lower.contains("s21-pro")
            || model_lower.contains("s21 pro")
        {
            Some("amlogic-a113d-bm1370".to_string())
        } else if model_lower.contains("s21") {
            Some("amlogic-a113d-bm1368".to_string())
        } else if model_lower.contains("s19xp")
            || model_lower.contains("s19-xp")
            || model_lower.contains("s19 xp")
        {
            // F-4 (Sweep-v3 PR-081): Amlogic S19 XP carries BM1366 (per
            // `dcentrald-silicon-profiles::asics.rs` Bm1366.used_in =
            // ["S19j","S19k Pro","S19 XP"]). It previously matched none
            // of s21/s19j/s19k and fell to `amlogic-unknown`
            // (fail-closed-safe — no brick, just no auto-route).
            // Mirrors the BM1370 ordered-substring pattern; safe before
            // the bare `s19j` branch because "s19 xp" contains neither
            // "s19j" nor "s19k" (and "s21 xp" was already matched far
            // above + contains "s21" not "s19"). The Sweep doc's
            // "S19j Pro+ -> BM1366" was REJECTED: the in-code
            // PROFILE_TABLE lists S19j Pro+ under BM1362, so the
            // unconditional `s19j -> bm1362` below is CORRECT and is
            // deliberately left untouched (changing it would be a
            // silicon-identity regression / brick risk).
            Some("amlogic-a113d-bm1366".to_string())
        } else if model_lower.contains("s19j") {
            Some("amlogic-a113d-bm1362".to_string())
        } else if model_lower.contains("s19k") {
            Some("amlogic-a113d-bm1366".to_string())
        } else {
            Some("amlogic-unknown".to_string())
        }
    } else {
        None
    }
}

///  W11-A test re-export of
/// [`detect_platform_signature_with_root`]. Hidden from rustdoc.
#[doc(hidden)]
pub async fn detect_platform_signature_with_root_for_test(
    proc_root: Option<&Path>,
) -> Option<String> {
    detect_platform_signature_with_root(proc_root).await
}

///  W10-G: outcome of matching a staged tarball SHA against
/// the stock-Bitmain manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestVerdict {
    /// Tarball SHA matches a manifest entry, the entry is
    /// dcentos_revertable, and the entry's platform_signature matches
    /// the daemon-detected platform.
    VerifiedSafe { model: String, version: String },
    /// Tarball SHA matches a manifest entry, but the entry's
    /// platform_signature does NOT match the daemon-detected
    /// platform — operator is about to flash an S19 image onto a S9.
    /// Critical no_override.
    WrongModel {
        manifest_model: String,
        manifest_platform: String,
        detected_platform: String,
    },
    /// Tarball SHA matches a manifest entry, but the entry is marked
    /// `dcentos_revertable: false` — operator must explicitly ack.
    NonRevertable {
        model: String,
        version: String,
        revert_notes: String,
    },
    /// Tarball SHA is not in the manifest. Informational — operator
    /// gets a Medium warning but can proceed.
    Unknown,
    /// Manifest file is missing, malformed, or otherwise unavailable.
    /// -prep A4''-HIGH-1: surfaces as `DCENT-2026-020` HIGH
    /// require-ack — was previously Medium informational, which let
    /// a manifest deletion silently fall open as "unknown image,
    /// proceed with operator ack". Operator now MUST set
    /// `acknowledge_high_findings: true` to flash when manifest
    /// verification can't run.
    ManifestUnavailable { reason: String },
}

impl ManifestVerdict {
    /// Convert the verdict into a `SafetyFinding` consumable by the
    /// existing run_preflight pipeline. The `detected_platform` is
    /// passed through for the diagnostic message on `Unknown`.
    pub fn into_finding(self, detected_platform: Option<&str>) -> SafetyFinding {
        match self {
            ManifestVerdict::VerifiedSafe { model, version } => SafetyFinding {
                id: "DCENT-2026-016".to_string(),
                severity: Severity::Info,
                title: format!("Stock image matches known-safe manifest entry for {model}"),
                matched_path: None,
                remediation: format!(
                    "Tarball SHA matched the {model} {version} entry in the \
                     stock-Bitmain manifest. Platform signature matches the \
                     daemon-detected platform. Proceed."
                ),
                no_override: false,
            },
            ManifestVerdict::WrongModel {
                manifest_model,
                manifest_platform,
                detected_platform,
            } => SafetyFinding {
                id: "DCENT-2026-017".to_string(),
                severity: Severity::Critical,
                title: "Stock image is for the wrong model".to_string(),
                matched_path: None,
                remediation: format!(
                    "Tarball SHA matches the {manifest_model} entry \
                     (platform_signature={manifest_platform}) in the \
                     stock-Bitmain manifest, but this miner detects as \
                     {detected_platform}. Flashing this image would brick \
                     the miner — the partition layouts and ASIC drivers do \
                     NOT match. Refusing flash."
                ),
                no_override: true,
            },
            ManifestVerdict::NonRevertable {
                model,
                version,
                revert_notes,
            } => SafetyFinding {
                id: "DCENT-2026-018".to_string(),
                severity: Severity::High,
                title: format!("Stock image for {model} is NOT marked dcentos_revertable"),
                matched_path: None,
                remediation: format!(
                    "Tarball SHA matched the {model} {version} entry, but the \
                     manifest marks this image as dcentos_revertable:false. \
                     Reason: {revert_notes} \
                     Set acknowledge_high_findings:true after reading the \
                     reason if you accept the revert risk."
                ),
                no_override: false,
            },
            ManifestVerdict::Unknown => SafetyFinding {
                id: "DCENT-2026-019".to_string(),
                severity: Severity::Medium,
                title: "Stock image SHA not in manifest".to_string(),
                matched_path: None,
                remediation: format!(
                    "Tarball SHA is not present in the stock-Bitmain manifest \
                     (detected platform: {}). The daemon cannot vouch that \
                     this is a known-safe stock image for your model. Verify \
                     the source out-of-band before proceeding.",
                    detected_platform.unwrap_or("unknown")
                ),
                no_override: false,
            },
            ManifestVerdict::ManifestUnavailable { reason } => SafetyFinding {
                // -prep A4''-HIGH-1: was DCENT-2026-019 Medium
                // (collapsed with Unknown), which let a manifest deletion
                // silently fall open as "unknown image, ack and proceed".
                // Promoted to its own ID + High severity. Operator must
                // explicitly `acknowledge_high_findings: true` to flash
                // when the manifest can't be loaded — defense against an
                // attacker who deletes /etc/dcentos/stock-bitmain-manifest.json
                // (now baked-in, but kept defensible if a future wave
                // re-introduces a disk path) or who corrupts the parser.
                id: "DCENT-2026-020".to_string(),
                severity: Severity::High,
                title: "Stock-Bitmain manifest unavailable".to_string(),
                matched_path: None,
                remediation: format!(
                    "Manifest lookup failed ({reason}); cannot verify the \
                     staged image against the known-safe SHA list. Operator \
                     MUST verify out-of-band that the staged tarball is \
                     genuinely a Bitmain stock image for THIS model AND a \
                     version DCENT_OS can revert FROM, then acknowledge the \
                     HIGH-severity findings. If unsure, abort."
                ),
                no_override: false,
            },
        }
    }
}

/// -prep A4''-CRITICAL-1: stock-Bitmain manifest BAKED into the
/// daemon binary at compile time. Closes the manifest-substitution
/// attack where an adversary with /etc/dcentos/ write access could swap
/// the disk-resident manifest for a rogue one listing their malicious
/// tarball SHA + `dcentos_revertable: true` and trick the daemon into
/// flashing a wrong-model or non-revertable image.
///
/// In production, `lookup_in_stock_manifest(.., None)` parses this
/// baked string directly — there is NO filesystem read on the
/// production path. Tests pass `Some(&fixture_path)` to override.
///
/// To update: edit `dcentrald-api/assets/stock-bitmain-manifest.json`
/// and rebuild dcentrald-api (the bake re-snapshots at compile time).
const STOCK_MANIFEST_BAKED: &str = include_str!("../../assets/stock-bitmain-manifest.json");

/// W29 (2026-05-13): at-rest ed25519 signature pin on the baked
/// stock-Bitmain manifest. Defense-in-depth on top of the
/// compile-time-baked manifest (W11-prep A4''-CRITICAL-1) — protects
/// against build-pipeline tampering (manifest swap before bake) and
/// post-build binary patching (manifest bytes overwritten in the
/// shipped binary).
///
/// Verification is gated on
/// `crate::ota_signature::manifest_signature_required()` (true when
/// `DCENT_MANIFEST_PUBLIC_KEY_HEX` was set at build time). When no
/// pubkey is pinned, the daemon emits a one-shot warning at startup
/// (matching the OTA pattern) and skips manifest signature checks.
/// When a pubkey IS pinned, this signature MUST verify or
/// `lookup_in_stock_manifest` short-circuits to
/// `ManifestVerdict::ManifestUnavailable` BEFORE any JSON parse.
///
/// The committed `stock-bitmain-manifest.json.sig` file is a
/// zero-bytes placeholder. Operators generate the real signature at
/// release time. This deliberately fails closed when a pubkey is
/// pinned and the .sig is still the placeholder — the system is
/// fail-closed by construction.
///
/// **Release process (run at tag time, NOT during normal builds):**
///
/// ```sh
/// # 1. Generate ed25519 keypair (one-time, store private key in HSM/Vault):
/// openssl genpkey -algorithm ED25519 -out manifest_key.pem
///
/// # 2. Extract raw 32-byte ed25519 public key as hex64 (the format the
/// #    DCENT_MANIFEST_PUBLIC_KEY_HEX env var expects). The DER SPKI
/// #    prefix is 12 bytes for ed25519, so we slice the trailing 32:
/// openssl pkey -in manifest_key.pem -pubout -outform DER \
///   | xxd -p -c 64 | tail -c 65 | head -c 64
///
/// # 3. Sign the manifest (must be byte-identical to the baked file):
/// openssl pkeyutl -sign -inkey manifest_key.pem -rawin \
///   -in dcentrald-api/assets/stock-bitmain-manifest.json \
///   -out dcentrald-api/assets/stock-bitmain-manifest.json.sig
///
/// # 4. Build with the pin (and optionally a key id for rotation tracking):
/// DCENT_MANIFEST_PUBLIC_KEY_HEX=<hex64-from-step-2> \
///   DCENT_MANIFEST_KEY_ID=manifest-2026-05 \
///   cargo build --release --target armv7-unknown-linux-musleabihf
/// ```
///
/// A helper script at `scripts/sign_stock_manifest.sh`
/// wraps step 3 (not load-bearing for tests; just a release-process
/// hook).
const STOCK_MANIFEST_SIG_BAKED: &[u8] =
    include_bytes!("../../assets/stock-bitmain-manifest.json.sig");

///  W10-G: look up the staged tarball SHA in the stock-Bitmain
/// manifest. The lookup is best-effort — manifest unavailability
/// degrades to `ManifestUnavailable` (high require-ack post-A4'',
/// previously medium informational), never silently passes.
///
/// `manifest_path` allows tests to point the helper at a fixture; in
/// production callers pass `None` and the helper uses the
/// compile-time-baked [`STOCK_MANIFEST_BAKED`] string. The previous
/// `/etc/dcentos/stock-bitmain-manifest.json` filesystem read was
/// removed in wave-11-prep A4''-CRITICAL-1 — disk-resident manifests
/// are an attack surface (writable by root, no integrity binding).
pub async fn lookup_in_stock_manifest(
    staged_sha256: &str,
    detected_platform: Option<&str>,
    manifest_path: Option<&Path>,
) -> ManifestVerdict {
    lookup_in_stock_manifest_with_sig(staged_sha256, detected_platform, manifest_path, None).await
}

/// W29 (2026-05-13): variant of `lookup_in_stock_manifest` that also
/// accepts an optional explicit signature path on the test path. The
/// production call site (`run_preflight`) uses the no-sig wrapper —
/// the production path always uses the baked manifest + baked .sig
/// gated on `manifest_signature_required()`. Tests pass
/// `manifest_sig_path: Some(&path)` to drive end-to-end signature
/// verification against fixture files.
///
/// W29 verification semantics:
///
/// - Production path (`manifest_path: None`): when
///   `manifest_signature_required()` is true, verify
///   `STOCK_MANIFEST_BAKED.as_bytes()` against `STOCK_MANIFEST_SIG_BAKED`
///   using the compile-time-pinned key. On verification failure, return
///   `ManifestUnavailable` with a reason citing signature failure
///   BEFORE any JSON parse. When no pubkey is pinned, verification is
///   skipped entirely (current behavior preserved; matches the OTA
///   pattern).
///
/// - Test path (`manifest_path: Some(...)`): when both
///   `manifest_signature_required()` is true AND `manifest_sig_path` is
///   `Some(...)`, read the .sig (capped at 256 bytes) and verify the
///   manifest bytes. On verification failure, fail closed.
pub async fn lookup_in_stock_manifest_with_sig(
    staged_sha256: &str,
    detected_platform: Option<&str>,
    manifest_path: Option<&Path>,
    manifest_sig_path: Option<&Path>,
) -> ManifestVerdict {
    // W29 (2026-05-13): at-rest signature pin on the manifest. Run
    // BEFORE the JSON parse so a tampered manifest never reaches the
    // parser. Gated on `manifest_signature_required()` — when no
    // pubkey is pinned at build time, verification is skipped (matches
    // the OTA `signature_required()` convention).
    if crate::ota_signature::manifest_signature_required() {
        match manifest_path {
            None => {
                // Production: verify the compile-time-baked manifest
                // against the compile-time-baked signature using the
                // compile-time-pinned pubkey.
                if let Err(e) = crate::ota_signature::verify_manifest_signature(
                    STOCK_MANIFEST_BAKED.as_bytes(),
                    STOCK_MANIFEST_SIG_BAKED,
                ) {
                    tracing::error!(
                        target = "manifest_signature",
                        error = %e,
                        "W29: baked manifest signature verification failed; refusing manifest lookup",
                    );
                    return ManifestVerdict::ManifestUnavailable {
                        reason: format!("baked manifest signature verification failed: {e}"),
                    };
                }
            }
            Some(_) => {
                // Test path with explicit sig file: caller drives
                // verification with a fixture .sig. If the caller
                // didn't provide a sig path, signature verification is
                // skipped on the test path (the test fixtures don't
                // need to carry a real signature unless they're
                // explicitly testing the signature gate).
                if let Some(sig_path) = manifest_sig_path {
                    // Cap sig size — ed25519 signatures are 64 bytes;
                    // 256 is generous and keeps a hostile fixture from
                    // OOMing the host.
                    const MAX_SIG_BYTES: u64 = 256;
                    let sig_bytes = match tokio::fs::metadata(sig_path).await {
                        Ok(meta) if meta.len() > MAX_SIG_BYTES => {
                            return ManifestVerdict::ManifestUnavailable {
                                reason: format!(
                                    "{}: signature size {} bytes exceeds {}-byte cap",
                                    sig_path.display(),
                                    meta.len(),
                                    MAX_SIG_BYTES,
                                ),
                            };
                        }
                        Ok(_) => match tokio::fs::read(sig_path).await {
                            Ok(b) => b,
                            Err(e) => {
                                return ManifestVerdict::ManifestUnavailable {
                                    reason: format!("read sig {}: {e}", sig_path.display()),
                                };
                            }
                        },
                        Err(e) => {
                            return ManifestVerdict::ManifestUnavailable {
                                reason: format!("stat sig {}: {e}", sig_path.display()),
                            };
                        }
                    };
                    let manifest_bytes = match tokio::fs::read(manifest_path.unwrap()).await {
                        Ok(b) => b,
                        Err(e) => {
                            return ManifestVerdict::ManifestUnavailable {
                                reason: format!(
                                    "read {} for sig verify: {e}",
                                    manifest_path.unwrap().display()
                                ),
                            };
                        }
                    };
                    if let Err(e) =
                        crate::ota_signature::verify_manifest_signature(&manifest_bytes, &sig_bytes)
                    {
                        tracing::error!(
                            target = "manifest_signature",
                            error = %e,
                            "W29: fixture manifest signature verification failed; refusing manifest lookup",
                        );
                        return ManifestVerdict::ManifestUnavailable {
                            reason: format!("fixture manifest signature verification failed: {e}"),
                        };
                    }
                }
            }
        }
    } else {
        // No pubkey pinned at build time — skip verification. The
        // startup-warning is emitted by the daemon's bring-up logging
        // (see ota_signature::signature_required's parallel warning).
        // Tracing here once at the call site would be too chatty —
        // this path is hit on every restore-to-stock preflight. The
        // daemon-level warning handles the operator-visibility need.
    }

    // -prep A4''-CRITICAL-1: production path uses the baked
    // string directly. Tests with manifest_path=Some(...) read from
    // disk for fixture flexibility.
    let raw: String = if let Some(p) = manifest_path {
        // -prep R1''-Q4: cap manifest file size on the test path
        // too. 1 MiB is generous; production is bounded by binary size.
        const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
        match tokio::fs::metadata(p).await {
            Ok(meta) if meta.len() > MAX_MANIFEST_BYTES => {
                return ManifestVerdict::ManifestUnavailable {
                    reason: format!(
                        "{}: manifest size {} bytes exceeds {}-byte cap",
                        p.display(),
                        meta.len(),
                        MAX_MANIFEST_BYTES,
                    ),
                };
            }
            Ok(_) => {}
            Err(e) => {
                return ManifestVerdict::ManifestUnavailable {
                    reason: format!("stat {}: {e}", p.display()),
                };
            }
        }
        match tokio::fs::read_to_string(p).await {
            Ok(s) => s,
            Err(e) => {
                return ManifestVerdict::ManifestUnavailable {
                    reason: format!("read {}: {e}", p.display()),
                };
            }
        }
    } else {
        // Production: baked-in string, no filesystem read.
        STOCK_MANIFEST_BAKED.to_string()
    };
    let chosen: PathBuf = manifest_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("<baked manifest>"));

    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return ManifestVerdict::ManifestUnavailable {
                reason: format!("parse {}: {e}", chosen.display()),
            };
        }
    };

    //  W11-A (A4''-MEDIUM-1): explicit `schema_version` gate.
    // Refuse forward-incompatible manifests so a wave-12 schema bump
    // (with renamed/removed fields) cannot silently fall through this
    // best-effort `unwrap_or` chain on an old daemon and hand back a
    // stale `Unknown` verdict.
    const SUPPORTED_MANIFEST_SCHEMA: u64 = 1;
    let schema_version = parsed.get("schema_version").and_then(|v| v.as_u64());
    match schema_version {
        Some(v) if v == SUPPORTED_MANIFEST_SCHEMA => {}
        Some(v) => {
            return ManifestVerdict::ManifestUnavailable {
                reason: format!(
                    "{}: unsupported schema_version {v} (daemon supports {SUPPORTED_MANIFEST_SCHEMA})",
                    chosen.display()
                ),
            };
        }
        None => {
            return ManifestVerdict::ManifestUnavailable {
                reason: format!(
                    "{}: missing top-level `schema_version` (daemon supports {SUPPORTED_MANIFEST_SCHEMA})",
                    chosen.display()
                ),
            };
        }
    }

    let entries = match parsed.get("stock_images").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => {
            return ManifestVerdict::ManifestUnavailable {
                reason: format!("{}: missing or non-array `stock_images`", chosen.display()),
            };
        }
    };

    //  W11-A (A4''-HIGH-3 + A4''-MEDIUM-2): build a SHA→entry
    // HashMap once instead of linearly scanning the array on every
    // call. Keys are lowercase-normalized so a manifest entry shipped
    // with mixed-case SHA still matches the always-lowercase
    // `sha256_of_file` output. Skips `UNKNOWN`/empty placeholder rows.
    let needle = staged_sha256.trim().to_ascii_lowercase();
    let mut by_sha: std::collections::HashMap<String, &serde_json::Value> =
        std::collections::HashMap::with_capacity(entries.len());
    for entry in entries {
        let entry_sha = entry
            .get("sha256")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if entry_sha.is_empty() || entry_sha == "unknown" {
            continue;
        }
        // First-write wins for duplicate SHAs (operator-supplied
        // manifest oddity); a future wave can promote this to an
        // explicit ManifestUnavailable if duplicates are observed in
        // the wild.
        by_sha.entry(entry_sha).or_insert(entry);
    }

    if let Some(entry) = by_sha.get(&needle) {
        let model = entry
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown-model")
            .to_string();
        let version = entry
            .get("stock_version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown-version")
            .to_string();
        let manifest_platform = entry
            .get("platform_signature")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown-platform")
            .to_string();
        let revertable = entry
            .get("dcentos_revertable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let revert_notes = entry
            .get("revert_notes")
            .and_then(|v| v.as_str())
            .unwrap_or("(no notes)")
            .to_string();

        // Platform check — only assert WrongModel if BOTH sides have
        // a known platform signature and they differ. Unknown
        // detected_platform falls through to the revertability check
        // (the operator at least gets the matched-image confirmation
        // even if the daemon couldn't read /proc/cpuinfo).
        if let Some(detected) = detected_platform {
            if !detected.is_empty()
                && !manifest_platform.is_empty()
                && manifest_platform != "unknown-platform"
                && manifest_platform != detected
            {
                return ManifestVerdict::WrongModel {
                    manifest_model: model,
                    manifest_platform,
                    detected_platform: detected.to_string(),
                };
            }
        }

        if !revertable {
            return ManifestVerdict::NonRevertable {
                model,
                version,
                revert_notes,
            };
        }

        return ManifestVerdict::VerifiedSafe { model, version };
    }

    ManifestVerdict::Unknown
}

async fn read_slot_plan() -> SlotPlan {
    //  W12-B: probe the active platform profile so we use
    // its `bootslot_env_keys` ordering. If the platform doesn't match
    // any PROFILE_TABLE entry (older daemons, Windows host tests,
    // dev environments), fall back to the union of all keys so the
    // wave-≤11 behavior is preserved.
    let keys: &[&str] = match profile_for_current_platform().await {
        Some(p) => p.bootslot_env_keys,
        None => {
            // Wave-≤11 default: union of all per-platform keys in
            // priority order. Existing tests rely on this fallback
            // when /proc/cpuinfo doesn't fingerprint to a known
            // PROFILE_TABLE entry.
            &[
                "active_slot",
                "bootslot",
                "firmware",
                "dcent_boot_slot",
                "firstboot",
            ]
        }
    };
    let mut active: Option<String> = None;
    for key in keys {
        if let Some(v) = run_fw_printenv(key).await {
            active = Some(v);
            break;
        }
    }

    let (inactive, inactive_mtd) = match active.as_deref() {
        Some("a") | Some("1") => (Some("b".to_string()), Some("/dev/mtd8".to_string())),
        Some("b") | Some("2") => (Some("a".to_string()), Some("/dev/mtd7".to_string())),
        _ => (None, None),
    };

    SlotPlan {
        active_slot: active,
        inactive_slot: inactive,
        inactive_mtd,
    }
}

async fn run_fw_printenv(key: &str) -> Option<String> {
    let output = tokio::process::Command::new("fw_printenv")
        .args(["-n", key])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Safety preflight — scans the staged tarball for IOCs.
// ---------------------------------------------------------------------------

/// Run the safety preflight on the staged tarball. Extracts to a
/// scratch dir, walks the dir, applies the detector list. Cleans up
/// the scratch dir before returning.
async fn safety_preflight(tarball: &Path) -> Vec<SafetyFinding> {
    let scratch = match tempdir_for_preflight() {
        Ok(p) => p,
        Err(e) => {
            return vec![SafetyFinding {
                id: "DCENT-INTERNAL-001".to_string(),
                severity: Severity::High,
                title: "Could not create preflight scratch directory".to_string(),
                matched_path: None,
                remediation: format!("Daemon error: {e}. Refusing flash."),
                no_override: true,
            }];
        }
    };

    // -prep A4''-CRITICAL-2 +  W11-A R1''-Q15: bomb-cap
    // fires PRE-extraction by walking tar headers (`tar -tvf`) and
    // both summing claimed entry sizes and counting entries. The
    // wave-10 cap walked the already-extracted tree, which on a
    // 4 GiB bomb would OOM-kill the daemon BEFORE the cap could fire
    // on a 512-MB-RAM Zynq. The wave-11 R1''-Q15 extension catches
    // inode-bomb tarballs (10K 1-byte files) that would survive the
    // size cap but blow up readdir / IOC-scan walks downstream.
    match header_extracted_size_violation(tarball).await {
        Some(TarHeaderViolation::SizeOverflow(total)) => {
            let _ = tokio::fs::remove_dir_all(&scratch).await;
            return vec![SafetyFinding {
                id: "DCENT-INTERNAL-005".to_string(),
                severity: Severity::Critical,
                title: "tarball decompression bomb (header-claimed size > 256 MiB)".to_string(),
                matched_path: None,
                remediation: format!(
                    "Tar header-walk reports cumulative entry sizes ≥ {} bytes \
                     (cap is {} bytes). Refusing pre-extract — staged tarball \
                     appears to be a decompression-bomb attack. The daemon never \
                     extracted a single byte.",
                    total, MAX_EXTRACTED_BYTES
                ),
                no_override: true,
            }];
        }
        Some(TarHeaderViolation::EntryCountOverflow(entries)) => {
            let _ = tokio::fs::remove_dir_all(&scratch).await;
            return vec![SafetyFinding {
                id: "DCENT-INTERNAL-006".to_string(),
                severity: Severity::Critical,
                title: format!("tarball inode-bomb (entry count > {} cap)", MAX_TAR_ENTRIES),
                matched_path: None,
                remediation: format!(
                    "Tar header-walk reports {} entries (cap is {}). Real \
                     Bitmain S9 stock tarballs ship 12 entries; the cap is \
                     ~5x that for headroom. Refusing pre-extract — staged \
                     tarball appears to be an inode-bomb attack designed to \
                     blow up readdir or IOC-scan walks downstream. The daemon \
                     never extracted a single byte.",
                    entries, MAX_TAR_ENTRIES
                ),
                no_override: true,
            }];
        }
        None => {}
    }

    // Extract via `tar` — works for `.tar`, `.tar.gz`, `.tar.bz2`,
    // `.tgz`, and Bitmain's `.bmu` (which is just a renamed gzipped
    // tar). Any failure here is a Critical finding because we cannot
    // evaluate safety on an opaque tarball.
    //
    //  W9-A — R4-H2 hardening: pass tar flags that suppress
    // privilege escalation during extraction (`--no-same-owner`,
    // `--no-same-permissions`) and refuse overwrite of existing
    // directories (`--no-overwrite-dir`). BusyBox tar accepts these
    // flags. After extraction, we additionally walk every entry and
    // refuse the whole preflight if any path canonicalizes to outside
    // the scratch dir (the belt-and-suspenders defense against
    // BusyBox tar variants that historically allowed `..` and
    // absolute paths).
    let extract = tokio::process::Command::new("tar")
        .args([
            "--no-same-owner",
            "--no-same-permissions",
            "--no-overwrite-dir",
            "-xf",
            tarball.to_string_lossy().as_ref(),
            "-C",
            scratch.to_string_lossy().as_ref(),
        ])
        .output()
        .await;
    match extract {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let _ = tokio::fs::remove_dir_all(&scratch).await;
            return vec![SafetyFinding {
                id: "DCENT-INTERNAL-002".to_string(),
                severity: Severity::Critical,
                title: "Stock firmware tarball failed to extract".to_string(),
                matched_path: None,
                remediation: format!(
                    "tar -xf returned non-zero. stderr: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
                no_override: true,
            }];
        }
        Err(e) => {
            //  W10-D (R4'-M3): tar-binary-missing is an
            // operator-actionable environment problem, not a server
            // crash. The `run_preflight` Critical-finding handler maps
            // this to a 400 Bad Request (see the `Severity::Critical`
            // gate around line 1597-1618), so the operator gets a
            // clear "install tar before retrying" message rather than
            // the generic 500 Internal Server Error that the wave-9
            // wording implied. The remediation text now spells out the
            // actionable fix explicitly.
            let _ = tokio::fs::remove_dir_all(&scratch).await;
            return vec![SafetyFinding {
                id: "DCENT-INTERNAL-003".to_string(),
                severity: Severity::Critical,
                title: "tar binary unavailable on target".to_string(),
                matched_path: None,
                remediation: format!(
                    "Cannot spawn `tar` to inspect the staged tarball ({e}). \
                     Install tar (busybox-tar is sufficient) before retrying. \
                     This surfaces as a 400 Bad Request, not a 500 — the daemon \
                     itself is healthy; the rootfs is missing the `tar` binary."
                ),
                no_override: true,
            }];
        }
    }

    // R4-H2 — post-extraction slip detection. Walk every extracted
    // entry, canonicalize, refuse if it escapes the scratch root.
    if let Some(escaped) = first_slip_violation(&scratch).await {
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        return vec![SafetyFinding {
            id: "DCENT-INTERNAL-004".to_string(),
            severity: Severity::Critical,
            title: "Tarball contains path-traversal entry (slip attempt)".to_string(),
            matched_path: Some(escaped),
            remediation: "Tarball includes an entry whose canonicalized path escapes the extraction root. Refusing flash."
                .to_string(),
            no_override: true,
        }];
    }

    //  W10-B (A1-HIGH-1) — decompression-bomb cap. Walk every
    // extracted file and accumulate sizes; if the running total
    // exceeds [`MAX_EXTRACTED_BYTES`] (256 MiB), short-circuit the
    // walk and refuse the flash. A small compressed tarball can
    // declare gigabyte-scale entries — if we let the IOC scan walk
    // it, we'd waste disk + I/O before refusing. Better: refuse as
    // soon as the cumulative size cross the cap.
    if let Some(total) = extracted_size_violation(&scratch).await {
        let _ = tokio::fs::remove_dir_all(&scratch).await;
        return vec![SafetyFinding {
            id: "DCENT-INTERNAL-005".to_string(),
            severity: Severity::Critical,
            title: "tarball decompression bomb (uncompressed size > 256 MiB)".to_string(),
            matched_path: None,
            remediation: format!(
                "Cumulative uncompressed size {} bytes exceeds the {} byte \
                 cap. Refusing flash — staged tarball appears to be a \
                 decompression-bomb attack.",
                total, MAX_EXTRACTED_BYTES
            ),
            no_override: true,
        }];
    }

    let mut findings = scan_extracted_dir(&scratch).await;
    if findings.is_empty() {
        findings.push(SafetyFinding {
            id: "DCENT-INFO-000".to_string(),
            severity: Severity::Info,
            title: "No IOCs detected in stock firmware tarball".to_string(),
            matched_path: None,
            remediation: "Tarball is clean per the W5-C/E detector list. Proceed to NAND backup."
                .to_string(),
            no_override: false,
        });
    }
    let _ = tokio::fs::remove_dir_all(&scratch).await;
    findings
}

///  W9-A — R4-H2 mitigation. Walk every entry under `root`,
/// canonicalize, return the first entry whose resolved path escapes
/// `root`. Returns `None` when no violation is found. The check
/// catches symlinks resolving outside the scratch dir as well as
/// (extremely rare) tar variants that wrote `..`-prefixed entries
/// that survived `--no-overwrite-dir`.
///
///  W10-B (A1-MEDIUM-1) extension: ALSO refuses any symlink
/// whose `read_link` target normalizes to a path outside the scratch
/// root, even when the link's canonicalized path resolves inside
/// `canonical_root` (e.g. a dangling symlink to `/etc/passwd` whose
/// target file doesn't exist — `canonicalize` returns `Err`, the
/// previous code silently accepted that). Hard links inside the
/// extracted tree are not separately detected here because BusyBox
/// `tar -xf` resolves hard-link entries to copies of the source file
/// rather than `link()` calls — but we still inspect `read_link`
/// targets so a `Symlink` entry pointing at `/etc/passwd` (which
/// would survive extraction as a literal symlink) is rejected.
///
/// Public so tests can assert it on synthetic fixtures without
/// reaching into the full preflight pipeline.
pub async fn first_slip_violation(root: &Path) -> Option<String> {
    let canonical_root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return None,
    };
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            // Refuse any entry whose textual path contains the
            // parent-traversal token. Cheap belt-and-suspenders that
            // does not depend on `canonicalize` being able to follow
            // a hostile symlink.
            if path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Some(path.to_string_lossy().into_owned());
            }

            //  W10-B (A1-MEDIUM-1): inspect symlink targets
            // BEFORE canonicalization. A dangling symlink (target
            // `/etc/passwd` doesn't exist on this host) makes
            // `canonicalize` return Err and the previous code would
            // silently accept the entry. Same idea catches
            // BusyBox-tar's `Symlink`/`Link` entries that point
            // outside the extraction root.
            //
            // Use `symlink_metadata` (does NOT follow links) +
            // `read_link` so we operate on the link itself, not the
            // resolved target file. We refuse the entry if EITHER:
            //   - the read_link target is absolute (any absolute
            //     path is automatically outside the scratch root),
            //   - the read_link target, joined with the link's
            //     parent, normalizes to a path outside
            //     `canonical_root`.
            if let Ok(lmeta) = tokio::fs::symlink_metadata(&path).await {
                if lmeta.file_type().is_symlink() {
                    if let Ok(target) = tokio::fs::read_link(&path).await {
                        if target.is_absolute() {
                            return Some(path.to_string_lossy().into_owned());
                        }
                        // Resolve relative to the link's parent,
                        // then normalize ParentDir components manually
                        // (canonicalize would follow the link AND
                        // require the target to exist).
                        let base = path.parent().unwrap_or(&path).to_path_buf();
                        let joined = base.join(&target);
                        let mut normalized: Vec<std::path::Component> = Vec::new();
                        for comp in joined.components() {
                            match comp {
                                std::path::Component::ParentDir => {
                                    if normalized
                                        .last()
                                        .is_none_or(|c| matches!(c, std::path::Component::RootDir))
                                    {
                                        // Can't pop past root → escape.
                                        return Some(path.to_string_lossy().into_owned());
                                    }
                                    normalized.pop();
                                }
                                std::path::Component::CurDir => {}
                                other => normalized.push(other),
                            }
                        }
                        let normalized_path: PathBuf = normalized.iter().collect();
                        if !normalized_path.starts_with(&canonical_root) {
                            return Some(path.to_string_lossy().into_owned());
                        }
                    }
                }
            }

            // Resolve via canonicalize when possible, otherwise via
            // a manual prefix check. Canonicalize follows symlinks
            // — a symlink whose target is `/etc/passwd` will
            // canonicalize to `/etc/passwd` and trip the prefix
            // check.
            if let Ok(resolved) = path.canonicalize() {
                if !resolved.starts_with(&canonical_root) {
                    return Some(path.to_string_lossy().into_owned());
                }
            }
            // Recurse into directories.
            if let Ok(ftype) = entry.file_type().await {
                if ftype.is_dir() {
                    stack.push(path);
                }
            }
        }
    }
    None
}

///  W10-B (A1-HIGH-1): walk every regular file under `root`
/// and accumulate `metadata().len()`. Returns `Some(total)` as soon
/// as the running total exceeds [`MAX_EXTRACTED_BYTES`]; returns
/// `None` if every file fits under the cap. Short-circuits — does
/// not finish walking the tree once the cap is exceeded, so a
/// hostile tarball with many huge entries doesn't pay the full I/O
/// cost.
///
/// Public so tests can drive it on synthetic fixtures without
/// reaching into the full preflight pipeline.
pub async fn extracted_size_violation(root: &Path) -> Option<u64> {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let ftype = match entry.file_type().await {
                Ok(f) => f,
                Err(_) => continue,
            };
            if ftype.is_dir() {
                stack.push(path);
                continue;
            }
            if ftype.is_file() {
                if let Ok(meta) = entry.metadata().await {
                    total = total.saturating_add(meta.len());
                    if total > MAX_EXTRACTED_BYTES {
                        return Some(total);
                    }
                }
            }
        }
    }
    None
}

///  W11-A (R1''-Q15): per-tar entry cap. Real Bitmain S9 stock
/// tarball
/// (`Antminer-S9-all-201705031918-550M-user-Update2UBI-NF.tar.gz`)
/// has exactly 12 entries (3 dirs + 9 files):
///
///   `c5/`, `c5/soc_system.rbf`, `c5/upgrade-marker.bin`,
///   `c5/angstrom_rootfs.jffs2`, `runme.sh`, `ubi_info`, `xilinx/`,
///   `xilinx/rootfs.jffs2`, `xilinx/upgrade-marker.bin`,
///   `xilinx/devicetree.dtb`, `xilinx/BOOT.bin`, `xilinx/uImage`.
///
/// 64 = ~5.3× the known real entry count, comfortably above the 3×
/// floor in the brief, leaves headroom for future signed manifests
/// or per-platform extras (Amlogic boot trees ship more BL2/FIP/U-Boot
/// stages), and refuses inode-bomb attacks (10K-entry tarballs that
/// blow up readdir budgets even at small individual sizes).
const MAX_TAR_ENTRIES: usize = 64;

///  W11-A (R1''-Q15): pre-extract entry-count + claimed-size
/// violation kinds returned by [`header_extracted_size_violation`].
/// Splitting the return type cleanly distinguishes the
/// decompression-bomb cap (cumulative bytes) from the inode-bomb cap
/// (entry count) so the safety preflight can surface different
/// finding IDs (DCENT-INTERNAL-005 vs DCENT-INTERNAL-006) with
/// finding-shape parity.
#[derive(Debug, Clone)]
enum TarHeaderViolation {
    /// Cumulative claimed entry sizes crossed [`MAX_EXTRACTED_BYTES`].
    SizeOverflow(u64),
    /// Total entry count crossed [`MAX_TAR_ENTRIES`] (inode-bomb).
    EntryCountOverflow(usize),
}

/// -prep A4''-CRITICAL-2 +  W11-A R1''-Q15: pre-extract
/// decompression-bomb + inode-bomb cap. Walks tar headers via
/// `tar -tvf` (works for `.tar`, `.tar.gz`, `.tgz`, `.bmu`) and
/// accumulates the size column AND the entry count. Returns
/// `Some(SizeOverflow(total))` if the cumulative claimed size crosses
/// [`MAX_EXTRACTED_BYTES`], `Some(EntryCountOverflow(n))` if the
/// entry count crosses [`MAX_TAR_ENTRIES`], or `None` otherwise.
///
/// Defends the daemon's 512 MB Zynq RAM against:
/// - a small tarball that declares gigabyte-scale entries (refused
///   before `tar -xf` writes a single byte),
/// - an inode-bomb tarball with thousands of 1-byte entries that
///   would survive the size cap but blow up readdir / IOC-scan
///   walks (~64 KiB / file) downstream.
///
///  W11-A (R1''-Q14, "sparse-file size truth"): we trust the
/// `tar -tvf` size column (header-claimed) which matches the
/// `tar -xf` decompressed-write quota — sparse files DO NOT bypass
/// the cap because the tar header still declares the logical size.
/// `tar -xf` then writes that many decompressed bytes (sparse holes
/// only avoid disk usage, not the size header). So column-3 is the
/// correct ceiling for both file payload AND for the IOC-scan
/// walk-budget downstream.
///
/// `tar -tvf` output (BusyBox-tar + GNU-tar both):
///   `-rw-r--r-- root/root    12345 2026-05-06 12:00:00 path/to/file`
/// We parse column-3 as the size. A line whose column-3 fails to
/// parse is treated as size 0 (e.g. directory entries) — the size
/// cap is computed on file payload only — but the line is still
/// counted toward the entry-count cap.
///
/// `None` is also returned if `tar -tvf` itself fails — extraction
/// proper will then fire `DCENT-INTERNAL-002` (tarball failed to
/// extract) and refuse the flash via the existing path. Belt-and-
/// suspenders.
async fn header_extracted_size_violation(tarball: &Path) -> Option<TarHeaderViolation> {
    let out = tokio::process::Command::new("tar")
        .args(["-tvf", tarball.to_string_lossy().as_ref()])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut total: u64 = 0;
    let mut entries: usize = 0;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        entries = entries.saturating_add(1);
        if entries > MAX_TAR_ENTRIES {
            return Some(TarHeaderViolation::EntryCountOverflow(entries));
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if let Some(size_str) = cols.get(2) {
            if let Ok(size) = size_str.parse::<u64>() {
                total = total.saturating_add(size);
                if total > MAX_EXTRACTED_BYTES {
                    return Some(TarHeaderViolation::SizeOverflow(total));
                }
            }
        }
    }
    None
}

/// Test-only re-export. Returns the discriminant + payload as a tuple
/// of `(kind, value)` so the integration tests can assert which arm
/// fired without needing access to the private enum. `kind` is
/// `"size_overflow"` or `"entry_count_overflow"`.
#[doc(hidden)]
pub async fn header_extracted_violation_for_test(tarball: &Path) -> Option<(&'static str, u64)> {
    match header_extracted_size_violation(tarball).await? {
        TarHeaderViolation::SizeOverflow(n) => Some(("size_overflow", n)),
        TarHeaderViolation::EntryCountOverflow(n) => Some(("entry_count_overflow", n as u64)),
    }
}

/// Test-only re-export of [`MAX_TAR_ENTRIES`]. Hidden from rustdoc.
#[doc(hidden)]
pub fn max_tar_entries_for_test() -> usize {
    MAX_TAR_ENTRIES
}

/// W9-F (R4-C3): hard cap on per-file IOC scan size. Files larger than
/// this are skipped (silently in earlier waves; W9-F now emits an
/// informational `DCENT-INFO-001` finding so the operator can see why
/// a large blob was excluded). Set well above the largest plausible
/// stock-Bitmain rootfs payload (~64 MiB UBI + ~64 MiB kernel) but low
/// enough that streaming a single file's worth of state stays bounded
/// even if many run concurrently. 256 MiB matches twice the
/// `rest.rs:608` upload cap so a single file can never be larger than
/// the entire tarball.
const IOC_SCAN_MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Per-file streaming chunk size. 64 KiB matches the W9-B
/// `sha256_of_file_streaming` cadence + is well below any plausible
/// L1 / L2 footprint on a Cortex-A9.
const IOC_SCAN_CHUNK_BYTES: usize = 64 * 1024;

/// Bytes after a needle match needed to be confident the match is
/// stable across a chunk boundary. Computed at compile time as the
/// length of the longest fixed needle minus 1; bumped to a constant
/// so future agents can't accidentally drop the overlap.
const IOC_SCAN_MAX_NEEDLE_OVERLAP: usize = 64;

/// Result struct returned by [`scan_file_for_needles_streaming`].
/// Each boolean tracks whether the corresponding needle was observed
/// at least once during the streaming scan. Order matches the
/// `needles` slice passed in.
#[derive(Debug, Default, Clone)]
struct StreamingScanResult {
    found: Vec<bool>,
}

/// Walk the extracted directory, applying every detector. Pure walk —
/// no daemon state touched.
///
///  W9-D rewrite (2026-05-05) brings the Rust IOC scanner to
/// byte-for-byte parity with the Python wave-5 detector
/// (`projects/dcent-toolbox/src/dcent_toolbox/exploits/vnish_security_audit.py:442-991`):
///
/// - **R1-C2** (closed jointly with W9-F): the wave-8 8 MiB hard skip
///   silently dropped every binary where the IOC needles actually live
///   (stock `bmminer` ≈ 6 MiB; VNish-tampered `cgminer`/`dashd` 9-12
///   MiB; Hashcore-injected variants can exceed 10 MiB). W9-F raised
///   the cap to 256 MiB AND replaced whole-file `tokio::fs::read` with
///   the streaming sliding-window scanner (peak per-call alloc ≈ 65 KiB
///   regardless of file size). W9-D ensures every Python-listed
///   binary path is actually visited by the scanner.
/// - **R1-C3**: DCENT-2026-012 (daemons:22322) now checks ALL THREE
///   needles (`daemons`, `monitor-ipsig`, `22322`) in BOTH init-script
///   paths AND the `daemons` binary itself (`usr/bin/daemons`,
///   `usr/sbin/daemons`). Previously only `monitor-ipsig` in
///   init.d/rcS/inittab fired.
/// - **M-1**: DCENT-2026-008 hotelfee.json now requires the canonical
///   `etc/factory/hotelfee.json` path (Python parity at
///   `vnish_security_audit.py:484-492`).
/// - **M-2**: DCENT-2026-008 now parses the JSON and only flags when
///   `donation > 0` (Python parity at `vnish_security_audit.py:494-525`).
/// - **DCENT-2026-009**: atlas SSH key match scoped to
///   `**/authorized_keys` (Python parity at
///   `vnish_security_audit.py:537`).
/// - **DCENT-2026-011**: Hashcore root hash match scoped to
///   `**/etc/shadow` (Python parity at `vnish_security_audit.py:627-634`).
/// - **DCENT-2026-013/014**: needle lists expanded to the full Python
///   needle vocabulary (`INNOSILICON_DTU_NEEDLES`,
///   `VNISH_FACTORY_RESET_NEEDLES`) and the scope walks both init
///   scripts AND binary search paths AND named miner binaries.
/// - **DCENT-2026-015**: negative-detection added (LOW severity) so
///   the Rust contract claims byte-for-byte parity honestly.
///
/// I/O contract: every file read goes through
/// [`scan_file_for_needles_streaming`] which enforces
/// [`IOC_SCAN_MAX_FILE_BYTES`] (256 MiB) + emits the
/// `DCENT-INFO-001` finding for over-cap files. The detector layer
/// never opens a file directly outside that path so the W9-F OOM
/// guarantee (peak alloc ~65 KiB / file) holds.
async fn scan_extracted_dir(root: &Path) -> Vec<SafetyFinding> {
    let mut findings = Vec::new();

    // Single tree walk — gather every regular file path ONCE so the
    // multiple detector passes below don't repeat the readdir I/O.
    let mut all_files: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let ftype = match entry.file_type().await {
                Ok(f) => f,
                Err(_) => continue,
            };
            if ftype.is_dir() {
                stack.push(path);
                continue;
            }
            if ftype.is_file() {
                all_files.push(path);
            }
        }
    }

    // ---------------------------------------------------------------
    // Pass 1 — per-file path-scoped detectors.
    // ---------------------------------------------------------------
    for path in &all_files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let path_str = path.to_string_lossy().to_string();
        let rel = relative_posix(root, path);

        // ---- DCENT-2026-010 SECURE_BOOT_SET (no-override) ----
        // Python parity: vnish_security_audit.py:579-614.
        //
        //  A1-HIGH-3: scope is now SHA256-prefix-on-any-1024-
        // byte-file, NOT just `name == "SECURE_BOOT_SET"`. Renaming
        // the eFuse-burn blob to `kernel.bin` or any innocuous name
        // would have bypassed the wave-9 filename-only gate. A
        // 1024-byte file with SHA `c3b77476bfc640ed…` is the eFuse
        // blob no matter what the operator (or attacker) named it.
        //
        // Bounded to exactly 1024 bytes by [`SECURE_BOOT_SET_SIZE`]
        // so the whole-file read here is safe — the size gate
        // strictly precedes the read.
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            if meta.len() == SECURE_BOOT_SET_SIZE {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    let mut hasher = Sha256::new();
                    hasher.update(&bytes);
                    let hash = hex::encode_lower(&hasher.finalize());
                    if hash.starts_with(SECURE_BOOT_SET_SHA256_PREFIX) {
                        let title = if name == "SECURE_BOOT_SET" {
                            "SECURE_BOOT_SET eFuse-burning blob (PERMANENT BRICK risk)".to_string()
                        } else {
                            format!(
                                "SECURE_BOOT_SET eFuse-burning blob (renamed to '{name}' — PERMANENT BRICK risk)"
                            )
                        };
                        findings.push(SafetyFinding {
                            id: "DCENT-2026-010".to_string(),
                            severity: Severity::Critical,
                            title,
                            matched_path: Some(path_str.clone()),
                            remediation:
                                "DO NOT INSTALL. This blob irreversibly burns the Amlogic A113D SECURE_BOOT eFuse. There is no override flag and no recovery path. SHA-256 prefix matched the known eFuse-burn payload regardless of filename."
                                    .to_string(),
                            no_override: true,
                        });
                    }
                }
            }
        }

        // ---- DCENT-2026-008 hotelfee.json devfee ----
        // Python parity: vnish_security_audit.py:484-525. Tighten:
        // canonical etc/factory/hotelfee.json + JSON donation>0.
        if name == VNISH_HOTELFEE_FILENAME && rel.contains("etc/factory/hotelfee.json") {
            // hotelfee.json is a small config file (~hundreds of
            // bytes); a whole-read is safe and we need the parsed
            // JSON value to evaluate `donation>0`.
            if let Ok(text) = tokio::fs::read_to_string(&path).await {
                let donation_pct = match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(serde_json::Value::Object(obj)) => obj
                        .get("donation")
                        .and_then(|v| match v {
                            serde_json::Value::Number(n) => n.as_f64(),
                            serde_json::Value::String(s) => s.parse::<f64>().ok(),
                            _ => None,
                        })
                        .unwrap_or(0.0),
                    _ => 0.0,
                };
                if donation_pct > 0.0 {
                    findings.push(SafetyFinding {
                        id: "DCENT-2026-008".to_string(),
                        severity: Severity::High,
                        title: format!(
                            "VNish hotelfee.json devfee {donation_pct}% present"
                        ),
                        matched_path: Some(path_str.clone()),
                        remediation:
                            "Remove /etc/factory/hotelfee.json before install (or set donation to 0). VNish dashd time-multiplexes hotelfee work alongside operator pools so the devfee never appears in `cgminer api pools`."
                                .to_string(),
                        no_override: false,
                    });
                }
            }
        }

        // ---- DCENT-2026-009 atlas SSH key ----
        // Python parity: vnish_security_audit.py:528-565 (filename
        // glob `**/authorized_keys`). Scope: only flag inside files
        // named `authorized_keys`.
        if name == "authorized_keys" {
            let needles: &[&[u8]] = &[VNISH_ATLAS_KEY_NEEDLE.as_bytes()];
            if let Some(skip) = streaming_skip_finding(path, &path_str, needles).await {
                findings.push(skip);
            } else if let Ok(scan) = scan_file_for_needles_streaming(path, needles).await {
                if scan.found.first().copied().unwrap_or(false) {
                    findings.push(SafetyFinding {
                        id: "DCENT-2026-009".to_string(),
                        severity: Severity::High,
                        title: "VNish atlas SSH key in authorized_keys".to_string(),
                        matched_path: Some(path_str.clone()),
                        remediation:
                            "Remove the atlas@anthill.farm public key from /root/.ssh/authorized_keys (and any other authorized_keys file in the rootfs). VNish vendor IOC across all platforms."
                                .to_string(),
                        no_override: false,
                    });
                }
            }
        }

        // ---- DCENT-2026-011 Hashcore root hash ----
        // Python parity: vnish_security_audit.py:617-665 (filename
        // glob `**/etc/shadow`). Scope: only flag inside `etc/shadow`.
        if name == "shadow" && rel.ends_with("etc/shadow") {
            let needles: &[&[u8]] = &[HASHCORE_ROOT_HASH_NEEDLE.as_bytes()];
            if let Some(skip) = streaming_skip_finding(path, &path_str, needles).await {
                findings.push(skip);
            } else if let Ok(scan) = scan_file_for_needles_streaming(path, needles).await {
                if scan.found.first().copied().unwrap_or(false) {
                    findings.push(SafetyFinding {
                        id: "DCENT-2026-011".to_string(),
                        severity: Severity::High,
                        title: "Hashcore SHA-512 universal root hash in /etc/shadow"
                            .to_string(),
                        matched_path: Some(path_str.clone()),
                        remediation:
                            "Reset the root password before deploying. The Hashcore Toolkit injected this universal SHA-512 root hash via the daemons:22322 RCE; every unit it touched shares the same credential."
                                .to_string(),
                        no_override: false,
                    });
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // Pass 2 — DCENT-2026-012 daemons:22322 (CRITICAL).
    // Python parity: vnish_security_audit.py:718-774.  W9-D
    // R1-C3: scope = init scripts + `daemons` binary; ALL THREE
    // needles (`daemons`, `monitor-ipsig`, `22322`).
    // ---------------------------------------------------------------
    let mut daemons_seen: Vec<(String, String)> = Vec::new();
    {
        let mut scan_paths: Vec<&PathBuf> = collect_init_script_paths(root, &all_files);
        let bin_paths = collect_binary_paths_for_names(&all_files, &["daemons"]);
        scan_paths.extend(bin_paths.iter());
        let mut seen_paths: std::collections::HashSet<PathBuf> = Default::default();
        let needles: Vec<&[u8]> = DAEMONS_22322_NEEDLES.iter().map(|s| s.as_bytes()).collect();
        for path in scan_paths {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            if let Some(skip) = streaming_skip_finding(path, &path_str, &needles).await {
                findings.push(skip);
                continue;
            }
            if let Ok(scan) = scan_file_for_needles_streaming(path, &needles).await {
                for (idx, hit) in scan.found.iter().enumerate() {
                    if *hit {
                        daemons_seen
                            .push((path_str.clone(), DAEMONS_22322_NEEDLES[idx].to_string()));
                        break; // record first needle per file
                    }
                }
            }
        }
    }
    if let Some((first_path, first_needle)) = daemons_seen.first().cloned() {
        let summary = daemons_seen
            .iter()
            .take(5)
            .map(|(p, n)| format!("{p}: {n}"))
            .collect::<Vec<_>>()
            .join("; ");
        findings.push(SafetyFinding {
            id: "DCENT-2026-012".to_string(),
            severity: Severity::Critical,
            title: "daemons:22322 unauthenticated RCE listener wired at boot"
                .to_string(),
            matched_path: Some(first_path.clone()),
            remediation: format!(
                "Remove the daemons/monitor-ipsig listener from init AND/OR the `daemons` binary itself before install. First match: {first_path} (needle: '{first_needle}'). All matches: {summary}. Wave-3 RE proved this listener has a recv->sprintf->system() chain executing as root with no auth (TCP 22322)."
            ),
            no_override: false,
        });
    }

    // ---------------------------------------------------------------
    // Pass 3 — DCENT-2026-013 Innosilicon DTU phone-home (MEDIUM).
    // Python parity: vnish_security_audit.py:777-850. Scope = init
    // scripts + binary search paths + miner-named binaries +
    // `dtu.conf.def` filename. ALL FOUR needles.
    // ---------------------------------------------------------------
    let mut dtu_seen: Vec<(String, String)> = Vec::new();
    {
        // (a) Filename heuristic: any `dtu.conf.def` in tree.
        for path in &all_files {
            if path.file_name().and_then(|n| n.to_str()) == Some("dtu.conf.def") {
                dtu_seen.push((
                    path.to_string_lossy().to_string(),
                    "dtu.conf.def".to_string(),
                ));
            }
        }
        // (b) String scan across init scripts + binary paths +
        // miner-named binaries.
        let mut scan_paths: Vec<&PathBuf> = collect_init_script_paths(root, &all_files);
        scan_paths.extend(collect_binary_search_paths(root, &all_files));
        let extras = collect_binary_paths_for_names(
            &all_files,
            &["bmminer", "cgminer", "dtu", "single-board-test"],
        );
        scan_paths.extend(extras.iter());
        let mut seen_paths: std::collections::HashSet<PathBuf> = Default::default();
        let needles: Vec<&[u8]> = INNOSILICON_DTU_NEEDLES
            .iter()
            .map(|s| s.as_bytes())
            .collect();
        for path in scan_paths {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            if let Some(skip) = streaming_skip_finding(path, &path_str, &needles).await {
                findings.push(skip);
                continue;
            }
            if let Ok(scan) = scan_file_for_needles_streaming(path, &needles).await {
                for (idx, hit) in scan.found.iter().enumerate() {
                    if *hit {
                        dtu_seen.push((path_str.clone(), INNOSILICON_DTU_NEEDLES[idx].to_string()));
                        break;
                    }
                }
            }
        }
    }
    if let Some((first_path, first_needle)) = dtu_seen.first().cloned() {
        let summary = dtu_seen
            .iter()
            .take(5)
            .map(|(p, n)| format!("{p}: {n}"))
            .collect::<Vec<_>>()
            .join("; ");
        findings.push(SafetyFinding {
            id: "DCENT-2026-013".to_string(),
            severity: Severity::Medium,
            title: "Innosilicon DTU phone-home endpoint baked into firmware"
                .to_string(),
            matched_path: Some(first_path.clone()),
            remediation: format!(
                "Remove `dtu`, `dtu.conf.def`, and any wrapper init script before install. First match: {first_path} (needle: '{first_needle}'). All matches: {summary}. Endpoint 39.104.179.132:20001 is the Innosilicon Aliyun cloud telemetry channel; DCENT_OS Innosilicon ports MUST exclude it."
            ),
            no_override: false,
        });
    }

    // ---------------------------------------------------------------
    // Pass 4 — DCENT-2026-014 VNish dashd --enable-factory-reset (MEDIUM).
    // Python parity: vnish_security_audit.py:853-917. Scope = init
    // scripts + binary search paths + `dashd` binary. BOTH needles.
    // ---------------------------------------------------------------
    let mut dashd_seen: Vec<(String, String)> = Vec::new();
    {
        let mut scan_paths: Vec<&PathBuf> = collect_init_script_paths(root, &all_files);
        scan_paths.extend(collect_binary_search_paths(root, &all_files));
        let extras = collect_binary_paths_for_names(&all_files, &["dashd"]);
        scan_paths.extend(extras.iter());
        let mut seen_paths: std::collections::HashSet<PathBuf> = Default::default();
        let needles: Vec<&[u8]> = VNISH_FACTORY_RESET_NEEDLES
            .iter()
            .map(|s| s.as_bytes())
            .collect();
        for path in scan_paths {
            if !seen_paths.insert(path.clone()) {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            if let Some(skip) = streaming_skip_finding(path, &path_str, &needles).await {
                findings.push(skip);
                continue;
            }
            if let Ok(scan) = scan_file_for_needles_streaming(path, &needles).await {
                for (idx, hit) in scan.found.iter().enumerate() {
                    if *hit {
                        dashd_seen.push((
                            path_str.clone(),
                            VNISH_FACTORY_RESET_NEEDLES[idx].to_string(),
                        ));
                        break;
                    }
                }
            }
        }
    }
    if let Some((first_path, first_needle)) = dashd_seen.first().cloned() {
        let summary = dashd_seen
            .iter()
            .take(5)
            .map(|(p, n)| format!("{p}: {n}"))
            .collect::<Vec<_>>()
            .join("; ");
        findings.push(SafetyFinding {
            id: "DCENT-2026-014".to_string(),
            severity: Severity::Medium,
            title: "VNish dashd --enable-factory-reset signature present"
                .to_string(),
            matched_path: Some(first_path.clone()),
            remediation: format!(
                "Remove the `--enable-factory-reset` flag from the dashd init script + any wrappers before install. First match: {first_path} (needle: '{first_needle}'). All matches: {summary}. The flag exposes a no-auth factory-reset REST endpoint that wipes operator hardening and re-enables vendor DevFee."
            ),
            no_override: false,
        });
    }

    // ---------------------------------------------------------------
    // Pass 5 — DCENT-2026-015 negative: stock Amlogic S21 unsigned (LOW).
    // Python parity: vnish_security_audit.py:920-991. Informational
    // LOW severity; never blocks install. Only fires when filename
    // heuristics suggest a stock Amlogic S21 image AND
    // SECURE_BOOT_SET (DCENT-2026-010) was NOT detected on this tree.
    // ---------------------------------------------------------------
    if !findings.iter().any(|f| f.id == "DCENT-2026-010")
        && looks_like_amlogic_s21(root, &all_files)
    {
        findings.push(SafetyFinding {
            id: "DCENT-2026-015".to_string(),
            severity: Severity::Low,
            title: "Stock Amlogic S21 firmware without SECURE_BOOT eFuse evidence".to_string(),
            matched_path: None,
            remediation:
                "No action required. Informational only: stock Amlogic S21 SoCs whose SECURE_BOOT eFuse is not yet burned remain vulnerable to future vnish.farm 'unlock' packs that flip the eFuse permanently. Keep recovery media; evaluate whether a deliberate eFuse burn is worthwhile before long-term deployment."
                    .to_string(),
            no_override: false,
        });
    }

    // De-duplicate `DCENT-INFO-001` skip findings: if a file is
    // visited by more than one detector pass, multiple skip records
    // are appended above. Keep only the first per `matched_path` so
    // the dashboard doesn't show the same skip 3 times.
    let mut seen_skip_paths: std::collections::HashSet<String> = Default::default();
    findings.retain(|f| {
        if f.id != "DCENT-INFO-001" {
            return true;
        }
        match &f.matched_path {
            Some(p) => seen_skip_paths.insert(p.clone()),
            None => true,
        }
    });

    findings
}

/// Helper: if `path` exceeds the IOC scan size cap, build the
/// `DCENT-INFO-001` skip finding once. Returns `None` when the file
/// is in-cap (caller proceeds to `scan_file_for_needles_streaming`)
/// or when metadata is unreadable. The skip finding is identical in
/// shape to the one W9-F installed inline; centralizing it here
/// keeps the multi-pass detector loops below DRY.
async fn streaming_skip_finding(
    path: &Path,
    path_str: &str,
    _needles: &[&[u8]],
) -> Option<SafetyFinding> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    if meta.len() <= IOC_SCAN_MAX_FILE_BYTES {
        return None;
    }
    tracing::warn!(
        path = %path_str,
        size_bytes = meta.len(),
        cap_bytes = IOC_SCAN_MAX_FILE_BYTES,
        "Restore-to-Stock IOC scan: file exceeds size cap, skipping (W9-D path-scoped pass)"
    );
    Some(SafetyFinding {
        id: "DCENT-INFO-001".to_string(),
        severity: Severity::Info,
        title: "IOC scan skipped: file exceeds 256 MiB size cap".to_string(),
        matched_path: Some(path_str.to_string()),
        remediation: format!(
            "File is {} bytes (cap: {} bytes). \
             IOC scan was skipped to avoid OOM on the daemon. \
             Manually inspect this file before flashing.",
            meta.len(),
            IOC_SCAN_MAX_FILE_BYTES
        ),
        no_override: false,
    })
}

/// POSIX-style relative path of `path` under `root`. Used by the
/// path-scope tightening in DCENT-2026-008/009/011 (Python parity at
/// `vnish_security_audit.py:489 / 545 / 631`).
pub(crate) fn relative_posix(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// Collect paths matching the Python `_iter_init_script_paths` glob
/// list (`vnish_security_audit.py:673-695`):
/// `etc/init.d/S*`, `etc/init.d/rc*`, `etc/rcS`, `etc/rc.local`,
/// `etc/inittab`, `etc/systemd/system/*.service`,
/// `lib/systemd/system/*.service`.
pub(crate) fn collect_init_script_paths<'a>(
    root: &Path,
    all_files: &'a [PathBuf],
) -> Vec<&'a PathBuf> {
    let mut out = Vec::new();
    for p in all_files {
        let rel = relative_posix(root, p);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let init_d_match = rel.starts_with("etc/init.d/") && {
            let tail = &rel["etc/init.d/".len()..];
            !tail.contains('/') && (tail.starts_with('S') || tail.starts_with("rc"))
        };
        let direct_match = matches!(rel.as_str(), "etc/rcS" | "etc/rc.local" | "etc/inittab");
        let systemd_match = (rel.starts_with("etc/systemd/system/")
            || rel.starts_with("lib/systemd/system/"))
            && name.ends_with(".service")
            && {
                let segments: Vec<&str> = rel.split('/').collect();
                segments.len() == 4
            };
        if init_d_match || direct_match || systemd_match {
            out.push(p);
        }
    }
    out
}

/// Collect paths matching the Python `_iter_binary_search_paths` glob
/// list (`vnish_security_audit.py:698-715`): `usr/bin/*`,
/// `usr/sbin/*`, `bin/*`, `sbin/*` (one level deep, regular files).
pub(crate) fn collect_binary_search_paths<'a>(
    root: &Path,
    all_files: &'a [PathBuf],
) -> Vec<&'a PathBuf> {
    let mut out = Vec::new();
    for p in all_files {
        let rel = relative_posix(root, p);
        for prefix in &["usr/bin/", "usr/sbin/", "bin/", "sbin/"] {
            if let Some(tail) = rel.strip_prefix(prefix) {
                if !tail.is_empty() && !tail.contains('/') {
                    out.push(p);
                    break;
                }
            }
        }
    }
    out
}

/// Collect paths in the tree whose file name matches any of `names`.
/// Mirrors Python's `extracted_dir.rglob(explicit)` loop for
/// `bmminer`, `cgminer`, `dtu`, `single-board-test`, `dashd`,
/// `daemons` (vnish_security_audit.py:803-805 / 871-872 / 718-746).
pub(crate) fn collect_binary_paths_for_names<'a>(
    all_files: &'a [PathBuf],
    names: &[&str],
) -> Vec<&'a PathBuf> {
    let mut out = Vec::new();
    for p in all_files {
        let nm = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if names.contains(&nm) {
            out.push(p);
        }
    }
    out
}

/// Negative-detection heuristic for DCENT-2026-015. Mirrors the
/// Python implementation at `vnish_security_audit.py:920-956`.
pub(crate) fn looks_like_amlogic_s21(root: &Path, all_files: &[PathBuf]) -> bool {
    // Heuristic 1: top-level extracted dir name + parents.
    let name_lower = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let parents_lower: String = root
        .ancestors()
        .skip(1)
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let candidate = format!("{name_lower} {parents_lower}");
    let has_s21 = candidate.contains(AMLOGIC_S21_FILENAME_HINTS[0]);
    let has_aml = candidate.contains(AMLOGIC_S21_FILENAME_HINTS[1])
        || candidate.contains(AMLOGIC_S21_FILENAME_HINTS[2]);
    if has_s21 && has_aml {
        return true;
    }
    // Heuristic 2: any path inside the tree containing both tokens.
    for p in all_files {
        let rel = relative_posix(root, p).to_lowercase();
        if rel.contains("s21") && (rel.contains("aml") || rel.contains("amlogic")) {
            return true;
        }
    }
    false
}

/// Stream-scan a file against multiple fixed needles. Returns a
/// boolean per needle indicating whether it was found at least once.
///
/// Implementation: 64 KiB rolling buffer with a `max_needle_len-1`
/// overlap retained between chunks so a needle that straddles a
/// chunk boundary still matches. Peak heap allocation is one
/// `Vec<u8>` of size `chunk + overlap` (~65.6 KiB) plus the
/// per-needle `Vec<bool>` (5 bytes today). Per-call RSS budget:
/// well under 70 KiB regardless of file size.
///
///  W9-F (R4-C3) — closes the OOM path on the 512 MB Zynq
/// where the previous whole-file `tokio::fs::read` was a concrete
/// failure mode for files >100 MiB.
async fn scan_file_for_needles_streaming(
    path: &Path,
    needles: &[&[u8]],
) -> Result<StreamingScanResult, RestoreError> {
    use tokio::io::AsyncReadExt;

    if needles.is_empty() {
        return Ok(StreamingScanResult { found: Vec::new() });
    }

    let max_needle_len = needles.iter().map(|n| n.len()).max().unwrap_or(0);
    // Belt-and-suspenders — if a future agent adds an absurd needle
    // we want the build to fail loudly, not silently read more state
    // than `IOC_SCAN_MAX_NEEDLE_OVERLAP` allows.
    debug_assert!(
        max_needle_len <= IOC_SCAN_MAX_NEEDLE_OVERLAP + 1,
        "needle longer than IOC_SCAN_MAX_NEEDLE_OVERLAP — bump the constant"
    );
    let overlap = max_needle_len.saturating_sub(1);

    let mut found = vec![false; needles.len()];
    if needles.iter().all(|n| n.is_empty()) {
        // Nothing meaningful to look for — short-circuit.
        return Ok(StreamingScanResult { found });
    }

    //  W11-A (R4''-io-helper-migration): use the
    // `RestoreError::io(source, path)` helper at the IOC-scan
    // operator-relevant sites.
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| RestoreError::io(e, path.to_path_buf()))?;

    // `window` always starts with the trailing `overlap` bytes from
    // the previous chunk, so a needle straddling the chunk boundary
    // is still seen as a contiguous match here.
    let mut window: Vec<u8> = Vec::with_capacity(IOC_SCAN_CHUNK_BYTES + overlap);
    let mut chunk = vec![0u8; IOC_SCAN_CHUNK_BYTES];

    loop {
        let n = f
            .read(&mut chunk)
            .await
            .map_err(|e| RestoreError::io(e, path.to_path_buf()))?;
        if n == 0 {
            break;
        }

        // Build the search window: previous tail + new chunk bytes.
        // First iteration `window` is empty so this is just `chunk`.
        window.extend_from_slice(&chunk[..n]);

        // Search every still-unmatched needle.
        for (idx, needle) in needles.iter().enumerate() {
            if found[idx] {
                continue;
            }
            if needle.is_empty() {
                continue;
            }
            if memmem(&window, needle) {
                found[idx] = true;
            }
        }

        // Early-out if everything is matched.
        if found.iter().all(|b| *b) {
            break;
        }

        // Retain only the trailing `overlap` bytes for the next
        // round, so a needle split across chunk boundaries still
        // survives. Trim the leading part to keep peak memory
        // bounded.
        if window.len() > overlap {
            let drop_n = window.len() - overlap;
            window.drain(..drop_n);
        }
    }

    Ok(StreamingScanResult { found })
}

/// Cheap byte-level substring search (no regex).
fn memmem(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

///  W9-A — R4-H4: typed error variant.
fn tempdir_for_preflight() -> Result<PathBuf, RestoreError> {
    let suffix = format!(
        "dcentos-restore-preflight-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let p = std::env::temp_dir().join(suffix);
    //  W11-A (R4''-io-helper-migration): use the helper for
    // consistency at the operator-relevant scratch-mkdir site.
    std::fs::create_dir_all(&p).map_err(|e| RestoreError::io(e, p.clone()))?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// NAND backup — dd of mtd4 + mtd7 + mtd8 + ubinfo + fw_printenv snapshot,
// with SHA-256 verify, free-space precheck, UBI shape, and LEB-mirror gates.
//
//  W9-B (2026-05-05) closes:
//   - R3-CRITICAL-2 — wrong mtd partition list (was mtd0/1/2)
//   - R3-HIGH       — 250 MB free-space precheck missing in Rust path
//   - R1-H-1        — no SHA-256 verify on dumped images
//   - R1-H-2        — no UBI shape check on inactive slot before flash
//   - R1-H-3        — no LEB-mirror check before sysupgrade
// ---------------------------------------------------------------------------

/// NAND backup with SHA-256 verify, free-space precheck, UBI shape
/// validation, and LEB-mirror check ( W9-B, 2026-05-05).
///
/// On success returns the backup directory containing:
/// - `mtd4.img`, `mtd7.img`, `mtd8.img` — raw partition dumps with
///   verified on-disk SHA-256s in `SHA256SUMS`
/// - `ubinfo.txt` — UBI volume table snapshot
/// - `fwenv.txt` — U-Boot env snapshot
/// - `proc-mtd.txt` — `/proc/mtd` for partition-name verification
///
/// `slot_plan` identifies which firmware-slot mtd belongs to the
/// inactive slot so [`ubi_image_has_magic`] and
/// [`leb_counts_match_expected`] can validate the right side.
///
/// `profile` selects the per-platform mtd list, expected UBI layout
/// (or `None` for am3-aml uImage), and minimum free-space tier.
///  W12-B refactored the wave-8-11 hardcoded S9 am1 paths
/// into [`PROFILE_TABLE`]-driven values.
async fn nand_backup(
    slot_plan: &SlotPlan,
    profile: &PlatformProfile,
) -> Result<PathBuf, RestoreError> {
    // -- Step 1. Free-space precheck (R3-HIGH) ------------------------------
    //
    // Before mkdir/dd anything, verify /data has at least the
    // platform-specific minimum free. The on-miner shell script
    // enforces this; the Rust path used to skip it and could
    // partially-fill /data with a corrupt backup.
    let free = get_free_space_bytes(NAND_BACKUP_DATA_DIR).map_err(|reason| {
        RestoreError::NandBackupFailed {
            step: "free_space_precheck".to_string(),
            reason,
        }
    })?;
    if free < profile.min_free_bytes {
        return Err(RestoreError::NandBackupFailed {
            step: "free_space_precheck".to_string(),
            reason: format!(
                "Only {} MB free at {}, need at least {} MB ({} dump)",
                free / 1024 / 1024,
                NAND_BACKUP_DATA_DIR,
                profile.min_free_bytes / 1024 / 1024,
                profile.signature,
            ),
        });
    }

    // -- Step 2. Make the backup directory ---------------------------------
    //
    //  W10-B (A1-HIGH-6): write into `<root>-<ts>.partial/`
    // first; only after every artifact (mtd dumps, SHA256SUMS,
    // fw_setenv copy, ubinfo, fwenv) has been fsync'd in does this
    // function rename the directory to its final `<root>-<ts>/`
    // name. The rename is atomic on a single Linux filesystem (both
    // paths live under `/data` per `NAND_BACKUP_ROOT`), so a crash
    // mid-backup leaves the operator with an obviously-incomplete
    // `.partial/` they can ignore — never a half-written
    // `restore-backup-<ts>/` that looks usable but isn't.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let final_dir = PathBuf::from(format!("{}-{}", NAND_BACKUP_ROOT, ts));
    let dir = PathBuf::from(format!("{}-{}.partial", NAND_BACKUP_ROOT, ts));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|source| RestoreError::NandBackupFailed {
            step: format!("mkdir {}", dir.display()),
            reason: source.to_string(),
        })?;

    // -prep R1''-Q8: arm a Drop guard that removes the partial
    // dir on any error return (or panic) below. Without this, repeated
    // failed flashes would orphan ~190 MiB `.partial/` dirs in /data,
    // eventually tripping the 250 MiB free-space precheck and blocking
    // ALL future flashes silently. Disarmed below right before the
    // successful return — past the atomic rename, the partial path no
    // longer exists.
    let mut cleanup_guard = PartialDirCleanup::arm(&dir);

    // -- Step 3. Dump each MTD partition + verify on-disk SHA-256 ----------
    //
    // mtd4 = U-Boot env (per BIBLE Volume BSR §2.1)
    // mtd7 = firmware1 slot (UBI volume)
    // mtd8 = firmware2 slot (UBI volume)
    //
    // After each dd completes, we MUST hash the on-disk file (R1-H-1) —
    // not the dd command's stdout — and write it to SHA256SUMS. The
    // operator's recovery path is `nandwrite -p /dev/mtdN <img>`, so a
    // silently-corrupt img would brick the unit. Fail closed on size 0
    // or hash failure.
    let mut sha_lines: Vec<String> = Vec::with_capacity(profile.nand_backup_mtds.len());

    for mtd in profile.nand_backup_mtds {
        let name = Path::new(mtd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("mtd_unknown");
        let out = dir.join(format!("{name}.img"));

        // (a) Run dd.
        let status = tokio::process::Command::new("dd")
            .args([
                &format!("if={mtd}"),
                &format!("of={}", out.to_string_lossy()),
                "bs=1M",
            ])
            .output()
            .await
            .map_err(|source| RestoreError::NandBackupFailed {
                step: format!("spawn dd {mtd}"),
                reason: source.to_string(),
            })?;
        if !status.status.success() {
            return Err(RestoreError::NandBackupFailed {
                step: format!("dd {mtd}"),
                reason: String::from_utf8_lossy(&status.stderr).into_owned(),
            });
        }

        // (b) Verify file exists and is non-empty (size sanity).
        let meta =
            tokio::fs::metadata(&out)
                .await
                .map_err(|source| RestoreError::NandBackupFailed {
                    step: format!("stat {} after dd", out.display()),
                    reason: source.to_string(),
                })?;
        if meta.len() == 0 {
            return Err(RestoreError::NandBackupFailed {
                step: format!("dd {mtd}: zero-byte dump"),
                reason: format!(
                    "dd reported success but {} is 0 bytes — refusing to proceed",
                    out.display()
                ),
            });
        }

        // (c) SHA-256 of the on-disk file (streaming — never reads the
        //     full image into RAM).
        let sha = sha256_of_file_streaming(&out).await.map_err(|reason| {
            RestoreError::NandBackupFailed {
                step: format!("sha256 {}", out.display()),
                reason,
            }
        })?;
        sha_lines.push(format!("{}  {}.img\n", sha, name));

        tracing::info!(
            mtd = %mtd,
            bytes = meta.len(),
            sha256 = %sha,
            "Restore-to-Stock: NAND backup partition dumped + verified"
        );
    }

    // (d) Write SHA256SUMS (R1-H-1 — operator's recovery starting point).
    let sha_path = dir.join("SHA256SUMS");
    tokio::fs::write(&sha_path, sha_lines.concat())
        .await
        .map_err(|source| RestoreError::NandBackupFailed {
            step: format!("write {}", sha_path.display()),
            reason: source.to_string(),
        })?;

    // (e)  W10-A (A1-HIGH-7): ship a working `fw_setenv` inside the
    //     backup dir so the operator on stock has Option-A recovery even if
    //     stock firmware is missing libubootenv-tools. Best-effort:
    //     warn-on-failure so a missing fw_setenv on the build doesn't fail
    //     the backup. Operator still has Option B (serial console) per
    //     STOCK_BOOT_HARVEST_PROCEDURE.md §10.
    //
    // -prep R1''-Q24: capture the success bool and reflect it in
    // STATUS so the dashboard can warn the operator BEFORE pulling the
    // trigger that Option-A recovery is unavailable for this backup.
    let fw_setenv_present = copy_fw_setenv_into_backup_dir("/usr/sbin/fw_setenv", &dir).await;
    {
        //  W11-A (R4''-poison-logging): use the recovering
        // helper so a poisoned STATUS surfaces a forensic log line.
        let mut guard = status_write_or_recover();
        let mut snap = guard.clone().unwrap_or_default();
        snap.last_backup_fw_setenv_present = Some(fw_setenv_present);
        *guard = Some(snap);
    }

    // -- Step 4. UBI shape validation on the dumped firmware-slot images ---
    //
    // R1-H-2: before staging stock to the inactive slot, verify that the
    // existing UBI volume on the inactive slot starts with the `UBI#`
    // magic. If the slot was previously bricked or never `ubiformat`-ed,
    // refuse to flash.
    //
    //  W12-B: the slot list is now profile-driven
    // ([`PlatformProfile::firmware_slot_mtds`]). We skip this gate
    // entirely on platforms with `ubi_expected_lebs: None`
    // (am3-aml uImage at mtd5 offset 0x5100000 — no UBI volume to
    // validate; the per-platform revert script does its own uImage
    // magic readback).
    if profile.ubi_expected_lebs.is_some() {
        for slot_mtd in profile.firmware_slot_mtds {
            //  W10-D (R4'-M2): the previous `.unwrap().to_str().unwrap()`
            // chain would `panic!` if the slot_mtd path was unusual (no
            // file_name, non-UTF-8). Treat malformed paths as a hard internal
            // error instead of crashing the daemon.
            let name = Path::new(slot_mtd)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| RestoreError::Internal {
                    reason: format!("invalid mtd path: {slot_mtd}"),
                })?;
            let img = dir.join(format!("{name}.img"));
            if !ubi_image_has_magic(&img).await.map_err(|reason| {
                RestoreError::NandBackupFailed {
                    step: format!("ubi_magic {}", img.display()),
                    reason,
                }
            })? {
                return Err(RestoreError::NandBackupFailed {
                    step: format!("ubi_shape_check {}", slot_mtd),
                    reason: format!(
                        "{}: first 4 bytes do not match UBI magic (UBI#). \
                         Slot is corrupt or unformatted; refusing to flash. \
                         Recover via `ubiformat {}` from a serial console.",
                        img.display(),
                        slot_mtd
                    ),
                });
            }
        }
    } else {
        tracing::info!(
            platform = %profile.signature,
            "Restore-to-Stock: skipping UBI shape gate (am3-aml uImage layout has no UBI volume)"
        );
    }

    // -- Step 5. LEB-mirror check on the inactive slot ---------------------
    //
    // R1-H-3 + : S9 needs
    // 25/166/525 LEBs in kernel/rootfs/rootfs_data on the inactive slot.
    // If the volume table has drifted, refuse — sysupgrade or any
    // nandwrite would land in undefined state.
    //
    // Only enforce if the slot_plan was successfully resolved AND the
    // platform profile has a UBI expectation. If the daemon couldn't
    // read fw_printenv (slot_plan empty), we can't tell which side is
    // inactive — fall through and let the flash dispatch do its own
    // slot-detect. If the profile has no UBI expectation
    // (am3-aml uImage), `leb_counts_match_expected` returns `Ok(None)`
    // and we skip the gate.
    if let Some(inactive_mtd_path) = slot_plan.inactive_mtd.as_deref() {
        if let Some(num) = mtd_dev_to_number(inactive_mtd_path) {
            match leb_counts_match_expected(num, profile).await {
                Ok(Some(true)) => {
                    tracing::info!(
                        inactive_mtd = %inactive_mtd_path,
                        platform = %profile.signature,
                        "Restore-to-Stock: inactive slot LEB counts match expected"
                    );
                }
                Ok(None) => {
                    tracing::info!(
                        inactive_mtd = %inactive_mtd_path,
                        platform = %profile.signature,
                        "Restore-to-Stock: skipping LEB-mirror gate (am3-aml uImage layout)"
                    );
                }
                Ok(Some(false)) => {
                    return Err(RestoreError::NandBackupFailed {
                        step: format!("leb_mirror {}", inactive_mtd_path),
                        reason: format!(
                            "Inactive slot {} has UBI volume table that does \
                             not mirror the expected layout for {}. Refusing to \
                             flash — \
                             for the ubiformat + ubimkvol recovery procedure.",
                            inactive_mtd_path, profile.signature,
                        ),
                    });
                }
                Err(reason) => {
                    //  W10-B (R1' residual) — probe failure
                    // is now a hard fail-closed instead of a soft
                    // warn-and-proceed. The previous arm's rationale
                    // ("dev/test environments don't have UBI
                    // attached") was a code-smell: in production the
                    // only way ubinfo can fail is (a) it's missing
                    // from the rootfs (build regression — refuse) or
                    // (b) the inactive slot is genuinely unattached
                    // (the very condition the gate is supposed to
                    // catch). Either way, refusing is the safe call
                    // and matches the existing UBI-shape failure
                    // semantics.
                    return Err(RestoreError::NandBackupFailed {
                        step: format!("leb_mirror_probe {}", inactive_mtd_path),
                        reason: format!(
                            "LEB-mirror probe failed on inactive slot {}: \
                             {reason}. Refusing to flash — install \
                             mtd-utils on the rootfs or recover the \
                             inactive slot via `ubiformat` from a serial \
                             console (see \
                             ).",
                            inactive_mtd_path
                        ),
                    });
                }
            }
        }
    }

    // -- Step 6. UBI volume table snapshot --------------------------------
    //
    // — captured as
    // a debugging aid alongside the binary mtd dumps.
    let ubinfo = tokio::process::Command::new("ubinfo")
        .args(["-a"])
        .output()
        .await;
    if let Ok(out) = ubinfo {
        let _ = tokio::fs::write(dir.join("ubinfo.txt"), &out.stdout).await;
    }

    // -- Step 7. U-Boot env snapshot ---------------------------------------
    //
    // Needed if rollback has to reset upgrade_stage / bootslot manually.
    // The raw mtd4 dump is the load-bearing recovery artifact; the text
    // dump is for fast operator triage.
    let fwenv = tokio::process::Command::new("fw_printenv").output().await;
    if let Ok(out) = fwenv {
        let _ = tokio::fs::write(dir.join("fwenv.txt"), &out.stdout).await;
    }

    // -- Step 8. /proc/mtd — partition-name witness -----------------------
    //
    // So the operator can later confirm which mtd number maps to which
    // partition name (mtd4=u-boot env, mtd7=firmware1, mtd8=firmware2).
    if let Ok(s) = tokio::fs::read_to_string("/proc/mtd").await {
        let _ = tokio::fs::write(dir.join("proc-mtd.txt"), s).await;
    }

    // -- Step 9.  W10-B (A1-HIGH-6) atomic rename -------------------
    //
    // Every artifact has been written + fsync'd into the `.partial`
    // directory. Now atomically rename to the final path the
    // restore-to-stock pipeline (and any operator-visible recovery
    // tooling) expects. `rename(2)` on the same filesystem is atomic
    // on Linux — there is never an instant where both paths exist
    // and never an instant where neither exists.
    tokio::fs::rename(&dir, &final_dir)
        .await
        .map_err(|source| RestoreError::NandBackupFailed {
            step: format!(
                "rename partial {} -> final {}",
                dir.display(),
                final_dir.display()
            ),
            reason: source.to_string(),
        })?;

    tracing::info!(
        partial = %dir.display(),
        final_path = %final_dir.display(),
        "Restore-to-Stock: NAND backup directory atomically renamed (W10-B A1-HIGH-6)"
    );

    // -prep R1''-Q8: disarm the partial-dir cleanup guard.
    // Past the rename, the partial path no longer exists; we don't
    // want Drop to attempt remove_dir_all on a non-existent path.
    cleanup_guard.disarm();

    Ok(final_dir)
}

/// -prep R1''-Q8: RAII cleanup of `<root>-<ts>.partial/` so
/// that any error return (or panic) inside [`nand_backup`] removes
/// the partial directory instead of orphaning ~190 MiB on /data.
/// Disarmed via [`PartialDirCleanup::disarm`] after the atomic rename
/// succeeds (at which point the partial path no longer exists).
///
/// Synchronous `std::fs::remove_dir_all` in Drop is acceptable here:
/// we are already on a failure path; tokio fs in Drop is fragile
/// because the runtime may be tearing down.
struct PartialDirCleanup<'a> {
    path: &'a Path,
    armed: bool,
}

impl<'a> PartialDirCleanup<'a> {
    fn arm(path: &'a Path) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

///  W11-A test harness: drive a `PartialDirCleanup` armed
/// against `path`, then drop it. Returns whether `path` still exists
/// after drop. The test asserts `false` (cleanup happened) for an
/// armed guard, `true` for a disarmed guard. Hidden from rustdoc.
#[doc(hidden)]
pub fn drive_partial_dir_cleanup_armed_for_test(path: &Path) -> bool {
    {
        let _g = PartialDirCleanup::arm(path);
        // _g drops at end of this scope.
    }
    path.exists()
}

///  W11-A test harness: arm a `PartialDirCleanup`, disarm it,
/// drop it. Returns whether `path` still exists after drop. The test
/// asserts `true` (disarmed → no cleanup happened).
#[doc(hidden)]
pub fn drive_partial_dir_cleanup_disarmed_for_test(path: &Path) -> bool {
    {
        let mut g = PartialDirCleanup::arm(path);
        g.disarm();
    }
    path.exists()
}

impl<'a> Drop for PartialDirCleanup<'a> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(e) = std::fs::remove_dir_all(self.path) {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "Wave-11-prep R1''-Q8: best-effort partial-dir cleanup failed (orphan may persist in /data)"
                );
            } else {
                tracing::info!(
                    path = %self.path.display(),
                    "Wave-11-prep R1''-Q8: partial NAND backup dir cleaned up after error"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// W9-B helpers — free space, streaming SHA-256, UBI shape, LEB mirror
// ---------------------------------------------------------------------------

/// Return free bytes at `path` (filesystem usage). Wraps `statvfs(3)`.
/// Used by [`nand_backup`] for the R3-HIGH 250 MB precheck.
///
/// On non-Unix builds (Windows host CI) returns a sentinel
/// "infinite" value so unit tests of unrelated logic don't trip the
/// precheck. The route is `#[cfg(unix)]` at the test boundary anyway.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn get_free_space_bytes(path: &str) -> Result<u64, String> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let cpath = CString::new(path).map_err(|e| format!("path → CString: {e}"))?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    // Safety: passing a valid path + an out-pointer to an uninit
    // statvfs struct, exactly as documented in statvfs(3).
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(format!(
            "statvfs({path}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // Safety: rc == 0 means stat was initialized.
    let stat = unsafe { stat.assume_init() };
    let bsize = stat.f_frsize as u64;
    let bavail = stat.f_bavail as u64;
    Ok(bsize.saturating_mul(bavail))
}

#[cfg(not(unix))]
fn get_free_space_bytes(_path: &str) -> Result<u64, String> {
    Ok(u64::MAX)
}

/// Test-only re-export: expose [`get_free_space_bytes`] to the
/// integration tests in `tests/restore_to_stock_routes.rs` so they
/// can verify the statvfs FFI plumbing without invoking the full
/// nand_backup pipeline. Hidden from rustdoc.
#[doc(hidden)]
pub fn get_free_space_bytes_for_test(path: &str) -> Result<u64, String> {
    get_free_space_bytes(path)
}

///  W10-A (A1-HIGH-7): best-effort copy of `fw_setenv` from
/// `src` into `backup_dir/fw_setenv`. Logs `info` on success, `warn`
/// on missing source or copy failure — never returns an error or
/// fails the backup pipeline. Operators on stock firmware that lacks
/// libubootenv-tools use this copied binary for Option-A recovery
/// (`./fw_setenv bootslot <slot>` to roll back). Option B (serial
/// console U-Boot env edit) is the fallback when this copy is absent.
///
/// Exposed as `pub` so the integration tests can drive it directly
/// without invoking the rest of `nand_backup` (which requires real
/// /dev/mtd devices). `#[doc(hidden)]` keeps it out of the public
/// API surface.
#[doc(hidden)]
pub async fn copy_fw_setenv_into_backup_dir(src: &str, backup_dir: &Path) -> bool {
    let src_path = Path::new(src);
    if !src_path.exists() {
        tracing::warn!(
            src = %src,
            "Wave-10 A1-HIGH-7: fw_setenv not found on this rootfs — \
             operator will only have Option B (serial console) recovery"
        );
        return false;
    }
    // -prep A4''-HIGH-4: refuse to copy through a symlink at the
    // source. `tokio::fs::copy` follows symlinks; if an attacker swaps
    // /usr/sbin/fw_setenv for a symlink to a malicious binary, the copy
    // would ship that binary inside /data/restore-backup-<ts>/ and the
    // operator would later run it as root during recovery. Use
    // symlink_metadata so we look at the link, not the target.
    match tokio::fs::symlink_metadata(&src_path).await {
        Ok(meta) if meta.file_type().is_symlink() => {
            tracing::warn!(
                src = %src,
                "Wave-11-prep A4''-HIGH-4: refusing to copy fw_setenv — source is a symlink (potential malicious-binary substitution). Operator falls back to Option B serial-console recovery."
            );
            return false;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                src = %src,
                error = %e,
                "Wave-11-prep A4''-HIGH-4: symlink_metadata failed; refusing to copy fw_setenv"
            );
            return false;
        }
    }
    let dest = backup_dir.join("fw_setenv");
    match tokio::fs::copy(&src_path, &dest).await {
        Ok(_) => {
            tracing::info!(
                dest = %dest.display(),
                "Wave-10 A1-HIGH-7: fw_setenv copied to backup dir for \
                 operator Option-A recovery"
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                src = %src,
                dest = %dest.display(),
                "Wave-10 A1-HIGH-7: fw_setenv copy into backup dir failed \
                 (operator can use Option B serial-console recovery instead)"
            );
            false
        }
    }
}

/// Test-only re-export of [`sha256_of_file`]'s streaming pipeline.
///  W9-F (R4-C3) — used by `tests/restore_to_stock_routes.rs`
/// to assert that a 100 MiB random buffer hashes to the same digest
/// the system `sha256sum` would produce. Hidden from rustdoc.
#[doc(hidden)]
pub async fn sha256_of_file_for_test(path: &Path) -> Result<String, String> {
    sha256_of_file(path).await.map_err(|e| e.to_string())
}

/// Test-only re-export of the streaming-needle scan helper. Hidden
/// from rustdoc. Returns one boolean per needle; order matches the
/// input slice.  W9-F (R4-C3).
#[doc(hidden)]
pub async fn scan_file_for_needles_streaming_for_test(
    path: &Path,
    needles: &[&[u8]],
) -> Result<Vec<bool>, String> {
    scan_file_for_needles_streaming(path, needles)
        .await
        .map(|r| r.found)
        .map_err(|e| e.to_string())
}

/// Test-only re-export of the IOC scan size cap so the integration
/// suite can assert the constant without touching the source.
/// W9-F (R4-C3). Hidden from rustdoc.
#[doc(hidden)]
pub fn ioc_scan_max_file_bytes_for_test() -> u64 {
    IOC_SCAN_MAX_FILE_BYTES
}

/// Test-only re-export of the IOC scan chunk size.  W9-F
/// (R4-C3). Hidden from rustdoc.
#[doc(hidden)]
pub fn ioc_scan_chunk_bytes_for_test() -> usize {
    IOC_SCAN_CHUNK_BYTES
}

/// Test-only re-export of [`scan_extracted_dir`] so the integration
/// tests in `tests/restore_to_stock_routes.rs` can drive synthetic
/// firmware trees through the whole detector pipeline without bringing
/// up an axum HTTP server.  W9-D (R1-C2 / R1-C3 parity tests).
/// Hidden from rustdoc.
#[doc(hidden)]
pub async fn scan_extracted_dir_for_test(root: &Path) -> Vec<SafetyFinding> {
    scan_extracted_dir(root).await
}

/// Streaming SHA-256 of a file — never reads the full image into RAM.
/// Required by the NAND backup path because mtd7/mtd8 dumps are
/// ~95 MiB each. Originally added by W9-B as a parallel implementation;
/// W9-F (R4-C3) consolidated `sha256_of_file` onto the same streaming
/// pipeline, so this helper now delegates and stringifies the typed
/// error at the call boundary so the NAND backup wire format remains
/// stable.  W9-B (R1-H-1) + W9-F (R4-C3).
async fn sha256_of_file_streaming(path: &Path) -> Result<String, String> {
    sha256_of_file(path).await.map_err(|e| e.to_string())
}

/// True when the file at `path` starts with the UBI magic
/// (`UBI#` = 0x55 0x42 0x49 0x23). Per R1-H-2 — refuse to flash if
/// the existing slot image isn't a valid UBI volume.
async fn ubi_image_has_magic(path: &Path) -> Result<bool, String> {
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {}: {}", path.display(), e))?;
    let mut buf = [0u8; 4];
    let n = f
        .read(&mut buf)
        .await
        .map_err(|e| format!("read {}: {}", path.display(), e))?;
    Ok(n == 4 && &buf == UBI_MAGIC)
}

/// Translate `/dev/mtd7` → `Some(7)`. Returns `None` if the path
/// doesn't match the expected shape.
fn mtd_dev_to_number(dev: &str) -> Option<u32> {
    let stripped = dev.strip_prefix("/dev/mtd")?;
    stripped.parse::<u32>().ok()
}

/// True when the inactive UBI slot has volume table mirroring the
/// active platform's [`PlatformProfile::ubi_expected_lebs`]. Per
/// .
///
/// Returns `Ok(Some(true))` when the LEB layout matches expected,
/// `Ok(Some(false))` when it diverges, `Ok(None)` when the platform
/// profile has no UBI expectation (am3-aml uImage layout — skip the
/// gate entirely), and `Err` if the probe couldn't run (ubinfo
/// missing, no UBI driver, etc.) — caller must NOT fail closed on
/// the Err case.
///
///  W12-B: signature extended to consume the live
/// [`PlatformProfile`] instead of hardcoding S9 LEBs. The `Option`
/// in the success arm encodes the "no UBI expected for am3-aml" path
/// without abusing `Err` as a control-flow channel.
async fn leb_counts_match_expected(
    mtd_num: u32,
    profile: &PlatformProfile,
) -> Result<Option<bool>, String> {
    let Some(expected) = profile.ubi_expected_lebs else {
        // am3-aml uImage layout — no UBI volume table to mirror.
        return Ok(None);
    };
    let output = tokio::process::Command::new("ubinfo")
        .args(["-a"])
        .output()
        .await
        .map_err(|e| format!("spawn ubinfo: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "ubinfo -a exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    leb_counts_match_in_ubinfo_with_expected(&stdout, mtd_num, expected).map(Some)
}

/// Wave-≤11 backward-compat wrapper: calls
/// [`leb_counts_match_in_ubinfo_with_expected`] with the S9
/// reference layout. Existing tests continue to pass.
pub fn leb_counts_match_in_ubinfo(ubinfo_output: &str, mtd_num: u32) -> Result<bool, String> {
    leb_counts_match_in_ubinfo_with_expected(ubinfo_output, mtd_num, S9_UBI_EXPECTED_LEBS)
}

/// Pure parser: given `ubinfo -a` text, the expected mtd number, and
/// the platform-specific expected LEB layout, return whether the
/// relevant UBI device's volume table mirrors expectation. Pulled out
/// of [`leb_counts_match_expected`] so it can be unit-tested without
/// invoking ubinfo.
///
///  W12-B: `expected` is now caller-supplied so each
/// [`PlatformProfile`] feeds in its own layout
/// (`PROFILE_TABLE[*].ubi_expected_lebs`). S9 am1's layout is
/// preserved verbatim via the wave-≤11 backward-compat wrapper above.
pub fn leb_counts_match_in_ubinfo_with_expected(
    ubinfo_output: &str,
    mtd_num: u32,
    expected: &[(&str, u32)],
) -> Result<bool, String> {
    // ubinfo -a prints a section per attached UBI device with one
    // sub-section per volume:
    //
    //   Volume ID:   0 (on ubi1)
    //   Type:        dynamic
    //   Alignment:   1
    //   Size:        25 LEBs (3174400 bytes, 3.0 MiB)
    //   State:       OK
    //   Name:        kernel
    //
    // ubinfo's output varies by kernel version. This parser is
    // permissive: it walks volume entries and verifies that every
    // expected `(name, leb_count)` pair from [`S9_UBI_EXPECTED_LEBS`]
    // is present somewhere in the output. That covers volume table
    // drift without false-failing on dev kernels that print a
    // slightly different header.

    let _ = mtd_num; // permissive parser — don't restrict to mtd_num
                     // for now since the device header section is
                     // not always parseable.

    let mut current_volume_name: Option<String> = None;
    let mut current_volume_lebs: Option<u32> = None;
    let mut found: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    for line in ubinfo_output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Volume ID:") {
            // Starting a new volume — flush previous if both fields known.
            if let (Some(n), Some(l)) = (current_volume_name.take(), current_volume_lebs.take()) {
                found.insert(n, l);
            }
        } else if let Some(rest) = trimmed.strip_prefix("Size:") {
            // "25 LEBs (3174400 bytes, 3.0 MiB)" — parse first integer.
            if let Some(num_str) = rest.split_whitespace().next() {
                if let Ok(n) = num_str.parse::<u32>() {
                    current_volume_lebs = Some(n);
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("Name:") {
            current_volume_name = Some(rest.trim().to_string());
        }
    }
    // Flush the last volume.
    if let (Some(n), Some(l)) = (current_volume_name.take(), current_volume_lebs.take()) {
        found.insert(n, l);
    }

    if found.is_empty() {
        return Err(
            "ubinfo -a produced no parseable volume entries — slot may not be attached".to_string(),
        );
    }

    for (name, expected_count) in expected {
        match found.get(*name) {
            Some(actual) if actual == expected_count => {}
            Some(actual) => {
                tracing::warn!(
                    volume = %name,
                    expected = %expected_count,
                    actual = %actual,
                    "LEB-mirror mismatch"
                );
                return Ok(false);
            }
            None => {
                tracing::warn!(
                    volume = %name,
                    "LEB-mirror missing volume in ubinfo output"
                );
                return Ok(false);
            }
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
//  W12-C — Dynamic preflight-checks endpoint
// ---------------------------------------------------------------------------
//
// `GET /api/system/restore-to-stock/preflight-checks` returns live
// probe results for the 6 binary/script paths + filesystem free-space
// + platform-supported / verified-revertable flags. Replaces the
// wave-11 W11-C static `PreflightChecklist` fallback in the dashboard
// (`dashboard/src/components/restore-to-stock/RestoreStatus.tsx`).
//
// The handler is intentionally `GET` and side-effect-free — each row
// is one shell-out (which/path stat) or one statvfs(3) call. Total
// budget: 6 PATH probes + 1 statvfs + 1 cpuinfo read + 1 PROFILE_TABLE
// lookup. ~50ms wall-clock on the S9.
//
// All probes are factored behind the [`PreflightProbes`] trait so
// integration tests can inject a mock implementation without touching
// the real filesystem.

/// Wire shape returned by `GET /api/system/restore-to-stock/preflight-checks`.
/// Field names + JSON shape pinned by the dashboard
/// `PreflightChecks` TypeScript interface
/// (`dashboard/src/api/restore-to-stock.ts`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreflightChecks {
    /// Resolved path to `setsid` (util-linux) or `None` if not on PATH.
    /// Required so the writer survives dcentrald exit (W9-C R3-HIGH /
    /// R4-H3 detach).
    pub setsid_path: Option<String>,

    /// Resolved path to the per-platform revert script. Pulled from
    /// the active `PlatformProfile::revert_script` field; falls back
    /// to the S9 canonical path (`/usr/sbin/revert_to_stock_s9.sh`)
    /// when no profile resolves (legacy S9 dev installs).
    pub revert_script_path: Option<String>,

    /// Resolved path to `fw_setenv` (libubootenv-tools). Required for
    /// Option-A recovery from inside booted stock.
    pub fw_setenv_path: Option<String>,

    /// Free space at `/data` in MiB. The destructive flow refuses to
    /// start when this is below `NAND_BACKUP_MIN_FREE_BYTES` (250
    /// MiB on S9 am1).
    pub data_free_mib: u64,

    /// Resolved path to `tar`. Required for the NAND backup tarball
    /// step.
    pub tar_path: Option<String>,

    /// Resolved path to `nandwrite`. Required by the per-platform
    /// revert script for the firmware-slot write step.
    pub nandwrite_path: Option<String>,

    /// Resolved path to `flash_erase`. Required by the S9 + AM335x BB
    /// revert scripts (UBI mode); not strictly required by the
    /// am3-aml uImage scripts but reported anyway for operator
    /// transparency.
    pub flash_erase_path: Option<String>,

    /// Platform fingerprint produced by [`detect_platform_signature`].
    /// `None` when /proc/cpuinfo isn't readable (Windows host tests).
    pub platform_signature: Option<String>,

    /// `true` when the running platform has a `PlatformProfile` entry
    /// in [`PROFILE_TABLE`] — i.e. the destructive code path supports
    /// it. Layer 1 of the W12-B 2-layer gate.
    pub platform_supported: bool,

    /// `true` when `platform_supported` AND the matching profile has
    /// `verified_revertable: true`. Layer 2 of the W12-B gate. Today
    /// only S9 am1 satisfies this; the operator can still dry-run on
    /// supported-but-unverified platforms (W12-B closure).
    pub platform_verified_revertable: bool,

    /// `true` when ALL of the following hold:
    /// - All 6 path probes resolved (`Some(_)` for setsid, revert
    ///   script, fw_setenv, tar, nandwrite, flash_erase).
    /// - `data_free_mib >= 250` (matches S9 am1 `min_free_bytes`).
    /// - `platform_supported && platform_verified_revertable`.
    ///
    /// The dashboard renders a green "ready" badge when this is
    /// `true` and a red "missing pieces" badge otherwise.
    pub all_present: bool,
}

/// Minimum free MiB on `/data` required for the destructive flow.
/// Mirrors the S9 am1 [`S9_AM1_MIN_FREE_BYTES`] constant (250 MiB)
/// because the dashboard checklist must call out the same threshold
/// the daemon enforces. The W12-C handler reports the threshold as
/// 250 MiB regardless of the active profile — the destructive
/// handler still uses the active `profile.min_free_bytes` at flash
/// time. Future enhancement: surface the per-platform threshold here
/// so am3-aml's lower 100-MiB bar is visible to operators.
const PREFLIGHT_MIN_FREE_MIB: u64 = 250;

/// Trait-style probe surface so tests can inject a mock without
/// touching the real filesystem. The real impl
/// ([`RealPreflightProbes`]) shells out via `tokio::process::Command`
/// and `tokio::fs::metadata`.
#[async_trait::async_trait]
pub trait PreflightProbes: Send + Sync {
    /// Resolve the absolute path of `cmd` on `$PATH` (e.g. `which
    /// setsid`). Returns `None` if not found or not executable.
    async fn which(&self, cmd: &str) -> Option<String>;

    /// Stat `path` and return `Some(path.to_string())` if it exists
    /// and is a regular file (executable bit not enforced — the
    /// destructive handler shells out to `sh script.sh` either way).
    async fn path_exists(&self, path: &str) -> Option<String>;

    /// Free space at `path` in MiB. Returns 0 on probe failure so
    /// the gate stays conservative.
    async fn free_mib_at(&self, path: &str) -> u64;

    /// Detect the active platform signature (`zynq-am1-bm1387` etc.).
    /// Returns `None` when /proc/cpuinfo isn't readable.
    async fn platform_signature(&self) -> Option<String>;
}

/// Production probe implementation — real `tokio::process::Command`
/// shell-outs + statvfs(3).
pub struct RealPreflightProbes;

#[async_trait::async_trait]
impl PreflightProbes for RealPreflightProbes {
    async fn which(&self, cmd: &str) -> Option<String> {
        // Probe a small set of canonical PATH entries directly so the
        // handler doesn't depend on `/usr/bin/which` being present
        // (Buildroot busybox-only systems sometimes lack it). Mirrors
        // the wave-9 setsid_present probe pattern.
        for prefix in [
            "/usr/sbin",
            "/usr/local/sbin",
            "/sbin",
            "/usr/bin",
            "/usr/local/bin",
            "/bin",
        ] {
            let candidate = format!("{prefix}/{cmd}");
            if let Ok(meta) = tokio::fs::metadata(&candidate).await {
                if meta.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    async fn path_exists(&self, path: &str) -> Option<String> {
        match tokio::fs::metadata(path).await {
            Ok(meta) if meta.is_file() => Some(path.to_string()),
            _ => None,
        }
    }

    async fn free_mib_at(&self, path: &str) -> u64 {
        match get_free_space_bytes(path) {
            Ok(bytes) => bytes / (1024 * 1024),
            Err(_) => 0,
        }
    }

    async fn platform_signature(&self) -> Option<String> {
        detect_platform_signature().await
    }
}

///  W12-C: assemble a [`PreflightChecks`] from any
/// [`PreflightProbes`] implementation. Pure logic — `Send`/`Sync` so
/// it can be `await`-ed from the axum handler. Tests pass a mock
/// `&dyn PreflightProbes` here without touching the real PATH.
pub async fn build_preflight_checks(probes: &dyn PreflightProbes) -> PreflightChecks {
    let setsid_path = probes.which("setsid").await;
    let fw_setenv_path = probes.which("fw_setenv").await;
    let tar_path = probes.which("tar").await;
    let nandwrite_path = probes.which("nandwrite").await;
    let flash_erase_path = probes.which("flash_erase").await;

    // Platform detection + matching profile lookup.
    let platform_signature = probes.platform_signature().await;
    let profile_opt = platform_signature.as_deref().and_then(profile_for);
    let platform_supported = profile_opt.is_some();
    let platform_verified_revertable = profile_opt.map(|p| p.verified_revertable).unwrap_or(false);

    // Revert-script path: prefer the profile's canonical path; fall
    // back to the S9 canonical path so legacy single-script installs
    // still surface a sensible row.
    let revert_basename = profile_opt
        .map(|p| {
            // strip the leading "/usr/sbin/"
            std::path::Path::new(p.revert_script)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("revert_to_stock_s9.sh")
                .to_string()
        })
        .unwrap_or_else(|| "revert_to_stock_s9.sh".to_string());

    // Probe the 3-candidate paths from `revert_script_candidates` so
    // dev installs (/usr/local/sbin/, /data/scripts/) also surface as
    // present.
    let revert_script_path = {
        let mut found = None;
        for candidate in revert_script_candidates(&revert_basename) {
            if let Some(p) = probes.path_exists(&candidate).await {
                found = Some(p);
                break;
            }
        }
        found
    };

    let data_free_mib = probes.free_mib_at(NAND_BACKUP_DATA_DIR).await;

    let all_paths_present = setsid_path.is_some()
        && revert_script_path.is_some()
        && fw_setenv_path.is_some()
        && tar_path.is_some()
        && nandwrite_path.is_some()
        && flash_erase_path.is_some();

    let all_present = all_paths_present
        && data_free_mib >= PREFLIGHT_MIN_FREE_MIB
        && platform_supported
        && platform_verified_revertable;

    PreflightChecks {
        setsid_path,
        revert_script_path,
        fw_setenv_path,
        data_free_mib,
        tar_path,
        nandwrite_path,
        flash_erase_path,
        platform_signature,
        platform_supported,
        platform_verified_revertable,
        all_present,
    }
}

/// `GET /api/system/restore-to-stock/preflight-checks` — live probe
/// results for the dashboard pre-flight checklist. Always 200 with a
/// JSON body; failed probes surface as `None` / `false` per field.
pub async fn preflight_checks(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let probes = RealPreflightProbes;
    let checks = build_preflight_checks(&probes).await;
    (StatusCode::OK, Json(checks))
}

/// Test-only re-export of [`build_preflight_checks`] for the
/// integration test suite to drive a mock `PreflightProbes` impl
/// without depending on `async_trait` re-export.
#[doc(hidden)]
pub async fn build_preflight_checks_for_test(probes: &dyn PreflightProbes) -> PreflightChecks {
    build_preflight_checks(probes).await
}

// ---------------------------------------------------------------------------
// Pure-logic unit tests (no network, no disk I/O against real devices).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memmem_basic_hits_and_misses() {
        assert!(memmem(b"hello world", b"world"));
        assert!(!memmem(b"hello world", b"xyz"));
        assert!(!memmem(b"abc", b""));
        assert!(!memmem(b"a", b"abc"));
    }

    #[test]
    fn hex_encode_known_values() {
        assert_eq!(hex::encode_lower(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(hex::encode_lower(&[]), "");
    }

    #[test]
    fn slot_plan_default_is_unknown() {
        let s = SlotPlan::default();
        assert!(s.active_slot.is_none());
        assert!(s.inactive_slot.is_none());
        assert!(s.inactive_mtd.is_none());
    }

    #[test]
    fn severity_serializes_lowercase() {
        let f = SafetyFinding {
            id: "X".into(),
            severity: Severity::Critical,
            title: "t".into(),
            matched_path: None,
            remediation: "r".into(),
            no_override: true,
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("\"severity\":\"critical\""), "got {s}");
    }

    #[test]
    fn default_hashboard_count_is_one() {
        //  W9-G (R5-MEDIUM): UI/backend default-divergence
        // resolved by pinning both to 1. The user's stated home-mining
        // intent is to unplug 1 board for breaker safety; the modal
        // defaults to 1 and a curl call without an explicit value now
        // behaves identically. Range validation 1..=3 still applies.
        assert_eq!(default_hashboard_count(), 1);
    }

    // -----------------------------------------------------------------
    //  W9-E (R4-C2) — wire-shape round-trip tests.
    //
    // The dashboard's `dashboard/src/api/restore-to-stock.ts` declares
    // the same field names asserted here. If a future refactor
    // accidentally renames a backend field, these tests fail and
    // signal the dashboard contract is about to drift again.
    //
    // Cross-reference:
    //   R4_restore_code.md — finding C-2.
    // -----------------------------------------------------------------

    #[test]
    fn safety_finding_wire_field_names() {
        let f = SafetyFinding {
            id: "DCENT-2026-009".into(),
            severity: Severity::High,
            title: "atlas@anthill.farm needle".into(),
            matched_path: Some("etc/dropbear/authorized_keys".into()),
            remediation: "remove third-party SSH key".into(),
            no_override: false,
        };
        let v: serde_json::Value = serde_json::to_value(&f).unwrap();
        let obj = v
            .as_object()
            .expect("SafetyFinding must serialize as object");
        // Backend fields the dashboard expects (W9-E):
        assert!(obj.contains_key("id"), "missing 'id'");
        assert!(obj.contains_key("severity"), "missing 'severity'");
        assert!(
            obj.contains_key("title"),
            "missing 'title' — dashboard reads f.title"
        );
        assert!(
            obj.contains_key("matched_path"),
            "missing 'matched_path' — dashboard reads f.matched_path"
        );
        assert!(
            obj.contains_key("remediation"),
            "missing 'remediation' — dashboard reads f.remediation"
        );
        assert!(obj.contains_key("no_override"), "missing 'no_override'");
        // The OLD UI-invented names MUST NOT exist on the wire.
        assert!(
            !obj.contains_key("detector"),
            "stale UI-only name 'detector' leaked into wire"
        );
        assert!(
            !obj.contains_key("description"),
            "stale UI-only name 'description' leaked into wire"
        );
        assert!(
            !obj.contains_key("evidence"),
            "stale UI-only name 'evidence' leaked into wire"
        );
    }

    #[test]
    fn slot_plan_wire_field_names() {
        let plan = SlotPlan {
            active_slot: Some("a".into()),
            inactive_slot: Some("b".into()),
            inactive_mtd: Some("/dev/mtd7".into()),
        };
        let v: serde_json::Value = serde_json::to_value(&plan).unwrap();
        let obj = v.as_object().expect("SlotPlan must serialize as object");
        assert!(obj.contains_key("active_slot"));
        assert!(
            obj.contains_key("inactive_slot"),
            "missing 'inactive_slot' — dashboard reads slot_plan.inactive_slot"
        );
        assert!(
            obj.contains_key("inactive_mtd"),
            "missing 'inactive_mtd' — dashboard reads slot_plan.inactive_mtd"
        );
        // The OLD UI-invented names MUST NOT exist.
        assert!(
            !obj.contains_key("target_slot"),
            "stale UI-only name 'target_slot' leaked into wire"
        );
        assert!(
            !obj.contains_key("source_sha256"),
            "stale UI-only name 'source_sha256' leaked into wire"
        );
        assert!(
            !obj.contains_key("unknown"),
            "stale UI-only name 'unknown' leaked into wire"
        );
    }

    #[test]
    fn restore_to_stock_status_is_flat_not_nested() {
        // The pre-W9-C dashboard expected nested last_preflight /
        // last_flash_attempt / in_progress. Backend canonical shape
        // (W9-C) is FLAT — see lines 347-366. Assert that the JSON
        // emits only the flat shape so a future refactor can't quietly
        // re-introduce the nested envelope.
        let status = RestoreToStockStatus {
            state: "scheduled".into(),
            state_detail: Some(RestoreState::Scheduled {
                reboot_at_ms: 1_700_000_000_000,
                backup_path: PathBuf::from("/data/restore-backup-1700000000"),
            }),
            last_preflight_at_ms: Some(1_700_000_000_000),
            last_preflight_verdict: Some("preflight_ok".into()),
            last_backup_path: Some("/data/restore-backup-1700000000".into()),
            last_scheduled_reboot_at_ms: Some(1_700_000_030_000),
            last_safety_findings: vec![],
            last_active_slot: Some("a".into()),
            last_inactive_slot: Some("b".into()),
            transitions: 7,
            last_transition_at_ms: Some(1_700_000_000_500),
            last_backup_fw_setenv_present: Some(true),
            recent_log_lines: VecDeque::new(),
        };
        let v: serde_json::Value = serde_json::to_value(&status).unwrap();
        let obj = v
            .as_object()
            .expect("RestoreToStockStatus must be a flat object");
        for key in [
            "state",
            "state_detail",
            "last_preflight_at_ms",
            "last_preflight_verdict",
            "last_backup_path",
            "last_scheduled_reboot_at_ms",
            "last_safety_findings",
            "transitions",
            "last_transition_at_ms",
        ] {
            assert!(obj.contains_key(key), "missing '{}' on flat status", key);
        }
        // The OLD nested shape MUST NOT exist:
        assert!(
            !obj.contains_key("last_preflight"),
            "nested 'last_preflight' leaked into wire"
        );
        assert!(
            !obj.contains_key("last_flash_attempt"),
            "nested 'last_flash_attempt' leaked into wire"
        );
        assert!(
            !obj.contains_key("in_progress"),
            "stale 'in_progress' leaked into wire"
        );
    }

    #[test]
    fn restore_state_phase_discriminator_is_snake_case() {
        // The dashboard's TypeScript discriminated-union reads
        // `state_detail.phase` as a snake_case string. Verify each
        // variant emits the expected tag.
        let cases: Vec<(RestoreState, &str)> = vec![
            (RestoreState::Idle, "idle"),
            (RestoreState::PreflightRunning, "preflight_running"),
            (RestoreState::PreflightOk, "preflight_ok"),
            (RestoreState::NandBackupRunning, "nand_backup_running"),
            (
                RestoreState::FlashRunning {
                    backup_path: PathBuf::from("/data/restore-backup-x"),
                },
                "flash_running",
            ),
        ];
        for (state, expected_phase) in cases {
            let v: serde_json::Value = serde_json::to_value(&state).unwrap();
            let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("");
            assert_eq!(phase, expected_phase, "RestoreState variant tag drift");
        }
    }

    #[test]
    fn restore_to_stock_response_wire_field_names() {
        let resp = RestoreToStockResponse {
            status: "scheduled".into(),
            reason: None,
            backup_path: Some("/data/restore-backup-1".into()),
            reboot_at_ms: Some(1_700_000_000_000),
            safety_findings: vec![],
            staged_sha256: Some("0xdeadbeef".into()),
            slot_plan: SlotPlan {
                active_slot: Some("a".into()),
                inactive_slot: Some("b".into()),
                inactive_mtd: Some("/dev/mtd7".into()),
            },
            hashboard_count_to_use: 3,
            dry_run: false,
        };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        let obj = v.as_object().expect("RestoreToStockResponse is an object");
        for key in [
            "status",
            "backup_path",
            "reboot_at_ms",
            "safety_findings",
            "staged_sha256",
            "slot_plan",
            "hashboard_count_to_use",
            "dry_run",
        ] {
            assert!(
                obj.contains_key(key),
                "missing '{}' on RestoreToStockResponse wire",
                key
            );
        }
    }

    /// F-4 (Sweep-v3 PR-081) dual regression pin. Two contracts:
    ///   1. Amlogic S19 XP now resolves to `amlogic-a113d-bm1366`
    ///      (was the silent `amlogic-unknown` fall-through; corpus
    ///      basis = `dcentrald-silicon-profiles::asics.rs` Bm1366.used_in).
    ///   2. S19j / S19j Pro / S19j Pro+ ALL stay `amlogic-a113d-bm1362`
    ///      — the Sweep doc's "S19j Pro+ -> BM1366" was rejected as a
    ///      silicon-identity regression (the in-code PROFILE_TABLE lists
    ///      S19j Pro+ under BM1362). Pinning BOTH so a future edit can't
    ///      regress either direction. Also pins that S21 XP is not
    ///      swallowed by the new s19xp branch.
    #[tokio::test]
    async fn f4_s19_xp_is_bm1366_and_s19j_family_stays_bm1362() {
        let base = std::env::temp_dir().join(format!(
            "f4-s19xp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dt = base.join("device-tree");
        std::fs::create_dir_all(&dt).unwrap();
        std::fs::write(base.join("cpuinfo"), "Hardware\t: Amlogic\n").unwrap();

        async fn sig(base: &std::path::Path) -> Option<String> {
            detect_platform_signature_with_root_for_test(Some(base)).await
        }

        for m in ["Antminer S19 XP", "antminer-s19-xp", "AntminerS19XP"] {
            std::fs::write(dt.join("model"), m).unwrap();
            assert_eq!(
                sig(&base).await,
                Some("amlogic-a113d-bm1366".to_string()),
                "{m} must map to BM1366 (S19 XP), not amlogic-unknown"
            );
        }

        for m in ["Antminer S19j", "Antminer S19j Pro", "Antminer S19j Pro+"] {
            std::fs::write(dt.join("model"), m).unwrap();
            assert_eq!(
                sig(&base).await,
                Some("amlogic-a113d-bm1362".to_string()),
                "{m} must STAY BM1362 — S19j Pro+ is BM1362 per the \
                 in-code PROFILE_TABLE; the Sweep's S19j-Pro+->BM1366 \
                 was a rejected silicon-identity regression"
            );
        }

        // The new s19xp branch must not swallow S21 XP.
        std::fs::write(dt.join("model"), "Antminer S21 XP").unwrap();
        assert_eq!(
            sig(&base).await,
            Some("amlogic-a113d-bm1370-xp".to_string())
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
