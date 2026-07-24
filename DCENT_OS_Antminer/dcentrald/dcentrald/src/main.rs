//! dcentrald - DCENTos Mining Daemon
//!
//! Entry point for the dcentrald mining daemon. Handles:
//! - Tokio async runtime initialization
//! - Signal handling (SIGTERM, SIGINT) for graceful shutdown
//! - Configuration loading from /data/dcentrald.toml
//! - Daemon lifecycle orchestration (init -> run -> shutdown)
//!
//! dcentrald is a clean rewrite, NOT a fork of any existing mining firmware.
//! 100% original D-Central codebase.

#![allow(
    dead_code,
    unused_assignments,
    unused_imports,
    unused_mut,
    unused_variables,
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items,
    clippy::enum_variant_names,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

mod am1_t15;
mod am2_chain_plan;
mod am3_bb_mining;
mod asic_identity_publication;
mod autotune;
mod bridge_glue;
mod bringup;
mod chain;
mod config;
mod daemon;
mod daemon_lifecycle;
mod error;
mod experimental;
pub mod fpga;
pub mod history;
mod logging;
mod metrics_export;
mod model;
mod persistent_log_ring;
mod restart;
mod runtime;
mod s19j_hybrid_admission;
mod s19j_hybrid_mining;
mod s19j_tap_mining;
mod serial_mining;
#[cfg(feature = "sim-hal")]
mod sim_runtime;
mod solar;
mod stock_mining;
mod stratum_proxy;
mod voltage_mailbox;
mod wave55a_recipe_guard;
mod work_dispatcher;
mod work_ledger;

use anyhow::{Context, Result};
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config::DcentraldConfig;
use crate::daemon::Daemon;
use crate::logging::init_logging;
use crate::s19j_hybrid_mining::S19jHybridMiner;
use crate::s19j_tap_mining::S19jTapMiner;
use crate::serial_mining::SerialMiner;
use crate::stock_mining::StockMiner;

/// Default configuration file path (persistent storage).
const DEFAULT_CONFIG_PATH: &str = "/data/dcentrald.toml";

/// Fallback configuration file path (read-only rootfs).
const FALLBACK_CONFIG_PATH: &str = "/etc/dcentrald.toml";

const VERIFY_BUNDLE_CAPABILITY_PATH: &str = "/data/dcentos/caps/verify-bundle";
const VERIFY_BUNDLE_CAPABILITY_BYTES: &[u8] = b"1\n";
/// The sentinel is currently two bytes. Keep a small bounded envelope for a
/// future version token without permitting accidental diagnostic payloads.
const VERIFY_BUNDLE_CAPABILITY_MAX_BYTES: usize = 16;

#[derive(Debug)]
enum VerifyBundleCapabilityPublishError {
    CreateParent(std::io::Error),
    Publish(dcentrald_common::atomic_file::AtomicWriteError),
}

fn publish_verify_bundle_capability(
    path: &std::path::Path,
) -> std::result::Result<
    dcentrald_common::atomic_file::AtomicWriteOutcome,
    VerifyBundleCapabilityPublishError,
> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent).map_err(VerifyBundleCapabilityPublishError::CreateParent)?;
    dcentrald_common::atomic_file::atomic_write(
        path,
        VERIFY_BUNDLE_CAPABILITY_BYTES,
        dcentrald_common::atomic_file::AtomicWriteOptions::state_file(
            VERIFY_BUNDLE_CAPABILITY_MAX_BYTES,
        ),
    )
    .map_err(VerifyBundleCapabilityPublishError::Publish)
}

/// Auto-detect whether we should force `--s19j-hybrid` by reading
/// `/etc/bos_platform` (BraiinsOS's platform marker) or the DCENT_OS mirror
/// at `/etc/dcentos/platform`. The am2 S19j Pro image enters the
/// s19j-hybrid code path by default — if we let it fall through to the default
/// S9/BraiinsOS `Daemon::run()` path, `unbind_kernel_i2c_driver()` fires
/// and runs S9 AXI IIC devmem logic on the am2 PSU/PIC I2C bus. That is
/// precisely what  bans and is a
/// strong candidate for poisoning PSU state (see CE audit 06-ce.md #2).
///
/// Returns `(auto_enabled, detected_platform)`. Empty platform means the
/// marker file was missing — we leave CLI behavior unchanged in that case.
fn classify_s19j_hybrid_auto(platform: &str, board_target: &str) -> (bool, String) {
    let platform = platform.trim().to_string();
    let board_target = board_target.trim().to_string();
    let is_s19j_am2 =
        matches!(platform.as_str(), "zynq-bm3-am2") && is_s19j_am2_board_target(&board_target);
    let marker = if board_target.is_empty() {
        platform
    } else {
        format!("{}:{}", platform, board_target)
    };
    (is_s19j_am2, marker)
}

fn is_s19j_am2_board_target(board_target: &str) -> bool {
    let board_target = board_target.trim();
    board_target == "am2-s19j" || board_target.starts_with("am2-s19jpro")
}

/// R1 (2026-05-17): compute the am2 low-idle fan command tuple for the
/// management-only park paths.
///
/// Returns `Some((fan_idle_pwm, fan_max_pwm))` ONLY when the detected
/// platform is am2 (`zynq-bm3-am2*`). On S9/am1 + am3 returns `None` so the
/// park paths are byte-identical no-ops there (am1 fan idle is correctly
/// handled by the init-script `devmem` path; the am2 fan IP is UIO-bound and
/// devmem is a proven no-op on it — see
/// ). The gate
/// matches the codebase's existing am2 detection convention
/// (`detected_platform.starts_with("zynq-bm3-am2")`, cf. the
/// `serial_mining_mode` arm). The actual DOWN-clamp (≤ fan_max_pwm ≤
/// PWM_SAFETY_MAX) lives in `force_am2_fans_to_quiet_idle`. Live `a lab unit`
/// feedback proved this command is not acoustic proof; callers must use RPM
/// and operator confirmation for quiet claims.
fn am2_quiet_idle_tuple(detected_platform: &str, config: &DcentraldConfig) -> Option<(u8, u8)> {
    if detected_platform.starts_with("zynq-bm3-am2") {
        Some((config.thermal.fan_idle_pwm, config.thermal.fan_max_pwm))
    } else {
        None
    }
}

/// R2: pure PWM clamp for the `--set-fan` one-shot.
///
/// PWM is clamped to `PWM_SAFETY_MAX` (30) UNLESS an explicit `--allow-loud`
/// is also present (the  "explicit user
/// override to a higher mode" carve-out — the ONLY sanctioned way to exceed
/// the home cap). Even with `--allow-loud` the value is still clamped to the
/// IP ceiling `PWM_MAX` (100) — the BraiinsOS fan_ctrl IP panics on > 100.
/// Without `--allow-loud` the value is `min(PWM_SAFETY_MAX)` — only ever
/// driven DOWN relative to the 30 cap; never above it.
fn clamp_set_fan_pwm(requested: u8, allow_loud: bool) -> u8 {
    if allow_loud {
        requested.min(dcentrald_hal::fan::PWM_MAX)
    } else {
        requested
            .min(dcentrald_hal::fan::PWM_SAFETY_MAX)
            .min(dcentrald_hal::fan::PWM_MAX)
    }
}

/// Pre-everything CLI info request (`--help`/`-h`, `--version`/`-V`).
#[derive(Debug, PartialEq, Eq)]
enum CliInfoRequest {
    Help,
    Version,
}

/// Pure classifier for the CLI info one-shots.
///
/// **Defect this closes (production-readiness + safety + operability):**
/// `dcentrald` had NO help/version flag. The single most universal CLI
/// invocation — `dcentrald --help` — matched none of the one-shot or
/// mode flags and fell through into the auto-`s19j-hybrid` routing,
/// **silently starting the mining daemon**. On a *configured* production
/// unit that begins real mining from a "show me usage" command (it was
/// only fan-safe on `a lab unit` because no pool was configured →
/// management-only-idle). Every competitor firmware CLI (`bosminer
/// --help`, LuxOS) prints usage and exits. `--help` takes precedence
/// over `--version`, and BOTH take precedence over every daemon/one-shot
/// flag. Pure + deterministic so that precedence is unit-pinned
/// independently of the (HAL-bound, un-host-runnable) `main` body.
fn wants_cli_info(args: &[String]) -> Option<CliInfoRequest> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Some(CliInfoRequest::Help);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Some(CliInfoRequest::Version);
    }
    None
}

/// Every `--flag`/`-x` the `dcentrald` binary itself recognizes.
/// Value-bearing flags (the next argv is their value, NOT a flag):
/// `--set-fan <pwm>`, `--fan-sweep <list>`, `--dwell-ms <ms>`,
/// `--config <path>`. Kept in lockstep with the `== "--…"` matches in
/// `run_main` (a structural test pins that they don't drift).
const KNOWN_CLI_BOOL_FLAGS: &[&str] = &[
    "--help",
    "-h",
    "--version",
    "-V",
    "--get-fan",
    "--allow-loud",
    "--s19j-hybrid",
    "--serial-mining",
    "--tap-mode",
    "--stratum-proxy",
    "--am3-bb-mining",
    "--stock-fpga",
    "--safe-off",
];
const KNOWN_CLI_VALUE_FLAGS: &[&str] = &[
    "--set-fan",
    "--hold-fan",
    "--fan-sweep",
    "--dwell-ms",
    "--config",
    "--verify-bundle",
];

/// Pure detector for **unrecognized** `-`/`--` flags.
///
/// **Defect this surfaces (same class as the missing `--help` handler):**
/// `dcentrald` matched flags with `args.iter().any(|a| a == "--x")` and
/// did NO unknown-flag validation — so a typo like `--gte-fan` (for
/// `--get-fan`) or any unrecognized flag fell through every check into
/// auto-`s19j-hybrid` and **silently started the mining daemon**. That
/// is exactly the failure mode that caused the 2026-05-19 `a lab unit`
/// over-spawn (a flag the binary did not recognize → silent daemonize)
/// and is dangerous on a fragile home unit.
///
/// **Wave C (2026-05-19): strict-reject ENABLED.** Returned non-empty
/// ⇒ the caller emits a loud error, dumps `cli_help_text()`, and exits
/// with code 2. G-T8-1 was closed in Wave B (commit `5aaae39f` removed
/// the only known offender, `--passthrough`, from the 3 production
/// deploy scripts; the canonical 16-flag set at
/// `KNOWN_CLI_BOOL_FLAGS` + `KNOWN_CLI_VALUE_FLAGS` is now exhaustive
/// across every production launcher site — see
/// ).
/// The previous deferred-TODO non-blocking warning is gone: silent
/// daemonization on a typo'd flag was the `a lab unit` over-spawn pattern,
/// and this strict-reject is what closes that defect class.
/// `argv[0]` and the value token after a value-bearing flag are skipped.
fn unrecognized_cli_flags(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if KNOWN_CLI_VALUE_FLAGS.iter().any(|f| f == arg) {
            iter.next(); // consume this flag's value token
            continue;
        }
        if KNOWN_CLI_BOOL_FLAGS.iter().any(|f| f == arg) {
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            out.push(arg.clone());
        }
        // Non-`-` tokens are stray positionals; conservatively ignored
        // (could be a value the existing positional logic still picks
        // up) — only `-`/`--` look-alikes are the dangerous typo case.
    }
    out
}

/// Concise usage for the `--help` one-shot. Lists the safety-relevant
/// read-only fan one-shots first (an operator reaching for `--help` on a
/// fragile home unit must see these), then the bring-up modes, and is
/// explicit that bare `dcentrald` auto-detects the platform and starts
/// the mining daemon.
fn cli_help_text() -> String {
    format!(
        "dcentrald {ver} — DCENT_OS · D-Central Technologies, the Mining Hackers · https://d-central.tech · GPL-3.0\n\
         \n\
         USAGE:\n\
         \x20 dcentrald [FLAGS]      (no flag = auto-detect platform, start the mining daemon)\n\
         \n\
         READ-ONLY / SAFETY ONE-SHOTS (no daemon, no mining, exit immediately):\n\
         \x20 --help, -h            print this help and exit\n\
         \x20 --version, -V         print the version and exit\n\
         \x20 --get-fan             read-only fan PWM/RPM snapshot (key=value), exit\n\
         \x20 --set-fan <0-100>     command fan PWM once (clamped to {cap} unless --allow-loud), exit\n\
         \x20 --hold-fan <0-100>    PERSISTENT fan custodian: park PWM + re-assert every ~5s until\n\
         \x20                       SIGTERM (clamped to {cap} unless --allow-loud). Required on AM2:\n\
         \x20                       the board reverts fans to full speed if nothing keeps commanding them.\n\
         \x20 --fan-sweep <list>    diagnostic fan-PWM sweep (e.g. 0,5,10,15,20,25,30); [--dwell-ms N]\n\
         \x20 --allow-loud          permit --set-fan/--fan-sweep above the {cap} home cap (explicit override)\n\
         \n\
         BRING-UP MODES (start a daemon):\n\
         \x20 (default)             auto-detect; am2/XIL auto-routes to --s19j-hybrid\n\
         \x20 --s19j-hybrid         am2 Zynq S19/S19j Pro hybrid bring-up\n\
         \x20 --serial-mining       Amlogic / serial BM1362 path\n\
         \x20 --am3-bb-mining       AM335x BeagleBone S19j Pro path\n\
         \x20 --tap-mode            FPGA work-dispatch only (bosminer owns HW)\n\
         \x20 --stratum-proxy       Stratum V1 byte-relay only (zero HW access)\n\
         \x20 --stock-fpga          stock-Bitmain FPGA bitstream mode\n\
         \n\
         Docs + recovery: https://d-central.tech  |  config: /data/dcentrald.toml\n",
        ver = env!("CARGO_PKG_VERSION"),
        cap = dcentrald_hal::fan::PWM_SAFETY_MAX,
    )
}

/// R2: discover the fan-control UIO device by NAME.
///
/// Scans `/sys/class/uio/uio*/name`. Returns `(uio_number, variant)`:
///   - the uio whose name is `fan-control` is the fan IP;
///   - if a `board-control` UIO also exists ⇒ this is an am2 unit ⇒
///     `FanVariant::Am2Uio16` (the 4-fan am2 layout);
///   - otherwise it's an am1-s9 unit ⇒ `FanVariant::Am1S9`.
///
/// Name-based (not number-based) discovery so it is robust across kernel UIO
/// enumeration order. Returns `None` if no `fan-control` UIO is present
/// (defined failure → exit 1 for the init script). Synchronous filesystem
/// reads only — no device is opened here.
fn discover_fan_uio() -> Option<(u8, dcentrald_hal::fan::FanVariant)> {
    dcentrald_hal::fan::discover_fan_uio()
        .map(|discovery| (discovery.uio_number, discovery.variant))
}

fn format_fan_pairs(pairs: &[(u8, u32)]) -> String {
    pairs
        .iter()
        .map(|(fan_id, value)| format!("{}:{}", fan_id, value))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_fan_raw_registers(regs: &[dcentrald_hal::fan::FanRawRegister]) -> String {
    regs.iter()
        .map(|reg| format!("0x{:02X}:0x{:08X}", reg.offset, reg.value))
        .collect::<Vec<_>>()
        .join(",")
}

fn am2_low_pwm_floor_present(
    variant: dcentrald_hal::fan::FanVariant,
    pwm: u8,
    max_rpm: u32,
) -> bool {
    matches!(variant, dcentrald_hal::fan::FanVariant::Am2Uio16)
        && pwm <= dcentrald_hal::fan::PWM_QUIET_BOOT
        && max_rpm >= 2_000
}

fn print_fan_snapshot(
    label: &str,
    discovery: dcentrald_hal::fan::FanUioDiscovery,
    fan: &dcentrald_hal::fan::FanController,
    requested_pwm: Option<u8>,
    include_raw_regs: bool,
) -> (u8, u32) {
    let (commanded_pwm0, commanded_pwm1) = fan.get_speed_pwm_channels();
    let (raw_pwm0, raw_pwm1) = fan.get_speed_pwm_raw_channels();
    let commanded_pwm = commanded_pwm0.max(commanded_pwm1);
    let raw_rps = fan.get_raw_rps_channels();
    let per_fan_rpm = fan.get_per_fan_rpm();
    let max_rpm = per_fan_rpm.iter().map(|(_, rpm)| *rpm).max().unwrap_or(0);
    let raw_rps_text = format_fan_pairs(&raw_rps);
    let per_fan_text = format_fan_pairs(&per_fan_rpm);
    let front_fan_surface_attached = fan.has_front_fan_surface();
    let am2_c52_fan_mode = fan
        .am2_c52_fan_mode_status()
        .map(|s| format!("uio{}:0x{:08X}->0x{:08X}", s.uio_number, s.before, s.after))
        .unwrap_or_else(|| "none".to_string());
    let requested_pwm = requested_pwm
        .map(|p| p.to_string())
        .unwrap_or_else(|| "none".to_string());

    if include_raw_regs {
        let raw_regs = fan.raw_register_dump();
        let raw_regs_text = format_fan_raw_registers(&raw_regs);
        println!(
            "dcentrald {label}: uio={} variant={:?} requested_pwm={} commanded_pwm={} commanded_pwm0={} commanded_pwm1={} am2_c52_fan_mode={} front_fan_surface_attached={} raw_pwm0=0x{:08X} raw_pwm1=0x{:08X} max_rpm={} raw_rps=[{}] per_fan_rpm=[{}] raw_regs=[{}]",
            discovery.uio_number,
            discovery.variant,
            requested_pwm,
            commanded_pwm,
            commanded_pwm0,
            commanded_pwm1,
            am2_c52_fan_mode,
            front_fan_surface_attached,
            raw_pwm0,
            raw_pwm1,
            max_rpm,
            raw_rps_text,
            per_fan_text,
            raw_regs_text
        );
    } else {
        println!(
            "dcentrald {label}: uio={} variant={:?} requested_pwm={} commanded_pwm={} commanded_pwm0={} commanded_pwm1={} am2_c52_fan_mode={} front_fan_surface_attached={} raw_pwm0=0x{:08X} raw_pwm1=0x{:08X} max_rpm={} raw_rps=[{}] per_fan_rpm=[{}]",
            discovery.uio_number,
            discovery.variant,
            requested_pwm,
            commanded_pwm,
            commanded_pwm0,
            commanded_pwm1,
            am2_c52_fan_mode,
            front_fan_surface_attached,
            raw_pwm0,
            raw_pwm1,
            max_rpm,
            raw_rps_text,
            per_fan_text
        );
    }

    (commanded_pwm, max_rpm)
}

/// R2: the `--set-fan <PWM>` one-shot body. Returns the process exit code.
///
/// SAFETY: this is intentionally a synchronous open→write→readback→exit on
/// the fan register ONLY. It performs NO PIC/PSU/ASIC/I2C/voltage access and
/// builds NO tokio HW runtime. It cannot raise PWM above `PWM_SAFETY_MAX`
/// (30) unless the caller passes explicit `--allow-loud` (and even then
/// never above the IP ceiling 100). This is the am2-correct replacement for
/// the init script's `devmem` fan write (a proven no-op on the UIO-bound am2
/// fan IP). am1-s9 keeps its devmem path in the init scripts; this one-shot
/// works on both variants when invoked.
fn run_set_fan_oneshot(pwm_arg: Option<&str>, allow_loud: bool) -> i32 {
    let Some(pwm_str) = pwm_arg else {
        eprintln!(
            "dcentrald --set-fan: missing PWM argument (usage: --set-fan <0-100> [--allow-loud])"
        );
        return 2;
    };
    let Ok(requested) = pwm_str.parse::<u8>() else {
        eprintln!(
            "dcentrald --set-fan: invalid PWM '{}' — must be a non-negative integer 0-255",
            pwm_str
        );
        return 2;
    };
    let pwm = clamp_set_fan_pwm(requested, allow_loud);
    if pwm != requested {
        eprintln!(
            "dcentrald --set-fan: clamped requested PWM {} -> {} (allow_loud={}; PWM_SAFETY_MAX={}, PWM_MAX={})",
            requested,
            pwm,
            allow_loud,
            dcentrald_hal::fan::PWM_SAFETY_MAX,
            dcentrald_hal::fan::PWM_MAX
        );
    }

    let Some((uio, variant)) = discover_fan_uio() else {
        eprintln!(
            "dcentrald --set-fan: no 'fan-control' UIO found under /sys/class/uio — cannot set fan PWM"
        );
        return 1;
    };

    match dcentrald_hal::fan::FanController::open_with_variant(uio, variant) {
        Ok(fan) => {
            fan.set_speed(pwm);
            let discovery = dcentrald_hal::fan::FanUioDiscovery {
                uio_number: uio,
                variant,
                has_board_control: matches!(variant, dcentrald_hal::fan::FanVariant::Am2Uio16),
            };
            let (commanded_pwm, max_rpm) =
                print_fan_snapshot("--set-fan", discovery, &fan, Some(pwm), false);
            if am2_low_pwm_floor_present(variant, pwm, max_rpm) {
                eprintln!(
                    "dcentrald --set-fan: AM2 low-PWM floor detected (requested={}, commanded_pwm={}, max_rpm={}); register write succeeded but the physical fan floor/failsafe is still loud",
                    pwm, commanded_pwm, max_rpm
                );
            }
            // AM2 (XIL Zynq) fan PWM is held only while a process owns the
            // uio16 mmap AND keeps re-commanding it; once this one-shot exits,
            // the board's fan IP drifts the fans back to its full-speed default.
            // For a persistent AM2 hold use `--hold-fan <PWM>` (stays resident,
            // re-asserts every ~5 s until SIGTERM). See
            // .
            if matches!(variant, dcentrald_hal::fan::FanVariant::Am2Uio16) {
                eprintln!(
                    "dcentrald --set-fan: NOTE — on AM2 this write does NOT persist after exit (the board reverts fans to full speed once no process keeps commanding the uio16 mmap). Use `--hold-fan {}` for a persistent custodian.",
                    pwm
                );
            }
            0
        }
        Err(e) => {
            eprintln!(
                "dcentrald --set-fan: failed to open fan-control uio{} ({:?}): {}",
                uio, variant, e
            );
            1
        }
    }
}

/// `--hold-fan <PWM>` PERSISTENT fan custodian. Returns the process exit code
/// (only on SIGTERM/SIGINT — this never returns on its own).
///
/// WHY THIS EXISTS (home-safety, AM2):
/// On AM2 (XIL Zynq) the fan PWM is held only while a process owns the uio16
/// mmap AND keeps re-commanding it. `--set-fan` writes the register once and
/// EXITS — so the board's fan IP drifts the fans back to its full-speed default,
/// blasting a home unit (observed: `--set-fan 10` then minutes later PWM 100 /
/// 6180 RPM). mmap-hold alone is NOT enough either: a daemon that wrote PWM once
/// and only held the mmap still drifted; the ~5 s re-assert is the cure. This
/// custodian parks the fan, then STAYS RESIDENT re-asserting the PWM (and, on
/// AM2, re-enabling C52 fan mode via the FanController open path) every ~5 s
/// until SIGTERM/SIGINT.
/// and `park_management_only_until_shutdown` (the proven in-process park path
/// this reuses).
///
/// SAFETY: PWM is only ever driven DOWN — clamped to `PWM_SAFETY_MAX` (30)
/// unless explicit `--allow-loud` (identical clamp to `--set-fan`), and even
/// then never above the IP ceiling `PWM_MAX` (100). No PIC/PSU/ASIC/I2C/voltage
/// access; fan-control UIO only. The 5 s re-assert (`force_am2_fans_to_quiet_idle`)
/// re-clamps every tick, so the hold can never raise PWM. On non-AM2 variants
/// the board's fan registers/sysfs hold on their own, so the re-assert is
/// harmless redundancy — kept uniform for simplicity (a single park + 5 s
/// re-assert + SIGTERM-block path for all variants).
///
/// SIGTERM-RESPONSIVE: a `tokio::select!` races the 5 s interval tick against
/// SIGTERM/SIGINT, so a signal is acted on immediately (the init-script `stop`
/// path kills this custodian by its pidfile). Logs the initial park at `info`,
/// the periodic refreshes at `debug`. Exit codes: 0 = parked + held then clean
/// signal exit; 1 = fan-control UIO discovery/open failed; 2 = bad/missing PWM.
async fn run_hold_fan(pwm_arg: Option<&str>, allow_loud: bool) -> i32 {
    let Some(pwm_str) = pwm_arg else {
        eprintln!(
            "dcentrald --hold-fan: missing PWM argument (usage: --hold-fan <0-100> [--allow-loud])"
        );
        return 2;
    };
    let Ok(requested) = pwm_str.parse::<u8>() else {
        eprintln!(
            "dcentrald --hold-fan: invalid PWM '{}' — must be a non-negative integer 0-255",
            pwm_str
        );
        return 2;
    };
    let pwm = clamp_set_fan_pwm(requested, allow_loud);
    if pwm != requested {
        eprintln!(
            "dcentrald --hold-fan: clamped requested PWM {} -> {} (allow_loud={}; PWM_SAFETY_MAX={}, PWM_MAX={})",
            requested,
            pwm,
            allow_loud,
            dcentrald_hal::fan::PWM_SAFETY_MAX,
            dcentrald_hal::fan::PWM_MAX
        );
    }

    let Some((uio, variant)) = discover_fan_uio() else {
        eprintln!(
            "dcentrald --hold-fan: no 'fan-control' UIO found under /sys/class/uio — cannot hold fan PWM"
        );
        return 1;
    };

    let is_am2 = matches!(variant, dcentrald_hal::fan::FanVariant::Am2Uio16);

    // Initial park (open → write → readback). On a clean open failure we exit 1
    // (the caller — init script — treats that as a real failure). The C52 fan
    // mode is re-enabled by `FanController::open_with_variant()` on every open,
    // and `force_am2_fans_to_quiet_idle` re-clamps the PWM (only ever DOWN).
    match dcentrald_hal::fan::FanController::open_with_variant(uio, variant) {
        Ok(fan) => {
            fan.set_speed(pwm);
            let discovery = dcentrald_hal::fan::FanUioDiscovery {
                uio_number: uio,
                variant,
                has_board_control: is_am2,
            };
            print_fan_snapshot(
                "--hold-fan (initial park)",
                discovery,
                &fan,
                Some(pwm),
                false,
            );
            // Drop `fan` here: the periodic re-assert reopens the controller via
            // `force_am2_fans_to_quiet_idle`'s own open path each tick (which is
            // what re-enables C52 + re-clamps), matching the proven
            // `park_management_only_until_shutdown` refresh exactly.
        }
        Err(e) => {
            eprintln!(
                "dcentrald --hold-fan: failed to open fan-control uio{} ({:?}): {}",
                uio, variant, e
            );
            return 1;
        }
    }

    info!(
        pwm,
        uio,
        variant = ?variant,
        is_am2,
        allow_loud,
        "dcentrald --hold-fan: persistent fan custodian started — parked PWM \
         and will re-assert it every ~5 s until SIGTERM (prevents the AM2 board \
         fan-controller revert to full speed)"
    );

    // Persistent custodian: re-assert the PWM every ~5 s until SIGTERM/SIGINT.
    // PWM is only ever driven DOWN (re-clamped inside `force_am2_fans_to_quiet_idle`
    // on AM2 / the FanController setter on non-AM2). The 5 s cadence + SIGTERM
    // race exactly mirror `park_management_only_until_shutdown`.
    const FAN_REFRESH: std::time::Duration = std::time::Duration::from_secs(5);
    let mut refresh = tokio::time::interval(FAN_REFRESH);
    // First tick fires immediately; skip-then-wait so we don't double the entry
    // park we just performed.
    refresh.tick().await;

    let mut sigterm = match signal::unix::signal(signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "dcentrald --hold-fan: failed to register SIGTERM handler: {} — holding via SIGINT only",
                e
            );
            // Degrade to SIGINT-only rather than dropping the custodian.
            loop {
                tokio::select! {
                    _ = signal::ctrl_c() => break,
                    _ = refresh.tick() => hold_fan_reassert(pwm, is_am2),
                }
            }
            info!("dcentrald --hold-fan: received SIGINT, releasing fan custodian (clean exit)");
            return 0;
        }
    };

    loop {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("dcentrald --hold-fan: received SIGINT, releasing fan custodian (clean exit)");
                break;
            }
            _ = sigterm.recv() => {
                info!("dcentrald --hold-fan: received SIGTERM, releasing fan custodian (clean exit)");
                break;
            }
            _ = refresh.tick() => hold_fan_reassert(pwm, is_am2),
        }
    }
    0
}

/// One periodic re-assert tick for `--hold-fan`. On AM2 this reuses the proven
/// `force_am2_fans_to_quiet_idle` path (reopens the FanController → re-enables
/// C52 fan mode → re-clamps + re-commands the PWM, only ever DOWN). On non-AM2
/// the board fan registers/sysfs hold on their own; we still re-command for
/// uniformity (a single FanController open + set, tolerant of open failure so
/// the custodian never aborts on a transient device error). Logs at `debug`.
fn hold_fan_reassert(pwm: u8, is_am2: bool) {
    if is_am2 {
        tracing::debug!(
            pwm,
            "dcentrald --hold-fan: re-asserting AM2 fan PWM (C52 + idle re-clamp)"
        );
        // Pass `pwm` as both the idle and the cap so `compute_quiet_idle_pwm`
        // resolves to exactly `pwm` (already clamped ≤ 30 unless --allow-loud,
        // which raised the value before this point but never above PWM_MAX).
        crate::s19j_hybrid_mining::force_am2_fans_to_quiet_idle(
            pwm,
            pwm,
            "--hold-fan periodic custodian re-assert (am2)",
        );
        return;
    }
    // Non-AM2: harmless redundant re-command (registers/sysfs already hold).
    match discover_fan_uio() {
        Some((uio, variant)) => {
            match dcentrald_hal::fan::FanController::open_with_variant(uio, variant) {
                Ok(fan) => {
                    fan.set_speed(pwm);
                    tracing::debug!(pwm, uio, "dcentrald --hold-fan: re-asserted fan PWM (non-AM2)");
                }
                Err(e) => tracing::debug!(
                    pwm,
                    uio,
                    error = %e,
                    "dcentrald --hold-fan: re-assert open failed (non-AM2) — custodian continues"
                ),
            }
        }
        None => tracing::debug!(
            pwm,
            "dcentrald --hold-fan: re-assert found no fan-control UIO (non-AM2) — custodian continues"
        ),
    }
}

/// `--safe-off` quiet-idle fan PWM. Only ever driven DOWN and clamped to
/// `PWM_SAFETY_MAX` (30); the chips are de-energized first so this is a quiet
/// cooldown setpoint, not a thermal response.
const SAFE_OFF_FAN_PWM: u8 = 10;

/// Platform → power-cut action mapping for `--safe-off`. Pure + unit-tested so
/// the per-platform dispatch is verified without touching hardware (Wave-B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SafeOffCut {
    /// am2 Zynq (S19/S19j, incl. .25/.109/.139): drive PWR_CONTROL gpio907 low —
    /// the audited home-hard-stop cut (`s19j_hybrid_mining::force_pwr_control_low`).
    Am2PwrControl,
    /// Amlogic A113D (S21 / S19k / S19jpro-aml).
    AmlogicDisablePsu,
    /// AM335x BeagleBone (am3-bb).
    BeagleboneDisablePsu,
    /// CVitek CV1835.
    CvitekDisablePsu,
    /// am1 S9 + other Zynq.
    ZynqDisablePsu,
    /// Unknown platform string — power is NOT cut (honest; never a false affordance).
    Unknown,
}

/// Map `/etc/dcentos/platform` to the correct `--safe-off` power-cut action.
/// Pure (string → enum) so it is unit-testable. Order matters: the am2
/// fingerprint (`zynq-bm3-am2`) MUST be checked before the generic `zynq` arm.
fn safe_off_cut_for_platform(platform: &str) -> SafeOffCut {
    let p = platform.trim();
    if p.starts_with("zynq-bm3-am2") {
        SafeOffCut::Am2PwrControl
    } else if p.starts_with("amlogic") || p.contains("am3-aml") || p.contains("a113d") {
        SafeOffCut::AmlogicDisablePsu
    } else if p.starts_with("am3-bb") || p.contains("beaglebone") || p.contains("am335x") {
        SafeOffCut::BeagleboneDisablePsu
    } else if p.contains("cvitek") || p.contains("cv1835") {
        SafeOffCut::CvitekDisablePsu
    } else if p.starts_with("zynq") {
        SafeOffCut::ZynqDisablePsu
    } else {
        SafeOffCut::Unknown
    }
}

/// `--safe-off` one-shot (Wave-B no-brick): an OPERATOR-INVOKED emergency power
/// cut. Cuts ASIC power FIRST (platform-dispatched to the audited per-platform
/// cut), then commands fans to a safe quiet idle (cut-hash-before-noise). Like
/// `--set-fan` it runs BEFORE config/logging/mode routing and touches only the
/// power-cut + fan paths — no tokio HW runtime, no mining, no EEPROM. It is a
/// NEW opt-in subcommand: it cannot alter the normal daemon path. Exit codes:
///   0 = power cut + fans set quiet; 1 = a cut/fan step failed or platform unknown.
fn run_safe_off_oneshot() -> i32 {
    let platform = std::fs::read_to_string("/etc/dcentos/platform").unwrap_or_default();
    let cut = safe_off_cut_for_platform(&platform);
    let platform_disp = platform.trim();
    eprintln!(
        "dcentrald --safe-off: platform='{}' -> cut={:?} (cutting ASIC power first, then fans to quiet idle)",
        platform_disp, cut
    );
    let mut rc = 0;

    // 1. Cut ASIC power (the load-bearing safety action) — audited per-platform.
    match cut {
        SafeOffCut::Am2PwrControl => {
            // gpio907 is the am2 PWR_CONTROL on every am2 control board
            // (psu_bypass_gate / .25 / .109 / .139); driving it low de-energizes
            // the hashboard rail — the proven home-hard-stop cut.
            let active_low = std::env::var("DCENT_AM2_PWR_CONTROL_ACTIVE_LOW")
                .map(|v| {
                    matches!(
                        v.as_str(),
                        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
                    )
                })
                .unwrap_or(false);
            let active_high = std::env::var("DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH")
                .map(|v| {
                    matches!(
                        v.as_str(),
                        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
                    )
                })
                .unwrap_or(false);
            if active_low == active_high {
                eprintln!(
                    "dcentrald --safe-off: AM2 PWR_CONTROL polarity unknown or conflicting; set exactly one of DCENT_AM2_PWR_CONTROL_ACTIVE_LOW=1 or DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH=1"
                );
                rc = 1;
            } else if let Err(e) =
                s19j_hybrid_mining::force_pwr_control_low_checked(Some("907"), "cli-safe-off")
            {
                eprintln!("dcentrald --safe-off: AM2 PWR_CONTROL cut failed: {}", e);
                rc = 1;
            }
        }
        SafeOffCut::AmlogicDisablePsu => {
            if let Err(e) = dcentrald_hal::platform::amlogic::disable_psu() {
                eprintln!("dcentrald --safe-off: amlogic disable_psu failed: {}", e);
                rc = 1;
            }
        }
        SafeOffCut::BeagleboneDisablePsu => {
            if let Err(e) = dcentrald_hal::platform::beaglebone::disable_psu() {
                eprintln!("dcentrald --safe-off: beaglebone disable_psu failed: {}", e);
                rc = 1;
            }
        }
        SafeOffCut::CvitekDisablePsu => {
            if let Err(e) = dcentrald_hal::platform::cvitek::disable_psu() {
                eprintln!("dcentrald --safe-off: cvitek disable_psu failed: {}", e);
                rc = 1;
            }
        }
        SafeOffCut::ZynqDisablePsu => {
            if let Err(e) = dcentrald_hal::platform::zynq::disable_psu_output() {
                eprintln!(
                    "dcentrald --safe-off: zynq disable_psu_output failed: {}",
                    e
                );
                rc = 1;
            }
        }
        SafeOffCut::Unknown => {
            eprintln!(
                "dcentrald --safe-off: unknown platform '{}' — ASIC power was NOT cut. \
                 Use the unit's power switch or its init.d safety path.",
                platform_disp
            );
            rc = 1;
        }
    }

    // 2. Fans -> safe quiet idle (chips are de-energized now; PWM is only ever
    //    driven DOWN, never above PWM_SAFETY_MAX). Best-effort: a fan-open
    //    failure does not abort — the power cut above is load-bearing.
    let quiet_pwm = SAFE_OFF_FAN_PWM.min(dcentrald_hal::fan::PWM_SAFETY_MAX);
    match discover_fan_uio() {
        Some((uio, variant)) => {
            match dcentrald_hal::fan::FanController::open_with_variant(uio, variant) {
                Ok(fan) => {
                    fan.set_speed(quiet_pwm);
                    eprintln!(
                        "dcentrald --safe-off: fans commanded to quiet idle PWM {} (uio{}, {:?}); RPM is separate proof",
                        quiet_pwm, uio, variant
                    );
                }
                Err(e) => {
                    eprintln!(
                        "dcentrald --safe-off: fan-control uio{} open failed ({:?}): {} — power already cut",
                        uio, variant, e
                    );
                    rc = 1;
                }
            }
        }
        None => {
            eprintln!(
                "dcentrald --safe-off: no 'fan-control' UIO found — fans left as-is (power already cut)"
            );
        }
    }
    rc
}

/// Read-only fan one-shot for init/verify scripts.
///
/// Same discovery/open path as `--set-fan`, but performs no writes. The output
/// is key=value so BusyBox shell can parse it without JSON tools.
/// `dcentrald --verify-bundle <sysupgrade.tar|extracted-dir>` — on-device OTA
/// signature + manifest verification, reusing the daemon's own pinned-key
/// verifier (`dcentrald_api::ota_signature::verify_sysupgrade_bundle`).
///
/// wf_c00e5d9e A/B follow-up (2026-05-29): the install scripts (e.g.
/// `install_amlogic_persistent.sh`) verify SHA256SUMS + MANIFEST but have NO
/// on-device ed25519 *signature* re-verify, because the device tool set ships no
/// openssl/ed25519 verifier. This verb gives them (and operators) that in-band
/// authenticity check by exposing the daemon's existing Rust verifier as a CLI.
/// Requires a signature (`allow_unsigned = false`) and, when present, pins the
/// bundle's embedded key against `/etc/dcentos/release_ed25519.pub`. READ-ONLY:
/// it verifies and exits, never flashes. Exit code: 0 = verified, 1 = failed,
/// 2 = usage error.
fn run_verify_bundle_oneshot(path_arg: Option<&str>) -> i32 {
    let path = match path_arg {
        Some(p) if !p.is_empty() && !p.starts_with("--") => p,
        _ => {
            eprintln!("usage: dcentrald --verify-bundle <sysupgrade.tar|extracted-dir>");
            eprintln!(
                "  Verifies the bundle's ed25519 signature + MANIFEST against the compile-time-pinned"
            );
            eprintln!(
                "  OTA key (and /etc/dcentos/release_ed25519.pub if present). Read-only; never flashes."
            );
            return 2;
        }
    };
    let pinned = std::path::Path::new("/etc/dcentos/release_ed25519.pub");
    let pinned_opt = if pinned.is_file() { Some(pinned) } else { None };
    match dcentrald_api::ota_signature::verify_sysupgrade_bundle(
        std::path::Path::new(path),
        false, // an explicit verify MUST require a signature — never allow_unsigned
        pinned_opt,
    ) {
        Ok(_bundle) => {
            println!(
                "OK: sysupgrade bundle verified (ed25519 signature + MANIFEST): {}{}",
                path,
                if pinned_opt.is_some() {
                    " [pinned against /etc/dcentos/release_ed25519.pub]"
                } else {
                    ""
                }
            );
            0
        }
        Err(e) => {
            eprintln!("FAIL: sysupgrade bundle verification failed: {e}");
            1
        }
    }
}

fn run_get_fan_oneshot() -> i32 {
    let Some((uio, variant)) = discover_fan_uio() else {
        eprintln!("dcentrald --get-fan: no 'fan-control' UIO found under /sys/class/uio");
        return 1;
    };

    match dcentrald_hal::fan::FanController::open_with_variant(uio, variant) {
        Ok(fan) => {
            let discovery = dcentrald_hal::fan::FanUioDiscovery {
                uio_number: uio,
                variant,
                has_board_control: matches!(variant, dcentrald_hal::fan::FanVariant::Am2Uio16),
            };
            print_fan_snapshot("--get-fan", discovery, &fan, None, true);
            0
        }
        Err(e) => {
            eprintln!(
                "dcentrald --get-fan: failed to open fan-control uio{} ({:?}): {}",
                uio, variant, e
            );
            1
        }
    }
}

fn parse_fan_sweep_list(raw: Option<&str>) -> Result<Vec<u8>, String> {
    let Some(raw) = raw else {
        return Err(
            "missing PWM list (usage: --fan-sweep 0,5,10,15,20,25,30 [--dwell-ms 8000])"
                .to_string(),
        );
    };
    let mut values = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("empty PWM entry in sweep list".to_string());
        }
        let pwm = part
            .parse::<u8>()
            .map_err(|_| format!("invalid PWM entry '{part}' in sweep list"))?;
        values.push(pwm);
    }
    if values.is_empty() {
        return Err("empty PWM sweep list".to_string());
    }
    Ok(values)
}

fn parse_fan_sweep_dwell_ms(args: &[String]) -> u64 {
    let raw = args
        .iter()
        .position(|a| a == "--dwell-ms")
        .and_then(|pos| args.get(pos + 1))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(8_000);
    raw.clamp(250, 60_000)
}

fn run_fan_sweep_oneshot(list_arg: Option<&str>, dwell_ms: u64, allow_loud: bool) -> i32 {
    let list = match parse_fan_sweep_list(list_arg) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("dcentrald --fan-sweep: {e}");
            return 2;
        }
    };

    let Ok((discovery, fan)) = dcentrald_hal::fan::FanController::open_discovered() else {
        eprintln!("dcentrald --fan-sweep: failed to discover/open fan-control UIO");
        return 1;
    };

    println!(
        "dcentrald --fan-sweep: uio={} variant={:?} dwell_ms={} values={:?}",
        discovery.uio_number, discovery.variant, dwell_ms, list
    );

    for requested in list {
        let pwm = clamp_set_fan_pwm(requested, allow_loud);
        if pwm != requested {
            eprintln!(
                "dcentrald --fan-sweep: clamped requested PWM {} -> {} (allow_loud={}; PWM_SAFETY_MAX={}, PWM_MAX={})",
                requested,
                pwm,
                allow_loud,
                dcentrald_hal::fan::PWM_SAFETY_MAX,
                dcentrald_hal::fan::PWM_MAX
            );
        }
        fan.set_speed(pwm);
        std::thread::sleep(std::time::Duration::from_millis(dwell_ms));
        let (commanded_pwm, max_rpm) =
            print_fan_snapshot("--fan-sweep", discovery, &fan, Some(pwm), true);
        if am2_low_pwm_floor_present(discovery.variant, pwm, max_rpm) {
            println!(
                "dcentrald --fan-sweep: LOW_PWM_FLOOR_PRESENT requested={} commanded_pwm={} max_rpm={}",
                pwm, commanded_pwm, max_rpm
            );
        }
    }
    0
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

fn config_selects_bm1362(config: &DcentraldConfig) -> bool {
    config
        .mining
        .serial_chip_type
        .as_deref()
        .map(|chip| chip.eq_ignore_ascii_case("BM1362"))
        .unwrap_or(false)
        || config.mining.model_chip_id() == Some(0x1362)
}

fn configured_asic_protocol_identity(
    config: &DcentraldConfig,
) -> std::result::Result<Option<dcentrald_common::AsicProtocolIdentity>, String> {
    use dcentrald_common::AsicProtocolIdentity;

    let serial_identity = config
        .mining
        .serial_chip_type
        .as_deref()
        .and_then(AsicProtocolIdentity::from_chip_label);
    let model_identity = config
        .mining
        .model_chip_id()
        .and_then(AsicProtocolIdentity::from_chip_id);

    if let (Some(serial), Some(model)) = (serial_identity, model_identity) {
        if serial != model {
            return Err(format!(
                "configured ASIC identity contradicts itself: serial_chip_type={serial:?}, model={model:?}"
            ));
        }
    }

    Ok(serial_identity.or(model_identity))
}

/// Existing top-level mining arm selected by CLI/auto-detection precedence.
///
/// This is deliberately not a miner model. It describes only the runtime's
/// requested chain/work ownership so BoardDesc can reject proven composition
/// contradictions without selecting ASIC, hashboard, voltage, PSU, cooling,
/// storage, or network behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeDispatchKind {
    Am3BeagleBone,
    StratumProxy,
    Tap,
    S19jHybrid,
    Serial,
    StockFpga,
    StandardDaemon,
}

impl RuntimeDispatchKind {
    const ALL: [Self; 7] = [
        Self::Am3BeagleBone,
        Self::StratumProxy,
        Self::Tap,
        Self::S19jHybrid,
        Self::Serial,
        Self::StockFpga,
        Self::StandardDaemon,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Am3BeagleBone => "am3-beaglebone",
            Self::StratumProxy => "stratum-proxy",
            Self::Tap => "tap",
            Self::S19jHybrid => "s19j-hybrid",
            Self::Serial => "serial",
            Self::StockFpga => "stock-fpga",
            Self::StandardDaemon => "standard-daemon",
        }
    }

    const fn owns_chain_or_work(self) -> bool {
        !matches!(self, Self::StratumProxy)
    }

    const fn required_facets(
        self,
    ) -> (
        Option<dcentrald_common::ChainTransportKind>,
        Option<dcentrald_common::WorkEngineKind>,
    ) {
        use dcentrald_common::{ChainTransportKind, WorkEngineKind};
        match self {
            // These arms have exact, non-overlapping composition contracts.
            Self::Am3BeagleBone => (
                Some(ChainTransportKind::Serial),
                Some(WorkEngineKind::SerialWork),
            ),
            Self::S19jHybrid => (
                Some(ChainTransportKind::ZynqHybrid),
                Some(WorkEngineKind::SerialWork),
            ),
            // Native serial is also an explicit AM2 diagnostic route, so its
            // work engine is exact while its carrier transport is intentionally
            // unresolved here. Existing AM2 lab admission remains downstream.
            Self::Serial => (None, Some(WorkEngineKind::SerialWork)),
            // Proxy owns no miner hardware. Tap/stock/default have ownership or
            // transport distinctions BoardDesc cannot generally express
            // faithfully. Exact am1-s9 standard-daemon resolution is applied
            // later with the descriptor in hand; all other rows fail closed
            // until they gain an explicit ownership contract.
            Self::StratumProxy | Self::Tap | Self::StockFpga | Self::StandardDaemon => (None, None),
        }
    }

    /// Exact ASIC protocol required by engines whose register plan and work
    /// codec are not yet family-generic. A matching carrier is insufficient:
    /// these arms must receive a typed protocol-admission proof.
    const fn required_asic_protocol(self) -> Option<dcentrald_common::AsicProtocolIdentity> {
        use dcentrald_common::AsicProtocolIdentity;
        match self {
            Self::Am3BeagleBone | Self::Tap | Self::S19jHybrid => {
                Some(AsicProtocolIdentity::Bm1362)
            }
            Self::StratumProxy | Self::Serial | Self::StockFpga | Self::StandardDaemon => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeDispatchAdmission {
    MiningInactive,
    ExternalHardwareOwner,
    Compatible {
        asic_protocol: Option<dcentrald_common::AsicProtocolAdmission>,
    },
}

impl RuntimeDispatchAdmission {
    fn require_asic_protocol(
        self,
        expected: dcentrald_common::AsicProtocolIdentity,
    ) -> std::result::Result<dcentrald_common::AsicProtocolAdmission, String> {
        match self {
            Self::Compatible {
                asic_protocol: Some(admission),
            } if admission.identity() == expected => Ok(admission),
            _ => Err(format!(
                "runtime dispatch lacks exact {expected:?} ASIC protocol admission"
            )),
        }
    }
}

fn selected_runtime_dispatch(
    am3_bb: bool,
    stratum_proxy: bool,
    tap: bool,
    s19j_hybrid: bool,
    serial: bool,
    stock_fpga: bool,
) -> RuntimeDispatchKind {
    if am3_bb {
        RuntimeDispatchKind::Am3BeagleBone
    } else if stratum_proxy {
        RuntimeDispatchKind::StratumProxy
    } else if tap {
        RuntimeDispatchKind::Tap
    } else if s19j_hybrid {
        RuntimeDispatchKind::S19jHybrid
    } else if serial {
        RuntimeDispatchKind::Serial
    } else if stock_fpga {
        RuntimeDispatchKind::StockFpga
    } else {
        RuntimeDispatchKind::StandardDaemon
    }
}

fn admit_board_desc_runtime_dispatch(
    board_desc: Option<&dcentrald_common::BoardDesc>,
    dispatch: RuntimeDispatchKind,
    mining_start_enabled: bool,
    configured_asic_protocol: Option<dcentrald_common::AsicProtocolIdentity>,
) -> std::result::Result<RuntimeDispatchAdmission, String> {
    use dcentrald_common::WorkEngineKind;

    if !mining_start_enabled {
        return Ok(RuntimeDispatchAdmission::MiningInactive);
    }
    if !dispatch.owns_chain_or_work() {
        return Ok(RuntimeDispatchAdmission::ExternalHardwareOwner);
    }
    let required_asic_protocol = dispatch.required_asic_protocol();
    let Some(board_desc) = board_desc else {
        if let Some(required) = required_asic_protocol {
            return Err(format!(
                "runtime dispatch {} requires exact {required:?} ASIC protocol admission, but no BoardDesc is registered",
                dispatch.label()
            ));
        }
        return Err(format!(
            "runtime dispatch {} owns mining hardware but no BoardDesc is registered",
            dispatch.label()
        ));
    };
    if board_desc.work_engine == WorkEngineKind::ManagementOnly {
        return Err(format!(
            "BoardDesc {} is management-only but runtime dispatch {} would own mining chain/work",
            board_desc.board_target,
            dispatch.label()
        ));
    }

    // The direct serial route selects an ASIC implementation before opening
    // its hardware-owning constructor. Bind that configured identity to the
    // complete BoardDesc here; a RuntimeDiscovered descriptor is not enough to
    // authorize pre-discovery mutations.
    if dispatch == RuntimeDispatchKind::Serial {
        let configured = configured_asic_protocol.ok_or_else(|| {
            format!(
                "runtime dispatch {} requires an explicit ASIC identity before hardware construction",
                dispatch.label()
            )
        })?;
        if board_desc.asic_protocol == dcentrald_common::AsicProtocolIdentity::RuntimeDiscovered {
            return Err(format!(
                "BoardDesc {} leaves ASIC protocol runtime-discovered; serial hardware construction requires passive identity refinement first",
                board_desc.board_target
            ));
        }
        board_desc.admit_asic_protocol(Some(configured), board_desc.asic_protocol)?;
    }

    let asic_protocol_admission = required_asic_protocol
        .map(|required| board_desc.admit_asic_protocol(configured_asic_protocol, required))
        .transpose()?;

    let (required_transport, required_work_engine) =
        if dispatch == RuntimeDispatchKind::StandardDaemon && board_desc.board_target == "am1-s9" {
            // Exact am1-s9 is the one proven standard-daemon composition: init
            // opens FpgaChain (UIO) and the runtime owns WorkDispatcher's FPGA work
            // path. Do not generalize this from Zynq family or matching facets;
            // other standard-daemon targets remain unresolved.
            (
                Some(dcentrald_common::ChainTransportKind::FpgaUio),
                Some(dcentrald_common::WorkEngineKind::FpgaWorkFifo),
            )
        } else {
            dispatch.required_facets()
        };
    if let Some(required_transport) = required_transport {
        if board_desc.chain_transport != required_transport {
            return Err(format!(
                "BoardDesc {} declares chain transport {:?}, incompatible with runtime dispatch {} requiring {:?}",
                board_desc.board_target,
                board_desc.chain_transport,
                dispatch.label(),
                required_transport
            ));
        }
    }
    if let Some(required_work_engine) = required_work_engine {
        if board_desc.work_engine != required_work_engine {
            return Err(format!(
                "BoardDesc {} declares work engine {:?}, incompatible with runtime dispatch {} requiring {:?}",
                board_desc.board_target,
                board_desc.work_engine,
                dispatch.label(),
                required_work_engine
            ));
        }
    }

    if required_transport.is_none() && required_work_engine.is_none() {
        Err(format!(
            "runtime dispatch {} has no exact transport/work ownership contract for BoardDesc {}; refusing hardware construction",
            dispatch.label(),
            board_desc.board_target
        ))
    } else {
        Ok(RuntimeDispatchAdmission::Compatible {
            asic_protocol: asic_protocol_admission,
        })
    }
}

#[cfg(test)]
mod board_desc_runtime_dispatch_tests {
    use super::*;

    const MAIN_SOURCE: &str = include_str!("main.rs");
    const DAEMON_SOURCE: &str = include_str!("daemon.rs");
    const TAP_SOURCE: &str = include_str!("s19j_tap_mining.rs");
    const STOCK_SOURCE: &str = include_str!("stock_mining.rs");
    const SERIAL_SOURCE: &str = include_str!("serial_mining.rs");
    const S17_INIT_SOURCE: &str = include_str!(
        "../../../br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/etc/init.d/S82dcentrald"
    );
    const S19PRO_INIT_SOURCE: &str = include_str!(
        "../../../br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/etc/init.d/S82dcentrald"
    );

    fn future_desc(
        board_target: &'static str,
        chain_transport: dcentrald_common::ChainTransportKind,
        work_engine: dcentrald_common::WorkEngineKind,
    ) -> dcentrald_common::BoardDesc {
        dcentrald_common::BoardDesc {
            board_target,
            family: dcentrald_common::BoardFamily::Zynq,
            chain_transport,
            work_engine,
            asic_protocol: dcentrald_common::AsicProtocolIdentity::RuntimeDiscovered,
            voltage_controller: dcentrald_common::VoltageControllerClass::RuntimeDiscovered,
            slot_policy: dcentrald_common::SlotPolicy::LabGated,
            enablement: dcentrald_common::HardwareEnablementPolicy {
                storage_topology: dcentrald_common::StorageTopology::Unknown,
                update_mechanism: dcentrald_common::UpdateMechanism::None,
                update_maturity: dcentrald_common::ImplementationMaturity::NotImplemented,
                install_authorization: dcentrald_common::InstallAuthorization::Denied,
                recovery_maturity: dcentrald_common::RecoveryMaturity::NotImplemented,
                artifact_kind: dcentrald_common::ArtifactKind::RuntimeBundle,
                artifact_maturity: dcentrald_common::ArtifactMaturity::Experimental,
            },
            public_beta_install: false,
            mining_default_enabled: false,
        }
    }

    #[test]
    fn board_desc_dispatch_priority_matches_the_runtime_branch_order() {
        assert_eq!(
            selected_runtime_dispatch(true, true, true, true, true, true),
            RuntimeDispatchKind::Am3BeagleBone
        );
        assert_eq!(
            selected_runtime_dispatch(false, true, true, true, true, true),
            RuntimeDispatchKind::StratumProxy
        );
        assert_eq!(
            selected_runtime_dispatch(false, false, true, true, true, true),
            RuntimeDispatchKind::Tap
        );
        assert_eq!(
            selected_runtime_dispatch(false, false, false, true, true, true),
            RuntimeDispatchKind::S19jHybrid
        );
        assert_eq!(
            selected_runtime_dispatch(false, false, false, false, true, true),
            RuntimeDispatchKind::Serial
        );
        assert_eq!(
            selected_runtime_dispatch(false, false, false, false, false, true),
            RuntimeDispatchKind::StockFpga
        );
        assert_eq!(
            selected_runtime_dispatch(false, false, false, false, false, false),
            RuntimeDispatchKind::StandardDaemon
        );
    }

    #[test]
    fn board_desc_dispatch_unknown_hardware_ownership_fails_closed() {
        for dispatch in RuntimeDispatchKind::ALL {
            assert_eq!(
                admit_board_desc_runtime_dispatch(None, dispatch, false, None).unwrap(),
                RuntimeDispatchAdmission::MiningInactive
            );
            let admitted = admit_board_desc_runtime_dispatch(None, dispatch, true, None);
            if dispatch == RuntimeDispatchKind::StratumProxy {
                assert_eq!(
                    admitted.unwrap(),
                    RuntimeDispatchAdmission::ExternalHardwareOwner
                );
            } else {
                assert!(
                    admitted.is_err(),
                    "{dispatch:?} must require BoardDesc identity"
                );
            }
        }
    }

    #[test]
    fn board_desc_admission_failure_preserves_cooling_and_closes_api_mutation() {
        let marker =
            "BoardDesc runtime dispatch contradiction; parking management-only before mining hardware construction";
        let start = MAIN_SOURCE
            .rfind(marker)
            .expect("runtime admission-failure branch must remain present");
        let end = MAIN_SOURCE[start..]
            .find("if am3_bb_mode")
            .map(|offset| start + offset)
            .expect("runtime dispatch must follow admission handling");
        let branch = &MAIN_SOURCE[start..end];

        assert!(branch.contains("enter_management_only_idle("));
        assert!(
            branch.contains("spawn_proxy_mode_api_with_hardware_mutation_gate(")
                && branch.contains("HardwareMutationGate::new_closed()"),
            "an unadmitted composition must expose a read-only API plane"
        );
        assert!(
            !branch.contains("am2_quiet_idle_tuple(")
                && !branch.contains("force_am2_fans_to_quiet_idle"),
            "admission failure has no rail-off proof and must not lower cooling"
        );
    }

    #[test]
    fn board_desc_dispatch_matrix_is_exhaustive_and_requires_exact_contracts() {
        use dcentrald_common::{ChainTransportKind, WorkEngineKind};

        for board_desc in dcentrald_common::BoardDesc::all_registered() {
            for dispatch in RuntimeDispatchKind::ALL {
                let result = admit_board_desc_runtime_dispatch(
                    Some(board_desc),
                    dispatch,
                    true,
                    Some(board_desc.asic_protocol),
                );
                if dispatch == RuntimeDispatchKind::StratumProxy {
                    assert_eq!(
                        result.unwrap(),
                        RuntimeDispatchAdmission::ExternalHardwareOwner,
                        "{}",
                        board_desc.board_target
                    );
                    continue;
                }
                if board_desc.work_engine == WorkEngineKind::ManagementOnly {
                    assert!(
                        result.is_err(),
                        "{} / {dispatch:?}",
                        board_desc.board_target
                    );
                    continue;
                }

                match dispatch {
                    RuntimeDispatchKind::Am3BeagleBone => {
                        assert_eq!(
                            result.is_ok(),
                            board_desc.chain_transport == ChainTransportKind::Serial
                                && board_desc.work_engine == WorkEngineKind::SerialWork
                                && board_desc.asic_protocol
                                    == dcentrald_common::AsicProtocolIdentity::Bm1362,
                            "{}",
                            board_desc.board_target
                        );
                    }
                    RuntimeDispatchKind::S19jHybrid => {
                        assert_eq!(
                            result.is_ok(),
                            board_desc.chain_transport == ChainTransportKind::ZynqHybrid
                                && board_desc.work_engine == WorkEngineKind::SerialWork
                                && board_desc.asic_protocol
                                    == dcentrald_common::AsicProtocolIdentity::Bm1362,
                            "{}",
                            board_desc.board_target
                        );
                    }
                    RuntimeDispatchKind::Serial => {
                        assert_eq!(
                            result.is_ok(),
                            board_desc.work_engine == WorkEngineKind::SerialWork
                                && board_desc.asic_protocol
                                    != dcentrald_common::AsicProtocolIdentity::RuntimeDiscovered,
                            "{}",
                            board_desc.board_target
                        );
                    }
                    RuntimeDispatchKind::StandardDaemon if board_desc.board_target == "am1-s9" => {
                        assert_eq!(
                            result.unwrap(),
                            RuntimeDispatchAdmission::Compatible {
                                asic_protocol: None
                            }
                        )
                    }
                    RuntimeDispatchKind::Tap | RuntimeDispatchKind::StockFpga => assert!(
                        result.is_err(),
                        "{} / {dispatch:?} must not construct hardware without an exact ownership contract",
                        board_desc.board_target
                    ),
                    RuntimeDispatchKind::StandardDaemon => assert!(
                        result.is_err(),
                        "{} must not enter the standard hardware route",
                        board_desc.board_target
                    ),
                    RuntimeDispatchKind::StratumProxy => unreachable!(),
                }
            }
        }
    }

    #[test]
    fn board_desc_dispatch_exact_s9_resolves_standard_daemon_without_generalizing() {
        use dcentrald_common::{
            BoardDesc, BoardFamily, ChainTransportKind, SlotPolicy, VoltageControllerClass,
            WorkEngineKind,
        };

        let s9 = BoardDesc::lookup("am1-s9").unwrap();
        assert_eq!(
            admit_board_desc_runtime_dispatch(
                Some(s9),
                RuntimeDispatchKind::StandardDaemon,
                true,
                Some(dcentrald_common::AsicProtocolIdentity::Bm1387),
            )
            .unwrap(),
            RuntimeDispatchAdmission::Compatible {
                asic_protocol: None
            }
        );

        // Matching facets on a future target do not mint exact S9 authority.
        let future_fpga = BoardDesc {
            board_target: "future-fpga-uio",
            family: BoardFamily::Zynq,
            chain_transport: ChainTransportKind::FpgaUio,
            work_engine: WorkEngineKind::FpgaWorkFifo,
            asic_protocol: dcentrald_common::AsicProtocolIdentity::RuntimeDiscovered,
            voltage_controller: VoltageControllerClass::RuntimeDiscovered,
            slot_policy: SlotPolicy::LabGated,
            enablement: dcentrald_common::HardwareEnablementPolicy {
                storage_topology: dcentrald_common::StorageTopology::Unknown,
                update_mechanism: dcentrald_common::UpdateMechanism::None,
                update_maturity: dcentrald_common::ImplementationMaturity::NotImplemented,
                install_authorization: dcentrald_common::InstallAuthorization::Denied,
                recovery_maturity: dcentrald_common::RecoveryMaturity::NotImplemented,
                artifact_kind: dcentrald_common::ArtifactKind::RuntimeBundle,
                artifact_maturity: dcentrald_common::ArtifactMaturity::Experimental,
            },
            public_beta_install: false,
            mining_default_enabled: false,
        };
        assert!(admit_board_desc_runtime_dispatch(
            Some(&future_fpga),
            RuntimeDispatchKind::StandardDaemon,
            true,
            None,
        )
        .is_err());

        // Conversely, a contradictory exact row is rejected rather than
        // accepted from the target string alone.
        let contradictory_s9 = BoardDesc {
            chain_transport: ChainTransportKind::Serial,
            ..future_fpga.clone()
        };
        let contradictory_s9 = BoardDesc {
            board_target: "am1-s9",
            ..contradictory_s9
        };
        assert!(admit_board_desc_runtime_dispatch(
            Some(&contradictory_s9),
            RuntimeDispatchKind::StandardDaemon,
            true,
            None,
        )
        .is_err());

        assert!(admit_board_desc_runtime_dispatch(
            None,
            RuntimeDispatchKind::StandardDaemon,
            true,
            None,
        )
        .is_err());
    }

    #[test]
    fn board_desc_dispatch_s9_resolution_is_pinned_to_actual_standard_runtime_ownership() {
        let init_signature = ["async fn in", "it("].concat();
        let init_start = DAEMON_SOURCE.find(&init_signature).unwrap();
        let init_body = &DAEMON_SOURCE[init_start..];
        assert!(init_body.contains("FpgaChain::open("));

        let lifecycle_signature = ["async fn run_", "lifecycle("].concat();
        let lifecycle_start = DAEMON_SOURCE.find(&lifecycle_signature).unwrap();
        let lifecycle_body = &DAEMON_SOURCE[lifecycle_start..init_start];
        assert!(lifecycle_body.contains("crate::work_dispatcher::WorkDispatcher::new("));
    }

    #[test]
    fn board_desc_dispatch_tap_and_stock_fail_closed_without_ownership_contracts() {
        for board_desc in dcentrald_common::BoardDesc::all_registered() {
            for dispatch in [RuntimeDispatchKind::Tap, RuntimeDispatchKind::StockFpga] {
                let result = admit_board_desc_runtime_dispatch(
                    Some(board_desc),
                    dispatch,
                    true,
                    Some(board_desc.asic_protocol),
                );
                assert!(
                    result.is_err(),
                    "{} must not gain {dispatch:?} authority without a sealed ownership contract",
                    board_desc.board_target
                );
            }
        }
    }

    #[test]
    fn board_desc_dispatch_am2_serial_diagnostic_admits_work_without_claiming_carrier() {
        use dcentrald_common::{ChainTransportKind, WorkEngineKind};

        let am2 = dcentrald_common::BoardDesc::lookup("am2-s19j").unwrap();
        assert_eq!(am2.chain_transport, ChainTransportKind::ZynqHybrid);
        let (required_transport, required_work) = RuntimeDispatchKind::Serial.required_facets();
        assert_eq!(
            required_transport, None,
            "serial carrier must remain unresolved"
        );
        assert_eq!(required_work, Some(WorkEngineKind::SerialWork));
        assert_eq!(
            admit_board_desc_runtime_dispatch(
                Some(am2),
                RuntimeDispatchKind::Serial,
                true,
                Some(dcentrald_common::AsicProtocolIdentity::Bm1362),
            )
            .unwrap(),
            RuntimeDispatchAdmission::Compatible {
                asic_protocol: None
            }
        );

        assert!(MAIN_SOURCE.contains("DCENT_ALLOW_AM2_BM1362_SERIAL_WORK"));
    }

    #[test]
    fn board_desc_dispatch_future_facet_lookalikes_do_not_gain_tap_or_stock_authority() {
        use dcentrald_common::{ChainTransportKind, WorkEngineKind};

        let tap_lookalike = future_desc(
            "future-am2-lookalike",
            ChainTransportKind::ZynqHybrid,
            WorkEngineKind::SerialWork,
        );
        let stock_lookalike = future_desc(
            "future-stock-lookalike",
            ChainTransportKind::StockFpga,
            WorkEngineKind::StockDma,
        );
        let s9_facet_copy = future_desc(
            "future-s9-facet-copy",
            ChainTransportKind::FpgaUio,
            WorkEngineKind::FpgaWorkFifo,
        );

        assert!(admit_board_desc_runtime_dispatch(
            Some(&tap_lookalike),
            RuntimeDispatchKind::Tap,
            true,
            Some(tap_lookalike.asic_protocol),
        )
        .is_err());

        for (board_desc, dispatch) in [
            (&stock_lookalike, RuntimeDispatchKind::StockFpga),
            (&s9_facet_copy, RuntimeDispatchKind::StockFpga),
        ] {
            assert!(
                admit_board_desc_runtime_dispatch(
                    Some(board_desc),
                    dispatch,
                    true,
                    Some(board_desc.asic_protocol),
                )
                .is_err(),
                "{} must fail closed for {dispatch:?}",
                board_desc.board_target
            );
        }
    }

    #[test]
    fn s19pro_bm1398_can_never_enter_the_bm1362_hybrid_engine() {
        use dcentrald_common::{AsicProtocolIdentity, BoardDesc, WorkEngineKind};

        let s19pro = BoardDesc::lookup("am2-s19pro").expect("registered S19 Pro target");
        assert_eq!(s19pro.asic_protocol, AsicProtocolIdentity::Bm1398);
        assert_eq!(s19pro.work_engine, WorkEngineKind::ManagementOnly);
        assert!(admit_board_desc_runtime_dispatch(
            Some(s19pro),
            RuntimeDispatchKind::S19jHybrid,
            true,
            Some(AsicProtocolIdentity::Bm1398),
        )
        .is_err());
        assert!(s19pro
            .admit_asic_protocol(
                Some(AsicProtocolIdentity::Bm1398),
                AsicProtocolIdentity::Bm1362,
            )
            .is_err());
    }

    #[test]
    fn non_bm1362_am2_launchers_never_inject_the_bm1362_hybrid_recipe() {
        for (name, source) in [
            ("am2-s17pro", S17_INIT_SOURCE),
            ("am2-s19pro", S19PRO_INIT_SOURCE),
        ] {
            let selector_start = source
                .find("IS_BM1362_HYBRID_TARGET=0")
                .unwrap_or_else(|| panic!("{name}: missing exact protocol selector"));
            let selector_end = source[selector_start..]
                .find("# Operator escape hatch")
                .map(|offset| selector_start + offset)
                .unwrap_or(source.len());
            let selector = &source[selector_start..selector_end];

            assert!(selector
                .contains("am2-s19j|am2-s19jpro|am2-s19jpro-zynq) IS_BM1362_HYBRID_TARGET=1"));
            assert!(selector.contains("if [ \"$IS_BM1362_HYBRID_TARGET\" = \"1\" ]; then"));
            assert!(!selector.contains("if [ \"$IS_AM2\" = \"1\" ]; then"));
            assert!(selector.contains("unset DCENT_AM2_SERIAL_WORK_DISPATCH"));
        }
    }

    #[test]
    fn board_desc_dispatch_unresolved_contract_is_pinned_to_actual_runtime_ownership() {
        // Tap has split ownership that BoardDesc cannot currently express.
        assert!(TAP_SOURCE.contains("DevmemFpgaChain::open_am2("));
        assert!(TAP_SOURCE.contains("fpga.write_work("));
        assert!(TAP_SOURCE.contains("bosminer owns PSU/PIC/serial"));
        assert!(!TAP_SOURCE.contains(".enable_voltage("));
        assert!(!TAP_SOURCE.contains(".set_voltage("));

        // Stock is a distinct kernel/DMA substrate, not S9's UIO work path.
        assert!(STOCK_SOURCE.contains("StockFpga::open("));
        assert!(STOCK_SOURCE.contains("StockFpgaDma::open("));
        assert!(STOCK_SOURCE.contains("StockFpgaWorkEngine::new("));

        // Serial carrier is selected downstream from device paths and a
        // separate lab gate, so main admission may validate work only.
        assert!(SERIAL_SOURCE.contains("SerialChainBackend::open_passthrough("));
        assert!(SERIAL_SOURCE.contains("UartTransService::open_paths_with_baud("));
        assert!(SERIAL_SOURCE.contains("DCENT_AM3_BB_ENABLE_UART_TRANS_LAB"));
    }

    #[test]
    fn board_desc_dispatch_has_one_consumer_before_every_mining_engine_constructor() {
        let run_signature = ["async fn run_", "main() -> Result<()> {"].concat();
        let run_end_marker = ["// `spawn_proxy_mode_api` ", "moved"].concat();
        let run_start = MAIN_SOURCE.find(&run_signature).unwrap();
        let run_end = MAIN_SOURCE[run_start..]
            .find(&run_end_marker)
            .map(|offset| run_start + offset)
            .unwrap();
        let run_body = &MAIN_SOURCE[run_start..run_end];
        assert_eq!(
            run_body
                .matches("admit_board_desc_runtime_dispatch(")
                .count(),
            1,
            "main runtime must have one typed BoardDesc dispatch consumer"
        );
        let admission = run_body.find("admit_board_desc_runtime_dispatch(").unwrap();
        for constructor in [
            "run_am3_bb_mining(",
            "S19jTapMiner::new(",
            "S19jHybridMiner::new(",
            "SerialMiner::new(",
            "StockMiner::new(",
            "Daemon::new(",
        ] {
            let constructor = run_body
                .find(constructor)
                .unwrap_or_else(|| panic!("missing runtime constructor {constructor}"));
            assert!(
                admission < constructor,
                "admission must precede {constructor}"
            );
        }

        let identity_capture = run_body
            .find("capture_system_platform_identity()")
            .expect("immutable platform identity capture");
        assert_eq!(
            run_body
                .matches("capture_system_platform_identity()")
                .count(),
            1
        );
        assert!(identity_capture < admission);
        assert!(!run_body.contains("auto_detect_s19j_hybrid()"));
        assert!(!run_body.contains("am3_bb_mining::auto_detect_am3_bb()"));
        assert!(run_body.contains(
            "Daemon::new(\n            config,\n            resolved_config_path,\n            platform_identity,"
        ));
        assert!(DAEMON_SOURCE.contains("let platform_identity = self.platform_identity.clone();"));
        let daemon_lifecycle = DAEMON_SOURCE
            .split("async fn run_lifecycle(&mut self)")
            .nth(1)
            .expect("daemon lifecycle body");
        assert!(
            !daemon_lifecycle.contains("capture_identity(&SystemPlatformIdentitySource)"),
            "standard daemon must consume the admitted snapshot, never reread identity"
        );

        for stale_reread in [
            "let legacy_psu_board_target = read_first_trimmed(",
            "let thermal_has_xadc = !detect_control_board()",
            "let mixed_chip_platform = read_first_trimmed(",
            "let control_board = detect_control_board();",
            "std::fs::read_to_string(\"/etc/dcentos/platform\")",
        ] {
            assert!(
                !daemon_lifecycle.contains(stale_reread),
                "post-admission policy must not reread mutable identity: {stale_reread}"
            );
        }
    }
}

/// Read `/etc/dcentos/board_target` once, before the tokio runtime is built,
/// so we can size the runtime to the detected platform. S17 / S17 Pro
/// images stamp `am2-s17p`, while older lab notes may still say `am1-s17`.
/// These low-RAM boards cannot afford the default tokio
/// blocking-pool of 512 threads — each holds a ~2 MB stack reservation.
///
/// Returns `(worker_threads, max_blocking_threads)`. When the file is
/// missing or unrecognized we fall back to tokio defaults (worker count
/// matches CPU; blocking pool = 512).
fn tokio_pool_for_board_target() -> (Option<usize>, Option<usize>) {
    let target = std::fs::read_to_string("/etc/dcentos/board_target")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    tokio_pool_for_board_target_marker(&target)
}

fn tokio_pool_for_board_target_marker(target: &str) -> (Option<usize>, Option<usize>) {
    match target.trim() {
        // S17: 228 MB RAM, dual-core Cortex-A9. 2 workers + 4 blocking
        // threads (vs tokio default 512) keeps the resident set well under
        // the RAM budget while still giving spawn_blocking enough lanes for
        // I²C + UIO + sysfs-thermal reads.
        "am2-s17p"
        | "am2-s17pro"
        | "am2-s17plus"
        | "am2-t17"
        | "am2-t17plus"
        | "x17-s17e-dspic-planned"
        | "x17-t17e-pic16-planned"
        | "am2-s17"
        | "am1-s17" => (Some(2), Some(4)),
        // BCB100 is a 128/256 MB-class STM32MP15 board depending on BOM/firmware
        // reporting; keep the runtime pool bounded like other low-RAM targets.
        "bcb100-s19jpro-lab" | "bcb100-s19-lab" => (Some(2), Some(4)),
        // S9 / S19/S19j Pro Zynq / am3-* etc.: tokio defaults.
        _ => (None, None),
    }
}

/// Install the process-wide cut-hash-on-crash panic hook (W24-CRASH-1).
///
/// On a `panic = "abort"` build, `Drop` impls do not run, so the
/// `Am2HomeHardStopGuard` RAII teardown is bypassed on a panic. This hook is
/// the software backstop: it runs the same best-effort cut-hash-before-noise
/// teardown the guard would have (drive PWR_CONTROL low FIRST, then command
/// fans to quiet idle, capped at PWM 30), but ONLY if an am2 hybrid run armed
/// the process-global teardown params (i.e. it actually energized hardware).
/// If nothing was armed, the teardown is a no-op. After the best-effort
/// teardown the previous (default) hook is invoked so the panic message is
/// still emitted, then the runtime aborts as configured.
fn install_cut_hash_on_crash_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort cut-hash-before-noise FIRST. This only touches sysfs
        // GPIO + the fan UIO via helpers that swallow their own errors and do
        // not allocate in the no-op path; it never re-panics on its own.
        crate::s19j_hybrid_mining::panic_hook_best_effort_teardown();
        // am3-aml NoPic (S21 / S19k Pro / S19j Pro-Amlogic): NoPic uses TAS5782M DAC
        // voltage with NO PIC heartbeat watchdog, so on a panic this is the ONLY
        // backstop that cuts PSU power (PWR_EN gpio437) + quiet-coasts fans. No-op
        // (allocation-free) unless a NoPic run armed it. Fire-critical — without it a
        // panic leaves an am3-aml home unit's boards energized indefinitely.
        // (wf_e0647147 swarm GAP #1, 2026-05-29.)
        crate::serial_mining::nopic_panic_hook_best_effort_teardown();
        // am3-bb (S19j Pro on AM335x BeagleBone): panic="abort" bypasses
        // Am3BbRunSafetyGuard::Drop. The fw=0x89 dsPIC's ~1-min hardware watchdog
        // would eventually cut voltage, but this cuts board-enable (hashboard power)
        // + asserts ASIC resets IMMEDIATELY instead. No-op unless an am3-bb run armed
        // it. Fans are already <= PWM_SAFETY_MAX by construction (Am3BbCappedFan).
        // (wf_7c757213 cross-platform safety audit, 2026-05-29.)
        crate::am3_bb_mining::am3_bb_panic_hook_best_effort_teardown();
        // stock-fpga (S9 BM1387 on /dev/axi_fpga_dev): StockMiner energizes the
        // chip rail via FPGA-I2C enable_voltage but has NO Drop guard, so on a
        // panic="abort" the rail stays up until only the ~60 s PIC heartbeat
        // watchdog cuts it. This cuts it immediately. No-op (allocation-free)
        // unless a stock-fpga run armed it. (prod-readiness hunt #16, 2026-05-29.)
        crate::stock_mining::stock_fpga_panic_hook_best_effort_teardown();
        // Chain to the default hook so the panic location/message is logged,
        // then `panic = "abort"` aborts the process as before.
        previous(info);
    }));
}

fn main() -> Result<()> {
    // W24-CRASH-1 (w24-thermal-safety F-1): install the cut-hash-on-crash
    // backstop BEFORE the tokio runtime starts. The release profile sets
    // `panic = "abort"` (see `Cargo.toml [profile.release]`), so on a Rust
    // panic the process aborts and NO `Drop` impl runs — including the
    // `Am2HomeHardStopGuard::Drop` run-scope safety net that drives PWR_CONTROL
    // low + fans to quiet idle. Without this hook the only remaining backstop
    // on a panic is the ~30 s hardware PIC/PSU heartbeat watchdog, leaving an
    // am2 home unit's chains energized for up to ~30 s. This hook reads the
    // process-global teardown params the am2 hybrid run-scope arms when it
    // energizes PWR_CONTROL and performs a best-effort cut-hash-before-noise
    // teardown, then chains to the default hook so the panic message is still
    // printed and the process still aborts (`panic = "abort"` is preserved —
    // the hook only runs first). The hook is intentionally tiny and
    // allocation-free in its hot path; it is a no-op if no run armed it.
    install_cut_hash_on_crash_panic_hook();

    let (worker_threads, max_blocking) = tokio_pool_for_board_target();
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if let Some(n) = worker_threads {
        builder.worker_threads(n);
    }
    if let Some(n) = max_blocking {
        builder.max_blocking_threads(n);
    }
    let runtime = builder.build()?;
    runtime.block_on(run_main())
}

async fn run_main() -> Result<()> {
    // Parse command-line arguments
    let args: Vec<String> = std::env::args().collect();

    // CLI info one-shots (`--help`/`-h`, `--version`/`-V`). MUST be the
    // FIRST thing checked — before the fan one-shots, config load,
    // logging init, mode routing, and ALL hardware/daemon bring-up.
    // Closes the defect where `dcentrald --help` (no handler existed)
    // fell through to the auto-`s19j-hybrid` path and silently started
    // the mining daemon. A "show me usage" / "what version" invocation
    // must print and exit(0), never daemonize.
    match wants_cli_info(&args) {
        Some(CliInfoRequest::Help) => {
            print!("{}", cli_help_text());
            std::process::exit(0);
        }
        Some(CliInfoRequest::Version) => {
            println!("dcentrald {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        None => {}
    }

    // Unrecognized-flag STRICT REJECT (Wave C, 2026-05-19).
    //
    // The previous non-blocking warning was a deferred TODO pending the
    // launcher-flag audit. Wave A audited every production launcher
    // (`T8_FLAG_ALLOWLIST.md`); Wave B closed G-T8-1 by removing the
    // single known offender (`--passthrough`) from the 3 deploy scripts
    // (commit `5aaae39f`). Pre-Wave-C re-audit confirmed zero
    // `NOT_IN_ALLOWLIST` flags across all 11 production launcher sites.
    //
    // Strict-reject now: a typo like `--gte-fan` (the 2026-05-19 `a lab unit`
    // over-spawn pattern — a flag the binary didn't recognize → silent
    // daemonize) fails loudly with exit 2 + full help text dump instead
    // of silently starting the mining daemon on a misconfigured invocation.
    //
    // eprintln (not tracing): logging is not initialized this early —
    // consistent with the fan one-shots.
    let unknown_flags = unrecognized_cli_flags(&args);
    if !unknown_flags.is_empty() {
        eprintln!(
            "dcentrald: ERROR: unrecognized flag(s) {:?}. \
             Refusing to start to prevent silent daemonization on a typo'd \
             flag (Wave C strict-reject; G-T8-1 closed in Wave B). \
             The canonical flag list:",
            unknown_flags
        );
        eprintln!();
        eprintln!("{}", cli_help_text());
        std::process::exit(2);
    }

    // R2 (2026-05-17): `--set-fan <PWM>` one-shot. This MUST run BEFORE config
    // load, logging init, any mode routing, and any hardware bring-up. It is
    // a tiny synchronous open→write→readback→exit that touches ONLY the FPGA
    // fan-control register (no PIC/PSU/ASIC/I2C/voltage, no tokio HW runtime).
    // Used by the am2 init-script paths (S82dcentrald fan_safety_override /
    // clean-stop, dcentos-early-init.sh) which can't reach the UIO-bound am2
    // fan IP via devmem
    //.
    // Exit codes are a defined contract for the init script:
    //   0 = fan PWM written + read back, 1 = UIO open/discovery failed,
    //   2 = bad/missing PWM argument.
    if let Some(pos) = args.iter().position(|a| a == "--set-fan") {
        let pwm_arg = args.get(pos + 1).map(String::as_str);
        let allow_loud = args.iter().any(|a| a == "--allow-loud");
        std::process::exit(run_set_fan_oneshot(pwm_arg, allow_loud));
    }
    // Wave (2026-05-29, fan-custodian): `--hold-fan <PWM>` PERSISTENT fan
    // custodian. Unlike `--set-fan` (write once → exit), this parks the fan at
    // the given PWM and then STAYS RESIDENT, re-asserting the PWM on a ~5 s
    // tokio interval until SIGTERM/SIGINT. This is the AM2-safe replacement for
    // the init-script `--set-fan N` that drifts loud: on AM2 (XIL Zynq) the fan
    // PWM is held only while a process owns the uio16 mmap AND keeps
    // re-commanding it (mmap-hold alone is NOT enough — a daemon that wrote PWM
    // once still drifted; the 5 s re-assert is the cure). It reuses the SAME
    // in-process park path `park_management_only_until_shutdown` uses
    // (FanController open + periodic `force_am2_fans_to_quiet_idle`), so it
    // re-enables C52 fan mode + idle PWM each tick. Clamp is identical to
    // `--set-fan` (≤ PWM_SAFETY_MAX home cap unless `--allow-loud`). Runs BEFORE
    // config/logging/mode routing; it is a NEW opt-in subcommand and cannot
    // alter the normal daemon path. SIGTERM-responsive (the init-script `stop`
    // path kills it by pidfile). It never exits on its own (returns only on
    // signal); rule: .
    if let Some(pos) = args.iter().position(|a| a == "--hold-fan") {
        let pwm_arg = args.get(pos + 1).map(String::as_str);
        let allow_loud = args.iter().any(|a| a == "--allow-loud");
        std::process::exit(run_hold_fan(pwm_arg, allow_loud).await);
    }
    if args.iter().any(|a| a == "--get-fan") {
        std::process::exit(run_get_fan_oneshot());
    }
    if let Some(pos) = args.iter().position(|a| a == "--fan-sweep") {
        let list_arg = args.get(pos + 1).map(String::as_str);
        let dwell_ms = parse_fan_sweep_dwell_ms(&args);
        let allow_loud = args.iter().any(|a| a == "--allow-loud");
        std::process::exit(run_fan_sweep_oneshot(list_arg, dwell_ms, allow_loud));
    }
    // Wave-B no-brick (2026-05-28): `--safe-off` operator emergency power cut.
    // Runs BEFORE config/logging/mode routing (same as --set-fan); cuts ASIC
    // power per platform then sets fans quiet. New opt-in subcommand — cannot
    // alter the normal daemon path.
    if args.iter().any(|a| a == "--safe-off") {
        std::process::exit(run_safe_off_oneshot());
    }
    // On-device OTA bundle verification one-shot (wf_c00e5d9e follow-up,
    // 2026-05-29): reuses the daemon's ed25519 + manifest verifier so install
    // scripts/operators get an in-band signature re-verify the device's tool set
    // otherwise lacks. Runs BEFORE config/logging/daemon routing (same as the fan
    // one-shots); read-only verify + exit, never flashes — cannot alter the
    // normal daemon path.
    if let Some(pos) = args.iter().position(|a| a == "--verify-bundle") {
        let path_arg = args.get(pos + 1).map(String::as_str);
        std::process::exit(run_verify_bundle_oneshot(path_arg));
    }

    let stock_fpga_mode = args.iter().any(|a| a == "--stock-fpga");
    let serial_mining_mode = args.iter().any(|a| a == "--serial-mining");
    let s19j_hybrid_cli = args.iter().any(|a| a == "--s19j-hybrid");
    // Phase 6 Option 3: tap mode. bosminer owns PIC/PSU/ASIC state — dcentrald
    // only dispatches work to the FPGA. Defensive alternative to --s19j-hybrid
    // while PIC 0x86 + PSU framing are unresolved.
    let tap_mode = args.iter().any(|a| a == "--tap-mode");
    // Phase 11B: Stratum V1 TCP relay. bosminer keeps full control of
    // hardware; dcentrald only proxies the Stratum V1 byte stream between
    // bosminer (localhost:3333) and the upstream pool. Zero hardware access.
    // See DCENT_OS_Antminer/dcentrald/dcentrald/src/stratum_proxy.rs.
    let stratum_proxy_mode = args.iter().any(|a| a == "--stratum-proxy");
    // Phase C (2026-05-12): AM335x BeagleBone S19j Pro (S19J_IO_BOARD_V2_0,
    // the `a lab unit`-class unit) mining mode. Cold-boot via the Phase-B
    // `BeagleBonePlatform` + `cold_boot_sequence_s19j_io_v2`, work dispatch via
    // the Phase-1 clean-room `bm1362::uart_transport::Am335xUartTransport` (no
    // kernel module). Auto-detected when the board-target file says
    // `am3-bb-s19jpro` or the device-tree model is an `S19J_IO_BOARD`.
    // See DCENT_OS_Antminer/dcentrald/dcentrald/src/am3_bb_mining.rs.
    let am3_bb_cli = args.iter().any(|a| a == "--am3-bb-mining");
    let platform_identity = crate::daemon::capture_system_platform_identity()?;
    let am3_bb_auto = platform_identity.board_target() == "am3-bb-s19jpro";
    let am3_bb_mode = am3_bb_cli || am3_bb_auto;

    // Auto-route am2 platforms into s19j-hybrid mode only when the operator did
    // not explicitly request another bring-up path. This keeps the am2 safety
    // guard for the default case while still letting us intentionally run the
    // serial/tap/proxy experiments on .139.
    let (auto_hybrid, detected_platform) = classify_s19j_hybrid_auto(
        platform_identity.platform_marker(),
        platform_identity.board_target(),
    );
    let explicit_non_hybrid_mode =
        stock_fpga_mode || serial_mining_mode || tap_mode || stratum_proxy_mode || am3_bb_mode;
    let s19j_hybrid_mode = s19j_hybrid_cli || (auto_hybrid && !explicit_non_hybrid_mode);

    // Find --config <path> argument
    let config_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());

    // Load configuration. NO-BRICK CONTRACT (gap-swarm daemon-startup #1/#9):
    // a config problem must NEVER `?`-crash here — the S82dcentrald wrapper treats
    // a non-zero exit as a crash and, after MAX_CRASH_RESTARTS, permanently gives
    // up, leaving a unit with no daemon / no :8080 API / no dashboard (the exact
    // unmanageable-brick state F1/F5 management-only was built to prevent). And a
    // PRESENT-BUT-CORRUPT primary config must NOT silently revert to the baked
    // /etc default (mining-enabled on some platforms). So:
    //   * primary loads          -> use it
    //   * primary ABSENT         -> try /etc fallback; if that also fails -> defaults
    //   * primary present+INVALID -> fail CLOSED to management-only defaults
    //                                (do NOT revert to /etc)
    // management_only_default() forces mining.enabled=false => management-only.
    let (config, resolved_config_path) = match DcentraldConfig::load(&config_path) {
        Ok(cfg) => (cfg, config_path.clone()),
        Err(primary_err) => {
            if std::path::Path::new(&config_path).exists() {
                eprintln!(
                    "dcentrald: config at {} is PRESENT but INVALID ({:#}) — NOT reverting to the baked default; entering MANAGEMENT-ONLY with safe defaults (mining disabled)",
                    config_path, primary_err
                );
                (
                    DcentraldConfig::management_only_default(),
                    "<management-only-default:invalid-primary>".to_string(),
                )
            } else {
                match DcentraldConfig::load(FALLBACK_CONFIG_PATH) {
                    Ok(cfg) => {
                        eprintln!(
                            "dcentrald: config {} absent — using fallback {}",
                            config_path, FALLBACK_CONFIG_PATH
                        );
                        (cfg, FALLBACK_CONFIG_PATH.to_string())
                    }
                    Err(fallback_err) => {
                        eprintln!(
                            "dcentrald: no usable config — {} absent and {} failed ({:#}) — entering MANAGEMENT-ONLY with safe defaults (mining disabled) instead of crashing",
                            config_path, FALLBACK_CONFIG_PATH, fallback_err
                        );
                        (
                            DcentraldConfig::management_only_default(),
                            "<management-only-default:no-config>".to_string(),
                        )
                    }
                }
            }
        }
    };

    // Initialize structured logging. No-brick: a logging-subsystem failure must
    // NOT crash the daemon (the S82dcentrald wrapper treats a non-zero exit as a
    // crash and, after MAX_CRASH_RESTARTS, gives up → permanent unmanageable
    // brick). The safety/management plane (API/dashboard/management-only park,
    // re-flash detection) does not depend on structured logging — degrade
    // closed-but-alive instead of dying. (gap-swarm daemon-startup #3)
    if let Err(e) = init_logging(&config.general.log_level) {
        eprintln!("dcentrald: logging init failed ({e:#}) — continuing without configured logging");
    }

    // W1.4: install the process-wide log-tail mask flag from [logging].
    // Default (true) masks wallet addresses on `/api/debug/log` responses.
    // Per-call masking on `worker=` / `username=` / `wallet=` log fields is
    // independent of this flag and cannot be disabled via config — see
    // `dcentrald-common/src/wallet_mask.rs`.
    dcentrald_common::set_mask_logs(config.logging.mask_logs);

    // Print the D-Central banner — first thing a miner sees on boot
    print_banner();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        config_path = %resolved_config_path,
        stock_fpga = stock_fpga_mode,
        serial_mining = serial_mining_mode,
        s19j_hybrid = s19j_hybrid_mode,
        s19j_hybrid_cli = s19j_hybrid_cli,
        s19j_hybrid_auto = auto_hybrid,
        tap_mode = tap_mode,
        stratum_proxy = stratum_proxy_mode,
        am3_bb = am3_bb_mode,
        am3_bb_auto = am3_bb_auto,
        platform = %detected_platform,
        "dcentrald starting — loading configuration and preparing hardware"
    );

    // wf_c00e5d9e OTA follow-up: self-attest `--verify-bundle` capability. Drop a
    // marker file so install scripts (e.g. install_amlogic_persistent.sh) can
    // SAFELY decide whether the on-device dcentrald supports the verb via a
    // no-daemon-start `[ -f ]` check BEFORE invoking it for an in-band ed25519
    // OTA re-verify — resolving the probe-safety problem (invoking --verify-bundle
    // on a pre-verb binary could start the daemon). Best-effort: /data may be
    // read-only/absent on some platforms; failure is non-fatal and never blocks
    // startup. Any run of THIS binary proves verb support, so dropping it on the
    // daemon path is correct.
    match publish_verify_bundle_capability(std::path::Path::new(VERIFY_BUNDLE_CAPABILITY_PATH)) {
        Ok(_) => {}
        Err(VerifyBundleCapabilityPublishError::CreateParent(error)) => tracing::debug!(
            error = %error,
            persistence_stage = "create-parent",
            target_published = false,
            publication_durability_uncertain = false,
            "could not publish verify-bundle capability sentinel (non-fatal)"
        ),
        Err(VerifyBundleCapabilityPublishError::Publish(error)) => tracing::debug!(
            error = %error,
            persistence_stage = %error.stage(),
            target_published = error.target_published(),
            publication_durability_uncertain = error.target_published(),
            cleanup_error = ?error.cleanup_error(),
            "could not publish verify-bundle capability sentinel (non-fatal)"
        ),
    }

    // Explicit operator-facing log line when we flipped the mode ourselves.
    // Operators SSHing into a bring-up unit need to see this reason clearly —
    // the alternative (silent auto-route) was a painful bring-up surprise on .139.
    if s19j_hybrid_mode && auto_hybrid && !s19j_hybrid_cli {
        info!(
            platform = %detected_platform,
            "auto-enabled s19j-hybrid from /etc/bos_platform={} (no --s19j-hybrid CLI flag passed)",
            detected_platform
        );
    }

    //  HIGH-8 (2026-05-24): recipe-broken runtime guard.
    //
    // Refuse to start on `a lab unit`-class AM2-XIL-Loki hardware if any of the
    // 4 LIVE-FALSIFIED "must-not-set" env vars from the  PROVEN
    // MINING RECIPE are present. Operator Gate-1 Q2 (2026-05-24): the
    // chosen posture is fail-closed at startup — silently mining with a
    // recipe-broken env on `a lab unit` would burn an AC-cycle for the
    // operator before they notice the broken handoff. See:
    //   * src/wave55a_recipe_guard.rs (decision + logging)
    //   * tests/wave55a_recipe_broken_guard_refuses_forbidden_env_on_xil_25.rs
    //   *
    //   *
    //
    // Position: AFTER config + logging init + platform-stamp read,
    // BEFORE any I²C / UIO / PSU / PIC / chain hardware touch (no-op
    // pass for S9 / `a lab unit` / `a lab unit` / `a lab unit` / `a lab unit` — guarded by
    // fingerprint match on platform `zynq-bm3-am2` + board_target
    // ending with `xil`, so non-`a lab unit` units never see  noise).
    //
    // Override (lab-only): `DCENT_BYPASS_WAVE54_GUARD=1` flips refusal
    // to a loud warn + continue.
    if let Err(refusal) = wave55a_recipe_guard::enforce() {
        // `tracing::error!` was already emitted inside `enforce()` with
        // the full operator-facing details + runbook + memory-rule
        // pointers. Use a clean EX_CONFIG exit code rather than
        // `?`-bubbling an anyhow stack trace — the operator gets a
        // distinct exit code (78) to distinguish "recipe-broken env"
        // from generic runtime failures (1) or strict-reject unknown
        // flag (2). The `_refusal` data is already in the structured
        // error log; nothing else to do here.
        let _ = refusal;
        std::process::exit(wave55a_recipe_guard::EX_CONFIG_EXIT_CODE);
    }

    // Wave K Lane A: load the on-disk silicon-profile registry into the global
    // RwLock so the `/api/profiles/silicon/*` REST surface reflects real
    // on-device profiles (shipped at /etc/dcentrald/profiles.d via the shared
    // rootfs overlays). Fail-safe: a missing dir → empty registry (logged), a
    // malformed JSON file → skipped. READ-ONLY: this does NOT drive mining
    // freq/voltage — the daemon setpoint path is `MinerProfile::for_chip` + the
    // autotuner slug presets, both untouched. Runs in every mode so the API is
    // consistent. (Driving setpoints from a resolved profile is the deferred
    // Wave-K-C / matrix §7 #15 power-adjacent work, NOT this.)
    {
        let profile_dir = dcentrald_api_types::profile_schema::PROFILE_DROP_IN_DIR;
        match dcentrald_silicon_profiles::registry::global().write() {
            Ok(mut reg) => match reg.reload(std::path::Path::new(profile_dir)) {
                Ok(stats) => info!(
                    dir = profile_dir,
                    loaded = stats.loaded,
                    skipped = stats.skipped,
                    "silicon-profile registry loaded from disk (REST /api/profiles/silicon/*)"
                ),
                Err(e) => tracing::warn!(
                    dir = profile_dir,
                    error = %e,
                    "silicon-profile registry load failed — continuing with empty registry"
                ),
            },
            Err(_) => tracing::warn!("silicon-profile registry lock poisoned — skipping load"),
        }
    }

    // Create cancellation token for coordinated shutdown
    let shutdown_token = CancellationToken::new();
    let shutdown_token_signal = shutdown_token.clone();

    // Spawn signal handler task
    tokio::spawn(async move {
        let ctrl_c = signal::ctrl_c();
        // No-brick: SIGTERM registration essentially never fails on Linux, but if
        // it does, degrade to SIGINT-only rather than panicking — the release
        // profile sets `panic = "abort"`, so an `.expect()` here would abort the
        // whole daemon (the wrapper counts that as a crash). Mirrors the graceful
        // SIGTERM-registration handling in run_hold_fan. (gap-swarm daemon-startup #4)
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => {
                        info!("Received SIGINT, initiating graceful shutdown");
                    }
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM, initiating graceful shutdown");
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "dcentrald: SIGTERM handler registration failed ({e}) — falling back to SIGINT-only shutdown"
                );
                let _ = ctrl_c.await;
                info!("Received SIGINT, initiating graceful shutdown");
            }
        }

        shutdown_token_signal.cancel();
    });

    // Supremacy S5.1: gRPC server scaffold. Spawn alongside REST/CGMiner when
    // `[api.grpc] enabled = true`. Default OFF — scaffold-priority; most
    // handlers return UNIMPLEMENTED until wired to live state. GetConstraints
    // is the one real handler (returns BM1362 envelope with home-mode fan
    // cap + 14500 mV am2 voltage cap). The task runs detached: an error from
    // tonic only kills the gRPC listener, never the daemon, mirroring the
    // existing REST `spawn_proxy_mode_api` semantics.
    if config.api.grpc.enabled {
        let grpc_addr_str = format!("{}:{}", config.api.grpc.bind, config.api.grpc.port);
        match grpc_addr_str.parse::<std::net::SocketAddr>() {
            Ok(addr) => {
                let home_mode =
                    dcentrald_api::OperatingMode::from_config_str(&config.mode.active).is_home();
                // CE-122: resolve the chip family so GetConstraints returns a
                // family-honest envelope instead of always BM1362. Fail-closed
                // chain: configured serial_chip_type -> model label -> stamped
                // board_target file. Empty -> BM1362 default (byte-identical).
                let grpc_chip_family = config
                    .mining
                    .serial_chip_type
                    .clone()
                    .or_else(|| {
                        config
                            .mining
                            .model
                            .as_deref()
                            .and_then(model::model_chip_label)
                            .map(str::to_string)
                    })
                    .or_else(|| {
                        model::board_target_chip_label(platform_identity.board_target())
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                tokio::spawn(async move {
                    if let Err(err) =
                        dcentrald_api_grpc::serve(addr, home_mode, grpc_chip_family).await
                    {
                        error!(error = %err, "dcentrald-api-grpc server exited with error");
                    }
                });
                info!(
                    addr = %grpc_addr_str,
                    "spawned dcentrald-api-grpc scaffold server (S5.1 — most RPCs return UNIMPLEMENTED)"
                );
            }
            Err(err) => {
                error!(
                    bind = config.api.grpc.bind.as_str(),
                    port = config.api.grpc.port,
                    error = %err,
                    "invalid [api.grpc] bind/port — gRPC server NOT started"
                );
            }
        }
    }

    // TD-003 defense-in-depth: the config validator blocks these platforms when
    // `[mining].model` is correct, but the baked board stamp is the stronger
    // runtime signal. If an in-development platform image carries a stale or
    // missing model field and mining would otherwise start, park management-only
    // before any voltage, PIC/PMBus, ASIC init, or hash-dispatch path.
    let td003_board_target = platform_identity.board_target().to_string();
    if config.mining_start_enabled() {
        if let Some(model_name) = model::td003_management_only_board_target(&td003_board_target) {
            tracing::warn!(
                board_target = %td003_board_target.trim(),
                platform = %detected_platform,
                model = model_name,
                "mining startup blocked by board-target gate: platform is an Experimental feature / In development and remains management-only until promotion gates complete"
            );
            let (_runtime_health_tx, runtime_health_rx) =
                tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                    dcentrald_api::RuntimeHealthMode::Native,
                ));
            let _api_handles =
                crate::runtime::api::spawn_proxy_mode_api_with_hardware_mutation_gate(
                    config.clone(),
                    dcentrald_api::RuntimeHealthMode::Native,
                    Some(runtime_health_rx),
                    dcentrald_hal::platform::HardwareMutationGate::new_closed(),
                    shutdown_token.clone(),
                )
                .await?;
            return enter_management_only_idle(
                "td003-board-target",
                config.mining.enabled,
                config.has_configured_pool(),
                shutdown_token.clone(),
                // Admission failure proves neither rail-off nor an idle thermal
                // state. Preserve the existing cooling command; lowering PWM
                // here could leave a hot donor/passthrough board under-cooled.
                None,
            )
            .await;
        }
    }

    #[cfg(feature = "sim-hal")]
    if dcentrald_hal::platform::sim::sim_environment_is_mentioned() {
        return sim_runtime::run(config, shutdown_token.clone()).await;
    }

    // Typed composition admission over the existing dispatch order. BoardDesc
    // does not choose a mining arm; CLI/auto-detection above still does. It can
    // only reject a proven transport/work-engine/ASIC contradiction before an
    // arm constructs hardware. Unknown and unresolved hardware ownership now
    // parks management-only; safe-direction cleanup remains independently
    // available and does not require mining admission.
    let runtime_dispatch = selected_runtime_dispatch(
        am3_bb_mode,
        stratum_proxy_mode,
        tap_mode,
        s19j_hybrid_mode,
        serial_mining_mode,
        stock_fpga_mode,
    );
    let runtime_board_desc = platform_identity.board_desc;
    let mut s19j_hybrid_route_admission = match configured_asic_protocol_identity(&config).and_then(
        |configured_asic_protocol| {
            let runtime_admission = admit_board_desc_runtime_dispatch(
                runtime_board_desc,
                runtime_dispatch,
                config.mining_start_enabled(),
                configured_asic_protocol,
            )?;
            let hybrid_route_admission = if runtime_dispatch == RuntimeDispatchKind::S19jHybrid
                && config.mining_start_enabled()
            {
                Some(s19j_hybrid_admission::admit_s19j_hybrid_route(
                    &platform_identity,
                    runtime_dispatch,
                    configured_asic_protocol,
                )?)
            } else {
                None
            };
            Ok((runtime_admission, hybrid_route_admission))
        },
    ) {
        Ok((admission, hybrid_route_admission)) => {
            info!(
                board_target = %td003_board_target.trim(),
                board_desc_registered = runtime_board_desc.is_some(),
                dispatch = runtime_dispatch.label(),
                admission = ?admission,
                hybrid_route_admitted = hybrid_route_admission.is_some(),
                "BoardDesc runtime transport/work/ASIC-protocol admission evaluated"
            );
            hybrid_route_admission
        }
        Err(reason) => {
            tracing::warn!(
                board_target = %td003_board_target.trim(),
                dispatch = runtime_dispatch.label(),
                reason = %reason,
                "BoardDesc runtime dispatch contradiction; parking management-only before mining hardware construction"
            );
            let (_runtime_health_tx, runtime_health_rx) =
                tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                    dcentrald_api::RuntimeHealthMode::Native,
                ));
            let _api_handles =
                crate::runtime::api::spawn_proxy_mode_api_with_hardware_mutation_gate(
                    config.clone(),
                    dcentrald_api::RuntimeHealthMode::Native,
                    Some(runtime_health_rx),
                    dcentrald_hal::platform::HardwareMutationGate::new_closed(),
                    shutdown_token.clone(),
                )
                .await?;
            return enter_management_only_idle(
                "board-desc-dispatch",
                config.mining.enabled,
                config.has_configured_pool(),
                shutdown_token.clone(),
                // Rejection proves neither rail-off nor an idle thermal state.
                // Preserve cooling on a possibly hot inherited composition.
                None,
            )
            .await;
        }
    };

    if am3_bb_mode {
        // Phase C: AM335x BeagleBone S19j Pro (S19J_IO_BOARD_V2_0) mining.
        // Bring up the dashboard / CGMiner API first (same pattern as the
        // other modes — closes the
        // class), then run the cold-boot + transport-setup + (Phase-C stub)
        // mining loop. See am3_bb_mining.rs.
        info!(
            am3_bb_cli = am3_bb_cli,
            am3_bb_auto = am3_bb_auto,
            "Entering AM335x BB mining mode (--am3-bb-mining)"
        );
        let (_runtime_health_tx, runtime_health_rx) =
            tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                dcentrald_api::RuntimeHealthMode::Native,
            ));

        // R1: compute the am2 low-idle command tuple before `config` is moved into
        // the miner. am3-bb is never `zynq-bm3-am2*` ⇒ this is None here (the
        // am3-bb cold-boot guard handles its own fans), but threading it
        // keeps the park-path signature uniform across all arms.
        let am2_qi = am2_quiet_idle_tuple(&detected_platform, &config);

        // F5: same fresh-unit gate as the s19j-hybrid arm. An unconfigured
        // am3-bb unit comes up management-only (API/dashboard/wizard
        // reachable, no cold-boot, no PSU/chain I/O) until the operator
        // configures a pool and enables mining.
        if !config.mining_start_enabled() {
            let _api_handles =
                crate::runtime::api::spawn_proxy_mode_api_with_hardware_mutation_gate(
                    config.clone(),
                    dcentrald_api::RuntimeHealthMode::Native,
                    Some(runtime_health_rx),
                    dcentrald_hal::platform::HardwareMutationGate::new_closed(),
                    shutdown_token.clone(),
                )
                .await?;
            return enter_management_only_idle(
                "am3-bb",
                config.mining.enabled,
                config.has_configured_pool(),
                shutdown_token.clone(),
                am2_qi,
            )
            .await;
        }

        // The watchdog must report its initial kick before the API can admit a
        // hardware mutation and before the blocking engine can touch GPIO,
        // I2C, UART, power, or cooling state.
        let safety_admission = match am3_bb_mining::Am3BbSafetyAdmission::start(&config).await {
            Ok(admission) => admission,
            Err(error) => {
                let _api_handles =
                    crate::runtime::api::spawn_proxy_mode_api_with_hardware_mutation_gate(
                        config.clone(),
                        dcentrald_api::RuntimeHealthMode::Native,
                        Some(runtime_health_rx),
                        dcentrald_hal::platform::HardwareMutationGate::new_closed(),
                        shutdown_token.clone(),
                    )
                    .await?;
                return enter_management_only(
                    "am3-bb-watchdog-admission",
                    error,
                    shutdown_token.clone(),
                    am2_qi,
                )
                .await;
            }
        };
        let api_mutation_gate = safety_admission.hardware_mutation_gate();
        let _api_handles =
            match crate::runtime::api::spawn_proxy_mode_api_with_hardware_mutation_gate(
                config.clone(),
                dcentrald_api::RuntimeHealthMode::Native,
                Some(runtime_health_rx),
                api_mutation_gate.clone(),
                shutdown_token.clone(),
            )
            .await
            {
                Ok(handles) => handles,
                Err(error) => {
                    let _ = api_mutation_gate.close_and_drain(std::time::Duration::ZERO);
                    return Err(error.context(
                    "am3-bb: API failed after watchdog admission; mutation gate was closed and watchdog remains armed",
                ));
                }
            };

        let mining_shutdown = shutdown_token.child_token();
        match am3_bb_mining::run_am3_bb_mining(config, mining_shutdown.clone(), safety_admission)
            .await
        {
            Ok(()) => {
                info!("dcentrald (am3-bb) stopped cleanly");
                Ok(())
            }
            // F1: management-only instead of process exit (see
            // `enter_management_only`). The am3-bb cold-boot path runs its
            // own quiet fail-closed guard (fan PWM cap, dsPIC voltage
            // disable, resets asserted, board-enable off) before any error
            // propagates, so hardware is already safely off here.
            Err(e) => {
                mining_shutdown.cancel();
                enter_management_only("am3-bb", e, shutdown_token.clone(), am2_qi).await
            }
        }
    } else if stratum_proxy_mode {
        // Phase 11B MVP: pure Stratum V1 TCP relay. bosminer owns ALL
        // hardware; dcentrald never touches /dev/mem, /dev/i2c*, /dev/ttyS*,
        // or the bosminer API for control. See stratum_proxy.rs for the
        // invariant list.
        //
        // Proxy mode v0 (task #29 / #72): bring the dashboard + CGMiner API
        // up too so operators on `a lab unit` can observe the unit even though
        // bosminer drives the chain. Build a minimal AppState, spawn the API
        // server BEFORE the relay, and start the bosminer health poller.
        info!(
            "Entering STRATUM V1 relay mode (--stratum-proxy) — no hardware access for chain control"
        );
        let stats = stratum_proxy::ProxiedStats::new();
        let (runtime_health_tx, runtime_health_rx) = tokio::sync::watch::channel(
            dcentrald_api::RuntimeHealthSnapshot::for_mode(dcentrald_api::RuntimeHealthMode::Proxy),
        );
        let _api_handles = crate::runtime::api::spawn_proxy_mode_api(
            config.clone(),
            dcentrald_api::RuntimeHealthMode::Proxy,
            Some(runtime_health_rx),
            shutdown_token.clone(),
        )
        .await?;
        let mining_shutdown = shutdown_token.child_token();
        let _health_handle =
            stratum_proxy::spawn_bosminer_health_task(stats.clone(), mining_shutdown.clone());
        let _health_publish_handle = stratum_proxy::spawn_proxy_health_publisher(
            stats.clone(),
            runtime_health_tx,
            mining_shutdown.clone(),
        );
        // R1: compute before `config` moves. Stratum-proxy never owns chain
        // hardware (bosminer does); on an am2 unit a low-idle fan command here
        // is still safe (PWM only ever driven DOWN) — but in practice this is
        // None unless an am2 unit is run in proxy mode.
        let am2_qi = am2_quiet_idle_tuple(&detected_platform, &config);
        match stratum_proxy::run(config, mining_shutdown.clone(), Some(stats)).await {
            Ok(()) => {
                info!("dcentrald (stratum proxy) stopped cleanly");
                Ok(())
            }
            // F1: management-only instead of process exit. Stratum-proxy
            // mode never owns chain hardware (bosminer does), so there is no
            // voltage/chain teardown to perform — the relay simply failed;
            // keep the API/dashboard reachable.
            Err(e) => {
                mining_shutdown.cancel();
                enter_management_only("stratum-proxy", e, shutdown_token.clone(), am2_qi).await
            }
        }
    } else if tap_mode {
        // Tap mode: bosminer owns PIC/PSU/serial/CTRL. dcentrald only dispatches
        // FPGA work. See DCENT_OS_Antminer/dcentrald/dcentrald/src/s19j_tap_mining.rs
        // for preconditions and the no-write invariant list.
        info!("Entering S19J TAP mining mode (--tap-mode) — bosminer owns hardware state");
        // R1: compute before `config` moves. Tap mode never writes chain/PIC/
        // PSU state (bosminer owns it); on an am2 unit a low-idle fan command
        // on the failure park is still safe (PWM only ever driven DOWN).
        let am2_qi = am2_quiet_idle_tuple(&detected_platform, &config);

        // F5 parity: tap mode still owns FPGA WORK_TX dispatch, so a fresh or
        // intentionally management-only unit must not construct/run the tap
        // miner unless mining is explicitly enabled with a configured pool.
        // Keep the API/dashboard reachable for onboarding, then park without
        // opening the FPGA chain or sending work.
        if !config.mining_start_enabled() {
            let (_runtime_health_tx, runtime_health_rx) =
                tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                    dcentrald_api::RuntimeHealthMode::Native,
                ));
            let _api_handles = crate::runtime::api::spawn_proxy_mode_api(
                config.clone(),
                dcentrald_api::RuntimeHealthMode::Native,
                Some(runtime_health_rx),
                shutdown_token.clone(),
            )
            .await?;
            return enter_management_only_idle(
                "tap",
                config.mining.enabled,
                config.has_configured_pool(),
                shutdown_token.clone(),
                am2_qi,
            )
            .await;
        }

        let mining_shutdown = shutdown_token.child_token();
        let mut miner = match S19jTapMiner::new(config, mining_shutdown.clone()) {
            Ok(m) => m,
            Err(e) => {
                error!(error = %e, "Failed to construct S19jTapMiner");
                return Err(e);
            }
        };

        match miner.run().await {
            Ok(()) => {
                info!("dcentrald (tap) stopped cleanly — hardware untouched, bosminer resumes");
                Ok(())
            }
            // F1: management-only instead of process exit. Tap mode never
            // writes chain/PIC/PSU state (bosminer owns it), so there is no
            // hardware teardown to perform — keep the API/dashboard up.
            Err(e) => {
                mining_shutdown.cancel();
                enter_management_only("tap", e, shutdown_token.clone(), am2_qi).await
            }
        }
    } else if s19j_hybrid_mode {
        // S19j Pro hybrid mining: serial ASIC init + FPGA work dispatch via /dev/mem
        //
        // Proxy mode v0 (task #29 / #72): hybrid mode previously skipped
        // `daemon::run()` and therefore never spawned the dashboard / CGMiner
        // API. Bring up a minimal AppState before the mining loop so :8080
        // and :4028 are reachable even when the daemon path isn't entered.
        info!("Entering S19J HYBRID mining mode (--s19j-hybrid)");
        let (_runtime_health_tx, runtime_health_rx) =
            tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                dcentrald_api::RuntimeHealthMode::Hybrid,
            ));
        // AT-DASH (2026-06-14): live MinerState channel for the standalone
        // hybrid path. The API serves the RECEIVER; the mining loop owns the
        // SENDER (attached via `with_state_tx` below) and pushes a fresh
        // MinerState on each am2_serial_status tick so /api/status + the
        // dashboard show real hashrate / per-dsPIC chains / accepted shares
        // instead of a blank default. Additive + fail-closed: if mining never
        // starts (idle branch) the sender is just dropped and the dashboard
        // shows zeros, exactly as before.
        let (miner_state_tx, miner_state_rx) = tokio::sync::watch::channel(
            dcentrald_api::MinerState::empty(dcentrald_api::OperatingMode::Standard),
        );
        let _api_handles = crate::runtime::api::spawn_proxy_mode_api_with_state(
            config.clone(),
            dcentrald_api::RuntimeHealthMode::Hybrid,
            Some(runtime_health_rx),
            Some(miner_state_rx),
            shutdown_token.clone(),
        )
        .await?;

        // R1: compute the am2 low-idle command tuple BEFORE `config` is moved into
        // the miner. On the home `a lab unit` unit (`zynq-bm3-am2`,
        // `[mining] enabled=false` baked default) this is
        // `Some((fan_idle_pwm, fan_max_pwm))` and the F5 idle branch below is
        // the EXACT path `a lab unit` takes — wiring the low-idle command there is
        // the command-path fix for the parked-am2-fans-stay-at-default root cause
        //.
        let am2_qi = am2_quiet_idle_tuple(&detected_platform, &config);

        // F5: a fresh, unconfigured am2 unit must NOT attempt a hardware
        // cold-boot / PIC preflight. The API is already up (spawned just
        // above); if no pool is configured / mining is not enabled, park in
        // management-only mode so the onboarding wizard (complete OR W1-A
        // skip) is reachable. This is the production-correct first-boot
        // behavior and kills the `a lab unit`-class crash at the source — the
        // wrong-config-for-this-unit PIC preflight is never attempted.
        if !config.mining_start_enabled() {
            return enter_management_only_idle(
                "s19j-hybrid",
                config.mining.enabled,
                config.has_configured_pool(),
                shutdown_token.clone(),
                am2_qi,
            )
            .await;
        }

        let mining_shutdown = shutdown_token.child_token();
        // AT-DASH: attach the live MinerState publisher so the hybrid mining
        // loop feeds /api/status + the dashboard.
        let route_admission = s19j_hybrid_route_admission.take().ok_or_else(|| {
            anyhow::anyhow!(
                "s19j-hybrid reached hardware construction without one-shot route admission"
            )
        })?;
        let mut miner = S19jHybridMiner::new(config, mining_shutdown.clone(), route_admission)?
            .with_state_tx(miner_state_tx);

        match miner.run().await {
            Ok(()) => {
                info!("dcentrald (s19j hybrid) stopped cleanly");
                Ok(())
            }
            // F1: do NOT return Err (that would exit the process and kill the
            // already-spawned :8080 API task). The s19j-hybrid path runs
            // `force_am2_home_hard_stop` + power teardown BEFORE it bails
            // (s19j_hybrid_mining.rs:4401-4406) — hardware is already safely
            // off. Stay alive in management-only mode so the
            // dashboard/wizard/toolbox-detector stay reachable.
            Err(e) => {
                mining_shutdown.cancel();
                enter_management_only("s19j-hybrid", e, shutdown_token.clone(), am2_qi).await
            }
        }
    } else if serial_mining_mode {
        if auto_hybrid
            && detected_platform.starts_with("zynq-bm3-am2")
            && config_selects_bm1362(&config)
            && !env_flag("DCENT_ALLOW_AM2_BM1362_SERIAL_WORK")
        {
            anyhow::bail!(
                "--serial-mining on zynq-bm3-am2/BM1362 is lab-only because native AM2 \
                 must dispatch work through FPGA WORK_TX. Use --s19j-hybrid, or set \
                 DCENT_ALLOW_AM2_BM1362_SERIAL_WORK=1 for an explicit diagnostic run."
            );
        }
        // Serial UART mining path — direct ASIC communication via /dev/ttyS*
        // Used for S19j Pro (BM1362) and other UART-based platforms
        if !config.mining_start_enabled() {
            info!(
                mining_enabled = config.mining.enabled,
                pool_configured = config.has_configured_pool(),
                "Serial mining startup gated by config; starting API/dashboard only and skipping hardware init"
            );
            let (_runtime_health_tx, runtime_health_rx) =
                tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                    dcentrald_api::RuntimeHealthMode::Native,
                ));
            let _api_handles = crate::runtime::api::spawn_proxy_mode_api(
                config.clone(),
                dcentrald_api::RuntimeHealthMode::Native,
                Some(runtime_health_rx),
                shutdown_token.clone(),
            )
            .await?;
            shutdown_token.cancelled().await;
            info!("dcentrald (serial idle/API-only) stopped cleanly");
            return Ok(());
        }

        info!("Entering SERIAL mining mode (--serial-mining)");
        // R1: compute before `config` moves. The am2/BM1362 serial path is
        // bailed above unless an explicit lab override is set; on the rare
        // explicit am2 diagnostic run a low-idle fan command on the failure
        // park is still safe (PWM only ever driven DOWN). None on S9/am1.
        let am2_qi = am2_quiet_idle_tuple(&detected_platform, &config);
        let mining_shutdown = shutdown_token.child_token();
        let mut miner = SerialMiner::new(config, mining_shutdown.clone());

        match miner.run().await {
            Ok(()) => {
                info!("dcentrald (serial) stopped cleanly");
                Ok(())
            }
            // F1: management-only instead of process exit — same lifetime
            // semantics this arm already uses for the config-gated idle
            // branch above (spawn API, then `shutdown_token.cancelled()`).
            // SerialMiner performs its own voltage-cut/teardown on failure.
            Err(e) => {
                mining_shutdown.cancel();
                enter_management_only("serial", e, shutdown_token.clone(), am2_qi).await
            }
        }
    } else if stock_fpga_mode {
        // Stock Bitmain FPGA mining path — uses /dev/axi_fpga_dev + /dev/fpga_mem
        // No BraiinsOS boot components or UIO devices required
        info!("Entering STOCK FPGA mining mode (--stock-fpga)");

        // F5 parity (no-brick first-boot): a fresh/unconfigured unit must NOT
        // energize the hash-board voltage rail. StockMiner::run() drives
        // i2c.set_voltage()/enable_voltage() on every detected chain with no
        // internal gate, so apply the SAME mining_start_enabled() gate the
        // s19j-hybrid / serial / am3-bb / default-daemon arms already use:
        // bring the dashboard/CGMiner API up and park management-only when
        // mining is not explicitly enabled with a configured pool. When mining
        // IS enabled the gate passes and the energize path below is unchanged.
        if !config.mining_start_enabled() {
            let (_runtime_health_tx, runtime_health_rx) =
                tokio::sync::watch::channel(dcentrald_api::RuntimeHealthSnapshot::for_mode(
                    dcentrald_api::RuntimeHealthMode::Native,
                ));
            let _api_handles = crate::runtime::api::spawn_proxy_mode_api(
                config.clone(),
                dcentrald_api::RuntimeHealthMode::Native,
                Some(runtime_health_rx),
                shutdown_token.clone(),
            )
            .await?;
            // stock-fpga is am1-s9 class — no am2 quiet-idle tuple (None).
            return enter_management_only_idle(
                "stock-fpga",
                config.mining.enabled,
                config.has_configured_pool(),
                shutdown_token.clone(),
                None,
            )
            .await;
        }

        let mut miner = StockMiner::new(config, shutdown_token.clone());

        match miner.run().await {
            Ok(()) => {
                info!("dcentrald (stock FPGA) stopped cleanly");
                Ok(())
            }
            Err(e) => {
                error!(error = %e, "dcentrald (stock FPGA) exited with error");
                Err(e)
            }
        }
    } else {
        // BraiinsOS FPGA mining path — uses UIO devices + per-chain FIFOs
        // (S9/am1 + am2-s17 Zynq). F1 no-brick parity (gap-swarm daemon-startup
        // #6): on a mining error this arm previously did `error!; Err(e)` →
        // process exit → the in-process :8080 API task dies, the dashboard proxy
        // loses its target, and persistent session admission refuses an
        // unverified replacement (the F1 unmanageable-brick class). Every OTHER mining arm
        // (serial / s19j-hybrid / stock-fpga / am3-bb) already routes its `Err`
        // through `enter_management_only`; the standard daemon arm was the lone
        // exception. Two-part fix:
        //   (1) Daemon::run() now runs the graceful hardware-safe-off teardown on
        //       ANY mining-lifecycle error before returning (the daemon.rs run()
        //       wrapper) — so by the time control reaches here the boards are
        //       de-energized, fans are idled, and the watchdog is disarmed
        //       (enter_management_only's hardware-already-off contract is met);
        //   (2) park in management-only instead of exiting, so the (detached,
        //       still-running) API/dashboard/wizard/re-flash plane stays reachable
        //       and the unit never loses its management plane to a refused replacement.
        // am2_qi (Some only on am2; None on S9/am1) keeps the parked am2 fans at
        // the idle setpoint via the periodic fan-hold refresh; computed BEFORE
        // `config` is moved into Daemon::new.
        let am2_qi = am2_quiet_idle_tuple(&detected_platform, &config);
        let mut daemon = Daemon::new(
            config,
            resolved_config_path,
            platform_identity,
            shutdown_token.clone(),
        );

        match daemon.run().await {
            Ok(()) => {
                info!("dcentrald stopped cleanly — all hardware safely powered down");
                Ok(())
            }
            Err(e) => {
                // Daemon::run() already ran the hardware-safe-off teardown (#6);
                // keep the management plane alive instead of exiting.
                enter_management_only("daemon", e, shutdown_token.clone(), am2_qi).await
            }
        }
    }
}

// `spawn_proxy_mode_api` moved to `crate::runtime::api::spawn_proxy_mode_api`
// (W2.1, 2026-05-07). The `MiningRuntime::default_api_servers` trait method
// in `runtime::mod` now delegates to the same helper, so adding a new
// mining mode automatically gets the dashboard up and closes the
//  regression class.

/// F5 (2026-05-17): a fresh, unconfigured unit comes up MANAGEMENT-ONLY
/// **by design** — no hardware mining cold-boot, no PIC preflight, no
/// crash, no chicken-and-egg.
///
/// `config.mining_start_enabled()` is `self.mining.enabled &&
/// self.has_configured_pool()`. The `serial_mining` arm has honored this
/// gate for a long time (spawn API, then `shutdown_token.cancelled().await`
/// — the "serial idle/API-only" branch). F5 generalizes that same gate to
/// the `s19j-hybrid` and `am3-bb` arms so the *designed* first-boot flow is:
///
///   fresh unit boots → dashboard + onboarding wizard reachable, mining
///   idle, ZERO hardware risk → operator completes OR skips the wizard
///   (W1-A) → operator configures a pool + enables mining → a restart
///   flips `mining_start_enabled()` → mining bring-up runs.
///
/// This removes the crash *at the source* for the fresh-`a lab unit`/am2 case:
/// the wrong-config-for-this-unit PIC preflight is never even attempted on
/// an unconfigured unit. F1 remains the safety net for the case where
/// mining IS enabled but bring-up fails for hardware reasons.
///
/// "Fresh unit does LESS, never MORE": an unconfigured unit performs no
/// PIC/PSU/chain I/O at all — strictly safer than attempting a cold boot.
///
/// R1 (2026-05-17, ):
/// `am2_quiet_idle` is `Some((fan_idle_pwm, fan_max_pwm))` ONLY on the am2
/// platform (gated at the call site on
/// `detected_platform.starts_with("zynq-bm3-am2")`). When present, we drive
/// the uio16-mmap `FanController` to the (down-clamped) idle PWM BEFORE
/// parking — this is the ONLY path that reliably lowers am2 fans on a parked,
/// non-mining unit (the home `a lab unit` path takes exactly this branch with
/// `[mining] enabled=false`; `enter_management_only_idle` previously did ZERO
/// hardware I/O so the fans stayed at hardware default). `None` on S9/am1 +
/// am3 → byte-identical no-op there (those paths use the init-script devmem
/// which is correct on am1). The fan call only ever drives PWM DOWN and never
/// above `PWM_SAFETY_MAX` (30). It is best-effort (a FanController open
/// failure is logged, not fatal — the park must still proceed; no hardware is
/// energized here).
/// Park the (idle, non-mining) process until SIGTERM/SIGINT, RE-ASSERTING the
/// am2 idle fan setpoint on a periodic interval so the AM2/XIL control board's
/// fan IP cannot silently revert the fans to its full-speed default.
///
/// HOME-SAFETY BUG this fixes (live-confirmed 2026-05-29 on the `a lab unit` home
/// unit): on AM2/XIL the fan PWM is asserted ONCE when the unit enters
/// management-only mode (the `AM2 low-idle: parked … idle PWM` log), then the
/// process idles on `shutdown_token.cancelled().await`. Nothing re-commands the
/// PWM, so the AM2 board's fan controller drifts the fans back up to its
/// full-speed default over time — the daemon stays alive while the fans end up
/// blasting (observed: last fan log `idle PWM pwm=10` minutes earlier, fans
/// actually at PWM 100 / 6180 RPM). The proven park call
/// (`force_am2_fans_to_quiet_idle`) physically lowered the fans on entry
/// (6180 → 2880 RPM); it just needs to repeat. The `--set-fan` one-shot CLI
/// path is deliberately NOT used here — it does not lower the register
/// reliably; we reuse the SAME in-process `FanController` park call that the
/// management-only entry already used, which also re-asserts C52 fan mode via
/// `FanController::open_with_variant()` on every refresh.
///
/// SCOPE: the periodic refresh is gated on `am2_quiet_idle == Some(..)`, which
/// is `Some` ONLY on the am2/uio16 fan variant (the call sites set it from
/// `am2_quiet_idle_tuple(..)` keyed on `detected_platform`). On S9/am1 + am3
/// (`None`) this is byte-identical to the previous behavior: a single
/// `shutdown_token.cancelled().await` with no fan I/O.
///
/// SIGTERM-RESPONSIVE: a `tokio::select!` races the 5 s interval tick against
/// `shutdown_token.cancelled()`. A cancel is acted on immediately (the interval
/// branch never blocks the signal branch), so shutdown latency is unchanged
/// from the old direct-await. PWM is only ever driven DOWN (clamped inside
/// `force_am2_fans_to_quiet_idle`); the refresh logs at `debug` to avoid spam,
/// while the one-time entry log stays at `info` (kept by the callers).
async fn park_management_only_until_shutdown(
    mode_label: &str,
    shutdown_token: CancellationToken,
    am2_quiet_idle: Option<(u8, u8)>,
) {
    // S9/am1 + am3, or any non-am2 fan variant: no periodic fan refresh needed
    // (am2_quiet_idle is None). Byte-identical to the prior terminal await.
    let Some((fan_idle_pwm, fan_max_pwm)) = am2_quiet_idle else {
        shutdown_token.cancelled().await;
        return;
    };

    // am2/XIL: re-assert the idle fan setpoint (+ C52 fan mode, via the
    // FanController open path) every ~5 s until SIGTERM so the board's fan IP
    // cannot drift back to its full-speed default while the daemon idles.
    const FAN_REFRESH: std::time::Duration = std::time::Duration::from_secs(5);
    let mut refresh = tokio::time::interval(FAN_REFRESH);
    // The first tick fires immediately; skip-then-wait so we don't double the
    // entry park the caller just performed.
    refresh.tick().await;
    loop {
        tokio::select! {
            _ = shutdown_token.cancelled() => break,
            _ = refresh.tick() => {
                tracing::debug!(
                    mode = mode_label,
                    fan_idle_pwm,
                    fan_max_pwm,
                    "AM2 management-only fan-hold refresh: re-asserting idle PWM \
                     (prevents board fan-controller revert to full speed)"
                );
                crate::s19j_hybrid_mining::force_am2_fans_to_quiet_idle(
                    fan_idle_pwm,
                    fan_max_pwm,
                    "management-only periodic fan-hold refresh (am2, hardware off)",
                );
            }
        }
    }
}

async fn enter_management_only_idle(
    mode_label: &str,
    mining_enabled: bool,
    pool_configured: bool,
    shutdown_token: CancellationToken,
    am2_quiet_idle: Option<(u8, u8)>,
) -> Result<()> {
    info!(
        mode = mode_label,
        mining_enabled,
        pool_configured,
        "mining startup gated by config (no pool configured and/or mining \
         not enabled) — starting API/dashboard/onboarding wizard only and \
         SKIPPING all hardware bring-up (no PIC preflight, no PSU, no chain). \
         This is the production-correct first-boot state for a fresh, \
         unconfigured unit. Mining starts after the operator configures a \
         pool and enables mining (then restarts the daemon)."
    );
    // R1: am2-only low-idle command. Drive the parked, non-mining am2 unit's
    // fans to the configured idle PWM via the proven uio16-mmap FanController
    // BEFORE we park. This is a SEPARATE, LOWER setpoint than the hard-stop
    // cap — it does not replace/reorder the hard-stop, and PWM is only ever
    // driven DOWN (clamped ≤ fan_max_pwm and ≤ PWM_SAFETY_MAX inside the
    // setter). No-op on S9/am1 + am3 (am2_quiet_idle is None there).
    if let Some((fan_idle_pwm, fan_max_pwm)) = am2_quiet_idle {
        crate::s19j_hybrid_mining::force_am2_fans_to_quiet_idle(
            fan_idle_pwm,
            fan_max_pwm,
            "management-only-idle (fresh/unconfigured am2 unit, not mining)",
        );
    }
    // Park until SIGTERM/SIGINT, periodically re-asserting the am2 idle fan
    // setpoint so the board's fan IP cannot revert to full speed while we idle
    // (no-op periodic refresh on S9/am1 + am3 where am2_quiet_idle is None).
    park_management_only_until_shutdown(mode_label, shutdown_token, am2_quiet_idle).await;
    info!(
        mode = mode_label,
        "dcentrald ({mode_label} idle/API-only — unconfigured unit) stopped cleanly"
    );
    Ok(())
}

/// F1 (2026-05-17): decouple the management/API plane lifetime from
/// mining-init success.
///
/// ROOT CAUSE this fixes (`a lab unit` first-boot dcentrald-down,
/// ):
/// when `S19jHybridMiner::run()` (or any other mining arm) returns `Err`,
/// returning that `Err` from `run_main()` exits the process — which tears
/// down the in-process `:8080` API task spawned by `spawn_proxy_mode_api`.
/// With the API dead, the dashboard's `:80`→`:8080` proxy has no target,
/// the onboarding wizard (complete OR W1-A skip) is unreachable, and the
/// toolbox detector returns `board_target=unknown` so the unit can't even
/// be re-flashed. `S82dcentrald` must then refuse an unverified replacement.
/// The management plane must NOT die with mining.
///
/// This is the EXACT pattern the codebase already endorses for the
/// serial-mining *gated-off* case at `main.rs` (the
/// `if !config.mining_start_enabled()` branch in the `serial_mining_mode`
/// arm): spawn the API, then `shutdown_token.cancelled().await` instead of
/// exiting. F1 generalizes that proven precedent from "mining-init *gated*"
/// to "mining-init *failed*", platform-wide.
///
/// SAFETY (proven by call order — see the callers): every mining arm that
/// can fail a hardware preflight has ALREADY run its hardware-safe-off
/// teardown *before* returning `Err`. For the am2 s19j-hybrid PIC-preflight
/// failure that triggered the `a lab unit` incident, that is
/// `force_am2_home_hard_stop(...)` (PWR_CONTROL low / voltage cut / fan →
/// PWM 30 / resets asserted) + `teardown_am2_power_after_failed_pic_preflight(...)`
/// at `s19j_hybrid_mining.rs:4401-4406`, executed strictly BEFORE the
/// `anyhow::bail!` at :4407. By the time control reaches this function the
/// hardware is already de-energized. This function does ZERO hardware
/// access — it only parks the process so the (already-spawned) API stays
/// reachable. No voltage is applied, no chain is re-attempted, no fan blast:
/// the unit sits in management-only mode with hardware safely off until
/// SIGTERM. The `DCENT_AM2_TRUST_RAIL_FALLBACK` default-off gate is
/// untouched.
///
/// R1 (2026-05-17, ):
/// `am2_quiet_idle` is `Some((fan_idle_pwm, fan_max_pwm))` ONLY on the am2
/// platform (gated at the call site). When present, AFTER the mode's pre-bail
/// hardware-safe-off teardown has already run (proven by call order — the
/// hard-stop sets fans to its `fan_max_pwm`/30 cap and cuts voltage BEFORE
/// the Err that drives us here), we drive the now-de-energized am2 unit's
/// fans DOWN further to the idle PWM via the uio16-mmap `FanController`. This
/// is a low-PWM command on an already-safe-off unit, not acoustic proof: PWM is
/// only ever lowered, never raised, never above `PWM_SAFETY_MAX` (30); it
/// does NOT replace or reorder the hard-stop. `None` on S9/am1 + am3 →
/// byte-identical no-op. Best-effort: a FanController open failure is logged,
/// not fatal.
async fn enter_management_only(
    mode_label: &str,
    err: anyhow::Error,
    shutdown_token: CancellationToken,
    am2_quiet_idle: Option<(u8, u8)>,
) -> Result<()> {
    error!(
        mode = mode_label,
        error = %err,
        "mining init failed — entering MANAGEMENT-ONLY mode. \
         Hardware was already safely powered down by the mode's pre-bail \
         teardown (voltage cut, fans ≤ PWM 30, resets asserted). The \
         API/dashboard/onboarding wizard stay reachable so the operator can \
         configure, skip, or re-flash the unit. Mining is disabled until \
         operator action; the process will NOT exit and will NOT re-attempt \
         hardware bring-up."
    );
    // R1: am2-only low-idle command. The mode's pre-bail teardown already ran
    // (hard-stop: PWR_CONTROL low / voltage cut / fans ≤ PWM 30 / resets
    // asserted) BEFORE the Err that drove us here — proven by call order and
    // the f1_* structural tests. Hardware is already de-energized; we now
    // drive the am2 fans DOWN further to the idle setpoint (separate, lower
    // than the hard-stop cap; only ever driven DOWN, never above
    // PWM_SAFETY_MAX). No-op on S9/am1 + am3.
    if let Some((fan_idle_pwm, fan_max_pwm)) = am2_quiet_idle {
        crate::s19j_hybrid_mining::force_am2_fans_to_quiet_idle(
            fan_idle_pwm,
            fan_max_pwm,
            "management-only after mining-init failure (am2, hardware already off)",
        );
    }
    info!(
        mode = mode_label,
        "dcentrald is alive in management-only mode (mining disabled, \
         hardware off). Waiting for SIGTERM."
    );
    // Park until SIGTERM/SIGINT (the signal handler task cancels this token).
    // On am2/XIL the idle fan setpoint is RE-ASSERTED every ~5 s so the board's
    // fan IP cannot silently revert the fans to its full-speed default while the
    // daemon idles here (home-safety fix, live-confirmed 2026-05-29 on `a lab unit`).
    // Identical lifetime + (no-op periodic refresh) semantics to the
    // serial-mining idle/API-only branch on S9/am1 + am3 (am2_quiet_idle None).
    park_management_only_until_shutdown(mode_label, shutdown_token, am2_quiet_idle).await;
    info!(
        mode = mode_label,
        "dcentrald (management-only after mining-init failure) stopped cleanly"
    );
    Ok(())
}

/// Print the D-Central Technologies startup banner.
///
/// This is the first thing a miner operator sees on serial console or SSH.
/// Keep it readable on 80-column terminals.
fn print_banner() {
    eprintln!();
    eprintln!("  ____   ____ _____ _   _ _____           ");
    eprintln!(" |  _ \\ / ___| ____| \\ | |_   _|___  ___  ");
    eprintln!(" | | | | |   |  _| |  \\| | | | / _ \\/ __| ");
    eprintln!(" | |_| | |___| |___| |\\  | | || (_) \\__ \\ ");
    eprintln!(" |____/ \\____|_____|_| \\_| |_| \\___/|___/ ");
    eprintln!();
    eprintln!(
        "  dcentrald v{}  —  D-Central Technologies",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("  Mining Hackers from Laval, QC  |  d-central.tech");
    eprintln!("  Open source firmware for the mining plebs");
    eprintln!();
    eprintln!("  Licensed under GPL-3.0. This software is provided AS-IS,");
    eprintln!("  with NO WARRANTY of any kind. See LICENSE for details.");
    eprintln!();
}

#[cfg(test)]
mod tests {
    //! F1 + F5 regression pins (2026-05-17, `a lab unit` first-boot
    //! dcentrald-down rootcause).
    //!
    //! Most per-mode assertions remain structural because the legacy mining
    //! arms still construct their own HALs. The standard daemon's init-timeout
    //! recovery ordering is now also exercised behaviorally through
    //! `daemon_lifecycle`; these assertions retain coverage for the remaining
    //! arms until they migrate behind the same injectable lifecycle port.

    const MAIN_RS: &str = include_str!("main.rs");
    const S19J_HYBRID_RS: &str = include_str!("s19j_hybrid_mining.rs");
    const DAEMON_RS: &str = include_str!("daemon.rs");
    const SERIAL_MINING_RS: &str = include_str!("serial_mining.rs");
    const AM3_BB_RS: &str = include_str!("am3_bb_mining.rs");
    const STOCK_MINING_RS: &str = include_str!("stock_mining.rs");

    fn first_function_end_offset(source: &str) -> usize {
        source
            .find("\n}\n")
            .or_else(|| source.find("\r\n}\r\n"))
            .or_else(|| source.find("\n}\r\n"))
            .expect("fn end not found")
    }

    #[cfg(unix)]
    #[test]
    fn verify_bundle_capability_publication_is_complete_and_replaceable() {
        let directory = std::env::temp_dir().join(format!(
            "dcentrald-verify-bundle-cap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = directory.join("caps").join("verify-bundle");

        let first = super::publish_verify_bundle_capability(&target).unwrap();
        assert!(!first.replaced_existing);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            super::VERIFY_BUNDLE_CAPABILITY_BYTES
        );
        let second = super::publish_verify_bundle_capability(&target).unwrap();
        assert!(second.replaced_existing);
        assert!(
            super::VERIFY_BUNDLE_CAPABILITY_BYTES.len()
                <= super::VERIFY_BUNDLE_CAPABILITY_MAX_BYTES
        );
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
            1
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn verify_bundle_capability_startup_path_is_atomic_observable_and_nonfatal() {
        let start_marker = ["// wf_c00e5d9e OTA follow-up", ": self-attest"].concat();
        let start = MAIN_RS.find(&start_marker).unwrap();
        let end = MAIN_RS[start..]
            .find("// Explicit operator-facing log line")
            .map(|offset| start + offset)
            .unwrap();
        let startup_block = &MAIN_RS[start..end];

        assert!(startup_block.contains("publish_verify_bundle_capability"));
        assert!(!startup_block.contains("std::fs::write"));
        assert!(startup_block.contains("VerifyBundleCapabilityPublishError::CreateParent"));
        assert!(startup_block.contains("VerifyBundleCapabilityPublishError::Publish"));
        assert!(startup_block.contains("target_published = error.target_published()"));
        assert!(startup_block.contains("publication_durability_uncertain"));
        assert!(!startup_block.contains("return Err"));
        assert!(MAIN_RS.contains("dcentrald_common::atomic_file::atomic_write"));
    }

    #[test]
    fn am2_s19j_auto_route_requires_exact_board_identity() {
        use super::classify_s19j_hybrid_auto;

        assert_eq!(
            classify_s19j_hybrid_auto("zynq-bm3-am2", ""),
            (false, "zynq-bm3-am2".to_string()),
            "generic AM2 platform marker without board_target must not auto-route into S19j"
        );
        assert_eq!(
            classify_s19j_hybrid_auto("zynq-bm3-am2\n", "  "),
            (false, "zynq-bm3-am2".to_string()),
            "whitespace-only board_target is still ambiguous"
        );

        for board_target in [
            "am2-s19j",
            "am2-s19jpro",
            "am2-s19jpro-zynq",
            "am2-s19jpro-xil",
        ] {
            assert_eq!(
                classify_s19j_hybrid_auto("zynq-bm3-am2\n", board_target).0,
                true,
                "{board_target} should keep the established S19j hybrid auto-route"
            );
        }

        for board_target in [
            "am2-s17",
            "am2-s17p",
            "am2-s19",
            "am2-s19pro",
            "am2-s19xp",
            "am2-t17",
            "am2-t19",
            "am3-bb-s19jpro",
        ] {
            assert_eq!(
                classify_s19j_hybrid_auto("zynq-bm3-am2", board_target).0,
                false,
                "{board_target} must stay out of the S19j hybrid auto-route"
            );
        }

        assert_eq!(
            classify_s19j_hybrid_auto("zynq-bm1-s9", "am2-s19j"),
            (false, "zynq-bm1-s9:am2-s19j".to_string()),
            "board_target alone is insufficient without the AM2 platform marker"
        );
    }

    #[test]
    fn s17_board_targets_use_low_memory_tokio_pool() {
        use super::tokio_pool_for_board_target_marker;

        for target in [
            "am2-s17p",
            "am2-s17pro",
            "am2-s17plus",
            "am2-t17",
            "am2-t17plus",
            "x17-s17e-dspic-planned",
            "x17-t17e-pic16-planned",
            "am2-s17",
            "am1-s17",
        ] {
            assert_eq!(
                tokio_pool_for_board_target_marker(target),
                (Some(2), Some(4)),
                "{target} must stay on the bounded low-memory runtime pool"
            );
        }

        assert_eq!(
            tokio_pool_for_board_target_marker("  am2-s17p\n"),
            (Some(2), Some(4)),
            "board_target file whitespace must not defeat the low-memory runtime cap"
        );
    }

    #[test]
    fn s19_family_board_targets_keep_default_tokio_pool() {
        use super::tokio_pool_for_board_target_marker;

        for target in [
            "",
            "am1-s9",
            "am2-s19j",
            "am2-s19jpro",
            "am2-s19jpro-zynq",
            "am2-s19pro",
            "am2-t19",
            "am2-s19xp",
            "am3-s19k",
            "am3-s21",
            "am3-bb-s19jpro",
        ] {
            assert_eq!(
                tokio_pool_for_board_target_marker(target),
                (None, None),
                "{target:?} should keep tokio defaults unless explicitly low-memory capped"
            );
        }
    }

    /// THE KEY SAFETY PROOF (F1): on a real am2 PIC-preflight failure the
    /// hardware-safe-off teardown (`force_am2_home_hard_stop` +
    /// `teardown_am2_power_after_failed_pic_preflight`) executes STRICTLY
    /// BEFORE the `anyhow::bail!` that produces the `Err` — and
    /// `enter_management_only` only runs AFTER that `Err` propagates out of
    /// `miner.run()`. Therefore management-only is reached only after
    /// voltage is cut / fans ≤ PWM 30 / resets asserted. F1 changed ONLY
    /// what happens to the `Err` (park instead of exit); it did not move,
    /// weaken, or bypass the teardown.
    #[test]
    fn f1_hard_stop_precedes_bail_in_pic_preflight_failure() {
        let hard_stop = S19J_HYBRID_RS
            .find("force_am2_home_hard_stop(&self.config, \"pic-get-version-failed\")")
            .expect(
                "F1 SAFETY REGRESSION: the am2 PIC-get-version-failed hard-stop \
                 call disappeared from s19j_hybrid_mining.rs — voltage cut / \
                 fan PWM 30 / resets MUST run before the daemon gives up the \
                 mining path",
            );
        let bail = S19J_HYBRID_RS
            .find("\"PIC GET_VERSION failed at 0x")
            .expect("F1: PIC GET_VERSION bail site missing");

        assert!(
            hard_stop < bail,
            "F1 SAFETY VIOLATION: force_am2_home_hard_stop must lexically \
             precede the PIC GET_VERSION bail! — hardware MUST be powered \
             down before the Err that drives management-only"
        );
        // The matching power teardown must live in the SAME failure block,
        // i.e. between the hard-stop and the bail (there are other,
        // unrelated teardown call sites elsewhere in the file — scope the
        // assertion so it can't pass on a coincidental earlier match).
        let pic_fail_block = &S19J_HYBRID_RS[hard_stop..bail];
        assert!(
            pic_fail_block.contains("teardown_am2_power_after_failed_pic_preflight("),
            "F1 SAFETY VIOLATION: teardown_am2_power_after_failed_pic_preflight \
             must be called between force_am2_home_hard_stop and the PIC \
             GET_VERSION bail! (same fail-closed block)"
        );
    }

    /// F1: `enter_management_only(...)` is invoked ONLY from `Err(e) =>`
    /// match arms — never on the success path and never before the miner
    /// runs. An `Err` from `miner.run()` can only occur after the mode's
    /// in-function teardown + bail, so reaching `enter_management_only`
    /// structurally implies hardware was already safely off.
    #[test]
    fn f1_management_only_only_reached_via_err_arm() {
        // Each real call must appear inside an `Err(e)` arm. The arm may be a
        // block now because it cancels the miner child token before parking on
        // the parent process token.
        for label in [
            "\"s19j-hybrid\"",
            "\"am3-bb\"",
            "\"serial\"",
            "\"tap\"",
            "\"stratum-proxy\"",
            // gap-swarm daemon-startup #6: the standard BraiinsOS-FPGA daemon arm.
            "\"daemon\"",
        ] {
            let call = format!("enter_management_only({label}, e, shutdown_token.clone()");
            let call_idx = MAIN_RS
                .find(&call)
                .unwrap_or_else(|| panic!("F1: management-only call missing for {label}"));
            let err_idx = MAIN_RS[..call_idx]
                .rfind("Err(e)")
                .unwrap_or_else(|| panic!("F1: call for {label} is not after an Err(e) arm"));
            let ok_idx = MAIN_RS[..call_idx].rfind("Ok(())").unwrap_or(0);
            assert!(
                err_idx > ok_idx,
                "F1: management-only call for {label} must live in the Err(e) \
                 arm for that mining mode, not the success path"
            );
        }
    }

    /// F1 platform-wide: all SIX enumerated mining arms
    /// (s19j-hybrid / am3-bb / serial / tap / stratum-proxy / daemon) route their
    /// failure through `enter_management_only`, not `Err(e) => { … Err(e) }`.
    /// The `daemon` arm (standard BraiinsOS-FPGA path, S9/am1 + am2-s17) was the
    /// lone exception until gap-swarm daemon-startup #6 — pin that it never
    /// regresses to a bare process-exit-on-error.
    #[test]
    fn f1_all_six_arms_use_management_only_on_err() {
        for label in [
            "\"s19j-hybrid\"",
            "\"am3-bb\"",
            "\"serial\"",
            "\"tap\"",
            "\"stratum-proxy\"",
            "\"daemon\"",
        ] {
            assert!(
                MAIN_RS.contains(&format!("enter_management_only({label}, e, shutdown_token")),
                "F1: mining arm {label} does not route miner.run() Err through \
                 enter_management_only — the coupled-lifetime regression class \
                 (`a lab unit` daemon exits → API dies) is not closed for this arm"
            );
        }
    }

    /// no-brick #6 (the other half of the standard-daemon-arm fix): the
    /// `Daemon::run()` wrapper runs the graceful hardware-safe-off teardown
    /// (`self.shutdown()`) on a mining-lifecycle error BEFORE propagating it, so
    /// the boards are de-energized / fans idled / watchdog disarmed by the time
    /// `main.rs` routes the `Err` into `enter_management_only` (whose contract is
    /// "hardware already off"). Pin (a) that `run()` delegates to the wrapped
    /// `run_lifecycle()`, and (b) that the `Err(e) if mining =>` arm contains the
    /// teardown call — while the api-only fall-through arm (`Err(e) => Err(e)`)
    /// does NOT (no hardware was energized there).
    #[test]
    fn nobrick6_daemon_run_tears_down_hardware_on_mining_error() {
        assert!(
            DAEMON_RS.contains("self.run_lifecycle().await"),
            "no-brick #6: Daemon::run() must delegate to run_lifecycle() so the \
             hardware-safe-off teardown wrapper is never bypassed"
        );
        let mining_arm = DAEMON_RS.find("Err(e) if mining =>").expect(
            "no-brick #6: the `Err(e) if mining =>` teardown arm is missing from Daemon::run()",
        );
        let fallthrough = DAEMON_RS[mining_arm..]
            .find("Err(e) => Err(e)")
            .map(|i| i + mining_arm)
            .expect("no-brick #6: the api-only `Err(e) => Err(e)` fall-through arm is missing");
        assert!(
            mining_arm < fallthrough,
            "no-brick #6: the mining teardown arm must precede the api-only fall-through"
        );
        let mining_arm_body = &DAEMON_RS[mining_arm..fallthrough];
        assert!(
            mining_arm_body.contains("self.shutdown().await"),
            "no-brick #6 SAFETY VIOLATION: the mining-lifecycle Err arm of \
             Daemon::run() must run the hardware-safe-off teardown \
             (self.shutdown()) before propagating — otherwise boards can stay \
             energized + watchdog armed on an error exit"
        );
        // The api-only fall-through must NOT tear down (nothing was energized).
        let fallthrough_tail = &DAEMON_RS[fallthrough..fallthrough + 64];
        assert!(
            !fallthrough_tail.contains("shutdown"),
            "no-brick #6: the api-only `Err(e) => Err(e)` arm must NOT run the \
             mining teardown (no hardware was energized in api-only mode)"
        );
    }

    /// no-brick #16: every energizing mining path must have a cut-hash peer in
    /// the crash-panic hook (panic=abort bypasses Drop). Pin all FOUR
    /// (s19j-hybrid am2 / serial-nopic am3-aml / am3-bb / stock-fpga S9) so a
    /// future platform path can't silently ship without its panic-hook teardown.
    #[test]
    fn panic_hook_chains_all_four_platform_teardowns() {
        let hook_start = MAIN_RS
            .find("fn install_cut_hash_on_crash_panic_hook()")
            .expect("install_cut_hash_on_crash_panic_hook missing");
        // Bound the search to the function body (up to `previous(info);`, the
        // last statement before the hook closure/fn close).
        let prev_rel = MAIN_RS[hook_start..]
            .find("previous(info);")
            .expect("panic hook must chain the previous hook via previous(info)");
        let hook_body = &MAIN_RS[hook_start..hook_start + prev_rel];
        for peer in [
            "s19j_hybrid_mining::panic_hook_best_effort_teardown",
            "serial_mining::nopic_panic_hook_best_effort_teardown",
            "am3_bb_mining::am3_bb_panic_hook_best_effort_teardown",
            "stock_mining::stock_fpga_panic_hook_best_effort_teardown",
        ] {
            assert!(
                hook_body.contains(peer),
                "no-brick #16: crash-panic hook is missing the cut-hash teardown peer `{peer}` — \
                 an energizing path without a panic-hook cut leaves boards hot on panic=abort"
            );
        }
    }

    /// WATCHDOG (2026-06-28): every mining entry path that bypasses
    /// `Daemon::run()` (`--s19j-hybrid`, `--serial-mining`, `--am3-bb-mining`,
    /// `--stock-fpga`) MUST
    /// arm the hardware `/dev/watchdog`. Native serial NoPic and AM3-BB use the
    /// stricter pre-energize `SafetyWatchdogOwner`; the remaining legacy modes
    /// use the shared `spawn_watchdog_kicker` helper after bring-up. The standard
    /// `Daemon::run` path uses its owned watchdog lifecycle. A CPU/runtime hang on
    /// an unarmed path leaves the hash boards energized & unsupervised. Source-order
    /// pin (mirrors `panic_hook_chains_all_four_platform_teardowns`): these are
    /// binary-crate / HAL-bound paths that cannot be driven in a host unit test.
    #[test]
    fn watchdog_armed_on_all_mining_entry_paths() {
        // The shared helper definition lives in daemon.rs.
        assert!(
            DAEMON_RS.contains("pub(crate) fn spawn_watchdog_kicker("),
            "WATCHDOG: the shared spawn_watchdog_kicker helper is missing from daemon.rs"
        );
        // The standard path must retain the watchdog under an independent task
        // owner with a thermal-liveness clock and explicit teardown/disarm intent.
        assert!(
            DAEMON_RS.contains("owned_watchdog_kicker(")
                && DAEMON_RS.contains("watch::channel(WatchdogIntent::Mining)")
                && DAEMON_RS.contains(".spawn(\"soc-watchdog-kicker\"")
                && DAEMON_RS.contains("intent_tx.send(WatchdogIntent::Disarm)"),
            "WATCHDOG: the standard path must use owned, explicit-intent watchdog supervision"
        );
        // Each Daemon::run-bypassing mining entry path must arm via the helper
        // with a path-local liveness counter (SAF-5). Passing None here is a
        // regression: a live-locked runtime would keep petting `/dev/watchdog`.
        for (src, name) in [
            (S19J_HYBRID_RS, "s19j-hybrid"),
            (STOCK_MINING_RS, "stock-fpga"),
        ] {
            assert!(
                src.contains("let watchdog_liveness = Arc::new(AtomicU64::new(0));")
                    && src.contains("Some(watchdog_liveness.clone())")
                    && src.contains("watchdog_liveness.fetch_add(1, Ordering::Relaxed)"),
                "WATCHDOG: the {name} mining entry path must arm /dev/watchdog via \
                 spawn_watchdog_kicker with a live runtime counter — a CPU/runtime hang there leaves boards energized & \
                 unsupervised"
            );
            assert!(
                !src.contains(
                    "crate::daemon::spawn_watchdog_kicker(&self.config.watchdog, self.shutdown.clone(), None)"
                ),
                "WATCHDOG: the {name} mining entry path regressed to unconditional watchdog kicks"
            );
        }
        assert!(
            SERIAL_MINING_RS.contains("SafetyWatchdogOwner::start_before_energizing")
                && SERIAL_MINING_RS.contains("nopic_watchdog_liveness.mark_progress()")
                && SERIAL_MINING_RS.contains("if nopic_watchdog.is_none()")
                && SERIAL_MINING_RS.contains("WatchdogDisarmPermit::from_evidence"),
            "WATCHDOG: native serial NoPic must use pre-energize, evidence-gated watchdog ownership while non-NoPic serial retains the legacy helper temporarily"
        );
        // AM3-BB admits the owner before API/hardware access, advances only
        // safety liveness, and can magic-close only from the complete evidence
        // set. Its enum-only stub must never counterfeit Mining liveness.
        assert!(
            AM3_BB_RS.contains("SafetyWatchdogOwner::start_before_energizing")
                && AM3_BB_RS.contains("admission.require_armed(\"am3-bb\")")
                && AM3_BB_RS.contains("HardwareMutationGateOwner::new_pending()")
                && AM3_BB_RS.contains("watchdog.enter_mining()")
                && AM3_BB_RS.contains("hardware_mutation_owner.open()")
                && AM3_BB_RS.contains("watchdog_liveness.mark_progress()")
                && AM3_BB_RS.contains("watchdog.begin_teardown(")
                && AM3_BB_RS.contains("WatchdogDisarmPermit::from_evidence_set")
                && AM3_BB_RS.contains("watchdog.disarm_and_join("),
            "WATCHDOG: AM3-BB must retain pre-energize ownership, safety liveness, and evidence-gated closeout"
        );
        assert!(
            !AM3_BB_RS.contains("spawn_watchdog_kicker") && !AM3_BB_RS.contains("Arc<AtomicU64>"),
            "WATCHDOG: AM3-BB regressed to the detached legacy kicker"
        );
        let admission = MAIN_RS
            .find("Am3BbSafetyAdmission::start(&config)")
            .expect("AM3-BB pre-energize admission missing from main");
        let api = MAIN_RS[admission..]
            .find("spawn_proxy_mode_api_with_hardware_mutation_gate(")
            .map(|offset| admission + offset)
            .expect("AM3-BB shared API gate spawn missing");
        let mining = MAIN_RS[api..]
            .find("am3_bb_mining::run_am3_bb_mining(")
            .map(|offset| api + offset)
            .expect("AM3-BB mining call missing");
        assert!(
            admission < api && api < mining,
            "WATCHDOG: AM3-BB watchdog admission must precede shared API admission and mining"
        );
    }

    /// F5: the s19j-hybrid / am3-bb / tap arms gate on
    /// `!config.mining_start_enabled()` and return via
    /// `enter_management_only_idle` BEFORE constructing/running the miner —
    /// so a fresh, unconfigured unit performs NO PIC preflight / cold boot
    /// / FPGA work dispatch at all ("does LESS, never MORE").
    #[test]
    fn f5_fresh_unit_gate_precedes_miner_run() {
        let run_start = MAIN_RS
            .find("async fn run_main() -> Result<()> {")
            .expect("run_main entry missing");
        let tests_start = MAIN_RS[run_start..]
            .find("#[cfg(test)]")
            .map(|offset| run_start + offset)
            .expect("runtime test module boundary missing");
        let runtime_main = &MAIN_RS[run_start..tests_start];

        // s19j-hybrid: the gate must precede `S19jHybridMiner::new`.
        let s19j_new = runtime_main
            .find("S19jHybridMiner::new(")
            .expect("s19j-hybrid miner construction missing");
        let s19j_gate = runtime_main[..s19j_new]
            .rfind("if !config.mining_start_enabled() {")
            .expect(
                "F5: s19j-hybrid arm has no mining_start_enabled() gate before miner construction",
            );
        assert!(
            s19j_gate < s19j_new,
            "F5 SAFETY: the mining_start_enabled() gate must precede \
             S19jHybridMiner::new — a fresh unit must NOT construct/run the \
             miner (no PIC preflight, no cold boot, no crash)"
        );
        // am3-bb: the gate must precede `run_am3_bb_mining(`.
        let am3_run = runtime_main
            .find("am3_bb_mining::run_am3_bb_mining(")
            .expect("am3-bb mining entry missing");
        let am3_gate = runtime_main[..am3_run]
            .rfind("if !config.mining_start_enabled() {")
            .expect("F5: am3-bb arm has no mining_start_enabled() gate before run_am3_bb_mining");
        assert!(
            am3_gate < am3_run,
            "F5 SAFETY: am3-bb fresh-unit gate must precede run_am3_bb_mining"
        );

        // tap: the gate must precede `S19jTapMiner::new`, because the tap
        // miner dispatches FPGA work even though bosminer owns PIC/PSU/serial.
        let tap_arm = runtime_main
            .find("} else if tap_mode {")
            .expect("tap-mode arm missing");
        let tap_new = runtime_main
            .find("S19jTapMiner::new(config, mining_shutdown.clone())")
            .expect("tap miner construction missing");
        let tap_gate = runtime_main[tap_arm..tap_new]
            .find("if !config.mining_start_enabled() {")
            .map(|idx| tap_arm + idx)
            .expect("F5: tap arm has no mining_start_enabled() gate before S19jTapMiner::new");
        let tap_idle_body = &runtime_main[tap_arm..tap_new];
        assert!(
            tap_gate < tap_new,
            "F5 SAFETY: tap fresh-unit gate must precede S19jTapMiner::new"
        );
        assert!(
            tap_idle_body.contains("spawn_proxy_mode_api("),
            "F5: tap idle branch must bring up API/dashboard before parking \
             management-only"
        );

        // All gated arms return through the idle helper, not a crash path.
        // `return enter_management_only_idle(` is the exact call shape used
        // by these arms (the fn definition uses `async fn`, prose/this test
        // do not use that prefix), so this count is exactly the call sites.
        assert!(
            runtime_main
                .matches("return enter_management_only_idle(")
                .count()
                >= 3,
            "F5: the s19j-hybrid, am3-bb, and tap fresh-unit gates must \
             `return enter_management_only_idle(...)` (park, not crash)"
        );
    }

    #[test]
    fn td003_runtime_board_target_gate_precedes_all_mining_modes() {
        let guard_start = MAIN_RS
            .find("let td003_board_target =")
            .expect("TD-003 board-target runtime marker read missing");
        let guard = MAIN_RS[guard_start..]
            .find("td003_management_only_board_target(&td003_board_target)")
            .map(|idx| guard_start + idx)
            .expect("TD-003 board-target runtime gate missing");
        let first_mining_arm = MAIN_RS
            .find("if am3_bb_mode {")
            .expect("first mining arm missing");
        assert!(
            guard < first_mining_arm,
            "TD-003 board-target gate must run before any mining arm can construct a miner"
        );

        let guard_body = &MAIN_RS[guard_start..first_mining_arm];
        assert!(
            guard_body.contains("config.mining_start_enabled()"),
            "TD-003 board-target gate should only intercept starts that would otherwise mine"
        );
        assert!(
            guard_body.contains("spawn_proxy_mode_api_with_hardware_mutation_gate(")
                && guard_body.contains("HardwareMutationGate::new_closed()"),
            "TD-003 board-target gate must keep a read-only management/API surface reachable"
        );
        assert!(
            guard_body.contains("\"td003-board-target\""),
            "TD-003 board-target gate must park through enter_management_only_idle"
        );
    }

    /// F1: `enter_management_only` parks on the shutdown token and returns
    /// `Ok(())` (it does NOT propagate the mining error as a process exit).
    /// This is what keeps the API alive — pin the no-`Err`-return shape.
    #[test]
    fn f1_management_only_parks_and_returns_ok() {
        let body_start = MAIN_RS
            .find("async fn enter_management_only(")
            .expect("enter_management_only definition missing");
        let body = &MAIN_RS[body_start..];
        let body_end = first_function_end_offset(body) + body_start;
        let body = &MAIN_RS[body_start..body_end];
        assert!(
            body.contains("park_management_only_until_shutdown("),
            "F1: enter_management_only must park the process on the shutdown \
             token (via park_management_only_until_shutdown, which awaits \
             shutdown_token.cancelled()) so the already-spawned API task stays \
             alive"
        );
        assert!(
            body.contains("Ok(())"),
            "F1: enter_management_only must return Ok(()) — returning Err \
             would exit the process and kill the API (the `a lab unit` defect)"
        );
        assert!(
            !body.contains("Err(e)") && !body.contains("return Err"),
            "F1: enter_management_only must NOT return the mining Err"
        );
    }

    /// F1-B: mining modes receive child cancellation tokens, while
    /// management-only parks on the parent process token. This prevents a
    /// miner-local failure path from cancelling the API parking lifetime.
    #[test]
    fn f1_mining_modes_use_child_tokens_not_process_token() {
        for marker in [
            "stratum_proxy::run(config, mining_shutdown.clone(), Some(stats))",
            "S19jTapMiner::new(config, mining_shutdown.clone())",
            "SerialMiner::new(config, mining_shutdown.clone())",
        ] {
            assert!(
                MAIN_RS.contains(marker),
                "F1-B: mining mode missing child-token call marker `{marker}`. \
                 Miner-local cancellation must not cancel the process token \
                 used by management-only parking."
            );
        }
        assert!(
            MAIN_RS
                .contains("S19jHybridMiner::new(config, mining_shutdown.clone(), route_admission)"),
            "F1-B: S19jHybridMiner must receive its mining child token and one-shot route proof"
        );
        let am3_call = MAIN_RS
            .find("am3_bb_mining::run_am3_bb_mining(")
            .expect("AM3-BB mining call missing");
        assert!(
            MAIN_RS[am3_call..].contains("mining_shutdown.clone(),"),
            "F1-B: AM3-BB must receive its mining child token"
        );

        assert!(
            MAIN_RS
                .matches("let mining_shutdown = shutdown_token.child_token();")
                .count()
                >= 5,
            "F1-B: expected mining modes to derive child tokens from the \
             process shutdown token before starting miner-owned tasks"
        );
        assert!(
            MAIN_RS.matches("mining_shutdown.cancel();").count() >= 5,
            "F1-B: each miner Err arm should cancel the child token to stop \
             mode-owned tasks before parking the API on the process token"
        );
        assert!(
            MAIN_RS.contains("enter_management_only(\"s19j-hybrid\", e, shutdown_token.clone()"),
            "F1-B: management-only must still park on the parent process \
             shutdown token, not the miner child token"
        );
    }

    /// HOME-SAFETY (2026-05-29, `a lab unit`): management-only mode must PERIODICALLY
    /// re-assert the am2 idle fan setpoint, not command it once and idle — the
    /// AM2 board's fan IP reverts the fans to full speed if nothing re-commands
    /// the PWM. Pin the periodic-refresh + SIGTERM-responsiveness shape of the
    /// shared park helper, and that BOTH management-only entries route through
    /// it (so the fix can't be silently removed from one path).
    #[test]
    fn management_only_periodically_reasserts_am2_idle_fan() {
        let start = MAIN_RS
            .find("async fn park_management_only_until_shutdown(")
            .expect("park_management_only_until_shutdown definition missing");
        let body = &MAIN_RS[start..];
        let body_end = first_function_end_offset(body);
        let body = &body[..body_end];

        // It must still park on the shutdown token (lifetime preserved) ...
        assert!(
            body.contains("shutdown_token.cancelled().await"),
            "park helper must await shutdown_token.cancelled() (park the process)"
        );
        // ... race the refresh tick against shutdown so SIGTERM is prompt ...
        assert!(
            body.contains("tokio::select!") && body.contains("refresh.tick()"),
            "park helper must select! between a periodic tick and shutdown so \
             the periodic fan refresh never delays SIGTERM"
        );
        // ... and the periodic body must RE-RUN the proven fan-park call.
        assert!(
            body.contains("force_am2_fans_to_quiet_idle"),
            "park helper must re-assert the am2 idle fan setpoint on each tick \
             (reuse the proven in-process FanController park, NOT --set-fan)"
        );
        // The refresh is gated on the am2/uio16 variant (Some) — None paths
        // (S9/am1 + am3) must keep the old single terminal await (no fan I/O).
        assert!(
            body.contains("let Some((fan_idle_pwm, fan_max_pwm)) = am2_quiet_idle else"),
            "park helper periodic refresh must be gated on am2_quiet_idle = \
             Some(..) (am2/uio16 only); None = byte-identical single await"
        );

        // Both management-only entry points must route through the shared helper.
        assert!(
            MAIN_RS
                .matches("park_management_only_until_shutdown(mode_label, shutdown_token, am2_quiet_idle)")
                .count()
                >= 2,
            "both enter_management_only_idle (F5) and enter_management_only (F1) \
             must park via park_management_only_until_shutdown so neither path \
             can silently let am2 fans revert to full speed"
        );
    }

    // -----------------------------------------------------------------------
    // R2 — `--set-fan` one-shot clamp + arg parse
    //
    // -----------------------------------------------------------------------

    use super::{
        am2_low_pwm_floor_present, clamp_set_fan_pwm, format_fan_raw_registers,
        parse_fan_sweep_list, run_set_fan_oneshot,
    };

    /// Wave-B `--safe-off`: the pure platform→power-cut mapping must route each
    /// platform to its AUDITED cut, and the am2 fingerprint must NOT fall
    /// through to the generic zynq arm (am2 uses PWR_CONTROL gpio907, not
    /// zynq::disable_psu_output). Runtime assertions, not a source-string parse.
    #[test]
    fn safe_off_cut_maps_each_platform_to_its_audited_cut() {
        use super::{safe_off_cut_for_platform, SafeOffCut};
        assert_eq!(
            safe_off_cut_for_platform("zynq-bm3-am2"),
            SafeOffCut::Am2PwrControl
        );
        // real .25 stamp variants (trailing newline + xil suffix) still route am2
        assert_eq!(
            safe_off_cut_for_platform("zynq-bm3-am2\n"),
            SafeOffCut::Am2PwrControl
        );
        assert_eq!(
            safe_off_cut_for_platform("amlogic-a113d"),
            SafeOffCut::AmlogicDisablePsu
        );
        assert_eq!(
            safe_off_cut_for_platform("am3-bb-s19jpro"),
            SafeOffCut::BeagleboneDisablePsu
        );
        assert_eq!(
            safe_off_cut_for_platform("cvitek-cv1835"),
            SafeOffCut::CvitekDisablePsu
        );
        // generic zynq (S9 / am1) only AFTER the am2 fingerprint check
        assert_eq!(
            safe_off_cut_for_platform("zynq-bm1-s9"),
            SafeOffCut::ZynqDisablePsu
        );
        assert_ne!(
            safe_off_cut_for_platform("zynq-bm3-am2"),
            SafeOffCut::ZynqDisablePsu
        );
        // unknown / empty → NOT cut (honest, never a false affordance)
        assert_eq!(safe_off_cut_for_platform(""), SafeOffCut::Unknown);
        assert_eq!(
            safe_off_cut_for_platform("future-board-x"),
            SafeOffCut::Unknown
        );
        // safe-off fan PWM is a quiet idle, never above the home safety cap
        assert!(super::SAFE_OFF_FAN_PWM <= dcentrald_hal::fan::PWM_SAFETY_MAX);
    }

    /// Without `--allow-loud`, PWM is clamped to PWM_SAFETY_MAX (30) — only
    /// ever driven DOWN relative to the home cap, never above it.
    #[test]
    fn set_fan_clamps_to_safety_max_by_default() {
        assert_eq!(clamp_set_fan_pwm(10, false), 10);
        assert_eq!(clamp_set_fan_pwm(30, false), 30);
        assert_eq!(
            clamp_set_fan_pwm(31, false),
            30,
            "31 must clamp DOWN to PWM_SAFETY_MAX (30) without --allow-loud"
        );
        assert_eq!(clamp_set_fan_pwm(100, false), 30);
        assert_eq!(clamp_set_fan_pwm(255, false), 30);
        assert_eq!(
            clamp_set_fan_pwm(30, false),
            dcentrald_hal::fan::PWM_SAFETY_MAX
        );
    }

    /// `--allow-loud` is the ONLY sanctioned bypass (rust-firmware.md
    /// "explicit user override to a higher mode") — but still clamped to the
    /// IP ceiling PWM_MAX (100) since the fan_ctrl IP panics on > 100.
    #[test]
    fn set_fan_allow_loud_bypasses_safety_but_not_ip_max() {
        assert_eq!(clamp_set_fan_pwm(50, true), 50);
        assert_eq!(clamp_set_fan_pwm(80, true), 80);
        assert_eq!(clamp_set_fan_pwm(100, true), 100);
        assert_eq!(
            clamp_set_fan_pwm(255, true),
            dcentrald_hal::fan::PWM_MAX,
            "even --allow-loud must clamp to PWM_MAX (100) — the IP panics on >100"
        );
        // Low values are never raised by the carve-out.
        assert_eq!(clamp_set_fan_pwm(5, true), 5);
    }

    /// A missing PWM argument is a defined exit-code-2 failure.
    #[test]
    fn set_fan_missing_arg_exits_2() {
        assert_eq!(run_set_fan_oneshot(None, false), 2);
    }

    /// A non-numeric PWM argument is a defined exit-code-2 failure (parse
    /// rejected) — it must NOT touch any hardware.
    #[test]
    fn set_fan_non_numeric_arg_exits_2() {
        assert_eq!(run_set_fan_oneshot(Some("loud"), false), 2);
        assert_eq!(run_set_fan_oneshot(Some(""), false), 2);
        assert_eq!(run_set_fan_oneshot(Some("-5"), false), 2);
        assert_eq!(run_set_fan_oneshot(Some("3.5"), false), 2);
    }

    #[test]
    fn fan_sweep_parser_accepts_comma_list() {
        assert_eq!(
            parse_fan_sweep_list(Some("0, 5,10,30")).unwrap(),
            vec![0, 5, 10, 30]
        );
        assert!(parse_fan_sweep_list(None).is_err());
        assert!(parse_fan_sweep_list(Some("10,,30")).is_err());
        assert!(parse_fan_sweep_list(Some("10,loud")).is_err());
    }

    #[test]
    fn fan_raw_register_format_is_parseable_and_keeps_mirror_offsets() {
        let regs = [
            dcentrald_hal::fan::FanRawRegister {
                offset: 0x18,
                value: 0x0000_0010,
            },
            dcentrald_hal::fan::FanRawRegister {
                offset: 0x1C,
                value: 0x0000_0014,
            },
            dcentrald_hal::fan::FanRawRegister {
                offset: 0x20,
                value: 0x0000_0020,
            },
        ];
        assert_eq!(
            format_fan_raw_registers(&regs),
            "0x18:0x00000010,0x1C:0x00000014,0x20:0x00000020"
        );
    }

    #[test]
    fn get_fan_and_fan_sweep_enable_raw_dump_but_set_fan_does_not() {
        assert!(
            MAIN_RS.contains("print_fan_snapshot(\"--get-fan\", discovery, &fan, None, true)"),
            "--get-fan must print the bounded raw fan register dump"
        );
        assert!(
            MAIN_RS
                .contains("print_fan_snapshot(\"--fan-sweep\", discovery, &fan, Some(pwm), true)"),
            "--fan-sweep must print the bounded raw fan register dump"
        );
        assert!(
            MAIN_RS
                .contains("print_fan_snapshot(\"--set-fan\", discovery, &fan, Some(pwm), false)"),
            "--set-fan output should not grow the diagnostics-only raw dump"
        );
    }

    #[test]
    fn am2_low_pwm_floor_predicate_requires_am2_low_pwm_and_high_rpm() {
        assert!(am2_low_pwm_floor_present(
            dcentrald_hal::fan::FanVariant::Am2Uio16,
            10,
            2460
        ));
        assert!(!am2_low_pwm_floor_present(
            dcentrald_hal::fan::FanVariant::Am2Uio16,
            30,
            2460
        ));
        assert!(!am2_low_pwm_floor_present(
            dcentrald_hal::fan::FanVariant::Am1S9,
            10,
            2460
        ));
        assert!(!am2_low_pwm_floor_present(
            dcentrald_hal::fan::FanVariant::Am2Uio16,
            10,
            1260
        ));
    }

    /// R2 source invariant: the `--set-fan` early-exit is positioned BEFORE
    /// config load, logging init, and mode routing. Pin it by lexical order
    /// so a future edit can't accidentally move hardware/runtime work ahead
    /// of the fan-only one-shot (which must stay a tiny sync open→write→exit).
    #[test]
    fn set_fan_oneshot_precedes_config_and_mode_routing() {
        let set_fan = MAIN_RS
            .find("args.iter().position(|a| a == \"--set-fan\")")
            .expect("R2: --set-fan early-exit branch missing from run_main");
        let config_load = MAIN_RS
            .find("DcentraldConfig::load(&config_path)")
            .expect("config load site missing");
        let logging = MAIN_RS
            .find("init_logging(&config.general.log_level)")
            .expect("logging init site missing");
        assert!(
            set_fan < config_load,
            "R2: --set-fan one-shot must run BEFORE config load"
        );
        assert!(
            set_fan < logging,
            "R2: --set-fan one-shot must run BEFORE logging init"
        );
        // It must early-exit the process, never fall through into routing.
        let branch = &MAIN_RS[set_fan..config_load];
        assert!(
            branch.contains("std::process::exit(run_set_fan_oneshot("),
            "R2: the --set-fan branch must std::process::exit (early-exit, \
             never fall through to mode routing / hardware bring-up)"
        );
    }

    /// `--hold-fan` source invariant: the persistent fan-custodian early-exit
    /// is positioned BEFORE config load + logging init (same as `--set-fan`),
    /// must dispatch via `run_hold_fan` and `std::process::exit`, and must be
    /// awaited (it is async — it stays resident on the tokio runtime).
    #[test]
    fn hold_fan_oneshot_precedes_config_and_mode_routing() {
        let hold_fan = MAIN_RS
            .find("args.iter().position(|a| a == \"--hold-fan\")")
            .expect("--hold-fan early-exit branch missing from run_main");
        let config_load = MAIN_RS
            .find("DcentraldConfig::load(&config_path)")
            .expect("config load site missing");
        assert!(
            hold_fan < config_load,
            "--hold-fan custodian must run BEFORE config load / mode routing"
        );
        let branch = &MAIN_RS[hold_fan..config_load];
        assert!(
            branch.contains("std::process::exit(run_hold_fan("),
            "the --hold-fan branch must std::process::exit(run_hold_fan(..)) \
             (early-exit, never fall through to mode routing)"
        );
        assert!(
            branch.contains("run_hold_fan(pwm_arg, allow_loud).await"),
            "--hold-fan must be awaited (it is a resident async custodian on the \
             tokio runtime, not a sync one-shot)"
        );
    }

    /// `--hold-fan` honors the SAME clamp as `--set-fan`: PWM only ever driven
    /// DOWN (≤ PWM_SAFETY_MAX) unless explicit `--allow-loud`, and never above
    /// the IP ceiling PWM_MAX even with `--allow-loud`. (Both call
    /// `clamp_set_fan_pwm`, pinned here so a future divergence is caught.)
    #[test]
    fn hold_fan_uses_set_fan_clamp() {
        use super::clamp_set_fan_pwm;
        let safety = dcentrald_hal::fan::PWM_SAFETY_MAX;
        let max = dcentrald_hal::fan::PWM_MAX;
        // Without --allow-loud: clamped to the home cap.
        assert_eq!(clamp_set_fan_pwm(100, false), safety);
        assert_eq!(clamp_set_fan_pwm(10, false), 10.min(safety));
        // With --allow-loud: permitted above the cap, still ≤ IP ceiling.
        assert_eq!(clamp_set_fan_pwm(200, true), max);
        // Source pin: run_hold_fan routes through the same clamp helper.
        let body_start = MAIN_RS
            .find("async fn run_hold_fan(")
            .expect("run_hold_fan definition missing");
        let body = &MAIN_RS[body_start..];
        let end = first_function_end_offset(body);
        assert!(
            body[..end].contains("clamp_set_fan_pwm(requested, allow_loud)"),
            "--hold-fan must clamp via the shared clamp_set_fan_pwm (home cap \
             unless --allow-loud — never raise PWM above the cap)"
        );
    }

    /// `--hold-fan` is SIGTERM-responsive: its hold loop must race a SIGTERM
    /// (and SIGINT) future against the 5 s re-assert interval so the
    /// init-script `stop` path (kill by pidfile) is acted on immediately.
    #[test]
    fn hold_fan_is_sigterm_responsive() {
        let body_start = MAIN_RS
            .find("async fn run_hold_fan(")
            .expect("run_hold_fan definition missing");
        let body = &MAIN_RS[body_start..];
        assert!(
            body.contains("SignalKind::terminate()"),
            "--hold-fan must register a SIGTERM handler"
        );
        assert!(
            body.contains("signal::ctrl_c()"),
            "--hold-fan must also respond to SIGINT"
        );
        assert!(
            body.contains("Duration::from_secs(5)"),
            "--hold-fan must re-assert on a ~5 s interval (mmap-hold alone drifts)"
        );
    }

    /// R2 source invariant: the one-shot body touches the fan register ONLY
    /// — no PIC / PSU / I2C / voltage / ASIC / tokio-HW-runtime symbols.
    #[test]
    fn set_fan_oneshot_is_fan_register_only() {
        let start = MAIN_RS
            .find("fn run_set_fan_oneshot(")
            .expect("run_set_fan_oneshot definition missing");
        let body = &MAIN_RS[start..];
        let end = first_function_end_offset(body);
        let body = &body[..end];
        for forbidden in [
            "Pic0x89",
            "Apw121215",
            "PsuBypassGate",
            "I2cBus",
            "I2cService",
            "i2c-0",
            "set_voltage",
            "cold_boot",
            "tokio::runtime",
            "Runtime::new",
        ] {
            assert!(
                !body.contains(forbidden),
                "R2 SAFETY: the --set-fan one-shot must be fan-register-ONLY \
                 — found forbidden symbol `{forbidden}` in run_set_fan_oneshot"
            );
        }
        // It DOES use the proven uio-mmap FanController.
        assert!(
            body.contains("FanController::open_with_variant"),
            "R2: the one-shot must use the proven uio-mmap FanController"
        );
    }

    // ---------------------------------------------------------------
    // CLI info one-shots (`--help`/`-h`, `--version`/`-V`).
    //
    // Closes the defect where `dcentrald --help` had no handler and fell
    // through to the auto-`s19j-hybrid` daemon path (silently starting
    // the miner on a configured unit). Pure behavioral test + structural
    // ordering pin (same style as the R2 `--set-fan` pins above).
    // ---------------------------------------------------------------

    #[test]
    fn cli_info_classifier_help_version_precedence() {
        use super::{wants_cli_info, CliInfoRequest};
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // Bare / mode / fan-one-shot flags → NOT an info request (the
        // daemon + fan one-shot paths must be entirely unchanged).
        assert_eq!(wants_cli_info(&s(&["dcentrald"])), None);
        assert_eq!(wants_cli_info(&s(&["dcentrald", "--s19j-hybrid"])), None);
        assert_eq!(wants_cli_info(&s(&["dcentrald", "--get-fan"])), None);
        assert_eq!(wants_cli_info(&s(&["dcentrald", "--set-fan", "10"])), None);

        // Help / version recognized (long + short).
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "--help"])),
            Some(CliInfoRequest::Help)
        );
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "-h"])),
            Some(CliInfoRequest::Help)
        );
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "--version"])),
            Some(CliInfoRequest::Version)
        );
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "-V"])),
            Some(CliInfoRequest::Version)
        );

        // Help wins over version, AND over any daemon/one-shot flag —
        // an operator asking for help must NEVER start the miner.
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "--version", "--help"])),
            Some(CliInfoRequest::Help)
        );
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "--s19j-hybrid", "--help"])),
            Some(CliInfoRequest::Help)
        );
        assert_eq!(
            wants_cli_info(&s(&["dcentrald", "--set-fan", "99", "--allow-loud", "-h"])),
            Some(CliInfoRequest::Help)
        );
    }

    /// Structural pin: the `wants_cli_info` match must run BEFORE the
    /// `--set-fan` one-shot, config load, logging, AND mode routing, and
    /// must `std::process::exit(0)` — so `dcentrald --help` can never
    /// fall through to the daemon. (Mirrors the R2 `--set-fan` pins.)
    #[test]
    fn cli_info_oneshot_precedes_everything_in_run_main() {
        let info = MAIN_RS
            .find("match wants_cli_info(&args)")
            .expect("CLI info one-shot branch missing from run_main");
        let set_fan = MAIN_RS
            .find("args.iter().position(|a| a == \"--set-fan\")")
            .expect("--set-fan branch missing");
        let config_load = MAIN_RS
            .find("DcentraldConfig::load(&config_path)")
            .expect("config load site missing");
        let hybrid_route = MAIN_RS
            .find("let s19j_hybrid_cli = args.iter().any(|a| a == \"--s19j-hybrid\")")
            .expect("s19j-hybrid routing site missing");
        assert!(
            info < set_fan,
            "CLI info must be checked BEFORE the --set-fan one-shot"
        );
        assert!(info < config_load, "CLI info must run BEFORE config load");
        assert!(
            info < hybrid_route,
            "CLI info must run BEFORE any daemon/mode routing"
        );
        // It must early-exit, never fall through into the daemon path.
        let branch = &MAIN_RS[info..set_fan];
        assert!(
            branch.contains("std::process::exit(0)"),
            "the CLI info branch must std::process::exit(0) — `dcentrald \
             --help` must NEVER fall through to the mining daemon"
        );
    }

    #[test]
    fn unrecognized_cli_flags_behavioral() {
        use super::unrecognized_cli_flags;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let empty: Vec<String> = vec![];

        // Bare / known boolean / mode combos → nothing unrecognized.
        assert_eq!(unrecognized_cli_flags(&s(&["dcentrald"])), empty);
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--s19j-hybrid"])),
            empty
        );
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--get-fan"])),
            empty
        );
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--serial-mining", "--allow-loud"])),
            empty
        );

        // Value-bearing flags: the VALUE token must NOT be mis-flagged,
        // even when it could look flag-ish; `--get-fan` is boolean so it
        // does NOT swallow a following token.
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--set-fan", "10"])),
            empty
        );
        assert_eq!(
            unrecognized_cli_flags(&s(&[
                "dcentrald",
                "--fan-sweep",
                "0,5,10,15",
                "--dwell-ms",
                "8000"
            ])),
            empty
        );
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--config", "/data/dcentrald.toml"])),
            empty
        );

        // The dangerous typo case: an unrecognized `--flag` IS surfaced.
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--gte-fan"])),
            s(&["--gte-fan"])
        );
        assert_eq!(
            unrecognized_cli_flags(&s(&["dcentrald", "--s19j-hybrid", "--totally-unknown"])),
            s(&["--totally-unknown"])
        );
        // A bare `-` (e.g. stdin sentinel) is NOT treated as a flag typo.
        assert_eq!(unrecognized_cli_flags(&s(&["dcentrald", "-"])), empty);
    }

    /// Drift guard: every `== "--…"` / `== "-x"` flag literal the parser
    /// matches in `run_main` MUST appear in the known-flag consts, so the
    /// warning allowlist can never silently diverge from the real parser
    /// (divergence ⇒ either spurious warnings on a real flag or a missed
    /// dangerous typo). Pairs with the behavioral test above.
    #[test]
    fn known_cli_flag_consts_cover_every_parser_literal() {
        use super::{KNOWN_CLI_BOOL_FLAGS, KNOWN_CLI_VALUE_FLAGS};
        // Scan only NON-test source (the test module legitimately
        // contains `== "--flag"` literals inside structural-pin
        // .find() strings, which would be false matches). Every
        // `== "-…"` flag comparison in real code must be in the
        // allowlist.
        let test_mod = MAIN_RS
            .find("\n#[cfg(test)]\nmod tests")
            .unwrap_or(MAIN_RS.len());
        let body = &MAIN_RS[..test_mod];
        // Scan per line, stripping any `//` line-comment first. Doc-comments in
        // non-test code legitimately contain illustrative literals like
        // `a == "--x"` (the unrecognized-flag detector's own typo example at the
        // `unrecognized_cli_flags` doc), which are prose, not real parser
        // comparisons — without this strip the scanner false-matches them.
        for raw_line in body.lines() {
            let line = match raw_line.find("//") {
                Some(c) => &raw_line[..c],
                None => raw_line,
            };
            let mut rest = line;
            while let Some(p) = rest.find("== \"-") {
                let after = &rest[p + 4..]; // past `== "`
                if let Some(end) = after.find('"') {
                    let lit = &after[..end];
                    if lit.starts_with('-') {
                        let known = KNOWN_CLI_BOOL_FLAGS.contains(&lit)
                            || KNOWN_CLI_VALUE_FLAGS.contains(&lit);
                        assert!(
                            known,
                            "parser matches flag `{lit}` but it is missing \
                             from KNOWN_CLI_BOOL_FLAGS/KNOWN_CLI_VALUE_FLAGS — \
                             the unknown-flag warning allowlist has drifted \
                             from the real parser"
                        );
                    }
                    rest = &after[end..];
                } else {
                    break;
                }
            }
        }
    }

    /// Structural pin: the unknown-flag handler MUST be a strict reject
    /// (Wave C, 2026-05-19) — `process::exit(2)` AFTER dumping
    /// `cli_help_text()`. A future regression that removes the strict
    /// reject (or reverts to a non-blocking warning) would silently
    /// daemonize on typo'd flags again (the 2026-05-19 `a lab unit` over-spawn
    /// pattern); this catches it.
    ///
    /// Inverted from the prior `unrecognized_flag_warning_is_non_blocking`
    /// pin in Wave C. Wave B closed G-T8-1 (the `--passthrough` corpus
    /// hazard); the 16-flag canonical allowlist is now exhaustive across
    /// every production launcher site, so strict-reject is safe to ship.
    #[test]
    fn strict_reject_unknown_flags_exits_2_with_usage() {
        let warn = MAIN_RS
            .find("let unknown_flags = unrecognized_cli_flags(&args);")
            .expect("unknown-flag detector site missing from run_main");
        let set_fan = MAIN_RS
            .find("args.iter().position(|a| a == \"--set-fan\")")
            .expect("--set-fan branch missing");
        assert!(
            warn < set_fan,
            "the unknown-flag check must run BEFORE the --set-fan one-shot \
             and daemon routing (so it can reject before the daemon starts)"
        );
        let block = &MAIN_RS[warn..set_fan];
        assert!(
            block.contains("process::exit(2)"),
            "REGRESSION: Wave C strict-reject was REMOVED — the unknown-flag \
             handler MUST `std::process::exit(2)` to prevent silent \
             daemonization on typo'd flags (the 2026-05-19 `a lab unit` over-spawn \
             pattern)."
        );
        assert!(
            block.contains("cli_help_text"),
            "REGRESSION: Wave C strict-reject MUST dump cli_help_text() \
             before exiting so operators see the full flag list inline \
             (don't make them re-run with --help to find the right flag). \
"
        );
    }
}
