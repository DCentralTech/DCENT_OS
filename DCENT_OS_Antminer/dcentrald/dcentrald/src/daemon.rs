//! Daemon lifecycle orchestration.
//!
//! The Daemon struct owns all subsystems and manages the startup, run, and
//! shutdown sequence. It ties together the HAL, ASIC drivers, Stratum client,
//! thermal controller, diagnostic service, and API server via Tokio channels.
//!
//! Lifecycle:
//!   1. init()   - Hardware detection, PIC init, FPGA chain setup, chip enumeration
//!   2. run()    - Start mining pipeline, thermal loop, API server, watchdog
//!   3. shutdown() - Graceful stop: disable voltages, cool down, close watchdog

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use dcentrald_asic::chain::{
    chain_meets_min_fraction, driver_for_chain_with_policy, Chain, ChainDriverDecision,
    DivergentChipPolicy,
};
use dcentrald_asic::drivers::{
    ChipDriverAdmission, ChipDriverExecutionPolicy, ChipRegistry, MinerProfile, PicType,
};
use dcentrald_asic::dspic::DspicService;
use dcentrald_asic::pic::{Pic16EndpointSession, PicController, PicFirmware, PicServiceController};
use dcentrald_hal::fan::FanController;
use dcentrald_hal::fpga_chain::FpgaChain;
use dcentrald_hal::gpio::GpioController;
use dcentrald_hal::led::{LedCommand, LedEngine, LedEngineConfig, LedPattern};
use dcentrald_hal::platform::FanAccess;
use dcentrald_hal::watchdog::Watchdog;
use dcentrald_hal::xadc::Xadc;
use dcentrald_thermal::controller::{ThermalAction, ThermalController};
use dcentrald_thermal::profiles::ThermalProfile;

use crate::asic_identity_publication::{
    DispatcherCompositionAuthority, EnumeratedMiningChainReceipt, ExpectedMiningChain,
};
use crate::config::DcentraldConfig;
use crate::history::{self, HistoryBuffer};
use crate::model;
use crate::runtime::efficiency::{
    now_unix_ms, psu_efficiency_for_model_name, ShareEfficiencyTracker,
};
use crate::runtime::hardware_info::{
    collect_hardware_info, detect_control_board, read_hashboard_eeprom_fingerprints,
    read_hashboard_eeprom_preamble_for_slot, resolve_pic_type_from_eeprom,
};
use crate::runtime::notifications::{
    spawn_notification_stack, AlertEvent, RuntimeNotificationConfig, RuntimeWebhookConfig,
    NOTIFICATION_RELOAD_INTERVAL,
};
use crate::runtime::task_guard::RuntimeTaskGuard;
use crate::runtime::thread_guard::{
    join_thread_bounded, sleep_until_cancelled, RuntimeThreadGuard, ThreadStopOutcome,
};
use crate::voltage_mailbox::{voltage_command_mailbox, VoltageCommandSender, VoltageTrySendError};
use crate::work_dispatcher::{VoltageCommand, VoltageCommandReply};

const FAN_PWM_MAX: u8 = dcentrald_hal::fan::PWM_MAX;
const FAN_PWM_QUIET_BOOT: u8 = dcentrald_hal::fan::PWM_QUIET_BOOT;
const FAN_PWM_SAFETY_MAX: u8 = dcentrald_hal::fan::PWM_SAFETY_MAX;
const COOLING_SPINUP_DWELL: Duration = Duration::from_secs(3);
const COOLING_SPINUP_SAMPLES: usize = 3;
const COOLING_SPINUP_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const PSU_WATCHDOG_THREAD_STOP_TIMEOUT: Duration = Duration::from_secs(3);
const MINING_TASK_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const WATCHDOG_TASK_STOP_TIMEOUT: Duration = Duration::from_secs(2);
/// Covers the standard path's explicit 26.5 s worst-case bounded waits while
/// still turning a wedged shutdown into a hardware reset. Unbounded transport
/// calls remain protected because feeding stops at this absolute deadline.
const WATCHDOG_TEARDOWN_GRACE: Duration = Duration::from_secs(30);
/// Persisted PH-3 state has four scalar/optional fields and is normally under
/// 128 bytes. A 1 KiB ceiling leaves ample schema-evolution room while refusing
/// accidental/unbounded diagnostic payloads on the small `/data` filesystem.
const RECOVERY_LADDER_STATE_MAX_BYTES: usize = 1024;

fn pic16_service_for_endpoint(
    sessions: &[Pic16EndpointSession],
    address: u8,
    firmware: Option<PicFirmware>,
) -> Result<PicServiceController> {
    let session = sessions
        .iter()
        .find(|session| session.address() == address)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no discovery-bound PIC16 endpoint session for I2C address 0x{address:02X}; refusing raw-address fallback"
            )
        })?;
    Ok(match firmware {
        Some(firmware) => session.service_with_firmware(firmware),
        None => session.service(),
    })
}

/// Initialization-time ownership of PIC endpoints that have successfully
/// enabled their rails and therefore require watchdog heartbeats.
///
/// A poisoned mutex must not abort the daemon: the protected `Vec<u8>` remains
/// structurally valid after unwinding, and losing its addresses would silently
/// stop heartbeats to energized boards. Recover the retained membership with a
/// loud error instead. Callers still decide whether an I2C operation succeeded;
/// this container never grants hardware authority by itself.
#[derive(Clone)]
struct InitializedPicAddrs(Arc<std::sync::Mutex<Vec<u8>>>);

impl InitializedPicAddrs {
    fn new(addrs: Vec<u8>) -> Self {
        let mut unique = Vec::with_capacity(addrs.len());
        for addr in addrs {
            if !unique.contains(&addr) {
                unique.push(addr);
            }
        }
        Self(Arc::new(std::sync::Mutex::new(unique)))
    }

    fn lock_recover(&self, operation: &'static str) -> std::sync::MutexGuard<'_, Vec<u8>> {
        match self.0.lock() {
            Ok(addrs) => addrs,
            Err(poisoned) => {
                error!(
                    operation,
                    retained_count = poisoned.get_ref().len(),
                    "Initialized PIC ownership mutex was poisoned; recovering the structurally valid retained address list"
                );
                let addrs = poisoned.into_inner();
                // The recovered guard proves exclusive access while the
                // structurally valid Vec is retained. Clear the poison now so
                // the 500ms heartbeat reader records this incident once rather
                // than flooding logs forever.
                self.0.clear_poison();
                addrs
            }
        }
    }

    fn snapshot(&self, operation: &'static str) -> Vec<u8> {
        self.lock_recover(operation).clone()
    }

    fn record_success(&self, addr: u8, operation: &'static str) {
        let mut addrs = self.lock_recover(operation);
        if !addrs.contains(&addr) {
            addrs.push(addr);
        }
    }
}

#[cfg(test)]
mod initialized_pic_addrs_tests {
    use super::InitializedPicAddrs;

    #[test]
    fn membership_is_deduplicated_at_construction_and_recording() {
        let addrs = InitializedPicAddrs::new(vec![0x55, 0x55, 0x56]);
        addrs.record_success(0x56, "duplicate test");
        addrs.record_success(0x57, "new member test");

        assert_eq!(
            addrs.snapshot("membership assertion"),
            vec![0x55, 0x56, 0x57]
        );
    }

    #[cfg(feature = "sim-hal")]
    #[tokio::test]
    async fn init_retry_revokes_old_service_and_refuses_unproven_replacement() {
        use super::{Daemon, PicFirmware};
        use crate::config::DcentraldConfig;
        use crate::daemon_lifecycle::PlatformIdentitySnapshot;
        use dcentrald_hal::platform::sim::{SimModel, SimPlatform};
        use tokio_util::sync::CancellationToken;

        let config: DcentraldConfig = toml::from_str("").unwrap();
        let identity = PlatformIdentitySnapshot {
            declared_board_target: Some("am3-s19xp".into()),
            board_desc: None,
            declared_platform_marker: None,
            declared_subtype: None,
            declared_psu_hardware_variant: None,
            observed_control_board: "unknown".into(),
        };
        let mut daemon = Daemon::new(
            config,
            "simulated-retry.toml".into(),
            identity.clone(),
            CancellationToken::new(),
        );
        let platform = SimPlatform::new(SimModel::S9);
        let old_service = platform.open_i2c_service(0).unwrap();
        let old_endpoint = platform.pic16_endpoint(0, 0x55).unwrap();
        daemon.i2c_service = Some(old_service.clone());
        daemon.initialized_pic_addrs_final = vec![0x55];
        daemon.pic_firmware = PicFirmware::BraiinsOs;
        let error = daemon.init(&identity).await.unwrap_err();

        assert!(error
            .to_string()
            .contains("in-process replacement is refused"));
        assert!(old_service.terminal_safe_off_is_latched());
        let _safe_off_trace = platform.drain_i2c_trace().unwrap();
        assert!(old_service.pic16_heartbeat(&old_endpoint).is_err());
        assert!(platform.drain_i2c_trace().unwrap().is_empty());
        assert!(daemon.initialized_pic_addrs_final.is_empty());
        assert_eq!(daemon.pic_firmware, PicFirmware::Unknown);
        assert!(daemon.asic_enumeration_receipts.is_empty());
    }
}

#[derive(Debug)]
enum RecoveryStatePersistError<E> {
    Serialize(serde_json::Error),
    Write(E),
}

fn persist_recovery_ladder_state_with_writer<E, F>(
    path: &std::path::Path,
    state: &dcentrald_api_types::hashrate_recovery::PersistedLadderState,
    writer: F,
) -> std::result::Result<
    dcentrald_common::atomic_file::AtomicWriteOutcome,
    RecoveryStatePersistError<E>,
>
where
    F: FnOnce(
        &std::path::Path,
        &[u8],
        dcentrald_common::atomic_file::AtomicWriteOptions,
    ) -> std::result::Result<dcentrald_common::atomic_file::AtomicWriteOutcome, E>,
{
    // Preserve the prior serde_json::to_string byte contract exactly: compact
    // JSON, UTF-8, and no trailing newline.
    let json = serde_json::to_string(state).map_err(RecoveryStatePersistError::Serialize)?;
    writer(
        path,
        json.as_bytes(),
        dcentrald_common::atomic_file::AtomicWriteOptions::state_file(
            RECOVERY_LADDER_STATE_MAX_BYTES,
        ),
    )
    .map_err(RecoveryStatePersistError::Write)
}

fn persist_recovery_ladder_state(
    path: &std::path::Path,
    state: &dcentrald_api_types::hashrate_recovery::PersistedLadderState,
) -> std::result::Result<
    dcentrald_common::atomic_file::AtomicWriteOutcome,
    RecoveryStatePersistError<dcentrald_common::atomic_file::AtomicWriteError>,
> {
    persist_recovery_ladder_state_with_writer(path, state, |path, bytes, options| {
        dcentrald_common::atomic_file::atomic_write(path, bytes, options)
    })
}

fn log_recovery_state_persist_failure(
    action: &'static str,
    error: &RecoveryStatePersistError<dcentrald_common::atomic_file::AtomicWriteError>,
) {
    match error {
        RecoveryStatePersistError::Serialize(error) => tracing::error!(
            action,
            error = %error,
            persistence_stage = "serialize",
            target_published = false,
            publication_durability_uncertain = false,
            "PH-3 auto-recovery state persistence failed"
        ),
        RecoveryStatePersistError::Write(error) => tracing::error!(
            action,
            error = %error,
            persistence_stage = %error.stage(),
            target_published = error.target_published(),
            publication_durability_uncertain = error.target_published(),
            cleanup_error = ?error.cleanup_error(),
            "PH-3 auto-recovery state persistence failed"
        ),
    }
}

#[cfg(test)]
mod recovery_state_persistence_tests {
    use super::{
        persist_recovery_ladder_state_with_writer, RecoveryStatePersistError,
        RECOVERY_LADDER_STATE_MAX_BYTES,
    };
    use dcentrald_api_types::hashrate_recovery::PersistedLadderState;

    const DAEMON_SOURCE: &str = include_str!("daemon.rs");

    fn state() -> PersistedLadderState {
        PersistedLadderState {
            episode_active: true,
            attempts: 2,
            last_action_at_s: Some(123),
            gave_up_at_s: None,
        }
    }

    #[test]
    fn injected_writer_receives_exact_bounded_legacy_json_bytes() {
        let expected =
            br#"{"episode_active":true,"attempts":2,"last_action_at_s":123,"gave_up_at_s":null}"#;
        let result = persist_recovery_ladder_state_with_writer(
            std::path::Path::new("recovery.json"),
            &state(),
            |path, bytes, options| {
                assert_eq!(path, std::path::Path::new("recovery.json"));
                assert_eq!(bytes, expected);
                assert_eq!(options.max_bytes(), RECOVERY_LADDER_STATE_MAX_BYTES);
                Err("injected-write-failure")
            },
        );
        assert!(matches!(
            result,
            Err(RecoveryStatePersistError::Write("injected-write-failure"))
        ));
        assert!(expected.len() < RECOVERY_LADDER_STATE_MAX_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn behavior_publishes_complete_exact_json_without_legacy_temp_file() {
        let unique = format!(
            "dcentrald-recovery-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&directory).unwrap();
        let target = directory.join("ladder.json");
        std::fs::write(&target, b"old").unwrap();

        let outcome = super::persist_recovery_ladder_state(&target, &state()).unwrap();
        assert!(outcome.replaced_existing);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            br#"{"episode_active":true,"attempts":2,"last_action_at_s":123,"gave_up_at_s":null}"#
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn source_centralizes_both_ph3_writes_and_keeps_restart_fail_closed() {
        let start_marker = ["// PH-3: drive the auto-recovery", " ladder"].concat();
        let start = DAEMON_SOURCE
            .find(&start_marker)
            .expect("PH-3 recovery block");
        let end = DAEMON_SOURCE[start..]
            .find("// Wait for shutdown signal")
            .map(|offset| start + offset)
            .expect("PH-3 recovery block end");
        let block = &DAEMON_SOURCE[start..end];

        assert_eq!(block.matches("persist_recovery_ladder_state(").count(), 2);
        let legacy_temp = ["with_extension", "(\"tmp\")"].concat();
        assert!(!block.contains(&legacy_temp));
        assert!(!block.contains("std::fs::rename"));

        let schedule_arm = block.find("LadderOutcome::ScheduleRestart").unwrap();
        let persisted = block[schedule_arm..]
            .find("persist_recovery_ladder_state(")
            .map(|offset| schedule_arm + offset)
            .unwrap();
        let restart = block[schedule_arm..]
            .find("schedule_daemon_restart(")
            .map(|offset| schedule_arm + offset)
            .unwrap();
        assert!(persisted < restart);
        assert!(block[persisted..restart].contains("Ok(outcome)"));
        assert!(block.contains("NOT restarting (fail-closed)"));

        assert!(DAEMON_SOURCE.contains("dcentrald_common::atomic_file::atomic_write"));
        assert!(DAEMON_SOURCE.contains("publication_durability_uncertain"));
        assert!(DAEMON_SOURCE.contains("target_published = error.target_published()"));
    }
}

/// CRASH-SAFETY (BUG-9, 2026-06-05): hard upper bound on the 7-phase hardware
/// bring-up (`init()`). The S9/am1 cold-boot path does blocking I²C/UART I/O
/// — PIC heartbeats, AXI-IIC transactions, chip enumeration with retries —
/// any of which can WEDGE indefinitely on real hardware (documented failure
/// modes: "AXI IIC Controller Stuck State (SR=0xC0)", "dead PICs burn the
/// entire heartbeat budget", a stuck chain-UART RX). Bring-up that hangs with
/// no bound means `run_lifecycle` never reaches `start_api_servers` → the
/// :8080 dashboard / :4028 CGMiner API NEVER come up and there is no recovery
/// (the live `.100`-class symptom: restart-to-mine took the API down 4+ min).
/// Bounding `init()` converts an infinite hang into a clean error that falls
/// back to management-only with the API reachable. Nominal cold boot is
/// ~16-25 s with retries; 90 s leaves generous headroom for a slow-but-healthy
/// unit while still guaranteeing the management plane recovers. Override for
/// lab bring-up of a very slow/cold unit via `DCENT_INIT_TIMEOUT_SECS`.
const DEFAULT_INIT_TIMEOUT_SECS: u64 = 90;
const ENV_INIT_TIMEOUT_SECS: &str = "DCENT_INIT_TIMEOUT_SECS";

/// Resolve the hardware bring-up (`init()`) timeout. Honors the
/// `DCENT_INIT_TIMEOUT_SECS` env override (clamped to a sane floor so an
/// operator can't accidentally set it to 0 and re-break the no-hang guarantee),
/// otherwise returns the compiled default. A value of `0` (or unparsable)
/// falls back to the default.
fn resolve_init_timeout() -> Duration {
    let secs = std::env::var(ENV_INIT_TIMEOUT_SECS)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        // Floor at 10 s: below the nominal cold-boot budget the timeout would
        // false-trip on a healthy unit. The env override is for RAISING the
        // bound on a slow lab unit, not for disabling the guarantee.
        .map(|s| s.max(10))
        .unwrap_or(DEFAULT_INIT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Td003DestructiveWriteRefusal {
    model_name: &'static str,
    source: &'static str,
}

const HASHBOARD_EEPROM_WRITE_DENYLIST: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];

fn normalize_td003_signal(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .collect()
}

/// Legacy kernel-I2C smart-PSU access is proven only for an authoritative
/// `am2-s17*` Buildroot board target plus an S17-family configured model.
///
/// The generic AM2/S19 path has documented cross-bus corruption, while CV,
/// Amlogic, BeagleBone, and unknown platforms have no evidence for this raw
/// bus-1 owner. A configured PSU override remains authoritative and disables
/// probing entirely.
fn legacy_kernel_smart_psu_path_allowed(
    config_model: Option<&str>,
    board_target: &str,
    psu_override_active: bool,
) -> bool {
    let board_target = normalize_td003_signal(board_target);
    if psu_override_active || !board_target.starts_with("am2s17") {
        return false;
    }
    config_model
        .map(normalize_td003_signal)
        .is_some_and(|model| model.contains("s17") && !model.contains("s19"))
}

#[cfg(test)]
mod legacy_smart_psu_path_tests {
    use super::legacy_kernel_smart_psu_path_allowed;

    #[test]
    fn only_explicit_am2_s17_evidence_allows_legacy_kernel_i2c_psu() {
        assert!(legacy_kernel_smart_psu_path_allowed(
            Some("Antminer S17 Pro"),
            "am2-s17pro-zynq",
            false,
        ));

        for (model, board) in [
            (Some("Antminer S19 Pro"), "am2-s17pro-zynq"),
            (Some("Antminer S19j Pro"), "am2-s17pro-zynq"),
            (Some("Antminer S17 Pro"), "am3-cv1835-s19"),
            (Some("Antminer S17 Pro"), "unknown"),
            (Some("Antminer S17 Pro"), ""),
            (None, "am2-s17pro-zynq"),
        ] {
            assert!(
                !legacy_kernel_smart_psu_path_allowed(model, board, false),
                "unexpected legacy PSU admission for model={model:?}, board={board}"
            );
        }
    }

    #[test]
    fn explicit_psu_override_disables_legacy_probe_even_on_s17() {
        assert!(!legacy_kernel_smart_psu_path_allowed(
            Some("Antminer S17"),
            "am2-s17-zynq",
            true,
        ));
    }
}

fn td003_model_from_freeform_signal(signal: &str) -> Option<&'static str> {
    let signal = normalize_td003_signal(signal);
    if signal.contains("s19xp") || signal.contains("s19jxp") {
        Some("Antminer S19 XP")
    } else if signal.contains("t19") {
        Some("Antminer T19")
    } else if signal.contains("t17") {
        Some("Antminer T17")
    } else if signal.contains("s17") {
        Some("Antminer S17 / S17 Pro")
    } else {
        None
    }
}

fn td003_generic_am2_without_exact_board_target(platform_marker: &str, board_target: &str) -> bool {
    let platform = normalize_td003_signal(platform_marker);
    let is_generic_am2 = platform == "zynqbm3am2" || platform == "am2";
    if !is_generic_am2 {
        return false;
    }

    let target = normalize_td003_signal(board_target);
    target.is_empty()
        || matches!(target.as_str(), "unknown" | "am2" | "zynqbm3am2")
        || model::board_target_chip_label(board_target).is_none()
}

fn td003_destructive_write_refusal_from_signals(
    config_model: Option<&str>,
    board_target: &str,
    platform_marker: &str,
    subtype: &str,
) -> Option<Td003DestructiveWriteRefusal> {
    if let Some(model_name) = config_model.and_then(model::td003_management_only_model) {
        return Some(Td003DestructiveWriteRefusal {
            model_name,
            source: "config-model",
        });
    }
    if let Some(model_name) = model::td003_management_only_board_target(board_target) {
        return Some(Td003DestructiveWriteRefusal {
            model_name,
            source: "board-target",
        });
    }
    if let Some(model_name) = td003_model_from_freeform_signal(platform_marker) {
        return Some(Td003DestructiveWriteRefusal {
            model_name,
            source: "platform-marker",
        });
    }
    if let Some(model_name) = td003_model_from_freeform_signal(subtype) {
        return Some(Td003DestructiveWriteRefusal {
            model_name,
            source: "subtype",
        });
    }
    if td003_generic_am2_without_exact_board_target(platform_marker, board_target) {
        return Some(Td003DestructiveWriteRefusal {
            model_name: "generic AM2 platform without exact board target",
            source: "platform-marker+board-target",
        });
    }
    None
}

fn read_first_trimmed(paths: &[&str]) -> String {
    paths
        .iter()
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
}

struct SystemPlatformIdentitySource;

impl crate::daemon_lifecycle::PlatformIdentitySource for SystemPlatformIdentitySource {
    fn capture_identity(&self) -> Result<crate::daemon_lifecycle::PlatformIdentitySnapshot> {
        let optional_declared = |paths: &[&str]| {
            let value = read_first_trimmed(paths);
            (!value.is_empty()).then_some(value)
        };

        let declared_board_target = optional_declared(&["/etc/dcentos/board_target"]);
        let board_desc = declared_board_target
            .as_deref()
            .and_then(dcentrald_common::BoardDesc::lookup);

        Ok(crate::daemon_lifecycle::PlatformIdentitySnapshot {
            declared_board_target,
            board_desc,
            declared_platform_marker: optional_declared(&[
                "/etc/bos_platform",
                "/etc/dcentos/platform",
                "/etc/dcentos-platform",
                "/proc/device-tree/model",
            ]),
            declared_subtype: optional_declared(&["/etc/subtype"]),
            declared_psu_hardware_variant: optional_declared(&[
                "/etc/dcentos/psu_hardware_variant",
            ]),
            // This remains an observation of OS device signatures. It must not
            // be promoted to measured hashboard or ASIC identity.
            observed_control_board: detect_control_board(),
        })
    }
}

/// Capture the production platform identity exactly once for route admission
/// and the complete standard-daemon lifecycle.
pub(crate) fn capture_system_platform_identity(
) -> Result<crate::daemon_lifecycle::PlatformIdentitySnapshot> {
    crate::daemon_lifecycle::PlatformIdentitySource::capture_identity(&SystemPlatformIdentitySource)
}

fn divergent_chip_policy_for_platform(
    enforce_on_xil25: bool,
    platform_marker: &str,
    board_target: &str,
    psu_hardware_variant: Option<&str>,
) -> DivergentChipPolicy {
    if crate::wave55a_recipe_guard::fingerprint_matches_xil_25(
        platform_marker,
        board_target,
        psu_hardware_variant,
    ) && !enforce_on_xil25
    {
        DivergentChipPolicy::LogOnly
    } else {
        DivergentChipPolicy::Enforce
    }
}

#[cfg(test)]
mod divergent_chip_policy_tests {
    use super::divergent_chip_policy_for_platform;
    use dcentrald_asic::chain::DivergentChipPolicy;

    #[test]
    fn xil25_mixed_chip_refusal_is_log_only_until_opted_in() {
        assert_eq!(
            divergent_chip_policy_for_platform(false, "zynq-bm3-am2", "am2-xil", Some("loki")),
            DivergentChipPolicy::LogOnly
        );
        assert_eq!(
            divergent_chip_policy_for_platform(true, "zynq-bm3-am2", "am2-xil", Some("loki")),
            DivergentChipPolicy::Enforce
        );
    }

    #[test]
    fn non_xil25_mixed_chip_refusal_enforces_by_default() {
        for (platform, board_target) in [
            ("zynq-bm3-am2", "am2-s19jpro"),
            ("zynq-bm3-am2", "am2-s19pro"),
            ("zynq-bm1-s9", "am1-s9"),
            ("amlogic-a113d", "am3-aml-s21"),
        ] {
            assert_eq!(
                divergent_chip_policy_for_platform(false, platform, board_target, None),
                DivergentChipPolicy::Enforce,
                "{platform}/{board_target} must enforce divergent production chip IDs"
            );
        }
    }
}

/// Decide whether this unit is an am1-s9 that must take the S9-only devmem I2C
/// path (unbind xiic-i2c + AXI-IIC recovery + an I2C service WITHOUT the
/// hashboard-EEPROM write denylist).
///
/// AUTHORITATIVE-FIRST: a non-empty `/etc/dcentos/board_target` (written by every
/// Buildroot post-build) is definitive — only an `am1-s9*` target is S9; any
/// `am2-*`/`am3-*` target fails CLOSED to the safe (xiic-bound, EEPROM-denylisted)
/// path even if the control-board UIO-count heuristic momentarily disagrees. That
/// heuristic (`detect_control_board`: uio_count<=14 => "Zynq am1-s9") misclassifies
/// a boot-race am2 (S19-family) that enumerates <=14 UIO devices as am1-s9, which
/// would then devmem-write `[55 AA 16]` heartbeats to 0x55-0x57 — the am2 AT24C
/// identity EEPROMs in the protected 0x50-0x57 range — and corrupt them.
///
/// Missing board_target is not identity evidence. The UIO-count-derived
/// control-board string remains useful telemetry but cannot authorize a raw
/// devmem transport or removal of the EEPROM write denylist.
fn is_am1_s9_from_evidence(board_target: &str, _control_board: &str) -> bool {
    board_target.trim().starts_with("am1-s9")
}

/// Existing single-owner I2C constructor selected from one identity snapshot.
///
/// This enum is the precise follow-on capability boundary: a future injected
/// `SerializedI2cFactory` should consume this decision and return the sole
/// `I2cServiceHandle`, owning any required S9 unbind/recovery. Endpoint drivers
/// must not grow independent raw-bus constructors while that factory is moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandardI2cTransport {
    Am1S9Devmem,
    AmlogicProtected,
    KernelProtected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandardHardwareOwnership {
    ColdBringup,
    AdoptedHandoff,
}

/// Complete, sealed execution plan for the legacy standard-daemon route.
///
/// This is intentionally distinct from safe-direction cleanup authority.  It
/// binds the authoritative board target, model topology, voltage-controller
/// class, serialized bus transport, work engine, ownership origin, and exact
/// executable ASIC-driver admission before the route opens I2C, fan, GPIO, or
/// FPGA hardware.  Fields are private so no other runtime can mint one from a
/// chip ID alone.
#[derive(Debug, Clone)]
struct StandardHardwareCompositionAdmission {
    board_target: String,
    profile: &'static MinerProfile,
    pic_type: PicType,
    pic_addrs: Vec<u8>,
    i2c_transport: StandardI2cTransport,
    ownership: StandardHardwareOwnership,
    asic: ChipDriverAdmission,
}

impl StandardHardwareCompositionAdmission {
    fn admits_chip(&self, chip_id: u16) -> bool {
        self.asic.chip_id() == chip_id && self.profile.chip_id == chip_id
    }
}

fn admit_standard_hardware_composition(
    board_target: &str,
    profile: &'static MinerProfile,
    pic_type: PicType,
    pic_addrs: &[u8],
    i2c_transport: StandardI2cTransport,
    passthrough: bool,
    registry: &ChipRegistry,
) -> Result<StandardHardwareCompositionAdmission> {
    let is_am1_s9 = board_target.trim().starts_with("am1-s9");
    validate_profile_platform_authority(board_target, is_am1_s9, profile)?;
    validate_standard_daemon_topology(profile, pic_type, pic_addrs)?;
    match (is_am1_s9, i2c_transport) {
        (true, StandardI2cTransport::Am1S9Devmem) => {}
        (true, observed) => anyhow::bail!(
            "am1-s9 composition requires the devmem I2C transport, got {observed:?}"
        ),
        (false, StandardI2cTransport::Am1S9Devmem) => anyhow::bail!(
            "non-S9 composition {board_target} cannot acquire the destructive AM1 devmem I2C transport"
        ),
        (false, _) => {}
    }
    let recognition = registry.recognize(profile.chip_id).ok_or_else(|| {
        anyhow::anyhow!(
            "{} declares unrecognized ASIC chip ID 0x{:04X}",
            profile.name,
            profile.chip_id
        )
    })?;
    let asic = registry.admit(profile.chip_id).ok_or_else(|| {
        anyhow::anyhow!(
            "{} / {} recognizes {} (0x{:04X}) as {:?}, but the active policy does not admit its executable driver",
            board_target,
            profile.name,
            recognition.chip_name(),
            recognition.chip_id(),
            recognition.maturity(),
        )
    })?;
    Ok(StandardHardwareCompositionAdmission {
        board_target: board_target.to_string(),
        profile,
        pic_type,
        pic_addrs: pic_addrs.to_vec(),
        i2c_transport,
        ownership: if passthrough {
            StandardHardwareOwnership::AdoptedHandoff
        } else {
            StandardHardwareOwnership::ColdBringup
        },
        asic,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SerializedI2cRequest {
    transport: StandardI2cTransport,
    recover_am1_bus: bool,
}

impl SerializedI2cRequest {
    fn new(transport: StandardI2cTransport, recover_am1_bus: bool) -> Result<Self> {
        if recover_am1_bus && transport != StandardI2cTransport::Am1S9Devmem {
            anyhow::bail!("AM1 AXI-IIC recovery was requested for non-S9 transport {transport:?}");
        }
        Ok(Self {
            transport,
            recover_am1_bus,
        })
    }
}

/// Sole-owner capability for the standard daemon's serialized bus-0 service.
///
/// The capability owns transport preparation as well as service construction:
/// callers cannot recover/unbind S9 separately and cannot select a second raw
/// bus constructor after receiving the handle.
trait SerializedI2cFactory {
    fn open_serialized_i2c(
        &self,
        request: SerializedI2cRequest,
    ) -> Result<dcentrald_hal::i2c::I2cServiceHandle>;
}

/// Low-level operations behind the production factory. This is kept private to
/// make exact ordering executable in host tests without touching miner device
/// nodes. It is not an alternate daemon-side bus capability.
trait SerializedI2cOperations {
    type Handle;

    fn spawn_am1_devmem(&self, recover_bus: bool) -> std::io::Result<Self::Handle>;
    fn spawn_amlogic_protected(&self) -> std::io::Result<Self::Handle>;
    fn spawn_kernel_protected(&self) -> std::io::Result<Self::Handle>;
}

fn open_serialized_i2c_with_operations<O: SerializedI2cOperations>(
    operations: &O,
    request: SerializedI2cRequest,
) -> std::io::Result<O::Handle> {
    match request.transport {
        StandardI2cTransport::Am1S9Devmem => operations.spawn_am1_devmem(request.recover_am1_bus),
        StandardI2cTransport::AmlogicProtected => operations.spawn_amlogic_protected(),
        StandardI2cTransport::KernelProtected => operations.spawn_kernel_protected(),
    }
}

struct ProductionSerializedI2cOperations;

impl SerializedI2cOperations for ProductionSerializedI2cOperations {
    type Handle = dcentrald_hal::i2c::I2cServiceHandle;

    fn spawn_am1_devmem(&self, recover_bus: bool) -> std::io::Result<Self::Handle> {
        dcentrald_hal::i2c::spawn_am1_s9_i2c0_service(recover_bus)
    }

    fn spawn_amlogic_protected(&self) -> std::io::Result<Self::Handle> {
        dcentrald_hal::platform::amlogic::spawn_amlogic_protected_i2c0_service()
    }

    fn spawn_kernel_protected(&self) -> std::io::Result<Self::Handle> {
        dcentrald_hal::i2c::spawn_i2c_service_no_register_touch_with_denylist(
            0,
            HASHBOARD_EEPROM_WRITE_DENYLIST.to_vec(),
        )
    }
}

struct ProductionSerializedI2cFactory;

impl SerializedI2cFactory for ProductionSerializedI2cFactory {
    fn open_serialized_i2c(
        &self,
        request: SerializedI2cRequest,
    ) -> Result<dcentrald_hal::i2c::I2cServiceHandle> {
        open_serialized_i2c_with_operations(&ProductionSerializedI2cOperations, request)
            .map_err(|error| anyhow::anyhow!("FATAL: failed to spawn I2C service thread: {error}"))
    }
}

#[cfg(feature = "sim-hal")]
impl SerializedI2cFactory for dcentrald_hal::platform::sim::SimPlatform {
    fn open_serialized_i2c(
        &self,
        request: SerializedI2cRequest,
    ) -> Result<dcentrald_hal::i2c::I2cServiceHandle> {
        // Request validation is shared with production. Hardware-only recovery
        // and unbind are intentionally absent; the simulator's existing method
        // returns its sole shared in-memory service for bus 0.
        let _ = SerializedI2cRequest::new(request.transport, request.recover_am1_bus)?;
        self.open_i2c_service(0).map_err(Into::into)
    }
}

fn standard_i2c_transport(
    identity: &crate::daemon_lifecycle::PlatformIdentitySnapshot,
    is_am1_s9: bool,
) -> Result<StandardI2cTransport> {
    let observed_family = if identity.observed_control_board.starts_with("Zynq") {
        Some(dcentrald_common::BoardFamily::Zynq)
    } else if identity.observed_control_board.starts_with("AML") {
        Some(dcentrald_common::BoardFamily::Amlogic)
    } else if identity.observed_control_board.starts_with("BeagleBone") {
        Some(dcentrald_common::BoardFamily::BeagleBone)
    } else if identity.observed_control_board.starts_with("CVITEK") {
        Some(dcentrald_common::BoardFamily::Cvitek)
    } else if identity.observed_control_board.starts_with("STM32") {
        Some(dcentrald_common::BoardFamily::Stm32Mp15)
    } else {
        None
    };

    // BoardDesc is a typed control-board composition input here, not a miner
    // product descriptor. Only its control-board family participates. ASIC,
    // hashboard, voltage-controller, work-engine, storage, PSU, cooling, and
    // network selection remain owned by their independent evidence boundaries.
    if let (Some(board_desc), Some(observed_family)) = (identity.board_desc, observed_family) {
        if board_desc.family != observed_family {
            anyhow::bail!(
                "declared BoardDesc {} ({:?}) contradicts observed control-board family {:?}; refusing serialized I2C transport selection",
                board_desc.board_target,
                board_desc.family,
                observed_family
            );
        }
    }

    if is_am1_s9 {
        Ok(StandardI2cTransport::Am1S9Devmem)
    } else if identity.observed_control_board.starts_with("AML") {
        Ok(StandardI2cTransport::AmlogicProtected)
    } else {
        Ok(StandardI2cTransport::KernelProtected)
    }
}

#[cfg(test)]
mod serialized_i2c_factory_tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingOperations {
        events: Mutex<Vec<&'static str>>,
    }

    impl RecordingOperations {
        fn record(&self, event: &'static str) {
            self.events.lock().unwrap().push(event);
        }

        fn snapshot(&self) -> Vec<&'static str> {
            self.events.lock().unwrap().clone()
        }
    }

    impl SerializedI2cOperations for RecordingOperations {
        type Handle = &'static str;

        fn spawn_am1_devmem(&self, recover_bus: bool) -> std::io::Result<Self::Handle> {
            self.record(if recover_bus {
                "reserve-prepare-spawn-am1-devmem-bus0"
            } else {
                "reserve-unbind-spawn-am1-devmem-bus0"
            });
            Ok("am1")
        }

        fn spawn_amlogic_protected(&self) -> std::io::Result<Self::Handle> {
            self.record("spawn-amlogic-protected-bus0");
            Ok("amlogic")
        }

        fn spawn_kernel_protected(&self) -> std::io::Result<Self::Handle> {
            self.record("spawn-kernel-protected-bus0");
            Ok("kernel")
        }
    }

    #[test]
    fn serialized_i2c_factory_production_delegation_preserves_s9_ordering() {
        let operations = RecordingOperations::default();
        let request = SerializedI2cRequest::new(StandardI2cTransport::Am1S9Devmem, true)
            .expect("valid S9 recovery request");
        assert_eq!(
            open_serialized_i2c_with_operations(&operations, request).unwrap(),
            "am1"
        );
        assert_eq!(
            operations.snapshot(),
            ["reserve-prepare-spawn-am1-devmem-bus0"]
        );

        let operations = RecordingOperations::default();
        let request = SerializedI2cRequest::new(StandardI2cTransport::Am1S9Devmem, false)
            .expect("valid S9 passthrough request");
        open_serialized_i2c_with_operations(&operations, request).unwrap();
        assert_eq!(
            operations.snapshot(),
            ["reserve-unbind-spawn-am1-devmem-bus0"]
        );
    }

    #[test]
    fn serialized_i2c_factory_production_delegation_keeps_protected_branches_isolated() {
        for (transport, expected_handle, expected_event) in [
            (
                StandardI2cTransport::AmlogicProtected,
                "amlogic",
                "spawn-amlogic-protected-bus0",
            ),
            (
                StandardI2cTransport::KernelProtected,
                "kernel",
                "spawn-kernel-protected-bus0",
            ),
        ] {
            let operations = RecordingOperations::default();
            let request = SerializedI2cRequest::new(transport, false).unwrap();
            assert_eq!(
                open_serialized_i2c_with_operations(&operations, request).unwrap(),
                expected_handle
            );
            assert_eq!(operations.snapshot(), [expected_event]);
        }

        assert!(SerializedI2cRequest::new(StandardI2cTransport::KernelProtected, true).is_err());
        assert!(SerializedI2cRequest::new(StandardI2cTransport::AmlogicProtected, true).is_err());
    }

    #[cfg(feature = "sim-hal")]
    #[test]
    fn serialized_i2c_factory_simplatform_uses_existing_shared_service() {
        use dcentrald_asic::dspic::{DspicFirmware, DspicService};
        use dcentrald_hal::platform::sim::{SimModel, SimPlatform};

        let platform = SimPlatform::new(SimModel::S19Pro);
        let request = SerializedI2cRequest::new(StandardI2cTransport::KernelProtected, false)
            .expect("valid simulated protected request");
        let service = SerializedI2cFactory::open_serialized_i2c(&platform, request).unwrap();
        service
            .write_bytes(0x20, &[0x55, 0xAA, 0x15, 0x01])
            .expect("existing simulated dsPIC service accepts enable");
        assert!(platform.i2c_voltage_enabled().unwrap());

        service.latch_terminal_safe_off();
        let mut controller =
            DspicService::new_with_firmware(service.clone(), 0x20, DspicFirmware::Fw89);
        controller.disable_voltage().unwrap();
        assert!(!platform.i2c_voltage_enabled().unwrap());
        assert!(service
            .write_bytes(0x20, &[0x55, 0xAA, 0x15, 0x01])
            .is_err());
    }
}

#[cfg(test)]
mod is_am1_s9_evidence_tests {
    use super::{is_am1_s9_from_evidence, standard_i2c_transport, StandardI2cTransport};
    use crate::daemon_lifecycle::PlatformIdentitySnapshot;

    fn identity(board_target: Option<&str>, control_board: &str) -> PlatformIdentitySnapshot {
        PlatformIdentitySnapshot {
            declared_board_target: board_target.map(str::to_string),
            board_desc: board_target.and_then(dcentrald_common::BoardDesc::lookup),
            declared_platform_marker: None,
            declared_subtype: None,
            declared_psu_hardware_variant: None,
            observed_control_board: control_board.to_string(),
        }
    }

    #[test]
    fn board_target_overrides_the_control_board_heuristic() {
        // The bug: a boot-race am2 (S19-family) that momentarily enumerates <=14
        // UIO devices makes detect_control_board() return "Zynq am1-s9". The
        // authoritative board_target says am2, so we must NOT take the S9
        // devmem/no-EEPROM-denylist path (which would write to the am2 0x55-0x57
        // identity EEPROMs).
        assert!(!is_am1_s9_from_evidence("am2-s19jpro-zynq", "Zynq am1-s9"));
        assert!(!is_am1_s9_from_evidence("am2-s17p", "Zynq am1-s9"));
        assert!(!is_am1_s9_from_evidence("am3-s21", "Zynq am1-s9"));
        // A genuine am1-s9 target takes the S9 path even if the heuristic disagrees.
        assert!(is_am1_s9_from_evidence("am1-s9", "Zynq am2-s17"));
        assert!(is_am1_s9_from_evidence("am1-s9", ""));
    }

    #[test]
    fn missing_board_target_never_mints_destructive_transport_authority() {
        assert!(!is_am1_s9_from_evidence("", "Zynq am1-s9"));
        assert!(!is_am1_s9_from_evidence("", "Zynq am2-s17"));
        // BeagleBone (AM335x, am3-bb) is NOT S9 — it has its own denylist path.
        assert!(!is_am1_s9_from_evidence("", "BeagleBone S9"));
        assert!(!is_am1_s9_from_evidence("am3-bb-s19jpro", "BeagleBone S9"));
        assert!(!is_am1_s9_from_evidence("   ", "Zynq am1-s9"));
    }

    #[test]
    fn non_s9_am1_variants_take_the_safe_path() {
        // am1-s15 / am1-s17 are Zynq am1 but NOT BM1387 S9 — they must not take
        // the S9 emergency-heartbeat-to-0x55-0x57 path; only "am1-s9*" is S9.
        assert!(!is_am1_s9_from_evidence("am1-s15", "Zynq am1-s9"));
        assert!(!is_am1_s9_from_evidence("am1-s17", "Zynq am1-s9"));
    }

    #[test]
    fn platform_identity_snapshot_selects_the_existing_single_owner_i2c_transport() {
        let s9 = identity(Some("am1-s9"), "Zynq am2-s17");
        assert_eq!(
            standard_i2c_transport(&s9, is_am1_s9_from_evidence(s9.board_target(), "ignored"))
                .unwrap(),
            StandardI2cTransport::Am1S9Devmem
        );

        let amlogic = identity(Some("am3-s21"), "AML Amlogic");
        assert_eq!(
            standard_i2c_transport(&amlogic, false).unwrap(),
            StandardI2cTransport::AmlogicProtected
        );

        // A contradictory/unknown heuristic never removes the protected
        // kernel path without an exact am1-s9 declaration.
        for non_s9 in [
            identity(Some("am2-s19pro"), "Zynq am1-s9"),
            identity(None, "Zynq am1-s9"),
            identity(Some("am3-bb-s19jpro"), "BeagleBone S9"),
        ] {
            assert_eq!(
                standard_i2c_transport(&non_s9, false).unwrap(),
                StandardI2cTransport::KernelProtected
            );
        }
    }

    #[test]
    fn board_desc_runtime_input_rejects_control_board_family_contradictions() {
        let declared_zynq_observed_amlogic = identity(Some("am1-s9"), "AML Amlogic");
        let error = standard_i2c_transport(&declared_zynq_observed_amlogic, true).unwrap_err();
        assert!(error
            .to_string()
            .contains("contradicts observed control-board family"));

        let declared_amlogic_observed_zynq = identity(Some("am3-s21"), "Zynq am2-s17");
        assert!(standard_i2c_transport(&declared_amlogic_observed_zynq, false).is_err());

        let declared_bb_observed_zynq = identity(Some("am3-bb-s19jpro"), "Zynq am2-s17");
        assert!(standard_i2c_transport(&declared_bb_observed_zynq, false).is_err());
    }

    #[test]
    fn board_desc_runtime_input_does_not_select_unowned_machine_facets() {
        // Unknown targets retain the exact legacy observed-family decision;
        // BoardDesc is not required to invent a machine composition.
        let unknown_amlogic = identity(Some("future-board"), "AML Amlogic");
        assert!(unknown_amlogic.board_desc.is_none());
        assert_eq!(
            standard_i2c_transport(&unknown_amlogic, false).unwrap(),
            StandardI2cTransport::AmlogicProtected
        );

        // Conversely, declared metadata alone does not force a transport when
        // the OS has not observed a control-board family. This preserves the
        // prior protected-kernel behavior and avoids collapsing BoardDesc's
        // work/storage/controller fields into whole-miner authority.
        let unobserved_amlogic = identity(Some("am3-s21"), "Unknown");
        assert!(unobserved_amlogic.board_desc.is_some());
        assert_eq!(
            standard_i2c_transport(&unobserved_amlogic, false).unwrap(),
            StandardI2cTransport::KernelProtected
        );
    }
}

fn clamp_fan_pwm(pwm: u8) -> u8 {
    pwm.min(FAN_PWM_MAX)
}

fn fan_pwm_percent(pwm: u8) -> u8 {
    clamp_fan_pwm(pwm)
}

/// Collect one fan-tach snapshot only while availability remains stable across
/// the read. A sampler can lose GPIO evidence between the readiness check and
/// per-channel loads; treating the resulting zeroes as measured stopped fans
/// would turn I/O loss into a false FanPanic. Re-readiness requires a complete
/// measurement window, so an unavailable second check cannot ABA back to ready
/// during this call.
fn collect_fan_tach_evidence<Available, Read>(
    mut available: Available,
    read: Read,
) -> (bool, Vec<u32>)
where
    Available: FnMut() -> bool,
    Read: FnOnce() -> Vec<u32>,
{
    if !available() {
        return (false, Vec::new());
    }
    let readings = read();
    if readings.is_empty() || !available() {
        return (false, Vec::new());
    }
    (true, readings)
}

#[cfg(test)]
mod fan_tach_evidence_tests {
    use super::collect_fan_tach_evidence;
    use std::cell::Cell;

    #[test]
    fn availability_loss_during_read_discards_zeroes_as_unknown() {
        let available = Cell::new(true);
        let (admitted, readings) = collect_fan_tach_evidence(
            || available.get(),
            || {
                available.set(false);
                vec![0, 0, 0, 0]
            },
        );
        assert!(!admitted);
        assert!(readings.is_empty());
    }

    #[test]
    fn stable_available_snapshot_preserves_per_fan_readings() {
        let (admitted, readings) =
            collect_fan_tach_evidence(|| true, || vec![1200, 1180, 1210, 1195]);
        assert!(admitted);
        assert_eq!(readings, [1200, 1180, 1210, 1195]);
    }

    #[test]
    fn available_without_channels_is_not_tach_evidence() {
        let (admitted, readings) = collect_fan_tach_evidence(|| true, Vec::new);
        assert!(!admitted);
        assert!(readings.is_empty());
    }
}

fn identified_miner_profile(chip_id: u16) -> Result<&'static MinerProfile> {
    if chip_id == 0 {
        anyhow::bail!("ASIC identity is uninitialized; refusing model-specific defaults");
    }
    MinerProfile::for_chip(chip_id).ok_or_else(|| {
        anyhow::anyhow!(
            "identified ASIC ChipID 0x{chip_id:04X} has no MinerProfile; refusing S9 topology, voltage, and PLL defaults"
        )
    })
}

/// Resolve topology before ASIC enumeration only from evidence that is strong
/// enough to authorize the standard daemon's model-specific hardware paths.
///
/// An exact `am1-s9` board target is authoritative for the S9 control-board
/// wiring, so it may seed the topology required to reach enumeration. It does
/// not prove that BM1387 silicon was observed; callers must not copy the
/// returned profile's ChipID into the discovered-identity field.
fn pre_enumeration_topology_profile(authoritative_am1_s9: bool) -> Result<&'static MinerProfile> {
    if authoritative_am1_s9 {
        return identified_miner_profile(0x1387);
    }
    anyhow::bail!(
        "no supported ASIC profile or exact am1-s9 topology identity is available; refusing model-specific PIC, chain, GPIO, and UIO access"
    )
}

fn validate_standard_daemon_topology(
    profile: &MinerProfile,
    effective_pic_type: PicType,
    effective_pic_addrs: &[u8],
) -> Result<()> {
    let chain_count = usize::from(profile.chain_count);
    if chain_count == 0 {
        anyhow::bail!("{} topology declares zero chains", profile.name);
    }
    if chain_count != STANDARD_DAEMON_BOARD_SLOTS {
        anyhow::bail!(
            "{} topology declares {} chains but the standard daemon supports exactly {} board slots",
            profile.name,
            chain_count,
            STANDARD_DAEMON_BOARD_SLOTS
        );
    }
    if profile.chain_ids.len() != chain_count {
        anyhow::bail!(
            "{} topology has {} chain IDs for {} chains",
            profile.name,
            profile.chain_ids.len(),
            chain_count
        );
    }
    if profile.uio_bases.len() != chain_count {
        anyhow::bail!(
            "{} topology has {} UIO bases for {} chains; use the platform-specific daemon instead of placeholder UIO routing",
            profile.name,
            profile.uio_bases.len(),
            chain_count
        );
    }
    if !matches!(effective_pic_type, PicType::NoPic) && effective_pic_addrs.len() != chain_count {
        anyhow::bail!(
            "{} topology has {} PIC addresses for {} PIC-controlled chains",
            profile.name,
            effective_pic_addrs.len(),
            chain_count
        );
    }
    if profile
        .chain_ids
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != chain_count
    {
        anyhow::bail!("{} topology contains duplicate chain IDs", profile.name);
    }
    if profile
        .uio_bases
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len()
        != chain_count
    {
        anyhow::bail!("{} topology contains duplicate UIO bases", profile.name);
    }
    if effective_pic_addrs.iter().any(|&address| address > 0x7f) {
        anyhow::bail!("{} topology contains a non-7-bit PIC address", profile.name);
    }
    if !matches!(effective_pic_type, PicType::NoPic)
        && effective_pic_addrs
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != chain_count
    {
        anyhow::bail!("{} topology contains duplicate PIC addresses", profile.name);
    }
    Ok(())
}

fn validate_profile_platform_authority(
    board_target: &str,
    authoritative_am1_s9: bool,
    profile: &MinerProfile,
) -> Result<()> {
    let board_target = board_target.trim().to_ascii_lowercase();
    let target_chip_label = model::board_target_chip_label(&board_target).ok_or_else(|| {
        anyhow::anyhow!(
            "authoritative board target {board_target} does not identify an ASIC family"
        )
    })?;
    let profile_chip_label = format!("BM{:04X}", profile.chip_id);
    if target_chip_label != profile_chip_label {
        anyhow::bail!(
            "authoritative {board_target} identifies {target_chip_label}, contradicting configured {} ({profile_chip_label}) topology",
            profile.name
        );
    }
    if authoritative_am1_s9 {
        return Ok(());
    }
    if board_target.starts_with("am2-") {
        return Ok(());
    }
    anyhow::bail!(
        "authoritative board target {board_target} is not supported by the standard Zynq daemon topology path"
    )
}

fn passthrough_miner_profile(chip_id: u16) -> Result<&'static MinerProfile> {
    identified_miner_profile(chip_id).map_err(|error| {
        anyhow::anyhow!(
            "passthrough requires an explicit supported ASIC model because enumeration is skipped: {error}"
        )
    })
}

#[cfg(test)]
mod identified_miner_profile_tests {
    use super::{
        admit_standard_hardware_composition, identified_miner_profile, passthrough_miner_profile,
        pre_enumeration_topology_profile, validate_profile_platform_authority,
        validate_standard_daemon_topology, StandardHardwareOwnership, StandardI2cTransport,
    };
    use dcentrald_asic::drivers::{
        ChipDriverExecutionPolicy, ChipDriverMaturity, ChipRegistry, PicType,
    };

    #[test]
    fn known_identity_resolves_and_unknown_or_zero_fail_closed() {
        assert_eq!(
            identified_miner_profile(0x1387).unwrap().name,
            "Antminer S9"
        );
        for chip_id in [0, 0x1234, 0xFFFF] {
            let error = identified_miner_profile(chip_id).unwrap_err().to_string();
            assert!(
                error.contains("refusing"),
                "ChipID 0x{chip_id:04X} must fail closed: {error}"
            );
        }
    }

    #[test]
    fn passthrough_never_invents_an_asic_identity() {
        assert_eq!(
            passthrough_miner_profile(0x1387).unwrap().name,
            "Antminer S9"
        );
        for chip_id in [0, 0x1234, 0xFFFF] {
            let error = passthrough_miner_profile(chip_id).unwrap_err().to_string();
            assert!(error.contains("enumeration is skipped"));
            assert!(error.contains("refusing"));
        }
    }

    #[test]
    fn only_authoritative_am1_s9_may_seed_an_absent_topology() {
        assert_eq!(
            pre_enumeration_topology_profile(true).unwrap().name,
            "Antminer S9"
        );
        let error = pre_enumeration_topology_profile(false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing model-specific"));
    }

    #[test]
    fn standard_daemon_admission_validates_every_indexed_topology_table() {
        let s9 = identified_miner_profile(0x1387).unwrap();
        validate_standard_daemon_topology(s9, PicType::Pic16F1704, s9.pic_addrs).unwrap();

        let amlogic = identified_miner_profile(0x1368).unwrap();
        let error = validate_standard_daemon_topology(amlogic, PicType::NoPic, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("UIO bases"));
    }

    #[test]
    fn configured_profile_cannot_override_authoritative_platform_wiring() {
        let s9 = identified_miner_profile(0x1387).unwrap();
        let s19 = identified_miner_profile(0x1398).unwrap();
        validate_profile_platform_authority("am1-s9", true, s9).unwrap();
        validate_profile_platform_authority("am2-s19pro", false, s19).unwrap();
        assert!(validate_profile_platform_authority("am1-s9", true, s19).is_err());
        assert!(validate_profile_platform_authority("am2-s19pro", false, s9).is_err());
        assert!(validate_profile_platform_authority("am3-s21", false, s19).is_err());
    }

    #[test]
    fn sealed_standard_composition_binds_every_execution_facet() {
        let s9 = identified_miner_profile(0x1387).unwrap();
        let registry = ChipRegistry::production();
        let admission = admit_standard_hardware_composition(
            "am1-s9",
            s9,
            PicType::Pic16F1704,
            s9.pic_addrs,
            StandardI2cTransport::Am1S9Devmem,
            false,
            &registry,
        )
        .unwrap();
        assert_eq!(admission.board_target, "am1-s9");
        assert_eq!(admission.ownership, StandardHardwareOwnership::ColdBringup);
        assert_eq!(admission.asic.chip_id(), 0x1387);
        assert_eq!(
            admission.asic.recognition().maturity(),
            ChipDriverMaturity::Production
        );
        assert!(admission.admits_chip(0x1387));
        assert!(!admission.admits_chip(0x1398));

        assert!(admit_standard_hardware_composition(
            "am1-s9",
            s9,
            PicType::Pic16F1704,
            s9.pic_addrs,
            StandardI2cTransport::KernelProtected,
            false,
            &registry,
        )
        .is_err());
    }

    #[test]
    fn experimental_driver_authority_is_exact_and_adopted_handoff_is_explicit() {
        let s19 = identified_miner_profile(0x1398).unwrap();
        assert!(admit_standard_hardware_composition(
            "am2-s19pro",
            s19,
            PicType::DsPic33EP,
            s19.pic_addrs,
            StandardI2cTransport::KernelProtected,
            true,
            &ChipRegistry::production(),
        )
        .is_err());

        let policy = ChipDriverExecutionPolicy::with_experimental_chip(0x1398);
        let registry = ChipRegistry::with_execution_policy(policy);
        let admission = admit_standard_hardware_composition(
            "am2-s19pro",
            s19,
            PicType::DsPic33EP,
            s19.pic_addrs,
            StandardI2cTransport::KernelProtected,
            true,
            &registry,
        )
        .unwrap();
        assert_eq!(
            admission.ownership,
            StandardHardwareOwnership::AdoptedHandoff
        );
        assert_eq!(
            admission.asic.recognition().maturity(),
            ChipDriverMaturity::Experimental
        );
        assert!(admission.admits_chip(0x1398));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CoolingReadiness {
    Ready {
        commanded_pwm: u8,
        tach_rpm_by_physical_fan: Vec<Option<u32>>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardPresence {
    Present,
    Absent,
    Unknown,
}

fn board_presence_from_sysfs_value(value: Option<&str>) -> BoardPresence {
    match value.map(str::trim) {
        Some("1") => BoardPresence::Present,
        Some("0") => BoardPresence::Absent,
        _ => BoardPresence::Unknown,
    }
}

fn cooling_readiness_from_tach(
    commanded_pwm: u8,
    tach_rpm_by_physical_fan: &[Option<u32>],
) -> CoolingReadiness {
    if commanded_pwm > FAN_PWM_SAFETY_MAX {
        CoolingReadiness::Unavailable {
            reason: format!(
                "spin-up PWM {commanded_pwm} exceeds home safety cap {FAN_PWM_SAFETY_MAX}"
            ),
        }
    } else if !tach_rpm_by_physical_fan.iter().any(Option::is_some) {
        CoolingReadiness::Unavailable {
            reason: "fan variant exposes no observable physical-fan tach channel".into(),
        }
    } else if tach_rpm_by_physical_fan
        .iter()
        .flatten()
        .any(|rpm| *rpm == 0)
    {
        CoolingReadiness::Unavailable {
            reason: format!(
                "one or more required physical-fan tach channels never showed motion: {tach_rpm_by_physical_fan:?}"
            ),
        }
    } else {
        CoolingReadiness::Ready {
            commanded_pwm,
            tach_rpm_by_physical_fan: tach_rpm_by_physical_fan.to_vec(),
        }
    }
}

fn hardware_bringup_permitted(cooling: &CoolingReadiness, presence: &[BoardPresence]) -> bool {
    matches!(cooling, CoolingReadiness::Ready { .. })
        && presence
            .iter()
            .any(|state| *state == BoardPresence::Present)
}

#[cfg(test)]
mod hardware_preflight_policy_tests {
    use super::{
        board_presence_from_sysfs_value, cooling_readiness_from_tach, hardware_bringup_permitted,
        BoardPresence, CoolingReadiness, COOLING_SPINUP_DWELL, COOLING_SPINUP_SAMPLES,
        COOLING_SPINUP_SAMPLE_INTERVAL, FAN_PWM_SAFETY_MAX,
    };

    const DAEMON_SOURCE: &str = include_str!("daemon.rs");

    #[test]
    fn failed_or_malformed_presence_reads_are_unknown_never_present() {
        assert_eq!(
            board_presence_from_sysfs_value(None),
            BoardPresence::Unknown
        );
        assert_eq!(
            board_presence_from_sysfs_value(Some("")),
            BoardPresence::Unknown
        );
        assert_eq!(
            board_presence_from_sysfs_value(Some("garbage")),
            BoardPresence::Unknown
        );
        assert_eq!(
            board_presence_from_sysfs_value(Some("1")),
            BoardPresence::Present
        );
        assert_eq!(
            board_presence_from_sysfs_value(Some("0")),
            BoardPresence::Absent
        );
    }

    #[test]
    fn bringup_requires_cooling_control_and_positive_board_presence() {
        let ready = cooling_readiness_from_tach(FAN_PWM_SAFETY_MAX, &[None, Some(1_800)]);
        let unavailable = CoolingReadiness::Unavailable {
            reason: "fan-control unavailable".into(),
        };

        assert!(hardware_bringup_permitted(
            &ready,
            &[BoardPresence::Unknown, BoardPresence::Present]
        ));
        assert!(!hardware_bringup_permitted(
            &ready,
            &[BoardPresence::Unknown, BoardPresence::Absent]
        ));
        assert!(!hardware_bringup_permitted(
            &unavailable,
            &[BoardPresence::Present]
        ));
    }

    #[test]
    fn zero_tach_and_over_cap_commands_deny_cooling_readiness() {
        let zero_tach = cooling_readiness_from_tach(FAN_PWM_SAFETY_MAX, &[Some(1_800), Some(0)]);
        assert!(matches!(zero_tach, CoolingReadiness::Unavailable { .. }));
        assert!(matches!(
            cooling_readiness_from_tach(FAN_PWM_SAFETY_MAX + 1, &[Some(2_000)]),
            CoolingReadiness::Unavailable { .. }
        ));
        assert!(matches!(
            cooling_readiness_from_tach(FAN_PWM_SAFETY_MAX, &[None, None]),
            CoolingReadiness::Unavailable { .. }
        ));
    }

    #[test]
    fn spinup_preflight_is_bounded_and_home_capped() {
        assert!(COOLING_SPINUP_DWELL <= std::time::Duration::from_secs(3));
        assert!(
            COOLING_SPINUP_SAMPLE_INTERVAL * COOLING_SPINUP_SAMPLES as u32 <= COOLING_SPINUP_DWELL
        );
        assert!(FAN_PWM_SAFETY_MAX <= 30);
    }

    #[test]
    fn source_orders_cooling_gpio_and_presence_before_non_mutating_pic16_capabilities() {
        // Anchor on the implementation's doc comment instead of reconstructing
        // an obsolete single-line signature. `init` intentionally accepts the
        // immutable platform-identity snapshot now.
        let init_doc = ["    /// Initialize all hardware ", "and subsystems."].concat();
        let init_doc = DAEMON_SOURCE.find(&init_doc).expect("init doc marker");
        let init_signature = ["async fn ", "init("].concat();
        let init = DAEMON_SOURCE[init_doc..]
            .find(&init_signature)
            .map(|offset| init_doc + offset)
            .expect("init body");
        let cooling = DAEMON_SOURCE[init..]
            .find("P0: Cooling Readiness Preflight")
            .map(|offset| init + offset)
            .expect("cooling preflight marker");
        let gpio = DAEMON_SOURCE[cooling..]
            .find("Phase 2b: GPIO Controller Init")
            .map(|offset| cooling + offset)
            .expect("GPIO preflight marker");
        let presence = DAEMON_SOURCE[gpio..]
            .find("Phase 3: Hash Board Detection")
            .map(|offset| gpio + offset)
            .expect("presence preflight marker");
        let capability = DAEMON_SOURCE[presence..]
            .find("Phase 0: Non-Mutating PIC16 Topology Capabilities")
            .map(|offset| presence + offset)
            .expect("PIC16 capability marker");
        assert!(cooling < gpio && gpio < presence && presence < capability);

        let pre_admission = &DAEMON_SOURCE[init..capability];
        assert!(!pre_admission.contains("[0x55, 0xAA, 0x16]"));
        assert!(!pre_admission.contains("devmem_i2c_write("));
        assert!(!pre_admission.contains(".heartbeat("));

        let endpoint_slice = &DAEMON_SOURCE[capability..];
        let endpoint_slice = endpoint_slice
            .split("Found {} hash board(s)")
            .next()
            .expect("endpoint capability block");
        assert!(endpoint_slice.contains("self.detected_board_indices"));
        assert!(endpoint_slice.contains("discover_system_pic16_endpoint"));
        assert!(endpoint_slice.contains("Pic16EndpointSession::new"));
        assert!(!endpoint_slice.contains("i2c_svc.heartbeat("));
        assert!(!endpoint_slice.contains(".pic16_heartbeat("));
        assert!(endpoint_slice.contains("if is_am1_s9"));
        assert!(endpoint_slice.contains("DEFAULT_PIC_ADDRS.get(board_index)"));
        assert!(!endpoint_slice.contains("self.pic_addrs()"));
        assert!(!endpoint_slice.contains("configured_model_pic"));
        assert!(!endpoint_slice.contains("miner_profile"));

        let cooling_handoff = &DAEMON_SOURCE[cooling..capability];
        assert!(!cooling_handoff.contains("FAN_PWM_QUIET_BOOT"));
        let legacy_runtime_drop = ["Fan mining boot", ": PWM"].concat();
        assert!(!DAEMON_SOURCE.contains(&legacy_runtime_drop));

        let legacy_assumption = ["assuming board is", " present"].concat();
        assert!(!DAEMON_SOURCE.contains(&legacy_assumption));
        assert!(DAEMON_SOURCE.contains("gpio.read_plug_detect()"));
        assert!(DAEMON_SOURCE.contains("BoardPresence::Unknown"));
        assert!(DAEMON_SOURCE.contains("latch_terminal_safe_off()"));
    }

    #[test]
    fn init_denial_cannot_turn_an_empty_disable_set_into_safe_off_proof() {
        let init_signature = ["async fn in", "it("].concat();
        let init = DAEMON_SOURCE
            .find(&init_signature)
            .expect("standard hardware init body");
        let unknown = DAEMON_SOURCE[init..]
            .find("self.preflight_hardware_state_unknown = true;")
            .map(|offset| init + offset)
            .expect("init entry must conservatively mark rail state unknown");
        let composition = DAEMON_SOURCE[init..]
            .find("let composition_admission = admit_standard_hardware_composition(")
            .map(|offset| init + offset)
            .expect("standard composition admission");
        let complete = DAEMON_SOURCE[composition..]
            .find("self.preflight_hardware_state_unknown = false;")
            .map(|offset| composition + offset)
            .expect("successful init completion receipt");

        assert!(
            unknown < composition && composition < complete,
            "composition denial must retain unknown rail state until complete init succeeds"
        );
        let shutdown = DAEMON_SOURCE
            .rfind("let mut software_disable_failed =")
            .expect("shutdown safe-off evidence fold");
        let disable_fold = DAEMON_SOURCE[shutdown..]
            .split_once(';')
            .map(|(statement, _)| statement.split_whitespace().collect::<Vec<_>>().join(" "))
            .expect("shutdown safe-off evidence statement");
        assert_eq!(
            disable_fold,
            "let mut software_disable_failed = self.preflight_hardware_state_unknown || mining_quiescence_failed"
        );
    }

    #[test]
    fn standard_daemon_pic16_services_have_no_raw_constructor_fallback() {
        let raw_new = ["PicServiceController", "::new("].concat();
        let raw_new_with_firmware = ["PicServiceController", "::new_with_firmware("].concat();
        assert!(!DAEMON_SOURCE.contains(&raw_new));
        assert!(!DAEMON_SOURCE.contains(&raw_new_with_firmware));
        assert!(DAEMON_SOURCE.contains("pic16_service_for_endpoint("));
        assert!(DAEMON_SOURCE.contains("refusing raw-address fallback"));
    }

    #[test]
    fn controller_admission_is_endpoint_scoped_and_gates_phase7() {
        let hot_start_marker = DAEMON_SOURCE
            .rfind("Hot start: sending PIC16 heartbeats via kernel I2C")
            .expect("hot-start controller marker");
        let hot_start = DAEMON_SOURCE[hot_start_marker..]
            .split("Collect PIC addresses from hot chains")
            .next()
            .expect("bounded hot-start controller block");
        assert!(hot_start.contains("for &chain_idx in &hot_chain_indices"));
        assert!(!hot_start.contains("for &idx in &self.detected_board_indices"));

        assert!(DAEMON_SOURCE.contains("cold_pic_admitted_board_indices"));
        assert!(DAEMON_SOURCE.contains("controller_admitted_chain_indices"));
        assert!(DAEMON_SOURCE.contains("controller admission is absent; chain remains disabled"));
        assert!(DAEMON_SOURCE.contains("PIC16 pre-admission heartbeat intentionally skipped"));
        assert!(DAEMON_SOURCE.contains("PIC16 detect/disable intentionally skipped"));
    }
}

fn normalize_fan_pwm_bounds(min_pwm: u8, max_pwm: u8) -> (u8, u8) {
    let max_pwm = clamp_fan_pwm(max_pwm);
    let min_pwm = clamp_fan_pwm(min_pwm).min(max_pwm);
    (min_pwm, max_pwm)
}

/// THERM-2 (defense-in-depth): the effective curtailment-SLEEP fan PWM.
///
/// Sleep is the quietest state — the boards are de-energized — so the sleep fan
/// command must respect BOTH the absolute home cap (`FAN_PWM_SAFETY_MAX = 30`)
/// AND any lower per-profile quiet ceiling (`cfg_fan_max_pwm`). The active-mining
/// arm already clamps to `cfg_fan_max_pwm` (see the `ThermalAction::SetFanPwm`
/// arm); previously the sleep arm only clamped to `FAN_PWM_SAFETY_MAX`, so a
/// profile with `fan_max_pwm < 30` could end up LOUDER asleep than awake. This
/// is additive and can only ever LOWER the sleep PWM further — it never raises
/// the fan (cut-hash-before-noise is preserved).
fn effective_sleep_fan_pwm(sleep_fan_pwm: u8, cfg_fan_max_pwm: u8) -> u8 {
    // Same policy language as hybrid park / thermal hard-stop (FanCommand).
    dcentrald_common::FanCommand {
        profile_max_pwm: cfg_fan_max_pwm
            .min(FAN_PWM_SAFETY_MAX)
            .min(clamp_fan_pwm(100)),
        requested_pwm: clamp_fan_pwm(sleep_fan_pwm),
        apply_home_safety_cap: true,
    }
    .effective_pwm()
}

/// THERM-3 (fail-closed defense-in-depth): the per-round result of a thermal
/// emergency / fan-failure voltage-disable attempt.
///
/// Returns `true` ("this round disabled all boards") ONLY when the runtime
/// voltage channel exists (`channel_present`) AND every queued `DisableVoltage`
/// was acknowledged (`all_addrs_acked`). With no channel no command can be sent,
/// so the round MUST be reported as FAILED rather than silently claiming
/// `all_disabled = true` while the hash boards stay energized.
///
/// On the S9 gating path `thermal_voltage_tx` is always `Some`, so this guard is
/// latent today (the `channel_present == true` branch is the only one taken). It
/// exists so a future platform that wires the thermal loop without a voltage
/// channel can never report a false all-clear after a thermal emergency.
const fn thermal_disable_round_ok(channel_present: bool, all_addrs_acked: bool) -> bool {
    channel_present && all_addrs_acked
}

fn mark_thermal_emergency_active(latch: &AtomicBool) {
    latch.store(true, Ordering::Release);
}

fn clear_thermal_emergency_active(latch: &AtomicBool) {
    latch.store(false, Ordering::Release);
}

fn thermal_emergency_active(latch: &AtomicBool) -> bool {
    latch.load(Ordering::Acquire)
}

/// The kicker's tokio interval period MUST be non-zero — `tokio::time::interval`
/// panics on a zero `Duration`. Config `validate()` already rejects
/// `kick_interval_s == 0` when the watchdog is enabled, but this is the single,
/// tested, panic-safe source for the kicker period so a validation-bypassing
/// construction (a programmatic default / a future call site) can never panic the
/// daemon at startup and leave the miner running with NO hardware watchdog.
pub(crate) fn watchdog_interval_secs(kick_interval_s: u64) -> u64 {
    kick_interval_s.max(1)
}

fn watchdog_teardown_kick_allowed(
    deadline: tokio::time::Instant,
    now: tokio::time::Instant,
) -> bool {
    now < deadline
}

fn watchdog_stall_limit(
    effective_timeout_s: u64,
    kick_secs: u64,
    expected_liveness_interval: Option<Duration>,
) -> u64 {
    let kick_secs = kick_secs.max(1);
    let half_window_limit = ((effective_timeout_s / 2) / kick_secs).max(2);
    let cadence_limit = expected_liveness_interval
        .map(|interval| {
            ((interval.as_secs_f64() / kick_secs as f64).ceil() as u64).saturating_add(2)
        })
        .unwrap_or(0);
    half_window_limit.max(cadence_limit)
}

/// Pure safety-liveness gate for the watchdog kicker — decides, on each kick
/// tick, whether to pet `/dev/watchdog` given the runtime loop's liveness
/// counter. Returns `(should_kick, new_last_live, new_stalls)`.
///
/// Load-bearing safety logic (extracted from `spawn_watchdog_kicker` so it can
/// be tested): a LIVELOCKED safety loop is the one failure that turns an
/// availability stall into an energized-board safety hazard, so once the loop has failed
/// to advance for `stall_limit` consecutive ticks the kick is WITHHELD and the
/// SoC reboots. It must never false-positive:
///   - `cur == 0`  → the monitored loop hasn't started yet (or has no counter):
///                    kick, and hold the stall count at 0 (legitimate startup/idle).
///   - `cur == last_live` → no advance this tick: bump the stall count; withhold
///                    the kick only once it reaches `stall_limit`.
///   - `cur` advanced → healthy: reset the stall count and kick.
pub(crate) fn watchdog_kick_decision(
    cur: u64,
    last_live: u64,
    stalls: u64,
    stall_limit: u64,
) -> (bool, u64, u64) {
    if cur == 0 {
        (true, last_live, 0)
    } else if cur == last_live {
        let stalls = stalls.saturating_add(1);
        (stalls < stall_limit, last_live, stalls)
    } else {
        (true, cur, 0)
    }
}

/// Guard the thermal PID loop period before it is fed to
/// `tokio::time::interval(Duration::from_secs_f32(..))`. `from_secs_f32` PANICS
/// on a negative, NaN, or overflowing (>= 2^64 s, incl. `f32::INFINITY`) input —
/// and on this `panic = "abort"` build that kills the daemon + thermal supervisor
/// with the hash boards powered. Config `validate()` already rejects non-finite /
/// <=0 / >60, but this point-of-use guard must be TOTAL (an unvalidated or
/// directly-built config must not crash the thermal loop): the prior `.max(0.5)`
/// floored the lower end and NaN but left the upper end open, so a value that
/// overflowed f32 to INFINITY (e.g. `1e40`) still panicked. Map any non-finite
/// value to the 5.0 default and clamp finite values to [0.5, 60] s.
pub(crate) fn thermal_pid_interval_secs(pid_interval_s: f32) -> f32 {
    if pid_interval_s.is_finite() {
        pid_interval_s.clamp(0.5, 60.0)
    } else {
        5.0
    }
}

/// Redact a webhook URL for logging. Discord (`/api/webhooks/<id>/<TOKEN>`) and
/// Telegram (`/bot<TOKEN>/...`) webhook URLs embed a SECRET in the path (and some
/// generic webhooks put a `?token=` in the query) — logging the raw URL leaks it
/// into daemon logs, support bundles, and the dashboard log-tail. Keep only
/// `scheme://host` so the operator can still see WHICH service is configured,
/// and replace the credential-bearing path/query with `<redacted>`. A URL with
/// no scheme (unknown shape, possibly a bare token) is redacted entirely.
pub(crate) fn sanitize_webhook_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return "<redacted-webhook-url>".to_string();
    };
    let after_scheme = scheme_end + 3;
    let rest = &url[after_scheme..];
    // The host authority ends at the first '/' or '?'. Drop any `user:pass@`
    // userinfo defensively (webhook URLs don't use it, but never log it if present).
    let host_span_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..host_span_end];
    let host = authority.rsplit('@').next().unwrap_or(authority);
    if host_span_end < rest.len() {
        format!("{}{}/<redacted>", &url[..after_scheme], host)
    } else {
        // No path/query to hide.
        format!("{}{}", &url[..after_scheme], host)
    }
}

#[cfg(test)]
mod watchdog_interval_tests {
    use super::{thermal_pid_interval_secs, watchdog_interval_secs};

    #[test]
    fn watchdog_interval_secs_is_never_zero() {
        // tokio::time::interval panics on a zero period. A kick_interval of 0 (a
        // typo that slipped past config validate(), or a programmatic default) must
        // fall back to 1s, never 0 — otherwise spawn_watchdog_kicker panics at
        // startup and the miner runs with NO hardware watchdog. Valid periods pass
        // through unchanged.
        assert_eq!(watchdog_interval_secs(0), 1);
        assert_eq!(watchdog_interval_secs(1), 1);
        assert_eq!(watchdog_interval_secs(5), 5);
        assert_eq!(watchdog_interval_secs(30), 30);
        for k in 0u64..=300 {
            assert!(
                watchdog_interval_secs(k) >= 1,
                "kicker period must be >= 1 for kick_interval {k}"
            );
        }
    }

    #[test]
    fn thermal_pid_interval_never_panics_from_secs_f32() {
        // Every output must be finite and in [0.5, 60] so
        // Duration::from_secs_f32 can NEVER panic (which on panic=abort would kill
        // the thermal supervisor with boards powered). Covers the adversarial
        // inputs the prior `.max(0.5)` mishandled: INFINITY (from an overflowing
        // TOML value), NaN, and negatives.
        let inf_overflow = f32::MAX * 2.0; // overflows to +INF, the `1e40` case
        for v in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            inf_overflow,
            -1.0,
            0.0,
            0.1,
            0.5,
            2.0,
            5.0,
            60.0,
            100.0,
            1e30,
        ] {
            let g = thermal_pid_interval_secs(v);
            assert!(
                g.is_finite() && (0.5..=60.0).contains(&g),
                "guarded {v} -> {g} not in [0.5, 60]"
            );
            // Constructing the Duration must not panic.
            let _ = std::time::Duration::from_secs_f32(g);
        }
        // Valid values pass through; the documented default sits mid-range.
        assert_eq!(thermal_pid_interval_secs(2.0), 2.0);
        assert_eq!(thermal_pid_interval_secs(5.0), 5.0);
    }

    #[test]
    fn sanitize_webhook_url_redacts_the_secret_and_keeps_the_host() {
        use super::sanitize_webhook_url;
        // Telegram + Discord webhook URLs embed a secret token in the PATH; the
        // host is safe to show, the path/query MUST be redacted so the daemon log
        // / support bundle / dashboard log-tail never leaks it.
        let tg = sanitize_webhook_url(
            "https://api.telegram.org/bot123456:AAExampleSecretToken/sendMessage",
        );
        assert_eq!(tg, "https://api.telegram.org/<redacted>");
        assert!(!tg.contains("AAExampleSecretToken"), "token leaked: {tg}");

        let dc = sanitize_webhook_url("https://discord.com/api/webhooks/998877/dQw4w9SecretToken");
        assert_eq!(dc, "https://discord.com/<redacted>");
        assert!(!dc.contains("dQw4w9SecretToken"), "token leaked: {dc}");

        // A `?token=` query is redacted too.
        let q = sanitize_webhook_url("https://hooks.example.com/notify?token=SUPERSECRET");
        assert_eq!(q, "https://hooks.example.com/<redacted>");
        assert!(!q.contains("SUPERSECRET"));

        // Defensive: `user:pass@` userinfo (unused by webhooks) is dropped.
        let ui = sanitize_webhook_url("https://user:pass@host.example.com/path/TOKEN");
        assert_eq!(ui, "https://host.example.com/<redacted>");
        assert!(
            !ui.contains("pass") && !ui.contains("TOKEN"),
            "leaked: {ui}"
        );

        // Host-only URL (no path/query) has nothing to redact.
        assert_eq!(
            sanitize_webhook_url("https://api.telegram.org"),
            "https://api.telegram.org"
        );

        // No scheme -> redact entirely (could be a bare token).
        assert_eq!(
            sanitize_webhook_url("bot123:SECRET/sendMessage"),
            "<redacted-webhook-url>"
        );
        assert_eq!(sanitize_webhook_url(""), "<redacted-webhook-url>");
    }

    #[test]
    fn watchdog_kick_decision_withholds_only_when_the_safety_loop_is_hung() {
        use super::watchdog_kick_decision;
        let limit = 3u64;
        // cur==0: monitored loop hasn't started -> ALWAYS kick, hold stalls at 0.
        assert_eq!(watchdog_kick_decision(0, 0, 0, limit), (true, 0, 0));
        assert_eq!(watchdog_kick_decision(0, 5, 2, limit), (true, 5, 0));
        // Advancing counter -> healthy: kick, reset stalls, adopt new last_live.
        assert_eq!(watchdog_kick_decision(7, 5, 2, limit), (true, 7, 0));
        // Stalled but under the limit -> still kick, bump stalls.
        assert_eq!(watchdog_kick_decision(5, 5, 0, limit), (true, 5, 1));
        assert_eq!(watchdog_kick_decision(5, 5, 1, limit), (true, 5, 2));
        // At the limit -> WITHHOLD the kick so the WDT fires.
        assert_eq!(watchdog_kick_decision(5, 5, 2, limit), (false, 5, 3));
        assert_eq!(watchdog_kick_decision(5, 5, 3, limit), (false, 5, 4));
        // Recovery: the loop advances again -> kick + reset, even after a withhold.
        assert_eq!(watchdog_kick_decision(9, 5, 4, limit), (true, 9, 0));
        // A permanently-hung loop must EVENTUALLY withhold (thermal-safety reboot).
        let (mut last, mut st, mut withheld) = (10u64, 0u64, false);
        for _ in 0..10 {
            let (kick, nl, ns) = watchdog_kick_decision(10, last, st, limit);
            last = nl;
            st = ns;
            if !kick {
                withheld = true;
            }
        }
        assert!(
            withheld,
            "a permanently-hung safety loop must eventually withhold the watchdog kick"
        );
    }

    #[test]
    fn watchdog_teardown_grace_is_absolute_and_expires_fail_closed() {
        let now = tokio::time::Instant::now();
        let deadline = now + super::WATCHDOG_TEARDOWN_GRACE;

        assert!(super::watchdog_teardown_kick_allowed(deadline, now));
        assert!(super::watchdog_teardown_kick_allowed(
            deadline,
            deadline - std::time::Duration::from_nanos(1)
        ));
        assert!(!super::watchdog_teardown_kick_allowed(deadline, deadline));
        assert!(!super::watchdog_teardown_kick_allowed(
            deadline,
            deadline + std::time::Duration::from_secs(1)
        ));
    }

    #[test]
    fn watchdog_stall_limit_respects_slower_healthy_thermal_cadence() {
        let limit = super::watchdog_stall_limit(2, 1, Some(std::time::Duration::from_secs(5)));
        assert_eq!(limit, 7);

        let mut last = 1_u64;
        let mut stalls = 0_u64;
        for _ in 0..6 {
            let (kick, new_last, new_stalls) =
                super::watchdog_kick_decision(1, last, stalls, limit);
            assert!(
                kick,
                "a healthy 5s thermal cadence must not be withheld early"
            );
            last = new_last;
            stalls = new_stalls;
        }
        let (kick, _, _) = super::watchdog_kick_decision(1, last, stalls, limit);
        assert!(
            !kick,
            "a thermal loop stalled beyond cadence plus jitter must fail closed"
        );
    }
}

/// Arm the hardware `/dev/watchdog` and spawn the kicker task **iff** enabled in
/// config. Byte-faithful extraction of the standard `Daemon::run` watchdog
/// arming so every mining entry path arms the SAME supervision the standard path
/// always had.
///
/// Today only the standard `Daemon::run` path armed `/dev/watchdog`; the
/// `--s19j-hybrid`, `--serial-mining`, and `--am3-bb-mining` paths armed none, so
/// a CPU/runtime hang on one of those left the hash boards energized and
/// unsupervised. They now all call this helper.
///
/// MUST be called AFTER hardware bring-up / chain-enum completes (mirrors the
/// standard path's NEW-4 "open after init, not during the slow bring-up"
/// discipline): the watchdog starts counting on open, so arming during a slow
/// cold-boot could trip the DTB-default ~10 s window and reboot mid-bring-up
/// (which would break `a lab unit` standalone bring-up). Placement at each call site is
/// load-bearing.
///
/// Gated on `watchdog.enabled` (default `true`; the `a lab unit`/XIL bring-up configs
/// set it `false`, so this is INERT on `a lab unit` and those recipes stay
/// byte-unchanged — the desired safe outcome). `config.rs` already rejects a
/// `kick_interval_s == 0` / `kick_interval_s >= timeout_s` typo when enabled.
///
/// Must be invoked from within a Tokio runtime context (all four call sites are:
/// the three async `run()` paths directly, and the am3-bb blocking path via a
/// `Handle::enter()` guard).
struct WatchdogKickerSetup {
    watchdog: Watchdog,
    kick_secs: u64,
    stall_limit: u64,
}

type WatchdogDisarmResult = std::result::Result<(), String>;

fn prepare_watchdog_kicker(
    watchdog: &crate::config::WatchdogConfig,
    expected_liveness_interval: Option<Duration>,
) -> Option<WatchdogKickerSetup> {
    if !watchdog.enabled {
        return None;
    }
    // NEW-4 (2026-06-10 adversarial pass): open the watchdog HERE (after init).
    // Open + set_timeout + an immediate kick + the kicker loop all happen
    // together, so the DTB-10s window can never fire during the slow hardware init.
    let wd = match Watchdog::open() {
        Ok(wd) => {
            info!("Watchdog opened at /dev/watchdog — SoC will auto-reboot if dcentrald crashes");
            wd
        }
        Err(e) => {
            warn!(error = %e, "Watchdog not available — miner will not auto-recover from crashes (this is OK for development)");
            return None;
        }
    };
    // BUG FIX (2026-04-11): Apply timeout_s from config to hardware watchdog.
    // Was parsed from TOML but never sent to kernel driver.
    let effective_timeout_s = {
        #[cfg(unix)]
        {
            match wd.set_timeout(watchdog.timeout_s) {
                Ok(effective_timeout_s) => effective_timeout_s,
                Err(e) => {
                    warn!(error = %e, requested_timeout_s = watchdog.timeout_s,
                        "Failed to set watchdog timeout — kernel default is unknown; supervision math conservatively retains the requested value");
                    watchdog.timeout_s
                }
            }
        }
        #[cfg(not(unix))]
        {
            watchdog.timeout_s
        }
    };
    // NEW-4: immediate kick so the freshly-opened WDT starts from a full timeout
    // (the kicker's first interval tick is one kick_interval away).
    #[cfg(unix)]
    let _ = wd.kick();
    let kick_interval = watchdog.kick_interval_s as u64;
    info!(
        kick_interval_s = kick_interval,
        requested_timeout_s = watchdog.timeout_s,
        effective_timeout_s,
        "Watchdog armed (requested timeout={}s, effective timeout={}s, kick={}s) — hardware will auto-reboot if dcentrald stops responding",
        watchdog.timeout_s, effective_timeout_s, kick_interval,
    );
    // Expert review fix: Use the owned Watchdog struct with persistent fd.
    // Previous code used std::fs::write which opens+closes /dev/watchdog every
    // tick. Closing without magic byte 'V' can trigger reboot.
    // Safety-liveness gating: when a liveness counter is provided, only pet the
    // WDT while the supervised runtime loop is making progress. On the standard
    // path this is the thermal control loop; on Daemon::run-bypassing paths it is
    // the path-local thermal/runtime housekeeping loop. A deadlocked / livelocked
    // loop then STOPS feeding the WDT, so the SoC reboots instead of leaving
    // energized boards unsupervised. `stall_limit` is sized to ~half the WDT
    // window (above the normal tick cadence) so scheduler jitter can't trip it; a
    // fresh (0) counter is "not yet started" and does not gate.
    let kick_secs = watchdog_interval_secs(kick_interval);
    let stall_limit = watchdog_stall_limit(
        effective_timeout_s as u64,
        kick_secs,
        expected_liveness_interval,
    );
    Some(WatchdogKickerSetup {
        watchdog: wd,
        kick_secs,
        stall_limit,
    })
}

fn watchdog_kicker_loop(
    setup: WatchdogKickerSetup,
    shutdown: CancellationToken,
    safety_liveness: Option<Arc<AtomicU64>>,
    disarm_tx: Option<oneshot::Sender<WatchdogDisarmResult>>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    async move {
        let WatchdogKickerSetup {
            watchdog: wd,
            kick_secs,
            stall_limit,
        } = setup;
        let mut last_live: u64 = 0;
        let mut stalls: u64 = 0;
        // Use the panic-safe `kick_secs` (>= 1), NOT the raw `kick_interval`:
        // tokio::time::interval panics on a zero Duration, and a validation-
        // bypassing kick_interval of 0 would otherwise crash the daemon here and
        // leave the miner with no watchdog.
        let mut interval = tokio::time::interval(Duration::from_secs(kick_secs));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("Watchdog kicker stopping — sending magic close to disarm");
                    let disarm_result = wd.close_magic().map_err(|error| error.to_string());
                    match &disarm_result {
                        Ok(()) => info!("Watchdog magic close completed"),
                        Err(error) => error!(error, "Watchdog magic close failed; hardware watchdog remains armed"),
                    }
                    if let Some(disarm_tx) = disarm_tx {
                        let _ = disarm_tx.send(disarm_result);
                    }
                    return;
                }
                _ = interval.tick() => {
                    if let Some(ref live) = safety_liveness {
                        let cur = live.load(std::sync::atomic::Ordering::Relaxed);
                        let (should_kick, new_last, new_stalls) =
                            watchdog_kick_decision(cur, last_live, stalls, stall_limit);
                        last_live = new_last;
                        stalls = new_stalls;
                        if !should_kick {
                            error!(
                                stalls,
                                stall_limit,
                                "Watchdog safety liveness has not advanced for ~{}s — WITHHOLDING watchdog kick so the SoC reboots (supervised loop appears hung)",
                                stalls.saturating_mul(kick_secs)
                            );
                            continue; // do NOT kick — let the WDT fire
                        }
                    }
                    if let Err(e) = wd.kick() {
                        error!(error = %e, "Watchdog kick failed — if this persists, the SoC may reboot!");
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WatchdogIntent {
    Mining,
    Teardown { deadline: tokio::time::Instant },
    Disarm,
}

#[derive(Debug)]
enum WatchdogTaskReceipt {
    NotOpenedByDaemon,
    MagicCloseWriteCompleted,
    MagicCloseWriteFailed(String),
}

fn owned_watchdog_kicker(
    config: crate::config::WatchdogConfig,
    expected_liveness_interval: Duration,
    owner_shutdown: CancellationToken,
    mut intent_rx: watch::Receiver<WatchdogIntent>,
    safety_liveness: Arc<AtomicU64>,
    receipt_tx: oneshot::Sender<WatchdogTaskReceipt>,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    async move {
        // Open only after RuntimeTaskGuard has accepted and spawned this future.
        // Registration refusal therefore cannot leave an armed, unowned fd.
        let Some(setup) = prepare_watchdog_kicker(&config, Some(expected_liveness_interval)) else {
            let _ = receipt_tx.send(WatchdogTaskReceipt::NotOpenedByDaemon);
            return;
        };
        let WatchdogKickerSetup {
            watchdog: wd,
            kick_secs,
            stall_limit,
        } = setup;
        let mut last_live = 0_u64;
        let mut stalls = 0_u64;
        let mut interval = tokio::time::interval(Duration::from_secs(kick_secs));
        let mut receipt_tx = Some(receipt_tx);

        loop {
            tokio::select! {
                _ = owner_shutdown.cancelled() => {
                    warn!("Watchdog task owner cancelled without explicit Disarm; leaving hardware watchdog armed");
                    return;
                }
                changed = intent_rx.changed() => {
                    if changed.is_err() {
                        warn!("Watchdog intent owner disappeared without explicit Disarm; leaving hardware watchdog armed");
                        return;
                    }
                    if matches!(*intent_rx.borrow_and_update(), WatchdogIntent::Disarm) {
                        info!("Watchdog received explicit Disarm after bounded hardware teardown");
                        let receipt = match wd.close_magic() {
                            Ok(()) => WatchdogTaskReceipt::MagicCloseWriteCompleted,
                            Err(error) => WatchdogTaskReceipt::MagicCloseWriteFailed(error.to_string()),
                        };
                        if let Some(receipt_tx) = receipt_tx.take() {
                            let _ = receipt_tx.send(receipt);
                        }
                        return;
                    }
                }
                _ = interval.tick() => {
                    let should_kick = match *intent_rx.borrow() {
                        WatchdogIntent::Mining => {
                            let current = safety_liveness.load(Ordering::Relaxed);
                            let (should_kick, new_last, new_stalls) =
                                watchdog_kick_decision(current, last_live, stalls, stall_limit);
                            last_live = new_last;
                            stalls = new_stalls;
                            should_kick
                        }
                        WatchdogIntent::Teardown { deadline } => {
                            if watchdog_teardown_kick_allowed(
                                deadline,
                                tokio::time::Instant::now(),
                            ) {
                                true
                            } else {
                                error!(
                                    grace_s = WATCHDOG_TEARDOWN_GRACE.as_secs(),
                                    "Watchdog teardown deadline expired; WITHHOLDING kick so a wedged shutdown resets the SoC"
                                );
                                false
                            }
                        }
                        WatchdogIntent::Disarm => false,
                    };
                    if should_kick {
                        if let Err(error) = wd.kick() {
                            error!(error = %error, "Watchdog kick failed — if this persists, the SoC may reboot");
                        }
                    }
                }
            }
        }
    }
}

pub(crate) fn spawn_watchdog_kicker(
    watchdog: &crate::config::WatchdogConfig,
    shutdown: CancellationToken,
    safety_liveness: Option<Arc<AtomicU64>>,
) {
    if let Some(setup) = prepare_watchdog_kicker(watchdog, None) {
        tokio::spawn(watchdog_kicker_loop(setup, shutdown, safety_liveness, None));
    }
}

/// THERMAL-8 pure tick (non-XADC / Amlogic twin of THERMAL-7): update the
/// board-temp-blindness counter and decide whether to escalate to a fail-closed
/// `EmergencyShutdown`. Factored out of the thermal loop so it is unit-testable
/// without a running daemon (mirrors [`atm_step_ceiling_decision`]).
///
/// On a non-XADC platform (Amlogic) the control-board `die_temp` is a hardcoded
/// 45.0 °C FALLBACK, not a real readback, so THERMAL-7 (XADC-gated) can never
/// fire. The SOLE real thermal source is the per-chain board/chip-temp pipeline;
/// if it goes fully stale the controller would otherwise see a permanent
/// fake-cool 45 °C and mine with ZERO thermal proof — the non-XADC fail-OPEN
/// twin of THERMAL-7.
///
/// Returns `(new_consecutive_failures, escalate)`:
/// - A single tick with ANY real board-temp proof (or any XADC platform) RESETS
///   the counter to 0 and never escalates — so a single empty tick can NEVER
///   force a shutdown (respects "never emergency on empty board temps ALONE";
///   only sustained TOTAL blindness escalates).
/// - `escalate` is true only on sustained total blindness (`failures >= limit`)
///   AND when the action this tick is not already an emergency (never WEAKENs a
///   more-severe action already chosen).
/// - Strictly gated on `!has_xadc`, so the XADC (Zynq, beta-gating) path is
///   byte-identical (counter pinned at 0, never escalates).
fn thermal8_board_blind_tick(
    has_xadc: bool,
    had_board_temp_proof: bool,
    prev_failures: u32,
    limit: u32,
    action_is_emergency: bool,
) -> (u32, bool) {
    if has_xadc || had_board_temp_proof {
        return (0, false);
    }
    let failures = prev_failures.saturating_add(1);
    let escalate = failures >= limit && !action_is_emergency;
    (failures, escalate)
}

/// THERMAL-7 pure escalation decision (XADC-gated / Zynq twin of THERMAL-8):
/// should a blind-XADC tick force a fail-closed `EmergencyShutdown`?
///
/// The XADC die temp is the S9/Zynq last-resort thermal proof. When the XADC
/// read has FAILED, there is NO hashboard board-temp fallback covering for it,
/// and that has held for `limit` consecutive ticks, the controller has zero
/// thermal visibility — so escalate rather than loop forever on the benign 45 °C
/// fallback with the boards energized. Extracted from the thermal loop so it is
/// testable (parity with `thermal8_board_blind_tick`). It:
///   - only applies on an XADC platform (`has_xadc`);
///   - never escalates on a good XADC read (`xadc_failed == false`) or when a
///     real board temp covered this tick (`had_board_temp_proof`) — either
///     resets the streak, so a single blind tick can NEVER trip it;
///   - never WEAKENS a more-severe action already chosen (`action_is_emergency`).
fn thermal7_xadc_blind_escalates(
    has_xadc: bool,
    xadc_failed: bool,
    had_board_temp_proof: bool,
    consecutive_failures: u32,
    limit: u32,
    action_is_emergency: bool,
) -> bool {
    has_xadc
        && xadc_failed
        && !had_board_temp_proof
        && consecutive_failures >= limit
        && !action_is_emergency
}

const BOARD_TEMP_STUCK_IDENTICAL_TICKS: u32 = 12;
const ENV_THERMAL_INCLUDE_DIE_ON_AM2: &str = "DCENT_THERMAL_INCLUDE_DIE_ON_AM2";

fn thermal_die_crosscheck_enabled(
    has_xadc: bool,
    die_temp: f32,
    platform_marker: &str,
    am2_override: bool,
) -> bool {
    has_xadc
        && die_temp > 0.0
        && die_temp < 125.0
        && (!platform_marker.trim().starts_with("zynq-bm3-am2") || am2_override)
}

#[derive(Debug, Default, Clone, Copy)]
struct StuckBoardTempState {
    last_bits: u32,
    repeated: u32,
    warned: bool,
}

fn update_stuck_board_temp_sensor(
    state: &mut StuckBoardTempState,
    sample_bits: Option<u32>,
    threshold: u32,
) -> bool {
    let Some(bits) = sample_bits else {
        *state = StuckBoardTempState::default();
        return false;
    };
    if threshold == 0 {
        return false;
    }
    if state.last_bits == bits {
        state.repeated = state.repeated.saturating_add(1);
    } else {
        state.last_bits = bits;
        state.repeated = 1;
        state.warned = false;
    }
    if state.repeated >= threshold && !state.warned {
        state.warned = true;
        return true;
    }
    false
}

#[cfg(test)]
mod saf2_thermal_crosscheck_tests {
    use super::{
        thermal_die_crosscheck_enabled, update_stuck_board_temp_sensor, StuckBoardTempState,
    };

    #[test]
    fn die_crosscheck_is_default_off_on_am2_zynq() {
        assert!(!thermal_die_crosscheck_enabled(
            true,
            70.0,
            "zynq-bm3-am2",
            false
        ));
        assert!(thermal_die_crosscheck_enabled(
            true,
            70.0,
            "zynq-bm3-am2",
            true
        ));
        assert!(thermal_die_crosscheck_enabled(
            true,
            70.0,
            "zynq-bm1-s9",
            false
        ));
        assert!(!thermal_die_crosscheck_enabled(
            false,
            70.0,
            "amlogic-a113d",
            true
        ));
        assert!(!thermal_die_crosscheck_enabled(
            true,
            150.0,
            "zynq-bm1-s9",
            false
        ));
    }

    #[test]
    fn stuck_board_temp_warns_once_then_resets_on_change_or_missing() {
        let bits = 55.0_f32.to_bits();
        let mut state = StuckBoardTempState::default();
        assert!(!update_stuck_board_temp_sensor(&mut state, Some(bits), 3));
        assert!(!update_stuck_board_temp_sensor(&mut state, Some(bits), 3));
        assert!(update_stuck_board_temp_sensor(&mut state, Some(bits), 3));
        assert!(
            !update_stuck_board_temp_sensor(&mut state, Some(bits), 3),
            "same stuck run should warn once"
        );

        assert!(!update_stuck_board_temp_sensor(
            &mut state,
            Some(56.0_f32.to_bits()),
            3
        ));
        assert!(!update_stuck_board_temp_sensor(&mut state, None, 3));
        assert!(!update_stuck_board_temp_sensor(&mut state, Some(bits), 3));
    }
}

/// R-11: stable snake_case label for a thermal-supervisor reason, used in the
/// hardware-safety audit-log rows the thermal loop emits. Mirrors the
/// `#[serde(rename_all = "snake_case")]` names on `ThermalReason` so the audit
/// string matches the supervisor snapshot / telemetry vocabulary. Exhaustive
/// over `ThermalReason` so a new reason cannot silently drift out of sync.
fn thermal_reason_label(reason: dcentrald_thermal::supervisor::ThermalReason) -> &'static str {
    use dcentrald_thermal::supervisor::ThermalReason as R;
    match reason {
        R::BoardHot => "board_hot",
        R::BoardPanic => "board_panic",
        R::ChipHot => "chip_hot",
        R::ChipPanic => "chip_panic",
        R::HydroPanic => "hydro_panic",
        R::HydroStartupCold => "hydro_startup_cold",
        R::HydroFlowLoss => "hydro_flow_loss",
        R::FanPanic => "fan_panic",
        R::SensorFailure => "sensor_failure",
    }
}

/// Direction of an ATM (Advanced Thermal Management) profile-step advisory the
/// thermal supervisor emitted this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtmStepDir {
    /// `RequestProfileStepDown` — hot; lower the active profile (the SAFE
    /// cut-hash-before-noise direction).
    Down,
    /// `RequestProfileStepUp` — cool + post-grace; raise the active profile.
    Up,
}

/// Pure decision for the ATM (Advanced Thermal Management) frequency-step
/// ceiling, factored out of the thermal loop so it is unit-testable without a
/// running daemon.
///
/// Returns the NEW value for the `FrequencyLimitSource::AtmStep` ceiling, where
/// `None` means "no ATM constraint" (the autotuner runs up to its own
/// nominal/SKU max). The caller only emits a `SetFrequencyLimit` when the
/// returned value differs from `current`.
///
/// LOAD-BEARING SAFETY (mirrors the doc-comment at the dispatch site):
/// - **Step-DOWN is the safe direction** and is always honored (subject only to
///   the debounce window): it lowers the ceiling one `step_mhz`, floored at
///   `floor_mhz` so a runaway step-down can never drive the chain to an
///   unminable frequency, and never above `nominal_mhz`.
/// - **Step-UP is BOUNDED by `nominal_mhz`** (the configured operator/SKU max):
///   at/above nominal it CLEARS the ATM ceiling (`None`) — it never commands a
///   frequency above the configured maximum. An already-unconstrained ceiling
///   (`None`) stays `None` on step-up (cannot exceed nominal).
/// - **Thermal safety always wins:** when `cutting_hash` is true (the reconciled
///   thermal action this tick is an emergency / fan-failure / throttle / restart
///   response — i.e. a hot event), a step-UP is REFUSED (returns `current`
///   unchanged). A step-UP is also refused while `debounced`.
/// - This helper never touches voltage; the autotuner lowers voltage with
///   frequency through its PVT envelope, so an ATM step can never raise voltage
///   past the 14500 mV cap.
fn atm_step_ceiling_decision(
    dir: AtmStepDir,
    current: Option<u16>,
    nominal_mhz: u16,
    step_mhz: u16,
    floor_mhz: u16,
    cutting_hash: bool,
    debounced: bool,
) -> Option<u16> {
    match dir {
        AtmStepDir::Down => {
            if debounced {
                return current;
            }
            // From "no ceiling" the first step-down starts at nominal − one step.
            let base = current.unwrap_or(nominal_mhz);
            Some(
                base.saturating_sub(step_mhz)
                    .max(floor_mhz)
                    // Never let a degenerate config push the floor above nominal.
                    .min(nominal_mhz),
            )
        }
        AtmStepDir::Up => {
            // A hot event (hash being cut/throttled) or the debounce window
            // suppresses any step-up — thermal safety wins.
            if cutting_hash || debounced {
                return current;
            }
            match current {
                // Already unconstrained — cannot go above the configured max.
                None => None,
                Some(cur) => {
                    let next = cur.saturating_add(step_mhz);
                    // BOUNDED by nominal: at/above the configured max we clear
                    // the ATM constraint entirely (never command above max).
                    if next >= nominal_mhz {
                        None
                    } else {
                        Some(next)
                    }
                }
            }
        }
    }
}

/// Stage-A OBSERVE-ONLY DPS governor shadow env flag.
///
/// When this env var is truthy (`1`/`true`/`yes`/`on`) the daemon spawns a
/// read-only task that periodically feeds the built-but-not-driven
/// `dcentrald_autotuner::dps_governor::DpsGovernor` a `DpsTick` built from
/// EXISTING live state and LOGS the returned `DpsAction` — it NEVER acts on it
/// (no freq/power/fan/PSU command, no I2C/UART/GPIO). When unset (default) the
/// governor is never constructed and no task is spawned — byte-identical to the
/// prior behaviour. This is the same observe-only-shadow pattern used to study
/// the pool-failover FSM before it was allowed to drive. The Stage-B flip
/// (letting DPS actually scale power) is separately soak- and operator-gated.
const ENV_DPS_GOVERNOR_SHADOW: &str = "DCENT_DPS_GOVERNOR_SHADOW";

/// True iff the observe-only DPS-governor shadow is enabled via
/// [`ENV_DPS_GOVERNOR_SHADOW`]. Reuses the shared autotuner env-truthiness
/// helper so the daemon and tests agree on what counts as "on".
fn dps_governor_shadow_enabled() -> bool {
    std::env::var(ENV_DPS_GOVERNOR_SHADOW)
        .ok()
        .map(|v| dcentrald_autotuner::config::env_flag_is_truthy(&v))
        .unwrap_or(false)
}

/// PERF-004: env gate that opts the am2/BM1362 autotuner into a SKU-aware
/// frequency ceiling. Default-OFF: when unset the daemon pins the historical
/// Standard 545-MHz ceiling for EVERY am2/BM1362 board (byte-identical to the
/// proven `a lab unit`/`a lab unit` behavior). When set (`1`/`true`/`yes`/`on`) the daemon
/// classifies the live hashboard SKU label and widens the ceiling to that SKU
/// class (mid-band/high-bin → 597). An unknown/standard SKU still resolves to
/// `Standard`, so the gate can never auto-promote a board the EEPROM does not
/// corroborate. Mirrors the `DCENT_AM2_FREQUENCY_AUTOTUNE` opt-in discipline.
const ENV_AM2_SKU_AWARE_CEILING: &str = "DCENT_AM2_SKU_AWARE_CEILING";

/// True iff the PERF-004 SKU-aware ceiling is opted in via
/// [`ENV_AM2_SKU_AWARE_CEILING`]. Reuses the shared autotuner env-truthiness
/// helper so the daemon and tests agree on what counts as "on".
fn am2_sku_aware_ceiling_enabled() -> bool {
    std::env::var(ENV_AM2_SKU_AWARE_CEILING)
        .ok()
        .map(|v| dcentrald_autotuner::config::env_flag_is_truthy(&v))
        .unwrap_or(false)
}

/// Map an ASIC chip-id to the documented DPS per-family thermal profile the
/// `DpsGovernor` reasons about (target/hot/dangerous from the BraiinsOS RE
/// doc §6). Used ONLY by the observe-only DPS shadow so the shadow's
/// "what would DPS decide" output uses the same thresholds the real DPS
/// governor would. Unknown chips fall back to the S19 family (the most
/// common am2/am3 case) — acceptable because the shadow only logs.
fn dps_thermal_profile_for_chip(
    chip_id: u16,
) -> dcentrald_api_types::braiinsos_dps_configuration::DpsThermalProfile {
    use dcentrald_api_types::braiinsos_dps_configuration::DpsThermalProfile;
    match chip_id {
        0x1387 => DpsThermalProfile::S9,
        0x1396 | 0x1397 => DpsThermalProfile::S17Family,
        0x1398 | 0x1362 | 0x1366 => DpsThermalProfile::S19Family,
        0x1368 | 0x1370 => DpsThermalProfile::S21Family,
        _ => DpsThermalProfile::S19Family,
    }
}

/// Stage-A OBSERVE-ONLY TunerDriver shadow env flag.
///
/// When this env var is truthy (`1`/`true`/`yes`/`on`) the daemon spawns a
/// read-only task that periodically feeds the built-but-not-driven
/// `crate::autotune::TunerDriver` (the 6-variant `TunerMode` strategy driver)
/// a `TelemetrySample` built from EXISTING live state and LOGS the returned
/// `TunerOutcome` — it NEVER acts on it (no freq/voltage/power/fan/PSU command,
/// no setter, no I2C/UART/GPIO). When unset (default) the driver is never
/// constructed and no task is spawned — byte-identical to the prior behaviour.
/// This mirrors the observe-only-shadow pattern shipped for the `DpsGovernor`
/// (`DCENT_DPS_GOVERNOR_SHADOW`). Letting the TunerDriver actually drive
/// frequency/voltage is the existing live `AutoTuner` path — wholly separate,
/// operator-gated, and untouched by this shadow.
const ENV_TUNER_DRIVER_SHADOW: &str = "DCENT_TUNER_DRIVER_SHADOW";

/// True iff the observe-only TunerDriver shadow is enabled via
/// [`ENV_TUNER_DRIVER_SHADOW`]. Reuses the shared autotuner env-truthiness
/// helper so the daemon and tests agree on what counts as "on".
fn tuner_driver_shadow_enabled() -> bool {
    std::env::var(ENV_TUNER_DRIVER_SHADOW)
        .ok()
        .map(|v| dcentrald_autotuner::config::env_flag_is_truthy(&v))
        .unwrap_or(false)
}

/// Stage-A OBSERVE-ONLY VnishPhaseAdapter shadow env flag.
///
/// When this env var is truthy (`1`/`true`/`yes`/`on`) the daemon spawns a
/// read-only task that periodically feeds the built-but-not-driven
/// `dcentrald_autotuner::vnish_phase_fsm::VnishPhaseAdapter` (the VNish-style
/// 5-phase autotune FSM) an `AutotuneObservation` built from EXISTING live
/// state and LOGS the returned `VnishTuneAction` — it NEVER acts on it (no
/// freq/voltage/power/fan/PSU command, no setter, no I2C/UART/GPIO). When unset
/// (default) the adapter is never constructed and no task is spawned —
/// byte-identical to the prior behaviour. This mirrors the observe-only-shadow
/// pattern shipped for the `DpsGovernor` (`DCENT_DPS_GOVERNOR_SHADOW`) and the
/// `TunerDriver` (`DCENT_TUNER_DRIVER_SHADOW`); it studies the 5-phase
/// voltage-walk strategy, orthogonal to those two. Letting the VNish phase FSM
/// actually drive frequency/voltage is a separate, soak- + operator-gated
/// Stage-B flip (the adapter's own `[autotune.vnish_phase].enabled` TOML gate),
/// untouched by this shadow.
const ENV_VNISH_PHASE_SHADOW: &str = "DCENT_VNISH_PHASE_SHADOW";

/// True iff the observe-only VnishPhaseAdapter shadow is enabled via
/// [`ENV_VNISH_PHASE_SHADOW`]. Reuses the shared autotuner env-truthiness
/// helper so the daemon and tests agree on what counts as "on".
fn vnish_phase_shadow_enabled() -> bool {
    std::env::var(ENV_VNISH_PHASE_SHADOW)
        .ok()
        .map(|v| dcentrald_autotuner::config::env_flag_is_truthy(&v))
        .unwrap_or(false)
}

fn discover_uio_number_by_name(wanted_name: &str) -> Option<u8> {
    let entries = std::fs::read_dir("/sys/class/uio").ok()?;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(num_str) = file_name.strip_prefix("uio") else {
            continue;
        };
        let Ok(num) = num_str.parse::<u8>() else {
            continue;
        };
        let name_path = entry.path().join("name");
        let Ok(name) = std::fs::read_to_string(name_path) else {
            continue;
        };
        if name.trim() == wanted_name {
            return Some(num);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Platform-default constants
// ---------------------------------------------------------------------------
//
// These are S9 fallbacks used by `Daemon::*_for_board()` and friends BEFORE
// chip-id detection completes, after which `MinerProfile` overrides them.
// The constants used to live as a contiguous block in this file; an earlier
// W2.1 extraction wave accidentally removed them. They were restored as
// part of the W2.1 + W2.2 + W2.5 closure pass (2026-05-07) so the workspace
// builds clean again.
//
// Note: `FAN_UIO` is canonical in `crate::fpga::FAN_UIO` — import that one
// rather than re-defining here, to keep the FPGA register/UIO map in a
// single source of truth.

/// Default GPIO pin numbers for hash board plug detect (S9 fallback).
/// Overridden by `MinerProfile` when chip type is detected.
const DEFAULT_PLUGO_GPIO_BASE: u32 = 902;

/// Default GPIO pin numbers for hash board enable (S9 fallback).
/// Overridden by `MinerProfile` when chip type is detected.
const DEFAULT_ENABLE_GPIO_BASE: u32 = 893;

/// Default PIC I2C addresses (S9 fallback: chains 6, 7, 8).
/// Overridden by `MinerProfile` when chip type is detected.
/// For dsPIC models (S17/S19), these are replaced by probed dsPIC addresses.
/// For NoPic models (S21), this is empty.
const DEFAULT_PIC_ADDRS: [u8; 3] = [0x55, 0x56, 0x57];

/// Default chain IDs (S9 fallback: connector numbering).
/// Overridden by `MinerProfile` when chip type is detected.
const DEFAULT_CHAIN_IDS: [u8; 3] = [6, 7, 8];

/// Default UIO device base numbers for each chain (S9 fallback).
/// S9 verified: uio1-4=chain6, uio5-8=chain7, uio9-12=chain8.
/// Overridden by `MinerProfile` when chip type is detected.
const DEFAULT_UIO_BASES: [u8; 3] = [1, 5, 9];

/// Default I2C bus number for PIC controllers (S9 = bus 0).
/// Overridden by `MinerProfile` when chip type is detected.
const DEFAULT_I2C_BUS: u8 = 0;

/// The standard Zynq daemon has three physical hash-board connectors and its
/// GPIO/UIO lifecycle is written around those three slots. Four-chain and
/// non-UIO platforms require their platform-specific daemon instead of being
/// admitted through truncated or placeholder tables.
const STANDARD_DAEMON_BOARD_SLOTS: usize = 3;

// ---------------------------------------------------------------------------
// Per-PIC heartbeat back-off state machine (WAVE-0 STABILIZE, 2026-06-05)
// ---------------------------------------------------------------------------
//
// ROOT CAUSE (live S9 audit `s9-live-audit-20260605`, finding B2 / N7):
// when a chain's PIC is electrically dead (or the whole I2C bus NACKs because
// 12V is absent / the AXI-IIC controller is wedged), the heartbeat loop kept
// calling `heartbeat()` on that address EVERY tick FOREVER. Each failing call
// drives the HAL devmem retry + SCL-clock-recovery path, so a single dead PIC
// is hammered ~33×/s and emits a `DIAG_HB ... FAIL` + `I2C bus recovered via
// SCL clock recovery` line on every attempt (the captured 13 s log is 19,704+
// NACK lines — the entire log ring is this one storm).
//
// The CLAUDE-documented contract ("AXI IIC Controller Stuck State": *skip
// after 10 failures, probe every 30s, declare dead*) was NEVER implemented in
// the heartbeat loop — `daemon.rs` only incremented a counter and logged. This
// state machine implements that contract. It is a pure, host-testable struct
// (no HAL deps) so the back-off logic is unit-tested off-hardware.
//
// SAFETY: back-off only changes WHEN we bother to poke a NACKing PIC; it never
// suppresses the voltage-cut safety response. A PIC declared Dead has already
// failed continuously — its hardware watchdog has long since cut its own rail,
// and the daemon's separate thermal/heartbeat-stability gates still apply. We
// keep reprobing forever (just at a slow cadence) so a board that is re-seated
// or re-powered is automatically picked back up.

/// Consecutive heartbeat failures before a PIC is moved out of the hot path.
const PIC_BACKOFF_FAIL_THRESHOLD: u32 = 10;

/// Reprobe interval (seconds) for a PIC that is backing off / declared dead.
/// Matches the CLAUDE "probe every 30s" contract.
const PIC_BACKOFF_REPROBE_SECS: u64 = 30;

/// How many tick-aligned NACK log lines to emit before rate-limiting kicks in.
/// We always log the first few of a fresh failure streak (so an operator sees
/// the onset), then go quiet until a state transition or the periodic reprobe.
const PIC_BACKOFF_LOG_BURST: u32 = 3;

/// Lifecycle state of a single PIC's heartbeat, per `PicBackoff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PicHbState {
    /// Heartbeating normally; the PIC is answering (or just started failing).
    Active,
    /// Failed `>= PIC_BACKOFF_FAIL_THRESHOLD` consecutive times; skipped in the
    /// hot path, reprobed every `PIC_BACKOFF_REPROBE_SECS`.
    BackingOff,
    /// A reprobe (after back-off) also failed; treated as dead but still
    /// reprobed at the same slow cadence so a re-seated board recovers.
    Dead,
}

/// What the heartbeat loop should do with one PIC THIS tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HbAction {
    /// Send a heartbeat normally (hot path).
    Beat,
    /// Skip silently — the PIC is backed off and not due for a reprobe.
    Skip,
    /// Send a heartbeat as a reprobe (PIC is backed off / dead but due).
    Reprobe,
}

/// Per-PIC heartbeat back-off state machine.
///
/// Pure logic — `now_secs` is injected by the caller (monotonic seconds) so the
/// whole thing is deterministically unit-testable without a clock. One instance
/// per PIC address; the heartbeat loop owns a `HashMap<u8, PicBackoff>`.
#[derive(Debug, Clone)]
pub(crate) struct PicBackoff {
    state: PicHbState,
    /// Consecutive failures since the last success.
    consecutive_failures: u32,
    /// Monotonic second at which the next reprobe is allowed (in back-off/dead).
    next_reprobe_at: u64,
}

impl Default for PicBackoff {
    fn default() -> Self {
        Self {
            state: PicHbState::Active,
            consecutive_failures: 0,
            next_reprobe_at: 0,
        }
    }
}

impl PicBackoff {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn state(&self) -> PicHbState {
        self.state
    }

    pub(crate) fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Is this PIC currently being treated as healthy (Active and not failing)?
    /// Used to gate the deferred-voltage "stable heartbeat window" — a PIC in
    /// back-off must NOT count as a stable tick.
    pub(crate) fn is_healthy(&self) -> bool {
        self.state == PicHbState::Active && self.consecutive_failures == 0
    }

    /// Decide what to do with this PIC this tick. Does NOT mutate the failure
    /// counters (those are updated by `record_success`/`record_failure` once
    /// the heartbeat actually runs); it only decides whether to poke the bus.
    pub(crate) fn decide(&self, now_secs: u64) -> HbAction {
        match self.state {
            PicHbState::Active => HbAction::Beat,
            PicHbState::BackingOff | PicHbState::Dead => {
                if now_secs >= self.next_reprobe_at {
                    HbAction::Reprobe
                } else {
                    HbAction::Skip
                }
            }
        }
    }

    /// Record a successful heartbeat. Returns true if this is a recovery
    /// (the PIC was previously failing / backed off) so the caller can log it.
    pub(crate) fn record_success(&mut self) -> bool {
        let was_unhealthy = self.state != PicHbState::Active || self.consecutive_failures > 0;
        self.state = PicHbState::Active;
        self.consecutive_failures = 0;
        self.next_reprobe_at = 0;
        was_unhealthy
    }

    /// Record a failed heartbeat at monotonic `now_secs`. `was_reprobe` is true
    /// if the failed attempt was a scheduled reprobe (vs a hot-path beat).
    ///
    /// Returns whether this failure should be LOGGED (rate-limited): we log the
    /// first `PIC_BACKOFF_LOG_BURST` of a fresh streak, every state transition,
    /// and every reprobe failure (one per ~30s — cheap, and shows liveness).
    pub(crate) fn record_failure(&mut self, now_secs: u64, was_reprobe: bool) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);

        match self.state {
            PicHbState::Active => {
                if self.consecutive_failures >= PIC_BACKOFF_FAIL_THRESHOLD {
                    // Transition Active -> BackingOff. Always log the transition.
                    self.state = PicHbState::BackingOff;
                    self.next_reprobe_at = now_secs.saturating_add(PIC_BACKOFF_REPROBE_SECS);
                    true
                } else {
                    // Still in the hot path — log only the first few of the streak.
                    self.consecutive_failures <= PIC_BACKOFF_LOG_BURST
                }
            }
            PicHbState::BackingOff | PicHbState::Dead => {
                // Schedule the next reprobe regardless.
                self.next_reprobe_at = now_secs.saturating_add(PIC_BACKOFF_REPROBE_SECS);
                if was_reprobe {
                    // A reprobe that failed confirms (or keeps) the PIC dead.
                    // Either way we log it: a reprobe only happens once per
                    // ~PIC_BACKOFF_REPROBE_SECS, so this is at most one line per
                    // ~30s — a cheap liveness heartbeat of the ongoing fault, not
                    // a storm. The BackingOff->Dead transition is included.
                    self.state = PicHbState::Dead;
                    true
                } else {
                    // A non-reprobe failure while backed off should not even have
                    // been attempted (decide() returned Skip); never log it.
                    false
                }
            }
        }
    }
}

// Notification, AlertEvent, and ShareEfficiencyTracker definitions moved
// to crate::runtime::* (W2.1, 2026-05-07). See `runtime::notifications`
// and `runtime::efficiency`. Re-imported via `use` statements above.

// SV2 Job Declaration helpers moved to `crate::runtime::job_declaration`
// (W2.1 follow-up extraction, 2026-05-07). Re-imported via `use` statements
// at the top of this file.
pub(crate) use crate::runtime::job_declaration::{
    initial_job_declaration_status, job_declaration_config_to_sv2, spawn_job_declaration_supervisor,
};

/// Assert a thread's stop flag and wait only until `timeout` for it to finish.
///
/// `JoinHandle::join()` has no timeout and a PIC heartbeat can be blocked behind
/// a wedged kernel I2C request. Polling `is_finished()` keeps teardown bounded;
/// dropping an unfinished handle detaches it, while the asserted stop flag
/// prevents another heartbeat if the kernel call eventually returns.
async fn stop_thread_bounded(
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: std::thread::JoinHandle<()>,
    timeout: Duration,
) -> ThreadStopOutcome {
    stop.store(true, std::sync::atomic::Ordering::Release);
    join_thread_bounded(handle, timeout).await
}

#[cfg(test)]
mod init_heartbeat_ownership_tests {
    use super::{stop_thread_bounded, Daemon, InitializedPicAddrs, ThreadStopOutcome};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn bounded_stop_signals_and_joins_a_responsive_init_heartbeat() {
        let stop = Arc::new(AtomicBool::new(false));
        let observed_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_observed = observed_stop.clone();
        let handle = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            thread_observed.store(true, Ordering::Release);
        });

        assert_eq!(
            stop_thread_bounded(stop, handle, Duration::from_secs(1)).await,
            ThreadStopOutcome::Joined
        );
        assert!(observed_stop.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn bounded_stop_classifies_a_panicked_thread() {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = std::thread::spawn(|| panic!("intentional test panic"));
        assert_eq!(
            stop_thread_bounded(stop, handle, Duration::from_secs(1)).await,
            ThreadStopOutcome::Panicked
        );
    }

    #[tokio::test]
    async fn bounded_stop_times_out_without_waiting_for_a_stalled_thread() {
        let stop = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let thread_release = release.clone();
        let handle = std::thread::spawn(move || {
            while !thread_release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        assert_eq!(
            stop_thread_bounded(stop, handle, Duration::from_millis(20)).await,
            ThreadStopOutcome::TimedOut
        );
        release.store(true, Ordering::Release);
    }

    #[test]
    fn initialized_pic_state_recovers_poison_without_losing_ownership() {
        let state = InitializedPicAddrs::new(vec![0x20]);
        let poison_target = state.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poison_target.0.lock().unwrap();
            panic!("intentional initialized PIC state poison");
        });

        assert_eq!(state.snapshot("poison recovery test snapshot"), vec![0x20]);
        assert!(!state.0.is_poisoned());
        state.record_success(0x21, "poison recovery test update");
        assert_eq!(
            state.snapshot("poison recovery test final snapshot"),
            vec![0x20, 0x21]
        );
    }

    #[test]
    fn production_initialized_pic_state_has_no_direct_lock_unwrap() {
        let source = include_str!("daemon.rs");
        let forbidden = ["initialized_pic_addrs", ".lock()", ".unwrap()"].concat();
        assert!(!source.contains(&forbidden));
    }

    #[cfg(feature = "sim-hal")]
    #[tokio::test]
    async fn sim_init_heartbeat_stall_is_bounded_and_watchdog_removes_power() {
        use super::{PicFirmware, PicType};
        use dcentrald_hal::platform::sim::{SimModel, SimPlatform};

        let platform = SimPlatform::new(SimModel::S19Pro);
        let service = platform.open_i2c_service(0).unwrap();
        service
            .write_bytes(0x20, &[0x55, 0xAA, 0x15, 0x01])
            .unwrap();
        platform
            .configure_controller_watchdog(Duration::from_millis(50))
            .unwrap();
        platform.arm_next_i2c_transfer_stall().unwrap();

        let (stop, _pause, handle) = Daemon::start_init_heartbeat_thread(
            InitializedPicAddrs::new(vec![0x20]),
            PicFirmware::Unknown,
            PicType::DsPic33EP,
            service,
        )
        .unwrap();
        assert!(platform
            .wait_for_i2c_transfer_stall(Duration::from_secs(1))
            .unwrap());

        assert_eq!(
            stop_thread_bounded(stop, handle, Duration::from_millis(20)).await,
            ThreadStopOutcome::TimedOut
        );
        platform
            .advance_i2c_time(Duration::from_millis(50))
            .unwrap();
        assert!(platform.controller_watchdog_expired().unwrap());
        assert!(!platform.i2c_voltage_enabled().unwrap());

        platform.release_i2c_transfer_stall().unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!platform.i2c_voltage_enabled().unwrap());
    }
}

/// Top-level daemon state machine.
pub struct Daemon {
    config: DcentraldConfig,
    config_path: String,
    /// Immutable identity already used by the top-level route admission.
    /// Never reread board-target/platform files after hardware authority is minted.
    platform_identity: crate::daemon_lifecycle::PlatformIdentitySnapshot,
    shutdown_token: CancellationToken,
    /// Sole owner of asynchronous mining hardware tasks. Its standalone token
    /// stops work dispatch and thermal actuation only after the watchdog enters
    /// teardown grace; the management API token remains independent.
    mining_tasks: RuntimeTaskGuard,
    /// Independent fail-closed owner for the standard-path SoC watchdog. Owner
    /// cancellation never means magic close; only an explicit Disarm intent
    /// after bounded hardware teardown may produce a positive receipt.
    watchdog_tasks: RuntimeTaskGuard,
    watchdog_intent_tx: Option<watch::Sender<WatchdogIntent>>,
    watchdog_receipt_rx: Option<oneshot::Receiver<WatchdogTaskReceipt>>,
    /// Shutdown consumes hardware ownership and is not retry-safe. In
    /// particular, a retry must not extend the watchdog teardown deadline.
    shutdown_attempted: bool,
    /// Active mining chains (initialized during init phase, moved to WorkDispatcher in run).
    chains: Vec<Chain>,
    /// Fan controller (initialized during init phase, shared via Arc).
    fan: Option<Arc<FanController>>,
    /// GPIO controller for direct AXI register access (board enable, plug detect, LEDs).
    /// Wrapped in Arc so the LED engine task and daemon can share it.
    gpio: Option<Arc<GpioController>>,
    /// Channel sender to the LED engine task.
    led_tx: Option<mpsc::Sender<LedCommand>>,
    /// Watch receiver for live LED engine status (passed to API layer).
    led_status_rx: Option<watch::Receiver<dcentrald_hal::led::LedStatus>>,
    /// Detected chip ID from enumeration (e.g., 0x1387 for BM1387).
    chip_id: u16,
    /// Indices of detected boards (0, 1, 2 mapping to chains 6, 7, 8).
    detected_board_indices: Vec<usize>,
    /// Detected PIC firmware type (same across all boards on one S9).
    pic_firmware: PicFirmware,
    /// PIC addresses that successfully completed initialization (hot or cold start).
    /// Used to build the mining heartbeat list — excludes PICs that never responded.
    initialized_pic_addrs_final: Vec<u8>,
    /// Detected miner profile (populated after chip enumeration).
    /// Contains all model-specific constants (PIC addrs, chain IDs, voltage range, etc.).
    /// Falls back to S9 defaults when no profile is detected.
    miner_profile: Option<&'static MinerProfile>,
    /// Sealed standard-route plan minted before new-generation hardware access.
    /// Safe-off/cleanup paths intentionally do not depend on this authority.
    standard_composition_admission: Option<StandardHardwareCompositionAdmission>,
    /// Exact driver-execution policy carried into Phase 7 and WorkDispatcher.
    asic_driver_execution_policy: ChipDriverExecutionPolicy,
    /// Separate token for heartbeat thread shutdown. This is NOT the same as shutdown_token.
    /// The heartbeat thread must keep running DURING graceful shutdown (while voltage is
    /// being disabled) and only stop AFTER voltage is safely off. The mining owner
    /// is cancelled explicitly during shutdown; this heartbeat token is cancelled
    /// later, after disable_voltage.
    heartbeat_shutdown_token: CancellationToken,
    /// v0.13.0: Single I2C service handle — the ONLY path to /dev/i2c-0.
    /// Spawned at the very start of init(), before Phase 0 emergency heartbeats.
    /// ALL I2C operations (init, heartbeat, shutdown) go through this handle.
    /// Matches BraiinsOS's AsyncI2cDev pattern: 1 fd, 1 thread, mpsc channel.
    i2c_service: Option<dcentrald_hal::i2c::I2cServiceHandle>,
    /// Read-only I2C/sysfs observations captured before any runtime fabric
    /// service exists. Runtime policy consumes these immutable values rather
    /// than issuing hidden kernel-adapter transfers through sysfs.
    bootstrap_eeprom_fingerprints: Vec<Option<String>>,
    bootstrap_eeprom_preambles: Vec<Option<[u8; 2]>>,
    bootstrap_hb_type: Option<String>,
    resolved_pic_type: Option<PicType>,
    /// Initialization-only PIC heartbeat ownership. These fields are installed
    /// immediately after the OS thread is spawned, rather than returned only
    /// after `init()` succeeds. If the bring-up future is cancelled or times
    /// out, the lifecycle can therefore stop the thread and let the hardware
    /// watchdog expire instead of orphaning a heartbeat producer.
    init_heartbeat_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    init_heartbeat_handle: Option<std::thread::JoinHandle<()>>,
    /// Runtime heartbeat ownership. The cancellation token is the stop
    /// authority; retaining the handle lets shutdown distinguish a clean join,
    /// panic, and bounded detach when I2C is wedged.
    runtime_heartbeat_handle: Option<std::thread::JoinHandle<()>>,
    /// Sole owner of the legacy smart-PSU watchdog feeder. Its cancellation
    /// token is independent of the mining token so shutdown can first quiesce
    /// this raw bus-1 owner, then touch shared shutdown hardware.
    psu_watchdog_threads: RuntimeThreadGuard,
    /// Preflight could not prove the state of already-energized hardware.
    /// Shutdown must therefore report watchdog/fallback reliance even when no
    /// positively detected slots are available for addressed software disable.
    preflight_hardware_state_unknown: bool,
    /// Runtime voltage command sender serviced by the heartbeat/I2C thread.
    /// Shutdown and thermal safety use this to avoid opening a second /dev/i2c-0 fd.
    // SAFETY (wave 8, 2026-04-28): bounded sync_channel (capacity 64). Previously
    // an unbounded mpsc — a stalled I2C worker would let the queue grow without
    // limit, OOMing the daemon. With a bounded channel, senders use try_send and
    // drop+log on Full so we get back-pressure visibility instead of silent RAM
    // growth. Capacity 64 is generous — voltage commands are rate-limited by the
    // I2C bus (~10ms/cmd) so 64 entries == ~640ms of backlog, well under the
    // observable thermal time constant.
    voltage_cmd_tx: Option<VoltageCommandSender>,
    /// Generation authority for measured ASIC identity publication. It is
    /// daemon-owned so a future dispatcher replacement invalidates older ports.
    dispatcher_composition_authority: DispatcherCompositionAuthority,
    /// Successful GetAddress results from this initialization generation.
    /// Assumed model/profile fields never mint entries in this receipt set.
    asic_enumeration_receipts: Vec<EnumeratedMiningChainReceipt>,
}

/// Production adapter for the injectable standard-daemon lifecycle boundary.
///
/// Keeping this adapter next to `Daemon` preserves private access to the legacy
/// hardware implementation while the coordinator itself remains independently
/// executable with simulated platforms and clocks.
struct StandardPlatformLifecycle<'a> {
    daemon: &'a mut Daemon,
}

struct BootProgressRecoveryPublisher {
    boot_progress: Arc<dcentrald_api::BootProgressSnapshot>,
}

impl crate::daemon_lifecycle::PlatformLifecycle for StandardPlatformLifecycle<'_> {
    async fn initialize_platform(
        &mut self,
        identity: &crate::daemon_lifecycle::PlatformIdentitySnapshot,
    ) -> Result<()> {
        self.daemon.init(identity).await
    }

    fn stop_initialization_keepalives(&mut self) {
        self.daemon.signal_init_heartbeat_stop();
    }

    async fn safe_off_partial_platform(&mut self) -> Result<()> {
        self.daemon.shutdown().await
    }

    async fn run_management_only(&mut self) -> Result<()> {
        self.daemon.run_api_only().await
    }
}

impl crate::daemon_lifecycle::RecoveryPublisher for BootProgressRecoveryPublisher {
    fn publish_management_recovery(&mut self) -> Result<()> {
        self.boot_progress
            .record_now(dcentrald_api_types::firmware_boot_timeline::BootPhase::ServicesStart);
        Ok(())
    }
}

impl Daemon {
    /// Create a new daemon instance with the given configuration.
    pub fn new(
        config: DcentraldConfig,
        config_path: String,
        platform_identity: crate::daemon_lifecycle::PlatformIdentitySnapshot,
        shutdown_token: CancellationToken,
    ) -> Self {
        // Mining hardware ownership is intentionally independent of the global
        // signal/API token. shutdown() must first move the SoC watchdog into its
        // bounded teardown grace, then explicitly stop these tasks. A child
        // token would propagate SIGTERM early and freeze thermal liveness before
        // the watchdog knows teardown is intentional.
        let mining_tasks = RuntimeTaskGuard::new(CancellationToken::new());
        let watchdog_tasks = RuntimeTaskGuard::new(CancellationToken::new());
        Self {
            config,
            config_path,
            platform_identity,
            shutdown_token,
            mining_tasks,
            watchdog_tasks,
            watchdog_intent_tx: None,
            watchdog_receipt_rx: None,
            shutdown_attempted: false,
            chains: Vec::new(),
            fan: None,
            gpio: None,
            led_tx: None,
            led_status_rx: None,
            chip_id: 0,
            detected_board_indices: Vec::new(),
            pic_firmware: PicFirmware::Unknown,
            initialized_pic_addrs_final: Vec::new(),
            miner_profile: None,
            standard_composition_admission: None,
            asic_driver_execution_policy: ChipDriverExecutionPolicy::production_only(),
            heartbeat_shutdown_token: CancellationToken::new(),
            i2c_service: None,
            bootstrap_eeprom_fingerprints: Vec::new(),
            bootstrap_eeprom_preambles: Vec::new(),
            bootstrap_hb_type: None,
            resolved_pic_type: None,
            init_heartbeat_stop: None,
            init_heartbeat_handle: None,
            runtime_heartbeat_handle: None,
            psu_watchdog_threads: RuntimeThreadGuard::new(CancellationToken::new()),
            preflight_hardware_state_unknown: false,
            voltage_cmd_tx: None,
            dispatcher_composition_authority: DispatcherCompositionAuthority::default(),
            asic_enumeration_receipts: Vec::new(),
        }
    }

    fn td003_destructive_write_refusal(
        &self,
        identity: &crate::daemon_lifecycle::PlatformIdentitySnapshot,
    ) -> Option<Td003DestructiveWriteRefusal> {
        td003_destructive_write_refusal_from_signals(
            self.config.mining.model.as_deref(),
            identity.board_target(),
            identity.platform_marker(),
            identity.subtype(),
        )
    }

    async fn run_api_only(&mut self) -> Result<()> {
        info!(
            "Mining auto-start disabled — skipping hardware bring-up and starting dashboard/API in idle-first mode"
        );

        let version = std::fs::read_to_string("/etc/dcentos-version")
            .unwrap_or_else(|_| "unknown".to_string())
            .trim()
            .to_string();

        let initial_mode = dcentrald_api::OperatingMode::from_config_str(&self.config.mode.active);

        let initial_state = dcentrald_api::MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: Vec::new(),
            fans: dcentrald_api::FanState {
                pwm: 10,
                rpm: 0,
                per_fan: Vec::new(),
            },
            pool: dcentrald_api::PoolState {
                url: self.config.pool.url.clone(),
                worker: self.config.pool.worker.clone(),
                status: "Disabled".to_string(),
                difficulty: 0.0,
                last_share_at: 0,
                protocol: self
                    .config
                    .pool
                    .protocol
                    .clone()
                    .unwrap_or_else(|| "sv1".to_string()),
                encrypted: matches!(
                    self.config.pool.protocol.as_deref(),
                    Some("sv2") | Some("v2")
                ),
                encrypted_source: if matches!(
                    self.config.pool.protocol.as_deref(),
                    Some("sv2") | Some("v2")
                ) {
                    dcentrald_api::pool_quality_config_source()
                } else {
                    dcentrald_api::pool_quality_honest_default_source()
                },
                sv2_session: None,
                sv2_session_source: dcentrald_api::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: dcentrald_api::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: dcentrald_api::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: dcentrald_api::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: dcentrald_api::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: dcentrald_api::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: dcentrald_api::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: dcentrald_api::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 0,
            firmware_version: version,
            mode: initial_mode,
        };

        let (_state_tx, state_rx) = watch::channel(initial_state);
        let (_mode_tx, mode_rx) = watch::channel(initial_mode);
        let (stats_broadcast_tx, _) = broadcast::channel::<String>(64);
        let (mining_sync_broadcast_tx, _) = broadcast::channel::<String>(256);
        let mining_pipeline_snapshot_rx = if self.config.mining.pipeline_snapshot.enabled {
            Some(
                dcentrald_api::mining_pipeline_snapshot::spawn_mining_pipeline_snapshot_publisher(
                    &mining_sync_broadcast_tx,
                    self.config.mining.pipeline_snapshot.stale_after_ms,
                ),
            )
        } else {
            None
        };
        let (diag_broadcast_tx, _) =
            broadcast::channel::<dcentrald_diagnostics::progress::DiagnosticProgress>(32);
        let (autotuner_broadcast_tx, _) = broadcast::channel::<String>(64);
        let (_power_tx, power_rx) =
            watch::channel(dcentrald_autotuner::LivePowerEstimate::default());
        let (_autotuner_status_tx, autotuner_status_rx) =
            watch::channel(dcentrald_autotuner::AutotunerRuntimeStatus::default());
        let (_autotuner_efficiency_tx, autotuner_efficiency_rx) =
            watch::channel(None::<dcentrald_autotuner::EfficiencySnapshot>);
        let (_autotuner_chip_health_tx, autotuner_chip_health_rx) =
            watch::channel(None::<dcentrald_autotuner::LiveChipHealthState>);
        let (_autotuner_telemetry_tx, autotuner_telemetry_rx) =
            watch::channel(dcentrald_autotuner::TelemetryExportState::default());
        let (jd_status_tx, jd_status_rx) =
            watch::channel(initial_job_declaration_status(&self.config.job_declaration));
        spawn_job_declaration_supervisor(
            self.config.job_declaration.clone(),
            jd_status_tx,
            self.shutdown_token.clone(),
        );

        let api_config = dcentrald_api::ApiConfig {
            cgminer_port: self.config.api.cgminer_port,
            http_port: self.config.api.http_port,
            http_bind: self.config.api.http_bind.clone(),
            websocket_enabled: self.config.api.websocket,
            websocket_tickets: self.config.api.websocket_tickets,
            cgminer_bind_lan: self.config.api.cgminer_bind_lan,
            cgminer_lan_writes: self.config.api.cgminer_lan_writes,
            metrics_require_auth: self.config.api.metrics_require_auth,
            // W13.D1: dev-mode boot-timeline gate. See ApiConfig docs.
            expose_boot_timeline: self.config.api.expose_boot_timeline,
        };

        let hardware_info = Arc::new(std::sync::Mutex::new(dcentrald_api::HardwareInfo {
            control_board: "Idle-first boot".to_string(),
            chip_type: self
                .config
                .mining
                .model
                .clone()
                .unwrap_or_else(|| "Uninitialized".to_string()),
            ..dcentrald_api::HardwareInfo::default()
        }));

        let curtailment = Arc::new(tokio::sync::Mutex::new(
            dcentrald_thermal::curtailment::CurtailmentController::new(),
        ));
        let curtailment_sleeping = Arc::new(AtomicBool::new(false));
        let power_calibration = Arc::new(std::sync::RwLock::new(
            self.config.power.calibration.clone().unwrap_or_default(),
        ));
        let psu_lock = Arc::new(std::sync::Mutex::new(()));
        let history_buffer = HistoryBuffer::load(&history::storage_path());
        let history_data = Arc::new(std::sync::Mutex::new(history::serialize_for_api(
            &history_buffer.samples(),
        )));
        let recent_share_history = Arc::new(std::sync::Mutex::new(Vec::new()));
        let solar_history = Arc::new(std::sync::Mutex::new(Vec::new()));

        let app_state = Arc::new(dcentrald_api::AppState {
            state_rx,
            mode_rx,
            stats_tx: stats_broadcast_tx,
            mining_sync_tx: mining_sync_broadcast_tx,
            mining_pipeline_snapshot_rx,
            mining_pipeline_snapshot_stale_after_ms: self
                .config
                .mining
                .pipeline_snapshot
                .stale_after_ms
                .max(1),
            diagnostic_progress_tx: diag_broadcast_tx.clone(),
            diagnostic_service: Arc::new(tokio::sync::Mutex::new(
                dcentrald_diagnostics::DiagnosticService::new(diag_broadcast_tx),
            )),
            autotuner_tx: autotuner_broadcast_tx,
            config: api_config,
            network_block: self.config.network_block.clone(),
            jd_status_rx,
            profile_path: self.config.autotuner.profile_path.clone(),
            led_tx: None,
            led_status_rx: None,
            curtailment,
            power_rx,
            power_calibration,
            psu_lock,
            hardware_mutation_gate: dcentrald_hal::platform::HardwareMutationGate::new_open(),
            autotuner_status_rx,
            autotuner_efficiency_rx,
            autotuner_chip_health_rx,
            autotuner_telemetry_rx,
            autotuner_command_tx: None,
            history_data: history_data.clone(),
            recent_share_history,
            local_reject_ring: Arc::new(std::sync::Mutex::new(
                dcentrald_api_types::share_validation::LocalRejectRing::with_default_capacity(),
            )),
            boot_progress: Arc::new(dcentrald_api::BootProgressSnapshot::new()),
            audit_ring: Arc::new(std::sync::Mutex::new(
                dcentrald_api_types::audit_log::AuditRing::with_default_capacity(),
            )),
            room_temp_c10: std::sync::atomic::AtomicU32::new(0),
            hardware_info,
            // W13.D1 boot phase tracker — default Generic(Booting), cold-boot
            // orchestrators publish into this once the W14 platform-dispatch
            // refactor lands.
            boot_phase_tracker: Arc::new(dcentrald_api::boot_phase_tracker::BootPhaseTracker::new()),
            offgrid_rx: None,
            // run_api_only has no thermal loop → honest "unavailable".
            pid_state_rx: None,
            pid_command_tx: None,
            solar_rx: None,
            solar_history,
            // P3-2: read-only status handlers read this in-memory mirror of
            // dcentrald.toml instead of re-parsing the file every request.
            config_cache: std::sync::Arc::new(dcentrald_api::ConfigTableCache::new()),
        });

        match dcentrald_api::start_api_servers(app_state).await {
            Ok((_cgminer_handle, _http_handle)) => {
                info!(
                    cgminer_port = self.config.api.cgminer_port,
                    http_port = self.config.api.http_port,
                    "API servers online in idle-first mode — dashboard is available, mining hardware is still offline"
                );
            }
            Err(e) => {
                error!(error = %e, "Failed to start API servers in idle-first mode");
                return Err(e.into());
            }
        }

        self.shutdown_token.cancelled().await;
        info!("Idle-first API mode stopping");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Profile-aware hardware constant accessors
    // -----------------------------------------------------------------------
    // These methods consume a profile resolved at the lifecycle identity
    // boundary. Missing profile state is an internal invariant violation, not
    // permission to guess S9 topology.

    fn required_topology_profile(&self) -> Result<&'static MinerProfile> {
        self.miner_profile.ok_or_else(|| {
            anyhow::anyhow!("topology was not admitted before a model-specific hardware accessor")
        })
    }

    /// Get the PIC I2C address for a given board index (0, 1, 2).
    /// Returns the profile-specific address. No absent-profile default exists.
    fn pic_addr_for_board(&self, board_idx: usize) -> Result<u8> {
        if let Some(addrs) = self.configured_model_pic_addrs_override() {
            if let Some(&address) = addrs.get(board_idx) {
                return Ok(address);
            }
        }
        self.required_topology_profile()?
            .pic_addrs
            .get(board_idx)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("board slot {board_idx} has no admitted PIC endpoint"))
    }

    /// Get the chain ID for a given board index (0, 1, 2).
    /// Returns the profile-specific chain ID. No absent-profile default exists.
    fn chain_id_for_board(&self, board_idx: usize) -> Result<u8> {
        self.required_topology_profile()?
            .chain_ids
            .get(board_idx)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("board slot {board_idx} has no admitted chain ID"))
    }

    /// Get the UIO base for a given board index (0, 1, 2).
    fn uio_base_for_board(&self, board_idx: usize) -> Result<u8> {
        self.required_topology_profile()?
            .uio_bases
            .get(board_idx)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("board slot {board_idx} has no admitted UIO base"))
    }

    /// Get all PIC I2C addresses for detected boards.
    fn pic_addrs(&self) -> Result<&[u8]> {
        if let Some(addrs) = self.configured_model_pic_addrs_override() {
            return Ok(addrs);
        }
        Ok(self.required_topology_profile()?.pic_addrs)
    }

    /// Get all chain IDs.
    fn chain_ids(&self) -> Result<&[u8]> {
        Ok(self.required_topology_profile()?.chain_ids)
    }

    /// Get the chain count for the detected profile.
    fn chain_count(&self) -> Result<u8> {
        Ok(self.required_topology_profile()?.chain_count)
    }

    fn configured_model_key(&self) -> Option<&'static str> {
        self.config
            .mining
            .model
            .as_deref()
            .and_then(model::model_key)
    }

    fn configured_runtime_profile(&self) -> Option<model::RuntimeProfile> {
        self.config
            .mining
            .model
            .as_deref()
            .and_then(model::model_runtime_profile)
    }

    fn configured_model_chip_count_hint(&self) -> Option<u8> {
        if let Some(runtime_profile) = self.configured_runtime_profile() {
            return runtime_profile.chips_per_chain();
        }
        self.config
            .mining
            .model
            .as_deref()
            .and_then(model::model_chip_count_hint)
    }

    fn configured_model_pic_type_override(&self) -> Option<PicType> {
        if let Some(runtime_profile) = self.configured_runtime_profile() {
            return Some(match runtime_profile.pic_type_hint() {
                model::ModelPicTypeHint::Pic16 => PicType::Pic16F1704,
                model::ModelPicTypeHint::DsPic => PicType::DsPic33EP,
                model::ModelPicTypeHint::NoPic => PicType::NoPic,
            });
        }
        self.config
            .mining
            .model
            .as_deref()
            .and_then(model::model_pic_type_hint)
            .map(|hint| match hint {
                model::ModelPicTypeHint::Pic16 => PicType::Pic16F1704,
                model::ModelPicTypeHint::DsPic => PicType::DsPic33EP,
                model::ModelPicTypeHint::NoPic => PicType::NoPic,
            })
    }

    fn configured_model_pic_addrs_override(&self) -> Option<&'static [u8]> {
        if let Some(runtime_profile) = self.configured_runtime_profile() {
            return runtime_profile.pic_addrs_hint();
        }
        self.config
            .mining
            .model
            .as_deref()
            .and_then(model::model_pic_addrs_hint)
    }

    /// Get the expected chips per chain for the detected profile.
    fn default_chips_per_chain(&self) -> Result<u8> {
        if let Some(chips) = self.configured_model_chip_count_hint() {
            return Ok(chips);
        }
        Ok(self.required_topology_profile()?.chips_per_chain)
    }

    /// Get the PIC type for the detected profile.
    ///
    /// The DECLARATIVE result is the model-string override → profile
    /// `pic_type` → S9 default chain, exactly as before. Before runtime
    /// I2C ownership begins, `capture_bootstrap_i2c_observations` applies the
    /// default-off `DCENT_AM2_EEPROM_PIC_DETECT` gate and caches the result.
    /// The chain EEPROM preamble is then the AUTHORITATIVE physical signal:
    /// a clear NoPic preamble (BHB56902 / `0x05 0x11`) forces
    /// `PicType::NoPic` so a NoPic board is never driven as a dsPIC
    /// (SET_VOLTAGE to a non-existent controller). This getter never performs
    /// I2C or sysfs I/O. With the gate off, the cached result is byte-identical
    /// to the declarative result. The EEPROM authority never overrides toward dsPIC on a
    /// weak/absent signal — see
    /// [`crate::runtime::hardware_info::resolve_pic_type`].
    fn pic_type(&self) -> Result<PicType> {
        let declarative = if let Some(pic_type) = self.configured_model_pic_type_override() {
            pic_type
        } else {
            self.required_topology_profile()?.pic_type
        };
        Ok(self.resolved_pic_type.unwrap_or(declarative))
    }

    fn capture_bootstrap_i2c_observations(&mut self) -> Result<()> {
        if self.i2c_service.is_some() {
            anyhow::bail!(
                "bootstrap I2C observations are forbidden after runtime fabric reservation"
            );
        }

        const EEPROM_SLOT_CAPACITY: usize = 8;
        let declarative_pic_type = self.pic_type()?;
        self.resolved_pic_type = Some(resolve_pic_type_from_eeprom(
            declarative_pic_type,
            self.chain_count()? as usize,
        ));
        self.bootstrap_eeprom_fingerprints =
            read_hashboard_eeprom_fingerprints(EEPROM_SLOT_CAPACITY);
        self.bootstrap_eeprom_preambles = (0..EEPROM_SLOT_CAPACITY)
            .map(read_hashboard_eeprom_preamble_for_slot)
            .collect();
        Ok(())
    }

    /// Get the I2C bus number for PIC controllers.
    fn i2c_bus(&self) -> Result<u8> {
        Ok(self.required_topology_profile()?.i2c_bus)
    }

    /// Get the plug detect GPIO base.
    fn plugo_gpio_base(&self) -> Result<u32> {
        Ok(self.required_topology_profile()?.plugo_gpio_base)
    }

    /// Get the enable GPIO base.
    fn enable_gpio_base(&self) -> Result<u32> {
        Ok(self.required_topology_profile()?.enable_gpio_base)
    }

    /// Convert a chain_id back to a board index (0, 1, 2).
    /// S9: chain 6→0, 7→1, 8→2. S19: chain 1→0, 2→1, 3→2.
    fn board_idx_for_chain(&self, chain_id: u8) -> Result<usize> {
        self.chain_ids()?
            .iter()
            .position(|&c| c == chain_id)
            .ok_or_else(|| anyhow::anyhow!("chain {chain_id} is outside the admitted topology"))
    }

    /// Update the miner profile after chip detection.
    /// Called after Phase 6 enumeration when self.chip_id is set.
    fn update_profile(&mut self) -> Result<()> {
        let profile = identified_miner_profile(self.chip_id)?;
        self.miner_profile = Some(profile);
        let effective_chips_per_chain = self.default_chips_per_chain()?;
        let effective_pic_type = self.pic_type()?;
        let effective_pic_addrs = self.pic_addrs()?.to_vec();
        let runtime_profile_key = self.configured_runtime_profile().map(|p| p.key());
        info!(
                chip_id = format_args!("0x{:04X}", self.chip_id),
                model = profile.name,
                runtime_profile = ?runtime_profile_key,
                chips_per_chain = effective_chips_per_chain,
                pic_type = ?effective_pic_type,
                pic_addrs = ?effective_pic_addrs,
                default_freq = profile.default_freq_mhz,
                "MinerProfile loaded — model-specific constants active for {}",
                profile.name,
        );

        if matches!(
            self.configured_model_key(),
            Some("t17") | Some("s17+") | Some("t17+") | Some("t17e")
        ) {
            warn!(
                    config_model = ?self.config.mining.model,
                    runtime_profile = ?runtime_profile_key,
                    anchor_profile = profile.name,
                    effective_chips_per_chain,
                    effective_pic_type = ?effective_pic_type,
                    effective_pic_addrs = ?effective_pic_addrs,
                    "Legacy x17 runtime profile is layered on top of a shared chip-family anchor profile"
            );
        }

        // Assign per-chain PIC addresses from the profile.
        // Maps chain_ids to pic_addrs by index: chain_ids[0]→pic_addrs[0], etc.
        // NoPic models have empty pic_addrs → all chains get None.
        for chain in &mut self.chains {
            let chain_idx = profile
                .chain_ids
                .iter()
                .position(|&id| id == chain.chain_id);
            chain.pic_type = effective_pic_type;
            chain.pic_address = chain_idx.and_then(|idx| effective_pic_addrs.get(idx).copied());
        }
        Ok(())
    }

    /// Run the daemon through its full lifecycle.
    ///
    /// This method does not return until shutdown is requested (via signal or API)
    /// or a fatal error occurs.
    pub async fn run(&mut self) -> Result<()> {
        // NO-BRICK CONTRACT (gap-swarm daemon-startup #6): guarantee a graceful
        // hardware-safe-off teardown on EVERY error exit of the mining lifecycle.
        //
        // `init()` (Phase 1-7, inside run_lifecycle) energizes the chip rail,
        // after which any `?` in the long body can return Err WITHOUT reaching
        // the graceful teardown at the end — that would leave the hash boards
        // energized and the SoC watchdog armed while the process exits and the
        // in-process :8080 API dies (the F1 unmanageable-brick class; only the
        // ~5-64s PIC heartbeat watchdog would eventually cut power). Run
        // shutdown() (disable voltage while heartbeats still flow -> stop
        // heartbeat -> fan cool-down -> watchdog magic-close) before propagating.
        //
        // shutdown() is fully defensive — every subsystem is Option-guarded and
        // best-effort — so it is safe on partial-init state. It is NOT run on the
        // lifecycle's own error exits today (only on the normal cancelled-token Ok
        // path at the end), so there is no double-teardown on success. The
        // api-only path (!mining_start_enabled) energizes no hardware, so its
        // errors skip the teardown.
        let mining = self.config.mining_start_enabled();
        match self.run_lifecycle().await {
            Ok(()) => Ok(()),
            Err(e) if mining => {
                error!(
                    error = %e,
                    "mining lifecycle errored after hardware init — running graceful \
                     hardware-safe-off teardown (voltage cut, fans to idle, explicit \
                     watchdog close attempt) before reporting the error (no-brick #6)"
                );
                if let Err(te) = self.shutdown().await {
                    error!(
                        teardown_error = %te,
                        "graceful teardown after lifecycle error also errored — the PIC \
                         heartbeat watchdog (~5-64s) remains the hardware safety net"
                    );
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// The full daemon lifecycle: management-only short-circuit, then Phases 1-7
    /// init -> spawn all async tasks -> wait for the shutdown signal -> graceful
    /// teardown. Wrapped by `run()` so an early `?` error after `init()` energized
    /// the rail still runs the hardware-safe-off teardown (no-brick #6). Call
    /// `run()`, never this directly, so the teardown guarantee is never bypassed.
    async fn run_lifecycle(&mut self) -> Result<()> {
        let mining_enabled = self.config.mining_start_enabled();

        if !mining_enabled {
            return self.run_api_only().await;
        }

        let platform_identity = self.platform_identity.clone();

        if let Some(refusal) = self.td003_destructive_write_refusal(&platform_identity) {
            warn!(
                model = refusal.model_name,
                source = refusal.source,
                config_model = ?self.config.mining.model,
                "TD-003 destructive-write gate: platform is an Experimental feature / In development or lacks exact board identity; parking management-only before I2C, fan, FPGA, voltage, ASIC init, or hash dispatch"
            );
            return self.run_api_only().await;
        }

        // Capture I2C-backed identity and policy observations before runtime
        // initialization can reserve I2C fabrics. The resulting values are
        // immutable lifecycle input; post-init policy must not fall back to
        // sysfs because an AT24 sysfs read is still a kernel I2C transfer.
        let bootstrap_hardware_info = collect_hardware_info(&self.config);
        self.bootstrap_hb_type = bootstrap_hardware_info.hb_type.clone();

        //  W5: shared boot-progress tracker. Records each
        // `BootPhase` transition with its wall-clock timestamp so
        // operators can see exactly where their unit is in the cold-boot
        // journey via `GET /api/system/boot_timeline`. Cloned into the
        // AppState below; recorded at the same sites as the existing
        // `tracing::info!(target: "boot", ...)` events.
        let boot_progress: Arc<dcentrald_api::BootProgressSnapshot> =
            Arc::new(dcentrald_api::BootProgressSnapshot::new());

        //  W5: structured boot-phase log per
        // dcentrald-api-types::firmware_boot_timeline::DCENT_OS_TIMELINE.
        // Emit one info-level event per phase transition under
        // target="boot" so journalctl / dashboard can subscribe to
        // canonical phase progress without grepping prose log lines.
        boot_progress
            .record_now(dcentrald_api_types::firmware_boot_timeline::BootPhase::ServicesStart);
        info!(
            target: "boot",
            phase = ?dcentrald_api_types::firmware_boot_timeline::BootPhase::ServicesStart,
            "DCENT_OS boot phase: services start (dcentrald run() entered)"
        );

        info!("=== HARDWARE INITIALIZATION ===");
        info!("Starting 7-phase init sequence: detect boards, wake voltage controllers, open FPGA registers, enumerate ASIC chips");

        // Phase 1-7: System Initialization
        // init() leaves its heartbeat guard owned by `self` so it can be stopped
        // only AFTER the mining heartbeat starts, without losing cleanup authority
        // across any fallible handoff step.
        //
        // CRASH-SAFETY (BUG-9, 2026-06-05): `init()` is the hardware bring-up that
        // can HANG or FAIL on real hardware (PIC/AXI-IIC/chip-UART wedge, PSU
        // fault, no hash boards). If it hangs, control NEVER reaches
        // `start_api_servers` below and the operator is locked out with no
        // dashboard and no recovery (the live `.100`-class symptom). Two guards:
        //
        //   (A) BOUND THE HANG: race `init()` against `resolve_init_timeout()` so
        //       an infinite wedge becomes a clean error in bounded time.
        //   (B) FALL BACK TO MANAGEMENT-ONLY *WITH THE API UP*: on timeout OR
        //       error, run the defensive hardware-safe-off teardown
        //       (`shutdown()` — Option-guarded, safe on partial-init state), then
        //       hand off to `run_api_only()`, which builds a clean management
        //       AppState, SPAWNS THE :8080/:4028 API, and parks until SIGTERM.
        //       The dashboard/wizard/toolbox-detector stay reachable and the
        //       bring-up error is reported, instead of a hung or exited daemon.
        //
        // This is the standard-daemon (S9/am1 + am2-s17) analogue of the
        // hybrid/serial/proxy/am3-bb arms, which already spawn the API BEFORE the
        // mining loop. Here the API lives further down inside this function, so
        // an init failure must route through `run_api_only()` to bring it up.
        let init_timeout = resolve_init_timeout();
        let disposition = {
            let mut recovery_publisher = BootProgressRecoveryPublisher {
                boot_progress: Arc::clone(&boot_progress),
            };
            let mut platform = StandardPlatformLifecycle { daemon: self };
            crate::daemon_lifecycle::initialize_or_recover(
                &mut platform,
                &platform_identity,
                &mut recovery_publisher,
                &crate::daemon_lifecycle::TokioLifecycleClock,
                init_timeout,
            )
            .await?
        };
        if disposition == crate::daemon_lifecycle::BringupDisposition::ManagementOnlyStopped {
            return Ok(());
        }

        boot_progress
            .record_now(dcentrald_api_types::firmware_boot_timeline::BootPhase::ChainsEnumerated);
        info!(
            target: "boot",
            phase = ?dcentrald_api_types::firmware_boot_timeline::BootPhase::ChainsEnumerated,
            chains_alive = self.chains.iter().filter(|c| c.mining).count(),
            "DCENT_OS boot phase: chains enumerated (init() complete)"
        );

        // Phase 8: Start all async tasks
        info!("=== ALL SYSTEMS GO ===");
        boot_progress
            .record_now(dcentrald_api_types::firmware_boot_timeline::BootPhase::FirstWorkDispatch);
        info!(
            target: "boot",
            phase = ?dcentrald_api_types::firmware_boot_timeline::BootPhase::FirstWorkDispatch,
            "DCENT_OS boot phase: first work dispatch (mining pipeline starting)"
        );
        if mining_enabled {
            info!(
                "Hardware init complete — starting mining pipeline, thermal control, and API servers"
            );
        } else {
            info!(
                "Hardware init complete — mining auto-start is disabled, bringing up dashboard and control surfaces only"
            );
        }

        // Switch LED to the runtime pattern now that init is complete.
        if let Some(ref led_tx) = self.led_tx {
            let pattern = if mining_enabled {
                LedPattern::Mining
            } else {
                LedPattern::PoolDisconnected
            };
            let _ = led_tx.try_send(LedCommand::SetPattern(pattern));
        }

        // Keep the bounded preflight spin-up command in force across the
        // init→runtime handoff. The thermal task's first evidence-bearing tick
        // is the only owner allowed to lower it; passthrough hardware may
        // already be hot even though no current temperature sample exists yet.

        // Create channels for the mining pipeline
        let (job_tx, job_rx) = mpsc::channel::<dcentrald_stratum::types::JobTemplate>(32);
        let (share_tx, share_rx) = mpsc::channel::<dcentrald_stratum::types::ValidShare>(256);
        let (status_tx, mut status_rx) =
            mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);

        let shutdown = self.shutdown_token.clone();

        // ---- Create webhook alert channel ----
        // The alert_tx sender is cloned into the thermal loop (and eventually
        // work dispatcher) to fire events. The receiver drives a task that
        // POSTs JSON to the configured webhook URL with a 5-second timeout.
        // try_send is used everywhere so the thermal loop never blocks.
        let (alert_tx, mut alert_rx) = mpsc::channel::<AlertEvent>(64);

        let miner_name = self.config.general.hostname.clone();
        let webhook_shutdown = shutdown.clone();
        let webhook_config_path = self.config_path.clone();
        let mut webhook_runtime = RuntimeWebhookConfig::from(self.config.webhook.clone());
        if webhook_runtime.enabled && !webhook_runtime.url.is_empty() {
            info!(
                url = %sanitize_webhook_url(&webhook_runtime.url),
                events = ?webhook_runtime.events,
                "Webhook alert system enabled — critical events will POST to configured URL"
            );
        } else {
            tracing::debug!("Webhook disabled at startup — daemon will poll for config changes");
        }
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            let mut reload_timer = tokio::time::interval(NOTIFICATION_RELOAD_INTERVAL);
            reload_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    _ = webhook_shutdown.cancelled() => {
                        info!("Webhook alert task stopping");
                        break;
                    }
                    _ = reload_timer.tick() => {
                        match RuntimeNotificationConfig::load(&webhook_config_path) {
                            Ok(runtime) => {
                                if runtime.webhook != webhook_runtime {
                                    webhook_runtime = runtime.webhook;
                                    info!(
                                        enabled = webhook_runtime.enabled,
                                        url = %sanitize_webhook_url(&webhook_runtime.url),
                                        events = ?webhook_runtime.events,
                                        "Reloaded webhook config from dcentrald.toml"
                                    );
                                }
                            }
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    path = %webhook_config_path,
                                    "Failed to reload webhook config — keeping previous runtime settings"
                                );
                            }
                        }
                    }
                    Some(mut event) = alert_rx.recv() => {
                        // NOTE (W-notify): this inline S9 AlertEvent task now
                        // REUSES the shared redaction + channel-formatting from
                        // `dcentrald_api::webhook` instead of raw-serializing the
                        // AlertEvent. There is no longer any path that serializes
                        // an un-redacted event. (Residual: this is still a
                        // separate task from `WebhookDispatcher`; the dispatcher's
                        // handle is created later in `spawn_notification_stack` and
                        // is out of this edit scope. The two share one redaction +
                        // one `payload_for`, so there is one formatting/redaction
                        // source of truth and no raw path.)
                        let event_name = event.event_name();
                        // Default-OFF + Telegram-aware live gate — mirrors
                        // `WebhookDispatchConfig::is_live`: Generic/Discord/Slack
                        // need a non-empty URL; Telegram needs a bot token + chat id.
                        let live = webhook_runtime.enabled
                            && match webhook_runtime.format {
                                dcentrald_api::webhook::WebhookFormat::Telegram => {
                                    !webhook_runtime.telegram_bot_token.trim().is_empty()
                                        && !webhook_runtime.telegram_chat_id.trim().is_empty()
                                }
                                _ => !webhook_runtime.url.trim().is_empty(),
                            };
                        if !live {
                            tracing::debug!(event = event_name, "Webhook disabled — alert dropped");
                            continue;
                        }
                        if !webhook_runtime.events.is_empty()
                            && !webhook_runtime.events.iter().any(|configured| configured == event_name)
                        {
                            tracing::debug!(
                                event = event_name,
                                "Webhook: event filtered out by config"
                            );
                            continue;
                        }
                        // Redact BEFORE any serialization/formatting. This closes
                        // the historical raw `PoolDisconnected { url }` wallet leak
                        // and applies uniformly to every channel.
                        crate::runtime::notifications::redact_alert_event(&mut event);
                        // Channel-specific (url, body). Generic keeps the
                        // byte-identical `{ miner, timestamp, alert }` envelope; the
                        // text channels reuse the shared `render_text`/`payload_for`
                        // via the AlertEvent -> WebhookEvent mapping.
                        let (target_url, payload) = match webhook_runtime.format {
                            dcentrald_api::webhook::WebhookFormat::Generic => (
                                webhook_runtime.url.clone(),
                                serde_json::json!({
                                    "miner": miner_name,
                                    "timestamp": std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                    "alert": event,
                                }),
                            ),
                            other => {
                                let mut webhook_event =
                                    crate::runtime::notifications::alert_event_to_webhook_event(&event);
                                // Idempotent belt-and-braces: the AlertEvent was
                                // already redacted, but re-running keeps redaction
                                // strictly ahead of `render_text`.
                                webhook_event.redact();
                                dcentrald_api::webhook::payload_for(
                                    other,
                                    &miner_name,
                                    &webhook_runtime.url,
                                    Some(webhook_runtime.telegram_bot_token.as_str()),
                                    Some(webhook_runtime.telegram_chat_id.as_str()),
                                    &webhook_event,
                                )
                            }
                        };
                        // Do NOT log the target URL: Discord/Slack webhook URLs and
                        // the Telegram endpoint both embed a delivery secret.
                        match client.post(&target_url)
                            .json(&payload)
                            .send()
                        .await
                        {
                            Ok(resp) if resp.status().is_success() => tracing::debug!(
                                status = %resp.status(),
                                event = event_name,
                                "Webhook sent"
                            ),
                            Ok(resp) => warn!(
                                status = %resp.status(),
                                event = event_name,
                                "Webhook send failed with non-success HTTP status"
                            ),
                            Err(error) => warn!(
                                error = %error,
                                event = event_name,
                                "Webhook send failed — alert was not delivered"
                            ),
                        }
                    }
                    else => break,
                }
            }
        });

        // ---- Start API servers ----
        info!("Starting API servers — your dashboard and monitoring endpoints are coming online");

        // Build initial MinerState snapshot
        let version = std::fs::read_to_string("/etc/dcentos-version")
            .unwrap_or_else(|_| "unknown".to_string())
            .trim()
            .to_string();

        let initial_chains: Vec<dcentrald_api::ChainState> = self
            .chains
            .iter()
            .map(|c| dcentrald_api::ChainState {
                id: c.chain_id,
                chips: c.chip_count,
                frequency_mhz: c.frequency_mhz,
                voltage_mv: 0,
                temp_c: 0.0,
                temp_source: None,
                hashrate_ghs: 0.0,
                errors: 0,
                status: if mining_enabled && c.mining {
                    "Active".to_string()
                } else {
                    "Idle".to_string()
                },
            })
            .collect();

        let initial_mode = dcentrald_api::OperatingMode::from_config_str(&self.config.mode.active);

        let initial_state = dcentrald_api::MinerState {
            hashrate_ghs: 0.0,
            hashrate_5s_ghs: 0.0,
            accepted: 0,
            rejected: 0,
            chains: initial_chains,
            fans: dcentrald_api::FanState {
                pwm: 10,
                rpm: 0,
                per_fan: Vec::new(),
            },
            pool: dcentrald_api::PoolState {
                url: self.config.pool.url.clone(),
                worker: self.config.pool.worker.clone(),
                status: if mining_enabled {
                    "Connecting".to_string()
                } else {
                    "Disabled".to_string()
                },
                difficulty: 0.0,
                last_share_at: 0,
                protocol: self
                    .config
                    .pool
                    .protocol
                    .clone()
                    .unwrap_or_else(|| "sv1".to_string()),
                encrypted: matches!(
                    self.config.pool.protocol.as_deref(),
                    Some("sv2") | Some("v2")
                ),
                encrypted_source: if matches!(
                    self.config.pool.protocol.as_deref(),
                    Some("sv2") | Some("v2")
                ) {
                    dcentrald_api::pool_quality_config_source()
                } else {
                    dcentrald_api::pool_quality_honest_default_source()
                },
                sv2_session: None,
                sv2_session_source: dcentrald_api::pool_quality_honest_default_source(),
                sv2_custom_job: None,
                donating: false,
                donating_source: dcentrald_api::pool_quality_honest_default_source(),
                donation_active_url: String::new(),
                donation_active_worker: String::new(),
                donation_pool_index: 0,
                share_efficiency: None,
                auto_fallback_active: false,
                auto_fallback_source: dcentrald_api::pool_quality_honest_default_source(),
                auto_retry_sv2_after_s: None,
                auto_fallback_reason: None,
                failover: dcentrald_stratum::types::PoolFailoverStatus::default(),
                failover_source: dcentrald_api::pool_quality_honest_default_source(),
                hashrate_split: dcentrald_stratum::types::HashrateSplitStatus::default(),
                hashrate_split_source: dcentrald_api::pool_quality_honest_default_source(),
                latency_ms: 0,
                latency_ms_source: dcentrald_api::pool_quality_honest_default_source(),
                reject_reason_counts: [0; 6],
                reject_reason_counts_source: dcentrald_api::pool_quality_honest_default_source(),
                rolling_acceptance_pct_30min: 100.0,
                rolling_acceptance_count_30min: (0, 0),
                rolling_acceptance_source: dcentrald_api::pool_quality_honest_default_source(),
                worst_chip_hw_err_rate: None,
            },
            uptime_s: 0,
            firmware_version: version,
            mode: initial_mode,
        };

        let (state_tx, state_rx) = watch::channel(initial_state);
        let (_mode_tx, mode_rx) = watch::channel(initial_mode);
        let (stats_broadcast_tx, _) = broadcast::channel::<String>(64);
        let (mining_sync_broadcast_tx, _) = broadcast::channel::<String>(256);
        let (diag_broadcast_tx, _) =
            broadcast::channel::<dcentrald_diagnostics::progress::DiagnosticProgress>(32);
        let (autotuner_broadcast_tx, _) = broadcast::channel::<String>(64);

        // Live power estimate channel — work dispatcher writes every 5s,
        // REST API and WebSocket read via borrow(). watch::channel allows
        // multiple concurrent readers without contention.
        let (power_tx, power_rx) =
            watch::channel(dcentrald_autotuner::LivePowerEstimate::default());
        // Consolidated hardware-inventory startup line (directive: "hardware
        // inventory reporting"). One structured, operator-facing summary of the
        // hardware identity at startup — the values are otherwise scattered across
        // separate logs / only on the dashboard API. READ-ONLY: it assembles
        // already-known config + early-detection state; no hardware access, no
        // behavior change. Runtime-detected refinements (actual enumerated chip
        // count, dsPIC fw versions, smart-PSU model) are logged later per-platform
        // during bring-up; this is the identity summary an operator sees first.
        {
            let inv_chip_id = self.config.mining.model_chip_id().unwrap_or(self.chip_id);
            let inv_pic = match self.pic_type()? {
                PicType::Pic16F1704 => "pic16f1704",
                PicType::DsPic33EP => "dspic33ep",
                PicType::NoPic => "nopic",
            };
            let inv_psu = match self.config.power.psu_override.as_ref() {
                Some(ovr) => format!("override:{}@{:.1}V", ovr.model, ovr.voltage_v),
                None => "smart-detect".to_string(),
            };
            info!(
                target: "hw_inventory",
                chip_id = %format_args!("0x{:04X}", inv_chip_id),
                pic = inv_pic,
                chains = state_rx.borrow().chains.len(),
                chips_per_chain = self.default_chips_per_chain()?,
                frequency_mhz = self.config.mining.frequency_mhz,
                psu = %inv_psu,
                mode = %self.config.mode.active,
                "HARDWARE INVENTORY (startup identity — runtime-detected chip enum + dsPIC fw + smart-PSU model are logged later during bring-up)"
            );
        }

        // PERF-006/011: honor the default-OFF `DCENT_AM2_VOLTAGE_AUTOTUNE` gate
        // when advertising capabilities. With the gate unset the function
        // returns the SAME conservative capability set as
        // `autotuner_capabilities_for_chip` (voltage optimization stays gated to
        // BM1387/PIC16) — byte-identical to the prior behavior. When set, the
        // am2/BM1362 dsPIC profile advertises a (downstream-clamped) voltage
        // search the operator opted into.
        let bootstrap_autotuner_capabilities =
            dcentrald_autotuner::autotuner_capabilities_for_chip_with_voltage_autotune(
                self.config.mining.model_chip_id().unwrap_or(0),
                match self.pic_type()? {
                    PicType::Pic16F1704 => "pic16",
                    PicType::DsPic33EP => "dspic",
                    PicType::NoPic => "nopic",
                },
                std::env::var(dcentrald_autotuner::AM2_VOLTAGE_AUTOTUNE_ENV)
                    .ok()
                    .as_deref(),
            );
        let bootstrap_autotuner_policy = dcentrald_autotuner::resolve_autotuner_policy(
            &self.config.autotuner,
            &bootstrap_autotuner_capabilities,
        );
        let initial_autotuner_status = if self.config.autotuner.enabled {
            dcentrald_autotuner::AutotunerRuntimeStatus {
                enabled: true,
                live_runtime: false,
                stale: false,
                age_s: 0,
                source: "runtime_bootstrap".to_string(),
                state: "Waiting".to_string(),
                phase: "Waiting".to_string(),
                percent_complete: 0.0,
                completed_chips: 0,
                active_chips: 0,
                total_chips: 0,
                active_chain_id: None,
                active_chain_total_chips: None,
                target_chains: 0,
                tuned_chains: 0,
                failed_chains: 0,
                tuned_chain_ids: Vec::new(),
                failed_chain_ids: Vec::new(),
                estimated_remaining_s: None,
                avg_frequency_mhz: None,
                efficiency_jth: None,
                silicon_grades: None,
                policy: Some(dcentrald_autotuner::AutotunerPolicyStatus {
                    requested_preset: bootstrap_autotuner_policy.requested_preset.clone(),
                    effective_preset: bootstrap_autotuner_policy.effective_preset.clone(),
                    requested_preset_supported: bootstrap_autotuner_policy
                        .requested_preset_supported,
                    requested_preset_display_name: bootstrap_autotuner_policy
                        .requested_preset
                        .as_deref()
                        .and_then(dcentrald_autotuner::autotuner_preset_display_name)
                        .map(str::to_string),
                    effective_preset_display_name: bootstrap_autotuner_policy
                        .effective_preset
                        .as_deref()
                        .and_then(dcentrald_autotuner::autotuner_preset_display_name)
                        .map(str::to_string),
                    requested_preset_reason: bootstrap_autotuner_policy
                        .requested_preset_reason
                        .clone(),
                    degraded_from_requested: bootstrap_autotuner_policy.degraded_from_requested,
                    capabilities: Some(bootstrap_autotuner_policy.capabilities.clone()),
                    active_objective: None,
                    active_limiting_factor: None,
                    safety_override: None,
                }),
                resume_state: None,
                last_update_s: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                message: "Autotuner waiting for mining stabilization".to_string(),
            }
        } else {
            dcentrald_autotuner::AutotunerRuntimeStatus::default()
        };
        let (autotuner_status_tx, autotuner_status_rx) = watch::channel(initial_autotuner_status);
        let (autotuner_efficiency_tx, autotuner_efficiency_rx) =
            watch::channel(None::<dcentrald_autotuner::EfficiencySnapshot>);
        let (autotuner_chip_health_tx, autotuner_chip_health_rx) =
            watch::channel(None::<dcentrald_autotuner::LiveChipHealthState>);
        // Honest thermal PID telemetry channel (replaces the fabricated
        // /api/debug/pid-state placeholder). Sender → thermal loop;
        // Receiver → AppState. run() is the full path with a thermal loop.
        let (pid_state_tx, pid_state_rx) =
            watch::channel(None::<dcentrald_thermal::controller::PidState>);
        // Runtime thermal-PID tuning command channel (P1, expert-gated:
        // ).
        // Handler clamps; thermal safety overrides stay independent.
        let (pid_command_tx, pid_command_rx) = tokio::sync::mpsc::channel::<(f32, f32, f32)>(8);
        let (autotuner_telemetry_tx, autotuner_telemetry_rx) =
            watch::channel(dcentrald_autotuner::TelemetryExportState::default());
        let (autotuner_command_tx, autotuner_command_rx) =
            mpsc::channel::<dcentrald_autotuner::AutoTunerCommand>(16);
        let (autotuner_share_efficiency_tx, autotuner_share_efficiency_rx) =
            watch::channel(None::<dcentrald_autotuner::AcceptedWorkSignal>);
        let (jd_status_tx, jd_status_rx) =
            watch::channel(initial_job_declaration_status(&self.config.job_declaration));
        spawn_job_declaration_supervisor(
            self.config.job_declaration.clone(),
            jd_status_tx,
            shutdown.clone(),
        );
        let autotuner_status_rx_for_ws = autotuner_status_rx.clone();
        let autotuner_efficiency_rx_for_ws = autotuner_efficiency_rx.clone();
        let autotuner_chip_health_rx_for_ws = autotuner_chip_health_rx.clone();

        let api_config = dcentrald_api::ApiConfig {
            cgminer_port: self.config.api.cgminer_port,
            http_port: self.config.api.http_port,
            http_bind: self.config.api.http_bind.clone(),
            websocket_enabled: self.config.api.websocket,
            websocket_tickets: self.config.api.websocket_tickets,
            cgminer_bind_lan: self.config.api.cgminer_bind_lan,
            cgminer_lan_writes: self.config.api.cgminer_lan_writes,
            metrics_require_auth: self.config.api.metrics_require_auth,
            // W13.D1: dev-mode boot-timeline gate. See ApiConfig docs.
            expose_boot_timeline: self.config.api.expose_boot_timeline,
        };

        // ---- Publish the pre-initialization hardware snapshot ----
        let hardware_info = std::sync::Arc::new(std::sync::Mutex::new(bootstrap_hardware_info));

        // Instantiate the curtailment controller for sleep/wake demand response.
        // Shared between the API (sleep/wake endpoints) and the thermal loop.
        let curtailment = Arc::new(tokio::sync::Mutex::new(
            dcentrald_thermal::curtailment::CurtailmentController::new(),
        ));
        let curtailment_sleeping = Arc::new(AtomicBool::new(false));

        // Clone power_rx before it moves into AppState
        let power_rx_for_publisher = power_rx.clone();

        // ---- Frequency command channel (shared) ----
        // Created early so the off-grid task, thermal throttle, and autotuner can all
        // send frequency commands. The work dispatcher consumes freq_cmd_rx.
        // Multiple senders (off-grid, thermal, autotuner), single receiver (dispatcher).
        let (freq_cmd_tx, freq_cmd_rx) = mpsc::channel::<dcentrald_autotuner::FreqCommand>(64);

        // ---- Off-grid controller setup ----
        let offgrid_rx = if let Some(ref offgrid_cfg) = self.config.power.offgrid {
            if offgrid_cfg.enabled {
                let (og_tx, og_rx) =
                    watch::channel(dcentrald_thermal::offgrid::OffGridTelemetry::default());
                info!("Off-grid mode ENABLED — voltage-based curtailment active");
                info!(
                    "  Battery preset: {}, freq step: {} MHz, min freq: {} MHz",
                    offgrid_cfg.battery_preset,
                    offgrid_cfg.freq_step_mhz,
                    offgrid_cfg.min_frequency_mhz
                );

                // Resolve battery thresholds from preset or custom values
                let preset = match offgrid_cfg.battery_preset.as_str() {
                    "lifepo4_48v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_48V,
                    "lifepo4_24v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_24V,
                    "lifepo4_12v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_12V,
                    "lead_acid_48v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_48V,
                    "lead_acid_24v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_24V,
                    "lead_acid_12v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_12V,
                    _ => dcentrald_thermal::battery::BatteryPreset::Custom,
                };
                let mut thresholds = preset.thresholds();
                // Apply custom overrides if provided
                if let Some(v) = offgrid_cfg.custom_critical_v {
                    thresholds.critical_v = v;
                }
                if let Some(v) = offgrid_cfg.custom_low_v {
                    thresholds.low_v = v;
                }
                if let Some(v) = offgrid_cfg.custom_high_v {
                    thresholds.high_v = v;
                }
                if let Some(v) = offgrid_cfg.custom_full_v {
                    thresholds.full_v = v;
                }
                if let Some(v) = offgrid_cfg.custom_recovery_v {
                    thresholds.recovery_v = v;
                }

                let max_freq = self.config.mining.frequency_mhz;
                let min_freq = offgrid_cfg.min_frequency_mhz;
                let freq_step = offgrid_cfg.freq_step_mhz;
                let interval_ms = offgrid_cfg.loop_interval_ms;
                let battery_backed_source = matches!(
                    self.config.power.source_profile.as_deref(),
                    Some("direct_dc") | Some("solar_battery")
                );

                let adc_config = offgrid_cfg.adc.clone();
                let (configured_source_name, configured_has_current) = match adc_config.as_ref() {
                    Some(dcentrald_hal::adc::AdcBackendConfig::Ina226 { .. }) => {
                        ("INA226".to_string(), true)
                    }
                    Some(dcentrald_hal::adc::AdcBackendConfig::Sysfs { .. }) => {
                        ("Sysfs ADC".to_string(), false)
                    }
                    Some(dcentrald_hal::adc::AdcBackendConfig::Simulated { .. }) => {
                        ("Simulated".to_string(), true)
                    }
                    None => ("Unconfigured".to_string(), false),
                };

                if adc_config.is_none() {
                    let mut controller = dcentrald_thermal::offgrid::OffGridController::new(
                        thresholds, max_freq, min_freq, freq_step,
                    );
                    controller.enter_sensor_fault();
                    {
                        let mut curt = curtailment.lock().await;
                        curt.enter_sleep();
                    }
                    let _ = og_tx.send(controller.fault_telemetry(
                        &configured_source_name,
                        configured_has_current,
                        "Off-grid mode requires an explicit ADC backend. Configure INA226, Sysfs ADC, or an intentional simulated source before enabling battery protection.",
                    ));
                    tracing::error!(
                        "Off-grid mode enabled without ADC backend — forcing curtailment sleep fail-safe"
                    );
                    Some(og_rx)
                } else {
                    let og_shutdown = self.shutdown_token.clone();
                    let og_curtailment = curtailment.clone();
                    let og_freq_tx = freq_cmd_tx.clone();
                    let Some(adc_config) = adc_config else {
                        unreachable!("adc_config is Some in this branch");
                    };

                    // Spawn off-grid control task
                    tokio::spawn(async move {
                        let mut controller = dcentrald_thermal::offgrid::OffGridController::new(
                            thresholds, max_freq, min_freq, freq_step,
                        );
                        let mut adc = match dcentrald_hal::adc::create_voltage_source(&adc_config) {
                            Ok(a) => a,
                            Err(e) => {
                                controller.enter_sensor_fault();
                                {
                                    let mut curt = og_curtailment.lock().await;
                                    curt.enter_sleep();
                                }
                                let _ = og_tx.send(controller.fault_telemetry(
                                    &configured_source_name,
                                    configured_has_current,
                                    &format!("ADC init failed: {}", e),
                                ));
                                tracing::error!(
                                    error = %e,
                                    "Off-grid ADC init failed — forcing curtailment sleep fail-safe"
                                );
                                return;
                            }
                        };

                        let sensor_source = adc.source_name().to_string();
                        let sensor_has_current = adc.has_current();
                        let mut interval =
                            tokio::time::interval(Duration::from_millis(interval_ms));
                        info!(
                            sensor = %sensor_source,
                            has_current = sensor_has_current,
                            "Off-grid controller running — monitoring voltage every {}ms",
                            interval_ms
                        );

                        loop {
                            tokio::select! {
                                _ = og_shutdown.cancelled() => {
                                    info!("Off-grid controller stopping");
                                    break;
                                }
                                _ = interval.tick() => {
                                    match adc.read() {
                                        Ok(reading) => {
                                            let action = controller.tick(&reading);
                                            match action {
                                                dcentrald_thermal::offgrid::OffGridAction::Sleep => {
                                                    let mut curt = og_curtailment.lock().await;
                                                    curt.enter_sleep();
                                                }
                                                dcentrald_thermal::offgrid::OffGridAction::Wake(freq) => {
                                                    let mut curt = og_curtailment.lock().await;
                                                    curt.wake();
                                                    // Apply the wake frequency ceiling across active chains.
                                                    if let Err(e) = og_freq_tx.try_send(
                                                        dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                            chain_id: 0xFF,
                                                            max_freq_mhz: Some(freq),
                                                            source: dcentrald_autotuner::FrequencyLimitSource::OffGrid,
                                                            ack_tx: None,
                                                        }
                                                    ) {
                                                        tracing::warn!(error = %e, "Off-grid wake freq command failed");
                                                    }
                                                }
                                                dcentrald_thermal::offgrid::OffGridAction::SetFrequency(freq) => {
                                                    // Send off-grid ceiling update to the work dispatcher.
                                                    if let Err(e) = og_freq_tx.try_send(
                                                        dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                            chain_id: 0xFF,
                                                            max_freq_mhz: Some(freq),
                                                            source: dcentrald_autotuner::FrequencyLimitSource::OffGrid,
                                                            ack_tx: None,
                                                        }
                                                    ) {
                                                        tracing::warn!(error = %e, "Off-grid freq command send failed");
                                                    }
                                                }
                                                dcentrald_thermal::offgrid::OffGridAction::Hold => {}
                                            }

                                            let telemetry = controller.telemetry(
                                                &reading,
                                                &sensor_source,
                                                sensor_has_current,
                                            );
                                            let _ = og_tx.send(telemetry);
                                        }
                                        Err(e) => {
                                            controller.enter_sensor_fault();
                                            {
                                                let mut curt = og_curtailment.lock().await;
                                                curt.enter_sleep();
                                            }
                                            let _ = og_tx.send(controller.fault_telemetry(
                                                &sensor_source,
                                                sensor_has_current,
                                                &format!("ADC read failed: {}", e),
                                            ));
                                            tracing::warn!(
                                                error = %e,
                                                sensor = %sensor_source,
                                                "Off-grid ADC read failed — entering sensor_fault and curtailment sleep"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    });

                    Some(og_rx)
                }
            } else {
                None
            }
        } else {
            None
        };

        let solar_history = Arc::new(std::sync::Mutex::new(Vec::new()));
        let solar_rx = if let Some(ref solar_cfg) = self.config.power.solar {
            if solar_cfg.enabled {
                let source_profile = self
                    .config
                    .power
                    .source_profile
                    .clone()
                    .unwrap_or_else(|| "grid".to_string());
                let battery_backed_source =
                    matches!(source_profile.as_str(), "solar_battery" | "direct_dc");
                let max_freq = self.config.mining.frequency_mhz;
                let min_freq = self
                    .config
                    .power
                    .offgrid
                    .as_ref()
                    .map(|cfg| cfg.min_frequency_mhz)
                    .unwrap_or(200)
                    .max(100);
                // If the operator's mining ceiling is below the off-grid frequency
                // floor, the (min,max) bounds are inverted; clamp the floor down so
                // the ceiling always wins and log the contradiction once. This also
                // keeps solar::decide_policy()'s u16::clamp() panic-free.
                let min_freq = if min_freq > max_freq {
                    tracing::warn!(
                        mining_frequency_mhz = max_freq,
                        offgrid_min_frequency_mhz = min_freq,
                        "off-grid/solar frequency floor exceeds the mining ceiling; clamping floor to ceiling"
                    );
                    max_freq
                } else {
                    min_freq
                };
                let fallback_reference_watts = if self.config.power.target_watts > 0 {
                    self.config.power.target_watts
                } else {
                    self.config.power.max_watts.max(1)
                };
                let solar_provider_support =
                    dcentrald_api::solar_provider_support(&solar_cfg.inverter_brand);
                let provider_telemetry_backed =
                    dcentrald_api::solar_provider_telemetry_backed(&solar_cfg.inverter_brand);
                let solar_power_rx = power_rx.clone();
                let boot_power = solar_power_rx.borrow().clone();
                let boot_mining_power = dcentrald_api::solar_mining_power_status(&boot_power);
                let (solar_tx, solar_rx) = watch::channel(dcentrald_api::SolarPolicyState {
                    enabled: true,
                    provider: solar_cfg.inverter_brand.clone(),
                    provider_live_backend: solar_provider_support.live_backend,
                    provider_telemetry_backed,
                    provider_configured: true,
                    provider_stage: solar_provider_support.stage.clone(),
                    provider_stage_reason: solar_provider_support.stage_reason.clone(),
                    runtime_adopted: true,
                    commissioning_state: dcentrald_api::solar_commissioning_state(
                        true,
                        &solar_cfg.inverter_brand,
                        false,
                        true,
                        0,
                    )
                    .to_string(),
                    source_profile: source_profile.clone(),
                    mining_watts: boot_mining_power.watts,
                    mining_watts_source: boot_mining_power.source,
                    mining_watts_live: boot_mining_power.live,
                    mining_watts_modeled: boot_mining_power.modeled,
                    mining_watts_note: boot_mining_power.note.to_string(),
                    solar_only_mode: solar_cfg.solar_only_mode,
                    action: "booting".to_string(),
                    message: "Solar policy waiting for first provider sample".to_string(),
                    last_update_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    ..dcentrald_api::SolarPolicyState::default()
                });

                let solar_cfg = solar_cfg.clone();
                let solar_shutdown = self.shutdown_token.clone();
                let solar_curtailment = curtailment.clone();
                let solar_freq_tx = freq_cmd_tx.clone();
                let solar_sleeping_state = curtailment_sleeping.clone();
                let solar_history_task = solar_history.clone();

                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(5));
                    let mut solar_forced_sleep = false;
                    let mut consecutive_failures: u32 = 0;
                    let mut last_success_ms: Option<u64> = None;

                    loop {
                        tokio::select! {
                            _ = solar_shutdown.cancelled() => {
                                info!("Solar policy controller stopping");
                                break;
                            }
                            _ = interval.tick() => {
                                let power = solar_power_rx.borrow().clone();
                                let mining_power = dcentrald_api::solar_mining_power_status(&power);
                                let mining_watts = mining_power.watts;
                                let failure_hysteresis = solar_cfg.provider_failure_hysteresis_samples.max(1) as u32;
                                match crate::solar::fetch_snapshot(&solar_cfg, mining_watts).await {
                                    Ok(snapshot) => {
                                        consecutive_failures = 0;
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis() as u64;
                                        last_success_ms = Some(now_ms);
                                        let reference_watts = if mining_watts > 0 {
                                            mining_watts.max((fallback_reference_watts / 2).max(1))
                                        } else {
                                            fallback_reference_watts.max(1)
                                        };
                                        let decision = crate::solar::decide_policy(
                                            &source_profile,
                                            &solar_cfg,
                                            &snapshot,
                                            mining_watts,
                                            max_freq,
                                            min_freq,
                                            reference_watts,
                                            solar_forced_sleep,
                                        );

                                        if decision.sleep {
                                            if !solar_forced_sleep {
                                                let mut curt = solar_curtailment.lock().await;
                                                curt.enter_sleep();
                                                solar_forced_sleep = true;
                                            }
                                            let _ = solar_freq_tx.try_send(
                                                dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                    chain_id: 0xFF,
                                                    max_freq_mhz: None,
                                                    source: dcentrald_autotuner::FrequencyLimitSource::SolarSurplus,
                                                    ack_tx: None,
                                                }
                                            );
                                        } else {
                                            if decision.wake && solar_forced_sleep {
                                                let mut curt = solar_curtailment.lock().await;
                                                // wake() only succeeds from Sleeping; during the
                                                // EnteringSleep window (thermal loop hasn't run
                                                // sleep_complete() yet) it returns false — keep
                                                // ownership so the next tick retries instead of
                                                // stranding the controller OFF (stuck-OFF trap).
                                                // Also release if some other owner already made it Active.
                                                let woke = curt.wake();
                                                if woke
                                                    || matches!(
                                                        curt.state(),
                                                        dcentrald_thermal::curtailment::CurtailmentState::Active
                                                            | dcentrald_thermal::curtailment::CurtailmentState::Waking
                                                    )
                                                {
                                                    solar_forced_sleep = false;
                                                }
                                            }

                                            let _ = solar_freq_tx.try_send(
                                                dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                    chain_id: 0xFF,
                                                    max_freq_mhz: decision.target_freq_mhz,
                                                    source: dcentrald_autotuner::FrequencyLimitSource::SolarSurplus,
                                                    ack_tx: None,
                                                }
                                            );
                                        }

                                        let base_load_estimate = snapshot.consumption_watts.saturating_sub(mining_watts);
                                        let solar_surplus_watts = snapshot.production_watts.saturating_sub(base_load_estimate);
                                        let message = format!("{} {}", snapshot.message, decision.message);
                                        if let Ok(mut history) = solar_history_task.lock() {
                                            history.push(dcentrald_api::SolarVerificationSample {
                                                timestamp_ms: now_ms,
                                                provider: solar_cfg.inverter_brand.clone(),
                                                transport: snapshot.transport.clone(),
                                                connected: snapshot.connected,
                                                sample_age_ms: snapshot.sample_age_ms,
                                                stale: snapshot.stale,
                                                consecutive_failures,
                                                last_success_ms,
                                                matched_fields: snapshot.matched_fields.clone(),
                                                production_watts: snapshot.production_watts,
                                                consumption_watts: snapshot.consumption_watts,
                                                net_grid_watts: snapshot.net_grid_watts,
                                                battery_soc_pct: snapshot.battery_soc_pct,
                                                message: message.clone(),
                                            });
                                            let excess = history
                                                .len()
                                                .saturating_sub(dcentrald_api::SOLAR_VERIFICATION_HISTORY_LIMIT);
                                            if excess > 0 {
                                                history.drain(0..excess);
                                            }
                                        }
                                        let _ = solar_tx.send(dcentrald_api::SolarPolicyState {
                                            enabled: true,
                                            provider: solar_cfg.inverter_brand.clone(),
                                            provider_live_backend: solar_provider_support.live_backend,
                                            provider_telemetry_backed,
                                            provider_configured: true,
                                            provider_stage: solar_provider_support.stage.clone(),
                                            provider_stage_reason: solar_provider_support.stage_reason.clone(),
                                            runtime_adopted: true,
                                            commissioning_state: dcentrald_api::solar_commissioning_state(
                                                true,
                                                &solar_cfg.inverter_brand,
                                                snapshot.connected,
                                                snapshot.stale,
                                                consecutive_failures,
                                            )
                                            .to_string(),
                                            source_profile: source_profile.clone(),
                                            connected: snapshot.connected,
                                            transport: snapshot.transport,
                                            matched_fields: snapshot.matched_fields,
                                            production_watts: snapshot.production_watts,
                                            consumption_watts: snapshot.consumption_watts,
                                            mining_watts,
                                            mining_watts_source: mining_power.source.clone(),
                                            mining_watts_live: mining_power.live,
                                            mining_watts_modeled: mining_power.modeled,
                                            mining_watts_note: mining_power.note.to_string(),
                                            net_grid_watts: snapshot.net_grid_watts,
                                            solar_surplus_watts,
                                            battery_soc_pct: snapshot.battery_soc_pct,
                                            solar_only_mode: solar_cfg.solar_only_mode,
                                            control_active: decision.control_active,
                                            sleeping: solar_forced_sleep || solar_sleeping_state.load(Ordering::Acquire),
                                            battery_floor_active: decision.battery_floor_active,
                                            target_freq_mhz: decision.target_freq_mhz,
                                            action: decision.action,
                                            sample_age_ms: snapshot.sample_age_ms,
                                            stale: snapshot.stale,
                                            consecutive_failures,
                                            last_success_ms,
                                            message,
                                            last_update_ms: now_ms,
                                        });
                                    }
                                    Err(e) => {
                                        consecutive_failures = consecutive_failures.saturating_add(1);
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis() as u64;
                                        let require_fail_closed_sleep = solar_cfg.solar_only_mode || battery_backed_source;
                                        let failure_hysteresis_for_mode = if battery_backed_source {
                                            1
                                        } else {
                                            failure_hysteresis
                                        };
                                        let fail_closed_triggered = require_fail_closed_sleep
                                            && consecutive_failures >= failure_hysteresis_for_mode;
                                        if fail_closed_triggered && !solar_forced_sleep {
                                            let mut curt = solar_curtailment.lock().await;
                                            curt.enter_sleep();
                                            solar_forced_sleep = true;
                                        }
                                        if !require_fail_closed_sleep || fail_closed_triggered {
                                            let _ = solar_freq_tx.try_send(
                                                dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                    chain_id: 0xFF,
                                                    max_freq_mhz: None,
                                                    source: dcentrald_autotuner::FrequencyLimitSource::SolarSurplus,
                                                    ack_tx: None,
                                                }
                                            );
                                        }
                                        let transport = dcentrald_api::solar_transport(
                                            &solar_cfg.inverter_brand,
                                            &solar_cfg.api_endpoint,
                                        );
                                        let message = if require_fail_closed_sleep && !fail_closed_triggered {
                                            format!(
                                                "Solar provider error: {}. Failure hysteresis is holding the previous solar policy until {} consecutive errors (currently {}).",
                                                e,
                                                failure_hysteresis_for_mode,
                                                consecutive_failures
                                            )
                                        } else if require_fail_closed_sleep {
                                            format!("Solar provider error: {}. DCENT_OS entered fail-closed sleep for this power mode.", e)
                                        } else {
                                            format!("Solar provider error: {}", e)
                                        };
                                        if let Ok(mut history) = solar_history_task.lock() {
                                            history.push(dcentrald_api::SolarVerificationSample {
                                                timestamp_ms: now_ms,
                                                provider: solar_cfg.inverter_brand.clone(),
                                                transport: transport.clone(),
                                                connected: false,
                                                sample_age_ms: None,
                                                stale: true,
                                                consecutive_failures,
                                                last_success_ms,
                                                matched_fields: Vec::new(),
                                                production_watts: 0,
                                                consumption_watts: mining_watts,
                                                net_grid_watts: mining_watts as i64,
                                                battery_soc_pct: None,
                                                message: message.clone(),
                                            });
                                            let excess = history
                                                .len()
                                                .saturating_sub(dcentrald_api::SOLAR_VERIFICATION_HISTORY_LIMIT);
                                            if excess > 0 {
                                                history.drain(0..excess);
                                            }
                                        }
                                        let _ = solar_tx.send(dcentrald_api::SolarPolicyState {
                                            enabled: true,
                                            provider: solar_cfg.inverter_brand.clone(),
                                            provider_live_backend: solar_provider_support.live_backend,
                                            provider_telemetry_backed,
                                            provider_configured: true,
                                            provider_stage: solar_provider_support.stage.clone(),
                                            provider_stage_reason: solar_provider_support.stage_reason.clone(),
                                            runtime_adopted: true,
                                            commissioning_state: dcentrald_api::solar_commissioning_state(
                                                true,
                                                &solar_cfg.inverter_brand,
                                                false,
                                                true,
                                                consecutive_failures,
                                            )
                                            .to_string(),
                                            source_profile: source_profile.clone(),
                                            connected: false,
                                            transport,
                                            mining_watts,
                                            mining_watts_source: mining_power.source.clone(),
                                            mining_watts_live: mining_power.live,
                                            mining_watts_modeled: mining_power.modeled,
                                            mining_watts_note: mining_power.note.to_string(),
                                            solar_only_mode: solar_cfg.solar_only_mode,
                                            control_active: fail_closed_triggered,
                                            sleeping: solar_forced_sleep || solar_sleeping_state.load(Ordering::Acquire),
                                            battery_floor_active: matches!(source_profile.as_str(), "solar_battery" | "direct_dc"),
                                            action: if require_fail_closed_sleep && !fail_closed_triggered {
                                                "fault_hysteresis".to_string()
                                            } else if require_fail_closed_sleep {
                                                "sleep".to_string()
                                            } else {
                                                "fault".to_string()
                                            },
                                            sample_age_ms: None,
                                            stale: true,
                                            consecutive_failures,
                                            last_success_ms,
                                            message,
                                            last_update_ms: now_ms,
                                            ..dcentrald_api::SolarPolicyState::default()
                                        });
                                    }
                                }
                            }
                        }
                    }
                });

                Some(solar_rx)
            } else {
                None
            }
        } else {
            None
        };

        // ---- Scheduled (time-of-day) curtailment driver ----
        //
        // GROUP B WIRING: drive the already-built, already-consumed
        // `CurtailmentController` from an operator-configured daily window
        // (off-peak / demand-response / quiet-night). The off-grid and solar
        // paths above drive the SAME shared controller from battery voltage /
        // solar surplus; this just adds a time-of-day driver. The thermal loop
        // is the consumer — on `EnteringSleep` it de-energizes the hash boards
        // (cut-hash) and drops fans to the controller's low `sleep_fan_pwm`; on
        // `Waking` it restores voltage. We never touch fans or freq directly
        // here, so the PWM-30 cap and fail-closed thermal behaviour still bound
        // everything downstream.
        //
        // SAFETY: curtailment is strictly the safe direction — sleeping CUTS
        // hash power and LOWERS fans. We only ever call `enter_sleep()` (in the
        // window) or `wake()` (outside it); we never raise fan speed or push
        // power up.
        //
        // DEFAULT-OFF: spawned only when `[power.curtailment].enabled`. When the
        // section is absent (`None`) or disabled, the controller is left
        // entirely to the off-grid/solar/API owners and the runtime path is
        // byte-identical to today.
        if let Some(curtail_cfg) = self
            .config
            .power
            .curtailment
            .as_ref()
            .filter(|c| c.enabled)
            .cloned()
        {
            info!(
                start_hour = curtail_cfg.start_hour,
                end_hour = curtail_cfg.end_hour,
                poll_interval_s = curtail_cfg.poll_interval_s,
                "Scheduled curtailment ENABLED — miner will sleep (cut hash, low fans) \
                 during the configured off-peak/demand-response window"
            );
            let schedule_shutdown = self.shutdown_token.clone();
            let schedule_curtailment = curtailment.clone();
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(Duration::from_secs(curtail_cfg.poll_interval_s.max(1)));
                // Mirrors the solar driver's `solar_forced_sleep`: tracks whether
                // THIS driver put the controller to sleep, so we only `wake()`
                // what we slept and don't fight the off-grid/solar/API owners.
                let mut schedule_forced_sleep = false;
                loop {
                    tokio::select! {
                        _ = schedule_shutdown.cancelled() => {
                            info!("Scheduled curtailment driver stopping");
                            break;
                        }
                        _ = interval.tick() => {
                            // FWSTAB-1: compare against the operator's LOCAL
                            // hour-of-day (UTC + configured offset), not raw UTC,
                            // so a 22:00 curtail window fires at 22:00 local.
                            let hour = {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let utc_hour = ((now / 3600) % 24) as u8;
                                dcentrald_common::time::local_hour_from_utc(
                                    utc_hour,
                                    curtail_cfg.timezone_offset_hours,
                                )
                            };
                            let in_window = curtail_cfg.is_active_at_hour(hour);

                            if in_window && !schedule_forced_sleep {
                                let mut curt = schedule_curtailment.lock().await;
                                // enter_sleep() only transitions from Active;
                                // if another owner already slept it, that's
                                // fine — claim ownership so we wake it later.
                                let entered = curt.enter_sleep();
                                schedule_forced_sleep = true;
                                if entered {
                                    info!(
                                        hour,
                                        "Scheduled curtailment: entering off-peak sleep \
                                         (hash will be cut, fans dropped to standby)"
                                    );
                                }
                            } else if !in_window && schedule_forced_sleep {
                                let mut curt = schedule_curtailment.lock().await;
                                // wake() only succeeds from Sleeping; during the
                                // EnteringSleep window (thermal loop hasn't run
                                // sleep_complete() yet) it returns false — keep
                                // ownership so the next tick retries instead of
                                // stranding the controller OFF (stuck-OFF trap).
                                // Also release if some other owner already made it
                                // Active/Waking. (Same pattern as the solar driver.)
                                let woke = curt.wake();
                                if woke
                                    || matches!(
                                        curt.state(),
                                        dcentrald_thermal::curtailment::CurtailmentState::Active
                                            | dcentrald_thermal::curtailment::CurtailmentState::Waking
                                    )
                                {
                                    schedule_forced_sleep = false;
                                    if woke {
                                        info!(
                                            hour,
                                            "Scheduled curtailment: window ended — waking miner \
                                             (voltage restored, mining resumes)"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }

        let power_calibration = Arc::new(std::sync::RwLock::new(
            self.config.power.calibration.clone().unwrap_or_default(),
        ));
        let psu_lock = Arc::new(std::sync::Mutex::new(()));
        let history_path = history::storage_path();
        let history_buffer = HistoryBuffer::load(&history_path);
        let history_data = Arc::new(std::sync::Mutex::new(history::serialize_for_api(
            &history_buffer.samples(),
        )));
        let recent_share_history = Arc::new(std::sync::Mutex::new(Vec::new()));

        //  W1: shared local-reject ring (work dispatcher pushes,
        // REST handler reads). One Arc<Mutex<...>> per AppState; cloned
        // into the dispatcher below via `set_local_reject_ring`.
        let local_reject_ring: Arc<
            std::sync::Mutex<dcentrald_api_types::share_validation::LocalRejectRing>,
        > = Arc::new(std::sync::Mutex::new(
            dcentrald_api_types::share_validation::LocalRejectRing::with_default_capacity(),
        ));
        let dispatcher_local_reject_ring = local_reject_ring.clone();
        let mining_pipeline_snapshot_rx = if self.config.mining.pipeline_snapshot.enabled {
            Some(
                dcentrald_api::mining_pipeline_snapshot::spawn_mining_pipeline_snapshot_publisher(
                    &mining_sync_broadcast_tx,
                    self.config.mining.pipeline_snapshot.stale_after_ms,
                ),
            )
        } else {
            None
        };

        let app_state = Arc::new(dcentrald_api::AppState {
            state_rx: state_rx.clone(),
            mode_rx: mode_rx.clone(),
            stats_tx: stats_broadcast_tx.clone(),
            mining_sync_tx: mining_sync_broadcast_tx.clone(),
            mining_pipeline_snapshot_rx,
            mining_pipeline_snapshot_stale_after_ms: self
                .config
                .mining
                .pipeline_snapshot
                .stale_after_ms
                .max(1),
            diagnostic_progress_tx: diag_broadcast_tx.clone(),
            diagnostic_service: Arc::new(tokio::sync::Mutex::new(
                dcentrald_diagnostics::DiagnosticService::new(diag_broadcast_tx),
            )),
            autotuner_tx: autotuner_broadcast_tx.clone(),
            config: api_config,
            network_block: self.config.network_block.clone(),
            jd_status_rx: jd_status_rx.clone(),
            profile_path: self.config.autotuner.profile_path.clone(),
            led_tx: self.led_tx.clone(),
            led_status_rx: self.led_status_rx.clone(),
            curtailment: curtailment.clone(),
            power_rx: power_rx.clone(),
            power_calibration: power_calibration.clone(),
            psu_lock: psu_lock.clone(),
            hardware_mutation_gate: dcentrald_hal::platform::HardwareMutationGate::new_open(),
            autotuner_status_rx: autotuner_status_rx.clone(),
            autotuner_efficiency_rx: autotuner_efficiency_rx.clone(),
            autotuner_chip_health_rx: autotuner_chip_health_rx.clone(),
            autotuner_telemetry_rx: autotuner_telemetry_rx.clone(),
            autotuner_command_tx: Some(autotuner_command_tx.clone()),
            history_data: history_data.clone(),
            recent_share_history: recent_share_history.clone(),
            local_reject_ring,
            boot_progress: boot_progress.clone(),
            audit_ring: Arc::new(std::sync::Mutex::new(
                dcentrald_api_types::audit_log::AuditRing::with_default_capacity(),
            )),
            room_temp_c10: std::sync::atomic::AtomicU32::new(0),
            hardware_info: hardware_info.clone(),
            // W13.D1 boot phase tracker — published into by cold-boot
            // orchestrators (W14+ wiring).
            boot_phase_tracker: Arc::new(dcentrald_api::boot_phase_tracker::BootPhaseTracker::new()),
            offgrid_rx: offgrid_rx.clone(),
            pid_state_rx: Some(pid_state_rx),
            pid_command_tx: Some(pid_command_tx),
            solar_rx: solar_rx.clone(),
            solar_history: solar_history.clone(),
            // P3-2: read-only status handlers read this in-memory mirror of
            // dcentrald.toml instead of re-parsing the file every request.
            config_cache: std::sync::Arc::new(dcentrald_api::ConfigTableCache::new()),
        });

        // DCENT Expansion Pack ("dcent-pack") bridge client. Spawned only when
        // [bridge].enabled = true (default off). Cancellation-aware via the
        // shared `shutdown` token, matching the other spawned tasks above. The
        // bridge crate is no-HAL; the daemon supplies the room-temp sink (the
        // EXISTING room_temp_c10 atomic) + a live miner-status snapshot via the
        // adapters in `crate::bridge_glue`. Spawned BEFORE `app_state` is moved
        // into `start_api_servers` below — uses `app_state.clone()` for the sink.
        if self.config.bridge.enabled {
            let bridge_cfg = self.config.bridge.clone();
            let bridge_shutdown = shutdown.clone();
            let bridge_runtime = crate::bridge_glue::build_runtime(
                self.config.mining.model.as_deref(),
                self.config.api.http_port,
            );
            let bridge_status: std::sync::Arc<dyn dcentrald_bridge::MinerStatusProvider> =
                std::sync::Arc::new(crate::bridge_glue::MinerStatusAdapter::new(
                    state_rx.clone(),
                    power_rx.clone(),
                ));
            let bridge_sink: std::sync::Arc<dyn dcentrald_bridge::RoomTempSink> =
                std::sync::Arc::new(crate::bridge_glue::RoomTempSinkAdapter::new(
                    app_state.clone(),
                ));
            info!("DCENT Expansion Pack bridge client enabled — watching for bridge gateway");
            tokio::spawn(dcentrald_bridge::bridge_client_task(
                bridge_cfg,
                bridge_shutdown,
                bridge_runtime,
                bridge_status,
                bridge_sink,
            ));
        }

        // SW-02: capture an Arc clone of the AppState BEFORE it is moved into
        // `start_api_servers` below, so the gRPC write-control delegate (wired
        // further down, next to `install_runtime_snapshot_rx`) can reach the
        // SAME gated runtime channels (autotuner command tx, led_tx) the REST
        // handlers use. Only captured when `[api.grpc].enabled` so the clone is
        // never made on the common (gRPC-disabled) path; it is only USED when
        // the default-OFF `DCENT_GRPC_WRITE_CONTROL` gate is ALSO set.
        let grpc_write_app_state: Option<Arc<dcentrald_api::AppState>> =
            if self.config.api.grpc.enabled {
                Some(app_state.clone())
            } else {
                None
            };

        // P2-7 (Omega): capture an Arc clone of the AppState for the MQTT/HA
        // command-subscriber sink BEFORE `app_state` is moved into
        // `start_api_servers` below. The sink routes HA setpoints (fan PWM /
        // target watts / target temp) through the SAME clamped setters the REST
        // API uses (`grpc_bridge_set_fan` + the autotuner PowerTarget command +
        // the thermal-target persist). Built unconditionally (a cheap Arc clone)
        // so a runtime `[mqtt].enabled` toggle has the sink ready; the command
        // surface itself stays default-OFF until the MQTT publisher spawns.
        let mqtt_command_state = app_state.clone();

        // R-11: capture an Arc clone of the AppState for the thermal-supervisor
        // hardware-safety audit path BEFORE `app_state` is moved into
        // `start_api_servers` below. The thermal loop (spawned further down)
        // calls `dcentrald_api::push_audit_event(&thermal_audit_app_state,
        // "thermal_supervisor", <event>)` when the supervisor ACTS on an
        // over-temp shutdown / fan panic / board power-off, so those safety
        // events land in the SAME best-effort audit ring + on-disk log the REST
        // handlers write. A cheap Arc clone; only USED inside the (default-OFF,
        // operator-gated) `thermal_supervisor.is_some()` block, and
        // `push_audit_event` is fail-safe (never panics, no-op on a poisoned
        // lock), so this can never affect mining.
        let thermal_audit_app_state = app_state.clone();

        // Keep the API server JoinHandles owned for the lifetime of run().
        // Detaching them via `_` discard caused the dashboard / CGMiner ports to
        // never bind reliably under heavy mining-loop runtime pressure on
        // S19j Pro `a lab unit` (DCENT_CE 2026-04-24 finding). Storing them here also
        // lets a future shutdown path call `abort()` cleanly.
        let _api_handles: Option<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>)> =
            match dcentrald_api::start_api_servers(app_state).await {
                Ok((cgminer_handle, http_handle)) => {
                    info!(
                        cgminer_port = self.config.api.cgminer_port,
                        http_port = self.config.api.http_port,
                        "API servers online — dashboard at http://<miner-ip>:{}, CGMiner API on port {} (pyasic/hass-miner compatible)",
                        self.config.api.http_port,
                        self.config.api.cgminer_port,
                    );
                    Some((cgminer_handle, http_handle))
                }
                Err(e) => {
                    error!(error = %e, "Failed to start API servers — miner will run but dashboard/monitoring won't be available");
                    None
                }
            };

        let _metrics_csv_handle = crate::metrics_export::spawn_metrics_csv_task(
            shutdown.clone(),
            state_rx.clone(),
            power_rx.clone(),
        );

        let history_shutdown = shutdown.clone();
        let history_state_rx = state_rx.clone();
        let history_power_rx = power_rx.clone();
        let history_buffer_task = history_buffer.clone();
        let history_data_task = history_data.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(history::HISTORY_INTERVAL_S));
            loop {
                tokio::select! {
                    _ = history_shutdown.cancelled() => break,
                    _ = interval.tick() => {
                        let timestamp_s = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        let state = history_state_rx.borrow().clone();
                        let power = history_power_rx.borrow().clone();
                        let sample = history::sample_from_runtime(timestamp_s, &state, &power);
                        history_buffer_task.push(sample);

                        if let Ok(mut guard) = history_data_task.lock() {
                            *guard = history::serialize_for_api(&history_buffer_task.samples());
                        }
                    }
                }
            }
        });

        let autotuner_ws_shutdown = shutdown.clone();
        let autotuner_status_broadcast = autotuner_broadcast_tx.clone();
        tokio::spawn(async move {
            let mut rx = autotuner_status_rx_for_ws;
            loop {
                tokio::select! {
                    _ = autotuner_ws_shutdown.cancelled() => break,
                    result = rx.changed() => {
                        if result.is_err() {
                            break;
                        }
                        let mut status = rx.borrow().clone();
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        status.age_s = now.saturating_sub(status.last_update_s);
                        status.stale = status.live_runtime && status.age_s > 15;
                        if status.stale {
                            status.live_runtime = false;
                        }
                        if let Ok(msg) = serde_json::to_string(&serde_json::json!({
                            "type": "autotuner_status",
                            "payload": status,
                        })) {
                            let _ = autotuner_status_broadcast.send(msg);
                        }
                    }
                }
            }
        });

        let autotuner_efficiency_shutdown = shutdown.clone();
        let autotuner_efficiency_broadcast = autotuner_broadcast_tx.clone();
        tokio::spawn(async move {
            let mut rx = autotuner_efficiency_rx_for_ws;
            loop {
                tokio::select! {
                    _ = autotuner_efficiency_shutdown.cancelled() => break,
                    result = rx.changed() => {
                        if result.is_err() {
                            break;
                        }
                        if let Some(snapshot) = rx.borrow().clone() {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let age_s = if snapshot.timestamp == 0 {
                                0
                            } else {
                                now.saturating_sub(snapshot.timestamp)
                            };
                            let stale = snapshot.timestamp > 0 && age_s > 15;
                            let mut payload =
                                serde_json::to_value(&snapshot).unwrap_or_else(|_| serde_json::json!({}));
                            if let Some(obj) = payload.as_object_mut() {
                                obj.insert("source".to_string(), serde_json::json!("runtime"));
                                // POWER-PROVENANCE: the snapshot's watts are
                                // model-derived, so this `source: "runtime"`
                                // freshness label is NOT a "measured" signal.
                                // Surface the provenance from the shared
                                // authority model right next to it.
                                let power_basis =
                                    dcentrald_autotuner::PowerAuthorityKind::Estimated;
                                obj.insert(
                                    "power_basis".to_string(),
                                    serde_json::json!(power_basis.as_str()),
                                );
                                obj.insert(
                                    "modeled".to_string(),
                                    serde_json::json!(!power_basis.is_measured()),
                                );
                                obj.insert("live_runtime".to_string(), serde_json::json!(!stale));
                                obj.insert("stale".to_string(), serde_json::json!(stale));
                                obj.insert("age_s".to_string(), serde_json::json!(age_s));
                                obj.insert(
                                    "last_update_s".to_string(),
                                    serde_json::json!(snapshot.timestamp),
                                );
                                obj.insert(
                                    "message".to_string(),
                                    serde_json::json!(
                                        "Efficiency snapshot is sourced from the live autotuner background monitor."
                                    ),
                                );
                            }
                            if let Ok(msg) = serde_json::to_string(&serde_json::json!({
                                "type": "autotuner_efficiency",
                                "payload": payload,
                            })) {
                                let _ = autotuner_efficiency_broadcast.send(msg);
                            }
                        }
                    }
                }
            }
        });

        let autotuner_health_shutdown = shutdown.clone();
        let autotuner_health_broadcast = autotuner_broadcast_tx.clone();
        tokio::spawn(async move {
            let mut rx = autotuner_chip_health_rx_for_ws;
            loop {
                tokio::select! {
                    _ = autotuner_health_shutdown.cancelled() => break,
                    result = rx.changed() => {
                        if result.is_err() {
                            break;
                        }
                        if let Some(runtime) = rx.borrow().clone() {
                            let last_update_s = runtime.last_update_s;
                            let chips = runtime.chips;
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let age_s = if last_update_s == 0 {
                                0
                            } else {
                                now.saturating_sub(last_update_s)
                            };
                            let stale = last_update_s > 0 && age_s > 15;
                            if let Ok(msg) = serde_json::to_string(&serde_json::json!({
                                "type": "autotuner_chip_health",
                                "payload": {
                                    "source": "runtime",
                                    "live_runtime": !stale,
                                    "stale": stale,
                                    "age_s": age_s,
                                    "last_update_s": last_update_s,
                                    "message": "Chip health is sourced from the live autotuner background monitor.",
                                    "total_chips": chips.len(),
                                    "chips": chips,
                                },
                            })) {
                                let _ = autotuner_health_broadcast.send(msg);
                            }
                        }
                    }
                }
            }
        });

        // ---- Start notification stack (MQTT + event-bus webhook dispatcher) ----
        // P1-4 (Omega): the MQTT publisher AND the event-bus webhook dispatcher
        // (+ its mining-sync bridge) are now brought up by a single shared
        // entrypoint, `runtime::notifications::spawn_notification_stack`, so
        // every mining mode wires them identically — S9 here, plus the am2/am3
        // `--s19j-hybrid` and `--stratum-proxy` paths via
        // `runtime::api::spawn_proxy_mode_api`. The MQTT half is
        // behaviour-equivalent to the prior inline block (same 5 s live-reload,
        // same default-OFF gate); the webhook dispatcher is purely additive and
        // default-OFF (drops every event until `[webhook]` is enabled w/ a URL).
        {
            let mqtt_mac = std::fs::read_to_string("/sys/class/net/eth0/address")
                .unwrap_or_else(|_| "00:00:00:00:00:00".to_string())
                .trim()
                .to_string();
            spawn_notification_stack(
                RuntimeNotificationConfig::from_config(&self.config),
                Some(self.config_path.clone()),
                mqtt_mac,
                self.config.general.hostname.clone(),
                stats_broadcast_tx.clone(),
                mining_sync_broadcast_tx.clone(),
                self.shutdown_token.clone(),
                Some(dcentrald_api::rest::app_state_mqtt_command_sink(
                    mqtt_command_state,
                )),
            );
        }

        // ---- Start Stratum client (V1/V2 via StratumRouter) ----
        if mining_enabled {
            info!(
                // OBS-3: mask the wallet-shaped worker + strip any user:pass@ from
                // the pool URL (matches the V1 client + daemon.rs:3155); the raw
                // forms must not ride syslog tunnels.
                pool = %dcentrald_stratum::pool_api::sanitize_pool_url(&self.config.pool.url),
                worker = %dcentrald_common::wallet_mask::mask_wallet(&self.config.pool.worker),
                protocol = ?self.config.pool.protocol,
                "Connecting to mining pool — this is where your hashpower earns bitcoin"
            );
            let stratum_config = crate::config::build_stratum_config(
                &self.config,
                crate::config::stratum_donation_config(&self.config.donation),
                self.config.mining.version_rolling,
                self.config
                    .sv2
                    .channel_type
                    .eq_ignore_ascii_case("extended")
                    || self.config.job_declaration.enabled,
            );

            let stratum_router = dcentrald_stratum::StratumRouter::new(stratum_config)
                .with_job_declaration_status_rx(jd_status_rx.clone());

            tokio::spawn(async move {
                stratum_router.run(job_tx, share_rx, status_tx).await;
            });

            // Stratum status handler — updates MinerState watch channel with pool info + LED events
            let stratum_status_shutdown = shutdown.clone();
            let stratum_state_tx = state_tx.clone();
            let stratum_led_tx = self.led_tx.clone();
            let stratum_mining_sync_tx = mining_sync_broadcast_tx.clone();
            let mut stratum_power_rx = power_rx.clone();
            let stratum_recent_share_history = recent_share_history.clone();
            let autotuner_share_efficiency_tx = autotuner_share_efficiency_tx.clone();
            //  W5: clone boot_progress so the stratum status loop
            // can record the FirstShareAccepted phase when it lands.
            let stratum_boot_progress = boot_progress.clone();
            tokio::spawn(async move {
                let mut share_efficiency_tracker =
                    ShareEfficiencyTracker::new(&stratum_power_rx.borrow().clone());
                loop {
                    tokio::select! {
                        _ = stratum_status_shutdown.cancelled() => {
                            info!("Stratum status handler stopping");
                            break;
                        }
                        power_changed = stratum_power_rx.changed() => {
                            if power_changed.is_err() {
                                break;
                            }
                            let power = stratum_power_rx.borrow().clone();
                            share_efficiency_tracker.observe_power(&power);
                            let snapshot = share_efficiency_tracker.snapshot();
                            let _ = autotuner_share_efficiency_tx.send(Some(dcentrald_autotuner::AcceptedWorkSignal {
                                window_s: snapshot.window_s,
                                accepted_share_count: snapshot.accepted_share_count,
                                accepted_difficulty_sum: snapshot.accepted_difficulty_sum,
                                accepted_pool_target_difficulty_sum: snapshot.accepted_pool_target_difficulty_sum,
                                achieved_difficulty_sum: snapshot.achieved_difficulty_sum,
                                estimated_wall_energy_kwh: snapshot.estimated_wall_energy_kwh,
                                accepted_shares_per_kwh: snapshot.accepted_shares_per_kwh,
                                accepted_difficulty_per_kwh: snapshot.accepted_difficulty_per_kwh,
                                accepted_pool_target_difficulty_per_kwh: snapshot.accepted_pool_target_difficulty_per_kwh,
                                achieved_difficulty_per_kwh: snapshot.achieved_difficulty_per_kwh,
                                difficulty_source: snapshot.difficulty_source.clone(),
                                power_source: snapshot.power_source.clone(),
                                calibrated: snapshot.calibrated,
                            }));
                            stratum_state_tx.send_modify(|s| {
                                s.pool.share_efficiency = Some(snapshot.clone());
                            });
                        }
                        Some(status) = status_rx.recv() => {
                            match status {
                            dcentrald_stratum::types::StratumStatus::StateChanged(state) => {
                                let (status_str, explanation) = match state {
                                    dcentrald_stratum::types::StratumState::Disconnected => ("Disconnected", "Lost connection to pool — will auto-reconnect"),
                                    dcentrald_stratum::types::StratumState::Connecting => ("Connecting", "Establishing TCP connection to pool server"),
                                    dcentrald_stratum::types::StratumState::Authorized => ("Authorized", "Pool accepted our worker credentials — ready to receive jobs"),
                                    dcentrald_stratum::types::StratumState::Mining => ("Alive", "Actively receiving jobs and submitting shares — mining!"),
                                    dcentrald_stratum::types::StratumState::Donating => ("Donating", "Optional donation mining active (transparent, configurable)"),
                                    dcentrald_stratum::types::StratumState::AuthFailed => ("AuthFailed", "Pool REJECTED our worker credentials — check the worker name / wallet address (solo pools require a valid BTC address)"),
                                };
                                info!(state = status_str, "Pool: {}", explanation);
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.status = status_str.to_string();
                                });
                                let pool_authorized = matches!(status_str, "Authorized" | "Alive" | "Donating");
                                let authorize_state = match status_str {
                                    "Alive" => "mining",
                                    other => other,
                                }
                                .to_ascii_lowercase();
                                let _ = stratum_mining_sync_tx.send(
                                    dcentrald_api::websocket::build_mining_sync_message_with_fields(
                                        &dcentrald_api::websocket::WsMiningSyncMessage {
                                            msg_type: "mining_sync".to_string(),
                                            timestamp_ms: now_unix_ms(),
                                            event: dcentrald_api::websocket::WsMiningSyncEventKind::AuthorizeState,
                                            chain_id: None,
                                            count: Some(1),
                                            job_id: None,
                                            difficulty: None,
                                            target_difficulty: None,
                                            intensity: None,
                                            error_code: None,
                                            error_msg: None,
                                        },
                                        vec![
                                            ("pool_authorized", serde_json::json!(pool_authorized)),
                                            ("authorize_state", serde_json::json!(authorize_state)),
                                        ],
                                    ),
                                );
                                // LED pattern based on pool state
                                if let Some(ref led) = stratum_led_tx {
                                    let pattern = match state {
                                        dcentrald_stratum::types::StratumState::Disconnected => LedPattern::PoolDisconnected,
                                        dcentrald_stratum::types::StratumState::Connecting => LedPattern::Initializing,
                                        dcentrald_stratum::types::StratumState::Mining |
                                        dcentrald_stratum::types::StratumState::Donating => LedPattern::Mining,
                                        dcentrald_stratum::types::StratumState::Authorized => LedPattern::Mining,
                                        dcentrald_stratum::types::StratumState::AuthFailed => LedPattern::PoolDisconnected,
                                    };
                                    let _ = led.try_send(LedCommand::SetPattern(pattern));
                                }
                            }
                            dcentrald_stratum::types::StratumStatus::DifficultyChanged(diff) => {
                                info!(difficulty = diff, "Pool difficulty changed — this controls how hard each share is to find (lower = more shares, higher = fewer but worth more)");
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.difficulty = diff;
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::ShareAccepted { job_id, pool_target_difficulty, achieved_difficulty, meta } => {
                                let target_difficulty = if pool_target_difficulty > 0.0 {
                                    pool_target_difficulty
                                } else {
                                    stratum_state_tx.borrow().pool.difficulty
                                }
                                .max(1.0);
                                let achieved_difficulty = achieved_difficulty
                                    .filter(|value| value.is_finite() && *value > 0.0);
                                let lucky_share = achieved_difficulty
                                    .map(|difficulty| difficulty >= target_difficulty * 10.0)
                                    .unwrap_or(false);
                                let intensity = achieved_difficulty
                                    .map(|difficulty| ((difficulty / target_difficulty).min(24.0) / 24.0) as f32)
                                    .unwrap_or(0.25);
                                let _ = stratum_mining_sync_tx.send(
                                    dcentrald_api::websocket::build_mining_sync_message(
                                        &dcentrald_api::websocket::WsMiningSyncMessage {
                                            msg_type: "mining_sync".to_string(),
                                            timestamp_ms: now_unix_ms(),
                                            event: if lucky_share {
                                                dcentrald_api::websocket::WsMiningSyncEventKind::LuckyShare
                                            } else {
                                                dcentrald_api::websocket::WsMiningSyncEventKind::ShareAccepted
                                            },
                                            chain_id: None,
                                            count: Some(1),
                                            job_id: Some(job_id.clone()),
                                            difficulty: achieved_difficulty,
                                            target_difficulty: Some(target_difficulty),
                                            intensity: Some(intensity.clamp(0.1, 1.0)),
                                            error_code: None,
                                            error_msg: None,
                                        },
                                    ),
                                );
                                dcentrald_api::push_recent_share_event(
                                    &stratum_recent_share_history,
                                    dcentrald_api::RecentShareEvent {
                                        timestamp_ms: now_unix_ms(),
                                        result: "accepted".to_string(),
                                        job_id: job_id.clone(),
                                        difficulty: achieved_difficulty,
                                        target_difficulty: Some(target_difficulty),
                                        error_code: None,
                                        error_msg: None,
                                        worker_name: meta.as_ref().map(|meta| meta.share.worker_name.clone()),
                                        nonce: meta.as_ref().map(|meta| meta.share.nonce.clone()),
                                        ntime: meta.as_ref().map(|meta| meta.share.ntime.clone()),
                                        extranonce2: meta.as_ref().map(|meta| meta.share.extranonce2.clone()),
                                        version_bits: meta.as_ref().and_then(|meta| meta.share.version_bits.clone()),
                                        version: meta.as_ref().map(|meta| meta.share.version),
                                        protocol_meta_present: meta.is_some(),
                                    },
                                );
                                info!(
                                    job_id = %job_id,
                                    pool_target_difficulty = target_difficulty,
                                    achieved_difficulty,
                                    nonce = meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                                    ntime = meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                                    extranonce2 = meta.as_ref().map(|meta| meta.share.extranonce2.as_str()),
                                    version_bits = meta.as_ref().and_then(|meta| meta.share.version_bits.as_deref()),
                                    "Share ACCEPTED - pool confirmed target-difficulty work; achieved difficulty is shown only when locally proven"
                                );
                                share_efficiency_tracker.record_share(target_difficulty, achieved_difficulty, now_unix_ms());
                                let share_efficiency = share_efficiency_tracker.snapshot();
                                let _ = autotuner_share_efficiency_tx.send(Some(dcentrald_autotuner::AcceptedWorkSignal {
                                    window_s: share_efficiency.window_s,
                                    accepted_share_count: share_efficiency.accepted_share_count,
                                    accepted_difficulty_sum: share_efficiency.accepted_difficulty_sum,
                                    accepted_pool_target_difficulty_sum: share_efficiency.accepted_pool_target_difficulty_sum,
                                    achieved_difficulty_sum: share_efficiency.achieved_difficulty_sum,
                                    estimated_wall_energy_kwh: share_efficiency.estimated_wall_energy_kwh,
                                    accepted_shares_per_kwh: share_efficiency.accepted_shares_per_kwh,
                                    accepted_difficulty_per_kwh: share_efficiency.accepted_difficulty_per_kwh,
                                    accepted_pool_target_difficulty_per_kwh: share_efficiency.accepted_pool_target_difficulty_per_kwh,
                                    achieved_difficulty_per_kwh: share_efficiency.achieved_difficulty_per_kwh,
                                    difficulty_source: share_efficiency.difficulty_source.clone(),
                                    power_source: share_efficiency.power_source.clone(),
                                    calibrated: share_efficiency.calibrated,
                                }));
                                stratum_state_tx.send_modify(|s| {
                                    s.accepted += 1;
                                    s.pool.last_share_at = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    s.pool.share_efficiency = Some(share_efficiency.clone());
                                });
                                // Green flash on accepted share
                                if let Some(ref led) = stratum_led_tx {
                                    let _ = led.try_send(LedCommand::FlashGreen { duration_ms: 150 });
                                }
                                //  W5: emit FirstShareAccepted boot
                                // milestone exactly once per daemon lifetime so
                                // journalctl picks up the moment first hash
                                // landed without scanning the prose log line
                                // above.
                                static FIRST_SHARE_LOGGED: AtomicBool = AtomicBool::new(false);
                                if !FIRST_SHARE_LOGGED.swap(true, Ordering::SeqCst) {
                                    //  W5: record the FirstShareAccepted
                                    // boot phase in the shared tracker so
                                    // /api/system/boot_timeline reports it.
                                    stratum_boot_progress.record_now(
                                        dcentrald_api_types::firmware_boot_timeline::BootPhase::FirstShareAccepted,
                                    );
                                    info!(
                                        target: "boot",
                                        phase = ?dcentrald_api_types::firmware_boot_timeline::BootPhase::FirstShareAccepted,
                                        job_id = %job_id,
                                        pool_target_difficulty = target_difficulty,
                                        "DCENT_OS boot phase: first share accepted"
                                    );
                                }
                            }
                            dcentrald_stratum::types::StratumStatus::ShareRejected { job_id, error_code, error_msg, meta } => {
                                let _ = stratum_mining_sync_tx.send(
                                    dcentrald_api::websocket::build_mining_sync_message(
                                        &dcentrald_api::websocket::WsMiningSyncMessage {
                                            msg_type: "mining_sync".to_string(),
                                            timestamp_ms: now_unix_ms(),
                                            event: dcentrald_api::websocket::WsMiningSyncEventKind::ShareRejected,
                                            chain_id: None,
                                            count: Some(1),
                                            job_id: Some(job_id.clone()),
                                            difficulty: None,
                                            target_difficulty: Some(stratum_state_tx.borrow().pool.difficulty.max(1.0)),
                                            intensity: Some(0.75),
                                            error_code: Some(error_code),
                                            error_msg: Some(error_msg.clone()),
                                        },
                                    ),
                                );
                                dcentrald_api::push_recent_share_event(
                                    &stratum_recent_share_history,
                                    dcentrald_api::RecentShareEvent {
                                        timestamp_ms: now_unix_ms(),
                                        result: "rejected".to_string(),
                                        job_id: job_id.clone(),
                                        difficulty: None,
                                        target_difficulty: Some(stratum_state_tx.borrow().pool.difficulty.max(1.0)),
                                        error_code: Some(error_code),
                                        error_msg: Some(error_msg.clone()),
                                        worker_name: meta.as_ref().map(|meta| meta.share.worker_name.clone()),
                                        nonce: meta.as_ref().map(|meta| meta.share.nonce.clone()),
                                        ntime: meta.as_ref().map(|meta| meta.share.ntime.clone()),
                                        extranonce2: meta.as_ref().map(|meta| meta.share.extranonce2.clone()),
                                        version_bits: meta.as_ref().and_then(|meta| meta.share.version_bits.clone()),
                                        version: meta.as_ref().map(|meta| meta.share.version),
                                        protocol_meta_present: meta.is_some(),
                                    },
                                );
                                warn!(
                                    job_id = %job_id,
                                    error_code,
                                    error = %error_msg,
                                    nonce = meta.as_ref().map(|meta| meta.share.nonce.as_str()),
                                    ntime = meta.as_ref().map(|meta| meta.share.ntime.as_str()),
                                    version_bits = meta.as_ref().and_then(|meta| meta.share.version_bits.as_deref()),
                                    "Share REJECTED by pool — occasional rejects are normal, but many in a row could mean stale work or clock drift"
                                );
                                // Superior diagnostic observability: classify the
                                // reject into an actionable cause bucket so the
                                // operator can see WHY shares fail, not just a total.
                                let reason_idx = dcentrald_api::classify_reject_reason(
                                    error_code,
                                    &error_msg,
                                );
                                stratum_state_tx.send_modify(|s| {
                                    s.rejected += 1;
                                    if let Some(slot) = s.pool.reject_reason_counts.get_mut(reason_idx)
                                    {
                                        *slot = slot.saturating_add(1);
                                    }
                                    s.pool.reject_reason_counts_source =
                                        dcentrald_api::pool_quality_source_tag(
                                            dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                        );
                                });
                                // Red flash on rejected share
                                if let Some(ref led) = stratum_led_tx {
                                    let _ = led.try_send(LedCommand::FlashRed { duration_ms: 300 });
                                }
                            }
                            dcentrald_stratum::types::StratumStatus::PoolMessage(msg) => {
                                info!(message = %msg, "Message from pool operator");
                            }
                            dcentrald_stratum::types::StratumStatus::ReconnectRequested { host, port, wait_seconds } => {
                                info!(host = %host, port, wait_s = wait_seconds, "Pool requested reconnect to different server — load balancing or maintenance");
                            }
                            dcentrald_stratum::types::StratumStatus::PoolFailoverUpdated(failover) => {
                                tracing::debug!(
                                    active_pool = failover.active_pool_priority,
                                    event = %failover.event,
                                    switch_count = failover.switch_count,
                                    reason = ?failover.last_switch_reason,
                                    "Pool failover state updated"
                                );
                                stratum_state_tx.send_modify(|s| {
                                    if !failover.active_pool_url.is_empty() && !s.pool.donating {
                                        s.pool.url = failover.active_pool_url.clone();
                                    }
                                    s.pool.failover = failover.clone();
                                    s.pool.failover_source = dcentrald_api::pool_quality_source_tag(
                                        dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                    );
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::HashrateSplitUpdated(split) => {
                                tracing::debug!(
                                    enabled = split.enabled,
                                    route = %split.active_route,
                                    active_pool = split.active_pool_priority,
                                    remaining_s = split.cycle_remaining_s,
                                    "Hashrate split state updated"
                                );
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.hashrate_split = split.clone();
                                    s.pool.hashrate_split_source =
                                        dcentrald_api::pool_quality_source_tag(
                                            dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                        );
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::Latency(ms) => {
                                tracing::debug!(latency_ms = ms, "Pool latency");
                                // HLA-9: surface the already-measured submit->response
                                // latency into the watched pool snapshot so /api/pools +
                                // Prometheus can expose it (VNish pools[].ping parity).
                                // Previously this value was measured and only debug!'d.
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.latency_ms = ms;
                                    s.pool.latency_ms_source = dcentrald_api::pool_quality_source_tag(
                                        dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                    );
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::DonationStateChanged {
                                active,
                                percent,
                                cycle_remaining_s,
                                active_url,
                                active_worker,
                                pool_index,
                            } => {
                                if active {
                                    info!(
                                        percent,
                                        remaining_s = cycle_remaining_s,
                                        active_url = %dcentrald_stratum::pool_api::sanitize_pool_url(&active_url),
                                        // W1.4: donation fallback worker is a wallet-shaped name.
                                        active_worker = %dcentrald_common::wallet_mask::mask_wallet(&active_worker),
                                        pool_index,
                                        "Donation mining active ({:.1}%) — supporting open-source development. \
                                         Disable in Settings if you prefer.",
                                        percent,
                                    );
                                } else {
                                    info!("Returned to user pool mining");
                                }
                                // W5.5: mirror the active donation route into
                                // the API PoolState so the dashboard
                                // DonatingIndicator can render which donation
                                // pool/worker is currently carrying the slice.
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.donating = active;
                                    s.pool.donation_active_url = active_url.clone();
                                    s.pool.donation_active_worker = active_worker.clone();
                                    s.pool.donation_pool_index = pool_index;
                                    s.pool.donating_source = dcentrald_api::pool_quality_source_tag(
                                        dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                    );
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::AutoFallbackStateChanged {
                                active,
                                retry_after_s,
                                reason,
                            } => {
                                if active {
                                    warn!(
                                        retry_after_s,
                                        reason = %reason,
                                        "Auto pool mode is temporarily running on V1 fallback"
                                    );
                                } else {
                                    info!("Auto pool mode returned to the preferred endpoint");
                                }
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.auto_fallback_active = active;
                                    s.pool.auto_retry_sv2_after_s = active.then_some(retry_after_s);
                                    s.pool.auto_fallback_reason = active.then_some(reason.clone());
                                    s.pool.auto_fallback_source =
                                        dcentrald_api::pool_quality_source_tag(
                                            dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                        );
                                    if active {
                                        s.pool.protocol = "sv1".to_string();
                                        s.pool.encrypted = false;
                                        s.pool.encrypted_source =
                                            dcentrald_api::pool_quality_source_tag(
                                                dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                            );
                                        s.pool.sv2_session = None;
                                        s.pool.sv2_session_source =
                                            dcentrald_api::pool_quality_source_tag(
                                                dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                            );
                                    }
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::RollingAcceptanceUpdated {
                                pct,
                                accepted,
                                total,
                            } => {
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.rolling_acceptance_pct_30min = pct;
                                    s.pool.rolling_acceptance_count_30min = (accepted, total);
                                    s.pool.rolling_acceptance_source =
                                        dcentrald_api::pool_quality_source_tag(
                                            dcentrald_stratum::pool_quality::PoolQualitySource::LOCAL_ACCOUNTING,
                                        );
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::Sv2CustomJobDeclared {
                                channel_id,
                                request_id,
                                template_id,
                            } => {
                                info!(
                                    channel_id,
                                    request_id,
                                    template_id,
                                    "SV2 custom job declared to upstream pool"
                                );
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.sv2_custom_job = Some(dcentrald_api::Sv2CustomJobInfo {
                                        status: "declared".to_string(),
                                        channel_id: Some(channel_id),
                                        request_id: Some(request_id),
                                        template_id: Some(template_id),
                                        job_id: None,
                                        last_error: None,
                                        updated_at_s: now_unix_ms() / 1000,
                                    });
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::Sv2CustomJobAccepted {
                                channel_id,
                                request_id,
                                template_id,
                                job_id,
                            } => {
                                info!(
                                    channel_id,
                                    request_id,
                                    template_id,
                                    job_id,
                                    "SV2 custom job accepted and dispatched"
                                );
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.sv2_custom_job = Some(dcentrald_api::Sv2CustomJobInfo {
                                        status: "accepted".to_string(),
                                        channel_id: Some(channel_id),
                                        request_id: Some(request_id),
                                        template_id: Some(template_id),
                                        job_id: Some(job_id),
                                        last_error: None,
                                        updated_at_s: now_unix_ms() / 1000,
                                    });
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::Sv2CustomJobRejected {
                                channel_id,
                                request_id,
                                template_id,
                                reason,
                            } => {
                                warn!(
                                    channel_id,
                                    request_id,
                                    template_id = ?template_id,
                                    reason = %reason,
                                    "SV2 custom job rejected by upstream pool"
                                );
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.sv2_custom_job = Some(dcentrald_api::Sv2CustomJobInfo {
                                        status: "rejected".to_string(),
                                        channel_id: Some(channel_id),
                                        request_id: Some(request_id),
                                        template_id,
                                        job_id: None,
                                        last_error: Some(reason.clone()),
                                        updated_at_s: now_unix_ms() / 1000,
                                    });
                                });
                            }
                            dcentrald_stratum::types::StratumStatus::Sv2SessionUpdated {
                                cipher_suite,
                                handshake_latency_ms,
                                pool_pubkey_fingerprint,
                                certificate_valid_from,
                                certificate_not_after,
                                channel_id,
                                noise_nonce_tx,
                                noise_nonce_rx,
                                bytes_encrypted,
                                bytes_decrypted,
                                messages_sent,
                                messages_received,
                            } => {
                                info!(
                                    cipher = %cipher_suite,
                                    handshake_ms = handshake_latency_ms,
                                    pubkey = %pool_pubkey_fingerprint,
                                    channel_id = ?channel_id,
                                    nonce_tx = noise_nonce_tx,
                                    nonce_rx = noise_nonce_rx,
                                    encrypted_bytes = bytes_encrypted,
                                    decrypted_bytes = bytes_decrypted,
                                    msgs_sent = messages_sent,
                                    msgs_recv = messages_received,
                                    "SV2 session metadata update — encrypted Noise_NX transport active"
                                );
                                stratum_state_tx.send_modify(|s| {
                                    s.pool.protocol = "sv2".to_string();
                                    s.pool.encrypted = true;
                                    s.pool.encrypted_source =
                                        dcentrald_api::pool_quality_source_tag(
                                            dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                        );
                                    s.pool.sv2_session = Some(dcentrald_api::Sv2SessionInfo {
                                        cipher_suite,
                                        handshake_latency_ms,
                                        pool_pubkey_fingerprint,
                                        certificate_valid_from,
                                        certificate_not_after,
                                        channel_id,
                                        noise_nonce_tx,
                                        noise_nonce_rx,
                                        bytes_encrypted,
                                        bytes_decrypted,
                                        messages_sent,
                                        messages_received,
                                    });
                                    s.pool.sv2_session_source =
                                        dcentrald_api::pool_quality_source_tag(
                                            dcentrald_stratum::pool_quality::PoolQualitySource::STRATUM_STATUS,
                                        );
                                });
                            }
                            }
                        }
                    }
                }
            });
        } else {
            info!(
                "Mining auto-start disabled — dashboard is live, but Stratum will not start until a pool is configured and mining is explicitly enabled"
            );
        }

        // ---- Start work dispatcher ----
        // Diagnostic: log WORK_TIME per chain to confirm init_chain set it correctly
        for chain in &self.chains {
            if chain.mining {
                let wt = chain
                    .fpga
                    .common
                    .read_reg(dcentrald_hal::fpga_chain::REG_WORK_TIME);
                info!(
                    chain_id = chain.chain_id,
                    work_time = format_args!("0x{:08X}", wt),
                    "Cold boot WORK_TIME: 0x{:08X}",
                    wt,
                );
            }
        }
        // Move chains from daemon into work dispatcher (it's the sole FPGA consumer)
        let dispatch_chains = std::mem::take(&mut self.chains);
        let dispatch_chip_id = self.chip_id;
        let dispatch_shutdown = self.mining_tasks.cancellation_token();
        let dispatch_state_tx = state_tx.clone();
        let autotune_state_rx = dispatch_state_tx.subscribe();
        let worker_name = self.config.pool.worker.clone();

        // freq_cmd_tx/freq_cmd_rx already created above (before off-grid spawn).
        // Clone for thermal throttle loop.
        let thermal_freq_tx = freq_cmd_tx.clone();

        // Create autotuner stats channels if auto-tuning is enabled
        let (autotune_stats_tx, mut autotune_stats_rx) = if self.config.autotuner.enabled {
            let (stats_tx, stats_rx) = mpsc::channel::<dcentrald_autotuner::ChipStatsSnapshot>(256);
            (Some(stats_tx), Some(stats_rx))
        } else {
            (None, None)
        };

        let autotune_window_s = self.config.autotuner.measurement_window_s;

        // Gap 1: runtime voltage command channel (std::sync::mpsc for OS thread).
        // The dispatcher and thermal loop send typed platform-aware voltage commands,
        // and the runtime heartbeat thread applies them during its quiet I2C window.
        // SAFETY (wave 8, 2026-04-28): bounded — see voltage_cmd_tx field comment.
        // Capacity 64 chosen to absorb a short I2C stall (~10ms/cmd × 64 = ~640ms)
        // without blocking the dispatcher loop or growing RAM unboundedly.
        let (voltage_cmd_tx, voltage_cmd_rx) = voltage_command_mailbox();
        self.voltage_cmd_tx = Some(voltage_cmd_tx.clone());
        // Clone sender for thermal emergency (cloned BEFORE move to dispatcher)
        let thermal_voltage_tx = self.voltage_cmd_tx.clone();
        let thermal_pic_addrs: Vec<u8> = self.pic_addrs()?.to_vec();
        let voltage_cmd_tx = self.voltage_cmd_tx.clone();

        // Gap 2: Shared XADC temperature for autotuner snapshots.
        // Thermal loop writes die temp, work dispatcher reads it into snapshots.
        let shared_xadc_temp = Arc::new(AtomicU32::new(0));
        let thermal_emergency_latch = Arc::new(AtomicBool::new(false));

        // Collect chain info before moving chains to dispatcher (autotuner needs this)
        let autotuner_pic_fw_byte = match self.pic_firmware {
            PicFirmware::Stock(fw) => Some(fw),
            PicFirmware::BraiinsOs => Some(0x03),
            PicFirmware::Unknown => None,
        };
        let chain_eeprom_fingerprints = if matches!(self.pic_type()?, PicType::NoPic) {
            (0..dispatch_chains.len())
                .map(|slot| {
                    self.bootstrap_eeprom_fingerprints
                        .get(slot)
                        .cloned()
                        .flatten()
                })
                .collect()
        } else {
            Vec::new()
        };
        // Wave K Lane B: observe-only hashboard-SKU classification. Reuse the
        // same read-only sysfs EEPROM path to grab the preamble + classify the
        // SKU per chain (the "HAL handles multiple hashboards" mixed-SKU audit).
        // TELEMETRY/LOG ONLY — this does NOT select an active silicon profile or
        // drive freq/voltage (that is the deferred matrix §7 #15 power-adjacent
        // work). Unknown preamble → "unclassified", never a guess.
        // CE-011: alongside the telemetry-only Wave-K classification, build a
        // chain_id -> Bm1362HashboardSku map so the autotuner can register a
        // CEILING-ONLY per-SKU PVT clamp (`AutoTuner::set_chain_sku`). Live
        // no-op today (NoPic is Amlogic BM1366/BM1368, which
        // `hashboard_to_bm1362_sku` maps to None), but completes the SKU->tuner
        // wiring so a future BM1362-on-NoPic path is backstopped. Declared
        // unconditionally so it is in scope at the autotuner spawn below.
        let mut autotuner_chain_skus: std::collections::HashMap<
            u8,
            dcentrald_silicon_profiles::bm1362::Bm1362HashboardSku,
        > = std::collections::HashMap::new();
        if matches!(self.pic_type()?, PicType::NoPic) {
            for slot in 0..dispatch_chains.len() {
                match self.bootstrap_eeprom_preambles.get(slot).copied().flatten() {
                    Some(preamble) => {
                        let sku =
                            dcentrald_silicon_profiles::hashboards::classify_by_eeprom_preamble(
                                preamble,
                            );
                        info!(
                            slot,
                            preamble = format_args!("0x{:02x} 0x{:02x}", preamble[0], preamble[1]),
                            sku = ?sku,
                            "Wave K: hashboard SKU classified from EEPROM preamble (telemetry only — no profile selection, no voltage change)"
                        );
                        // CE-011: record the BM1362 PVT-bearing SKU (if any) for
                        // ceiling-only autotuner registration. Non-BM1362 /
                        // table-less boards map to None and are skipped.
                        if let Some(bm) =
                            sku.and_then(dcentrald_autotuner::pvt_envelope::hashboard_to_bm1362_sku)
                        {
                            autotuner_chain_skus.insert(dispatch_chains[slot].chain_id, bm);
                        }
                    }
                    None => info!(
                        slot,
                        "Wave K: hashboard EEPROM preamble unavailable — SKU unclassified"
                    ),
                }
            }
        }
        let chain_infos: Vec<dcentrald_autotuner::ChainTuneInfo> = dispatch_chains
            .iter()
            .enumerate()
            .filter(|(_, c)| c.mining)
            .map(|(slot, c)| dcentrald_autotuner::ChainTuneInfo {
                chain_id: c.chain_id,
                chip_count: c.chip_count,
                voltage_mv: c.voltage_mv,
                chip_id: c.chip_id,
                hardware_identity: dcentrald_autotuner::ChainHardwareIdentity {
                    eeprom_serial: None,
                    eeprom_fingerprint: chain_eeprom_fingerprints
                        .get(slot)
                        .and_then(|fingerprint| fingerprint.clone()),
                    dspic_fw_byte: c.pic_address.and(autotuner_pic_fw_byte),
                },
            })
            .collect();

        // Deferred voltage reduction targets: chains left at 9400mV during init
        // because post-open-core I2C noise prevents 4-byte PIC writes.
        let deferred_voltage_targets: Vec<(u8, u8, u128)> = dispatch_chains
            .iter()
            .filter(|c| c.mining && c.voltage_mv >= 9400)
            .filter_map(|c| {
                c.pic_address.map(|pic_addr| {
                    let generation = self
                        .voltage_cmd_tx
                        .as_ref()
                        .expect("voltage mailbox was installed above")
                        .capture_generation(self.chip_id, pic_addr)
                        .expect("new voltage mailbox has no pending disable or terminal latch");
                    (c.chain_id, pic_addr, generation)
                })
            })
            .collect();

        // FIX (swarm review 2026-03-26): use hardware TicketMask difficulty,
        // NOT pool suggest_difficulty. This must come from the miner profile so
        // non-BM1387 families keep truthful hashrate math too.
        let hw_difficulty: u64 = MinerProfile::for_chip(self.chip_id)
            .map(|profile| profile.hardware_difficulty as u64)
            .unwrap_or(256);
        // v0.15.4: Send heartbeats right BEFORE work dispatcher starts.
        // The work dispatcher's initial burst of FPGA WORK_TX writes permanently
        // wedges PICs that receive I2C during AXI contention. By heartbeating
        // NOW (while AXI is quiet), we maximize the watchdog margin. PICs won't
        // need another heartbeat for 64 seconds, by which time the work dispatch
        // has settled into a steady-state rhythm.
        {
            let predispatch_i2c_fw = match self.pic_firmware {
                PicFirmware::BraiinsOs => dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs,
                PicFirmware::Stock(_) => dcentrald_hal::i2c::I2cPicFirmware::Stock,
                PicFirmware::Unknown => dcentrald_hal::i2c::I2cPicFirmware::Unknown,
            };

            if matches!(self.pic_type()?, PicType::Pic16F1704) {
                let predispatch_i2c_service = self.i2c_service.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "pre-dispatch heartbeat: I2C service not initialized before start_mining()"
                    )
                })?;
                let predispatch_async_i2c = predispatch_i2c_service.async_handle();
                for &addr in &self.initialized_pic_addrs_final {
                    match predispatch_async_i2c
                        .heartbeat(addr, predispatch_i2c_fw)
                        .await
                    {
                        Ok(()) => info!("PRE_DISPATCH_HB: PIC 0x{:02X} OK", addr),
                        Err(e) => warn!("PRE_DISPATCH_HB: PIC 0x{:02X} FAIL: {}", addr, e),
                    }
                }
            } else {
                let predispatch_i2c_service = self.i2c_service.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "pre-dispatch dsPIC heartbeat: I2C service not initialized before start_mining()"
                    )
                })?;
                let _ = predispatch_i2c_service.set_timeout(10);
                for &addr in &self.initialized_pic_addrs_final {
                    let mut dspic = DspicService::new(predispatch_i2c_service.clone(), addr);
                    match dspic.send_heartbeat() {
                        Ok(()) => info!("PRE_DISPATCH_HB: dsPIC 0x{:02X} OK", addr),
                        Err(e) => warn!("PRE_DISPATCH_HB: dsPIC 0x{:02X} FAIL: {}", addr, e),
                    }
                }
            }
        }

        let i2c_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let i2c_active_for_heartbeat = i2c_active.clone();

        // Per-chain board temperatures via BM1387 I2C passthrough.
        // The WorkDispatcher reads these every 5s and stores f32 bits.
        // The thermal loop reads them for per-chain thermal control.
        let board_temps: Vec<Arc<AtomicU32>> = dispatch_chains
            .iter()
            .map(|_| Arc::new(AtomicU32::new(0)))
            .collect();
        let board_temps_thermal: Vec<Arc<AtomicU32>> = board_temps.to_vec();
        let board_temps_heartbeat: Vec<Arc<AtomicU32>> = board_temps.to_vec();
        let board_temps_autotune: Vec<Arc<AtomicU32>> = board_temps.to_vec();
        let board_temp_seen_at: Vec<Arc<AtomicU32>> = dispatch_chains
            .iter()
            .map(|_| Arc::new(AtomicU32::new(0)))
            .collect();
        let board_temp_seen_at_thermal: Vec<Arc<AtomicU32>> = board_temp_seen_at.to_vec();
        let board_temp_seen_at_heartbeat: Vec<Arc<AtomicU32>> = board_temp_seen_at.to_vec();
        let board_temp_seen_at_autotune: Vec<Arc<AtomicU32>> = board_temp_seen_at.to_vec();
        let board_temp_time_base = Arc::new(Instant::now());
        let board_temp_time_base_autotune = board_temp_time_base.clone();
        let board_temp_time_base_thermal = board_temp_time_base.clone();
        let board_temp_time_base_heartbeat = board_temp_time_base.clone();

        // PSU efficiency: depends on PSU model and input voltage.
        // With PSU override, use model-specific efficiency values.
        // Without override, prefer the declared circuit voltage from onboarding.
        // Fall back to the older watt-based heuristic only when voltage is unknown.
        let declared_circuit_voltage = self.config.power.circuit_voltage_v;
        let dc_source_profile = matches!(
            self.config.power.source_profile.as_deref(),
            Some("direct_dc") | Some("solar_battery")
        );
        let psu_efficiency = if let Some(ref ovr) = self.config.power.psu_override {
            if ovr.enabled {
                psu_efficiency_for_model_name(&ovr.model).unwrap_or(0.88)
            } else if dc_source_profile {
                1.0
            } else if matches!(declared_circuit_voltage, Some(v) if v <= 120) {
                0.88
            } else if matches!(declared_circuit_voltage, Some(v) if v >= 200) {
                0.93
            } else if self.config.power.circuit_capacity_watts <= 1800 {
                0.88
            } else {
                0.93
            }
        } else if dc_source_profile {
            1.0
        } else if matches!(declared_circuit_voltage, Some(v) if v <= 120) {
            0.88
        } else if matches!(declared_circuit_voltage, Some(v) if v >= 200) {
            0.93
        } else if self.config.power.circuit_capacity_watts <= 1800 {
            0.88
        } else {
            0.93
        };

        let expected_dispatcher_composition = dispatch_chains
            .iter()
            .filter(|chain| chain.mining)
            .map(|chain| ExpectedMiningChain {
                chain_id: chain.chain_id,
                chip_count: chain.chip_count,
            })
            .collect();
        let dispatcher_enumeration_receipts = self
            .asic_enumeration_receipts
            .iter()
            .copied()
            .filter(|receipt| {
                dispatch_chains
                    .iter()
                    .any(|chain| chain.mining && chain.chain_id == receipt.chain_id())
            })
            .collect();
        let identity_publication_port = match self.dispatcher_composition_authority.activate(
            expected_dispatcher_composition,
            dispatcher_enumeration_receipts,
            Arc::clone(&hardware_info),
        ) {
            Ok(port) => Some(port),
            Err(error) => {
                warn!(
                    error = %error,
                    "Measured ASIC identity publication is not armed for this dispatcher composition"
                );
                None
            }
        };

        let mut dispatcher = crate::work_dispatcher::WorkDispatcher::new(
            job_rx,
            share_tx,
            dispatch_state_tx,
            mining_sync_broadcast_tx.clone(),
            dispatch_shutdown,
            worker_name,
            dispatch_chains,
            dispatch_chip_id,
            self.asic_driver_execution_policy,
            hw_difficulty,
            autotune_stats_tx,
            Some(freq_cmd_rx),
            autotune_window_s,
            voltage_cmd_tx,
            shared_xadc_temp.clone(),
            i2c_active,
            board_temps,
            board_temp_seen_at,
            board_temp_time_base,
            self.led_tx.clone(),
            power_tx.clone(),
            psu_efficiency,
            power_calibration.clone(),
            curtailment_sleeping.clone(),
            self.config.mining.skip_board_temp,
        );
        dispatcher.set_asic_identity_publication_port(identity_publication_port);
        let circuit_capacity = if dc_source_profile {
            None
        } else {
            Some(self.config.power.circuit_capacity_watts)
        };
        dispatcher.set_circuit_capacity(circuit_capacity);
        //  W1: install the shared local-reject ring so the
        // dispatcher can push diagnostic entries on every reject.
        dispatcher.set_local_reject_ring(dispatcher_local_reject_ring);
        //  W1: install the stale-age divisor from MiningConfig.
        // Default 4 (= 64-cycle threshold for BM1387's 8-bit ring) per
        // the analysis in
        dispatcher.set_stale_age_divisor(self.config.mining.stale_age_divisor);
        if !self.mining_tasks.spawn("work-dispatcher", async move {
            dispatcher.run().await;
        }) {
            anyhow::bail!(
                "work dispatcher task ownership already exists; refusing detached replacement"
            );
        }

        // ---- Stage-A OBSERVE-ONLY DPS-governor shadow (DEFAULT-OFF) ----
        //
        // When `DCENT_DPS_GOVERNOR_SHADOW` is truthy, spawn a read-only task
        // that periodically (~30 s) builds a `DpsTick` from EXISTING live state
        // (autotuner status string + die/board temps + fan PWM + configured
        // power target), feeds it to the built-but-not-driven `DpsGovernor`, and
        // LOGS the returned `DpsAction`. It NEVER acts on the action — no
        // freq/power/fan/PSU command, no setter, no I2C/UART/GPIO. This lets an
        // operator compare what DPS power-scaling WOULD decide vs reality on a
        // live soak BEFORE the separately soak+operator-gated Stage-B flip where
        // DPS actually drives. Mirrors the observe-only shadow pattern used to
        // study the pool-failover FSM before it was allowed to drive.
        //
        // ZERO-FOOTPRINT default: when the flag is unset this whole block is a
        // no-op — the governor is never constructed, no task is spawned, nothing
        // is logged, and the captured clones below are never created. Behaviour
        // is byte-identical to the prior code. The input clones MUST be taken
        // HERE (before the auto-tuner closure below moves the temp atomics /
        // time-base) so the shadow is independent of whether the autotuner runs.
        if dps_governor_shadow_enabled() {
            use dcentrald_api_types::braiinsos_dps_configuration::{
                DpsConfiguration, DpsThermalProfile, SustainedBelowHotCounter,
            };

            // Read-only clones of the shared live state. Cloning a
            // `Vec<Arc<AtomicU32>>` clones the Arcs (same underlying atomics) so
            // the shadow OBSERVES the identical samples the rest of the daemon
            // writes — it never mutates them.
            let shadow_xadc_temp = shared_xadc_temp.clone();
            let shadow_board_temps: Vec<Arc<AtomicU32>> = board_temps_autotune.clone();
            let shadow_board_seen_at: Vec<Arc<AtomicU32>> = board_temp_seen_at_autotune.clone();
            let shadow_time_base = board_temp_time_base_autotune.clone();
            // Fan PWM (0..100) is published into the shared mining status; the
            // status watch sender is clonable via `.subscribe()`.
            let shadow_state_rx = state_tx.subscribe();
            // Tuner FSM state is published as a string in the autotuner status
            // watch; subscribe read-only.
            let shadow_status_rx = autotuner_status_tx.subscribe();
            let shadow_shutdown = shutdown.clone();
            // Same chip-id resolution the auto-tuner gate uses below
            // (`effective_chip_id` is defined later in this fn, so recompute the
            // identical expression here to keep the shadow self-contained).
            let shadow_chip_id = self.config.mining.model_chip_id().unwrap_or(self.chip_id);
            let shadow_configured_watts = self.config.power.target_watts;
            // DPS profile the real governor would use for this chip family.
            let shadow_thermal_profile: DpsThermalProfile =
                dps_thermal_profile_for_chip(shadow_chip_id);

            info!(
                chip_id = format_args!("0x{:04X}", shadow_chip_id),
                thermal_profile = ?shadow_thermal_profile,
                configured_power_target_watts = shadow_configured_watts,
                env_flag = ENV_DPS_GOVERNOR_SHADOW,
                "DPS-governor OBSERVE-ONLY shadow ENABLED — DpsActions are LOGGED \
                 only, NEVER actuated (no freq/power/fan/PSU command). Stage-B \
                 (DPS actually drives) is separately soak + operator gated."
            );

            tokio::spawn(async move {
                // DPS config for the shadow: enabled so the FSM actually
                // evaluates (the OUTER env flag is the real on/off switch), with
                // documented S19j-class anchors. `shutdown_enabled = false` so
                // the shadow FSM never even *suggests* a Shutdown action; even if
                // it did, the action is only ever logged. NO persistence path is
                // set (`DpsGovernor::new` leaves it None), so the shadow writes
                // NOTHING to disk.
                let dps_config = DpsConfiguration {
                    enabled: Some(true),
                    power_step_watts: 300,
                    hashrate_step_ths: 11.0,
                    min_power_target_watts: 943,
                    min_hashrate_target_ths: 70.7417,
                    shutdown_enabled: Some(false),
                    shutdown_duration_hours: 3,
                    mode: None,
                    on_start_target_percent: Some(100),
                    min_psu_power_budget: None,
                    hashboard_idx: None,
                };
                let walker = dcentrald_autotuner::dps::DpsWalkerConfig::default();
                let mut governor = dcentrald_autotuner::dps_governor::DpsGovernor::new(
                    dps_config,
                    shadow_thermal_profile,
                    walker,
                );

                let mut tuner_clock =
                    dcentrald_autotuner::tuner_stability::TunerStabilityClock::new();
                let mut below_hot = SustainedBelowHotCounter::new();
                let (_target_c, hot_c, _dangerous_c) = shadow_thermal_profile.thresholds();

                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last_tick = std::time::Instant::now();

                loop {
                    tokio::select! {
                        _ = shadow_shutdown.cancelled() => {
                            info!("DPS-governor shadow observer stopping");
                            return;
                        }
                        _ = ticker.tick() => {
                            let now = std::time::Instant::now();
                            let elapsed_secs =
                                now.saturating_duration_since(last_tick).as_secs();
                            last_tick = now;

                            // --- Read live state (READ-ONLY) ---
                            // Die temp (XADC), f32 bits in the shared atomic.
                            let die_temp_c =
                                f32::from_bits(shadow_xadc_temp.load(Ordering::Acquire));
                            // Max fresh board temp (best-effort). Falls back to 0.
                            let now_s = shadow_time_base.elapsed().as_secs() as u32;
                            let mut max_board_temp_c = 0.0f32;
                            for (temp_atomic, seen_at_atomic) in
                                shadow_board_temps.iter().zip(shadow_board_seen_at.iter())
                            {
                                let bits = temp_atomic.load(Ordering::Acquire);
                                let seen_at_s = seen_at_atomic.load(Ordering::Acquire);
                                if bits == 0 || seen_at_s == 0 {
                                    continue;
                                }
                                let temp_c = f32::from_bits(bits);
                                let fresh = now_s.saturating_sub(seen_at_s)
                                    <= dcentrald_autotuner::chip_stats::BOARD_TEMP_STALE_TIMEOUT_S
                                        as u32;
                                if fresh && temp_c > 0.0 && temp_c < 150.0 && temp_c > max_board_temp_c {
                                    max_board_temp_c = temp_c;
                                }
                            }
                            // Board temp the governor reasons on: max fresh board
                            // temp, else fall back to die temp (mirrors the
                            // thermal supervisor's die-temp fallback). 0.0 if
                            // neither is available — the governor treats a low
                            // temp as "cool", which for an observe-only shadow is
                            // the safe (no-spurious-scale-down) reading.
                            let board_temp_c = if max_board_temp_c > 0.0 {
                                max_board_temp_c
                            } else if die_temp_c > 0.0 && die_temp_c < 125.0 {
                                die_temp_c
                            } else {
                                0.0
                            };
                            let chip_temp_c = if die_temp_c > 0.0 && die_temp_c < 125.0 {
                                die_temp_c
                            } else {
                                0.0
                            };

                            // Fan PWM percent from the shared mining status.
                            let fan_speed_pct = shadow_state_rx.borrow().fans.pwm;

                            // Tuner-stable minutes from the published status
                            // string → TunerState → stability clock. Unknown /
                            // synthetic strings ("disabled") map to Idle, which
                            // resets the clock (treated as "not stable").
                            let status = shadow_status_rx.borrow().clone();
                            let tuner_state =
                                dcentrald_autotuner::tuner_stability::tuner_state_from_status_str(
                                    &status.state,
                                )
                                .unwrap_or(dcentrald_autotuner::tuner::TunerState::Idle);
                            let tuner_stable_minutes = tuner_clock.observe(tuner_state);

                            // Sustained-below-hot minutes. A chip temp of <= 0.0
                            // means "unavailable" (sentinel) and is treated as
                            // not-hot, so the absence of a chip sensor never
                            // spuriously resets the below-hot clock.
                            let is_below_hot = board_temp_c < hot_c
                                && (chip_temp_c <= 0.0 || chip_temp_c < hot_c);
                            let sustained_below_hot_minutes =
                                below_hot.observe(is_below_hot, elapsed_secs);

                            // Configured power target. The shadow does NOT track a
                            // live scaled target (DPS isn't driving), so the
                            // "current" target IS the configured target.
                            let tick = dcentrald_autotuner::dps_governor::DpsTick {
                                board_temp_c,
                                chip_temp_c,
                                fan_speed_pct,
                                sustained_below_hot_minutes,
                                tuner_stable_minutes,
                                current_power_target_watts: shadow_configured_watts,
                                configured_power_target_watts: shadow_configured_watts,
                            };

                            // OBSERVE-ONLY: evaluate the FSM and LOG. The returned
                            // action is never executed — no setter, no hardware.
                            let action = governor.tick(&tick, elapsed_secs as u32);
                            info!(
                                target: "dps_shadow",
                                dps_state = ?governor.state(),
                                action = ?action,
                                board_temp_c,
                                chip_temp_c,
                                fan_speed_pct,
                                sustained_below_hot_minutes,
                                tuner_stable_minutes,
                                configured_power_target_watts = shadow_configured_watts,
                                "DPS shadow (OBSERVE-ONLY): action LOGGED, NOT actuated"
                            );
                        }
                    }
                }
            });
        }

        // ---- Stage-A OBSERVE-ONLY TunerDriver shadow (DEFAULT-OFF) ----
        //
        // When `DCENT_TUNER_DRIVER_SHADOW` is truthy, spawn a read-only task
        // that periodically (~30 s) builds a `TelemetrySample` from EXISTING
        // live state (live MinerState watch: per-chain voltage/freq/chip-count,
        // total hashrate, fan PWM → the no-HAL V²f power estimate), feeds it to
        // the built-but-not-driven 6-variant `crate::autotune::TunerDriver`
        // (`TunerMode` configured by the operator's `[autotune] mode = ...`),
        // and LOGS the returned `TunerOutcome`. It NEVER acts on the outcome —
        // no freq/voltage/power/fan/PSU command, no setter, no I2C/UART/GPIO.
        // This lets an operator compare what the unified TunerMode strategy
        // WOULD decide vs reality on a live soak. The TunerDriver actually
        // driving frequency/voltage is the live `AutoTuner` path — wholly
        // separate, operator-gated, and untouched here. Mirrors the observe-only
        // shadow shipped for the DpsGovernor.
        //
        // ZERO-FOOTPRINT default: when the flag is unset this whole block is a
        // no-op — the driver is never constructed, no task is spawned, nothing
        // is logged, and the captured clones below are never created. Behaviour
        // is byte-identical to the prior code.
        if tuner_driver_shadow_enabled() {
            use crate::autotune::{TunerDriver, TunerMode, TunerOutcome};
            use dcentrald_api_types::power_model::TunerShadowTelemetry;

            // Read-only subscribe to the live mining-status watch — same source
            // the DpsGovernor shadow uses for fan PWM. Cloning the watch
            // receiver OBSERVES the identical state the rest of the daemon
            // publishes; it never mutates it.
            let shadow_state_rx = state_tx.subscribe();
            let shadow_shutdown = shutdown.clone();
            // Same chip-id resolution the DpsGovernor shadow + autotuner gate use.
            let shadow_chip_id = self.config.mining.model_chip_id().unwrap_or(self.chip_id);
            // Operator-configured TunerMode (`[autotune] mode = "..."`). This is
            // exactly the strategy the live AutoTuner would honour — the shadow
            // observes "what would THIS mode decide" without acting.
            let shadow_mode: TunerMode = self.config.autotune.mode.clone();
            // Seed the driver with the configured on-chip operating point so the
            // first tick doesn't slam a band edge. A neutral `Manual { 0, 0 }`
            // default mode is re-seeded to the live freq/voltage.
            let seed_freq_mhz = self.config.mining.frequency_mhz;
            let seed_voltage_mv = if self.config.mining.voltage_mv > 0 {
                self.config.mining.voltage_mv
            } else {
                13_700
            };
            let shadow_seed_mode = match shadow_mode {
                TunerMode::Manual {
                    freq_mhz: 0,
                    voltage_mv: 0,
                    ..
                } => TunerMode::default_manual_at(seed_freq_mhz, seed_voltage_mv),
                other => other,
            };

            info!(
                chip_id = format_args!("0x{:04X}", shadow_chip_id),
                tuner_mode = ?shadow_seed_mode,
                seed_freq_mhz,
                seed_voltage_mv,
                env_flag = ENV_TUNER_DRIVER_SHADOW,
                "TunerDriver OBSERVE-ONLY shadow ENABLED — TunerOutcomes are \
                 LOGGED only, NEVER actuated (no freq/voltage/power/fan/PSU \
                 command). The live AutoTuner path is separate + operator-gated."
            );

            tokio::spawn(async move {
                // Construct the driver from the operator's TunerMode + the
                // configured on-chip seed. The driver owns NO hardware handle:
                // `step()` is pure arithmetic returning a TunerOutcome the
                // shadow only logs.
                let mut driver = TunerDriver::new(shadow_seed_mode, seed_freq_mhz, seed_voltage_mv);

                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                loop {
                    tokio::select! {
                        _ = shadow_shutdown.cancelled() => {
                            info!("TunerDriver shadow observer stopping");
                            return;
                        }
                        _ = ticker.tick() => {
                            // --- Read live state (READ-ONLY) ---
                            // Snapshot the live mining status once, then drop the
                            // borrow before any await-free compute.
                            let (hashrate_ghs, fan_pwm, chains): (
                                f64,
                                u8,
                                Vec<(u16, u16, u32)>,
                            ) = {
                                let s = shadow_state_rx.borrow();
                                let chains: Vec<(u16, u16, u32)> = s
                                    .chains
                                    .iter()
                                    .map(|c| (c.voltage_mv, c.frequency_mhz, c.chips as u32))
                                    .collect();
                                (s.hashrate_ghs, s.fans.pwm, chains)
                            };

                            // Derive the four TelemetrySample inputs purely from
                            // the live state via the no-HAL V²f power model. Fans:
                            // fan count/RPM aren't needed for the TunerDriver's
                            // decisions (only PWM is), so pass a zero fan-power
                            // term — the power estimate stays board-level and the
                            // shadow only logs. The PWM percent is carried through
                            // verbatim for the Heater fan-cap check.
                            let telemetry = TunerShadowTelemetry::from_live_state(
                                shadow_chip_id,
                                hashrate_ghs,
                                fan_pwm,
                                (0, 0, 6000),
                                &chains,
                            );

                            let sample = crate::autotune::TelemetrySample {
                                actual_watts: telemetry.actual_watts,
                                hashrate_ths: telemetry.hashrate_ths,
                                voltage_mv: telemetry.voltage_mv,
                                fan_pwm: telemetry.fan_pwm,
                            };

                            // OBSERVE-ONLY: evaluate the strategy and LOG. The
                            // returned outcome is never executed — no setter, no
                            // hardware. `outcome` appears only here and in the
                            // info! line below.
                            let outcome: TunerOutcome = driver.step(sample);
                            info!(
                                target: "tuner_shadow",
                                tuner_mode = ?driver.mode(),
                                outcome = ?outcome,
                                driver_freq_mhz = driver.current_freq_mhz(),
                                driver_voltage_mv = driver.current_voltage_mv(),
                                actual_watts = telemetry.actual_watts,
                                hashrate_ths = telemetry.hashrate_ths,
                                voltage_mv = telemetry.voltage_mv,
                                fan_pwm = telemetry.fan_pwm,
                                "TunerDriver shadow (OBSERVE-ONLY): outcome LOGGED, NOT actuated"
                            );
                        }
                    }
                }
            });
        }

        // ---- Stage-A OBSERVE-ONLY VnishPhaseAdapter shadow (DEFAULT-OFF) ----
        //
        // When `DCENT_VNISH_PHASE_SHADOW` is truthy, spawn a read-only task that
        // periodically (~30 s) builds an `AutotuneObservation` from EXISTING live
        // state (live MinerState watch: per-chain voltage → max-rail mV*10,
        // cumulative chain CRC errors → per-tick delta, total hashrate → fraction
        // of the configured target) and feeds it to the built-but-not-driven
        // `dcentrald_autotuner::vnish_phase_fsm::VnishPhaseAdapter` (the VNish
        // 5-phase voltage-walk FSM), then LOGS the returned `VnishTuneAction`. It
        // NEVER acts on the action — no freq/voltage/power/fan/PSU command, no
        // setter, no I2C/UART/GPIO. This lets an operator compare what the VNish
        // phase strategy WOULD decide vs reality on a live soak. The phase FSM
        // actually driving voltage/freq is its own separate, soak- +
        // operator-gated Stage-B flip (the adapter's `[autotune.vnish_phase]
        // .enabled` TOML gate), untouched here. Mirrors the observe-only shadows
        // shipped for the DpsGovernor + TunerDriver.
        //
        // The FSM is CLOCK-FREE by contract (the caller threads time — see
        // `AutotuneObservation::timed_wait_done`): the shadow threads time by
        // setting `timed_wait_done = true` once per ~30 s tick, which is exactly
        // the "settle window elapsed" semantic the FSM expects.
        //
        // ZERO-FOOTPRINT default: when the flag is unset this whole block is a
        // no-op — the adapter is never constructed, no task is spawned, nothing
        // is logged, and the captured clones below are never created. Behaviour
        // is byte-identical to the prior code.
        if vnish_phase_shadow_enabled() {
            use dcentrald_api_types::autotune_phase::{
                hashrate_ratio, AutotuneObservation, HwErrorDeltaCounter,
            };
            use dcentrald_autotuner::vnish_phase_fsm::{
                VnishPhaseAdapter, VnishPhaseConfig, VnishTuneAction,
            };

            // Read-only subscribe to the live mining-status watch — the same
            // source the DpsGovernor + TunerDriver shadows use. Cloning the
            // watch receiver OBSERVES the identical state the rest of the daemon
            // publishes; it never mutates it.
            let shadow_state_rx = state_tx.subscribe();
            let shadow_shutdown = shutdown.clone();
            let shadow_chip_id = self.config.mining.model_chip_id().unwrap_or(self.chip_id);
            // Operator-configured hashrate target (TH/s) if any. The FSM's
            // `hashrate_ratio` needs a preset target; when none is configured
            // the shadow self-references its first non-zero live hashrate as the
            // baseline (honest: there is no operator preset to compare against,
            // so "ratio vs the run's own steady-state" is the only meaningful
            // observe-only reading).
            let shadow_configured_target_ths: Option<f64> =
                self.config.psu.hashrate_target_ths.filter(|t| *t > 0.0);

            info!(
                chip_id = format_args!("0x{:04X}", shadow_chip_id),
                configured_target_ths = ?shadow_configured_target_ths,
                env_flag = ENV_VNISH_PHASE_SHADOW,
                "VnishPhaseAdapter OBSERVE-ONLY shadow ENABLED — VnishTuneActions \
                 are LOGGED only, NEVER actuated (no freq/voltage/power/fan/PSU \
                 command). The Stage-B flip (FSM actually drives) is its own \
                 [autotune.vnish_phase].enabled gate, separate + operator-gated."
            );

            tokio::spawn(async move {
                // Construct the adapter ENABLED so the FSM actually evaluates
                // (the OUTER env flag is the real on/off switch), with the
                // VNish 1.2.7 catalog default constants. The adapter owns NO
                // hardware handle: `observe()` is pure arithmetic returning a
                // VnishTuneAction the shadow only logs, and writes NOTHING to
                // disk (no persistence path exists in the adapter).
                let cfg = VnishPhaseConfig {
                    enabled: true,
                    ..VnishPhaseConfig::default()
                };
                let mut adapter = VnishPhaseAdapter::new(cfg);

                let mut hw_err_delta = HwErrorDeltaCounter::new();
                // Self-referenced hashrate baseline used only when no operator
                // target is configured (see comment above). Latched to the
                // first observed non-zero live hashrate.
                let mut baseline_ths: Option<f64> = None;
                // The FSM advances out of Idle on the first observation with
                // `operator_started = true`; the shadow self-drives that once.
                let mut started = false;

                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                loop {
                    tokio::select! {
                        _ = shadow_shutdown.cancelled() => {
                            info!("VnishPhaseAdapter shadow observer stopping");
                            return;
                        }
                        _ = ticker.tick() => {
                            // --- Read live state (READ-ONLY) ---
                            // Snapshot the live mining status once, then drop the
                            // borrow before the await-free compute below.
                            let (hashrate_ghs, max_voltage_mv, cumulative_errors): (
                                f64,
                                u16,
                                u32,
                            ) = {
                                let s = shadow_state_rx.borrow();
                                let max_voltage_mv =
                                    s.chains.iter().map(|c| c.voltage_mv).max().unwrap_or(0);
                                let cumulative_errors: u32 = s
                                    .chains
                                    .iter()
                                    .fold(0u32, |acc, c| acc.saturating_add(c.errors));
                                (s.hashrate_ghs, max_voltage_mv, cumulative_errors)
                            };

                            let current_ths = hashrate_ghs / 1000.0;
                            // Latch the self-referenced baseline on first non-zero
                            // hashrate (only used when no operator target exists).
                            if baseline_ths.is_none() && current_ths > 0.0 {
                                baseline_ths = Some(current_ths);
                            }
                            let target_ths = shadow_configured_target_ths
                                .or(baseline_ths)
                                .unwrap_or(0.0);

                            // Derive the AutotuneObservation purely from live
                            // state via the no-HAL helpers.
                            // * voltage_mv10: VNish uses mV*10 (1640 = 16.40 V);
                            //   the live rail is mV, so ×10.
                            // * hw_errors_sum: per-tick delta of cumulative CRC
                            //   errors (the FSM wants "since last tick").
                            // * hashrate_ratio: current/target as a fraction.
                            // * timed_wait_done: the shadow threads time — each
                            //   ~30 s tick IS the settle window elapsing.
                            // * operator_started: self-driven true (the shadow
                            //   is the "operator" for an observe-only run).
                            // * hard_fault: ALWAYS false — the shadow never
                            //   injects a fault (it would only ever be LOGGED
                            //   anyway, never actuated).
                            let obs = AutotuneObservation {
                                operator_started: true,
                                voltage_mv10: (max_voltage_mv as u32).saturating_mul(10),
                                hw_errors_sum: hw_err_delta.observe(cumulative_errors),
                                hashrate_ratio: hashrate_ratio(current_ths, target_ths),
                                phase4_converged: false,
                                timed_wait_done: started,
                                hard_fault: false,
                            };
                            started = true;

                            // OBSERVE-ONLY: evaluate the FSM and LOG. The returned
                            // action is never executed — no setter, no hardware.
                            // `action` appears only here and in the info! line.
                            let action: VnishTuneAction = adapter.observe(obs);
                            info!(
                                target: "vnish_phase_shadow",
                                phase = ?adapter.phase(),
                                phase4_round = adapter.phase4_round(),
                                action = ?action,
                                voltage_mv10 = obs.voltage_mv10,
                                hw_errors_sum = obs.hw_errors_sum,
                                hashrate_ratio = obs.hashrate_ratio,
                                current_ths,
                                target_ths,
                                "VnishPhaseAdapter shadow (OBSERVE-ONLY): action LOGGED, NOT actuated"
                            );
                        }
                    }
                }
            });
        }

        // ---- Start auto-tuner task (after work dispatcher is running) ----
        // Phase 2: The auto-tuner communicates with WorkDispatcher via channels.
        // stats_rx receives per-chip nonce/error snapshots.
        // freq_cmd_tx sends frequency change commands that the dispatcher applies.
        // freq_cmd_tx is shared with the thermal throttle loop (cloned earlier).
        // ----  am2/BM1362 frequency-only autotuner gate ----
        //
        // The am2 BM1362 family (S19j Pro Zynq, dsPIC per-chain voltage)
        // historically had ZERO live autotuning: voltage-opt / DVFS were
        // hard-gated to S9/BM1387+PIC16, and BM1362 frequency tuning was
        // never opted into. BraiinsOS's only real practical lead is that
        // its tuner runs on S19j Pro in production. This wave closes the
        // gap with a deliberately conservative FREQUENCY-ONLY layer.
        //
        // BRICK-CRITICAL discipline (`a lab unit` is a live home unit):
        //  * Default OFF — without an explicit operator opt-in
        //    (`[autotuner] am2_frequency_autotune = true` OR
        //    `DCENT_AM2_FREQUENCY_AUTOTUNE=1`) the autotuner is NOT
        //    spawned for am2/BM1362 at all → byte-identical behavior to
        //    today on the proven `a lab unit` mining path (zero regression).
        //  * When opted in: voltage_optimization / dvfs are HARD-pinned
        //    false (no live voltage write this wave) and the frequency
        //    search band is clamped to the nameplate `[245, 545]` MHz
        //    window — never above 545 on a home unit.
        //
        // Other families (S9/BM1387, am3-aml, am3-bb) are NOT affected
        // by this gate — `am2_bm1362_family` is false for them.
        let effective_chip_id = self.config.mining.model_chip_id().unwrap_or(self.chip_id);
        let am2_bm1362_family =
            effective_chip_id == 0x1362 && matches!(self.pic_type()?, PicType::DsPic33EP);
        let am2_freq_autotune_opted_in = self.config.autotuner.am2_frequency_autotune_enabled(
            std::env::var(dcentrald_autotuner::config::AM2_FREQUENCY_AUTOTUNE_ENV)
                .ok()
                .as_deref(),
        );
        if am2_bm1362_family && !am2_freq_autotune_opted_in {
            // Gate CLOSED. Drop the stats receiver so the autotuner is
            // not spawned for this family. This is the zero-regression
            // default for the live `a lab unit` / XIL home unit.
            if autotune_stats_rx.is_some() {
                info!(
                    chip_id = format_args!("0x{:04X}", effective_chip_id),
                    "am2/BM1362 frequency-only autotuner is DISABLED by default. \
                     Set [autotuner] am2_frequency_autotune = true (or \
                     DCENT_AM2_FREQUENCY_AUTOTUNE=1) to opt in to FREQUENCY-ONLY \
                     TABS tuning (no live voltage write on am2 this wave)."
                );
            }
            autotune_stats_rx = None;
        }

        // ---- W24-BC-1 (): bad-chip supervisor tee (DEFAULT-OFF) ----
        //
        // When `[autotune.bad_chip].enabled = true`, interpose a TELEMETRY-FIRST
        // observer between the work dispatcher's `ChipStatsSnapshot` mpsc and the
        // autotuner. The observer feeds each per-chain snapshot into
        // `BadChipSupervisor::observe()` and LOGS the resulting `BadChipAction`s,
        // then forwards the snapshot UNCHANGED to the autotuner so per-chip
        // characterization is unaffected.
        //
        // SAFETY / default-off contract (load-bearing):
        //   * The supervisor is constructed and the tee task is spawned ONLY when
        //     `self.config.autotune.bad_chip.enabled` is true. When it is false
        //     (the default — and the case for an absent `[autotune.bad_chip]`
        //     block) this whole block is a no-op: `autotune_stats_rx` is passed to
        //     the autotuner unchanged, `observe()` is NEVER called, and the channel
        //     wiring is byte-identical to today. Zero behavior change on the proven
        //     live `a lab unit` / `a lab unit` am2 path.
        //   * ACTUATION IS DEFERRED. This pass is telemetry-first: NONE of the
        //     emitted `BadChipAction`s (PerChipDownclock / BlacklistChip /
        //     ReduceBoardProfile / BoardReset / HaltMining) are wired to a control
        //     surface yet — they are logged only. Actuating per-chip downclock /
        //     blacklist / bounded board-reset / halt is Wave-H work behind operator
        //     per-action authorization, and the supervisor's math (rolling window,
        //     per-chain healthy-chip floor) must be live-validated first. A
        //     half-actuated default-off path is safe; an unsafe actuation is not.
        //   * The supervisor NEVER emits a fan-control action (enforced by the
        //     `BadChipAction` enum + the supervisor's own structural test) — the
        //     quiet-home cut-hash-before-noise cap is untouched.
        //   * Per-chip observation only runs when the autotuner is also enabled,
        //     because the only `ChipStatsSnapshot` stream the daemon produces is the
        //     dispatcher mpsc feeding the autotuner. We deliberately do NOT add a
        //     second telemetry pipeline this pass. With the autotuner disabled there
        //     is no stream to observe and the supervisor stays dormant — still
        //     default-off-correct.
        let bad_chip_cfg = self.config.autotune.bad_chip.clone();
        let autotune_stats_rx = if bad_chip_cfg.enabled && autotune_stats_rx.is_some() {
            let original_rx = autotune_stats_rx
                .take()
                .expect("checked is_some() above for the bad-chip tee");
            // The tuner consumes a fresh receiver; we forward observed snapshots
            // into this tee channel so the autotuner sees the identical stream.
            let (tee_tx, tee_rx) = mpsc::channel::<dcentrald_autotuner::ChipStatsSnapshot>(256);

            // Per-chain board fingerprints (platform/model/chip_count) so the
            // supervisor can key persistence + Missing detection per chain. EEPROM
            // hash is left None here (read-only fingerprinting is a later wiring
            // step); a None hash simply means a fingerprint mismatch can't discard a
            // persisted blacklist — irrelevant while actuation is deferred.
            let platform_key = match self.platform_identity.platform_marker() {
                "" => "unknown".to_string(),
                marker => marker.to_string(),
            };
            let model_key = self
                .config
                .mining
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let fingerprints: Vec<dcentrald_autotuner::BoardFingerprint> = chain_infos
                .iter()
                .map(|c| dcentrald_autotuner::BoardFingerprint {
                    platform: platform_key.clone(),
                    model: model_key.clone(),
                    chain_id: c.chain_id,
                    chip_count: c.chip_count as u16,
                    eeprom_hash16: None,
                })
                .collect();

            // Per-chain chip_id for the expected-nonce estimate (telemetry-only).
            let chain_chip_ids: std::collections::HashMap<u8, u16> = chain_infos
                .iter()
                .map(|c| (c.chain_id, c.chip_id))
                .collect();
            let bad_chip_nominal_mhz = self.config.mining.frequency_mhz;

            let bad_chip_shutdown = shutdown.clone();
            info!(
                chains = fingerprints.len(),
                "W24-BC-1: bad-chip supervisor ENABLED (telemetry-first — actions \
                 are LOGGED only this pass, NOT actuated; per-chip downclock / \
                 blacklist / board-reset / halt actuation is Wave-H operator-gated)"
            );

            let mut supervisor =
                dcentrald_autotuner::BadChipSupervisor::new(bad_chip_cfg, fingerprints);

            tokio::spawn(async move {
                let mut original_rx = original_rx;
                loop {
                    tokio::select! {
                        _ = bad_chip_shutdown.cancelled() => {
                            info!("W24-BC-1: bad-chip supervisor observer stopping");
                            return;
                        }
                        maybe_snapshot = original_rx.recv() => {
                            let Some(snapshot) = maybe_snapshot else {
                                // Dispatcher dropped the sender — mining stopped.
                                return;
                            };

                            // Expected nonces per chip over this window (telemetry
                            // estimate): expected_nps(chip_id, freq, diff) ×
                            // window_seconds. Uses the same public chip-geometry
                            // helper the autotuner uses; an approximate nominal freq
                            // is acceptable because actuation is deferred and the
                            // supervisor's own min_samples confidence gate guards
                            // against noise.
                            let chip_id = chain_chip_ids
                                .get(&snapshot.chain_id)
                                .copied()
                                .unwrap_or(0);
                            let diff = if snapshot.current_difficulty == 0 {
                                256
                            } else {
                                snapshot.current_difficulty
                            };
                            let nps = dcentrald_autotuner::chip_geometry::expected_nps_for_chip(
                                chip_id,
                                bad_chip_nominal_mhz,
                                diff,
                            );
                            let expected_per_chip =
                                nps * snapshot.window_duration_s.max(1.0);

                            let actions = supervisor.observe(&snapshot, expected_per_chip);
                            for action in &actions {
                                match action {
                                    dcentrald_autotuner::BadChipAction::NoOp => {}
                                    other => {
                                        // TELEMETRY-ONLY: logged, never actuated this
                                        // pass. See the default-off contract above.
                                        warn!(
                                            chain_id = snapshot.chain_id,
                                            action = ?other,
                                            "W24-BC-1: bad-chip supervisor action (NOT actuated — telemetry-first)"
                                        );
                                    }
                                }
                            }

                            // Forward the snapshot UNCHANGED to the autotuner. If the
                            // autotuner's receiver is gone, exit the observer.
                            if tee_tx.send(snapshot).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });

            Some(tee_rx)
        } else {
            autotune_stats_rx
        };

        if let Some(stats_rx) = autotune_stats_rx {
            let mut autotune_config = self.config.autotuner.clone();

            //  am2/BM1362 frequency-only PIN.
            //
            // Reached only when `am2_bm1362_family && opted-in` (the
            // closed-gate case already dropped `autotune_stats_rx`
            // above and never enters this block). This is the single
            // load-bearing transform that makes this wave SAFE on the
            // live `a lab unit` home unit:
            //  * voltage_optimization = false  (HARD — no voltage write)
            //  * dvfs_enabled         = false  (HARD — DVFS ⇒ voltage)
            //  * freq band clamped to [245, 545] MHz (home-safe, no
            //    above-nameplate exploration).
            // Applied BEFORE the legacy dvfs/voltage gate below so the
            // legacy gate sees already-safe values (idempotent).
            if am2_bm1362_family {
                // PERF-004: SKU-aware autotune ceiling.
                //
                // Default (gate unset) keeps the load-bearing Standard 545-MHz
                // pin → byte-identical to the historical `a lab unit`/`a lab unit` behavior.
                // ONLY when the operator opts in via the default-OFF
                // `DCENT_AM2_SKU_AWARE_CEILING` gate do we classify the LIVE
                // hashboard SKU (from the read-only EEPROM SKU label) and widen
                // the ceiling to that class's value (mid-band/high-bin → 597,
                // still PLL-lockable). An unknown/standard SKU label classifies
                // back to `Standard`, so even with the gate on a `a lab unit`/`a lab unit`
                // BHB42601 home unit keeps the 545 ceiling — the gate cannot
                // auto-promote a board the EEPROM doesn't corroborate.
                let sku_class = if am2_sku_aware_ceiling_enabled() {
                    let label = self.bootstrap_hb_type.clone();
                    let class = label
                        .as_deref()
                        .map(dcentrald_autotuner::Bm1362SkuClass::from_sku_label)
                        .unwrap_or_default();
                    info!(
                        sku_label = label.as_deref().unwrap_or("<none>"),
                        ?class,
                        ceiling_mhz = class.max_freq_mhz(),
                        "PERF-004: SKU-aware ceiling opted in (DCENT_AM2_SKU_AWARE_CEILING=1) — \
                         classified live hashboard SKU"
                    );
                    class
                } else {
                    dcentrald_autotuner::Bm1362SkuClass::Standard
                };
                autotune_config.pin_am2_bm1362_frequency_only_for_sku(sku_class);
                info!(
                    chip_id = format_args!("0x{:04X}", effective_chip_id),
                    ?sku_class,
                    freq_band = format_args!(
                        "{}-{} MHz",
                        autotune_config.min_freq_mhz, autotune_config.max_freq_mhz
                    ),
                    voltage_optimization = autotune_config.voltage_optimization,
                    dvfs_enabled = autotune_config.dvfs_enabled,
                    "am2/BM1362 FREQUENCY-ONLY autotuner opted in: voltage/DVFS \
                     HARD-pinned off, frequency search clamped to the SKU-class \
                     band (Standard=545). Voltage co-opt is a separate later wave."
                );
            }

            // W1.3 — Mode-aware tune target.
            //
            // `TuneTarget::default()` is now `Efficiency` (was `Hashrate`)
            // so home miners optimize the J/TH bill instead of the
            // leaderboard. Hacker mode opts back into `Hashrate` because
            // raw-register users explicitly asked for the leaderboard.
            // We only override when the loaded config still has the
            // structural default — operator TOML overrides (`target_mode =
            // "power"`, `"hashrate_target"`, or an explicit `"hashrate"` /
            // `"efficiency"`) are preserved.
            //
            // Donation default = 2% (operator-locked). NOT touched here.
            let mode_str = self.config.mode.active.as_str();
            if matches!(
                autotune_config.target_mode,
                dcentrald_autotuner::config::TuneTarget::Efficiency
            ) {
                let mode_default = dcentrald_autotuner::config::TuneTarget::for_mode(mode_str);
                if mode_default != autotune_config.target_mode {
                    info!(
                        operating_mode = %mode_str,
                        old = ?autotune_config.target_mode,
                        new = ?mode_default,
                        "Autotuner target_mode adjusted by operating-mode default \
                         (W1.3 — Heater/Mining → Efficiency, Hacker → Hashrate)"
                    );
                    autotune_config.target_mode = mode_default;
                }
            }

            let pic_type = self.pic_type()?;
            // PERF-006/011: honor the default-OFF `DCENT_AM2_VOLTAGE_AUTOTUNE`
            // gate. Gate unset ⇒ identical conservative capability set as
            // `autotuner_capabilities_for_chip` (byte-identical behavior).
            let autotune_capabilities =
                dcentrald_autotuner::autotuner_capabilities_for_chip_with_voltage_autotune(
                    self.config.mining.model_chip_id().unwrap_or(self.chip_id),
                    match pic_type {
                        PicType::Pic16F1704 => "pic16",
                        PicType::DsPic33EP => "dspic",
                        PicType::NoPic => "nopic",
                    },
                    std::env::var(dcentrald_autotuner::AM2_VOLTAGE_AUTOTUNE_ENV)
                        .ok()
                        .as_deref(),
                );
            if autotune_config.dvfs_enabled && !autotune_capabilities.dvfs_runtime_supported {
                warn!(
                    capability_profile = %autotune_capabilities.profile_key,
                    "Autotuner DVFS requested but this family/controller path does not support live DVFS yet — disabling it for truthful behavior"
                );
                autotune_config.dvfs_enabled = false;
            }
            if autotune_config.voltage_optimization
                && (self.config.mining.model_chip_id().unwrap_or(self.chip_id) != 0x1387
                    || !matches!(pic_type, PicType::Pic16F1704))
            {
                warn!(
                    chip_id = format_args!("0x{:04X}", self.config.mining.model_chip_id().unwrap_or(self.chip_id)),
                    ?pic_type,
                    "Autotuner runtime voltage optimization is currently limited to BM1387/PIC16 until other controller paths have a proven live-safe implementation"
                );
                autotune_config.voltage_optimization = false;
            }
            let autotune_shutdown = shutdown.clone();
            let mut autotune_state_rx = autotune_state_rx;
            let nominal_mhz = self.config.mining.frequency_mhz;
            let autotuner_status_watch = autotuner_status_tx.clone();
            let autotuner_efficiency_watch = autotuner_efficiency_tx.clone();
            let autotuner_chip_health_watch = autotuner_chip_health_tx.clone();
            let autotuner_telemetry_watch = autotuner_telemetry_tx.clone();
            let autotuner_command_rx = autotuner_command_rx;
            let chip_type = {
                let registry =
                    ChipRegistry::with_execution_policy(self.asic_driver_execution_policy);
                registry
                    .detect(self.chip_id)
                    .map(|d| d.chip_name().to_string())
                    .unwrap_or_else(|| format!("0x{:04X}", self.chip_id))
            };

            info!(
                enabled = autotune_config.enabled,
                target_mode = ?autotune_config.target_mode,
                measurement_s = autotune_config.measurement_window_s,
                error_threshold = format_args!("{}%", autotune_config.error_threshold_pct),
                safety_margin = format_args!("{}%", autotune_config.safety_margin_pct),
                freq_range = format_args!("{}-{} MHz", autotune_config.min_freq_mhz, autotune_config.max_freq_mhz),
                "Auto-tuner enabled: TABS per-chip frequency characterization with thermal refinement."
            );

            let chain_infos_clone = chain_infos.clone();
            let autotune_freq_tx = freq_cmd_tx.clone();
            let autotune_power_calibration = power_calibration.clone();
            let autotune_xadc_temp = shared_xadc_temp.clone();
            tokio::spawn(async move {
                let mut tuner = dcentrald_autotuner::AutoTuner::new(
                    autotune_config,
                    nominal_mhz,
                    chip_type,
                    match pic_type {
                        PicType::Pic16F1704 => "pic16".to_string(),
                        PicType::DsPic33EP => "dspic".to_string(),
                        PicType::NoPic => "nopic".to_string(),
                    },
                    autotune_power_calibration,
                );
                // CE-011: register any classified BM1362 SKU so `AutoTuner::run`
                // tightens the frequency CEILING to the SKU's PVT envelope max
                // (ceiling-only; never raises the ceiling, never touches the
                // floor). Empty map (the live default — Wave-K is NoPic/BM1366)
                // => no registration => byte-identical to today's behavior.
                for (&chain_id, &sku) in &autotuner_chain_skus {
                    tuner.set_chain_sku(chain_id, sku);
                }
                tuner.set_runtime_status_watch(autotuner_status_watch);
                tuner.set_efficiency_watch(autotuner_efficiency_watch);
                tuner.set_chip_health_watch(autotuner_chip_health_watch);
                tuner.set_telemetry_watch(autotuner_telemetry_watch);
                tuner.set_accepted_work_watch(autotuner_share_efficiency_rx);
                tuner.set_command_receiver(autotuner_command_rx);

                // Wait for real mining readiness before starting characterization.
                // A fixed sleep is not enough on S9 handoff paths where zero nonces or
                // missing board-temp samples can linger briefly after startup.
                let require_board_temp_gate = matches!(
                    pic_type,
                    PicType::Pic16F1704 | PicType::DsPic33EP | PicType::NoPic
                );
                let mut stable_hashrate_ticks = 0u8;
                let mut readiness_tick = tokio::time::interval(std::time::Duration::from_secs(5));
                readiness_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = autotune_shutdown.cancelled() => {
                            info!("Auto-tuner stopping before characterization started");
                            return;
                        }
                        _ = readiness_tick.tick() => {
                            let state = autotune_state_rx.borrow().clone();
                            let now_s = board_temp_time_base_autotune.elapsed().as_secs() as u32;
                            let fresh_board_temp_count = board_temps_autotune
                                .iter()
                                .zip(board_temp_seen_at_autotune.iter())
                                .filter(|(temp_atomic, seen_at_atomic)| {
                                    let bits = temp_atomic.load(Ordering::Acquire);
                                    let seen_at_s = seen_at_atomic.load(Ordering::Acquire);
                                    if bits == 0 || seen_at_s == 0 {
                                        return false;
                                    }
                                    let temp_c = f32::from_bits(bits);
                                    temp_c > 0.0
                                        && temp_c < 150.0
                                        && now_s.saturating_sub(seen_at_s)
                                            <= dcentrald_autotuner::chip_stats::BOARD_TEMP_STALE_TIMEOUT_S
                                                as u32
                                })
                                .count();

                            let die_temp_c = f32::from_bits(autotune_xadc_temp.load(Ordering::Acquire));
                            let has_valid_die_temp = die_temp_c > 0.0 && die_temp_c < 125.0;
                            let has_valid_temp = if require_board_temp_gate {
                                (fresh_board_temp_count >= chain_infos_clone.len() && !chain_infos_clone.is_empty())
                                    || has_valid_die_temp
                            } else {
                                true
                            };

                            let telemetry_ready = state.hashrate_5s_ghs > 0.0
                                && has_valid_temp;

                            if telemetry_ready {
                                stable_hashrate_ticks = stable_hashrate_ticks.saturating_add(1);
                            } else {
                                stable_hashrate_ticks = 0;
                            }

                            if stable_hashrate_ticks >= 2 {
                                break;
                            }
                        }
                    }
                }

                info!("Auto-tuner: Mining stable, beginning per-chip characterization...");

                // Run the full auto-tuner lifecycle via channel-based architecture
                tuner
                    .run(
                        &chain_infos_clone,
                        stats_rx,
                        autotune_freq_tx,
                        autotune_shutdown,
                    )
                    .await;
            });
        }

        // ---- Start watchdog kicker task ----
        // The hardware watchdog reboots the miner if dcentrald crashes. We "kick" it
        // periodically to prove we're alive. If we stop kicking (crash), the SoC
        // reboots automatically — this prevents a bricked miner from sitting idle.
        // NEW-4 (2026-06-10 adversarial pass): open the watchdog HERE (after init),
        // not in init Phase 1 (see the deferral note there). Open + set_timeout +
        // an immediate kick + the kicker loop all happen together, so the DTB-10s
        // window can never fire during the slow hardware init. Shared with the
        // hybrid / serial / am3-bb mining entry paths via `spawn_watchdog_kicker`
        // (one implementation; config-gated; inert on `a lab unit` where it is disabled).
        // Thermal-liveness clock for the WDT kicker: the thermal control loop
        // below increments this every tick, and the kicker withholds the WDT kick
        // if it stops advancing — so a hung thermal loop (the case where boards
        // stay energized with NO thermal supervision) triggers a SoC reboot rather
        // than being fed forever. The other mining modes retain path-local
        // liveness counters but still need migration to this owned lifecycle.
        let thermal_liveness = Arc::new(AtomicU64::new(0));
        let (watchdog_intent_tx, watchdog_intent_rx) = watch::channel(WatchdogIntent::Mining);
        let (watchdog_receipt_tx, watchdog_receipt_rx) = oneshot::channel();
        let watchdog_owner_shutdown = self.watchdog_tasks.cancellation_token();
        let watchdog_liveness_interval = Duration::from_secs_f32(thermal_pid_interval_secs(
            self.config.thermal.pid_interval_s,
        ));
        let watchdog_future = owned_watchdog_kicker(
            self.config.watchdog.clone(),
            watchdog_liveness_interval,
            watchdog_owner_shutdown,
            watchdog_intent_rx,
            thermal_liveness.clone(),
            watchdog_receipt_tx,
        );
        if !self
            .watchdog_tasks
            .spawn("soc-watchdog-kicker", watchdog_future)
        {
            anyhow::bail!(
                "SoC watchdog task ownership is unavailable; refusing unowned watchdog supervision"
            );
        }
        self.watchdog_intent_tx = Some(watchdog_intent_tx);
        self.watchdog_receipt_rx = Some(watchdog_receipt_rx);

        // v0.12.0: ZERO devmem AXI IIC register writes. Kernel driver is sole owner.
        //
        // Root cause (proven by 40+ agents across 3 days): init code used devmem to
        // write AXI IIC registers (GIE=0, SOFTR, timing). This corrupted the kernel
        // xiic driver's internal state machine. BraiinsOS NEVER touches AXI IIC via
        // devmem — the kernel driver manages everything.
        //
        // Fix: remove restore_kernel_i2c_interrupts() entirely. The kernel driver
        // sets GIE, IER, and timing on its own during xiic_reinit(). Don't interfere.

        // v0.13.0: I2C service already spawned at start of init(). Use the stored handle.
        let i2c_service = self.i2c_service.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "I2C service not initialized — init() must spawn it before start_mining()"
            )
        })?;

        // ---- Start PIC heartbeat task (CRITICAL for voltage safety) ----
        // Each hash board has a PIC microcontroller that controls voltage. The PIC
        // has an internal watchdog — if it doesn't receive a heartbeat every ~5
        // seconds (stock Bitmain PIC) or ~10 seconds (BraiinsOS PIC), it cuts
        // power to the hash board. This is an intentional hardware safety feature
        // that prevents a crashed miner from overheating hash boards.
        // We MUST send heartbeats every 1 second to keep voltage flowing.
        // GRACEFUL SHUTDOWN FIX: The heartbeat thread uses a SEPARATE shutdown token
        // (heartbeat_shutdown_token) that is cancelled AFTER voltage is disabled in
        // shutdown(). Previously it used the global shutdown_token, which caused heartbeats
        // to stop BEFORE voltage disable, leaving a gap where PICs could watchdog-timeout.
        let hb_shutdown = self.heartbeat_shutdown_token.clone();
        // Fix G: Only heartbeat PICs that actually initialized successfully.
        // detected_board_indices includes boards with dead PICs (GPIO plug detect
        // doesn't verify PIC health). Dead PICs waste I2C time with EIO errors.
        let hb_pic_addrs: Vec<u8> = self.initialized_pic_addrs_final.clone();
        let hb_pic_firmware = self.pic_firmware;
        let mut runtime_heartbeat_ready_rx = None;

        if !hb_pic_addrs.is_empty() {
            let pic_count = hb_pic_addrs.len();
            let addrs_hex: Vec<String> = hb_pic_addrs
                .iter()
                .map(|a| format!("0x{:02X}", a))
                .collect();
            info!(
                pic_count,
                addresses = %addrs_hex.join(", "),
                firmware = %hb_pic_firmware,
                "v0.11.0: PIC heartbeat via I2C service — {} PIC(s), channel-serialized, single fd",
                pic_count,
            );
            // v0.11.0: Heartbeat thread uses the I2C SERVICE HANDLE instead of opening
            // its own fd. The I2C service thread (spawned above) owns the ONLY fd to
            // /dev/i2c-0. All I2C goes through the mpsc channel. This matches BraiinsOS's
            // AsyncI2cDev architecture and eliminates concurrent fd access that corrupted
            // the kernel xiic adapter state (root cause of 2/3 PIC heartbeat loss).
            let hb_shutdown_flag = hb_shutdown.clone();
            let hb_i2c_svc = i2c_service.clone();
            let hb_board_temps = board_temps_heartbeat;
            let hb_board_temp_seen_at = board_temp_seen_at_heartbeat;
            let hb_board_temp_time_base = board_temp_time_base_heartbeat;
            let hb_pic_chain_map: std::collections::HashMap<u8, usize> = self
                .chains
                .iter()
                .enumerate()
                .filter_map(|(idx, chain)| chain.pic_address.map(|addr| (addr, idx)))
                .collect();
            let hb_i2c_fw = match hb_pic_firmware {
                PicFirmware::BraiinsOs => dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs,
                PicFirmware::Stock(_) => dcentrald_hal::i2c::I2cPicFirmware::Stock,
                PicFirmware::Unknown => dcentrald_hal::i2c::I2cPicFirmware::Unknown,
            };
            let hb_pic_type = self.pic_type()?;
            let hb_chip_id = self.chip_id;
            let hb_i2c_active = i2c_active_for_heartbeat;
            let hb_thermal_emergency_latch = thermal_emergency_latch.clone();
            let hb_deferred_target_mv = self.config.mining.voltage_mv;
            let hb_deferred_targets = deferred_voltage_targets.clone();
            let (runtime_ready_tx, runtime_ready_rx) = tokio::sync::oneshot::channel();
            let runtime_heartbeat_handle = std::thread::Builder::new()
                .name("pic-heartbeat".to_string())
                .spawn(move || {
                    let mut tick: u64 = 0;
                    let mut runtime_ready_tx = Some(runtime_ready_tx);
                    let mut stable_heartbeat_ticks: u64 = 0;
                    let mut pending_voltage_targets = hb_deferred_targets;
                    let mut consecutive_failures: std::collections::HashMap<u8, u32> = hb_pic_addrs.iter()
                        .map(|&a| (a, 0u32)).collect();
                    // WAVE-0: per-PIC heartbeat back-off. A PIC that NACKs is moved
                    // out of the hot path after PIC_BACKOFF_FAIL_THRESHOLD fails and
                    // reprobed every PIC_BACKOFF_REPROBE_SECS instead of being
                    // hammered ~33x/s. `consecutive_failures` above is preserved for
                    // the deferred-voltage stable-tick accounting; `pic_backoff`
                    // drives WHETHER we poke the bus and rate-limits the FAIL log.
                    let hb_thread_start = std::time::Instant::now();
                    let mut pic_backoff: std::collections::HashMap<u8, PicBackoff> = hb_pic_addrs
                        .iter()
                        .map(|&a| (a, PicBackoff::new()))
                        .collect();

                    loop {
                        if hb_shutdown_flag.is_cancelled() {
                            info!("PIC heartbeat stopping — voltage controllers will auto-shutdown via their hardware watchdog (~5-64s, controller-dependent)");
                            break;
                        }

                        tick += 1;
                        let mut cycle_heartbeat_succeeded = false;

                        if matches!(hb_pic_type, PicType::Pic16F1704) {
                            // Signal work dispatcher to pause FPGA AXI reads.
                            // v0.9.7 proved: nonce RX AXI reads on shared GP0 port cause
                            // PICs to NACK (ISR=0xD2). 135ms pause loses zero nonces.
                            hb_i2c_active.store(true, std::sync::atomic::Ordering::Release);
                            std::thread::sleep(Duration::from_millis(15));
                            while let Ok(delivery) = voltage_cmd_rx.try_recv() {
                                let (cmd, completion) = delivery.into_parts();
                                if hb_shutdown_flag.is_cancelled() {
                                    completion.complete(Err(
                                        "runtime heartbeat is stopping; command was not executed"
                                            .to_string(),
                                    ));
                                    break;
                                }
                                let (result, reply_tx): (
                                    std::result::Result<VoltageCommandReply, String>,
                                    Option<tokio::sync::oneshot::Sender<std::result::Result<VoltageCommandReply, String>>>,
                                ) = match cmd {
                                    VoltageCommand::SetVoltage { chain_id, chip_id, pic_addr, target_mv, reply_tx } => {
                                        if voltage_cmd_rx.is_terminal_latched() {
                                            warn!(
                                                target_mv,
                                                pic_addr = format_args!("0x{:02X}", pic_addr),
                                                "Runtime voltage BLOCKED: terminal safe-off is latched"
                                            );
                                            (Err("terminal safe-off latched; refusing SetVoltage".to_string()), reply_tx)
                                        } else if thermal_emergency_active(&hb_thermal_emergency_latch) {
                                            warn!(
                                                target_mv,
                                                pic_addr = format_args!("0x{:02X}", pic_addr),
                                                "Runtime voltage BLOCKED: thermal emergency active"
                                            );
                                            (Err("thermal emergency active; refusing SetVoltage".to_string()), reply_tx)
                                        } else if stable_heartbeat_ticks < 5 {
                                            warn!(
                                                stable_heartbeat_ticks,
                                                target_mv,
                                                pic_addr = format_args!("0x{:02X}", pic_addr),
                                                "Runtime voltage BLOCKED: PIC heartbeat not stable (need 5 ticks, have {})",
                                                stable_heartbeat_ticks,
                                            );
                                            (Err(format!("PIC heartbeat not stable: {} < 5 ticks", stable_heartbeat_ticks)), reply_tx)
                                        } else {
                                        let pic_type = MinerProfile::for_chip(chip_id)
                                            .map(|profile| profile.pic_type)
                                            .unwrap_or(hb_pic_type);
                                        let result = match pic_type {
                                            PicType::Pic16F1704 => {
                                                let pic_val = PicController::voltage_to_pic(target_mv as f64 / 1000.0);
                                                hb_i2c_svc
                                                    .set_voltage(pic_addr, hb_i2c_fw, pic_val)
                                                    .map(|_| {
                                                        info!(
                                                            chain_id = ?chain_id,
                                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                                            target_mv,
                                                            pic_val,
                                                            "Runtime voltage apply: PIC16 target committed"
                                                        );
                                                        VoltageCommandReply::Applied(target_mv)
                                                    })
                                                    .map_err(|e: dcentrald_hal::HalError| e.to_string())
                                            }
                                            _ => Err("Runtime heartbeat service is in PIC16 mode; non-PIC16 voltage apply is unsupported on this path".to_string()),
                                        };
                                        (result, reply_tx)
                                        }
                                    }
                                    VoltageCommand::DisableVoltage { chain_id, chip_id, pic_addr, reply_tx } => {
                                        let pic_type = MinerProfile::for_chip(chip_id)
                                            .map(|profile| profile.pic_type)
                                            .unwrap_or(hb_pic_type);
                                        let result = match pic_type {
                                            PicType::Pic16F1704 => {
                                                hb_i2c_svc
                                                    .disable_voltage(pic_addr, hb_i2c_fw)
                                                    .map(|_| {
                                                        warn!(
                                                            chain_id = ?chain_id,
                                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                                            "Runtime voltage disable: PIC16 output disabled"
                                                        );
                                                        VoltageCommandReply::Disabled
                                                    })
                                                    .map_err(|e: dcentrald_hal::HalError| e.to_string())
                                            }
                                            _ => Err("Runtime heartbeat service is in PIC16 mode; non-PIC16 voltage disable is unsupported on this path".to_string()),
                                        };
                                        (result, reply_tx)
                                    }
                                    VoltageCommand::VerifyVoltage { chain_id, chip_id, pic_addr, target_mv, reply_tx } => {
                                        let pic_type = MinerProfile::for_chip(chip_id)
                                            .map(|profile| profile.pic_type)
                                            .unwrap_or(hb_pic_type);
                                        let result = match pic_type {
                                            PicType::Pic16F1704 => {
                                                info!(
                                                    chain_id = ?chain_id,
                                                    pic_addr = format_args!("0x{:02X}", pic_addr),
                                                    target_mv,
                                                    firmware = %hb_pic_firmware,
                                                    "Runtime voltage verification skipped for PIC16F1704 to avoid parser-corrupting I2C_RDWR reads"
                                                );
                                                Ok(VoltageCommandReply::Verified(None))
                                            }
                                            _ => Err("Runtime heartbeat service is in PIC16 mode; non-PIC16 voltage verification is unsupported on this path".to_string()),
                                        };
                                        (result, reply_tx)
                                    }
                                };

                                debug_assert!(reply_tx.is_none());
                                if let Err(detail) = &result {
                                    warn!(error = %detail, "Runtime voltage command failed");
                                }
                                completion.complete(result);
                                std::thread::sleep(Duration::from_millis(10));
                            }

                            if hb_shutdown_flag.is_cancelled() {
                                hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                                break;
                            }

                            let now_secs = hb_thread_start.elapsed().as_secs();
                            for &addr in &hb_pic_addrs {
                                if hb_shutdown_flag.is_cancelled() {
                                    break;
                                }
                                let backoff = pic_backoff.entry(addr).or_default();
                                let action = backoff.decide(now_secs);
                                if action == HbAction::Skip {
                                    // Backed-off / dead PIC not yet due for a reprobe —
                                    // do not touch the bus, do not log. This is the
                                    // fix for the ~33x/s NACK storm.
                                    continue;
                                }
                                let is_reprobe = action == HbAction::Reprobe;
                                let fails = consecutive_failures.entry(addr).or_insert(0);
                                let hb_t0 = std::time::Instant::now();
                                let result = hb_i2c_svc.heartbeat(addr, hb_i2c_fw);
                                let hb_us = hb_t0.elapsed().as_micros();

                                if result.is_ok() {
                                    cycle_heartbeat_succeeded = true;
                                    let recovered = backoff.record_success();
                                    if recovered || *fails > 0 {
                                        info!("PIC 0x{:02X} heartbeat recovered after {} failures", addr, fails);
                                    }
                                    *fails = 0;
                                    if tick <= 20 || tick.is_multiple_of(30) {
                                        info!("DIAG_HB: tick={} PIC=0x{:02X} OK us={}", tick, addr, hb_us);
                                    }
                                } else {
                                    *fails += 1;
                                    let should_log = backoff.record_failure(now_secs, is_reprobe);
                                    if should_log {
                                        warn!(
                                            "DIAG_HB: tick={} PIC=0x{:02X} FAIL us={} consecutive={} state={:?}",
                                            tick, addr, hb_us, fails, backoff.state()
                                        );
                                    }
                                    if backoff.state() == PicHbState::BackingOff
                                        && backoff.consecutive_failures() == PIC_BACKOFF_FAIL_THRESHOLD
                                    {
                                        error!(
                                            "PIC 0x{:02X} heartbeat failed {} times — backing off, reprobe every {}s (dead PIC or stuck I2C bus)",
                                            addr, PIC_BACKOFF_FAIL_THRESHOLD, PIC_BACKOFF_REPROBE_SECS
                                        );
                                    }
                                }
                            }

                            if hb_shutdown_flag.is_cancelled() {
                                hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                                break;
                            }

                            if cycle_heartbeat_succeeded {
                                if let Some(tx) = runtime_ready_tx.take() {
                                    let _ = tx.send(());
                                }
                            }

                            // Stability gate for deferred voltage: only Active PICs
                            // must be answering. A PIC the back-off machine has
                            // declared BackingOff/Dead is excluded (it can never be
                            // made healthy by waiting) so a single dead chain does
                            // NOT permanently block the deferred reduction on the
                            // healthy chains. An Active PIC that is currently failing
                            // still resets the window.
                            let tick_all_ok = hb_pic_addrs.iter().all(|a| {
                                match pic_backoff.get(a) {
                                    // Active PIC must be healthy (0 consecutive fails).
                                    Some(b) if b.state() == PicHbState::Active => b.is_healthy(),
                                    // BackingOff / Dead PIC is excluded from the gate.
                                    Some(_) => true,
                                    // No backoff entry yet — fall back to the legacy counter.
                                    None => *consecutive_failures.get(a).unwrap_or(&0) == 0,
                                }
                            });
                            if tick_all_ok {
                                stable_heartbeat_ticks += 1;
                            } else {
                                stable_heartbeat_ticks = 0;
                            }

                            if stable_heartbeat_ticks >= 5
                                && hb_deferred_target_mv > 0
                                && hb_deferred_target_mv < 9400
                                && !pending_voltage_targets.is_empty()
                                && !thermal_emergency_active(&hb_thermal_emergency_latch)
                                && !voltage_cmd_rx.is_terminal_latched()
                                && !hb_shutdown_flag.is_cancelled()
                            {
                                info!(
                                    stable_heartbeat_ticks,
                                    target_mv = hb_deferred_target_mv,
                                    count = pending_voltage_targets.len(),
                                    "Applying deferred voltage reduction after stable heartbeat window"
                                );
                                let mut still_pending = Vec::new();
                                for (pending_idx, &(chain_id, pic_addr, generation)) in
                                    pending_voltage_targets.iter().enumerate()
                                {
                                    if hb_shutdown_flag.is_cancelled()
                                        || thermal_emergency_active(&hb_thermal_emergency_latch)
                                        || voltage_cmd_rx.is_terminal_latched()
                                    {
                                        still_pending.extend_from_slice(
                                            &pending_voltage_targets[pending_idx..],
                                        );
                                        break;
                                    }
                                    if !voltage_cmd_rx.permits_ordinary_generation(
                                        hb_chip_id,
                                        pic_addr,
                                        generation,
                                    ) {
                                        warn!(
                                            chain_id,
                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                            "Deferred voltage target superseded by a newer endpoint disable generation"
                                        );
                                        continue;
                                    }
                                    let pic_val = PicController::voltage_to_pic(
                                        hb_deferred_target_mv as f64 / 1000.0
                                    );
                                    match hb_i2c_svc.set_voltage(pic_addr, hb_i2c_fw, pic_val) {
                                        Ok(()) => info!(
                                            chain_id,
                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                            target_mv = hb_deferred_target_mv,
                                            pic_val,
                                            "Deferred voltage reduction applied"
                                        ),
                                        Err(e) => {
                                            warn!(
                                                chain_id,
                                                pic_addr = format_args!("0x{:02X}", pic_addr),
                                                error = %e,
                                                "Deferred voltage failed — retry next tick"
                                            );
                                            still_pending.push((chain_id, pic_addr, generation));
                                        }
                                    }
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                pending_voltage_targets = still_pending;
                            }

                            if hb_shutdown_flag.is_cancelled() {
                                hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                                break;
                            }

                            hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                            std::thread::sleep(Duration::from_millis(1000));
                            continue;
                        }

                        hb_i2c_active.store(true, std::sync::atomic::Ordering::Release);
                        std::thread::sleep(Duration::from_millis(5));
                        if hb_shutdown_flag.is_cancelled() {
                            hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                            break;
                        }
                        let _ = hb_i2c_svc.set_timeout(10); // 100ms
                        if hb_shutdown_flag.is_cancelled() {
                            hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                            break;
                        }

                        if matches!(hb_pic_type, PicType::DsPic33EP) && tick.is_multiple_of(5) {
                            let now_s = hb_board_temp_time_base.elapsed().as_secs() as u32;
                            for &addr in &hb_pic_addrs {
                                if hb_shutdown_flag.is_cancelled() {
                                    break;
                                }
                                if let Some(chain_idx) = hb_pic_chain_map.get(&addr).copied() {
                                    let mut dspic = DspicService::new(hb_i2c_svc.clone(), addr);
                                    let hottest = dspic
                                        .read_all_temperatures()
                                        .into_iter()
                                        .filter(|temp| *temp > -40.0 && *temp < 125.0)
                                        .fold(None, |acc: Option<f64>, temp| {
                                            Some(acc.map_or(temp, |current| current.max(temp)))
                                        });
                                    if let Some(temp_c) = hottest {
                                        if chain_idx < hb_board_temps.len() {
                                            hb_board_temps[chain_idx]
                                                .store((temp_c as f32).to_bits(), Ordering::Release);
                                        }
                                        if chain_idx < hb_board_temp_seen_at.len() {
                                            hb_board_temp_seen_at[chain_idx]
                                                .store(now_s.max(1), Ordering::Release);
                                        }
                                    }
                                }
                            }
                        }

                        if hb_shutdown_flag.is_cancelled() {
                            hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                            break;
                        }

                        while let Ok(delivery) = voltage_cmd_rx.try_recv() {
                            let (cmd, completion) = delivery.into_parts();
                            if hb_shutdown_flag.is_cancelled() {
                                completion.complete(Err(
                                    "runtime heartbeat is stopping; command was not executed"
                                        .to_string(),
                                ));
                                break;
                            }
                            let (result, reply_tx): (
                                std::result::Result<VoltageCommandReply, String>,
                                Option<tokio::sync::oneshot::Sender<std::result::Result<VoltageCommandReply, String>>>,
                            ) = match cmd {
                                VoltageCommand::SetVoltage { chain_id, chip_id, pic_addr, target_mv, reply_tx } => {
                                    if voltage_cmd_rx.is_terminal_latched() {
                                        warn!(
                                            target_mv,
                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                            "Runtime voltage BLOCKED: terminal safe-off is latched"
                                        );
                                        (Err("terminal safe-off latched; refusing SetVoltage".to_string()), reply_tx)
                                    } else if thermal_emergency_active(&hb_thermal_emergency_latch) {
                                        warn!(
                                            target_mv,
                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                            "Runtime voltage BLOCKED: thermal emergency active"
                                        );
                                        (Err("thermal emergency active; refusing SetVoltage".to_string()), reply_tx)
                                    } else if stable_heartbeat_ticks < 5 {
                                        warn!(
                                            stable_heartbeat_ticks,
                                            target_mv,
                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                            "Runtime voltage BLOCKED: PIC heartbeat not stable (need 5 ticks, have {})",
                                            stable_heartbeat_ticks,
                                        );
                                        (Err(format!("PIC heartbeat not stable: {} < 5 ticks", stable_heartbeat_ticks)), reply_tx)
                                    } else {
                                    let pic_type = MinerProfile::for_chip(chip_id)
                                        .map(|profile| profile.pic_type)
                                        .unwrap_or(hb_pic_type);
                                    let result = match pic_type {
                                        PicType::Pic16F1704 => {
                                            let pic_val = PicController::voltage_to_pic(target_mv as f64 / 1000.0);
                                            hb_i2c_svc
                                                .set_voltage(pic_addr, hb_i2c_fw, pic_val)
                                                .map(|_| {
                                                    info!(
                                                        chain_id = ?chain_id,
                                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                                        target_mv,
                                                        pic_val,
                                                        "Runtime voltage apply: PIC16 target committed"
                                                    );
                                                    VoltageCommandReply::Applied(target_mv)
                                                })
                                                .map_err(|e: dcentrald_hal::HalError| e.to_string())
                                        }
                                        PicType::DsPic33EP => {
                                            let mut dspic = DspicService::new(hb_i2c_svc.clone(), pic_addr);
                                            dspic.cold_boot_init(target_mv)
                                                .map(|_| {
                                                    info!(
                                                        chain_id = ?chain_id,
                                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                                        target_mv,
                                                        "Runtime voltage apply: dsPIC target committed"
                                                    );
                                                    VoltageCommandReply::Applied(target_mv)
                                                })
                                                .map_err(|e| e.to_string())
                                        }
                                        PicType::NoPic => Err("NoPic architecture has no runtime voltage controller".to_string()),
                                    };
                                    (result, reply_tx)
                                    }
                                }
                                VoltageCommand::DisableVoltage { chain_id, chip_id, pic_addr, reply_tx } => {
                                    let pic_type = MinerProfile::for_chip(chip_id)
                                        .map(|profile| profile.pic_type)
                                        .unwrap_or(hb_pic_type);
                                    let result = match pic_type {
                                        PicType::Pic16F1704 => {
                                            hb_i2c_svc
                                                .disable_voltage(pic_addr, hb_i2c_fw)
                                                .map(|_| {
                                                    warn!(
                                                        chain_id = ?chain_id,
                                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                                        "Runtime voltage disable: PIC16 output disabled"
                                                    );
                                                    VoltageCommandReply::Disabled
                                                })
                                                .map_err(|e: dcentrald_hal::HalError| e.to_string())
                                        }
                                        PicType::DsPic33EP => {
                                            let mut dspic = DspicService::new(hb_i2c_svc.clone(), pic_addr);
                                            dspic.disable_voltage()
                                                .map(|_| {
                                                    warn!(
                                                        chain_id = ?chain_id,
                                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                                        "Runtime voltage disable: dsPIC output disabled"
                                                    );
                                                    VoltageCommandReply::Disabled
                                                })
                                                .map_err(|e| e.to_string())
                                        }
                                        PicType::NoPic => Err("NoPic architecture has no runtime voltage disable path".to_string()),
                                    };
                                    (result, reply_tx)
                                }
                                VoltageCommand::VerifyVoltage { chain_id, chip_id, pic_addr, target_mv, reply_tx } => {
                                    let pic_type = MinerProfile::for_chip(chip_id)
                                        .map(|profile| profile.pic_type)
                                        .unwrap_or(hb_pic_type);
                                    let result = match pic_type {
                                        PicType::Pic16F1704 => {
                                            info!(
                                                chain_id = ?chain_id,
                                                pic_addr = format_args!("0x{:02X}", pic_addr),
                                                target_mv,
                                                firmware = %hb_pic_firmware,
                                                "Runtime voltage verification skipped for PIC16F1704 to avoid parser-corrupting I2C_RDWR reads"
                                            );
                                            Ok(VoltageCommandReply::Verified(None))
                                        }
                                        PicType::DsPic33EP => {
                                            let mut dspic = DspicService::new(hb_i2c_svc.clone(), pic_addr);
                                            dspic.read_voltage()
                                                .map(|actual_mv| {
                                                    info!(
                                                        chain_id = ?chain_id,
                                                        pic_addr = format_args!("0x{:02X}", pic_addr),
                                                        target_mv,
                                                        actual_mv,
                                                        delta_mv = actual_mv as i32 - target_mv as i32,
                                                        "Runtime voltage verification: dsPIC readback complete"
                                                    );
                                                    VoltageCommandReply::Verified(Some(actual_mv))
                                                })
                                                .map_err(|e| e.to_string())
                                        }
                                        PicType::NoPic => Ok(VoltageCommandReply::Verified(None)),
                                    };
                                    (result, reply_tx)
                                }
                            };

                            debug_assert!(reply_tx.is_none());
                            if let Err(detail) = &result {
                                warn!(error = %detail, "Runtime voltage command failed");
                            }
                            completion.complete(result);
                        }

                        if hb_shutdown_flag.is_cancelled() {
                            hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                            break;
                        }

                        let now_secs = hb_thread_start.elapsed().as_secs();
                        for &addr in &hb_pic_addrs {
                            if hb_shutdown_flag.is_cancelled() {
                                break;
                            }
                            let backoff = pic_backoff.entry(addr).or_default();
                            let action = backoff.decide(now_secs);
                            if action == HbAction::Skip {
                                // Backed-off / dead dsPIC not yet due for a reprobe —
                                // do not touch the bus, do not log (NACK-storm fix).
                                continue;
                            }
                            let is_reprobe = action == HbAction::Reprobe;
                            let fails = consecutive_failures.entry(addr).or_insert(0);
                            let hb_t0 = std::time::Instant::now();
                            let mut dspic = DspicService::new(hb_i2c_svc.clone(), addr);
                            let result = dspic.send_heartbeat();
                            let hb_us = hb_t0.elapsed().as_micros();

                            if result.is_ok() {
                                cycle_heartbeat_succeeded = true;
                                let recovered = backoff.record_success();
                                if recovered || *fails > 0 {
                                    info!("PIC 0x{:02X} heartbeat recovered after {} failures", addr, fails);
                                }
                                *fails = 0;
                                if tick <= 20 || tick.is_multiple_of(30) {
                                    info!("DIAG_HB: tick={} PIC=0x{:02X} OK us={}", tick, addr, hb_us);
                                }
                            } else {
                                *fails += 1;
                                let should_log = backoff.record_failure(now_secs, is_reprobe);
                                if should_log {
                                    warn!(
                                        "DIAG_HB: tick={} PIC=0x{:02X} FAIL us={} consecutive={} state={:?}",
                                        tick, addr, hb_us, fails, backoff.state()
                                    );
                                }
                                if backoff.state() == PicHbState::BackingOff
                                    && backoff.consecutive_failures() == PIC_BACKOFF_FAIL_THRESHOLD
                                {
                                    error!(
                                        "PIC 0x{:02X} heartbeat failed {} times — backing off, reprobe every {}s (dead PIC or stuck I2C bus)",
                                        addr, PIC_BACKOFF_FAIL_THRESHOLD, PIC_BACKOFF_REPROBE_SECS
                                    );
                                }
                            }
                        }

                        if hb_shutdown_flag.is_cancelled() {
                            hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);
                            break;
                        }

                        if cycle_heartbeat_succeeded {
                            if let Some(tx) = runtime_ready_tx.take() {
                                let _ = tx.send(());
                            }
                        }

                        hb_i2c_active.store(false, std::sync::atomic::Ordering::Release);

                        // 1000ms interval matching BraiinsOS (VOLTAGE_CTRL_HEART_BEAT_PERIOD).
                        std::thread::sleep(Duration::from_millis(1000));
                    }
                })
                .map_err(|e| anyhow::anyhow!("failed to spawn PIC heartbeat thread: {}", e))?;
            self.runtime_heartbeat_handle = Some(runtime_heartbeat_handle);
            runtime_heartbeat_ready_rx = Some(runtime_ready_rx);
        }

        // Runtime heartbeat ownership is established before the init guard is
        // released. Keeping the guard on `self` across every fallible handoff
        // step prevents an early return or future cancellation from detaching a
        // live initialization heartbeat without asserting its stop flag.
        if let Some(ready_rx) = runtime_heartbeat_ready_rx {
            match tokio::time::timeout(Duration::from_secs(3), ready_rx).await {
                Ok(Ok(())) => info!("Runtime heartbeat completed its first successful cycle"),
                Ok(Err(_)) => {
                    anyhow::bail!("runtime heartbeat exited before completing one successful cycle")
                }
                Err(_) => {
                    anyhow::bail!("runtime heartbeat did not complete a successful cycle within 3s")
                }
            }
        }
        if !self.stop_init_heartbeat_bounded().await {
            anyhow::bail!(
                "initialization heartbeat ownership could not be reclaimed after runtime handoff"
            );
        }

        // Deferred voltage reductions are now applied inside the heartbeat thread
        // after 5 consecutive stable heartbeat ticks. See heartbeat loop above.

        // Init heartbeat has now been stopped after the runtime handoff.
        // Mining heartbeat thread is now the sole I2C user.

        // ---- Legacy smart-PSU initialization (authoritative AM2-S17 only) ----
        // Raw kernel bus-1 access is admitted only when board_target is am2-s17*
        // AND the configured model is S17-family. S9 uses estimated power; S19,
        // CV, Amlogic, BeagleBone, missing/unknown targets, and PSU overrides all
        // fail closed to bypass mode. This boundary is load-bearing: the legacy
        // bus-1 path corrupts kernel I2C bus 0 on S19 Pro, causing dsPIC heartbeat
        // loss and watchdog rail cutoff.
        let psu_override_active = self
            .config
            .power
            .psu_override
            .as_ref()
            .map(|o| o.enabled)
            .unwrap_or(false);
        let legacy_psu_board_target = self.platform_identity.board_target();
        let legacy_psu_path_allowed = legacy_kernel_smart_psu_path_allowed(
            self.config.mining.model.as_deref(),
            legacy_psu_board_target,
            psu_override_active,
        );
        let mut detected_smart_psu_version: Option<String> = None;
        let psu_available = if legacy_psu_path_allowed
            && !matches!(self.pic_type()?, PicType::NoPic)
        {
            match psu_lock.lock() {
                Ok(_guard) => match dcentrald_hal::psu::PsuController::open_kernel_only() {
                    Ok(mut psu) => match psu.get_version() {
                        Ok(version) => {
                            detected_smart_psu_version = Some(version);
                            true
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Smart PSU probe failed on kernel I2C bus 1");
                            false
                        }
                    },
                    Err(e) => {
                        tracing::debug!(error = %e, "Smart PSU kernel I2C bus unavailable");
                        false
                    }
                },
                Err(_) => false,
            }
        } else {
            if !psu_override_active {
                tracing::debug!(
                    board_target = %legacy_psu_board_target,
                    config_model = ?self.config.mining.model,
                    "Legacy kernel-I2C smart-PSU probe/feed path is not proven for this platform; staying in bypass mode"
                );
            }
            false
        };

        if let Some(ref version) = detected_smart_psu_version {
            if let Ok(mut hw) = hardware_info.lock() {
                hw.psu_model = Some(dcentrald_hal::psu::PsuController::model_name_from_version(
                    version,
                ));
                hw.psu_fw_version = Some(version.clone());
                hw.psu_voltage_range =
                    dcentrald_hal::psu::PsuController::format_voltage_range(version);
            }
        }

        // ---- PSU watchdog feed thread ----
        // This legacy bus-1 owner is retained only behind a successful smart-PSU
        // probe. The handle is owned by `psu_watchdog_threads`; shutdown must
        // cancel and join it before any shared hardware teardown. Cancellation
        // deliberately STOPS feeding without disarming: an armed PSU watchdog is
        // a safety fallback, while disarming here could preserve energized rails
        // after a later software safe-off failure.
        if psu_available {
            let psu_shutdown = self.psu_watchdog_threads.cancellation_token();
            let psu_lock_for_watchdog = psu_lock.clone();
            match std::thread::Builder::new()
                .name("psu-watchdog".to_string())
                .spawn(move || {
                    let mut psu = match dcentrald_hal::psu::PsuController::open_kernel_only() {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(error = %e, "PSU watchdog thread: failed to open I2C");
                            return;
                        }
                    };
                    tracing::info!("PSU watchdog thread started — feeding every {}s",
                        dcentrald_hal::psu::WATCHDOG_INTERVAL_S);
                    loop {
                        if psu_shutdown.is_cancelled() {
                            tracing::info!("PSU watchdog feeder stopping; watchdog remains armed as the shutdown fallback");
                            break;
                        }
                        match psu_lock_for_watchdog.try_lock() {
                            Ok(_guard) => {
                                if let Err(e) = psu.feed_watchdog() {
                                    tracing::warn!(error = %e, "PSU watchdog feed failed");
                                }
                            }
                            Err(std::sync::TryLockError::WouldBlock) => tracing::warn!(
                                "PSU watchdog feed skipped because another owner holds the bus-1 transport lock; the hardware watchdog remains fail-safe"
                            ),
                            Err(std::sync::TryLockError::Poisoned(_)) => {
                                tracing::error!("PSU watchdog transport lock is poisoned; stopping feeds so the hardware watchdog can cut power");
                                break;
                            }
                        }
                        if sleep_until_cancelled(
                            &psu_shutdown,
                            std::time::Duration::from_secs(
                                dcentrald_hal::psu::WATCHDOG_INTERVAL_S,
                            ),
                        ) {
                            tracing::info!("PSU watchdog feeder stopping during interval wait; watchdog remains armed");
                            break;
                        }
                    }
                })
            {
                Ok(handle) => self.psu_watchdog_threads.push("psu-watchdog", handle),
                Err(error) => tracing::error!(
                    error = %error,
                    "Failed to spawn PSU watchdog feeder; the armed PSU watchdog may cut output unless another owner feeds it"
                ),
            }
        }

        // ---- Start thermal control loop ----
        // The thermal controller is the safety brain of the miner. Every 5 seconds it:
        //   1. Reads chip temperatures (currently SoC die temp, future: per-board TMP75)
        //   2. Runs a PID controller to calculate optimal fan speed
        //   3. Detects fan failures (fan spinning at 0 RPM = danger)
        //   4. Throttles frequency or shuts down if temps get dangerous
        // This keeps your chips alive and your house from burning down.
        let thermal_shutdown = self.mining_tasks.cancellation_token();
        // pid_interval_s captured as f32 for Duration::from_secs_f32 below (interval
        // is constructed inside the spawned task, after config is moved).
        let thermal_pid_interval_s = thermal_pid_interval_secs(self.config.thermal.pid_interval_s);
        let (thermal_fan_min_pwm, thermal_fan_max_pwm) = normalize_fan_pwm_bounds(
            self.config.thermal.fan_min_pwm,
            self.config.thermal.fan_max_pwm,
        );
        let thermal_profile = ThermalProfile {
            target_temp_c: self.config.thermal.target_temp_c,
            hot_temp_c: self.config.thermal.hot_temp_c,
            dangerous_temp_c: self.config.thermal.dangerous_temp_c,
            fan_min_pwm: thermal_fan_min_pwm,
            fan_max_pwm: thermal_fan_max_pwm,
            ramp_delay_s: 300,
            hysteresis_c: self.config.thermal.hysteresis_c,
        };

        info!(
            target_temp = self.config.thermal.target_temp_c,
            hot_temp = self.config.thermal.hot_temp_c,
            dangerous_temp = self.config.thermal.dangerous_temp_c,
            fan_min = thermal_fan_min_pwm,
            fan_max = thermal_fan_max_pwm,
            "Thermal controller armed — PID loop targets {}C, throttles at {}C, emergency shutdown at {}C",
            self.config.thermal.target_temp_c,
            self.config.thermal.hot_temp_c,
            self.config.thermal.dangerous_temp_c,
        );

        // Capture fan limits before thermal_profile is moved into controller
        let cfg_fan_min_pwm = thermal_profile.fan_min_pwm;
        let cfg_fan_max_pwm = thermal_profile.fan_max_pwm;

        // Share fan controller between thermal loop and shutdown
        let thermal_fan = self.fan.clone();
        let thermal_state_tx = state_tx.clone();
        let thermal_pic_firmware = self.pic_firmware;
        let thermal_xadc_temp = shared_xadc_temp.clone();
        let thermal_curtailment = curtailment.clone();
        let thermal_curtailment_sleeping = curtailment_sleeping.clone();
        let thermal_power_tx = power_tx.clone();
        // thermal_voltage_tx was cloned earlier (before dispatcher creation)

        // Capture chain IDs and nominal frequency for thermal throttle commands.
        // chain_infos was already collected above with per-chain chip metadata.
        let thermal_chain_ids: Vec<u8> = chain_infos.iter().map(|info| info.chain_id).collect();
        let thermal_nominal_freq = self.config.mining.frequency_mhz;
        let thermal_board_temps = board_temps_thermal;
        let thermal_board_temp_seen_at = board_temp_seen_at_thermal;
        let thermal_led_tx = self.led_tx.clone();
        let thermal_alert_tx = alert_tx.clone();
        let thermal_night_mode = self.config.thermal.night_mode.clone();
        let thermal_chip_id = self.chip_id;
        let thermal_pic_type = self.pic_type()?;
        let thermal_emergency_latch = thermal_emergency_latch.clone();
        let thermal_skip_board_temp = self.config.mining.skip_board_temp;
        let thermal_has_xadc = !self
            .platform_identity
            .observed_control_board
            .starts_with("AML");
        let thermal_platform_marker = self.platform_identity.platform_marker().to_string();
        let thermal_include_die_on_am2 = std::env::var(ENV_THERMAL_INCLUDE_DIE_ON_AM2)
            .map(|v| dcentrald_autotuner::config::env_flag_is_truthy(&v))
            .unwrap_or(false);
        let thermal_restart_voltage_mv = match self.pic_type()? {
            PicType::Pic16F1704 => 9400,
            _ => self
                .miner_profile
                .map(|profile| profile.default_voltage_mv)
                .unwrap_or(self.config.mining.voltage_mv),
        };

        let thermal_pid_state_tx = pid_state_tx;
        let mut thermal_pid_command_rx = pid_command_rx;
        // Wave-G G1 (E3b): LuxOS-shape ThermalSupervisor. Default-off — when
        // `[thermal.supervisor].enabled` is false the supervisor is never
        // constructed and the controller-only path below is byte-identical to
        // pre-Wave-G. When true (operator opt-in, Wave-H live-soak gated) the
        // loop drives the 6-layer FSM alongside the controller and reconciles
        // strongest-safety-wins via `reconcile_with_supervisor`. The snapshot
        // channel feeds `/api/thermal/supervisor` (honest telemetry).
        let thermal_supervisor_cfg = self.config.thermal.supervisor.clone();
        // R-11: snapshot the supervisor config values the hardware-safety audit
        // rows need (Copy scalars, moved into the thermal task). Captured here
        // because `thermal_supervisor_cfg` is moved into the FSM constructor
        // inside the spawned task; the supervisor doesn't expose these fields.
        let thermal_audit_min_fans = thermal_supervisor_cfg.min_fans;
        let thermal_audit_board_panic_c = thermal_supervisor_cfg.board_panic_c;
        let thermal_audit_chip_panic_c = thermal_supervisor_cfg.chip_panic_c;
        // THERMAL-8: per-platform default-enable for the fail-closed supervisor.
        // The compiled default stays OFF — `supervisor_default_enabled` returns
        // false for every platform unless the operator sets the per-platform
        // live-validation gate `DCENT_THERMAL_SUPERVISOR_DEFAULT_ON=1` AND that
        // platform's arm has been signed off in `supervisor.rs`. An explicit
        // `[thermal.supervisor].enabled = true` in config always wins. This makes
        // the capability reachable + host-testable without flipping any live
        // platform to default-on (LIVE-HARDWARE-DEFAULT principle). FLAGGED FOR
        // OPERATOR LIVE VALIDATION.
        let thermal_supervisor_default_on = {
            let validated = std::env::var("DCENT_THERMAL_SUPERVISOR_DEFAULT_ON")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let marker = self
                .config
                .mining
                .model
                .clone()
                .unwrap_or_else(|| self.platform_identity.observed_control_board.clone());
            let platform =
                dcentrald_thermal::supervisor::SupervisorPlatform::from_board_target(&marker);
            let on = dcentrald_thermal::supervisor::supervisor_default_enabled(platform, validated);
            if on && !thermal_supervisor_cfg.enabled {
                info!(?platform, "THERMAL-8: thermal supervisor default-enabled for this validated platform (DCENT_THERMAL_SUPERVISOR_DEFAULT_ON=1)");
            }
            on
        };
        let (thermal_supervisor_snapshot_tx, thermal_supervisor_snapshot_rx) =
            tokio::sync::watch::channel::<Option<dcentrald_thermal::supervisor::SupervisorSnapshot>>(
                None,
            );
        dcentrald_api::install_thermal_supervisor_rx(
            thermal_supervisor_snapshot_rx,
            thermal_supervisor_cfg.enabled || thermal_supervisor_default_on,
        );

        // W8 parity: immersion / hydro cooling mode. Captured before `config` is
        // moved into the spawned thermal task (mirrors `thermal_supervisor_cfg`).
        // DEFAULT-OFF — a missing `[thermal.immersion]` deserializes to
        // `ImmersionConfig::default()` (enabled = false), so the controller's
        // `immersion_active()` stays false and the fan-write path below is
        // byte-identical to the pre-immersion daemon. `platform_looks_air_cooled`
        // is TRUE for every current control board (am1-s9 / am2 / am3-bb /
        // am3-aml are all air-cooled chassis), so immersion fail-closes (REFUSES)
        // unless the operator sets BOTH `enabled` AND
        // `acknowledge_air_cooled_override`. The over-temp HASH-CUT safety net
        // (`EmergencyShutdown` / `FanFailure`) is never weakened by immersion.
        let thermal_immersion_cfg = self.config.thermal.immersion.clone();
        let thermal_platform_looks_air_cooled = true;

        // Wave-I Lane A: live gRPC read-RPC backing. Default-off — only when
        // `[api.grpc].enabled` do we install the snapshot channel + spawn the
        // publisher that converts each `MinerState` update (+ restart-static
        // autotune config) into the lean plain snapshot the gRPC read RPCs
        // serve. The gRPC server itself is spawned in `main.rs` (also
        // default-off); this just backs its 4 READ RPCs with honest live
        // state. No write path; pool passwords are never included.
        if self.config.api.grpc.enabled {
            let grpc_platform_marker = self
                .config
                .mining
                .model
                .clone()
                .unwrap_or_else(detect_control_board);
            let grpc_chip_family = self
                .config
                .mining
                .serial_chip_type
                .clone()
                .unwrap_or_default();
            let grpc_home_cap_pwm = self.config.thermal.fan_max_pwm as u32;
            let grpc_tuner = grpc_tuner_snapshot_from_config(&self.config.autotune.mode);
            let mut grpc_state_rx = state_tx.subscribe();
            let (grpc_snapshot_tx, grpc_snapshot_rx) = tokio::sync::watch::channel::<
                Option<dcentrald_api_grpc::GrpcRuntimeSnapshot>,
            >(None);
            dcentrald_api_grpc::install_runtime_snapshot_rx(grpc_snapshot_rx);

            // SW-02: install the WRITE-control delegate next to the read-RPC
            // snapshot. Default-OFF: only when `DCENT_GRPC_WRITE_CONTROL=1` is
            // ALSO set. With the gate unset (compiled default), NO delegate is
            // installed → every gRPC write RPC keeps returning UNIMPLEMENTED,
            // byte-identical to the prior read-only contract (no live default
            // changes). When on, the delegate bridges all five write RPCs to
            // the SAME gated runtime channels / REST helpers the dashboard +
            // cgminer-LuxOS surface use: set_tuner_mode + locate to the gated
            // runtime channels (all ≤14500 mV / fan-cap / PVT clamps enforced
            // downstream); set_pools / set_fan_mode / reboot to the narrow
            // `dcentrald_api::rest::grpc_bridge_*` hooks (same pool validation /
            // PWM-30 home cap / restart action as the REST handlers).
            // `grpc_write_app_state` is `Some` exactly when `[api.grpc].enabled`
            // (this branch). If the gate is on AND we have the state, install;
            // a missing-state invariant slip is logged and skipped (never kills
            // mining), leaving the write RPCs at their UNIMPLEMENTED default.
            if grpc_write_control_enabled() {
                match grpc_write_app_state.as_ref() {
                    Some(delegate_state) => {
                        let delegate = Box::new(DaemonGrpcWriteDelegate {
                            app_state: delegate_state.clone(),
                        });
                        if dcentrald_api_grpc::install_write_delegate(delegate) {
                            info!(
                                "gRPC WRITE control plane delegate installed \
                                 (DCENT_GRPC_WRITE_CONTROL=1) — set_tuner_mode + \
                                 locate + set_pools + set_fan_mode + reboot bridge \
                                 to the gated runtime channels / REST helpers"
                            );
                        } else {
                            warn!(
                                "gRPC write delegate was already installed — \
                                 keeping the existing one"
                            );
                        }
                    }
                    None => warn!(
                        "DCENT_GRPC_WRITE_CONTROL=1 but no AppState captured — \
                         gRPC writes stay UNIMPLEMENTED"
                    ),
                }
            } else {
                info!(
                    "gRPC WRITE control plane stays UNIMPLEMENTED \
                     (DCENT_GRPC_WRITE_CONTROL not set — default-OFF)"
                );
            }

            tokio::spawn(async move {
                loop {
                    let snapshot = {
                        let state = grpc_state_rx.borrow_and_update();
                        build_grpc_runtime_snapshot(
                            &state,
                            &grpc_platform_marker,
                            &grpc_chip_family,
                            grpc_home_cap_pwm,
                            grpc_tuner.clone(),
                        )
                    };
                    if grpc_snapshot_tx.send(Some(snapshot)).is_err() {
                        break; // gRPC-side receiver dropped
                    }
                    if grpc_state_rx.changed().await.is_err() {
                        break; // state publisher dropped → daemon shutting down
                    }
                }
            });
            info!("gRPC read-RPC snapshot publisher started ([api.grpc].enabled)");
        }

        let thermal_liveness_loop = thermal_liveness.clone();
        if !self.mining_tasks.spawn("thermal-controller", async move {
            let mut controller = ThermalController::new(thermal_profile);
            // W8 parity: arm immersion / hydro mode (default-OFF → no-op).
            // `enable_immersion` is fail-closed: on an air-cooled-looking
            // platform it REFUSES (keeps fan management) unless the operator
            // also set `acknowledge_air_cooled_override`. Disabled config →
            // `immersion_active()` stays false → fan writes below are
            // byte-identical to the pre-immersion path. The controller emits the
            // matching `tracing` warning for the decision.
            controller.enable_immersion(&thermal_immersion_cfg, thermal_platform_looks_air_cooled);
            controller.set_tach_available(
                thermal_fan
                    .as_ref()
                    .is_some_and(|fan| fan.tach_available()),
            );
            // Construct the supervisor when explicitly enabled in config OR when
            // THERMAL-8's per-platform default-enable resolved on (still default-off
            // unless the operator set the per-platform validation gate). When armed
            // via the platform default we force `enabled = true` on the cfg copy so
            // the FSM's own `tick()` dormancy guard agrees with construction.
            let mut thermal_supervisor =
                if thermal_supervisor_cfg.enabled || thermal_supervisor_default_on {
                    let mut cfg = thermal_supervisor_cfg;
                    cfg.enabled = true;
                    Some(dcentrald_thermal::supervisor::ThermalSupervisor::new(cfg))
                } else {
                    None
                };
            let mut interval =
                tokio::time::interval(Duration::from_secs_f32(thermal_pid_interval_s));

            // THERMAL-7: consecutive-XADC-failure counter. On a Zynq unit the XADC
            // die temp is the control-board thermal source; if it keeps failing AND
            // no valid hashboard board temp is available, the loop has NO thermal
            // proof at all. Feeding a benign hardcoded 45.0C forever would let the
            // controller believe the unit is cool indefinitely — a fail-OPEN hole.
            // We count consecutive failures and, once the threshold is crossed with
            // no board-temp fallback, force a fail-CLOSED EmergencyShutdown path
            // (strictly safer: de-energizes boards + caps fans at the home PWM cap).
            // A single transient XADC glitch (or any tick where board temps ARE
            // present) resets the counter — this only escalates on a sustained,
            // total loss of thermal visibility. ~5 ticks of total blindness.
            const XADC_BLIND_FAIL_LIMIT: u32 = 5;
            let mut consecutive_xadc_failures: u32 = 0;

            // THERMAL-8 (non-XADC / Amlogic twin of THERMAL-7): on a non-XADC
            // platform `die_temp` is a hardcoded 45.0C FALLBACK (not a real
            // readback), so THERMAL-7 — which is `thermal_has_xadc`-gated — can
            // NEVER fire. The SOLE real thermal source is the per-chain board/chip
            // temp pipeline; if it goes fully stale the controller would otherwise
            // believe the unit is a steady 45C forever and mine with ZERO thermal
            // proof (the non-XADC fail-OPEN twin of THERMAL-7). Count consecutive
            // ticks of TOTAL board-temp blindness; a single tick with any real
            // board temp (or any XADC platform) resets it. At the default 5 s PID
            // cadence with a 30 s board-temp stale window, this is ~50 s of total
            // blindness before escalation — sustained, not a single empty tick.
            const BOARD_TEMP_BLIND_FAIL_LIMIT: u32 = 5;
            let mut consecutive_board_temp_failures: u32 = 0;
            let mut board_temp_stuck_states =
                vec![StuckBoardTempState::default(); thermal_board_temps.len()];

            // ATM (Advanced Thermal Management) profile-step wiring. The
            // thermal supervisor emits `RequestProfileStepDown` (hot) and
            // `RequestProfileStepUp` (cool, post-grace) advisories; before this
            // they reached `reconcile_with_supervisor` only to be dropped as
            // "advisory / telemetry" (the compiled-but-unwired anti-pattern).
            // We now CONSUME them by driving an ATM frequency-step ceiling
            // through the `FreqCommand::SetFrequencyLimit` channel the thermal
            // loop already owns, on the dedicated `FrequencyLimitSource::AtmStep`
            // slot. Step-DOWN lowers the ceiling (the SAFE cut-hash-before-noise
            // direction); step-UP RELAXES it, BOUNDED by `thermal_nominal_freq`
            // (the configured ceiling — never above the operator/SKU max).
            //
            // SAFETY / load-bearing:
            // - Active ONLY inside the `thermal_supervisor.is_some()` block,
            //   which is itself gated on the operator-opt-in (default-off)
            //   supervisor. With the supervisor disabled this state is never
            //   touched and the daemon path is byte-identical.
            // - A ceiling can only LOWER effective frequency; voltage falls with
            //   frequency through the autotuner's PVT envelope, so an ATM step
            //   can never raise voltage past the 14500 mV cap.
            // - Thermal safety ALWAYS wins: step-UP is suppressed whenever the
            //   reconciled `action` this tick is an emergency / throttle / fan-
            //   max response, or the same tick also produced a step-DOWN
            //   advisory (a hot event). Step-DOWN is never suppressed.
            // - Debounce/rate-limit: at most one ATM step command per
            //   `ATM_STEP_MIN_INTERVAL` so a flapping temperature cannot thrash
            //   the profile (defense-in-depth on top of the supervisor's own
            //   `atm_post_ramp_grace_secs` emission grace).
            //
            // `atm_step_ceiling_mhz == None` means "no ATM constraint" (cleared
            // — at or above nominal). The step granularity is ~8% of nominal,
            // floored at `ATM_STEP_FLOOR_MHZ` so a runaway step-down can never
            // drive the ceiling to an unminable frequency.
            const ATM_STEP_FLOOR_MHZ: u16 = 200;
            let atm_step_size_mhz: u16 = (thermal_nominal_freq / 12).max(15);
            let atm_step_min_interval = Duration::from_secs(30);
            let mut atm_step_ceiling_mhz: Option<u16> = None;
            let mut atm_last_step_at: Option<Instant> = None;

            // R-11: hardware-safety audit de-dup state. The supervisor RE-EMITS
            // the same protective action every tick while a condition persists
            // (FanPanic fires each tick until fans recover; a hot board
            // re-emits RequestBoardPowerOff until it cools), so we emit ONE
            // audit row per TRANSITION into a safety event — mirroring the
            // ModeChange no-op-skip in rest.rs. `audit_last_emergency_reason`
            // holds the last whole-unit emergency cause we logged (a change of
            // cause re-logs; a persisting cause does not);
            // `audit_boards_off_latched` holds the chains we have already logged
            // as powered-off (a board that recovers is removed, so a fresh trip
            // re-logs). Only touched inside the `thermal_supervisor.is_some()`
            // block — with the supervisor disabled these stay empty and the
            // path is byte-identical.
            let mut audit_last_emergency_reason: Option<
                dcentrald_thermal::supervisor::ThermalReason,
            > = None;
            let mut audit_boards_off_latched: std::collections::HashSet<u8> =
                std::collections::HashSet::new();

            loop {
                tokio::select! {
                    _ = thermal_shutdown.cancelled() => {
                        info!("Thermal controller stopping");
                        break;
                    }
                    _ = interval.tick() => {
                        // Thermal-liveness: signal the WDT kicker that the thermal
                        // control loop is alive this tick. If this stops advancing
                        // the kicker withholds the WDT kick and the SoC reboots — a
                        // hung thermal loop means boards energized with NO thermal
                        // supervision, which the reboot recovers.
                        thermal_liveness_loop
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        // Read temperature from XADC (Zynq SoC die temp)
                        // This is the control board temp, not hash board chip temp.
                        // Future: add TMP75 I2C reads for per-board temps.
                        // `xadc_failed` records whether THIS tick's XADC read failed;
                        // the consecutive-failure escalation below only fires when the
                        // read failed AND no valid board temp covers for it.
                        let mut xadc_failed = false;
                        let die_temp = if thermal_has_xadc {
                            match Xadc::read_temp() {
                                Ok(t) => {
                                    // Healthy read: clear the blind-failure counter.
                                    consecutive_xadc_failures = 0;
                                    t
                                }
                                Err(e) => {
                                    xadc_failed = true;
                                    consecutive_xadc_failures =
                                        consecutive_xadc_failures.saturating_add(1);
                                    error!(error = %e, consecutive = consecutive_xadc_failures, "XADC temp read FAILED — keeping fan command within the home safety cap while board-temp supervision continues");
                                    if let Some(ref fan) = thermal_fan {
                                        fan.set_speed(
                                            dcentrald_common::FanCommand::emergency_cap(
                                                cfg_fan_max_pwm,
                                            )
                                            .effective_pwm(),
                                        );
                                    }
                                    45.0
                                }
                            }
                        } else {
                            // Non-XADC platform (Amlogic): XADC isn't the thermal
                            // source here, so it can't go "blind". Keep the counter
                            // clear so the escalation below never spuriously fires.
                            consecutive_xadc_failures = 0;
                            45.0
                        };

                        // Gap 2: Share die temp with work dispatcher for autotuner snapshots.
                        // XADC die temp is a proxy for board temp (same enclosure).
                        thermal_xadc_temp.store(die_temp.to_bits(), Ordering::Relaxed);

                        let (curtailment_state, sleep_fan_pwm) = {
                            let curt = thermal_curtailment.lock().await;
                            // W11 B-1: clamp the curtailment SLEEP fan to the home
                            // PWM-30 cap (FAN_PWM_SAFETY_MAX), not just the IP ceiling
                            // (FAN_PWM_MAX=100). Sleep is the quietest state and must
                            // never exceed the home cap even if sleep_fan_pwm is raised
                            // above 30 in dcentrald-thermal — defense-in-depth on the
                            // load-bearing PWM-30 contract.
                            // THERM-2: also respect a LOWER per-profile quiet ceiling
                            // (`cfg_fan_max_pwm`), matching the active-mining arm — so a
                            // profile with fan_max_pwm < 30 is never louder asleep than
                            // awake. `effective_sleep_fan_pwm` only ever lowers the value.
                            (
                                curt.state(),
                                effective_sleep_fan_pwm(curt.sleep_fan_pwm(), cfg_fan_max_pwm),
                            )
                        };

                        let publish_sleep_snapshot = |state_tx: &watch::Sender<dcentrald_api::MinerState>,
                                                       power_tx: &watch::Sender<dcentrald_autotuner::LivePowerEstimate>,
                                                       fan_pwm: u8,
                                                       fan_rpm: u32,
                                                       chain_status: &str| {
                            state_tx.send_modify(|s| {
                                s.hashrate_ghs = 0.0;
                                s.hashrate_5s_ghs = 0.0;
                                s.fans.pwm = fan_pwm;
                                s.fans.rpm = fan_rpm;
                                for chain in &mut s.chains {
                                    chain.hashrate_ghs = 0.0;
                                    chain.status = chain_status.to_string();
                                    // Curtailment sleep de-energizes the boards, so
                                    // there is genuinely no chain temperature — clear
                                    // both the value and its provenance (the UI shows
                                    // "no telemetry", which is honest while asleep).
                                    chain.temp_c = 0.0;
                                    chain.temp_source = None;
                                }
                            });

                            let sleep_wall_watts = 25.0;
                            let _ = power_tx.send(dcentrald_autotuner::LivePowerEstimate {
                                board_watts: sleep_wall_watts,
                                wall_watts: sleep_wall_watts,
                                per_chain_watts: vec![0.0; thermal_chain_ids.len()],
                                efficiency_jth: 0.0,
                                // Curtailed/asleep: 0.0 J/TH is the "no reading"
                                // sentinel, not a settled efficiency measurement.
                                efficiency_jth_low_confidence: true,
                                btu_h: dcentrald_autotuner::btu_from_watts(sleep_wall_watts),
                                calibrated: false,
                                calibration_multiplier: None,
                                source: "curtailment".to_string(),
                                dispatcher_limits: Vec::new(),
                                watt_cap: None,
                                timestamp_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                            });
                        };

                        match curtailment_state {
                            dcentrald_thermal::curtailment::CurtailmentState::EnteringSleep => {
                                thermal_curtailment_sleeping.store(true, Ordering::Release);
                                for board_temp in &thermal_board_temps {
                                    board_temp.store(0, Ordering::Release);
                                }
                                for seen_at in &thermal_board_temp_seen_at {
                                    seen_at.store(0, Ordering::Release);
                                }
                                if let Some(ref led) = thermal_led_tx {
                                    let _ = led.try_send(LedCommand::SetPattern(LedPattern::Sleep));
                                }
                                if let Some(ref fan) = thermal_fan {
                                    fan.set_speed(sleep_fan_pwm);
                                }
                                let sleep_fan_rpm = thermal_fan
                                    .as_ref()
                                    .map(|f| f.get_rpm())
                                    .unwrap_or(0);

                                let sleep_ok = match thermal_pic_type {
                                    PicType::NoPic => match dcentrald_hal::platform::amlogic::disable_psu() {
                                        Ok(()) => true,
                                        Err(e) => {
                                            error!(error = %e, "Curtailment sleep: failed to disable NoPic PSU");
                                            false
                                        }
                                    },
                                    _ => {
                                        let mut all_ok = true;
                                        if let Some(ref tx) = thermal_voltage_tx {
                                            for &addr in &thermal_pic_addrs {
                                                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                                                if let Err(e) = tx.try_send(VoltageCommand::DisableVoltage {
                                                    chain_id: None,
                                                    chip_id: thermal_chip_id,
                                                    pic_addr: addr,
                                                    reply_tx: Some(reply_tx),
                                                }) {
                                                    match &e {
                                                        VoltageTrySendError::Full(_) => warn!(pic_addr = format_args!("0x{:02X}", addr), "voltage mailbox full, rejecting DisableVoltage (curtailment sleep)"),
                                                        VoltageTrySendError::Disconnected => error!(pic_addr = format_args!("0x{:02X}", addr), "voltage worker thread dead — daemon shutdown imminent (curtailment sleep)"),
                                                        other => warn!(pic_addr = format_args!("0x{:02X}", addr), error = %other, "voltage mailbox rejected DisableVoltage (curtailment sleep)"),
                                                    }
                                                    warn!(pic_addr = format_args!("0x{:02X}", addr), error = %e, "Curtailment sleep: failed to queue voltage disable");
                                                    all_ok = false;
                                                    continue;
                                                }

                                                match tokio::time::timeout(Duration::from_secs(3), reply_rx).await {
                                                    Ok(Ok(Ok(VoltageCommandReply::Disabled))) => {}
                                                    Ok(Ok(Ok(other))) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), reply = ?other, "Curtailment sleep: unexpected voltage reply");
                                                        all_ok = false;
                                                    }
                                                    Ok(Ok(Err(detail))) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), error = %detail, "Curtailment sleep: voltage disable failed");
                                                        all_ok = false;
                                                    }
                                                    Ok(Err(_)) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), "Curtailment sleep: voltage disable reply dropped");
                                                        all_ok = false;
                                                    }
                                                    Err(_) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), "Curtailment sleep: timed out waiting for voltage disable");
                                                        all_ok = false;
                                                    }
                                                }
                                            }
                                        } else {
                                            warn!("Curtailment sleep: runtime voltage channel unavailable");
                                            all_ok = false;
                                        }
                                        all_ok
                                    }
                                };

                                publish_sleep_snapshot(
                                    &thermal_state_tx,
                                    &thermal_power_tx,
                                    sleep_fan_pwm,
                                    sleep_fan_rpm,
                                    if sleep_ok { "Sleeping" } else { "Sleep pending" },
                                );

                                if sleep_ok {
                                    let mut curt = thermal_curtailment.lock().await;
                                    curt.sleep_complete();
                                    info!(fan_pwm = sleep_fan_pwm, "Curtailment sleep complete — hash boards powered down, low-power standby active");
                                } else {
                                    warn!("Curtailment sleep request did not complete cleanly — will retry on next thermal tick");
                                }
                                continue;
                            }
                            dcentrald_thermal::curtailment::CurtailmentState::Sleeping => {
                                thermal_curtailment_sleeping.store(true, Ordering::Release);
                                for board_temp in &thermal_board_temps {
                                    board_temp.store(0, Ordering::Release);
                                }
                                for seen_at in &thermal_board_temp_seen_at {
                                    seen_at.store(0, Ordering::Release);
                                }
                                if let Some(ref fan) = thermal_fan {
                                    fan.set_speed(sleep_fan_pwm);
                                }
                                let sleep_fan_rpm = thermal_fan
                                    .as_ref()
                                    .map(|f| f.get_rpm())
                                    .unwrap_or(0);
                                publish_sleep_snapshot(
                                    &thermal_state_tx,
                                    &thermal_power_tx,
                                    sleep_fan_pwm,
                                    sleep_fan_rpm,
                                    "Sleeping",
                                );
                                continue;
                            }
                            dcentrald_thermal::curtailment::CurtailmentState::Waking => {
                                if let Some(ref fan) = thermal_fan {
                                    fan.set_speed(cfg_fan_min_pwm);
                                }
                                let wake_fan_rpm = thermal_fan
                                    .as_ref()
                                    .map(|f| f.get_rpm())
                                    .unwrap_or(0);
                                publish_sleep_snapshot(
                                    &thermal_state_tx,
                                    &thermal_power_tx,
                                    cfg_fan_min_pwm,
                                    wake_fan_rpm,
                                    "Waking",
                                );

                                let wake_ok = if thermal_emergency_active(&thermal_emergency_latch) {
                                    warn!(
                                        "Curtailment wake: refusing voltage re-enable while thermal emergency is active"
                                    );
                                    false
                                } else {
                                    match thermal_pic_type {
                                    PicType::NoPic => {
                                        error!("Curtailment wake: refusing NoPic re-energization because the standard daemon does not own the retained Amlogic power/thermal service");
                                        false
                                    },
                                    _ => {
                                        let mut all_ok = true;
                                        if let Some(ref tx) = thermal_voltage_tx {
                                            for &addr in &thermal_pic_addrs {
                                                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                                                if let Err(e) = tx.try_send(VoltageCommand::SetVoltage {
                                                    chain_id: None,
                                                    chip_id: thermal_chip_id,
                                                    pic_addr: addr,
                                                    target_mv: thermal_restart_voltage_mv,
                                                    reply_tx: Some(reply_tx),
                                                }) {
                                                    match &e {
                                                        VoltageTrySendError::Full(_) => warn!(pic_addr = format_args!("0x{:02X}", addr), "voltage mailbox full, rejecting SetVoltage (curtailment wake)"),
                                                        VoltageTrySendError::Disconnected => error!(pic_addr = format_args!("0x{:02X}", addr), "voltage worker thread dead — daemon shutdown imminent (curtailment wake)"),
                                                        other => warn!(pic_addr = format_args!("0x{:02X}", addr), error = %other, "voltage mailbox rejected SetVoltage (curtailment wake)"),
                                                    }
                                                    warn!(pic_addr = format_args!("0x{:02X}", addr), error = %e, "Curtailment wake: failed to queue voltage enable");
                                                    all_ok = false;
                                                    continue;
                                                }

                                                match tokio::time::timeout(Duration::from_secs(3), reply_rx).await {
                                                    Ok(Ok(Ok(VoltageCommandReply::Applied(_)))) => {}
                                                    Ok(Ok(Ok(other))) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), reply = ?other, "Curtailment wake: unexpected voltage reply");
                                                        all_ok = false;
                                                    }
                                                    Ok(Ok(Err(detail))) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), error = %detail, "Curtailment wake: voltage enable failed");
                                                        all_ok = false;
                                                    }
                                                    Ok(Err(_)) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), "Curtailment wake: voltage enable reply dropped");
                                                        all_ok = false;
                                                    }
                                                    Err(_) => {
                                                        warn!(pic_addr = format_args!("0x{:02X}", addr), "Curtailment wake: timed out waiting for voltage enable");
                                                        all_ok = false;
                                                    }
                                                }
                                            }
                                        } else {
                                            warn!("Curtailment wake: runtime voltage channel unavailable");
                                            all_ok = false;
                                        }
                                        all_ok
                                    }
                                    }
                                };

                                if wake_ok {
                                    if let Some(ref led) = thermal_led_tx {
                                        let _ = led.try_send(LedCommand::SetPattern(LedPattern::Mining));
                                    }
                                    thermal_state_tx.send_modify(|s| {
                                        for chain in &mut s.chains {
                                            chain.status = "Mining".to_string();
                                        }
                                    });
                                    let mut curt = thermal_curtailment.lock().await;
                                    curt.wake_complete();
                                    thermal_curtailment_sleeping.store(false, Ordering::Release);
                                    info!(restart_voltage_mv = thermal_restart_voltage_mv, "Curtailment wake complete — voltage restored, dispatcher may resume on next work cycle");
                                } else {
                                    warn!("Curtailment wake request did not complete cleanly — will retry on next thermal tick");
                                }
                                continue;
                            }
                            dcentrald_thermal::curtailment::CurtailmentState::Active => {
                                thermal_curtailment_sleeping.store(false, Ordering::Release);
                            }
                        }

                        // Per-chain board temperatures from BM1387 I2C passthrough.
                        // The WorkDispatcher reads these every 5s via the FPGA CMD FIFO
                        // and stores f32 bits in shared atomics. We read them here.
                        let mut max_board_temp: Option<f32> = None;
                        let mut per_chain_board_temps: Vec<Option<f32>> = vec![None; thermal_board_temps.len()];
                        let now_s = board_temp_time_base_thermal.elapsed().as_secs() as u32;
                        for (i, (board_temp_atomic, board_temp_seen_at_atomic)) in thermal_board_temps
                            .iter()
                            .zip(thermal_board_temp_seen_at.iter())
                            .enumerate()
                        {
                            let bits = board_temp_atomic.load(Ordering::Relaxed);
                            let seen_at_s = board_temp_seen_at_atomic.load(Ordering::Relaxed);
                            let board_temp = f32::from_bits(bits);
                            let fresh = seen_at_s != 0
                                && now_s.saturating_sub(seen_at_s)
                                    <= dcentrald_autotuner::chip_stats::BOARD_TEMP_STALE_TIMEOUT_S
                                        as u32;
                            if bits != 0 && fresh && board_temp > 0.0 && board_temp < 150.0 {
                                per_chain_board_temps[i] = Some(board_temp);
                                if max_board_temp.is_none_or(|current| board_temp > current) {
                                    max_board_temp = Some(board_temp);
                                }
                                if let Some(state) = board_temp_stuck_states.get_mut(i) {
                                    if update_stuck_board_temp_sensor(
                                        state,
                                        Some(bits),
                                        BOARD_TEMP_STUCK_IDENTICAL_TICKS,
                                    ) {
                                        warn!(
                                            chain_idx = i,
                                            board_temp_c = format_args!("{:.1}", board_temp),
                                            repeated_ticks = state.repeated,
                                            sensor_status = "suspect_stuck",
                                            "Board temp chain {} repeated the same plausible value for {} thermal ticks; treating sensor as suspect telemetry and relying on die-temp cross-check where enabled",
                                            i,
                                            state.repeated,
                                        );
                                    }
                                }
                                tracing::debug!(
                                    chain_idx = i,
                                    board_temp_c = format_args!("{:.1}", board_temp),
                                    "Board temp chain {}: {:.1}C",
                                    i, board_temp,
                                );
                            } else if let Some(state) = board_temp_stuck_states.get_mut(i) {
                                let _ = update_stuck_board_temp_sensor(
                                    state,
                                    None,
                                    BOARD_TEMP_STUCK_IDENTICAL_TICKS,
                                );
                            }
                        }
                        // BUG-11 (S9 board+chip temp missing from telemetry):
                        // publish per-chain temps into the API/dashboard snapshot.
                        // When a chain's BM1387-passthrough board sensor returned
                        // no data (the NORMAL S9 case — TMP451/ADT7461/NCT218 need
                        // 12V hashboard power, while the PIC answers on the 3.3V
                        // rail), fall back to the honest XADC SoC die temp instead
                        // of publishing 0.0. Publishing 0.0 made the dashboard show
                        // "N/A" / "No power to hash board" on a healthy, actively
                        // mining S9 (ChainCard/HashBoardStrip treat temp_c==0 as
                        // unpowered). `temp_source` labels the fallback so the UI
                        // can present it honestly as a die-temp proxy, never as a
                        // per-board sensor reading. die_temp is only used as the
                        // fallback when it is a valid reading (0 < die < 125) — on
                        // a platform with no XADC (Amlogic) or a failed read it is
                        // left as 0.0/no-source, which the UI still treats as "no
                        // telemetry" rather than a fabricated number.
                        // Single source of truth with the host-testable helper
                        // `assemble_chain_published_temp` (BUG-11 tests live in
                        // dcentrald-api-types::thermal_model).
                        thermal_state_tx.send_modify(|s| {
                            for (i, chain) in s.chains.iter_mut().enumerate() {
                                if i >= per_chain_board_temps.len() {
                                    continue;
                                }
                                let (temp_c, source) =
                                    dcentrald_api_types::thermal_model::assemble_chain_published_temp(
                                        per_chain_board_temps[i],
                                        die_temp,
                                    );
                                chain.temp_c = temp_c;
                                chain.temp_source = source.map(|s| s.to_string());
                            }
                        });
                        // Use the hottest temperature for thermal control.
                        // This ensures fans respond to the hottest board, not just
                        // the SoC die temp which is typically 20-30C cooler.
                        if let Some(max_board_temp) = max_board_temp.filter(|temp| *temp > die_temp) {
                            tracing::debug!(
                                die_temp = format_args!("{:.1}", die_temp),
                                max_board_temp = format_args!("{:.1}", max_board_temp),
                                "Thermal control using board temp {:.1}C (die temp {:.1}C)",
                                max_board_temp, die_temp,
                            );
                        }

                        // SAFETY (S9 2026-04-19 root cause — LOAD-BEARING, do NOT regress): when board
                        // temperatures disappear, ALWAYS fall back to the control-board XADC die temp,
                        // for BOTH skip_board_temp values. NEVER trigger an EmergencyShutdown from an
                        // empty board-temp set alone — S9 board-temp I2C sensors don't respond via
                        // BM1387 passthrough, so an empty board read is normal and die temp (~45C at
                        // 500MHz) is the safe thermal input. (An earlier version of this comment said
                        // the OPPOSITE — "do not fall back to die temp; let the controller see the empty
                        // set so it triggers shutdown" — that wording was WRONG and is superseded; the
                        // code below is correct, do NOT "fix" it to match the old comment.)
                        //
                        // The pure decision is extracted to
                        // `dcentrald_api_types::thermal_model::assemble_thermal_input` so the daemon and
                        // the host-runnable regression test
                        // (`empty_board_temps_fall_back_to_die_temp_never_empty_s9_2026_04_19`) share
                        // ONE source of truth. The helper collects every valid `Some(_)` board temp and,
                        // when none exist, pushes `die_temp` (never empty) — identical for both
                        // skip_board_temp values. skip_board_temp only changes the log line below, not
                        // the assembled vector. SAF-2's `always_include_die` can append a real XADC
                        // die reading when the platform is opted into board-sensor cross-checking.
                        // `max_board_temp.is_none()` is true iff zero valid board
                        // temps were collected (max is only set in the same branch that produced a
                        // `Some(_)` per-chain temp), so it is the faithful "board temps empty" predicate.
                        let always_include_die = thermal_die_crosscheck_enabled(
                            thermal_has_xadc,
                            die_temp,
                            &thermal_platform_marker,
                            thermal_include_die_on_am2,
                        );
                        let temps: Vec<f32> =
                            dcentrald_api_types::thermal_model::assemble_thermal_input(
                                &per_chain_board_temps,
                                die_temp,
                                thermal_skip_board_temp,
                                always_include_die,
                            );
                        if always_include_die && max_board_temp.is_some() {
                            tracing::debug!(
                                die_temp_c = format_args!("{:.1}", die_temp),
                                "SAF-2: appended XADC die temp to thermal input for board-sensor cross-check"
                            );
                        }
                        if max_board_temp.is_none() {
                            if thermal_skip_board_temp {
                                tracing::debug!(
                                    die_temp_c = format_args!("{:.1}", die_temp),
                                    "skip_board_temp: using XADC die temp {:.1}C for thermal control",
                                    die_temp,
                                );
                            } else {
                                tracing::debug!(
                                    die_temp_c = format_args!("{:.1}", die_temp),
                                    "Board temp sensors returned no data — using XADC die temp {:.1}C as fallback",
                                    die_temp,
                                );
                            }
                        } else if thermal_pic_type == PicType::NoPic && thermal_board_temps.len() == 1 {
                            thermal_board_temps[0]
                                .store(max_board_temp.unwrap_or(die_temp).to_bits(), Ordering::Release);
                            if !thermal_board_temp_seen_at.is_empty() {
                                thermal_board_temp_seen_at[0].store(now_s.max(1), Ordering::Release);
                            }
                        }

                        // THERMAL-7: did this tick have ANY real hashboard board-temp
                        // proof? If so, a failed XADC read is covered and must not
                        // escalate (this is the S9 case — board temps present, XADC is
                        // just a proxy). Captured BEFORE the shadowing `unwrap_or` below
                        // collapses the Option.
                        let had_board_temp_proof = max_board_temp.is_some();

                        let max_board_temp = max_board_temp.unwrap_or(die_temp);

                        // Read fan RPM (per-fan data built after thermal action sets the new PWM).
                        // THERMAL-9: build the FULL per-fan RPM vector once, here, so both
                        // the controller (which wants a single representative RPM for its
                        // own fan-failure heuristic) AND the supervisor (which counts how
                        // many fans are turning) see honest data. Passing the supervisor a
                        // single `vec![min_rpm]` made it believe there is exactly ONE fan;
                        // if that single value was the slowest fan reading 0 RPM, the
                        // supervisor saw `working_fans=0, total_fans=1` and fired a spurious
                        // FanPanic → EmergencyShutdown even when the other 3 fans were fine.
                        let (tach_available, per_fan_rpms) = thermal_fan
                            .as_ref()
                            .map(|fan| {
                                collect_fan_tach_evidence(
                                    || fan.tach_available(),
                                    || {
                                        fan.get_per_fan_rpm()
                                            .into_iter()
                                            .map(|(_, rpm)| rpm)
                                            .collect()
                                    },
                                )
                            })
                            .unwrap_or_else(|| (false, Vec::new()));
                        controller.set_tach_available(tach_available);
                        // Unavailable, warming, or concurrently lost tach is no
                        // fan-count evidence. The supervisor interprets an
                        // empty vector as unknown and continues temperature-
                        // based protection; a fabricated zero would mean a
                        // proven stopped fan and trigger FanPanic.
                        // Single representative RPM for the controller's own
                        // tick: the slowest fan, so a stalled fan still
                        // surfaces. Use zero only as an ignored input while
                        // tach availability is false; never present it to the
                        // supervisor as a measured stopped fan.
                        let fan_rpm = if per_fan_rpms.is_empty() {
                            0
                        } else {
                            per_fan_rpms.iter().copied().min().unwrap_or(0)
                        };

                        // Apply any pending operator PID-tuning commands
                        // (P1, handler-clamped) BEFORE the tick so new gains
                        // take effect this cycle. Non-blocking drain.
                        while let Ok((kp, ki, kd)) = thermal_pid_command_rx.try_recv() {
                            controller.set_pid_params(kp, ki, kd);
                        }

                        let action = controller.tick(&temps, fan_rpm);
                        // Publish the real PID state for honest
                        // /api/debug/pid-state telemetry (no fabrication).
                        let _ = thermal_pid_state_tx.send(Some(controller.pid_state()));

                        // Wave-G G1 (E3b): when the LuxOS-shape supervisor is
                        // enabled, drive its 6-layer FSM from a faithful
                        // per-board ThermalTick and reconcile strongest-
                        // safety-wins. Disabled (default) → `action` is
                        // unchanged (byte-identical path). The supervisor can
                        // only make the response MORE conservative; it never
                        // weakens the controller's fail-closed floor, and its
                        // RequestFansMax is capped at cfg_fan_max_pwm (never
                        // 255) inside `reconcile_with_supervisor`.
                        //
                        // ATM step capture: the supervisor's
                        // `RequestProfileStepDown` / `RequestProfileStepUp`
                        // advisories are reconciled into the fan/shutdown
                        // `action` (where they are intentionally inert), but we
                        // ALSO capture the step intent here so the dispatch
                        // below can drive the autotuner's ATM frequency-step
                        // ceiling. `None` = no profile-step advice this tick.
                        let mut atm_step_request: Option<dcentrald_thermal::supervisor::SupervisorAction> = None;
                        let action = if let Some(ref mut sup) = thermal_supervisor {
                            let board_sensors: Vec<
                                dcentrald_thermal::supervisor::BoardSensors,
                            > = thermal_chain_ids
                                .iter()
                                .enumerate()
                                .map(|(i, &chain_id)| {
                                    dcentrald_thermal::supervisor::BoardSensors {
                                        chain_id,
                                        // Per-chain board temp, with the SAME
                                        // load-bearing XADC die-temp fallback the
                                        // controller path uses (assemble_thermal_input
                                        // above + the S9 2026-04-19 rule at the top of
                                        // this block): when a chain's board temp is
                                        // stale/missing, fall back to the real die_temp
                                        // instead of an EMPTY vec. Empty would trip the
                                        // supervisor's min_per_board gate →
                                        // RequestBoardPowerOff{SensorFailure} →
                                        // whole-unit EmergencyShutdown even when the
                                        // XADC die temp is safe (~45 C) — the exact
                                        // spurious-shutdown the controller fallback
                                        // exists to prevent (S9 board-temp I2C sensors
                                        // routinely return nothing via BM1387
                                        // passthrough). die_temp is a REAL reading, so a
                                        // genuinely hot die still escalates
                                        // (board_hot/board_panic); only the
                                        // empty-sensors false alarm is removed.
                                        // (prod-readiness hunt needs_more_thought #2.)
                                        pcb_temps_c: per_chain_board_temps
                                            .get(i)
                                            .copied()
                                            .flatten()
                                            .map(|t| vec![t])
                                            .unwrap_or_else(|| vec![die_temp]),
                                        // Per-chip die diodes are not wired
                                        // into this loop yet; empty is safe
                                        // (chip_panic simply never triggers
                                        // here — board thresholds still do).
                                        chip_temps_c: Vec::new(),
                                        powered_on: true,
                                    }
                                })
                                .collect();
                            let tick = dcentrald_thermal::supervisor::ThermalTick {
                                board_sensors,
                                // THERMAL-9: pass the FULL per-fan RPM vector, not a single
                                // `vec![min_rpm]`. The supervisor counts working fans
                                // (rpm > 0) against `min_fans`; a single min-value element
                                // made one slow/zero fan look like a total fan loss and
                                // tripped a spurious FanPanic. Unavailable or warming
                                // tach produces an empty vector, meaning no RPM evidence.
                                fan_tach_rpms: per_fan_rpms.clone(),
                                current_fan_pwm: controller.current_pwm(),
                                hydro_inlet_c: None,
                                hydro_outlet_c: None,
                                tick_elapsed_secs: thermal_pid_interval_s as u32,
                            };
                            let sup_actions = sup.tick(&tick);
                            // Honest telemetry for /api/thermal/supervisor.
                            let _ = thermal_supervisor_snapshot_tx
                                .send(Some(sup.snapshot()));

                            // R-11: record hardware-safety events in the
                            // operator audit log. De-duped (one row per
                            // transition into a safety event) so it does NOT
                            // spam every tick. Best-effort + fail-safe:
                            // `push_audit_event` writes the ring + on-disk log
                            // and NEVER panics (no-op on a poisoned lock), so
                            // this can never affect mining.
                            {
                                use dcentrald_thermal::supervisor::SupervisorAction as SupAct;
                                use dcentrald_thermal::supervisor::ThermalReason;

                                // (1) Whole-unit emergency shutdown. De-dup on
                                //     the reason: a change of cause re-logs, a
                                //     persisting cause does not.
                                let emergency_reason =
                                    sup_actions.iter().find_map(|a| match a {
                                        SupAct::RequestEmergencyShutdown { reason } => {
                                            Some(*reason)
                                        }
                                        _ => None,
                                    });
                                if emergency_reason != audit_last_emergency_reason {
                                    if let Some(reason) = emergency_reason {
                                        let event = if reason == ThermalReason::FanPanic {
                                            let working_fans = tick
                                                .fan_tach_rpms
                                                .iter()
                                                .filter(|r| **r > 0)
                                                .count()
                                                .min(u8::MAX as usize)
                                                as u8;
                                            dcentrald_api_types::audit_log::AuditEvent::FanPanic {
                                                working_fans,
                                                min_fans: thermal_audit_min_fans,
                                            }
                                        } else {
                                            dcentrald_api_types::audit_log::AuditEvent::ThermalEmergencyShutdown {
                                                reason: thermal_reason_label(reason).to_string(),
                                            }
                                        };
                                        dcentrald_api::push_audit_event(
                                            &thermal_audit_app_state,
                                            "thermal_supervisor",
                                            event,
                                        );
                                    }
                                    audit_last_emergency_reason = emergency_reason;
                                }

                                // (2) Per-board power-off. De-dup per chain_id:
                                //     a board that STAYS off does not re-log; a
                                //     board that recovered then trips again does.
                                let mut boards_off_this_tick: std::collections::HashSet<u8> =
                                    std::collections::HashSet::new();
                                for act in &sup_actions {
                                    if let SupAct::RequestBoardPowerOff {
                                        chain_id, reason, ..
                                    } = act
                                    {
                                        boards_off_this_tick.insert(*chain_id);
                                        // `insert` returns true only when the
                                        // chain was NOT already latched → first
                                        // occurrence this off-episode.
                                        if audit_boards_off_latched.insert(*chain_id) {
                                            let event = match reason {
                                                ThermalReason::BoardPanic
                                                | ThermalReason::ChipPanic => {
                                                    // Over-temp board-off: carry
                                                    // the board's hottest valid
                                                    // reading + the crossed
                                                    // panic threshold.
                                                    let max_temp_c = tick
                                                        .board_sensors
                                                        .iter()
                                                        .find(|b| b.chain_id == *chain_id)
                                                        .map(|b| {
                                                            b.pcb_temps_c
                                                                .iter()
                                                                .chain(b.chip_temps_c.iter())
                                                                .cloned()
                                                                .fold(f32::MIN, f32::max)
                                                        })
                                                        .filter(|t| t.is_finite())
                                                        .unwrap_or(0.0);
                                                    let threshold_c = if *reason
                                                        == ThermalReason::ChipPanic
                                                    {
                                                        thermal_audit_chip_panic_c
                                                    } else {
                                                        thermal_audit_board_panic_c
                                                    };
                                                    dcentrald_api_types::audit_log::AuditEvent::OvertempShutdown {
                                                        max_temp_c,
                                                        threshold_c,
                                                    }
                                                }
                                                other => {
                                                    dcentrald_api_types::audit_log::AuditEvent::BoardPowerOff {
                                                        chain_id: *chain_id,
                                                        reason: thermal_reason_label(*other)
                                                            .to_string(),
                                                    }
                                                }
                                            };
                                            dcentrald_api::push_audit_event(
                                                &thermal_audit_app_state,
                                                "thermal_supervisor",
                                                event,
                                            );
                                        }
                                    }
                                }
                                // Clear the latch for any chain no longer
                                // commanded off, so a future re-trip logs afresh.
                                audit_boards_off_latched
                                    .retain(|c| boards_off_this_tick.contains(c));
                            }

                            // Capture the ATM profile-step advisory for the
                            // dispatch below. Step-DOWN (the safe cut-hash
                            // direction) wins over Step-UP if both ever appear
                            // in the same tick — we never raise hash on a tick
                            // that also asked to cut it.
                            use dcentrald_thermal::supervisor::SupervisorAction as SupAct;
                            if sup_actions
                                .iter()
                                .any(|a| matches!(a, SupAct::RequestProfileStepDown { .. }))
                            {
                                atm_step_request = sup_actions
                                    .iter()
                                    .find(|a| matches!(a, SupAct::RequestProfileStepDown { .. }))
                                    .cloned();
                            } else if sup_actions
                                .iter()
                                .any(|a| matches!(a, SupAct::RequestProfileStepUp))
                            {
                                atm_step_request = Some(SupAct::RequestProfileStepUp);
                            }
                            dcentrald_thermal::controller::reconcile_with_supervisor(
                                action,
                                &sup_actions,
                                cfg_fan_max_pwm,
                            )
                        } else {
                            action
                        };

                        // THERMAL-7 fail-closed escalation: if the XADC has been blind for
                        // `XADC_BLIND_FAIL_LIMIT` consecutive ticks AND there is no hashboard
                        // board-temp proof to cover for it, we have NO thermal visibility at
                        // all. Rather than loop forever on the benign 45.0C fallback (which
                        // would keep the controller in NormalMining and the boards energized
                        // with zero thermal protection), force an EmergencyShutdown. This is
                        // strictly safer on failure — it de-energizes the hash boards and the
                        // emergency arm caps fans at the home PWM cap. A single good XADC read
                        // or any tick with real board temps resets the counter, so this only
                        // fires on a sustained, total loss of thermal sensing. We never WEAKEN
                        // a more-severe action the controller/supervisor already chose, so only
                        // override when `action` is not already an emergency response.
                        let action = if thermal7_xadc_blind_escalates(
                            thermal_has_xadc,
                            xadc_failed,
                            had_board_temp_proof,
                            consecutive_xadc_failures,
                            XADC_BLIND_FAIL_LIMIT,
                            matches!(action, ThermalAction::EmergencyShutdown),
                        ) {
                            error!(
                                consecutive = consecutive_xadc_failures,
                                "THERMAL-7: XADC blind for {} consecutive ticks with no board-temp fallback — \
                                 no thermal proof; escalating to fail-closed EmergencyShutdown",
                                consecutive_xadc_failures,
                            );
                            ThermalAction::EmergencyShutdown
                        } else {
                            action
                        };

                        // THERMAL-8 fail-closed escalation (non-XADC / Amlogic twin
                        // of THERMAL-7): on a non-XADC platform the control-board
                        // die temp is the hardcoded 45.0C fallback, so THERMAL-7
                        // never fires. If the SOLE real thermal source — the
                        // per-chain board/chip-temp pipeline — has been fully stale
                        // for `BOARD_TEMP_BLIND_FAIL_LIMIT` consecutive ticks, the
                        // loop has NO thermal proof at all and would mine forever on
                        // the benign 45.0C fallback. Force the same fail-CLOSED
                        // EmergencyShutdown. Pure decision in `thermal8_board_blind_tick`:
                        // a single tick with ANY real board temp resets the counter
                        // (respects "never emergency on empty board temps ALONE" —
                        // only sustained TOTAL blindness escalates), it is strictly
                        // gated on `!thermal_has_xadc` (the XADC/Zynq beta path is
                        // byte-identical), and it never WEAKENs a more-severe action
                        // already chosen.
                        let (next_board_temp_failures, thermal8_escalate) =
                            thermal8_board_blind_tick(
                                thermal_has_xadc,
                                had_board_temp_proof,
                                consecutive_board_temp_failures,
                                BOARD_TEMP_BLIND_FAIL_LIMIT,
                                matches!(action, ThermalAction::EmergencyShutdown),
                            );
                        consecutive_board_temp_failures = next_board_temp_failures;
                        let action = if thermal8_escalate {
                            error!(
                                consecutive = consecutive_board_temp_failures,
                                "THERMAL-8: non-XADC board-temp pipeline blind for {} consecutive \
                                 ticks with no die-temp readback (45.0C is a fallback, not a \
                                 measurement) — no thermal proof; escalating to fail-closed \
                                 EmergencyShutdown",
                                consecutive_board_temp_failures,
                            );
                            ThermalAction::EmergencyShutdown
                        } else {
                            action
                        };

                        // ATM (Advanced Thermal Management) profile-step
                        // dispatch. Consumes the supervisor's
                        // `RequestProfileStepDown` / `RequestProfileStepUp`
                        // advisories captured above by driving the autotuner's
                        // dedicated `FrequencyLimitSource::AtmStep` ceiling
                        // through the `thermal_freq_tx` channel this loop
                        // already owns. Only reachable when the (default-off,
                        // operator-gated) thermal supervisor is enabled — with
                        // it disabled `atm_step_request` is always `None`, so
                        // this whole block is a no-op and the path is
                        // byte-identical.
                        if let Some(req) = atm_step_request.take() {
                            use dcentrald_thermal::supervisor::SupervisorAction as SupAct;

                            let step_dir = match req {
                                SupAct::RequestProfileStepDown { .. } => AtmStepDir::Down,
                                _ => AtmStepDir::Up,
                            };

                            // Thermal safety ALWAYS wins. The reconciled thermal
                            // action this tick being an emergency / fan-failure /
                            // throttle / restart response means hash is being cut
                            // — a step-UP must yield (a hot event wins). Step-DOWN
                            // is the safe direction and is honored regardless.
                            let is_cutting_hash = matches!(
                                action,
                                ThermalAction::EmergencyShutdown
                                    | ThermalAction::FanFailure
                                    | ThermalAction::ThrottleAndFan { .. }
                                    | ThermalAction::RestartInit
                            );

                            // Debounce/rate-limit: at most one ATM step per
                            // `atm_step_min_interval` so a flapping temperature
                            // cannot thrash the profile.
                            let debounced = atm_last_step_at
                                .map(|t| t.elapsed() < atm_step_min_interval)
                                .unwrap_or(false);

                            let desired_ceiling = atm_step_ceiling_decision(
                                step_dir,
                                atm_step_ceiling_mhz,
                                thermal_nominal_freq,
                                atm_step_size_mhz,
                                ATM_STEP_FLOOR_MHZ,
                                is_cutting_hash,
                                debounced,
                            );

                            if desired_ceiling != atm_step_ceiling_mhz {
                                atm_step_ceiling_mhz = desired_ceiling;
                                atm_last_step_at = Some(Instant::now());
                                let dir = match step_dir {
                                    AtmStepDir::Down => "down",
                                    AtmStepDir::Up => "up",
                                };
                                info!(
                                    direction = dir,
                                    ceiling_mhz = desired_ceiling
                                        .map(|c| c as i32)
                                        .unwrap_or(-1),
                                    nominal_mhz = thermal_nominal_freq,
                                    step_mhz = atm_step_size_mhz,
                                    "ATM profile-step: thermal supervisor stepped the active \
                                     profile {} (AtmStep freq ceiling = {})",
                                    dir,
                                    desired_ceiling
                                        .map(|c| format!("{c} MHz"))
                                        .unwrap_or_else(|| "cleared (at nominal max)".to_string()),
                                );
                                for &chain_id in &thermal_chain_ids {
                                    let _ = thermal_freq_tx.try_send(
                                        dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                            chain_id,
                                            max_freq_mhz: desired_ceiling,
                                            source:
                                                dcentrald_autotuner::FrequencyLimitSource::AtmStep,
                                            ack_tx: None,
                                        },
                                    );
                                }
                            }
                        }

                        // Apply thermal action to hardware
                        match action {
                            ThermalAction::SetFanPwm(pwm) => {
                                // Clamp fan PWM to config range. The thermal PID controller
                                // already outputs within [fan_min_pwm, fan_max_pwm], but
                                // enforce here too as a safety net. Home mining needs quiet
                                // fans (PWM 10-30), not the hardcoded 50 floor.
                                let mut pwm = pwm.clamp(
                                    cfg_fan_min_pwm,
                                    cfg_fan_max_pwm,
                                );

                                // Night mode enforcement — cap fan PWM and frequency
                                // during configured quiet hours. Night mode is a pure
                                // noise reduction feature: it never INCREASES anything,
                                // only caps maximums. Safety overrides (EmergencyShutdown,
                                // FanFailure) bypass this by using separate match arms.
                                if thermal_night_mode.enabled {
                                    // FWSTAB-1: compare against the operator's
                                    // LOCAL hour (UTC + configured offset), not
                                    // raw UTC, so a 22:00 quiet window is quiet
                                    // at 22:00 local.
                                    let hour = {
                                        let now = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs();
                                        let utc_hour = ((now / 3600) % 24) as u8;
                                        dcentrald_common::time::local_hour_from_utc(
                                            utc_hour,
                                            thermal_night_mode.timezone_offset_hours,
                                        )
                                    };

                                    let is_night = if thermal_night_mode.start_hour > thermal_night_mode.end_hour {
                                        // Wraps midnight: e.g., 22:00 - 06:00
                                        hour >= thermal_night_mode.start_hour || hour < thermal_night_mode.end_hour
                                    } else {
                                        hour >= thermal_night_mode.start_hour && hour < thermal_night_mode.end_hour
                                    };

                                    if is_night {
                                        // Cap fan PWM during night hours
                                        let night_max = clamp_fan_pwm(thermal_night_mode.max_fan_pwm);
                                        if pwm > night_max {
                                            tracing::debug!(
                                                pwm_before = pwm,
                                                night_max,
                                                "Night mode: capping fan PWM {} -> {}",
                                                pwm, night_max,
                                            );
                                            pwm = night_max;
                                        }

                                        // Cap frequency during night hours via freq command channel.
                                        // The work dispatcher applies this as a ceiling — autotuner
                                        // and thermal throttle requests above this are clamped.
                                        let night_max_freq = thermal_night_mode.max_frequency_mhz;
                                        if thermal_nominal_freq > night_max_freq {
                                            for &chain_id in &thermal_chain_ids {
                                                let _ = thermal_freq_tx.try_send(
                                                    dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                        chain_id,
                                                        max_freq_mhz: Some(night_max_freq),
                                                        source: dcentrald_autotuner::FrequencyLimitSource::QuietMode,
                                                        ack_tx: None,
                                                    }
                                                );
                                            }
                                            tracing::debug!(
                                                night_max_freq,
                                                "Night mode: frequency capped to {} MHz",
                                                night_max_freq,
                                            );
                                        } else {
                                            for &chain_id in &thermal_chain_ids {
                                                let _ = thermal_freq_tx.try_send(
                                                    dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                        chain_id,
                                                        max_freq_mhz: None,
                                                        source: dcentrald_autotuner::FrequencyLimitSource::QuietMode,
                                                        ack_tx: None,
                                                    }
                                                );
                                            }
                                        }
                                    } else {
                                        for &chain_id in &thermal_chain_ids {
                                            let _ = thermal_freq_tx.try_send(
                                                dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                    chain_id,
                                                    max_freq_mhz: None,
                                                    source: dcentrald_autotuner::FrequencyLimitSource::QuietMode,
                                                    ack_tx: None,
                                                }
                                            );
                                        }
                                    }
                                } else {
                                    for &chain_id in &thermal_chain_ids {
                                        let _ = thermal_freq_tx.try_send(
                                            dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                                chain_id,
                                                max_freq_mhz: None,
                                                source: dcentrald_autotuner::FrequencyLimitSource::QuietMode,
                                                ack_tx: None,
                                            }
                                        );
                                    }
                                }

                                // W8 immersion: SKIP the HAL fan write on an
                                // immersion / hydro rig (no chassis fans — the
                                // controller already returns pwm:0; this gate
                                // guarantees no fan command reaches hardware).
                                // Default-OFF: `immersion_active()` is false on
                                // every air-cooled unit, so this write fires as
                                // before. State telemetry below still publishes
                                // the (zero) pwm so the dashboard is honest.
                                if !controller.immersion_active() {
                                    if let Some(ref fan) = thermal_fan {
                                        fan.set_speed(pwm);
                                    }
                                }
                                // Update fan state with per-fan data using the NEW pwm
                                let new_pct = fan_pwm_percent(pwm);
                                let per_fan: Vec<dcentrald_api::PerFanReading> = thermal_fan
                                    .as_ref()
                                    .map(|f| f.get_per_fan_rpm().into_iter().map(|(id, rpm)| {
                                        dcentrald_api::PerFanReading { id, rpm, pwm_percent: new_pct }
                                    }).collect())
                                    .unwrap_or_default();
                                thermal_state_tx.send_modify(|s| {
                                    s.fans.pwm = pwm;
                                    s.fans.rpm = fan_rpm;
                                    s.fans.per_fan = per_fan;
                                });
                                for &chain_id in &thermal_chain_ids {
                                    let _ = thermal_freq_tx.try_send(
                                        dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                            chain_id,
                                            max_freq_mhz: None,
                                            source: dcentrald_autotuner::FrequencyLimitSource::Thermal,
                                            ack_tx: None,
                                        }
                                    );
                                }
                            }
                            ThermalAction::ThrottleAndFan { pwm, freq_reduction_pct } => {
                                let pwm = pwm.clamp(cfg_fan_min_pwm, cfg_fan_max_pwm);
                                warn!(
                                    temp_c = format_args!("{:.1}", die_temp),
                                    fan_pwm = pwm,
                                    freq_reduction_pct,
                                    fan_rpm,
                                    "THERMAL THROTTLE — chips are running hot! Fans maxed out, reducing frequency by {}% to cool down. This is normal under heavy load or high ambient temps.",
                                    freq_reduction_pct,
                                );
                                // LED: slow red blink to indicate thermal warning
                                if let Some(ref led) = thermal_led_tx {
                                    let _ = led.try_send(LedCommand::SetPattern(LedPattern::ThermalWarning));
                                }
                                // W8 immersion: SKIP the HAL fan write on an
                                // immersion / hydro rig. The freq-reduction
                                // throttle below is UNCHANGED — immersion only
                                // bypasses the (nonexistent) chassis fans, never
                                // the hash-side thermal response. Default-OFF:
                                // false on every air-cooled unit → fires as before.
                                if !controller.immersion_active() {
                                    if let Some(ref fan) = thermal_fan {
                                        fan.set_speed(pwm);
                                    }
                                }
                                let throttle_pct = fan_pwm_percent(pwm);
                                let throttle_per_fan: Vec<dcentrald_api::PerFanReading> = thermal_fan
                                    .as_ref()
                                    .map(|f| f.get_per_fan_rpm().into_iter().map(|(id, rpm)| {
                                        dcentrald_api::PerFanReading { id, rpm, pwm_percent: throttle_pct }
                                    }).collect())
                                    .unwrap_or_default();
                                thermal_state_tx.send_modify(|s| {
                                    s.fans.pwm = pwm;
                                    s.fans.rpm = fan_rpm;
                                    s.fans.per_fan = throttle_per_fan;
                                });
                                // Send frequency reduction to work dispatcher as a thermal ceiling.
                                // The dispatcher clamps autotuner requests against this source-owned limit.
                                let reduced_mhz = thermal_nominal_freq
                                    .saturating_mul(100u16.saturating_sub(freq_reduction_pct as u16))
                                    / 100;
                                for &chain_id in &thermal_chain_ids {
                                    if let Err(e) = thermal_freq_tx.try_send(
                                        dcentrald_autotuner::FreqCommand::SetFrequencyLimit {
                                            chain_id,
                                            max_freq_mhz: Some(reduced_mhz),
                                            source: dcentrald_autotuner::FrequencyLimitSource::Thermal,
                                            ack_tx: None,
                                        }
                                    ) {
                                        warn!(
                                            chain_id,
                                            reduced_mhz,
                                            error = %e,
                                            "Thermal throttle: failed to send freq reduction to dispatcher"
                                        );
                                    } else {
                                        info!(
                                            chain_id,
                                            reduced_mhz,
                                            "Thermal throttle: frequency reduced to {} MHz ({}% reduction)",
                                            reduced_mhz, freq_reduction_pct,
                                        );
                                    }
                                }
                            }
                            ThermalAction::EmergencyShutdown => {
                                mark_thermal_emergency_active(&thermal_emergency_latch);
                                error!(
                                    temp_c = format_args!("{:.1}", die_temp),
                                    "EMERGENCY THERMAL SHUTDOWN — disabling all hash boards. The miner will cool down and attempt to restart."
                                );
                                if let Some(ref led) = thermal_led_tx {
                                    let _ = led.try_send(LedCommand::SetPattern(LedPattern::Error));
                                }
                                match thermal_pic_type {
                                    PicType::NoPic => {
                                        match dcentrald_hal::platform::amlogic::disable_psu() {
                                            Ok(()) => warn!("Thermal emergency: NoPic PSU disabled"),
                                            Err(e) => error!(error = %e, "Thermal emergency: failed to disable NoPic PSU"),
                                        }
                                    }
                                    _ => {
                                        // Disable all hash board voltages via the runtime voltage thread.
                                        // Uses platform-aware controller commands instead of S9-only DAC magic.
                                        // Retry up to 3 times — I2C bus may be stuck on first attempt.
                                        let mut all_disabled = false;
                                        for retry in 0..3u8 {
                                            let mut round_ok = true;
                                            if let Some(ref tx) = thermal_voltage_tx {
                                                for &addr in &thermal_pic_addrs {
                                                    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                                                    if let Err(e) = tx.try_send(VoltageCommand::DisableVoltage {
                                                        chain_id: None,
                                                        chip_id: thermal_chip_id,
                                                        pic_addr: addr,
                                                        reply_tx: Some(reply_tx),
                                                    }) {
                                                        match &e {
                                                            VoltageTrySendError::Full(_) => warn!(addr = format_args!("0x{:02X}", addr), "voltage mailbox full, rejecting DisableVoltage (thermal emergency)"),
                                                            VoltageTrySendError::Disconnected => error!(addr = format_args!("0x{:02X}", addr), "voltage worker thread dead — daemon shutdown imminent (thermal emergency)"),
                                                            other => warn!(addr = format_args!("0x{:02X}", addr), error = %other, "voltage mailbox rejected DisableVoltage (thermal emergency)"),
                                                        }
                                                        round_ok = false;
                                                        error!(addr = format_args!("0x{:02X}", addr), error = %e, "Thermal emergency: failed to queue voltage disable");
                                                        continue;
                                                    }
                                                    match tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx).await {
                                                        Ok(Ok(Ok(VoltageCommandReply::Disabled))) => {}
                                                        Ok(Ok(Ok(other))) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), reply = ?other, "Thermal emergency: unexpected voltage-disable reply");
                                                        }
                                                        Ok(Ok(Err(detail))) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), error = %detail, "Thermal emergency: voltage disable failed");
                                                        }
                                                        Ok(Err(_)) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), "Thermal emergency: voltage disable acknowledgement dropped");
                                                        }
                                                        Err(_) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), "Thermal emergency: voltage disable timed out");
                                                        }
                                                    }
                                                }
                                            } else {
                                                // THERM-3 (fail-closed): with no runtime
                                                // voltage channel, no DisableVoltage can be
                                                // sent — this round did NOT power the boards
                                                // down, so it must not count as success.
                                                // Latent on the S9 gating path (the channel is
                                                // always Some); see `thermal_disable_round_ok`.
                                                error!("Thermal emergency: runtime voltage channel unavailable — cannot disable hash boards (fail-closed)");
                                            }
                                            let round_ok = thermal_disable_round_ok(
                                                thermal_voltage_tx.is_some(),
                                                round_ok,
                                            );
                                            if round_ok {
                                                all_disabled = true;
                                                break;
                                            }
                                            if retry < 2 {
                                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                                warn!(retry, "Thermal emergency: retrying voltage disable");
                                            }
                                        }
                                        if !all_disabled {
                                            error!("Thermal emergency: one or more controllers may still be energized after retries");
                                        }
                                    }
                                }
                                if let Some(ref fan) = thermal_fan {
                                    fan.set_speed(
                                        dcentrald_common::FanCommand::emergency_cap(cfg_fan_max_pwm)
                                            .effective_pwm(),
                                    );
                                }
                                // Fire webhook alert — non-blocking try_send so thermal loop is never stalled
                                let _ = thermal_alert_tx.try_send(AlertEvent::EmergencyShutdown {
                                    temp_c: max_board_temp,
                                    chain_id: 0, // all chains affected
                                });
                                warn!("Thermal loop continues monitoring after EmergencyShutdown — hash boards should be disabled, waiting for cooldown");
                                continue; // DO NOT break — keep monitoring so controller can detect cooldown and trigger RestartInit
                            }
                            ThermalAction::FanFailure => {
                                mark_thermal_emergency_active(&thermal_emergency_latch);
                                error!("FAN FAILURE DETECTED — fan RPM reads zero but PWM is set! Shutting down hash boards. Check: fan connector, fan power, fan blades obstructed.");
                                if let Some(ref led) = thermal_led_tx {
                                    let _ = led.try_send(LedCommand::SetPattern(LedPattern::FanFailure));
                                }
                                match thermal_pic_type {
                                    PicType::NoPic => {
                                        match dcentrald_hal::platform::amlogic::disable_psu() {
                                            Ok(()) => warn!("Fan failure: NoPic PSU disabled"),
                                            Err(e) => error!(error = %e, "Fan failure: failed to disable NoPic PSU"),
                                        }
                                    }
                                    _ => {
                                        // Then disable hash board voltages via the runtime voltage thread.
                                        // Retry up to 3 times — I2C bus may be stuck on first attempt.
                                        let mut all_disabled = false;
                                        for retry in 0..3u8 {
                                            let mut round_ok = true;
                                            if let Some(ref tx) = thermal_voltage_tx {
                                                for &addr in &thermal_pic_addrs {
                                                    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                                                    if let Err(e) = tx.try_send(VoltageCommand::DisableVoltage {
                                                        chain_id: None,
                                                        chip_id: thermal_chip_id,
                                                        pic_addr: addr,
                                                        reply_tx: Some(reply_tx),
                                                    }) {
                                                        match &e {
                                                            VoltageTrySendError::Full(_) => warn!(addr = format_args!("0x{:02X}", addr), "voltage mailbox full, rejecting DisableVoltage (fan failure)"),
                                                            VoltageTrySendError::Disconnected => error!(addr = format_args!("0x{:02X}", addr), "voltage worker thread dead — daemon shutdown imminent (fan failure)"),
                                                            other => warn!(addr = format_args!("0x{:02X}", addr), error = %other, "voltage mailbox rejected DisableVoltage (fan failure)"),
                                                        }
                                                        round_ok = false;
                                                        error!(addr = format_args!("0x{:02X}", addr), error = %e, "Fan failure: failed to queue voltage disable");
                                                        continue;
                                                    }
                                                    match tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx).await {
                                                        Ok(Ok(Ok(VoltageCommandReply::Disabled))) => {}
                                                        Ok(Ok(Ok(other))) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), reply = ?other, "Fan failure: unexpected voltage-disable reply");
                                                        }
                                                        Ok(Ok(Err(detail))) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), error = %detail, "Fan failure: voltage disable failed");
                                                        }
                                                        Ok(Err(_)) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), "Fan failure: voltage disable acknowledgement dropped");
                                                        }
                                                        Err(_) => {
                                                            round_ok = false;
                                                            error!(addr = format_args!("0x{:02X}", addr), "Fan failure: voltage disable timed out");
                                                        }
                                                    }
                                                }
                                            } else {
                                                // THERM-3 (fail-closed): with no runtime
                                                // voltage channel, no DisableVoltage can be
                                                // sent — this round did NOT power the boards
                                                // down, so it must not count as success.
                                                // Latent on the S9 gating path (the channel is
                                                // always Some); see `thermal_disable_round_ok`.
                                                error!("Fan failure: runtime voltage channel unavailable — cannot disable hash boards (fail-closed)");
                                            }
                                            let round_ok = thermal_disable_round_ok(
                                                thermal_voltage_tx.is_some(),
                                                round_ok,
                                            );
                                            if round_ok {
                                                all_disabled = true;
                                                break;
                                            }
                                            if retry < 2 {
                                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                                warn!(retry, "Fan failure: retrying voltage disable");
                                            }
                                        }
                                        if !all_disabled {
                                            error!("Fan failure: one or more controllers may still be energized after retries");
                                        }
                                    }
                                }
                                if let Some(ref fan) = thermal_fan {
                                    fan.set_speed(
                                        dcentrald_common::FanCommand::emergency_cap(cfg_fan_max_pwm)
                                            .effective_pwm(),
                                    );
                                }
                                // Fire webhook alert — non-blocking try_send
                                let _ = thermal_alert_tx.try_send(AlertEvent::FanFailure {
                                    rpm: fan_rpm,
                                });
                                warn!("Thermal loop continues monitoring after FanFailure — boards disabled, monitoring for recovery");
                                continue; // DO NOT break — keep monitoring for fan recovery and cooldown
                            }
                            ThermalAction::RestartInit => {
                                // BUG FIX (2026-04-11): Was log-only — boards stayed powered
                                // down after emergency cooldown. Now re-enables voltage and
                                // restores LED. The thermal controller already transitioned
                                // to ColdStart, so subsequent ticks return normal PID actions.
                                // PIC heartbeats kept running during shutdown, so the bus is healthy.
                                clear_thermal_emergency_active(&thermal_emergency_latch);
                                info!(
                                    restart_voltage_mv = thermal_restart_voltage_mv,
                                    "Thermal: Temperature cooled to safe levels — restarting mining. Re-enabling voltage to the platform-safe restart target."
                                );
                                match thermal_pic_type {
                                    PicType::NoPic => {
                                        warn!(
                                            "Thermal recovery requires a new NoPic hardware session; automatic process replacement is suspended until typed disposition receipts exist"
                                        );
                                        let _ = thermal_alert_tx.try_send(AlertEvent::ThermalRestart);
                                        let _ = crate::restart::schedule_daemon_restart(
                                            "thermal_nopic_restart",
                                            Duration::from_secs(1),
                                        );
                                        break;
                                    }
                                    _ => {
                                        // Re-enable voltage on all controllers at a platform-safe restart target.
                                        if let Some(ref tx) = thermal_voltage_tx {
                                            let mut all_reenabled = true;
                                            for &addr in &thermal_pic_addrs {
                                                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                                                if let Err(e) = tx.try_send(VoltageCommand::SetVoltage {
                                                    chain_id: None,
                                                    chip_id: thermal_chip_id,
                                                    pic_addr: addr,
                                                    target_mv: thermal_restart_voltage_mv,
                                                    reply_tx: Some(reply_tx),
                                                }) {
                                                    match &e {
                                                        VoltageTrySendError::Full(_) => warn!(addr = format_args!("0x{:02X}", addr), "voltage mailbox full, rejecting SetVoltage (thermal restart)"),
                                                        VoltageTrySendError::Disconnected => error!(addr = format_args!("0x{:02X}", addr), "voltage worker thread dead — daemon shutdown imminent (thermal restart)"),
                                                        other => warn!(addr = format_args!("0x{:02X}", addr), error = %other, "voltage mailbox rejected SetVoltage (thermal restart)"),
                                                    }
                                                    all_reenabled = false;
                                                    error!(addr = format_args!("0x{:02X}", addr), error = %e, "Thermal restart: failed to queue voltage re-enable");
                                                    continue;
                                                }
                                                match tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx).await {
                                                    Ok(Ok(Ok(VoltageCommandReply::Applied(actual_mv)))) => {
                                                        info!(addr = format_args!("0x{:02X}", addr), actual_mv, "Thermal restart: controller re-enabled at safe restart voltage");
                                                    }
                                                    Ok(Ok(Ok(other))) => {
                                                        all_reenabled = false;
                                                        error!(addr = format_args!("0x{:02X}", addr), reply = ?other, "Thermal restart: unexpected voltage-apply reply");
                                                    }
                                                    Ok(Ok(Err(detail))) => {
                                                        all_reenabled = false;
                                                        error!(addr = format_args!("0x{:02X}", addr), error = %detail, "Thermal restart: voltage re-enable failed");
                                                    }
                                                    Ok(Err(_)) => {
                                                        all_reenabled = false;
                                                        error!(addr = format_args!("0x{:02X}", addr), "Thermal restart: voltage apply acknowledgement dropped");
                                                    }
                                                    Err(_) => {
                                                        all_reenabled = false;
                                                        error!(addr = format_args!("0x{:02X}", addr), "Thermal restart: voltage re-enable timed out");
                                                    }
                                                }
                                            }
                                            if !all_reenabled {
                                                error!("Thermal restart: one or more controllers failed to re-enable at the safe restart voltage");
                                            }
                                        }
                                    }
                                }
                                // LED: back to normal mining pattern
                                if let Some(ref led) = thermal_led_tx {
                                    let _ = led.try_send(LedCommand::SetPattern(LedPattern::Mining));
                                }
                                // Fire webhook for restart event
                                let _ = thermal_alert_tx.try_send(AlertEvent::ThermalRestart);
                                info!("Thermal restart complete — hash boards re-enabled, \
                                       work dispatcher will resume on next pool job");
                            }
                        }
                    }
                }
            }
        }) {
            anyhow::bail!(
                "thermal controller task ownership is unavailable; refusing unowned hardware control"
            );
        }

        // ---- Start state publisher task (WebSocket broadcast every 1s) ----
        let state_shutdown = shutdown.clone();
        let start_time = std::time::Instant::now();
        let publisher_fan = self.fan.clone();
        // Clone a power_rx for the state publisher to read live power data
        // (power_rx was already cloned before app_state was moved into start_api_servers)
        let publisher_power_rx = power_rx_for_publisher.clone();
        // C-4 (Omega P0-5): construct + fire the three home-operator
        // mining-health alerts (PoolDisconnected / MiningStopped /
        // HashBoardOffline) through the same alert channel the thermal loop
        // uses. The monitor lives in runtime::notifications and debounces each
        // event so a flapping condition can't spam the webhook / browser
        // notification surface. The 1 Hz cadence here matches its
        // ALERT_IDLE_CONFIRM_TICKS confirmation window.
        let publisher_alert_tx = alert_tx.clone();
        let publisher_mining_enabled = mining_enabled;
        let publisher_pool_url = self.config.pool.url.clone();
        // HLA-10: operator's degraded-hashrate alert thresholds (0.0 = disabled).
        // The %-form (pct of rated nominal) takes precedence over the absolute
        // floor when set; nominal is resolved per-tick from the detected profile
        // + live chip count below.
        let publisher_degraded_floor_ghs = self.config.mining.degraded_hashrate_alert_floor_ghs;
        let publisher_degraded_pct = self.config.mining.degraded_hashrate_alert_pct;
        let publisher_profile = self.miner_profile;
        // PH-3 auto-recovery ladder inputs (default-OFF; captured into the task).
        let publisher_recovery_config = self.config.mining.recovery_ladder.clone();
        let publisher_recovery_sleeping = curtailment_sleeping.clone();
        let publisher_recovery_state_path =
            std::path::PathBuf::from("/data/dcentrald-recovery-ladder.json");
        // The platform allowlist is fixed at runtime — resolve it once. Only
        // platforms where a daemon restart is PROVEN to recover mining (am1-s9)
        // arm the ladder; every other platform stays alert-only.
        let publisher_recovery_platform_allowed = {
            dcentrald_api_types::hashrate_recovery::platform_recovery_allowed(
                self.platform_identity.platform_marker(),
            )
        };
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let mut alert_monitor = crate::runtime::notifications::MiningAlertMonitor::new();
            // PH-3: construct the ladder only when enabled (shipped default OFF →
            // None → zero behavior change). Fail-closed on a corrupt persisted
            // state file (start exhausted, never act); fresh on a missing one.
            let mut recovery_ladder = if publisher_recovery_config.enabled {
                use dcentrald_api_types::hashrate_recovery::{
                    HashrateRecoveryLadder, PersistedLadderState,
                };
                let persisted = match std::fs::read_to_string(&publisher_recovery_state_path) {
                    Ok(s) => match serde_json::from_str::<PersistedLadderState>(&s) {
                        Ok(st) => Some(st),
                        Err(_) => Some(PersistedLadderState {
                            episode_active: true,
                            attempts: u32::MAX,
                            last_action_at_s: None,
                            gave_up_at_s: None,
                        }),
                    },
                    Err(_) => None,
                };
                Some(match persisted {
                    Some(st) => HashrateRecoveryLadder::from_persisted(
                        publisher_recovery_config.clone(),
                        st,
                    ),
                    None => HashrateRecoveryLadder::new(publisher_recovery_config.clone()),
                })
            } else {
                None
            };
            let mut recovery_was_curtailed = false;
            let mut recovery_last_wake_uptime: Option<u64> = None;
            // Session energy meter (kWh) for the MQTT/HA `total_increasing`
            // energy sensor: integrates the SAME gated wall watts the stats
            // frame displays over real elapsed time between ticks.
            let mut energy_acc = dcentrald_api::websocket::EnergyAccumulator::new();
            let mut energy_last_tick = std::time::Instant::now();
            loop {
                tokio::select! {
                    _ = state_shutdown.cancelled() => {
                        info!("State publisher stopping");
                        break;
                    }
                    _ = interval.tick() => {
                        let uptime = start_time.elapsed().as_secs();

                        // Read current fan state
                        let (fan_pwm, fan_rpm) = publisher_fan
                            .as_ref()
                            .map(|f| (f.get_speed_pwm(), f.get_rpm()))
                            .unwrap_or((0, 0));
                        let fan_pwm = clamp_fan_pwm(fan_pwm);

                        let pub_per_fan: Vec<dcentrald_api::PerFanReading> = publisher_fan
                            .as_ref()
                            .map(|f| {
                                let pct = f.get_speed_percent();
                                f.get_per_fan_rpm().into_iter().map(|(id, rpm)| {
                                    dcentrald_api::PerFanReading { id, rpm, pwm_percent: pct }
                                }).collect()
                            })
                            .unwrap_or_default();

                        // Update uptime and fan in the current state
                        state_tx.send_modify(|state| {
                            state.uptime_s = uptime;
                            state.fans.pwm = fan_pwm;
                            state.fans.rpm = fan_rpm;
                            state.fans.per_fan = pub_per_fan;
                        });

                        // Broadcast stats via WebSocket (with live power data)
                        let state = state_tx.borrow().clone();
                        let power = publisher_power_rx.borrow().clone();
                        // Integrate the energy meter over REAL elapsed time.
                        // Cap one sample at 60 s so a stalled/suspended
                        // publisher can't fold hours of stale wattage into
                        // the monotonic total in a single tick.
                        let energy_now = std::time::Instant::now();
                        let energy_elapsed_s = energy_now
                            .duration_since(energy_last_tick)
                            .as_secs_f64()
                            .min(60.0);
                        energy_last_tick = energy_now;
                        let energy_kwh = energy_acc.add_sample(
                            dcentrald_api::websocket::energy_integration_watts(&power),
                            energy_elapsed_s,
                        );
                        let ws_msg = dcentrald_api::websocket::build_stats_message(
                            &state, &power, energy_kwh,
                        );
                        let _ = stats_broadcast_tx.send(ws_msg);

                        // C-4 (Omega P0-5): evaluate mining-health alerts off
                        // the same 1 Hz snapshot and fire any that are due.
                        // try_send is non-blocking so the publisher never
                        // stalls on a slow/full webhook consumer.
                        let health = crate::runtime::notifications::MiningHealthSnapshot {
                            mining_enabled: publisher_mining_enabled,
                            pool_status: state.pool.status.clone(),
                            pool_url: if state.pool.url.is_empty() {
                                publisher_pool_url.clone()
                            } else {
                                state.pool.url.clone()
                            },
                            total_hashrate_ghs: state.hashrate_ghs,
                            degraded_floor_ghs: {
                                // HLA-10 %-form: resolve rated nominal (GH/s)
                                // from the detected profile + live chips, mirroring
                                // the API's rated_nominal_ths, then let the %-form
                                // override the absolute floor when configured.
                                let nominal_ghs = publisher_profile
                                    .map(|p| {
                                        let live_chips: u64 =
                                            state.chains.iter().map(|c| c.chips as u64).sum();
                                        if live_chips > 0 {
                                            live_chips as f64
                                                * p.chip_hashrate_ghs(p.default_freq_mhz)
                                        } else {
                                            p.total_hashrate_ths(p.default_freq_mhz) * 1000.0
                                        }
                                    })
                                    .unwrap_or(0.0);
                                crate::runtime::notifications::effective_degraded_floor_ghs(
                                    publisher_degraded_pct,
                                    publisher_degraded_floor_ghs,
                                    nominal_ghs,
                                )
                            },
                            chains: state
                                .chains
                                .iter()
                                .map(|c| crate::runtime::notifications::ChainHealth {
                                    chain_id: c.id,
                                    chips: c.chips,
                                    hashrate_ghs: c.hashrate_ghs,
                                })
                                .collect(),
                        };
                        for event in alert_monitor.evaluate(&health, std::time::Instant::now()) {
                            let event_name = event.event_name();
                            if let Err(err) = publisher_alert_tx.try_send(event) {
                                tracing::debug!(
                                    event = event_name,
                                    error = %err,
                                    "mining-health alert dropped (alert channel full or closed)"
                                );
                            }
                        }

                        // PH-3: drive the auto-recovery ladder off the SAME
                        // snapshot. The FSM (dcentrald-api-types, host-tested)
                        // owns every safety gate; here we only feed real inputs
                        // and perform the gated side effect.
                        if let Some(ladder) = recovery_ladder.as_mut() {
                            use dcentrald_api_types::hashrate_recovery::{LadderOutcome, LadderTick};
                            let curtailed_now = publisher_recovery_sleeping
                                .load(std::sync::atomic::Ordering::Acquire);
                            if recovery_was_curtailed && !curtailed_now {
                                recovery_last_wake_uptime = Some(uptime);
                            }
                            recovery_was_curtailed = curtailed_now;
                            let observed_ghs = state.hashrate_ghs;
                            let floor_ghs = health.degraded_floor_ghs;
                            let tick = LadderTick {
                                now_s: uptime,
                                degraded_confirmed: alert_monitor.hashrate_degraded_confirmed(),
                                observed_ghs,
                                floor_ghs,
                                mining_enabled: publisher_mining_enabled,
                                curtailed: curtailed_now,
                                since_last_wake_s: recovery_last_wake_uptime
                                    .map(|w| uptime.saturating_sub(w)),
                                daemon_uptime_s: uptime,
                                platform_recovery_allowed: publisher_recovery_platform_allowed,
                                // Deferred: the dsPIC fw=0x86 / untrusted-EEPROM
                                // read via the single-owner I2C service. The
                                // am1-s9-only allowlist already keeps the ladder
                                // off every AM2 unit where this is the stop cond.
                                degraded_hardware: false,
                            };
                            match ladder.step(tick) {
                                LadderOutcome::ScheduleRestart { reason, attempt } => {
                                    // Persist the per-episode budget BEFORE
                                    // evaluating recovery so the per-episode
                                    // budget survives any future safe re-admission;
                                    // fail closed if persistence is not proven.
                                    match persist_recovery_ladder_state(
                                        &publisher_recovery_state_path,
                                        &ladder.persisted_state(),
                                    ) {
                                        Ok(outcome) => {
                                            tracing::warn!(
                                                attempt,
                                                reason,
                                                observed_ghs,
                                                floor_ghs,
                                                persisted_bytes = outcome.bytes_written,
                                                replaced_existing = outcome.replaced_existing,
                                                "PH-3 recovery: durable ladder budget published; evaluating guarded daemon restart"
                                            );
                                            if !crate::restart::schedule_daemon_restart(
                                                reason,
                                                Duration::from_secs(5),
                                            ) {
                                                tracing::error!(
                                                    "PH-3 recovery: automatic restart remains suspended pending typed hardware disposition"
                                                );
                                            }
                                        }
                                        Err(error) => {
                                            log_recovery_state_persist_failure(
                                                "schedule-restart",
                                                &error,
                                            );
                                            tracing::error!(
                                                "PH-3 auto-recovery: ladder budget durability was not proven — NOT restarting (fail-closed)"
                                            );
                                        }
                                    }
                                }
                                LadderOutcome::GiveUp { attempts } => {
                                    if let Err(error) = persist_recovery_ladder_state(
                                        &publisher_recovery_state_path,
                                        &ladder.persisted_state(),
                                    ) {
                                        log_recovery_state_persist_failure("give-up", &error);
                                    }
                                    let _ = publisher_alert_tx.try_send(
                                        crate::runtime::notifications::AlertEvent::HashrateRecoveryExhausted {
                                            observed_ghs,
                                            floor_ghs,
                                            attempts,
                                        },
                                    );
                                }
                                LadderOutcome::Idle(_) => {}
                            }
                        }
                    }
                }
            }
        });

        // Wait for shutdown signal
        self.shutdown_token.cancelled().await;

        if let Err(e) = history_buffer.save(&history_path) {
            warn!(error = %e, path = %history_path.display(), "Failed to persist history to disk");
        }

        // Graceful shutdown sequence
        self.shutdown().await?;

        Ok(())
    }

    /// Initialize all hardware and subsystems.
    ///
    /// Phases 1-7 from the architecture doc:
    /// 1. Mount /data, load config, open watchdog
    /// 2. GPIO and fan setup
    /// 3. Hash board detection
    /// 4. PIC initialization (per chain)
    /// 5. FPGA chain initialization
    /// 6. Chip detection and driver selection
    /// 7. Chip configuration
    async fn init(
        &mut self,
        identity: &crate::daemon_lifecycle::PlatformIdentitySnapshot,
    ) -> Result<()> {
        // Until this complete init generation reaches the explicit success
        // receipt below, shutdown must assume an inherited/hot rail may exist.
        // This is deliberately set before composition admission: a denial can
        // occur before controller discovery, leaving no endpoints on which to
        // issue disable commands. Such an empty command set is not safe-off.
        self.preflight_hardware_state_unknown = true;

        // Initialization is a composition boundary even when it later fails.
        // Revoke the preceding engine lease before touching discovery state so
        // management-only recovery can never retain an earlier dispatcher's
        // Measured/High identity authorization.
        if let Err(error) = self.dispatcher_composition_authority.invalidate_active() {
            warn!(
                error = %error,
                "Could not revoke the preceding dispatcher composition at initialization start"
            );
        }
        // An init retry is a new discovery and serialized-transport lifetime.
        // Stop the preceding heartbeat owner and terminal-close the old worker
        // before any address or generation-zero state can be reused. A detached
        // worker clone still observes the terminal latch, while the hardware
        // watchdog remains the independent cutoff if its current syscall never
        // returns.
        self.signal_init_heartbeat_stop();
        let had_previous_i2c_lifetime = self.i2c_service.is_some();
        if let Some(previous_i2c) = self.i2c_service.as_ref() {
            let previous_i2c_fw = match self.pic_firmware {
                PicFirmware::BraiinsOs => dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs,
                PicFirmware::Stock(_) => dcentrald_hal::i2c::I2cPicFirmware::Stock,
                PicFirmware::Unknown => dcentrald_hal::i2c::I2cPicFirmware::Unknown,
            };
            for &address in &self.initialized_pic_addrs_final {
                if (0x55..=0x57).contains(&address) {
                    if let Err(error) = previous_i2c.disable_voltage(address, previous_i2c_fw) {
                        warn!(
                            address = format_args!("0x{address:02X}"),
                            error = %error,
                            "Previous PIC16 rail SafeOff was not proven before init-lifetime revocation; refusing in-process transport replacement and relying on watchdog cutoff"
                        );
                    }
                } else {
                    warn!(
                        address = format_args!("0x{address:02X}"),
                        "Previous controller is not a PIC16 safe-off target; refusing in-process transport replacement and relying on protocol-specific teardown/watchdog cutoff"
                    );
                }
            }
            let transition = previous_i2c.latch_terminal_safe_off();
            info!(
                generation = transition.generation(),
                no_controller_mutation_stage_in_flight =
                    transition.no_controller_mutation_stage_in_flight(),
                "Revoked previous I2C service lifetime at initialization boundary"
            );
        }
        let _previous_init_heartbeat_joined = self.stop_init_heartbeat_bounded().await;

        // Never let successful receipts or address-only compatibility state
        // from an earlier, partially completed attempt survive this boundary.
        self.asic_enumeration_receipts.clear();
        self.initialized_pic_addrs_final.clear();
        self.pic_firmware = PicFirmware::Unknown;
        if had_previous_i2c_lifetime {
            anyhow::bail!(
                "previous serialized I2C service lifetime was revoked; in-process replacement is refused until worker shutdown/join can be proven (restart the daemon lifecycle after hardware watchdog reconciliation)"
            );
        }
        self.standard_composition_admission = None;
        self.asic_driver_execution_policy = ChipDriverExecutionPolicy::production_only();

        if let Some(refusal) = self.td003_destructive_write_refusal(identity) {
            anyhow::bail!(
                "TD-003 destructive-write gate refused hardware init for {} from {} \
                 (Experimental feature / In development; exact promotion gates incomplete)",
                refusal.model_name,
                refusal.source
            );
        }

        // I2C MIGRATION PLAN: PIC16 and dsPIC bring-up, heartbeat, voltage,
        // readback, and shutdown paths now use i2c_svc. AM2 must not open a
        // second raw /dev/i2c-0 owner after this service starts.
        //
        // ---- I2C Bus Recovery (BEFORE opening /dev/i2c-0) ----
        // PERMANENTLY scoped to AM1/S9 by design (NOT a temporary disable): the
        // unbind/SOFTR/rebind recovery only correctly fixes S9's stuck AXI IIC. On
        // am2 the same sequence poisons the APW PSU's framed-I2C session on shared
        // bus 0 (output disabled / stuck safe-mode voltage -> ASICs under-powered ->
        // zero nonces), so recovery is gated to the `is_am1_s9` allowlist below and
        // must NOT be re-enabled broadly..
        let control_board = identity.observed_control_board.as_str();
        // AUTHORITATIVE-FIRST detection: /etc/dcentos/board_target decides the
        // S9-only devmem I2C path, NOT detect_control_board()'s fragile
        // uio_count<=14 heuristic (which misclassifies a boot-race am2 as am1-s9
        // and would corrupt the am2 hashboard EEPROMs at 0x55-0x57). See
        // is_am1_s9_from_evidence.
        let board_target_for_i2c = identity.board_target();
        if board_target_for_i2c.is_empty() {
            self.preflight_hardware_state_unknown = true;
            self.signal_init_heartbeat_stop();
            self.heartbeat_shutdown_token.cancel();
            error!(
                detected_control_board = %control_board,
                passthrough = self.config.mining.passthrough,
                rail_state_unknown = true,
                "Authoritative /etc/dcentos/board_target is missing; refusing transport selection, endpoint keepalives, rail bring-up, and mining"
            );
            anyhow::bail!(
                "authoritative board_target identity is missing; management-only fallback required"
            );
        }
        let is_am1_s9 = is_am1_s9_from_evidence(board_target_for_i2c, control_board);

        // Lifecycle topology admission. This must complete before bus recovery,
        // transport startup, GPIO reads, endpoint keepalives, or UIO mapping.
        // A configured supported model may provide a profile. With no such
        // profile, only the exact authoritative am1-s9 board target is strong
        // enough to seed pre-enumeration wiring. Seeding S9 topology here does
        // NOT set `chip_id`; silicon identity remains unknown until enumeration.
        if let Some(configured_chip_id) = self.config.mining.model_chip_id() {
            self.miner_profile = Some(identified_miner_profile(configured_chip_id)?);
        } else {
            self.miner_profile = Some(pre_enumeration_topology_profile(is_am1_s9)?);
        }
        let admitted_profile = self.required_topology_profile()?;
        let admitted_pic_type = self
            .configured_model_pic_type_override()
            .unwrap_or(admitted_profile.pic_type);
        let admitted_pic_addrs = self
            .configured_model_pic_addrs_override()
            .unwrap_or(admitted_profile.pic_addrs);
        validate_profile_platform_authority(board_target_for_i2c, is_am1_s9, admitted_profile)?;
        validate_standard_daemon_topology(admitted_profile, admitted_pic_type, admitted_pic_addrs)?;

        // Executable maturity is not inferred from recognition or a product
        // name.  Experimental authority is an explicit, exact chip-ID opt-in;
        // an opt-in for one family cannot authorize another.  This policy is
        // then bound into the complete standard-route composition before any
        // new-generation I2C, fan, GPIO, reset, rail, or FPGA access.
        let experimental_config = crate::experimental::ExperimentalConfig::load();
        self.asic_driver_execution_policy = if experimental_config
            .executable_asic_chip_ids
            .contains(&admitted_profile.chip_id)
        {
            ChipDriverExecutionPolicy::with_experimental_chip(admitted_profile.chip_id)
        } else {
            ChipDriverExecutionPolicy::production_only()
        };
        let i2c_transport = standard_i2c_transport(identity, is_am1_s9)?;
        let registry = ChipRegistry::with_execution_policy(self.asic_driver_execution_policy);
        let composition_admission = admit_standard_hardware_composition(
            board_target_for_i2c,
            admitted_profile,
            admitted_pic_type,
            admitted_pic_addrs,
            i2c_transport,
            self.config.mining.passthrough,
            &registry,
        )?;
        info!(
            board_target = %board_target_for_i2c,
            profile = admitted_profile.name,
            profile_chip_id = format_args!("0x{:04X}", admitted_profile.chip_id),
            discovered_chip_id = format_args!("0x{:04X}", self.chip_id),
            source = if self.config.mining.model_chip_id().is_some() {
                "configured-supported-model"
            } else {
                "authoritative-am1-s9-topology-only"
            },
            driver_maturity = ?composition_admission.asic.recognition().maturity(),
            admitted_board_target = %composition_admission.board_target,
            ownership = ?composition_admission.ownership,
            i2c_transport = ?composition_admission.i2c_transport,
            pic_type = ?composition_admission.pic_type,
            pic_addrs = ?composition_admission.pic_addrs,
            "Complete standard hardware composition admitted before hardware access"
        );
        self.standard_composition_admission = Some(composition_admission);

        self.capture_bootstrap_i2c_observations()?;

        let preserve_passthrough_i2c =
            self.config.mining.passthrough && self.config.mining.model.is_none();
        let recover_am1_bus =
            self.config.mining.model.is_none() && !preserve_passthrough_i2c && is_am1_s9;
        if !recover_am1_bus {
            info!(
                passthrough = self.config.mining.passthrough,
                model_hint = self.config.mining.model.is_some(),
                control_board = %control_board,
                "Skipping I2C bus recovery — preserving live hardware/I2C state for passthrough handoff"
            );
        }

        // ---- v0.16.0: Spawn I2C service in DEVMEM mode ----
        // The kernel 4.4 xiic-i2c driver is broken on S9/am1: its error recovery
        // does SOFTR which zeros timing registers → PICs NACK → cascading failure
        // spiral. Devmem mode bypasses the kernel driver entirely and manages the
        // AXI IIC registers directly. This is the same path that achieved first
        // hash on S9 and the sustained-mining path on .39.
        //
        // CRITICAL (CE audit 06-ce.md #2, ):
        // the S9 devmem path — and specifically the HAL's kernel-driver unbind —
        // MUST NEVER run on am2. am2 shares I2C bus 0 with the APW PSU via
        // framed-I2C. Unbinding xiic-i2c + SOFTR'ing the controller kills the
        // PSU's framing session: output disabled or stuck at safe-mode voltage
        // → ASICs under-powered → zero nonces. The old `!starts_with("AML")`
        // gate let am2 (Zynq but not AML) through, which was the poisoning
        // vector. Switch to an explicit AM1/S9 allowlist: only boards we
        // positively identify as am1 take the unbind path.
        let use_devmem_i2c = i2c_transport == StandardI2cTransport::Am1S9Devmem;
        if use_devmem_i2c {
            info!(
                control_board = %control_board,
                recover_am1_bus,
                "AM1/S9 detected — serialized I2C factory will prepare the devmem AXI IIC path"
            );
        } else {
            info!(
                control_board = %control_board,
                "Skipping kernel I2C unbind — only AM1/S9 uses devmem I2C path (am2/Amlogic keep xiic-i2c bound)"
            );
        }
        // W3.1 (2026-05-07): am3-aml platforms (S21, S19j Pro Amlogic, S19K
        // Pro) get the same hashboard-EEPROM write-deny [0x50..=0x57] that
        // the am2 hybrid path registers. BHB56902 hashboards on S19K Pro
        // use a `0x05 0x11` EEPROM preamble (vs am2 BHB42xxx `0x04 0x11`);
        // both store factory identity and must be defended from misrouted
        // writes. and
        // dcentrald_hal::platform::amlogic doc comment.
        let i2c_request = SerializedI2cRequest::new(i2c_transport, recover_am1_bus)?;
        let i2c_svc = ProductionSerializedI2cFactory.open_serialized_i2c(i2c_request)?;
        self.i2c_service = Some(i2c_svc.clone());
        info!(
            use_devmem_i2c,
            "v0.16.0: I2C service spawned with platform-aware transport"
        );
        let i2c_fw_for = |firmware: PicFirmware| match firmware {
            PicFirmware::BraiinsOs => dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs,
            PicFirmware::Stock(_) => dcentrald_hal::i2c::I2cPicFirmware::Stock,
            PicFirmware::Unknown => dcentrald_hal::i2c::I2cPicFirmware::Unknown,
        };

        // ---- Early MinerProfile loading from config model hint ----
        // In passthrough mode, chip enumeration is skipped, so we need the profile
        // loaded from config to get the correct chain IDs, PIC addresses, etc.
        if let Some(chip_id) = self.config.mining.model_chip_id() {
            self.chip_id = chip_id;
            self.update_profile()?;
            if let Some(profile) = self.miner_profile {
                info!(
                    config_model = ?self.config.mining.model,
                    model = profile.name,
                    chip_id = format_args!("0x{:04X}", chip_id),
                    chips_per_chain = self.default_chips_per_chain()?,
                    chain_ids = ?profile.chain_ids,
                    pic_addrs = ?profile.pic_addrs,
                    "MinerProfile loaded from config model hint"
                );
            }
        }

        // ---- P0 cooling preflight (before any emergency heartbeat) ----
        // A passthrough unit may already be energized by prior firmware. Prove
        // fan-control ownership and tach motion before extending that state with
        // a heartbeat. PWM write success alone is not cooling evidence.
        info!("--- P0: Cooling Readiness Preflight ---");
        let spinup_pwm =
            clamp_fan_pwm(self.config.thermal.fan_max_pwm.max(20)).min(FAN_PWM_SAFETY_MAX);
        let cooling_readiness = match FanController::open_discovered() {
            Ok((discovery, fan)) => {
                fan.set_speed(spinup_pwm);
                let mut tach_rpm_by_physical_fan: Vec<Option<u32>> =
                    vec![None; discovery.variant.physical_fan_count() as usize];
                for sample_index in 0..COOLING_SPINUP_SAMPLES {
                    tokio::time::sleep(COOLING_SPINUP_SAMPLE_INTERVAL).await;
                    let snapshot = fan.get_tach_snapshot();
                    for (observed_max, sample_rpm) in tach_rpm_by_physical_fan
                        .iter_mut()
                        .zip(snapshot.rpm_by_physical_fan.iter())
                    {
                        if let Some(sample_rpm) = sample_rpm {
                            *observed_max = Some(observed_max.unwrap_or_default().max(*sample_rpm));
                        }
                    }
                    info!(
                        sample = sample_index + 1,
                        samples = COOLING_SPINUP_SAMPLES,
                        tach_rpm_by_physical_fan = ?snapshot.rpm_by_physical_fan,
                        "Cooling preflight tach sample collected"
                    );
                }
                let readiness = cooling_readiness_from_tach(spinup_pwm, &tach_rpm_by_physical_fan);
                info!(
                    uio_device = discovery.uio_number,
                    variant = ?discovery.variant,
                    commanded_pwm = spinup_pwm,
                    dwell_ms = COOLING_SPINUP_DWELL.as_millis(),
                    tach_rpm_by_physical_fan = ?tach_rpm_by_physical_fan,
                    readiness = ?readiness,
                    "Cooling preflight command/per-channel tach evidence collected; tach motion is not a physical-airflow-rate assertion"
                );
                if matches!(readiness, CoolingReadiness::Ready { .. }) {
                    self.fan = Some(Arc::new(fan));
                }
                readiness
            }
            Err(error) => CoolingReadiness::Unavailable {
                reason: error.to_string(),
            },
        };

        if let CoolingReadiness::Unavailable { reason } = &cooling_readiness {
            self.preflight_hardware_state_unknown = true;
            let transition = i2c_svc.latch_terminal_safe_off();
            self.signal_init_heartbeat_stop();
            self.heartbeat_shutdown_token.cancel();

            let board_target = identity.board_target();
            let psu_override_active = self
                .config
                .power
                .psu_override
                .as_ref()
                .is_some_and(|override_cfg| override_cfg.enabled);
            let hard_cut_allowed = legacy_kernel_smart_psu_path_allowed(
                self.config.mining.model.as_deref(),
                board_target,
                psu_override_active,
            );
            let hard_cut_asserted = hard_cut_allowed
                && match dcentrald_hal::platform::zynq::disable_psu_output() {
                    Ok(()) => true,
                    Err(error) => {
                        error!(
                            error = %error,
                            board_target,
                            "Cooling preflight failed and the proven AM2-S17 transport-independent hard cut also failed"
                        );
                        false
                    }
                };
            error!(
                error = %reason,
                passthrough = self.config.mining.passthrough,
                detected_slots_known = false,
                board_target,
                safety_generation = transition.generation(),
                hard_cut_allowed,
                hard_cut_asserted,
                rail_state_unknown = !hard_cut_asserted,
                controller_watchdog_state = "unknown; no new heartbeat was sent",
                "Cooling preflight failed closed before any heartbeat, board detection, rail enable, or mining; entering management-only fallback"
            );
            anyhow::bail!(
                "cooling preflight unavailable: fan-control/tach readiness not proven; management-only fallback required"
            );
        }

        let mode_desc = match self.config.mode.active.as_str() {
            "home" => "Home (low-power heat reuse with bitcoin mining as a bonus)",
            "hacker" => "Hacker (raw register access, overclock, full control)",
            _ => "Standard (balanced mining with auto-tuning)",
        };

        info!(
            hostname = %self.config.general.hostname,
            mode = %self.config.mode.active,
            "--- Phase 1: System Identification ---"
        );
        info!(
            "Operating mode: {} — {}",
            self.config.mode.active, mode_desc
        );

        // Read DCENTos version
        let version = std::fs::read_to_string("/etc/dcentos-version")
            .unwrap_or_else(|_| "unknown".to_string());
        info!(version = %version.trim(), "DCENTos firmware version (from /etc/dcentos-version)");

        // Read XADC die temperature for startup check
        // XADC is the Xilinx Analog-to-Digital Converter built into the Zynq SoC.
        // It gives us the control board die temperature without any external sensors.
        match Xadc::read_temp() {
            Ok(temp) => info!(
                die_temp_c = format_args!("{:.1}", temp),
                "Zynq SoC die temperature — this is the control board temp, not the ASIC chip temp (that comes from I2C sensors on each hash board)"
            ),
            Err(e) => tracing::debug!(error = %e, "XADC temperature sensor not available — this is normal on non-Zynq boards"),
        }

        // ---- Phase 1: Watchdog ----
        // The hardware watchdog (/dev/watchdog) is a Zynq peripheral that reboots
        // the system if it's not "kicked" periodically. This prevents bricked miners.
        if self.config.watchdog.enabled {
            // NEW-4 (2026-06-10 adversarial pass): do NOT open /dev/watchdog here.
            // The Zynq DTB sets the WDT `timeout-sec=10`. Opening it unkicked during
            // init Phase 1 reboots the SoC ~10s in — BEFORE the slow hardware bring-up
            // (PIC warm ~7s + enum + PLL + open-core; far longer on a cold reflash)
            // completes and BEFORE the kicker task arms (it only starts after init()
            // returns). That reboot-loops a perfectly healthy miner every boot and
            // defeats the 90s init-timeout management-only fallback. The watchdog is
            // now opened + set_timeout + kicked together just before the kicker task
            // (after init). During init, the daemon's own init-timeout/fallback guards.
            info!("Watchdog enabled — deferred: armed after hardware init completes (avoids the DTB 10s timeout rebooting mid-init)");
        } else {
            info!("Watchdog disabled in config — no auto-reboot protection");
        }

        // ---- Phase 2: Fan setup (home boot default) ----
        // The fan controller is the UIO device whose sysfs name is `fan-control`.
        // Braiins fan-control accepts PWM 0-100 and exposes tachometer feedback.
        // On AM2/XIL, live .25 evidence shows
        // the command register can be correct while the physical fan controller
        // still sits at a loud low-PWM/failsafe floor, so logs must report RPM.
        info!("--- Phase 2: Fan Initialization (Preflight Command Retained) ---");
        if let CoolingReadiness::Ready {
            commanded_pwm,
            tach_rpm_by_physical_fan,
        } = &cooling_readiness
        {
            info!(
                commanded_pwm,
                tach_rpm_by_physical_fan = ?tach_rpm_by_physical_fan,
                "Cooling readiness was established before any heartbeat or rail bring-up; retaining the proven home-capped command until thermal supervision assumes ownership"
            );
        }

        // Cold boot: run fans at configured max during ASIC initialization.
        // v0.16.0: Uses fan_max_pwm (e.g. 30 for home mining) instead of hardware max.
        // Home miners need quiet boot. The thermal PID ramps up if needed.
        if !self.config.mining.passthrough {
            if let Some(ref fan) = self.fan {
                let boot_max =
                    clamp_fan_pwm(self.config.thermal.fan_max_pwm.max(20)).min(FAN_PWM_SAFETY_MAX);
                fan.set_speed(boot_max);
                let rpm = fan.get_rpm();
                info!(
                    commanded_pwm = boot_max,
                    max_tach_rpm = rpm,
                    tach_motion_observed = rpm > 0,
                    "Cold-boot fan command issued for init sequence; tach is observational and does not prove physical airflow"
                );
            }
        }

        // ---- Phase 2b: GPIO controller (direct register access) ----
        // Initialize the AXI GPIO controller via /dev/mem mmap for direct
        // register access. This bypasses sysfs and gives us reliable control
        // over board enable/reset, plug detect, and LEDs.
        info!("--- Phase 2b: GPIO Controller Init (AXI Register Access) ---");
        match GpioController::new() {
            Ok(gpio) => {
                let input_val = gpio.read_input();
                let output_val = gpio.read_output();
                info!(
                    input_reg = format_args!("0x{:08X}", input_val),
                    output_reg = format_args!("0x{:08X}", output_val),
                    "GPIO controller initialized via /dev/mem — direct AXI register access active"
                );
                // Take manual control of LEDs (set sysfs triggers to "none")
                gpio.init_leds();
                let gpio = Arc::new(gpio);
                // Set green LED on to show we're alive
                gpio.set_led(dcentrald_hal::gpio::Led::Green, true);

                // Spawn the LED engine task
                let led_config = LedEngineConfig {
                    enabled: self.config.led.enabled,
                    heartbeat_on_ms: self.config.led.heartbeat_on_ms,
                    heartbeat_off_ms: self.config.led.heartbeat_off_ms,
                    locate_pattern: self.config.led.locate_pattern.clone(),
                    locate_duration_s: self.config.led.locate_duration_s,
                    flash_on_accepted_share: self.config.led.flash_on_accepted_share,
                    flash_on_rejected_share: self.config.led.flash_on_rejected_share,
                    night_mode_disable: self.config.led.night_mode_disable,
                    celebration_on_lucky_share: self.config.led.celebration_on_lucky_share,
                    chain_status_blink_codes: self.config.led.chain_status_blink_codes,
                };
                let (led_cmd_tx, led_cmd_rx) = mpsc::channel::<LedCommand>(64);
                let led_gpio = gpio.clone();
                let led_shutdown = self.shutdown_token.clone();
                let (mut engine, led_status_rx) =
                    LedEngine::new(led_gpio, led_cmd_rx, led_shutdown, led_config);
                tokio::spawn(async move {
                    engine.run().await;
                });
                let _ = led_cmd_tx.try_send(LedCommand::SetPattern(LedPattern::Initializing));
                self.led_tx = Some(led_cmd_tx);
                self.led_status_rx = Some(led_status_rx);

                self.gpio = Some(gpio);
            }
            Err(e) => {
                warn!(error = %e, "GPIO controller init failed — falling back to sysfs GPIO (less reliable)");
            }
        }

        // ---- Phase 3: Hash board detection ----
        // Each hash board connector has a "PLUGO" pin that goes HIGH when a board
        // is physically plugged in. We read 3 GPIO pins to see which slots have boards.
        // S9 has 3 slots (chain 6, 7, 8) but you can run with 1, 2, or 3 boards.
        info!("--- Phase 3: Hash Board Detection ---");
        let plugo_base = self.plugo_gpio_base()?;
        info!("Checking PLUGO GPIO pins {} to {} — each pin tells us if a hash board is plugged into that slot", plugo_base, plugo_base + 2);

        self.detected_board_indices.clear();
        let direct_plug_detect = if is_am1_s9 {
            self.gpio.as_ref().map(|gpio| gpio.read_plug_detect())
        } else {
            None
        };
        let mut slot_presence = [BoardPresence::Unknown; 3];

        for i in 0..3 {
            let gpio_num = plugo_base + i as u32;
            let gpio_path = format!("/sys/class/gpio/gpio{}/value", gpio_num);
            let (presence, source) = if let Some(direct) = direct_plug_detect.as_ref() {
                (
                    if direct[i] {
                        BoardPresence::Present
                    } else {
                        BoardPresence::Absent
                    },
                    "direct-axi-gpio",
                )
            } else {
                let value = std::fs::read_to_string(&gpio_path);
                let presence = board_presence_from_sysfs_value(value.as_deref().ok());
                if let Err(error) = &value {
                    warn!(
                        chain_id = self.chain_id_for_board(i)?,
                        gpio = gpio_num,
                        error = %error,
                        "PLUGO GPIO read failed; slot presence is unknown and the slot will not be energized"
                    );
                } else if presence == BoardPresence::Unknown {
                    warn!(
                        chain_id = self.chain_id_for_board(i)?,
                        gpio = gpio_num,
                        value = %value.as_deref().unwrap_or_default().trim(),
                        "PLUGO GPIO value is malformed; slot presence is unknown and the slot will not be energized"
                    );
                }
                (presence, "sysfs")
            };

            slot_presence[i] = presence;
            match presence {
                BoardPresence::Present => {
                    self.detected_board_indices.push(i);
                    info!(
                        chain_id = self.chain_id_for_board(i)?,
                        gpio = gpio_num,
                        source,
                        connector = format_args!("J{}", self.chain_id_for_board(i)?),
                        "Hash board positively detected; slot is eligible for hardware bring-up"
                    );
                }
                BoardPresence::Absent => info!(
                    chain_id = self.chain_id_for_board(i)?,
                    gpio = gpio_num,
                    source,
                    connector = format_args!("J{}", self.chain_id_for_board(i)?),
                    "Hash board slot is authoritatively absent; slot will be skipped"
                ),
                BoardPresence::Unknown => warn!(
                    chain_id = self.chain_id_for_board(i)?,
                    gpio = gpio_num,
                    source,
                    connector = format_args!("J{}", self.chain_id_for_board(i)?),
                    "Hash board slot presence is unknown; fail-closed policy forbids energizing it"
                ),
            }
        }

        if !hardware_bringup_permitted(&cooling_readiness, &slot_presence) {
            self.preflight_hardware_state_unknown = true;
            let transition = i2c_svc.latch_terminal_safe_off();
            self.signal_init_heartbeat_stop();
            self.heartbeat_shutdown_token.cancel();

            let board_target = identity.board_target();
            let psu_override_active = self
                .config
                .power
                .psu_override
                .as_ref()
                .is_some_and(|override_cfg| override_cfg.enabled);
            let hard_cut_allowed = legacy_kernel_smart_psu_path_allowed(
                self.config.mining.model.as_deref(),
                board_target,
                psu_override_active,
            );
            let hard_cut_asserted = hard_cut_allowed
                && match dcentrald_hal::platform::zynq::disable_psu_output() {
                    Ok(()) => true,
                    Err(error) => {
                        error!(
                            error = %error,
                            board_target,
                            "Board-presence preflight denied bring-up and the proven AM2-S17 transport-independent hard cut also failed"
                        );
                        false
                    }
                };
            error!(
                slot_presence = ?slot_presence,
                positively_present_slots = self.detected_board_indices.len(),
                passthrough = self.config.mining.passthrough,
                board_target,
                safety_generation = transition.generation(),
                hard_cut_allowed,
                hard_cut_asserted,
                rail_state_unknown = !hard_cut_asserted,
                controller_watchdog_state = "no endpoint keepalive was sent; expiry timing and prior rail state are not proven",
                "No positively present hash-board slot is eligible for bring-up; entering management-only fallback"
            );
            anyhow::bail!(
                "hash-board presence not positively established for any slot; management-only fallback required"
            );
        }

        // ---- Phase 0: NON-MUTATING PIC16 TOPOLOGY CAPABILITIES ----
        // This runs only after cooling and positive per-slot presence evidence.
        // Only authoritative AM1/S9 board identity admits its canonical endpoint
        // table. Config/model hints cannot mint early mutation authority. This
        // phase performs no PIC I2C traffic so cold-boot admission retains the
        // original raw-state evidence for its atomic exact-0xCC decision.
        info!("--- Phase 0: Non-Mutating PIC16 Topology Capabilities (post-presence) ---");
        let mut capability_success = 0u8;
        let mut capability_endpoints = Vec::new();
        let mut pic16_endpoint_sessions = Vec::new();
        if is_am1_s9 {
            for &board_index in &self.detected_board_indices {
                let Some(&addr) = DEFAULT_PIC_ADDRS.get(board_index) else {
                    warn!(
                        board_index,
                        "Positive S9 slot has no canonical PIC address; no keepalive sent to that slot"
                    );
                    continue;
                };
                capability_endpoints.push((board_index, addr));
                match dcentrald_hal::platform::discover_system_pic16_endpoint(&i2c_svc, addr) {
                    Ok(endpoint) => match Pic16EndpointSession::new(i2c_svc.clone(), endpoint) {
                        Ok(session) => {
                            capability_success += 1;
                            pic16_endpoint_sessions.push(session);
                        }
                        Err(error) => warn!(
                            board_index,
                            i2c_addr = format_args!("0x{:02X}", addr),
                            error = %error,
                            "PIC16 topology capability was issued but session binding failed; address remains unauthorized"
                        ),
                    },
                    Err(error) => warn!(
                        board_index,
                        i2c_addr = format_args!("0x{:02X}", addr),
                        error = %error,
                        "PIC16 topology capability was not established; later init will not use a raw-address fallback"
                    ),
                }
            }
        } else {
            info!(
                board_target = %board_target_for_i2c,
                "Platform identity does not independently prove AM1/S9 PIC16 endpoints; relying on controller watchdog until typed controller initialization"
            );
        }
        info!(
            success_count = capability_success,
            endpoints = ?capability_endpoints,
            "Non-mutating post-presence PIC16 capability issuance complete"
        );

        info!(
            boards = self.detected_board_indices.len(),
            "Found {} hash board(s) — enabling power via GPIO",
            self.detected_board_indices.len(),
        );

        // ---- Phase 3b: Release hash board RESET (GPIO HIGH) ----
        // On passthrough (hot start), boards are already powered and running,
        // so releasing RESET is safe and expected.
        //
        // v0.8.4.2 FIX: On cold boot (passthrough=false), do NOT release RESET here.
        // Releasing GPIO HIGH before voltage is enabled causes ASICs to attempt
        // partial boot with no power, creating a HIGH-LOW-HIGH glitch that leaves
        // chips in an undefined state. Phase 5.2 will assert LOW (proper reset)
        // and Phase 5.5 will release HIGH (after voltage is stable).
        //
        // CRITICAL: PICs do NOT respond on I2C when RESET GPIO is LOW on this S9 unit.
        // The GPIO gates I2C bus access to the hash board (not just ASIC reset).
        // v0.14.0 fix: release GPIO HIGH before PIC init, assert LOW only for ASIC reset.
        if !self.config.mining.passthrough {
            info!("--- Phase 3b: SKIPPING GPIO RESET release (cold boot — Phase 5 handles reset sequence) ---");
        } else {
            info!("--- Phase 3b: Release Hash Board RESET (GPIO HIGH — passthrough mode) ---");

            for &idx in &self.detected_board_indices {
                let gpio_num = self.enable_gpio_base()? + idx as u32;
                let dir_path = format!("/sys/class/gpio/gpio{}/direction", gpio_num);
                let val_path = format!("/sys/class/gpio/gpio{}/value", gpio_num);
                let _ = std::fs::write(&dir_path, "out");
                let _ = std::fs::write(&val_path, "1");

                // AXI GPIO: only on S9 (am1). am2-s17 uses different GPIO address (0x41220000).
                // Sysfs GPIO write above handles all platforms correctly via kernel driver.
                if self.config.mining.model.is_none() {
                    if let Some(ref gpio) = self.gpio {
                        gpio.exit_reset(idx as u8);
                    }
                }

                info!(
                    chain_id = self.chain_id_for_board(idx)?,
                    gpio = gpio_num,
                    "RESET released on chain {} (GPIO {} = HIGH)",
                    self.chain_id_for_board(idx)?,
                    gpio_num,
                );
            }

            info!("Waiting 500ms for hash board state to settle...");
            tokio::time::sleep(Duration::from_millis(500)).await;
        } // end of passthrough Phase 3b else block

        // ---- Phase 4: FPGA chain setup (moved BEFORE PIC — matches asic_comm_test.c) ----
        // The FPGA must be initialized before any ASIC communication. The C test tool
        // does this first: disable core → reset FIFOs → set baud → set work_time →
        // clear errors → enable IRQ → enable core.
        info!("--- Phase 4: FPGA Chain Setup (Memory-Mapped UIO) ---");
        info!("Opening FPGA register blocks via UIO — 4 memory-mapped regions per chain");

        for &idx in &self.detected_board_indices {
            let chain_id = self.chain_id_for_board(idx)?;
            let uio_base = self.uio_base_for_board(idx)?;

            info!(
                chain_id,
                uio_devices = format_args!("uio{}-uio{}", uio_base, uio_base + 3),
                "Opening FPGA chain {} — mapping 4 x 4KB register blocks",
                chain_id,
            );

            match FpgaChain::open(chain_id, uio_base) {
                Ok(fpga) => {
                    let version = fpga.read_version();
                    let build_id = fpga.read_build_id();
                    let version_ok = version == 0x00901002;
                    let status = if version_ok {
                        "OK (s9io v1.0.2)"
                    } else {
                        "UNEXPECTED"
                    };

                    info!(
                        chain_id,
                        fpga_version = format_args!("0x{:08X}", version),
                        build_id = format_args!("0x{:08X}", build_id),
                        status,
                        "FPGA chain {} operational — version 0x{:08X} ({})",
                        chain_id,
                        version,
                        status,
                    );

                    if !version_ok {
                        warn!(
                            chain_id,
                            expected = format_args!("0x{:08X}", 0x00901002u32),
                            actual = format_args!("0x{:08X}", version),
                            "FPGA version mismatch — expected s9io v1.0.2"
                        );
                    }

                    let mut chain = Chain::new(fpga, chain_id);
                    chain.pic_type = self.pic_type()?;
                    chain.pic_address = self.pic_addrs()?.get(idx).copied();

                    // Hybrid mode: open serial command channel for am2-s17 platforms.
                    // On S19 Pro and similar, ASIC commands (GetAddress, register writes)
                    // go through PL UARTs (/dev/ttyS1-3), NOT the FPGA CMD FIFO.
                    // Work dispatch still uses FPGA WORK_TX/RX FIFOs.
                    //
                    // am2-s17 models (S17/S19/S19j/S19XP) use chain_ids 1-3 and need
                    // serial command channels. S9 (am1-s9, chain_ids 6-8) uses FPGA CMD
                    // FIFO for everything. Only open serial for am2 models.
                    // NOTE: Do NOT open serial channels on am2. Confirmed 2026-04-10:
                    // (1) Bosminer uses FPGA CMD FIFO (not serial) — verified by BAUD_REG=0x6C and UIO fds
                    // (2) PL UART TX does NOT reach hash boards — devmem test got zero response
                    // (3) DevmemUart::open() unbinds kernel serial driver which CORRUPTS the I2C bus
                    //     (PSU I2C fails with "no ACK" after serial unbind — requires power cycle)
                    // Use FPGA CMD path only (same as S9, with BM1397+ GetAddress header 0x52).

                    self.chains.push(chain);
                }
                Err(e) => {
                    error!(
                        chain_id,
                        uio_base,
                        error = %e,
                        "FPGA chain {} open FAILED — this chain won't mine",
                        chain_id,
                    );
                }
            }
        }

        if self.chains.is_empty() {
            warn!("No FPGA chains could be opened — no mining possible");
            return Ok(());
        }

        // ---- Phase 4b: FPGA chain enable & baud rate ----
        // Reconfigure FPGA chains WITHOUT disabling them.
        //
        // BUG FIX (2026-03-12): Writing 0 to CTRL_REG (set_enabled(false)) permanently
        // breaks the FPGA UART state machine. After disable+re-enable, the UART can
        // transmit (TX_EMPTY toggles) but ASICs never respond (RX_EMPTY stays 1).
        // This was proven by A/B testing on live S9 hardware:
        //   Test A: Keep chain enabled, reset FIFOs, send GetAddress -> chips respond
        //   Test B: Disable chain, re-enable, reset FIFOs, send GetAddress -> silence
        //
        // The fix: use reconfigure() which resets FIFOs, sets baud, sets WORK_TIME,
        // clears errors, and writes CTRL_REG -- all while keeping the chain ENABLED.
        // This matches bosminer's behavior: it never writes 0 to CTRL_REG.
        // === DIAGNOSTIC: ZERO-BAUD-CHANGE HOT START ===
        // Phase 4b/4c REPLACED: Don't touch FPGA baud at all. Don't enumerate.
        // Just read current state, assume 63 chips per chain, reset WORK FIFOs only.
        // This preserves bosminer's exact FPGA+ASIC baud match (1.5M).
        info!("--- Phase 4b: ZERO-CHANGE hot start (preserving inherited live FPGA state) ---");
        let ctrl_value = dcentrald_hal::fpga_chain::CTRL_ENABLE
            | (2 << dcentrald_hal::fpga_chain::CTRL_MIDSTATE_SHIFT);

        let mut hot_chain_indices: Vec<usize> = Vec::new();
        let mut cold_chain_indices: Vec<usize> = Vec::new();

        // v0.17.2: If passthrough=false, force cold boot on ALL chains (both S9 and am2).
        // Cold boot does: BREAK (4s) → ASIC baud reset to 115200 → full init_chain.
        // Previously am2 was excluded (wrong assumption that CMD_TX can't work).
        // Proven: CMD_TX DOES work at 115200 baud on am2 (PLL readback succeeded in
        // the 568K nonce test). It only fails at BAUD=0x00 (special fast mode).
        // The cold boot path sets BAUD=0x6C (115200) first, making CMD work.
        if !self.config.mining.passthrough {
            info!(
                "passthrough=false: FORCING cold boot on all {} chains (full init_chain)",
                self.chains.len()
            );
            for chain_idx in 0..self.chains.len() {
                cold_chain_indices.push(chain_idx);
            }
        }

        if self.config.mining.passthrough {
            passthrough_miner_profile(self.chip_id)?;
        }
        let assumed_chip_id = self.chip_id;
        let configured_chip_count_hint = self.configured_model_chip_count_hint();
        let assumed_chips = configured_chip_count_hint
            .or_else(|| MinerProfile::for_chip(assumed_chip_id).map(|p| p.chips_per_chain))
            .unwrap_or(0);

        for (chain_idx, chain) in self.chains.iter_mut().enumerate() {
            let ctrl = chain
                .fpga
                .common
                .read_reg(dcentrald_hal::fpga_chain::REG_CTRL);
            let baud = chain
                .fpga
                .common
                .read_reg(dcentrald_hal::fpga_chain::REG_BAUD);
            let wtime = chain
                .fpga
                .common
                .read_reg(dcentrald_hal::fpga_chain::REG_WORK_TIME);
            let err = chain.fpga.read_error_count();
            let baud_hz = dcentrald_hal::fpga_chain::FpgaChain::baud_from_divisor(baud);

            info!(
                chain_id = chain.chain_id,
                ctrl = format_args!("0x{:08X}", ctrl),
                baud = format_args!("0x{:02X} ({} Hz)", baud, baud_hz),
                work_time = format_args!("0x{:08X}", wtime),
                errors = err,
                "Inherited live FPGA state — CTRL=0x{:08X}, BAUD={} Hz, WORK_TIME=0x{:08X}, ERRORS={}",
                ctrl, baud_hz, wtime, err,
            );

            // Read MIDSTATE_CNT from FPGA CTRL register (needed by both paths).
            let fpga_midstate_cnt = (ctrl >> dcentrald_hal::fpga_chain::CTRL_MIDSTATE_SHIFT) & 0x03;
            chain.fpga_midstate_cnt = fpga_midstate_cnt as u8;

            // Passthrough has already proven a supported model above. Cold boot may
            // remain unidentified here; enumeration must supply its identity before
            // any model-specific driver is selected.
            chain.chip_count = assumed_chips;
            chain.chip_id = assumed_chip_id;

            if self.config.mining.passthrough && assumed_chip_id != 0 && assumed_chips != 0 {
                chain
                    .admit_address_assignment_for_current_identity()
                    .with_context(|| {
                        format!(
                            "hot-start chain {} lacks an admitted ASIC/board composition",
                            chain.chain_id
                        )
                    })?;
            }

            // AM2 model detection: cold boot vs passthrough.
            //
            // v0.19.1: passthrough=true IS supported on am2 (S19 Pro) when bosminer
            // has already initialized the hash chains. The previous "9 passthrough tests
            // all zero nonces" failure was caused by sending 36-word work items into a
            // 68-word FIFO slot (MIDSTATE_CNT mismatch). Now fixed: send_work() reads
            // runtime MIDSTATE_CNT from FPGA CTRL and builds correct packet size.
            //
            // passthrough=false: FULL COLD BOOT (BREAK + init_chain)
            // passthrough=true:  HOT START (preserve bosminer's CTRL/BAUD/WORK_TIME)
            if self.config.mining.model.is_some() && !self.config.mining.passthrough {
                info!(
                    chain_id = chain.chain_id,
                    "AM2 FULL COLD BOOT: pushing chain to cold boot path (BREAK + init_chain)",
                );
                if !cold_chain_indices.contains(&chain_idx) {
                    cold_chain_indices.push(chain_idx);
                }
                continue;
            }

            // Skip chains that were intentionally disabled by bosminer.
            // Signature: ENABLE bit clear AND idle baud (0x6C = 115200).
            // On S19 Pro, bosminer disables chain 2 (no board or bad board).
            // Don't re-enable, don't dispatch work, don't reset FIFOs.
            if (ctrl & dcentrald_hal::fpga_chain::CTRL_ENABLE) == 0 && baud == 0x6C {
                info!(
                    chain_id = chain.chain_id,
                    ctrl = format_args!("0x{:02X}", ctrl),
                    baud = format_args!("0x{:02X}", baud),
                        "DISABLED chain (not enabled by the pre-existing runtime, idle baud) — SKIPPING",
                );
                continue;
            }

            // CRITICAL: Re-enable FPGA chain. Bosminer clears ENABLE bit on exit
            // (CTRL goes from 0x1E to 0x16). Without re-enable, work sits in FIFO
            // but never gets serialized to hash board UART → TX_FULL, zero nonces.
            let current_ctrl = chain
                .fpga
                .common
                .read_reg(dcentrald_hal::fpga_chain::REG_CTRL);
            let enabled_ctrl = current_ctrl | dcentrald_hal::fpga_chain::CTRL_ENABLE;
            if current_ctrl != enabled_ctrl {
                chain
                    .fpga
                    .common
                    .write_reg(dcentrald_hal::fpga_chain::REG_CTRL, enabled_ctrl);
                info!(
                    chain_id = chain.chain_id,
                    old_ctrl = format_args!("0x{:02X}", current_ctrl),
                    new_ctrl = format_args!("0x{:02X}", enabled_ctrl),
                    "Hot start: re-enabled FPGA chain (legacy runtime left ENABLE cleared)",
                );
            }

            // Diagnostic: read WORK_RX_STAT before any FIFO reset to check for
            // residual nonces from bosminer's pipeline. This is critical for the
            // hypothesis test: "is WORK_RX physically connected to hash boards?"
            let rx_stat_before = chain
                .fpga
                .work_rx
                .read_reg(dcentrald_hal::fpga_chain::REG_WORK_RX_STAT);
            let tx_stat_before = chain
                .fpga
                .work_tx
                .read_reg(dcentrald_hal::fpga_chain::REG_WORK_TX_STAT);
            let rx_empty = rx_stat_before & dcentrald_hal::fpga_chain::STAT_RX_EMPTY != 0;
            info!(
                chain_id = chain.chain_id,
                rx_stat = format_args!("0x{:08X}", rx_stat_before),
                tx_stat = format_args!("0x{:08X}", tx_stat_before),
                rx_empty,
                "Hot start PRE-RESET: WORK_RX_STAT=0x{:08X} (empty={}), WORK_TX_STAT=0x{:08X}",
                rx_stat_before,
                rx_empty,
                tx_stat_before,
            );

            // If skip_fifo_reset is set, read residual nonces without resetting.
            // This is a diagnostic mode: after SIGKILL of mining bosminer, any
            // nonces in WORK_RX prove the FIFO is physically connected to hash boards.
            if self.config.mining.skip_fifo_reset {
                let mut residual_count = 0u32;
                while chain.fpga.work_rx_has_data() && residual_count < 100 {
                    let w0 = chain
                        .fpga
                        .work_rx
                        .read_reg(dcentrald_hal::fpga_chain::REG_WORK_RX_FIFO);
                    let w1 = chain
                        .fpga
                        .work_rx
                        .read_reg(dcentrald_hal::fpga_chain::REG_WORK_RX_FIFO);
                    info!(
                        chain_id = chain.chain_id,
                        nonce = format_args!("0x{:08X}", w0),
                        solution = format_args!("0x{:08X}", w1),
                        "RESIDUAL NONCE #{}: nonce=0x{:08X}, solution=0x{:08X}",
                        residual_count + 1,
                        w0,
                        w1,
                    );
                    residual_count += 1;
                }
                info!(
                    chain_id = chain.chain_id,
                    residual_count,
                    "skip_fifo_reset: found {} residual nonces (proves WORK_RX {} connected)",
                    residual_count,
                    if residual_count > 0 {
                        "IS"
                    } else {
                        "may NOT be"
                    },
                );
                // Don't reset FIFOs — preserve state for continued observation
                info!(
                    chain_id = chain.chain_id,
                    "skip_fifo_reset: SKIPPING FIFO reset + THR write"
                );
            } else {
                // Normal operation: reset FIFOs for clean start.
                // TX FIFO: clears bosminer's stale work items
                // RX FIFO: clears bosminer's unread nonces
                // CMD FIFO: NOT reset (preserves ASIC register state)
                // BAUD, WORK_TIME: NOT touched (preserved from bosminer)

                // Set WORK_TX threshold BEFORE FIFO reset (matches BraiinsOS init order).
                // Without this, the FPGA work scheduler doesn't dispatch — TX fills to FULL
                // but work items never reach the ASIC UART. THR=1848 = FIFO_SIZE(2048) - 200.
                chain
                    .fpga
                    .work_tx
                    .write_reg(dcentrald_hal::fpga_chain::REG_WORK_TX_THR, 1848);
                chain
                    .fpga
                    .work_tx
                    .write_reg(dcentrald_hal::fpga_chain::REG_WORK_TX_CTRL, 0x02);
                std::thread::sleep(std::time::Duration::from_millis(2));
                chain.fpga.work_tx.write_reg(
                    dcentrald_hal::fpga_chain::REG_WORK_TX_CTRL,
                    dcentrald_hal::fpga_chain::CMD_CTRL_IRQ_EN,
                );
                chain
                    .fpga
                    .work_rx
                    .write_reg(dcentrald_hal::fpga_chain::REG_WORK_RX_CTRL, 0x01);
                std::thread::sleep(std::time::Duration::from_millis(2));
                chain.fpga.work_rx.write_reg(
                    dcentrald_hal::fpga_chain::REG_WORK_RX_CTRL,
                    dcentrald_hal::fpga_chain::CMD_CTRL_IRQ_EN,
                );
            }

            // CRITICAL: Sanitize WORK_TIME. If bosminer was killed during init
            // (before mining started), WORK_TIME may be 0xFFFFFFFF (43s per item).
            // Replace with a sane value for the target frequency.
            if wtime == 0xFFFFFFFF || wtime == 0 {
                let sane_wtime = dcentrald_asic::drivers::bm1398::Bm1398Driver::calculate_work_time(
                    self.config.mining.frequency_mhz,
                    1u32 << fpga_midstate_cnt, // 4 or 8 midstates
                );
                chain
                    .fpga
                    .common
                    .write_reg(dcentrald_hal::fpga_chain::REG_WORK_TIME, sane_wtime);
                info!(
                    chain_id = chain.chain_id,
                    old_wtime = format_args!("0x{:08X}", wtime),
                    new_wtime = format_args!("0x{:08X}", sane_wtime),
                    freq_mhz = self.config.mining.frequency_mhz,
                    "Hot start: sanitized stale WORK_TIME (was 0x{:08X}, set to 0x{:08X} for {} MHz)",
                    wtime, sane_wtime, self.config.mining.frequency_mhz,
                );
            }

            // BM1397/BM1398 relay programming is board-composition state, not
            // a chip-family constant. The old generic register-0x34 broadcast
            // was contradicted by both local runtime evidence and the stock
            // NBP1901 sequence (twelve addressed register-0x2c writes). Keep
            // passthrough state untouched and refuse a cold transition until
            // the exact board composition owns an evidence-backed recipe.
            if (assumed_chip_id == 0x1398 || assumed_chip_id == 0x1397)
                && !self.config.mining.passthrough
            {
                anyhow::bail!(
                    "chain {}: refusing generic BM1397/BM1398 cold relay transition; exact board-composition relay recipe is required",
                    chain.chain_id
                );
            } else if (assumed_chip_id == 0x1398 || assumed_chip_id == 0x1397)
                && self.config.mining.passthrough
            {
                info!(
                    chain_id = chain.chain_id,
                    "Passthrough: retaining externally established board relay state without chip-generic writes",
                );
            }

            info!(
                chain_id = chain.chain_id,
                fpga_midstate_cnt,
                "Hot start: FPGA MIDSTATE_CNT={} — work_id shift={}",
                fpga_midstate_cnt,
                fpga_midstate_cnt,
            );
            // Only push to hot if not already forced to cold boot
            if !cold_chain_indices.contains(&chain_idx) {
                hot_chain_indices.push(chain_idx);
            }

            info!(
                chain_id = chain.chain_id,
                assumed_chip_id = format_args!("0x{:04X}", assumed_chip_id),
                assumed_chips,
                "Hot start: {} chips (ChipID 0x{:04X}), FIFOs reset, relay state retained, baud {} Hz",
                assumed_chips,
                assumed_chip_id,
                baud_hz,
            );
        }

        // A hot-start chain is only admitted with the explicit supported profile
        // proven above. Cold boot intentionally remains profile-free until ASIC
        // enumeration reports an identity.
        if !hot_chain_indices.is_empty() && self.miner_profile.is_none() {
            self.update_profile()?;
        }

        // Skip Phase 4c enumeration entirely
        info!(
            "=== HOT START: {} chain(s) assumed alive (no enumeration, no baud change) ===",
            hot_chain_indices.len()
        );

        // Old Phase 4c enumeration code skipped — replaced by zero-change hot start above

        let mut hot_controller_admitted_chain_indices: Vec<usize> = Vec::new();
        if !hot_chain_indices.is_empty() {
            info!(
                hot = hot_chain_indices.len(),
                cold = cold_chain_indices.len(),
                "=== HOT START: {} chain(s) alive, {} need cold boot ===",
                hot_chain_indices.len(),
                cold_chain_indices.len(),
            );

            // Start PIC heartbeats immediately (PICs are running from previous firmware,
            // their watchdog is ticking — we have ~5s before they timeout)
            //
            // BUG FIX (2026-03-15): Do NOT call detect_firmware() during hot start.
            // detect_firmware() uses I2C_RDWR ioctl which permanently corrupts the
            // Xilinx AXI IIC controller when a PIC doesn't respond (SR=0xC0 stuck).
            // This kills ALL I2C communication to ALL PICs on the bus — even healthy
            // ones. The corruption persists until SoC power cycle.
            //
            // All PIC16F1704 app-mode firmwares use heartbeat cmd 0x16.
            info!("Hot start: sending PIC16 heartbeats via kernel I2C to preserve the live handoff state.");
            {
                for &chain_idx in &hot_chain_indices {
                    let idx = self.board_idx_for_chain(self.chains[chain_idx].chain_id)?;
                    let addr = self.pic_addr_for_board(idx)?;

                    if i2c_svc
                        .heartbeat(addr, dcentrald_hal::i2c::I2cPicFirmware::Unknown)
                        .is_ok()
                    {
                        info!(
                            chain_id = self.chain_id_for_board(idx)?,
                            pic_addr = format_args!("0x{:02X}", addr),
                            "PIC heartbeat sent (write-only)",
                        );
                        if !self.initialized_pic_addrs_final.contains(&addr) {
                            self.initialized_pic_addrs_final.push(addr);
                        }
                        if !hot_controller_admitted_chain_indices.contains(&chain_idx) {
                            hot_controller_admitted_chain_indices.push(chain_idx);
                        }
                    } else {
                        warn!(
                            chain_id = self.chain_id_for_board(idx)?,
                            pic_addr = format_args!("0x{:02X}", addr),
                            "PIC heartbeat failed during hot start — voltage may drop",
                        );
                    }
                }
            }
        }

        // Collect PIC addresses from hot chains for heartbeats during cold boot waits.
        // Uses poison-aware shared ownership so the init heartbeat thread can see newly-added
        // cold boot PICs in real time. CE REVIEW FIX: Previously used a static Vec
        // snapshot — cold boot PICs never got heartbeats from the init thread.
        let initialized_pic_addrs =
            InitializedPicAddrs::new(self.initialized_pic_addrs_final.clone());
        let mixed_chip_platform = identity.platform_marker();
        let mixed_chip_board_target = identity.board_target();
        let mixed_chip_policy = divergent_chip_policy_for_platform(
            self.config.mining.enforce_mixed_chip_id_refusal_on_xil25,
            mixed_chip_platform,
            mixed_chip_board_target,
            identity.psu_hardware_variant(),
        );

        // Start a background OS thread to send heartbeats every 500ms during init.
        // This is only needed when there are cold chains that still require the
        // longer bring-up path. Pure hot-start passthrough handoffs should avoid
        // a second direct-I2C heartbeat thread and let the runtime service take over.
        let init_hb_pause = if !cold_chain_indices.is_empty() {
            // G46 (no-brick / deterministic startup, gap-swarm 2026-05-28): if the OS
            // refuses to spawn the init-heartbeat thread (resource exhaustion), do NOT
            // panic — panic=abort would skip every Drop guard. Log it and proceed
            // WITHOUT the init HB: a degraded but SAFE state, since the cold chains then
            // rely on the PIC watchdog (which cuts voltage if unfed = safe direction)
            // rather than leaving an unsupervised energized rail behind an aborted process.
            match Self::start_init_heartbeat_thread(
                initialized_pic_addrs.clone(),
                self.pic_firmware,
                self.pic_type()?,
                i2c_svc.clone(),
            ) {
                Ok((stop, pause, handle)) => {
                    // Install ownership on `self` immediately. `init()` has many
                    // fallible phases after this point and is itself wrapped in a
                    // timeout. Keeping the handle only in local variables would
                    // detach the heartbeat thread when that future is dropped.
                    self.init_heartbeat_stop = Some(stop);
                    self.init_heartbeat_handle = Some(handle);
                    Some(pause)
                }
                Err(e) => {
                    error!(
                        error = %e,
                        "Failed to spawn init heartbeat thread — proceeding WITHOUT it (degraded). \
                         Cold-chain PICs will rely on the PIC watchdog (cuts voltage if unfed) instead \
                         of the 500ms init heartbeat; NOT panicking, so shutdown guards still run."
                    );
                    None
                }
            }
        } else {
            None
        };

        // Cold controller admission is deliberately independent of hot
        // keepalive membership and remains available to the Phase-7 gate.
        let mut cold_pic_admitted_board_indices: Vec<usize> = Vec::new();
        if !cold_chain_indices.is_empty() {
            // Derive board indices for cold chains (for GPIO/PIC operations).
            // Uses profile-aware mapping: S9: chain 6→0, S19: chain 1→0, etc.
            let cold_board_indices: Vec<usize> = cold_chain_indices
                .iter()
                .map(|&ci| self.board_idx_for_chain(self.chains[ci].chain_id))
                .collect::<Result<Vec<_>>>()?;

            // ---- Phase 5: Bosminer-style reset_and_enumerate (cold chains only) ----
            //
            // Modeled on bosminer's HashChain::reset_and_enumerate() from braiins_lib.rs.
            // Only runs for chains that didn't respond during hot start detection.
            //
            // Bosminer sequence (VERIFIED against live register dump):
            //   1. disable_ip_core()        — CTRL_REG = 0 (FPGA stops driving UART)
            //   2. enter_reset()            — GPIO RESET LOW (hold ASICs in reset)
            //   3. disable_voltage()        — PIC cmd: cut DC-DC output
            //   4. sleep(1s)                — capacitor discharge
            //   5. enable_voltage()         — PIC cmd: set voltage + enable DC-DC
            //   6. sleep(2s)                — DC-DC ramp + ASIC boot prep
            //   7. exit_reset()             — GPIO RESET HIGH (ASICs start booting)
            //   8. enable_ip_core()         — CTRL_REG = 0x0C (FPGA UART active)
            //   9. sleep(1s)                — ASIC boot time
            //  10. enumerate_chips()        — GetAddress broadcast
            //
            // CRITICAL FIX: Step 1 (disable_ip_core / CTRL_REG=0) is now included.
            // The 2026-03-12 bug (UART breaks after disable+re-enable) only occurs AFTER
            // UART traffic has flowed. On cold boot, there's been no traffic, so
            // disable→re-enable is safe and matches bosminer exactly.
            // FIX (swarm review #3, 2026-03-26): Do NOT unbind kernel I2C driver.
            // The heartbeat thread needs /dev/i2c-0 for kernel I2C (matching BraiinsOS).
            // Devmem cold_boot_init bypasses the kernel anyway — unbinding is unnecessary
            // and kills /dev/i2c-0, forcing heartbeats to fall back to broken devmem path.
            // BraiinsOS keeps the kernel driver bound for the entire daemon lifetime.
            // The former direct kernel-driver unbind was removed.

            info!("--- Phase 5: Hash Board Reset for {} cold chain(s) (bosminer-matched sequence) ---", cold_chain_indices.len());
            info!("Sequence: PSU init -> disable IP core -> assert RESET -> PIC voltage init -> release RESET -> re-enable IP core -> enumerate");

            // Step 5.0: Legacy smart-PSU initialization. The same authoritative
            // AM2-S17 predicate used by the later probe/feeder gates this early
            // raw kernel bus-1 access. Overrides and unproven platforms bypass it.
            let skip_psu_init = self
                .config
                .power
                .psu_override
                .as_ref()
                .map(|o| o.enabled)
                .unwrap_or(false);
            if skip_psu_init {
                info!("Step 5.0: PSU override active — skipping I2C PSU init (non-smart PSU, no Loki needed)");
            }
            let smart_psu_board_target = identity.board_target();
            let smart_psu_path_allowed = legacy_kernel_smart_psu_path_allowed(
                self.config.mining.model.as_deref(),
                smart_psu_board_target,
                skip_psu_init,
            );
            if smart_psu_path_allowed && !matches!(self.pic_type()?, PicType::NoPic) {
                info!("Step 5.0: Smart PSU initialization (kernel I2C APW path)");
                if let Err(e) = dcentrald_hal::platform::zynq::enable_psu_output() {
                    warn!(error = %e, "Zynq PSU output gate enable failed before smart PSU init");
                } else {
                    tracing::info!(
                        output_gate_enabled =
                            dcentrald_hal::platform::zynq::is_psu_output_enabled(),
                        "Zynq PSU output gate asserted before smart PSU initialization"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }

                match dcentrald_hal::psu::PsuController::open_kernel_only() {
                    Ok(mut psu) => match psu.get_version() {
                        Ok(version) => {
                            let model = dcentrald_hal::psu::PsuController::model_name_from_version(
                                &version,
                            );
                            let output_before = psu.read_state().ok();
                            let voltage_before = psu.measure_voltage().ok();

                            info!(
                                version = %version,
                                model = %model,
                                output_before = ?output_before,
                                voltage_before = ?voltage_before,
                                "PSU detected on kernel I2C — arming watchdog and verifying output state"
                            );

                            if let Err(e) = psu.disable_watchdog() {
                                tracing::debug!(error = %e, "PSU watchdog disable preflight failed");
                            }
                            if let Err(e) = psu.enable_watchdog() {
                                warn!(error = %e, "PSU watchdog enable failed");
                            }

                            std::thread::sleep(std::time::Duration::from_millis(500));
                            let output_after = psu.read_state().ok();
                            let voltage_after = psu.measure_voltage().ok();

                            if matches!(output_after, Some(false)) {
                                warn!(
                                    version = %version,
                                    "Smart PSU responded but reports output OFF. Direct EN-pin output gating is not wired on this platform yet."
                                );
                            }

                            if let Some(v) = voltage_after {
                                info!(
                                    voltage = format_args!("{:.2}V", v),
                                    "PSU output voltage after watchdog arm"
                                );
                            }

                            info!(
                                output_after = ?output_after,
                                voltage_after = ?voltage_after,
                                "Smart PSU initialization complete"
                            );
                        }
                        Err(e) => {
                            warn!(error = %e, "No smart PSU on kernel I2C bus 1 — assuming external/bypass power");
                        }
                    },
                    Err(e) => {
                        info!(error = %e, "Cannot open kernel I2C bus 1 for smart PSU — using bypass power");
                    }
                }
            } else if !skip_psu_init {
                tracing::debug!(
                    board_target = %smart_psu_board_target,
                    config_model = ?self.config.mining.model,
                    "Step 5.0: legacy kernel-I2C smart-PSU initialization is unproven for this platform; staying in bypass mode"
                );
            }

            // Step 5.1: Disable FPGA IP core on cold chains (matches bosminer enter_reset)
            //
            // BraiinsOS enter_reset() does: disable_ip_core() THEN GPIO LOW.
            // Disabling the IP core drops the UART TX line to LOW (BREAK condition),
            // which is the correct idle state while ASICs are held in reset.
            //
            // Previously we only reset FIFOs here, leaving the FPGA driving UART TX
            // in an indeterminate state. On cold boot (before Phase 4c UART traffic),
            // disable_ip_core is safe. After Phase 4c traffic, we use reset_ip_core()
            // which preserves MIDSTATE_CNT during the disable/enable cycle.
            //
            // The IP core will be RE-ENABLED in Step 5.5b (after GPIO reset release),
            // matching bosminer's exit_reset() sequence exactly.
            info!("Step 5.1: Disabling FPGA IP core on cold chains (bosminer enter_reset: IP core off BEFORE GPIO reset)");
            // Chip-aware CTRL value (BM1387=0x0C, BM1398=0x1C with BM139X bit).
            let ctrl_value_for_reconfigure = {
                let registry =
                    ChipRegistry::with_execution_policy(self.asic_driver_execution_policy);
                if let Some(drv) = registry.detect(self.chip_id) {
                    drv.ctrl_reg_value()
                } else {
                    dcentrald_hal::fpga_chain::CTRL_ENABLE
                        | (2 << dcentrald_hal::fpga_chain::CTRL_MIDSTATE_SHIFT) // 0x0C fallback
                }
            };
            for &chain_idx in &cold_chain_indices {
                // Single clean disable matching bosminer's enter_reset() exactly.
                // DO NOT call reset_ip_core() here — it creates a double-toggle
                // (ON→OFF→ON→OFF) that corrupts the FPGA UART state machine after
                // prior UART traffic from a previous session. BraiinsOS just clears ENABLE.
                let ctrl = self.chains[chain_idx]
                    .fpga
                    .common
                    .read_reg(dcentrald_hal::fpga_chain::REG_CTRL);
                self.chains[chain_idx].fpga.common.write_reg(
                    dcentrald_hal::fpga_chain::REG_CTRL,
                    ctrl & !dcentrald_hal::fpga_chain::CTRL_ENABLE,
                );
                info!(
                chain_id = self.chains[chain_idx].chain_id,
                "Chain {} FPGA IP core DISABLED (single clean disable, matching bosminer enter_reset)",
                self.chains[chain_idx].chain_id,
            );
            }
            // BREAK duration: 4 seconds for am2 (BM1398 needs prolonged BREAK to
            // auto-reset UART baud from operational back to 115200 default).
            // S9 (BM1387) only needs ~100ms but 4s doesn't hurt.
            let break_ms = if self.config.mining.model.is_some() {
                4000
            } else {
                100
            };
            info!(
                "UART BREAK: holding TX LOW for {}ms (ASIC baud reset to 115200)",
                break_ms
            );
            tokio::time::sleep(Duration::from_millis(break_ms)).await;

            // Step 5.2: RELEASE RESET on all hash boards (GPIO HIGH)
            //
            // CRITICAL FIX (v0.14.0): Previously this step ASSERTED reset (GPIO LOW),
            // but on this S9 hardware, GPIO LOW cuts I2C access to the PICs — they
            // NACK all transactions. This was THE root cause of the 60-second bug:
            // PICs 0x55/0x56 were initialized while GPIO was LOW (unreachable),
            // so they never received heartbeats, and their watchdog fired at ~64s.
            //
            // New sequence: RELEASE reset first so PICs are reachable for init,
            // then ASSERT reset briefly around ASIC enumeration only.
            // FPGA IP core is already disabled (Step 5.1), so ASICs don't see
            // spurious UART data even with GPIO HIGH.
            info!("Step 5.2: Releasing RESET on cold hash boards (GPIO HIGH — PICs reachable for I2C init)");
            for &idx in &cold_board_indices {
                let gpio_num = self.enable_gpio_base()? + idx as u32;
                let dir_path = format!("/sys/class/gpio/gpio{}/direction", gpio_num);
                let val_path = format!("/sys/class/gpio/gpio{}/value", gpio_num);
                let _ = std::fs::write(&dir_path, "out");
                let _ = std::fs::write(&val_path, "1");
                // AXI GPIO: only on S9 (am1). am2-s17 uses different GPIO address (0x41220000).
                // Sysfs GPIO write above handles all platforms correctly via kernel driver.
                if self.config.mining.model.is_none() {
                    if let Some(ref gpio) = self.gpio {
                        gpio.exit_reset(idx as u8);
                    }
                }
                info!(
                    chain_id = self.chain_id_for_board(idx)?,
                    gpio = gpio_num,
                    "Chain {} RESET released (GPIO {} = HIGH) — PIC I2C now reachable",
                    self.chain_id_for_board(idx)?,
                    gpio_num,
                );
            }
            // Give PICs 100ms to stabilize after GPIO change
            tokio::time::sleep(Duration::from_millis(100)).await;

            // PIC16 cold-boot admission must be the first controller mutation:
            // a pre-admission heartbeat would overwrite the raw SSPBUF evidence
            // needed for the worker's exact-0xCC transition decision.
            info!("Step 5.2a: Preserving cold PIC16 raw-state evidence until atomic admission");
            for &idx in &cold_board_indices {
                let pic_addr = self.pic_addr_for_board(idx)?;
                match self.pic_type()? {
                    PicType::DsPic33EP => {
                        let mut dspic = DspicService::new(i2c_svc.clone(), pic_addr);
                        let _ = dspic.send_heartbeat();
                    }
                    _ => {
                        info!(
                            i2c_addr = format_args!("0x{:02X}", pic_addr),
                            "PIC16 pre-admission heartbeat intentionally skipped"
                        );
                    }
                }
            }

            // Step 5.2b: Disable voltage on cold chain PICs (power cycle DC-DC)
            //
            // Bosminer does disable_voltage() → sleep(1s) → enable_voltage() on every
            // reset_and_enumerate(). On warm-cold boot (PIC watchdog fired after bosminer
            // was killed), there may be residual charge keeping ASICs in a partial state.
            // This power-cycles the DC-DC converter to ensure a clean cold start.
            //
            // We detect PIC firmware first (needed for correct command set), then disable
            // voltage. If PIC is unresponsive, we skip — cold_boot_init in Step 5.3 will
            // handle it.
            // BUG FIX (2026-03-14): If firmware is already known from hot start, skip
            // detect_firmware() on cold chains. detect_firmware() uses I2C_RDWR which
            // corrupts the Zynq I2C adapter when PICs are dead (EIO). This corruption
            // is bus-wide — it prevents ALL subsequent I2C operations including heartbeats
            // to hot chain PICs, causing voltage cutoff on working hash boards.
            if self.pic_type()? != PicType::DsPic33EP {
                info!("Step 5.2b: PIC16 detect/disable intentionally skipped; atomic cold-boot admission owns first controller mutation");
            } else if self.pic_firmware != PicFirmware::Unknown {
                info!("Step 5.2b: Skipping cold chain PIC probe — firmware already known ({}), avoiding I2C_RDWR bus corruption",
                self.pic_firmware);
                // Just try simple write-only disable_voltage on cold PICs (safe for I2C bus)
                for &idx in &cold_board_indices {
                    let pic_addr = self.pic_addr_for_board(idx)?;
                    match self.pic_type()? {
                        PicType::DsPic33EP => {
                            // FIX: Do NOT disable dsPIC voltage on cold boot.
                            // disable_voltage kills rails that may already be providing
                            // power from PSU power-on defaults. cold_boot_init() will
                            // set the correct voltage without disabling first.
                            info!("Step 5.2b: SKIP dsPIC disable (preserving PSU power-on state)");
                        }
                        _ => {
                            let pic = pic16_service_for_endpoint(
                                &pic16_endpoint_sessions,
                                pic_addr,
                                Some(self.pic_firmware),
                            )?;
                            let _ = pic.disable_voltage(); // ignore errors — cold PICs may be dead
                        }
                    }
                }
            } else {
                info!("Step 5.2b: Detecting PIC firmware and disabling voltage on cold chains (DC-DC power cycle)");
                for &idx in &cold_board_indices {
                    let pic_addr = self.pic_addr_for_board(idx)?;
                    match self.pic_type()? {
                        PicType::DsPic33EP => {
                            // FIX: Skip dsPIC disable + detect entirely on cold boot.
                            // disable_voltage kills rails. detect_firmware uses I2C_RDWR (dangerous).
                            // cold_boot_init() in Step 5.3 handles everything write-only.
                            info!(
                                chain_id = self.chain_id_for_board(idx)?,
                                "Step 5.2b: SKIP dsPIC disable+detect (write-only init in Step 5.3)",
                            );
                            info!(
                                "dsPIC Step 5.2b skipped for chain {}",
                                self.chain_id_for_board(idx)?
                            );
                        }
                        _ => {
                            let mut pic = pic16_service_for_endpoint(
                                &pic16_endpoint_sessions,
                                pic_addr,
                                None,
                            )?;
                            match pic.detect_firmware() {
                                Ok(fw) => {
                                    if self.pic_firmware == PicFirmware::Unknown {
                                        self.pic_firmware = fw;
                                    }
                                    let pic = pic16_service_for_endpoint(
                                        &pic16_endpoint_sessions,
                                        pic_addr,
                                        Some(self.pic_firmware),
                                    )?;
                                    match pic.disable_voltage() {
                                        Ok(()) => info!(
                                            chain_id = self.chain_id_for_board(idx)?,
                                            pic_addr = format_args!("0x{:02X}", pic_addr),
                                            firmware = %self.pic_firmware,
                                            "DC-DC disabled on chain {} — power cycling for clean cold start",
                                            self.chain_id_for_board(idx)?,
                                        ),
                                        Err(e) => info!(
                                            chain_id = self.chain_id_for_board(idx)?,
                                            error = %e,
                                            "Could not disable voltage on chain {} — may be true cold boot (DC-DC never enabled)",
                                            self.chain_id_for_board(idx)?,
                                        ),
                                    }
                                }
                                Err(e) => info!(
                                    chain_id = self.chain_id_for_board(idx)?,
                                    pic_addr = format_args!("0x{:02X}", pic_addr),
                                    error = %e,
                                    "PIC firmware detection failed on chain {} — skipping voltage disable",
                                    self.chain_id_for_board(idx)?,
                                ),
                            }
                        }
                    }
                }
            }

            // Step 5.2c: Wait 1 second for capacitor discharge
            // After disabling DC-DC, ASICs need time for the bulk capacitors on the hash
            // board to drain. Bosminer waits 1 second. During this wait, heartbeat any
            // hot chain PICs to keep their voltage alive.
            info!("Step 5.2c: Waiting 1s for capacitor discharge on cold chains (heartbeating hot chain PICs)...");
            tokio::time::sleep(Duration::from_secs(1)).await;
            {
                let addrs = initialized_pic_addrs.snapshot("phase 5.2c heartbeat snapshot");
                if !addrs.is_empty() {
                    match self.pic_type()? {
                        PicType::DsPic33EP => {
                            for &addr in &addrs {
                                let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                                let _ = dspic.send_heartbeat();
                            }
                        }
                        _ => {
                            for &addr in &addrs {
                                let _ = i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                            }
                        }
                    }
                }
            }

            // Step 5.3: PIC Voltage Controller Init (I2C)
            // PIC16 fail-closed admission:
            //   1. Worker reads one exact raw-state byte.
            //   2. Only same-worker exact 0xCC may emit fixed JUMP [55 AA 06].
            //   3. Positive application evidence is required after any JUMP.
            //   4. Five consecutive 1 Hz heartbeats precede one SET+ENABLE.
            // RESET is intentionally absent from the production PIC16 path.
            //
            // CRITICAL TIMING: The BraiinsOS PIC watchdog fires at ~10s without heartbeats.
            // When we kill bosminer, the PIC watchdog starts counting down. We MUST complete
            // PIC init for each chain before the watchdog fires, and we must send interim
            // heartbeats to already-initialized PICs while initializing subsequent chains.
            info!("--- Step 5.3: PIC Voltage Controller Init (I2C) ---");
            info!(
                "Each hash board has a PIC16F1704 that controls chip voltage via I2C bus {}",
                DEFAULT_I2C_BUS
            );

            info!("Step 5.3a: Probing PICs in current state (no RESET — preserving existing PIC state)");

            // Quick I2C bus diagnostic after reset
            {
                let mut found_any = false;
                for &idx in &cold_board_indices {
                    let addr = self.pic_addr_for_board(idx)?;
                    match self.pic_type()? {
                        PicType::DsPic33EP => {
                            if let Ok(buf) = i2c_svc.read_bytes(addr, 1) {
                                if let Some(&byte) = buf.first() {
                                    info!(
                                        i2c_addr = format_args!("0x{:02X}", addr),
                                        response = format_args!("0x{:02X}", byte),
                                        "PIC at 0x{:02X} responding after reset (raw: 0x{:02X})",
                                        addr,
                                        byte,
                                    );
                                    found_any = true;
                                }
                            }
                        }
                        _ => {
                            info!(
                                i2c_addr = format_args!("0x{:02X}", addr),
                                "PIC16 diagnostic raw read deferred to atomic cold-boot admission"
                            );
                        }
                    }
                }
                if !found_any && self.pic_type()? == PicType::DsPic33EP {
                    warn!("No PICs responding after reset — hash boards may lack PSU power (12V from 6-pin connectors)");
                }
            }

            // Fast-skip PIC init strategy:
            // A dead/slow PIC must NOT block init of working PICs. BraiinsOS PIC watchdog
            // fires at ~10s — if we spend 10s retrying a dead PIC, the live ones die too.
            //
            // Pass 1: Try each PIC ONCE. Init whoever responds, skip failures immediately.
            // Pass 2: Retry failed PICs (they may have needed more time to boot).
            // Between passes: send heartbeats to keep initialized PICs alive.

            let initial_pic_val = dcentrald_asic::pic::DEFAULT_VOLTAGE_PIC;
            let voltage_v = PicController::pic_to_voltage(initial_pic_val);
            // dsPIC uses millivolt values from the MinerProfile (e.g. 13800 for BM1398)
            let dspic_init_voltage_mv: u16 = self
                .miner_profile
                .map(|p| p.default_voltage_mv)
                .unwrap_or(dcentrald_asic::dspic::DEFAULT_VOLTAGE_MV);

            let mut failed_indices: Vec<usize> = Vec::new();

            // === Pass 1: Fast single-attempt init of cold chain PICs ===
            info!("Step 5.3c: Fast PIC init pass — trying each cold chain PIC once, skipping failures immediately");
            for &idx in &cold_board_indices {
                let pic_addr = self.pic_addr_for_board(idx)?;
                info!(
                    chain_id = self.chain_id_for_board(idx)?,
                    i2c_addr = format_args!("0x{:02X}", pic_addr),
                    "PIC init (pass 1) on chain {} at I2C 0x{:02X}",
                    self.chain_id_for_board(idx)?,
                    pic_addr,
                );

                let mut pic_ok = false;
                match self.pic_type()? {
                    PicType::DsPic33EP => {
                        let mut dspic = DspicService::new(i2c_svc.clone(), pic_addr);
                        match dspic.cold_boot_init(dspic_init_voltage_mv) {
                            Ok(()) => {
                                let dspic_v = dspic_init_voltage_mv as f64 / 1000.0;
                                info!(
                                    chain_id = self.chain_id_for_board(idx)?,
                                    firmware = %dspic.firmware(),
                                    voltage_mv = dspic_init_voltage_mv,
                                    voltage = format_args!("{:.2}V", dspic_v),
                                    "dsPIC initialized on first try — voltage {:.2}V, output enabled",
                                    dspic_v,
                                );
                                pic_ok = true;
                            }
                            Err(e) => {
                                warn!(
                                    chain_id = self.chain_id_for_board(idx)?,
                                    i2c_addr = format_args!("0x{:02X}", pic_addr),
                                    error = %e,
                                    "dsPIC init failed (pass 1) — skipping to next, will retry later",
                                );
                            }
                        }
                    }
                    _ => {
                        let mut pic =
                            pic16_service_for_endpoint(&pic16_endpoint_sessions, pic_addr, None)?;
                        match pic.cold_boot_init(initial_pic_val) {
                            Ok(()) => {
                                if self.pic_firmware == PicFirmware::Unknown {
                                    self.pic_firmware = pic.firmware();
                                    info!(
                                        firmware = %self.pic_firmware,
                                        "PIC firmware type detected — all subsequent PIC operations will use this command set",
                                    );
                                }
                                info!(
                                    chain_id = self.chain_id_for_board(idx)?,
                                    firmware = %self.pic_firmware,
                                    pic_value = initial_pic_val,
                                    voltage = format_args!("{:.2}V", voltage_v),
                                    "PIC initialized on first try — voltage {:.2}V, output enabled",
                                    voltage_v,
                                );
                                pic_ok = true;
                            }
                            Err(e) => {
                                warn!(
                                    chain_id = self.chain_id_for_board(idx)?,
                                    i2c_addr = format_args!("0x{:02X}", pic_addr),
                                    error = %e,
                                    "PIC init failed (pass 1) — skipping to next PIC, will retry later",
                                );
                            }
                        }
                    }
                }

                if pic_ok {
                    if !cold_pic_admitted_board_indices.contains(&idx) {
                        cold_pic_admitted_board_indices.push(idx);
                    }
                    initialized_pic_addrs
                        .record_success(pic_addr, "phase 5.3 pass 1 PIC admission");
                    // Init heartbeat thread automatically picks up new PIC on next tick.
                    // Also send a manual heartbeat to all previously-initialized PICs now.
                    {
                        let addrs = initialized_pic_addrs
                            .snapshot("phase 5.3 post-admission heartbeat snapshot");
                        if addrs.len() > 1 {
                            match self.pic_type()? {
                                PicType::DsPic33EP => {
                                    for &addr in &addrs[..addrs.len() - 1] {
                                        let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                                        let _ = dspic.send_heartbeat();
                                    }
                                }
                                _ => {
                                    for &addr in &addrs[..addrs.len() - 1] {
                                        let _ =
                                            i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    failed_indices.push(idx);
                }
            }

            // === Pass 2: Retry failed PICs (up to 3 more attempts each) ===
            if !failed_indices.is_empty() {
                info!(
                    "Step 5.3d: Retrying {} failed PIC(s) — they may need more time after reset",
                    failed_indices.len(),
                );

                for retry_round in 1..=3u32 {
                    if failed_indices.is_empty() {
                        break;
                    }

                    // Heartbeat all live PICs before each retry round
                    {
                        let addrs =
                            initialized_pic_addrs.snapshot("phase 5.3 retry heartbeat snapshot");
                        match self.pic_type()? {
                            PicType::DsPic33EP => {
                                for &addr in &addrs {
                                    let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                                    let _ = dspic.send_heartbeat();
                                }
                            }
                            _ => {
                                for &addr in &addrs {
                                    let _ = i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                                }
                            }
                        }
                    }

                    tokio::time::sleep(Duration::from_millis(2000)).await;

                    let mut still_failed: Vec<usize> = Vec::new();
                    for &idx in &failed_indices {
                        let pic_addr = self.pic_addr_for_board(idx)?;
                        info!(
                            chain_id = self.chain_id_for_board(idx)?,
                            i2c_addr = format_args!("0x{:02X}", pic_addr),
                            retry_round,
                            "PIC retry {}/3 on chain {} at I2C 0x{:02X}",
                            retry_round,
                            self.chain_id_for_board(idx)?,
                            pic_addr,
                        );

                        let mut pic_ok = false;
                        match self.pic_type()? {
                            PicType::DsPic33EP => {
                                let mut dspic = DspicService::new(i2c_svc.clone(), pic_addr);
                                match dspic.cold_boot_init(dspic_init_voltage_mv) {
                                    Ok(()) => {
                                        let dspic_v = dspic_init_voltage_mv as f64 / 1000.0;
                                        info!(
                                            chain_id = self.chain_id_for_board(idx)?,
                                            firmware = %dspic.firmware(),
                                            retry_round,
                                            "dsPIC initialized on retry {} — voltage {:.2}V, output enabled",
                                            retry_round, dspic_v,
                                        );
                                        pic_ok = true;
                                    }
                                    Err(e) => {
                                        if retry_round == 3 {
                                            error!(
                                                chain_id = self.chain_id_for_board(idx)?,
                                                i2c_addr = format_args!("0x{:02X}", pic_addr),
                                                error = %e,
                                                "dsPIC init FAILED after all retries on chain {} — this hash board won't mine. Check: PSU 6-pin cables, ribbon cable on J{}",
                                                self.chain_id_for_board(idx)?, self.chain_id_for_board(idx)?,
                                            );
                                        } else {
                                            warn!(
                                                chain_id = self.chain_id_for_board(idx)?,
                                                error = %e,
                                                retry_round,
                                                "dsPIC retry {}/3 failed — will try again",
                                                retry_round,
                                            );
                                        }
                                    }
                                }
                            }
                            _ => {
                                let mut pic = pic16_service_for_endpoint(
                                    &pic16_endpoint_sessions,
                                    pic_addr,
                                    None,
                                )?;
                                match pic.cold_boot_init(initial_pic_val) {
                                    Ok(()) => {
                                        if self.pic_firmware == PicFirmware::Unknown {
                                            self.pic_firmware = pic.firmware();
                                        }
                                        info!(
                                            chain_id = self.chain_id_for_board(idx)?,
                                            firmware = %self.pic_firmware,
                                            retry_round,
                                            "PIC initialized on retry {} — voltage {:.2}V, output enabled",
                                            retry_round, voltage_v,
                                        );
                                        pic_ok = true;
                                    }
                                    Err(e) => {
                                        if retry_round == 3 {
                                            error!(
                                                chain_id = self.chain_id_for_board(idx)?,
                                                i2c_addr = format_args!("0x{:02X}", pic_addr),
                                                error = %e,
                                                "PIC init FAILED after all retries on chain {} — this hash board won't mine. Check: PSU 6-pin cables, ribbon cable on J{}",
                                                self.chain_id_for_board(idx)?, self.chain_id_for_board(idx)?,
                                            );
                                        } else {
                                            warn!(
                                                chain_id = self.chain_id_for_board(idx)?,
                                                error = %e,
                                                retry_round,
                                                "PIC retry {}/3 failed — will try again",
                                                retry_round,
                                            );
                                        }
                                    }
                                }
                            }
                        }

                        if pic_ok {
                            if !cold_pic_admitted_board_indices.contains(&idx) {
                                cold_pic_admitted_board_indices.push(idx);
                            }
                            initialized_pic_addrs
                                .record_success(pic_addr, "phase 5.3 retry PIC admission");
                        } else {
                            still_failed.push(idx);
                        }
                    }
                    failed_indices = still_failed;
                }
            }

            // From this point forward, shadow the cold board/chain lists with
            // controller-admitted pairs. Failed PIC chains remain FPGA-disabled
            // and cannot enter reset release, enumeration, or Phase 7.
            let eligible_cold_pairs: Vec<(usize, usize)> = cold_board_indices
                .iter()
                .copied()
                .zip(cold_chain_indices.iter().copied())
                .filter(|(board_idx, _)| cold_pic_admitted_board_indices.contains(board_idx))
                .collect();
            let cold_board_indices: Vec<usize> = eligible_cold_pairs
                .iter()
                .map(|(board_idx, _)| *board_idx)
                .collect();
            let cold_chain_indices: Vec<usize> = eligible_cold_pairs
                .iter()
                .map(|(_, chain_idx)| *chain_idx)
                .collect();

            // Report final cold-controller admission results.
            let admitted_cold_count = cold_pic_admitted_board_indices.len();
            if admitted_cold_count == 0 {
                error!("NO PICs initialized — all hash boards failed. Mining cannot proceed without voltage control.");
            } else {
                let ok_count = admitted_cold_count;
                let fail_count = failed_indices.len();
                info!(
                    initialized = ok_count,
                    failed = fail_count,
                    "PIC init complete — {}/{} hash board(s) have voltage control",
                    ok_count,
                    ok_count + fail_count,
                );
                for &idx in &failed_indices {
                    warn!(
                    chain_id = self.chain_id_for_board(idx)?,
                    "Chain {} has no PIC — ASIC chips have no voltage, mining disabled on this chain",
                    self.chain_id_for_board(idx)?,
                );
                }
            }

            // Step 5.4: Wait 2 seconds with voltage enabled, RESET still asserted
            // This matches bosminer's 2-second delay after enable_voltage() but BEFORE
            // exit_reset(). The DC-DC converter needs time to ramp up and stabilize.
            // ASICs are held in reset during this time — they don't boot yet.
            // Send heartbeats at 1s intervals to prevent PIC watchdog timeout.
            info!("Step 5.4: Waiting 2 seconds for DC-DC voltage to stabilize (RESET still asserted, ASICs not booting yet)...");
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let addrs = initialized_pic_addrs.snapshot("phase 5.4 heartbeat snapshot");
                match self.pic_type()? {
                    PicType::DsPic33EP => {
                        for &addr in &addrs {
                            let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                            let _ = dspic.send_heartbeat();
                        }
                    }
                    _ => {
                        for &addr in &addrs {
                            let _ = i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                        }
                    }
                }
            }

            // Step 5.4b: Verify DC-DC voltage readback (COLD CHAIN PICs ONLY)
            //
            // BUG FIX (2026-03-14): read_voltage() uses I2C_RDWR which CORRUPTS the
            // stock PIC's I2C parser. If we read hot-chain PICs here, their parser gets
            // stuck and all subsequent heartbeats fail, causing the ~5s watchdog to fire
            // and cut voltage. Only read cold-chain PICs (which went through full init
            // and can tolerate the parser reset in Phase 5.4c).
            info!("Step 5.4b: Verifying DC-DC voltage output via PIC readback (cold chain PICs only)...");
            let admitted_readback_addrs = cold_board_indices
                .iter()
                .map(|&idx| self.pic_addr_for_board(idx))
                .collect::<Result<Vec<_>>>()?;
            let initialized_addrs_for_readback =
                initialized_pic_addrs.snapshot("phase 5.4 voltage readback admission");
            let addrs_for_readback: Vec<u8> = admitted_readback_addrs
                .into_iter()
                .filter(|addr| initialized_addrs_for_readback.contains(addr))
                .collect();
            {
                for &addr in &addrs_for_readback {
                    match self.pic_type()? {
                        PicType::DsPic33EP => {
                            let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                            match dspic.read_voltage() {
                                Ok(mv) => {
                                    let voltage = mv as f64 / 1000.0;
                                    info!(
                                        pic_addr = format_args!("0x{:02X}", addr),
                                        voltage_mv = mv,
                                        voltage_v = format_args!("{:.2}", voltage),
                                        "dsPIC DC-DC voltage readback: 0x{:02X} -> {} mV ({:.2}V)",
                                        addr,
                                        mv,
                                        voltage,
                                    );
                                    if mv == 0 {
                                        warn!(
                                                pic_addr = format_args!("0x{:02X}", addr),
                                                voltage_mv = mv,
                                                "dsPIC DC-DC may not be producing voltage! — check: PSU cable to hash board",
                                            );
                                    }
                                }
                                Err(e) => warn!(
                                    pic_addr = format_args!("0x{:02X}", addr),
                                    error = %e,
                                    "dsPIC voltage readback failed on 0x{:02X} — cannot verify DC-DC output",
                                    addr,
                                ),
                            }
                        }
                        _ => info!(
                            pic_addr = format_args!("0x{:02X}", addr),
                            firmware = %self.pic_firmware,
                            "Skipping PIC16 voltage readback to avoid parser-corrupting I2C_RDWR reads",
                        ),
                    }
                }
            }

            // Step 5.4c: No PIC16 parser reset needed because PIC16 readback is skipped above.

            // Step 5.5: ASIC reset pulse + Enable FPGA IP core
            //
            // v0.14.0: GPIO was already set HIGH in Step 5.2 (for PIC I2C access).
            // Now we need to reset the ASICs: assert LOW briefly, then release HIGH.
            // This gives ASICs a clean hardware reset while PICs stay alive (PIC
            // watchdog is ~64s, the pulse is only 100ms).
            //
            // BraiinsOS exit_reset() does: GPIO HIGH + enable_ip_core().
            // We match this by asserting LOW (100ms pulse) then releasing HIGH.
            // Step 5.5: Enable FPGA IP core (NO GPIO pulse)
            //
            // v0.14.3: REMOVED the GPIO LOW pulse. The 100ms GPIO LOW was killing PICs:
            // if the init heartbeat thread sent a heartbeat during the pulse, the PIC's
            // I2C slave saw START+address then the bus went dead (GPIO cut SDA/SCL).
            // Without a STOP condition, the PIC's MSSP module gets stuck forever.
            //
            // ASICs don't need a GPIO reset pulse — the FPGA IP core was disabled
            // since Step 5.1 (~8s ago), putting the UART TX line in BREAK state.
            // ASICs reset themselves after seeing BREAK for >1s. BraiinsOS's
            // enter_reset() was paired with exit_reset() where PICs respond during
            // RESET — but on this S9, GPIO LOW kills I2C. Skip the pulse entirely.
            //
            // GPIO was set HIGH in Step 5.2 and stays HIGH throughout.
            info!("Step 5.5: Enabling FPGA IP core (GPIO stays HIGH — no pulse, PICs safe)");

            // v0.15.0: Pause init heartbeats during FPGA enable (AXI register writes)
            if let Some(ref pause) = init_hb_pause {
                pause.store(true, std::sync::atomic::Ordering::Release);
            }

            // Enable FPGA IP core on each chain (matching BraiinsOS exit_reset)
            for (&idx, &chain_idx) in cold_board_indices.iter().zip(cold_chain_indices.iter()) {
                let gpio_num = self.enable_gpio_base()? + idx as u32;
                let chain = &mut self.chains[chain_idx];

                // GPIO HIGH (release reset) FIRST — ASICs need power before FPGA enables UART.
                // On cold boot after a mining session, the FPGA UART state machine has residual
                // state from the previous 3.125 MHz baud rate. Releasing GPIO before FPGA enable
                // ensures ASICs are powered and stable before the UART comes out of BREAK.
                let val_path = format!("/sys/class/gpio/gpio{}/value", gpio_num);
                let _ = std::fs::write(&val_path, "1");
                // AXI GPIO: only on S9 (am1). am2-s17 uses different GPIO address (0x41220000).
                // Sysfs GPIO write above handles all platforms correctly via kernel driver.
                if self.config.mining.model.is_none() {
                    if let Some(ref gpio) = self.gpio {
                        gpio.exit_reset(idx as u8);
                    }
                }

                // Use reconfigure() for proper FPGA re-initialization sequence.
                // Cold boot (both S9 and am2): use 115200 for enumeration.
                // The 4-second BREAK reset ASICs to 115200 default baud.
                let reconfigure_baud = dcentrald_hal::fpga_chain::BAUD_REG_115200;
                chain
                    .fpga
                    .reconfigure(ctrl_value_for_reconfigure, reconfigure_baud);

                // CRITICAL: Update fpga_midstate_cnt to match the CTRL we just wrote.
                // Phase 4b read the OLD CTRL (e.g. 0x1E = MIDSTATE_CNT=3 from bosminer).
                // Phase 5 wrote NEW CTRL (0x1C = MIDSTATE_CNT=2). Without this update,
                // send_work uses shift=3 but FPGA uses shift=2 → work_id corrupted → 100% share rejection.
                let ctrl = chain
                    .fpga
                    .common
                    .read_reg(dcentrald_hal::fpga_chain::REG_CTRL);
                chain.fpga_midstate_cnt =
                    ((ctrl >> dcentrald_hal::fpga_chain::CTRL_MIDSTATE_SHIFT) & 0x03) as u8;
                info!(
                chain_id = self.chain_id_for_board(idx)?,
                gpio = gpio_num,
                ctrl = format_args!("0x{:08X}", ctrl),
                "Chain {} exit_reset: GPIO {} = HIGH + FPGA CTRL=0x{:02X} (ENABLE={}, MIDSTATE_CNT={})",
                self.chain_id_for_board(idx)?, gpio_num, ctrl,
                if ctrl & 0x08 != 0 { "ON" } else { "OFF" },
                (ctrl >> 1) & 0x03,
            );
            }

            // v0.15.0: Resume init heartbeats after FPGA enable.
            // 200ms settle for AXI bus after CTRL/FIFO register writes.
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Some(ref pause) = init_hb_pause {
                pause.store(false, std::sync::atomic::Ordering::Release);
            }

            // Step 5.6: Diagnostic register dump after exit_reset
            info!("Step 5.6: FPGA state after exit_reset (diagnostic dump)");
            for &chain_idx in &cold_chain_indices {
                let chain = &mut self.chains[chain_idx];
                let ctrl = chain
                    .fpga
                    .common
                    .read_reg(dcentrald_hal::fpga_chain::REG_CTRL);
                let baud = chain
                    .fpga
                    .common
                    .read_reg(dcentrald_hal::fpga_chain::REG_BAUD);
                let cmd_stat = chain
                    .fpga
                    .cmd
                    .read_reg(dcentrald_hal::fpga_chain::REG_CMD_STAT);
                let wrx_stat = chain
                    .fpga
                    .work_rx
                    .read_reg(dcentrald_hal::fpga_chain::REG_WORK_RX_STAT);
                let wtx_stat = chain
                    .fpga
                    .work_tx
                    .read_reg(dcentrald_hal::fpga_chain::REG_WORK_TX_STAT);
                let wrx_ctrl = chain
                    .fpga
                    .work_rx
                    .read_reg(dcentrald_hal::fpga_chain::REG_WORK_RX_CTRL);
                let wtx_ctrl = chain
                    .fpga
                    .work_tx
                    .read_reg(dcentrald_hal::fpga_chain::REG_WORK_TX_CTRL);
                let err_cnt = chain.fpga.read_error_count();
                let actual_baud = dcentrald_hal::fpga_chain::FpgaChain::baud_from_divisor(baud);
                info!(
                    chain_id = chain.chain_id,
                    ctrl = format_args!("0x{:08X}", ctrl),
                    baud_div = format_args!("0x{:02X}", baud),
                    baud_hz = actual_baud,
                    cmd_stat = format_args!("0x{:02X}", cmd_stat),
                    wrx_ctrl = format_args!("0x{:02X}", wrx_ctrl),
                    wrx_stat = format_args!("0x{:02X}", wrx_stat),
                    wtx_ctrl = format_args!("0x{:02X}", wtx_ctrl),
                    wtx_stat = format_args!("0x{:02X}", wtx_stat),
                    crc_errors = err_cnt,
                    "Chain {} FPGA state after exit_reset: \
                 CTRL=0x{:02X} (ENABLE={}, MIDSTATE_CNT={}), baud={} (div=0x{:02X}), \
                 CMD: TX_EMPTY={} RX_EMPTY={} IRQ={}, \
                 WORK_RX: CTRL=0x{:02X} (IRQ_EN={}) RX_EMPTY={}, \
                 WORK_TX: CTRL=0x{:02X} (IRQ_EN={}) TX_EMPTY={}, \
                 CRC_ERRORS={}",
                    chain.chain_id,
                    ctrl,
                    if ctrl & 0x08 != 0 { "ON" } else { "OFF" },
                    (ctrl >> 1) & 0x03,
                    actual_baud,
                    baud,
                    if cmd_stat & 0x04 != 0 { "yes" } else { "NO" },
                    if cmd_stat & 0x01 != 0 { "yes" } else { "NO" },
                    if cmd_stat & 0x10 != 0 { "yes" } else { "no" },
                    wrx_ctrl,
                    if wrx_ctrl & 0x04 != 0 { "ON" } else { "OFF" },
                    if wrx_stat & 0x01 != 0 { "yes" } else { "NO" },
                    wtx_ctrl,
                    if wtx_ctrl & 0x04 != 0 { "ON" } else { "OFF" },
                    if wtx_stat & 0x04 != 0 { "yes" } else { "NO" },
                    err_cnt,
                );
            }

            // Step 5.7: Wait for ASICs to complete boot sequence
            //
            // BraiinsOS timing: exit_reset() → delay(INIT_DELAY=1s) → enumerate_chips()
            // But BraiinsOS retries up to 6 times with 2s delay, so effective wait can be
            // up to ~19s. On true cold boot (first power-on, no residual charge), ASICs
            // need more time for PLL lock and UART initialization.
            //
            // We use 4s initial wait (vs bosminer's 1s) because:
            // - We do a full DC-DC power cycle (bosminer often has residual voltage)
            // - On cold boot after a mining session, ASICs need extra time for PLL lock
            //   and UART initialization due to residual FPGA UART state from 3.125 MHz
            // - The retry loop (Step 5.8) handles the rest if 4s isn't enough
            //
            // Total budget with retries: 4s + (3 retries * (enumerate_time + 3s)) ≈ 16s
            // am2 (S19 Pro): DC-DC ramps from 0V to 13.8V — needs more time.
            // am2 (S19 Pro): bosminer waits 21+ seconds for ASIC boot after voltage enable.
            // BM1398 hash boards need significant time for DC-DC ramp + ASIC power-on-reset.
            let initial_boot_wait_secs: u64 = if self.config.mining.model.is_some() {
                21
            } else {
                4
            };
            info!(
            "Step 5.7: Waiting {}s for ASIC boot after reset release (retry loop follows if needed)...",
            initial_boot_wait_secs,
        );
            for i in 0..initial_boot_wait_secs {
                tokio::time::sleep(Duration::from_secs(1)).await;
                // Send PIC heartbeats during the wait to prevent watchdog timeout
                let addrs = initialized_pic_addrs.snapshot("initial boot wait heartbeat snapshot");
                match self.pic_type()? {
                    PicType::DsPic33EP => {
                        for &addr in &addrs {
                            let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                            let _ = dspic.send_heartbeat();
                        }
                    }
                    _ => {
                        for &addr in &addrs {
                            let _ = i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                        }
                    }
                }
                info!(
                    "  ASIC boot wait: {}s / {}s elapsed...",
                    i + 1,
                    initial_boot_wait_secs
                );
            }
            info!("Initial ASIC boot wait complete — starting enumeration with retry loop");

            // ---- Phase 6: Chip detection with retry loop (matches bosminer) ----
            //
            // BraiinsOS retries enumeration up to ENUM_RETRY_COUNT=6 times with
            // ENUM_RETRY_DELAY=2s between attempts (braiins_lib.rs:84-86, 495-516).
            // Each retry does a full reset_and_enumerate() which includes voltage
            // cycle + reset + enumerate.
            //
            // Our retry is lighter: we don't re-do the full voltage cycle, just
            // re-attempt GetAddress with increasing wait times. On cold boot,
            // ASICs may need up to 8-10s to fully boot PLL and UART after a
            // complete power cycle. The initial 2s wait + 3 retries with 3s
            // delay gives us up to 13s total.
            //
            // Retry strategy per chain:
            //   Attempt 1: enumerate at 115200/1.5M/3.125M (already waited 2s)
            //   Attempt 2: wait 3s, reset FPGA FIFOs, retry enumerate
            //   Attempt 3: wait 3s, toggle IP core (BREAK), retry enumerate
            //   Attempt 4: wait 4s, last attempt (accept any chip count)
            const ENUM_MAX_RETRIES: usize = 3;
            const ENUM_RETRY_DELAY_SECS: u64 = 3;

            info!(
                "--- Phase 6: ASIC Chip Detection with retry loop (up to {} retries per chain) ---",
                ENUM_MAX_RETRIES
            );
            info!("Sending GetAddress broadcast on each cold chain — every ASIC chip will respond with its ID");

            let registry = ChipRegistry::with_execution_policy(self.asic_driver_execution_policy);

            for &chain_idx in &cold_chain_indices {
                let mut enumerated = false;

                for attempt in 0..=ENUM_MAX_RETRIES {
                    // Send PIC heartbeats before each enumeration attempt.
                    // enumerate_chips() blocks for ~500ms+ waiting for responses.
                    {
                        let addrs = initialized_pic_addrs
                            .snapshot("enumeration attempt heartbeat snapshot");
                        match self.pic_type()? {
                            PicType::DsPic33EP => {
                                for &addr in &addrs {
                                    let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                                    let _ = dspic.send_heartbeat();
                                }
                            }
                            _ => {
                                for &addr in &addrs {
                                    let _ = i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                                }
                            }
                        }
                    }

                    if attempt > 0 {
                        // On retry: wait, then reset FPGA state before re-attempting
                        let retry_wait = if attempt <= 2 {
                            ENUM_RETRY_DELAY_SECS
                        } else {
                            4
                        };
                        info!(
                        chain_id = self.chains[chain_idx].chain_id,
                        attempt = attempt + 1,
                        max_attempts = ENUM_MAX_RETRIES + 1,
                        wait_secs = retry_wait,
                        "Enumeration retry {}/{} on chain {} — waiting {}s for ASIC boot to complete...",
                        attempt + 1, ENUM_MAX_RETRIES + 1, self.chains[chain_idx].chain_id, retry_wait,
                    );
                        for _ in 0..retry_wait {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            // Heartbeat during wait
                            let addrs = initialized_pic_addrs
                                .snapshot("enumeration retry heartbeat snapshot");
                            match self.pic_type()? {
                                PicType::DsPic33EP => {
                                    for &addr in &addrs {
                                        let mut dspic = DspicService::new(i2c_svc.clone(), addr);
                                        let _ = dspic.send_heartbeat();
                                    }
                                }
                                _ => {
                                    for &addr in &addrs {
                                        let _ =
                                            i2c_svc.heartbeat(addr, i2c_fw_for(self.pic_firmware));
                                    }
                                }
                            }
                        }

                        let chain = &mut self.chains[chain_idx];
                        if attempt >= 2 {
                            // On attempt 3+: toggle IP core (sends UART BREAK to ASICs,
                            // resetting their internal registers back to defaults).
                            // This mimics BraiinsOS's enter_reset/exit_reset without
                            // touching GPIO or voltage — just the FPGA UART BREAK.
                            info!(
                                chain_id = chain.chain_id,
                                "Retry {}: toggling FPGA IP core (UART BREAK → IDLE reset)",
                                attempt + 1,
                            );
                            chain.fpga.reset_ip_core();
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        // Reset FIFOs and ensure baud is at 115200 for fresh enumeration.
                        // reconfigure() flushes residual high-baud bytes from the previous
                        // session's 3.125 MHz UART state, then sets clean 115200 baud.
                        // 500ms settle gives the FPGA UART state machine time to fully
                        // transition from BREAK to IDLE before we send GetAddress.
                        chain.fpga.reconfigure(
                            ctrl_value_for_reconfigure,
                            dcentrald_hal::fpga_chain::BAUD_REG_115200,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }

                    // v0.15.0: Pause init heartbeats during enumeration (FPGA AXI burst)
                    if let Some(ref pause) = init_hb_pause {
                        pause.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let enum_expected_chips_hint = self.configured_model_chip_count_hint();
                    let enum_default_chips = self.default_chips_per_chain()?;
                    let enum_min_chip_fraction = self.config.mining.min_chip_fraction;
                    let chain = &mut self.chains[chain_idx];
                    match chain.enumerate_chips() {
                        Ok(report) => {
                            let count = report.chip_count();
                            let chip_id = report.chip_id();
                            match report.measured_identity() {
                                Ok(measured) => self.asic_enumeration_receipts.push(
                                    EnumeratedMiningChainReceipt::from_successful_get_address(
                                        chain.chain_id,
                                        measured,
                                    ),
                                ),
                                Err(reasons) => warn!(
                                    chain_id = chain.chain_id,
                                    ?reasons,
                                    "GetAddress preserved mining geometry but is ineligible for Measured ASIC identity"
                                ),
                            }
                            let chip_name = match chip_id {
                                0x1387 => "BM1387 (Antminer S9, 16nm)",
                                0x1397 => "BM1397 (Antminer S17/T17, 7nm)",
                                0x1398 => "BM1398 (Antminer S19/S19j, 7nm)",
                                0x1362 => "BM1362 (Antminer S19j Pro, 5nm)",
                                0x1366 => "BM1366 (Antminer S19 XP, 5nm)",
                                0x1368 => "BM1368 (Antminer S21, 5nm)",
                                0x1370 => "BM1370 (Antminer S21 Pro, 3nm)",
                                _ => "Unknown ASIC chip",
                            };

                            info!(
                            chain_id = chain.chain_id,
                            chip_count = count,
                            chip_id = format_args!("0x{:04X}", chip_id),
                            chip = chip_name,
                            attempt = attempt + 1,
                            "Chain {} enumerated: {} chips detected, ChipID 0x{:04X} = {} (attempt {}/{})",
                            chain.chain_id, count, chip_id, chip_name, attempt + 1, ENUM_MAX_RETRIES + 1,
                        );

                            let expected_chips = enum_expected_chips_hint
                                .or_else(|| {
                                    MinerProfile::for_chip(chip_id).map(|p| p.chips_per_chain)
                                })
                                .unwrap_or(enum_default_chips);
                            let population_fraction = if expected_chips == 0 {
                                1.0
                            } else {
                                count as f32 / expected_chips as f32
                            };
                            if count < expected_chips {
                                warn!(
                                    chain_id = chain.chain_id,
                                    chip_count = count,
                                    expected_chips,
                                    fraction = population_fraction,
                                    event = "enum_shortfall",
                                    "ASIC enumeration shortfall on chain {}: found {} of {} expected chips ({:.3})",
                                    chain.chain_id,
                                    count,
                                    expected_chips,
                                    population_fraction,
                                );
                            }
                            if let Some(min_chip_fraction) = enum_min_chip_fraction {
                                if !chain_meets_min_fraction(
                                    count,
                                    expected_chips,
                                    min_chip_fraction,
                                ) {
                                    chain.mining = false;
                                    error!(
                                        chain_id = chain.chain_id,
                                        chip_count = count,
                                        expected_chips,
                                        fraction = population_fraction,
                                        min_chip_fraction,
                                        "Chain {} below mining.min_chip_fraction floor ({:.3} < {:.3}); Phase 7 will not mine this chain",
                                        chain.chain_id,
                                        population_fraction,
                                        min_chip_fraction,
                                    );
                                }
                            }

                            if self.chip_id == 0 {
                                self.chip_id = chip_id;
                            } else if matches!(
                                driver_for_chain_with_policy(
                                    self.chip_id,
                                    chip_id,
                                    mixed_chip_policy
                                ),
                                ChainDriverDecision::SkipDivergent
                            ) {
                                chain.mining = false;
                                error!(
                                    chain_id = chain.chain_id,
                                    expected = format_args!("0x{:04X}", self.chip_id),
                                    actual = format_args!("0x{:04X}", chip_id),
                                    event = "mixed_chip_id_refused",
                                    "Mixed production chip IDs across chains: chain {} has 0x{:04X} but the latched driver is 0x{:04X}. This chain will not be mined with the wrong driver.",
                                    chain.chain_id,
                                    chip_id,
                                    self.chip_id,
                                );
                            } else if matches!(
                                driver_for_chain_with_policy(
                                    self.chip_id,
                                    chip_id,
                                    mixed_chip_policy
                                ),
                                ChainDriverDecision::LogOnlyDivergent
                            ) {
                                warn!(
                                    chain_id = chain.chain_id,
                                    expected = format_args!("0x{:04X}", self.chip_id),
                                    actual = format_args!("0x{:04X}", chip_id),
                                    event = "mixed_chip_id_log_only",
                                    "Mixed production chip IDs across chains on .25-class XIL: chain {} has 0x{:04X} but the latched driver is 0x{:04X}. Log-only unless mining.enforce_mixed_chip_id_refusal_on_xil25 is enabled.",
                                    chain.chain_id,
                                    chip_id,
                                    self.chip_id,
                                );
                            } else if chip_id != self.chip_id {
                                warn!(
                                chain_id = chain.chain_id,
                                expected = format_args!("0x{:04X}", self.chip_id),
                                actual = format_args!("0x{:04X}", chip_id),
                                "Mixed chip types across chains — chain {} has 0x{:04X} but chain 6 has 0x{:04X}. Using first chain's driver for all. This is unusual but supported by Universal Hash Board Compatibility.",
                                chain.chain_id, chip_id, self.chip_id,
                            );
                            }

                            enumerated = true;
                            break; // Success — no more retries needed for this chain
                        }
                        Err(e) => {
                            if attempt < ENUM_MAX_RETRIES {
                                warn!(
                                    chain_id = chain.chain_id,
                                    error = %e,
                                    attempt = attempt + 1,
                                    max_attempts = ENUM_MAX_RETRIES + 1,
                                    "Enumeration attempt {}/{} failed on chain {} — will retry after delay",
                                    attempt + 1, ENUM_MAX_RETRIES + 1, chain.chain_id,
                                );
                            } else {
                                // Final attempt failed — mark chain as dead
                                chain.chip_count = 0;
                                chain.chip_id = 0;
                                chain.mining = false;
                                error!(
                                    chain_id = chain.chain_id,
                                    error = %e,
                                    attempts = ENUM_MAX_RETRIES + 1,
                                    "Chip enumeration FAILED on chain {} after {} attempts — no chips responded to GetAddress broadcast. Check: hash board power (PSU on?), UART cable, FPGA bitstream, board connector. This chain won't mine.",
                                    chain.chain_id, ENUM_MAX_RETRIES + 1,
                                );
                            }
                        }
                    }
                    // v0.15.0: Resume init heartbeats after enumeration.
                    // 200ms settle for AXI bus after FPGA CMD writes.
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    if let Some(ref pause) = init_hb_pause {
                        pause.store(false, std::sync::atomic::Ordering::Release);
                    }
                } // end retry loop

                if !enumerated {
                    // Already logged as error above
                }
            } // end per-chain loop

            // Select chip driver based on detected ChipID
            if self.chip_id != 0 {
                let composition =
                    self.standard_composition_admission
                        .as_ref()
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                        "standard hardware composition authority disappeared before ASIC adoption"
                    )
                        })?;
                let divergent = self
                    .chains
                    .iter()
                    .filter(|chain| chain.chip_id != 0)
                    .find(|chain| !composition.admits_chip(chain.chip_id))
                    .map(|chain| (chain.chain_id, chain.chip_id));
                if let Some((chain_id, observed_chip_id)) = divergent {
                    for chain in &mut self.chains {
                        chain.mining = false;
                    }
                    anyhow::bail!(
                        "enumerated chain {chain_id} ASIC 0x{observed_chip_id:04X} contradicts admitted {} / {} ASIC 0x{:04X}; revoking the generation before Phase 7 and entering safe teardown",
                        composition.board_target,
                        composition.profile.name,
                        composition.asic.chip_id(),
                    );
                }
                if let Some(driver) = registry.detect(self.chip_id) {
                    info!(
                    chip = driver.chip_name(),
                    chip_id = format_args!("0x{:04X}", self.chip_id),
                    "ChipDriver selected — hash board auto-detected by ChipID (broad Zynq-era support)"
                );
                } else {
                    warn!(
                    chip_id = format_args!("0x{:04X}", self.chip_id),
                    "No built-in driver for ChipID 0x{:04X} — this ASIC type isn't supported yet. Please report this to D-Central!",
                    self.chip_id,
                );
                }

                // Load MinerProfile for detected chip — activates model-specific constants
                self.update_profile()?;
            }
        } // end cold boot block (if !cold_chain_indices.is_empty())

        // ---- Phase 7: Chip configuration ----
        // Now we configure each ASIC chip for mining:
        //   1. Assign unique addresses to each chip on the daisy chain
        //   2. Set the UART baud rate (from 115200 enumeration speed to operational speed)
        //   3. Set the mining frequency (how fast each chip hashes)
        //   4. Configure the TicketMask (hardware difficulty filter)
        info!("--- Phase 7: ASIC Chip Configuration ---");
        let target_freq = self.config.mining.frequency_mhz;
        info!(
            target_freq_mhz = target_freq,
            "Configuring all chips to {} MHz — higher frequency = more hashrate but more power and heat",
            target_freq,
        );

        let registry = ChipRegistry::with_execution_policy(self.asic_driver_execution_policy);
        let admitted_execution_chip_id = self
            .standard_composition_admission
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "standard hardware composition authority disappeared before Phase 7"
                )
            })?
            .asic
            .chip_id();

        // Collect PIC addresses for heartbeat keepalive during Phase 7.
        // Must be a local variable to avoid borrow conflict with &mut self.chains loop.
        let phase7_pic_addrs =
            initialized_pic_addrs.snapshot("phase 7 admitted controller heartbeat snapshot");

        // Convert PicFirmware to I2cPicFirmware for Phase 7 service heartbeats.
        let phase7_i2c_fw = if format!("{}", self.pic_firmware).contains("BraiinsOS") {
            dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs
        } else if format!("{}", self.pic_firmware).contains("Stock") {
            dcentrald_hal::i2c::I2cPicFirmware::Stock
        } else {
            dcentrald_hal::i2c::I2cPicFirmware::Unknown
        };

        // Pre-compute chain→PIC address mapping and pic_type before mutable borrow.
        let chain_pic_map: Vec<(u8, u8)> = self
            .chains
            .iter()
            .map(|c| {
                Ok((
                    c.chain_id,
                    self.pic_addr_for_board(self.board_idx_for_chain(c.chain_id)?)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let phase7_pic_type = self.pic_type()?;
        let assumed_chip_id = self.chip_id;
        let assumed_chips = dcentrald_asic::drivers::MinerProfile::for_chip(assumed_chip_id)
            .map(|p| p.chips_per_chain)
            .unwrap_or(0);
        let min_chip_fraction = self.config.mining.min_chip_fraction;
        let configured_chip_count_hint = self.configured_model_chip_count_hint();

        let mut controller_admitted_chain_indices = hot_controller_admitted_chain_indices;
        for &chain_idx in &cold_chain_indices {
            let board_idx = self.board_idx_for_chain(self.chains[chain_idx].chain_id)?;
            if cold_pic_admitted_board_indices.contains(&board_idx)
                && !controller_admitted_chain_indices.contains(&chain_idx)
            {
                controller_admitted_chain_indices.push(chain_idx);
            }
        }

        for (chain_idx, chain) in self.chains.iter_mut().enumerate() {
            if !controller_admitted_chain_indices.contains(&chain_idx) {
                chain.mining = false;
                warn!(
                    chain_id = chain.chain_id,
                    chain_index = chain_idx,
                    "Phase 7: controller admission is absent; chain remains disabled"
                );
                continue;
            }
            // For am2 passthrough: if enumeration failed but model hint is set,
            // use assumed chip_id instead of skipping. Bosminer already configured
            // the ASICs — we just need to mark them as mining.
            if chain.chip_id == 0 {
                if self.config.mining.model.is_some() && assumed_chip_id != 0 {
                    chain.chip_id = assumed_chip_id;
                    chain.chip_count = assumed_chips;
                    chain
                        .admit_address_assignment_for_current_identity()
                        .with_context(|| {
                            format!(
                                "Phase 7 model-hint chain {} lacks an admitted address composition",
                                chain.chain_id
                            )
                        })?;
                    tracing::info!(
                        chain_id = chain.chain_id,
                        chip_id = format_args!("0x{:04X}", chain.chip_id),
                        chips = chain.chip_count,
                        "Phase 7: using model hint for unenumerated chain during passthrough handoff"
                    );
                } else {
                    continue;
                }
            }

            if chain.chip_id != admitted_execution_chip_id {
                chain.mining = false;
                error!(
                    chain_id = chain.chain_id,
                    admitted_chip_id = format_args!("0x{admitted_execution_chip_id:04X}"),
                    observed_chip_id = format_args!("0x{:04X}", chain.chip_id),
                    "Phase 7: chain identity contradicts the sealed standard composition; chain remains disabled"
                );
                continue;
            }

            match driver_for_chain_with_policy(assumed_chip_id, chain.chip_id, mixed_chip_policy) {
                ChainDriverDecision::SkipDivergent => {
                    chain.mining = false;
                    error!(
                        chain_id = chain.chain_id,
                        expected = format_args!("0x{:04X}", assumed_chip_id),
                        actual = format_args!("0x{:04X}", chain.chip_id),
                        event = "mixed_chip_id_phase7_skip",
                        "Phase 7: skipping chain {} with divergent production chip ID 0x{:04X}; latched driver is 0x{:04X}",
                        chain.chain_id,
                        chain.chip_id,
                        assumed_chip_id,
                    );
                    continue;
                }
                ChainDriverDecision::LogOnlyDivergent => {
                    warn!(
                        chain_id = chain.chain_id,
                        expected = format_args!("0x{:04X}", assumed_chip_id),
                        actual = format_args!("0x{:04X}", chain.chip_id),
                        event = "mixed_chip_id_phase7_log_only",
                        "Phase 7: .25-class XIL log-only mixed-chip policy keeps chain {} eligible despite 0x{:04X} vs latched 0x{:04X}",
                        chain.chain_id,
                        chain.chip_id,
                        assumed_chip_id,
                    );
                }
                ChainDriverDecision::Drive => {}
            }

            if let Some(min_chip_fraction) = min_chip_fraction {
                let expected_chips = configured_chip_count_hint
                    .or_else(|| MinerProfile::for_chip(chain.chip_id).map(|p| p.chips_per_chain))
                    .unwrap_or(assumed_chips);
                if !chain_meets_min_fraction(chain.chip_count, expected_chips, min_chip_fraction) {
                    let population_fraction = if expected_chips == 0 {
                        1.0
                    } else {
                        chain.chip_count as f32 / expected_chips as f32
                    };
                    chain.mining = false;
                    error!(
                        chain_id = chain.chain_id,
                        chip_count = chain.chip_count,
                        expected_chips,
                        fraction = population_fraction,
                        min_chip_fraction,
                        "Phase 7: skipping chain {} below mining.min_chip_fraction floor ({:.3} < {:.3})",
                        chain.chain_id,
                        population_fraction,
                        min_chip_fraction,
                    );
                    continue;
                }
            }

            // Send PIC heartbeats before each chain's configuration.
            // Phase 7 runs assign_addresses() which blocks for ~615ms per chain
            // (3x100ms ChainInactive + 63x5ms SetChipAddress). With 3 chains that's
            // ~1.85s total blocking time. Stock Bitmain PIC watchdog is only ~5s,
            if let Some(driver) = registry.detect(chain.chip_id) {
                let passthrough = self.config.mining.passthrough;

                // Phase 7 passthrough: skip init_chain if config says passthrough=true.
                // am2 models: DO NOT force passthrough. 9 passthrough tests all produce
                // zero nonces. init_chain MUST run to activate ASIC cores.
                // The 25s timeout from earlier was caused by init_chain's baud upgrade
                // (Steps 11-12). We now skip baud change in init_chain when FPGA is
                // already at operational baud (BAUD != 0x6C).
                let phase7_passthrough = passthrough;

                if !phase7_passthrough {
                    // Cold-init chains still need keepalive before long FPGA bursts.
                    for &addr in &phase7_pic_addrs {
                        let _ = i2c_svc.heartbeat(addr, phase7_i2c_fw);
                    }
                }

                if phase7_passthrough {
                    // Passthrough mode: skip ASIC register writes entirely.
                    // Bosminer already configured ASICs (PLL, MiscCtrl, baud, TicketMask,
                    // open-core). Just preserve bosminer's existing FPGA+ASIC state.
                    let work_time = chain
                        .fpga
                        .common
                        .read_reg(dcentrald_hal::fpga_chain::REG_WORK_TIME);
                    info!(
                        chain_id = chain.chain_id,
                        work_time = format_args!("0x{:08X}", work_time),
                        "PASSTHROUGH: Keeping inherited ASIC config (PLL, baud, TicketMask, cores). \
                         WORK_TIME=0x{:08X}. Skipping init_chain + open-core.",
                        work_time,
                    );
                } else {
                    // Full init mode: configure ASICs from scratch.

                    // v0.15.0: PAUSE init heartbeats during FPGA operations.
                    // Open-core writes 114 work items (4104 AXI writes) which saturates
                    // the AXI bus and corrupts concurrent I2C transactions, permanently
                    // wedging PIC MSSP modules. Proven by INIT_HB diagnostic 2026-04-05.
                    if let Some(ref pause) = init_hb_pause {
                        pause.store(true, std::sync::atomic::Ordering::Release);
                    }

                    // Step 1: Assign addresses
                    info!(
                        chain_id = chain.chain_id,
                        chip_count = chain.chip_count,
                        "Assigning addresses to {} chips on chain {} (addresses spaced evenly across 0x00-0xFF)",
                        chain.chip_count, chain.chain_id,
                    );
                    if let Err(e) = chain.assign_addresses() {
                        error!(
                            chain_id = chain.chain_id,
                            error = %e,
                            "Address assignment FAILED on chain {} — chips aren't responding to Chain Inactive command",
                            chain.chain_id,
                        );
                        if let Some(ref pause) = init_hb_pause {
                            pause.store(false, std::sync::atomic::Ordering::Release);
                        }
                        continue;
                    }

                    // Board relay state belongs to the admitted composition,
                    // not to the ASIC family. NBP1901/BM1398 consumes the exact
                    // twelve addressed register-0x2c writes recovered from the
                    // stock binary; BM1397 remains fail-closed until its board
                    // topology provides an equally explicit recipe.
                    match chain.apply_admitted_board_relay() {
                        Ok(0) => {}
                        Ok(relay_writes) => info!(
                            chain_id = chain.chain_id,
                            relay_writes, "Applied admitted board relay composition"
                        ),
                        Err(e) => {
                            error!(
                                chain_id = chain.chain_id,
                                error = %e,
                                "Board relay admission FAILED; refusing chip initialization"
                            );
                            if let Some(ref pause) = init_hb_pause {
                                pause.store(false, std::sync::atomic::Ordering::Release);
                            }
                            continue;
                        }
                    }

                    // Step 2: Initialize chain (PLL → MiscCtrl+gate_block → WORK_TIME → baud upgrade → TicketMask)
                    if let Err(e) = chain.init_with_driver(driver, target_freq) {
                        error!(
                            chain_id = chain.chain_id,
                            error = %e,
                            "Driver init FAILED on chain {} — chip-specific initialization sequence failed",
                            chain.chain_id,
                        );
                        if let Some(ref pause) = init_hb_pause {
                            pause.store(false, std::sync::atomic::Ordering::Release);
                        }
                        continue;
                    }

                    // Step 3: Open-core — activate all 114 SHA-256 cores per chip
                    info!(
                        chain_id = chain.chain_id,
                        "Sending open-core init work to activate SHA-256 cores",
                    );
                    match driver.send_open_core_work(&mut chain.fpga, chain.chip_count) {
                        Ok(init_nonces) => {
                            info!(
                                chain_id = chain.chain_id,
                                init_nonces,
                                "Open-core complete: {} init nonces received — all cores active",
                                init_nonces,
                            );
                        }
                        Err(e) => {
                            error!(
                                chain_id = chain.chain_id,
                                error = %e,
                                "Open-core FAILED on chain {} — cores may not be active",
                                chain.chain_id,
                            );
                        }
                    }
                }

                // Compute pic_addr early — needed for post-open-core recovery AND
                // voltage reduction below.
                let pic_addr = chain_pic_map
                    .iter()
                    .find(|(cid, _)| *cid == chain.chain_id)
                    .map(|(_, addr)| *addr)
                    .unwrap_or(0x55);

                // Post-open-core I2C bus recovery: FPGA UART activity during open-core
                // (114 work items at 1.5 MHz baud) can corrupt PIC MSSP parser state.
                // Recovery strategy: bus_recovery (9 SCL clocks) to unstick PIC MSSP,
                // then heartbeat ALL initialized PICs (not just current chain).
                // NO SOFTR — it resets the AXI IIC state machine which can make things
                // worse if a PIC is holding SDA during an incomplete transaction.
                info!(
                    chain_id = chain.chain_id,
                    pic_addr = format_args!("0x{:02X}", pic_addr),
                    phase7_passthrough,
                    use_devmem_i2c,
                    "POST_OPENCORE_GUARD: checking recovery conditions",
                );
                if !phase7_passthrough && use_devmem_i2c {
                    // Flush ALL chains' WORK_TX FIFOs first — other chains' FPGA UART
                    // from their earlier open-core causes I2C crosstalk on the shared
                    // ribbon cable (UART pins 11,12 couple into I2C pins 3,4).
                    dcentrald_hal::fpga_chain::flush_all_work_tx_devmem();
                    std::thread::sleep(std::time::Duration::from_millis(500));

                    if let Err(error) = i2c_svc.recover_unmanaged_bus() {
                        warn!(
                            chain_id = chain.chain_id,
                            %error,
                            "Post-open-core whole-fabric recovery was refused or failed"
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));

                    let addrs =
                        initialized_pic_addrs.snapshot("phase 7 I2C recovery heartbeat snapshot");
                    for &addr in &addrs {
                        let mut recovered = false;
                        for attempt in 0..3u8 {
                            if i2c_svc.heartbeat(addr, phase7_i2c_fw).is_ok() {
                                info!(
                                    chain_id = chain.chain_id,
                                    attempt,
                                    pic_addr = format_args!("0x{:02X}", addr),
                                    "Post-open-core PIC recovery OK",
                                );
                                recovered = true;
                                break;
                            }
                            warn!(
                                chain_id = chain.chain_id,
                                attempt,
                                pic_addr = format_args!("0x{:02X}", addr),
                                "Post-open-core PIC NACK — bus recovery + retry",
                            );
                            if let Err(error) = i2c_svc.recover_unmanaged_bus() {
                                warn!(
                                    chain_id = chain.chain_id,
                                    attempt,
                                    %error,
                                    "Post-open-core retry recovery was refused or failed"
                                );
                            }
                            std::thread::sleep(std::time::Duration::from_millis(
                                100 * (attempt as u64 + 1),
                            ));
                        }
                        if !recovered {
                            error!(
                                chain_id = chain.chain_id,
                                pic_addr = format_args!("0x{:02X}", addr),
                                "Post-open-core PIC 0x{:02X} UNRECOVERABLE after 3 attempts",
                                addr,
                            );
                        }
                    }
                }

                chain.frequency_mhz = target_freq;
                chain.mining = true;

                // LED: flash green N times when chain N comes online during init
                if let Some(ref led) = self.led_tx {
                    let _ = led.try_send(LedCommand::ChainOnline(chain.chain_id));
                }

                // Step 4: Reduce voltage from init to configured operating voltage.
                // Running at init voltage wastes power. The configured voltage
                // (default 8600 mV for S9, 13800 mV for S19) is safe for typical frequencies.
                //
                // v0.15.1: Heartbeats still PAUSED here. The voltage reduce is an
                // I2C operation that must succeed. We resume heartbeats AFTER this
                // completes, so only ONE thread touches I2C at a time.
                let mut target_voltage_mv = self.config.mining.voltage_mv;
                // FIX (2026-04-13, swarm #2): Validate voltage against platform range.
                // Default config has voltage_mv=9100 (S9). On dsPIC platforms (S19 Pro),
                // sending 9100mV is below the operating range (12000-15000mV).
                // Use MinerProfile default if config value is inappropriate for dsPIC.
                if matches!(phase7_pic_type, PicType::DsPic33EP) {
                    let profile_mv = self
                        .miner_profile
                        .map(|p| p.default_voltage_mv)
                        .unwrap_or(13800);
                    if target_voltage_mv == 0 || target_voltage_mv < 10000 {
                        warn!(
                            config_mv = target_voltage_mv,
                            profile_mv = profile_mv,
                            "Config voltage_mv={} is below dsPIC range — using profile default {} mV",
                            target_voltage_mv, profile_mv,
                        );
                        target_voltage_mv = profile_mv;
                    }
                }

                if phase7_passthrough {
                    chain.voltage_mv = target_voltage_mv;
                    info!(
                        chain_id = chain.chain_id,
                        voltage_mv = target_voltage_mv,
                        "PASSTHROUGH: preserving donor voltage state — skipping phase7 voltage write"
                    );
                } else {
                    // Routed through I2C service to prevent concurrent fd access.
                    match phase7_pic_type {
                        PicType::DsPic33EP => {
                            // dsPIC: send set_voltage command with millivolt value via raw I2C
                            // Wire format: [0x55, 0xAA, CMD_SET_VOLTAGE(0x10), voltage_hi, voltage_lo]
                            let mv = target_voltage_mv;
                            let cmd = [0x55, 0xAA, 0x10, (mv >> 8) as u8, (mv & 0xFF) as u8];
                            match i2c_svc.write_bytes(pic_addr, &cmd) {
                                Ok(()) => {
                                    chain.voltage_mv = target_voltage_mv;
                                    info!(
                                        chain_id = chain.chain_id,
                                        voltage_mv = target_voltage_mv,
                                        "dsPIC voltage reduced to {} mV",
                                        target_voltage_mv,
                                    );
                                }
                                Err(e) => {
                                    // Non-fatal: continue mining at init voltage
                                    let init_mv = self
                                        .miner_profile
                                        .map(|p| p.default_voltage_mv)
                                        .unwrap_or(13800);
                                    chain.voltage_mv = init_mv;
                                    warn!(
                                        chain_id = chain.chain_id,
                                        error = %e,
                                        "dsPIC failed to reduce voltage — running at init {} mV",
                                        init_mv,
                                    );
                                }
                            }
                        }
                        _ => {
                            // Defer voltage reduction to the mining phase.
                            // Post-open-core ASIC UART noise on hash board pins 11/12
                            // couples into I2C SDA/SCL on pins 3/4, causing PIC NACKs
                            // on 4-byte writes (SET_VOLTAGE). 3-byte heartbeats survive
                            // because bus_recovery's SCL clocks can unstick them, but
                            // the longer voltage command fails consistently.
                            // The runtime heartbeat thread has a quiet-window mechanism
                            // that pauses FPGA work dispatch before I2C — voltage
                            // reduction will succeed there.
                            chain.voltage_mv = 9400;
                            info!(
                                chain_id = chain.chain_id,
                                target_mv = target_voltage_mv,
                                "Voltage reduction DEFERRED to mining phase quiet window",
                            );
                        }
                    }
                }

                // Estimated hashrate = chips * cores_per_chip * frequency_mhz * 1e6 / 1e9 (GH/s)
                // For BM1387: cores_per_chip ~= 114 (based on ~14 TH/s with 189 chips at 650 MHz)
                let est_hashrate_ghs =
                    chain.chip_count as f64 * driver.cores_per_chip() as f64 * target_freq as f64
                        / 1000.0;
                let est_hashrate_ths = est_hashrate_ghs / 1000.0;
                info!(
                    chain_id = chain.chain_id,
                    chips = chain.chip_count,
                    chip = driver.chip_name(),
                    freq_mhz = target_freq,
                    est_hashrate = format_args!(
                        "{:.1} GH/s ({:.2} TH/s)",
                        est_hashrate_ghs, est_hashrate_ths
                    ),
                    "Chain {} READY — {} x {} chips at {} MHz (~{:.1} GH/s)",
                    chain.chain_id,
                    chain.chip_count,
                    driver.chip_name(),
                    target_freq,
                    est_hashrate_ghs,
                );

                // v0.16.0: Resume heartbeats + explicit keepalive to ALL PICs.
                // This is the safe gap between chains. The FPGA burst for this
                // chain is done. The next chain's FPGA burst hasn't started yet.
                // We MUST heartbeat ALL initialized PICs here because once the
                // next chain's open-core starts, AXI contention will prevent I2C.
                if !phase7_passthrough {
                    dcentrald_hal::fpga_chain::flush_all_work_tx_devmem();
                    if let Some(ref pause) = init_hb_pause {
                        pause.store(false, std::sync::atomic::Ordering::Release);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    {
                        let addrs =
                            initialized_pic_addrs.snapshot("phase 7 FPGA gap heartbeat snapshot");
                        for &addr in &addrs {
                            match i2c_svc.heartbeat(addr, phase7_i2c_fw) {
                                Ok(()) => info!("PHASE7_GAP_HB: PIC 0x{:02X} OK", addr),
                                Err(e) => warn!("PHASE7_GAP_HB: PIC 0x{:02X} FAIL: {}", addr, e),
                            }
                        }
                    }
                }
            } else {
                // Driver selection is an execution authority, not merely a
                // display lookup.  A chain can arrive here with inherited
                // `mining=true` state (notably a passthrough/hot handoff), so
                // absence of an admitted executable driver must actively
                // revoke that state.  Leaving the old value untouched would
                // let a recognized-but-unadmitted or unknown ASIC cross the
                // WorkDispatcher hardware-write boundary.
                chain.mining = false;
                error!(
                    chain_id = chain.chain_id,
                    chip_id = format_args!("0x{:04X}", chain.chip_id),
                    "Phase 7: no executable ASIC driver is admitted; chain remains disabled"
                );
            }
        }

        // Defensive: ensure heartbeats are NEVER left paused after chain loop exits,
        // regardless of which path was taken (success, error, or skip).
        if let Some(ref pause) = init_hb_pause {
            pause.store(false, std::sync::atomic::Ordering::Release);
        }

        let active_chains: usize = self.chains.iter().filter(|c| c.mining).count();
        let total_chips: u16 = self
            .chains
            .iter()
            .filter(|c| c.mining)
            .map(|c| c.chip_count as u16)
            .sum();

        // Fix: use chip_id from first active mining chain, not the global one.
        // Phase 4c may detect noise on empty chains (e.g., chip_id=0xFF57), which
        // poisons self.chip_id. The work dispatcher needs the REAL chip_id to look
        // up the correct driver for send_work/decode_nonce.
        if let Some(active) = self.chains.iter().find(|c| c.mining) {
            if self.chip_id != active.chip_id {
                info!(
                    old_chip_id = format_args!("0x{:04X}", self.chip_id),
                    real_chip_id = format_args!("0x{:04X}", active.chip_id),
                    "Correcting chip_id from noise (0x{:04X}) to real mining chip (0x{:04X})",
                    self.chip_id,
                    active.chip_id,
                );
                self.chip_id = active.chip_id;
            }
        }

        // DO NOT stop the init heartbeat thread here — return it to run() so the
        // mining heartbeat thread can start FIRST, closing the gap where no heartbeats
        // flow during API/Stratum setup. The init thread keeps PICs alive until the
        // mining heartbeat takes over seamlessly.

        // Fix G: Store the final list of successfully-initialized PIC addresses.
        // Only these PICs get heartbeats during mining — dead PICs are excluded.
        self.initialized_pic_addrs_final =
            initialized_pic_addrs.snapshot("mining heartbeat ownership handoff");
        info!(
            pic_count = self.initialized_pic_addrs_final.len(),
            addrs = %self.initialized_pic_addrs_final.iter()
                .map(|a| format!("0x{:02X}", a))
                .collect::<Vec<_>>()
                .join(", "),
            "Initialized PIC list for mining heartbeat: {} PIC(s)",
            self.initialized_pic_addrs_final.len(),
        );

        info!("=== HARDWARE INIT COMPLETE ===");
        info!(
            active_chains,
            total_chips,
            frequency_mhz = target_freq,
            "{} chain(s) with {} total ASIC chips at {} MHz — ready to connect to pool and start mining!",
            active_chains, total_chips, target_freq,
        );

        // Successful completion means slot presence, controller endpoints, and
        // every hardware actor needed by shutdown are now known. Only this
        // receipt may clear the conservative state installed at init entry.
        self.preflight_hardware_state_unknown = false;

        // Ownership stays on `self` through the complete fallible runtime
        // heartbeat handoff. Error, cancellation, shutdown, and Drop therefore
        // retain the authority required to assert the stop flag.
        Ok(())
    }

    /// Start a background heartbeat thread that keeps PICs alive during init.
    ///
    /// Returns stop/pause flags plus the join handle. The daemon owns the stop
    /// authority and uses `stop_thread_bounded`; callers must not perform an
    /// unbounded raw join during lifecycle handoff or teardown.
    ///
    /// The PIC address list uses poison-aware shared ownership so that the main
    /// init task can ADD newly-initialized cold boot PICs to the list at runtime.
    /// The thread picks up new addresses on its next 500ms tick automatically.
    ///
    /// CE REVIEW FIX: Previously used a static Vec snapshot — cold boot PICs
    /// initialized after thread start never got heartbeats, causing watchdog death.
    /// v0.15.0: Returns (stop_flag, pause_flag, join_handle).
    /// Set pause=true during FPGA bursts (open-core, enumeration) to prevent
    /// AXI contention from corrupting I2C transactions and permanently wedging PICs.
    fn start_init_heartbeat_thread(
        pic_addrs: InitializedPicAddrs,
        firmware: PicFirmware,
        pic_type: PicType,
        i2c_service: dcentrald_hal::i2c::I2cServiceHandle,
    ) -> std::io::Result<(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    )> {
        use std::sync::atomic::{AtomicBool, Ordering};

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let pause = Arc::new(AtomicBool::new(false));
        let pause_clone = pause.clone();
        let init_i2c_service = i2c_service.clone();
        let init_i2c_fw = match firmware {
            PicFirmware::BraiinsOs => dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs,
            PicFirmware::Stock(_) => dcentrald_hal::i2c::I2cPicFirmware::Stock,
            PicFirmware::Unknown => dcentrald_hal::i2c::I2cPicFirmware::Unknown,
        };

        let handle = std::thread::Builder::new()
            .name("pic-heartbeat-init".to_string())
            .spawn(move || {
                info!(
                    firmware = %firmware,
                    "Init heartbeat thread started — 500ms interval, dynamic PIC list, FPGA-pause aware",
                );
                while !stop_clone.load(Ordering::Relaxed) {
                    // v0.15.0: Skip I2C during FPGA bursts (open-core, enumeration).
                    // AXI bus contention from WORK_TX writes corrupts I2C transactions
                    // and permanently wedges PIC MSSP modules.
                    if pause_clone.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }

                    let addrs = pic_addrs.snapshot("init heartbeat worker snapshot");

                    if !addrs.is_empty() {
                        if matches!(pic_type, PicType::Pic16F1704) {
                            for &addr in &addrs {
                                if stop_clone.load(Ordering::Acquire)
                                    || pause_clone.load(Ordering::Acquire)
                                {
                                    break;
                                }
                                let hb_t0 = std::time::Instant::now();
                                if let Err(e) = init_i2c_service.heartbeat(addr, init_i2c_fw) {
                                    let hb_us = hb_t0.elapsed().as_micros();
                                    tracing::warn!(
                                        addr = format_args!("0x{:02X}", addr),
                                        error = %e,
                                        us = hb_us,
                                        "INIT_HB_FAIL: PIC 0x{:02X} us={}", addr, hb_us,
                                    );
                                } else {
                                    let hb_us = hb_t0.elapsed().as_micros();
                                    tracing::info!(
                                        addr = format_args!("0x{:02X}", addr),
                                        us = hb_us,
                                        "INIT_HB_OK: PIC 0x{:02X} us={}", addr, hb_us,
                                    );
                                }
                                if stop_clone.load(Ordering::Acquire) {
                                    break;
                                }
                            }
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }

                        if stop_clone.load(Ordering::Acquire) {
                            break;
                        }
                        let _ = init_i2c_service.set_timeout(10); // 100ms timeout
                        if stop_clone.load(Ordering::Acquire) {
                            break;
                        }
                        for &addr in &addrs {
                            // Re-check stop and pause before each PIC (either may
                            // change while the previous service call is blocked).
                            if stop_clone.load(Ordering::Acquire)
                                || pause_clone.load(Ordering::Acquire)
                            {
                                break;
                            }
                            let hb_t0 = std::time::Instant::now();
                            let result = match pic_type {
                                PicType::DsPic33EP => {
                                    let mut dspic = DspicService::new(init_i2c_service.clone(), addr);
                                    dspic.send_heartbeat().map_err(|e| e.to_string())
                                }
                                _ => init_i2c_service
                                    .heartbeat(addr, init_i2c_fw)
                                    .map_err(|e| e.to_string()),
                            };
                            let hb_us = hb_t0.elapsed().as_micros();
                            if let Err(e) = result {
                                tracing::warn!(
                                    addr = format_args!("0x{:02X}", addr),
                                    error = %e,
                                    us = hb_us,
                                    "INIT_HB_FAIL: PIC 0x{:02X} us={}", addr, hb_us,
                                );
                            } else {
                                tracing::info!(
                                    addr = format_args!("0x{:02X}", addr),
                                    us = hb_us,
                                    "INIT_HB_OK: PIC 0x{:02X} us={}", addr, hb_us,
                                );
                            }
                            if stop_clone.load(Ordering::Acquire) {
                                break;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                info!("Init heartbeat thread stopped");
            })?; // G46: propagate the spawn error instead of panic!(...). A panic
                 // under panic=abort skips every Drop guard; the single caller
                 // handles Err by proceeding without the init HB (degraded but safe
                 // — PIC watchdog cuts voltage if unfed) so shutdown guards still run.

        Ok((stop, pause, handle))
    }

    /// Signal an initialization heartbeat to stop without waiting for it.
    ///
    /// This is deliberately called before partial-init teardown after an
    /// initialization error or timeout. If teardown itself wedges on the same
    /// I2C service, the init thread must not keep feeding the PIC indefinitely;
    /// stopping heartbeats lets the hardware watchdog cut the rail.
    fn signal_init_heartbeat_stop(&self) {
        if let Some(ref stop) = self.init_heartbeat_stop {
            stop.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// Stop and bounded-join any initialization heartbeat still owned by the
    /// daemon. A worker blocked in a kernel I2C call cannot be killed safely;
    /// after the bound expires its join handle is detached, but the stop flag
    /// remains asserted so it cannot emit another heartbeat if the call returns.
    async fn stop_init_heartbeat_bounded(&mut self) -> bool {
        let stop = self.init_heartbeat_stop.take();
        let handle = self.init_heartbeat_handle.take();
        match (stop, handle) {
            (Some(stop), Some(handle)) => {
                match stop_thread_bounded(stop, handle, Duration::from_secs(2)).await {
                    ThreadStopOutcome::Joined => {
                        info!("Initialization heartbeat thread stopped and joined");
                        true
                    }
                    ThreadStopOutcome::Panicked => {
                        error!("Initialization heartbeat thread panicked while stopping");
                        true
                    }
                    ThreadStopOutcome::TimedOut => {
                        warn!(
                            "Initialization heartbeat thread did not exit within 2s; \
                             stop remains asserted and the handle was detached so shutdown \
                             can continue toward the hardware-watchdog safety path"
                        );
                        false
                    }
                }
            }
            (Some(stop), None) => {
                stop.store(true, std::sync::atomic::Ordering::Release);
                true
            }
            (None, Some(handle)) => {
                // Defensive impossible-state handling: never block shutdown on
                // a handle for which the stop authority was lost.
                warn!(
                    finished = handle.is_finished(),
                    "Initialization heartbeat handle existed without its stop flag; detaching"
                );
                false
            }
            (None, None) => true,
        }
    }

    /// Graceful shutdown sequence.
    ///
    /// During normal runtime shutdown, heartbeats keep running until the voltage
    /// disable attempt completes. During partial-init failure, the initialization
    /// heartbeat is deliberately stopped before teardown so the hardware watchdog
    /// can cut power even if the same I2C service wedges the safe-off request.
    ///
    /// Sequence:
    /// 1. Cancel all Tokio tasks via CancellationToken (heartbeat thread still runs)
    /// 2. Stop submitting new work
    /// 3. Wait 500ms for in-flight nonces
    /// 4. Submit any remaining valid shares
    /// 5a. Disable hash board voltages (PIC ENABLE_VOLTAGE = 0)
    /// 5b. Stop heartbeat thread (voltage is off, PIC watchdog no longer matters)
    /// 6. Wait 2 seconds for power discharge
    /// 7. Ramp fans to the configured cool-down envelope
    /// 8. Wait 5 seconds
    /// 9. Set fans to minimum
    /// 10. Close watchdog (write "V" then close fd)
    /// 11. Log "dcentrald stopped cleanly"
    async fn shutdown(&mut self) -> Result<()> {
        if std::mem::replace(&mut self.shutdown_attempted, true) {
            anyhow::bail!(
                "shutdown was already attempted; refusing to consume hardware ownership or extend watchdog teardown grace twice"
            );
        }
        // Identity authorization ends at shutdown admission, before any
        // potentially slow or degraded hardware teardown. The dispatcher's
        // owned session may still be unwinding; its later stale-token Drop is
        // deliberately a no-op.
        if let Err(error) = self.dispatcher_composition_authority.invalidate_active() {
            warn!(
                error = %error,
                "Could not revoke the active dispatcher composition at shutdown start"
            );
        }
        // Thermal liveness will intentionally stop below. Move the independent
        // watchdog into an absolute-deadline teardown grace first, so a healthy
        // bounded safe-off keeps receiving kicks while a wedged safe-off still
        // ends in a hardware reset. This is not disarm authority.
        if let Some(intent_tx) = self.watchdog_intent_tx.as_ref() {
            let deadline = tokio::time::Instant::now() + WATCHDOG_TEARDOWN_GRACE;
            if intent_tx
                .send(WatchdogIntent::Teardown { deadline })
                .is_err()
            {
                warn!("SoC watchdog task was unavailable at teardown admission; final receipt will determine whether it was never armed or failed closed");
            }
        }
        self.mining_tasks.request_stop();
        // Close mutation admission after requesting mining-task cancellation.
        // A delayed or later-aborted dispatcher/thermal task cannot enqueue
        // another SetVoltage while shutdown waits for task termination.
        if let Some(i2c_service) = self.i2c_service.as_ref() {
            let transition = i2c_service.latch_terminal_safe_off();
            info!(
                safety_generation = transition.generation(),
                no_controller_mutation_stage_in_flight =
                    transition.no_controller_mutation_stage_in_flight(),
                "Latched terminal I2C safe-off barrier; this is software-stage evidence, not physical rail-off evidence"
            );
        }
        if let Some(voltage_mailbox) = self.voltage_cmd_tx.as_ref() {
            voltage_mailbox.latch_terminal();
        }
        // Quiesce every asynchronous mining hardware owner before voltage or
        // controller teardown. This uses a standalone mining-only token, so
        // global signal cancellation cannot preempt watchdog teardown admission
        // and the API remains available for recovery. The watchdog kicker
        // remains outside this group so stalled liveness still causes reboot.
        let mining_stop = self
            .mining_tasks
            .stop_and_join(MINING_TASK_STOP_TIMEOUT)
            .await;
        let mining_quiescence_failed = mining_stop.any_timed_out();
        if mining_quiescence_failed {
            error!(
                timeout_ms = MINING_TASK_STOP_TIMEOUT.as_millis(),
                "Mining hardware task did not terminate after cancellation and abort; continuing fail-safe voltage teardown"
            );
        } else if mining_stop.any_panicked() {
            warn!(
                "Mining hardware task panicked; task ownership was reclaimed before hardware teardown"
            );
        }
        // The legacy smart-PSU feeder owns a raw bus-1 transport. Reclaim it
        // before latching terminal state or touching any shared shutdown
        // hardware. A timed-out feeder may still be inside an ioctl while
        // holding the API serialization lock, so that path must never re-enter
        // PsuController. Use the GPIO output gate only with an authoritative
        // am2-s17 board target; on every other board, stop feeding and preserve
        // the already-armed PSU watchdog as the transport-independent fallback.
        let mut psu_quiescence_failed = false;
        if self.psu_watchdog_threads.contains("psu-watchdog") {
            let psu_stop = self
                .psu_watchdog_threads
                .stop_and_join(PSU_WATCHDOG_THREAD_STOP_TIMEOUT)
                .await;
            if psu_stop.any_timed_out() {
                psu_quiescence_failed = true;
                let control_board = self.platform_identity.observed_control_board.as_str();
                let board_target = self.platform_identity.board_target();
                if legacy_kernel_smart_psu_path_allowed(
                    self.config.mining.model.as_deref(),
                    board_target,
                    false,
                ) {
                    match dcentrald_hal::platform::zynq::disable_psu_output() {
                        Ok(()) => error!(
                            control_board,
                            board_target,
                            feeder_timed_out = true,
                            hard_cut_asserted = true,
                            watchdog_fallback = true,
                            "PSU watchdog feeder did not quiesce; asserted the proven am2-s17 GPIO output hard cut without re-entering its transport"
                        ),
                        Err(cut_error) => error!(
                            control_board,
                            board_target,
                            error = %cut_error,
                            feeder_timed_out = true,
                            hard_cut_asserted = false,
                            watchdog_fallback = true,
                            "PSU watchdog feeder did not quiesce and the proven GPIO hard cut failed; the armed PSU watchdog is the remaining power-cut fallback"
                        ),
                    }
                } else {
                    error!(
                        control_board,
                        board_target,
                        feeder_timed_out = true,
                        hard_cut_available = false,
                        watchdog_fallback = true,
                        "PSU watchdog feeder did not quiesce; no platform-proven transport-independent hard cut exists, so shutdown will not re-enter the possibly held PSU transport"
                    );
                }
            } else if psu_stop.any_panicked() {
                warn!(
                    watchdog_fallback = true,
                    "PSU watchdog feeder panicked before shutdown; its transport is quiescent and the armed watchdog remains the fallback"
                );
            }
        }

        info!("=== GRACEFUL SHUTDOWN SEQUENCE ===");
        info!(
            "Attempting software safe-off; the PIC/dsPIC watchdog remains the independent power-cut safety net"
        );

        // prod-readiness hunt #1 (log-honesty): track whether SOFTWARE actually
        // completed every software disable command. This is command-delivery
        // evidence, not measured rail-off evidence. Every Step-5a/5b failure
        // branch sets this true so the final log distinguishes a completed write
        // from exclusive reliance on the ~5-64 s PIC/dsPIC watchdog.
        let mut software_disable_failed =
            self.preflight_hardware_state_unknown || mining_quiescence_failed;
        if self.preflight_hardware_state_unknown {
            warn!(
                "Preflight terminated hardware bring-up before controller/slot state was fully discovered; shutdown cannot attest that all rails were de-energized by software"
            );
        }

        // Step 1-2: Stop work submission (CancellationToken already cancelled)
        // A runtime heartbeat uses heartbeat_shutdown_token and continues here.
        // On partial-init failure, the init heartbeat may already be stopping so
        // the controller watchdog can expire if this teardown wedges.
        info!("Step 1-2: Stopping work submission — no new jobs will be sent to ASICs");
        info!("  (runtime heartbeat remains active when available; init-failure teardown may already rely on controller watchdog cutoff)");

        // Step 3: Wait for in-flight nonces to arrive
        info!("Step 3: Waiting 500ms for any in-flight nonces to arrive from FPGA FIFOs...");
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Step 5a: Disable hash board voltages via PIC
        // This is a bounded best-effort command. A stopped init heartbeat is an
        // intentional fail-safe: if the command cannot execute, watchdog expiry
        // still removes hash power.
        info!(
            "Step 5a: Disabling hash board voltages — telling each PIC to cut power to ASIC chips"
        );
        if let Some(ref tx) = self.voltage_cmd_tx {
            for &idx in &self.detected_board_indices {
                let (chain_id, pic_addr) = match (
                    self.chain_id_for_board(idx),
                    self.pic_addr_for_board(idx),
                ) {
                    (Ok(chain_id), Ok(pic_addr)) => (chain_id, pic_addr),
                    (chain_result, pic_result) => {
                        error!(
                            board_index = idx,
                            chain_error = ?chain_result.err(),
                            pic_error = ?pic_result.err(),
                            "Could not resolve shutdown controller endpoint; watchdog remains armed"
                        );
                        software_disable_failed = true;
                        continue;
                    }
                };
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                if let Err(e) = tx.try_send(VoltageCommand::DisableVoltage {
                    chain_id: Some(chain_id),
                    chip_id: self.chip_id,
                    pic_addr,
                    reply_tx: Some(reply_tx),
                }) {
                    match &e {
                        VoltageTrySendError::Full(_) => warn!(
                            chain_id,
                            pic_addr = format_args!("0x{:02X}", pic_addr),
                            "voltage mailbox full, rejecting DisableVoltage (graceful shutdown)"
                        ),
                        VoltageTrySendError::Disconnected => error!(
                            chain_id,
                            pic_addr = format_args!("0x{:02X}", pic_addr),
                            "voltage worker thread dead — daemon shutdown imminent (graceful shutdown)"
                        ),
                        other => warn!(
                            chain_id,
                            pic_addr = format_args!("0x{:02X}", pic_addr),
                            error = %other,
                            "voltage mailbox rejected DisableVoltage (graceful shutdown)"
                        ),
                    }
                    warn!(
                        chain_id,
                        pic_addr = format_args!("0x{:02X}", pic_addr),
                        error = %e,
                        "Shutdown voltage disable command could not be queued — watchdog remains the safety net"
                    );
                    software_disable_failed = true;
                    continue;
                }

                match tokio::time::timeout(Duration::from_secs(3), reply_rx).await {
                    Ok(Ok(Ok(_))) => {
                        info!(
                            chain_id,
                            "Chain {} voltage-disable command completed (physical rail-off is not independently measured)", chain_id,
                        );
                    }
                    Ok(Ok(Err(detail))) => {
                        warn!(
                            chain_id,
                            pic_addr = format_args!("0x{:02X}", pic_addr),
                            error = %detail,
                            "Shutdown voltage disable failed on runtime I2C thread — watchdog remains the safety net"
                        );
                        software_disable_failed = true;
                    }
                    Ok(Err(_)) => {
                        warn!(
                            chain_id,
                            pic_addr = format_args!("0x{:02X}", pic_addr),
                            "Shutdown voltage disable reply channel dropped — watchdog remains the safety net"
                        );
                        software_disable_failed = true;
                    }
                    Err(_) => {
                        warn!(
                            chain_id,
                            pic_addr = format_args!("0x{:02X}", pic_addr),
                            "Timed out waiting for shutdown voltage disable — watchdog remains the safety net"
                        );
                        software_disable_failed = true;
                    }
                }
            }
        } else {
            let shutdown_i2c_service = self.i2c_service.clone();
            let shutdown_i2c_fw = match self.pic_firmware {
                PicFirmware::BraiinsOs => dcentrald_hal::i2c::I2cPicFirmware::BraiinsOs,
                PicFirmware::Stock(_) => dcentrald_hal::i2c::I2cPicFirmware::Stock,
                PicFirmware::Unknown => dcentrald_hal::i2c::I2cPicFirmware::Unknown,
            };
            let shutdown_pic_type = self.pic_type();
            for &idx in &self.detected_board_indices {
                let (chain_id, pic_addr, pic_type) = match (
                    self.chain_id_for_board(idx),
                    self.pic_addr_for_board(idx),
                    shutdown_pic_type.as_ref(),
                ) {
                    (Ok(chain_id), Ok(pic_addr), Ok(pic_type)) => (chain_id, pic_addr, *pic_type),
                    (chain_result, pic_result, pic_type_result) => {
                        error!(
                            board_index = idx,
                            chain_error = ?chain_result.err(),
                            pic_error = ?pic_result.err(),
                            pic_type_error = ?pic_type_result.err(),
                            "Could not resolve fallback shutdown controller; watchdog remains armed"
                        );
                        software_disable_failed = true;
                        continue;
                    }
                };
                if let Some(i2c_svc) = shutdown_i2c_service.as_ref() {
                    match pic_type {
                        PicType::DsPic33EP => {
                            let mut dspic = DspicService::new(i2c_svc.clone(), pic_addr);
                            let _ = dspic.send_heartbeat();
                            if let Err(e) = dspic.disable_voltage() {
                                warn!(
                                    chain_id,
                                    pic_addr = format_args!("0x{:02X}", pic_addr),
                                    error = %e,
                                    "dsPIC failed to disable voltage on chain {} — watchdog will cut power anyway (hardware safety)",
                                    chain_id,
                                );
                                software_disable_failed = true;
                            } else {
                                info!(
                                    chain_id,
                                    "Chain {} dsPIC voltage-disable command completed (physical rail-off is not independently measured)",
                                    chain_id,
                                );
                            }
                        }
                        _ => {
                            let async_i2c = i2c_svc.async_handle();
                            let _ = async_i2c.heartbeat(pic_addr, shutdown_i2c_fw).await;
                            if let Err(e) =
                                async_i2c.disable_voltage(pic_addr, shutdown_i2c_fw).await
                            {
                                warn!(
                                    chain_id,
                                    pic_addr = format_args!("0x{:02X}", pic_addr),
                                    error = %e,
                                    "Failed to disable voltage on chain {} — PIC heartbeat will timeout and cut power in ~5s (stock) / ~10s (BraiinsOS) anyway (hardware safety)",
                                    chain_id,
                                );
                                software_disable_failed = true;
                            } else {
                                info!(
                                    chain_id,
                                    "Chain {} voltage-disable command completed (physical rail-off is not independently measured)",
                                    chain_id,
                                );
                            }
                        }
                    }
                } else {
                    error!(
                        chain_id,
                        pic_addr = format_args!("0x{:02X}", pic_addr),
                        "I2C service missing during shutdown — PIC watchdog will cut voltage in ~64s (hardware safety net)",
                    );
                    software_disable_failed = true;
                    // Continue shutdown — PIC watchdog provides hardware safety net
                }
            }
        }

        // Step 5b: Stop heartbeat ownership. If software safe-off was not
        // confirmed, this is what permits the hardware watchdog to cut voltage.
        if software_disable_failed {
            warn!(
                "Step 5b: Stopping PIC heartbeat thread — voltage was NOT confirmed off; controller watchdog cutoff is now the safety path"
            );
        } else {
            info!(
                "Step 5b: Stopping PIC heartbeat thread — software disable commands completed; physical rail-off was not independently measured"
            );
        }
        self.heartbeat_shutdown_token.cancel();
        let mut heartbeat_owners_quiesced = true;
        if let Some(handle) = self.runtime_heartbeat_handle.take() {
            match join_thread_bounded(handle, Duration::from_secs(3)).await {
                ThreadStopOutcome::Joined => info!("Runtime heartbeat thread stopped and joined"),
                ThreadStopOutcome::Panicked => {
                    error!("Runtime heartbeat thread panicked while stopping")
                }
                ThreadStopOutcome::TimedOut => {
                    warn!(
                        "Runtime heartbeat thread did not exit within 3s; its cancellation token remains asserted and the handle was detached"
                    );
                    heartbeat_owners_quiesced = false;
                }
            }
        }
        heartbeat_owners_quiesced &= self.stop_init_heartbeat_bounded().await;

        // Step 6: Wait for power discharge
        info!("Step 6: Waiting 2s for hash board capacitors to discharge...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Step 7: Ramp fans to configured max for cool-down
        // Chips were just mining and are still hot. Run at max configured speed
        // (not hardcoded 50%) to evacuate residual heat safely.
        if let Some(ref fan) = self.fan {
            let cooldown_pwm =
                clamp_fan_pwm(self.config.thermal.fan_max_pwm.max(FAN_PWM_SAFETY_MAX));
            fan.set_speed(cooldown_pwm);
            info!(
                "Step 7: Fans set to PWM {} for post-mining cool-down",
                cooldown_pwm
            );
        }

        // Step 8: Wait for cool-down
        info!("Step 8: Cooling down for 5 seconds before reducing fan speed...");
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Step 9: Command fans back to the home idle minimum.
        if let Some(ref fan) = self.fan {
            fan.set_speed(FAN_PWM_QUIET_BOOT);
            info!(
                "Step 9: Fans commanded back to home idle PWM {}; cool-down complete",
                FAN_PWM_QUIET_BOOT
            );
        }

        // Step 10: Explicitly disarm the independently owned SoC watchdog only
        // after every hardware actor is quiescent and the software safe-off
        // policy completed. Owner cancellation, abort, timeout, and Drop are
        // deliberately not disarm authority.
        let watchdog_disarm_allowed = !software_disable_failed
            && !mining_quiescence_failed
            && !psu_quiescence_failed
            && heartbeat_owners_quiesced;
        if !watchdog_disarm_allowed {
            error!(
                software_disable_failed,
                mining_quiescence_failed,
                psu_quiescence_failed,
                heartbeat_owners_quiesced,
                "Step 10: Refusing SoC watchdog disarm because shutdown safety evidence is incomplete"
            );
            return Err(anyhow::anyhow!(
                "shutdown safety evidence is incomplete; SoC watchdog remains armed"
            ));
        }

        if let Some(intent_tx) = self.watchdog_intent_tx.take() {
            // A receiver may already have exited with NotOpenedByDaemon; the receipt,
            // rather than send success alone, is the authoritative outcome.
            let _ = intent_tx.send(WatchdogIntent::Disarm);
        }
        let watchdog_receipt = match self.watchdog_receipt_rx.take() {
            Some(receipt_rx) => {
                match tokio::time::timeout(WATCHDOG_TASK_STOP_TIMEOUT, receipt_rx).await {
                    Ok(Ok(receipt)) => Some(receipt),
                    Ok(Err(_)) => {
                        return Err(anyhow::anyhow!(
                            "SoC watchdog task ended without a disarm receipt; kernel watchdog state is unknown"
                        ));
                    }
                    Err(_) => {
                        return Err(anyhow::anyhow!(
                            "timed out after sending SoC watchdog Disarm; the irreversible magic-close outcome is unknown"
                        ));
                    }
                }
            }
            None => None,
        };

        let magic_close_write_completed = match watchdog_receipt {
            Some(WatchdogTaskReceipt::MagicCloseWriteCompleted) => {
                info!("Step 10: Watchdog magic-close byte write completed and task exit will be observed; kernel timer state remains unmeasured");
                true
            }
            Some(WatchdogTaskReceipt::NotOpenedByDaemon) => {
                info!("Step 10: This daemon did not open the hardware watchdog (disabled, unavailable, or busy); pre-existing kernel watchdog state is unmeasured");
                false
            }
            Some(WatchdogTaskReceipt::MagicCloseWriteFailed(error)) => {
                return Err(anyhow::anyhow!(
                    "SoC watchdog magic-close byte write failed ({error}); kernel watchdog state is unknown"
                ));
            }
            None => {
                info!("Step 10: Standard-path hardware watchdog was never started");
                false
            }
        };

        let watchdog_stop = self
            .watchdog_tasks
            .stop_and_join(WATCHDOG_TASK_STOP_TIMEOUT)
            .await;
        if watchdog_stop.any_timed_out() || watchdog_stop.any_panicked() {
            return Err(anyhow::anyhow!(
                "SoC watchdog task termination was not cleanly observed"
            ));
        }

        info!("=== SHUTDOWN COMPLETE ===");
        // prod-readiness hunt #1: only attest "Safe to unplug or restart" when
        // SOFTWARE completed every disable command. If any disable branch failed
        // (or the I2C service was missing), the rail is relying
        // solely on the ~5-64 s PIC/dsPIC watchdog — do NOT prompt a warm restart.
        if software_disable_failed {
            warn!(
                "Shutdown finished but one or more chains were NOT confirmed \
                 de-energized by software — the PIC/dsPIC hardware watchdog (~5-64 s) \
                 is now the only thing cutting voltage. Do NOT warm-restart until \
                 power is confirmed off (wait out the watchdog or AC-cycle)."
            );
        } else {
            if magic_close_write_completed {
                info!(
                    "All hash-board voltage-disable commands completed, fans were commanded to idle, the SoC watchdog magic-close write completed, and its task exit was observed. Kernel timer state and physical rail-off were not independently measured; observe the documented discharge/watchdog interval before a warm restart."
                );
            } else {
                info!(
                    "All hash-board voltage-disable commands completed and fans were commanded to idle. This daemon performed no SoC watchdog magic-close write; pre-existing kernel watchdog state and physical rail-off remain unmeasured."
                );
            }
        }
        Ok(())
    }
}
// Hardware-info free functions moved to crate::runtime::hardware_info
// (W2.1, 2026-05-07). The daemon imports them via `use` at the top.
// EEPROM fingerprint helpers + collect_hardware_info + detect_control_board
// + read_miner_serial + read_hb_type + probe_psu_info + tests now all live
// in src/runtime/hardware_info.rs.

// ---------------------------------------------------------------------------
// Wave-I Lane A — gRPC read-RPC snapshot builders (module-level helpers).
// Convert the live `MinerState` (+ restart-static tuner config) into the lean
// plain `dcentrald_api_grpc` snapshot structs. Read-only; no secrets.
// ---------------------------------------------------------------------------

/// Map the live `MinerState` (+ the restart-static tuner snapshot) into the
/// gRPC runtime snapshot. Pool passwords are intentionally dropped — the read
/// RPC never carries them. `mining_state` is derived honestly from observed
/// hashrate + pool connection state.
fn build_grpc_runtime_snapshot(
    state: &dcentrald_api::MinerState,
    platform_marker: &str,
    chip_family: &str,
    home_cap_pwm: u32,
    tuner: dcentrald_api_grpc::GrpcTunerSnapshot,
) -> dcentrald_api_grpc::GrpcRuntimeSnapshot {
    let chain_alive_count = state
        .chains
        .iter()
        .filter(|c| c.chips > 0 && c.status != "dead" && c.status != "down")
        .count() as u32;
    let mining_state = if state.hashrate_ghs > 0.0 {
        "mining"
    } else if state.pool.status == "connecting" {
        "starting"
    } else {
        "idle"
    }
    .to_string();
    let status = dcentrald_api_grpc::GrpcMinerStatus {
        firmware_version: state.firmware_version.clone(),
        platform_marker: platform_marker.to_string(),
        chip_family: chip_family.to_string(),
        hashrate_ths: state.hashrate_ghs / 1000.0,
        chain_count: state.chains.len() as u32,
        chain_alive_count,
        uptime_seconds: state.uptime_s,
        mining_state,
    };
    // MinerState carries the connected/active pool, not the full configured
    // failover list — surface it as a single priority-0 entry (honest live
    // state). Empty URL ⇒ no pool entry rather than a blank row.
    let pools = if state.pool.url.is_empty() {
        Vec::new()
    } else {
        vec![dcentrald_api_grpc::GrpcPoolEntry {
            url: state.pool.url.clone(),
            worker: state.pool.worker.clone(),
            priority: 0,
        }]
    };
    let fans = if state.fans.per_fan.is_empty() {
        // Legacy single-tach fallback when no per-fan readings exist.
        vec![dcentrald_api_grpc::GrpcFanReading {
            index: 0,
            rpm: state.fans.rpm,
            pwm: state.fans.pwm as u32,
            failed: state.fans.rpm == 0 && state.fans.pwm > 0,
        }]
    } else {
        state
            .fans
            .per_fan
            .iter()
            .map(|f| dcentrald_api_grpc::GrpcFanReading {
                index: f.id as u32,
                rpm: f.rpm,
                pwm: f.pwm_percent as u32,
                failed: f.rpm == 0 && f.pwm_percent > 0,
            })
            .collect()
    };
    let fan = dcentrald_api_grpc::GrpcFanSnapshot {
        fans,
        control_mode: "auto".to_string(),
        home_cap_pwm,
    };
    dcentrald_api_grpc::GrpcRuntimeSnapshot {
        status,
        pools,
        fan,
        tuner,
    }
}

/// Map the configured `TunerMode` discriminant into the gRPC tuner snapshot.
/// Config is restart-static, so this is captured once at install time.
fn grpc_tuner_snapshot_from_config(
    mode: &crate::autotune::TunerMode,
) -> dcentrald_api_grpc::GrpcTunerSnapshot {
    use crate::autotune::TunerMode;
    let mut snap = dcentrald_api_grpc::GrpcTunerSnapshot::default();
    match mode {
        TunerMode::Performance { .. } => snap.mode = "performance".into(),
        TunerMode::PowerTarget { target_watts, .. } => {
            snap.mode = "power_target".into();
            snap.power_target_watts = *target_watts;
        }
        TunerMode::HashrateTarget { target_ths, .. } => {
            snap.mode = "hashrate_target".into();
            snap.hashrate_target_ths = *target_ths;
        }
        TunerMode::Manual {
            freq_mhz,
            voltage_mv,
            ..
        } => {
            snap.mode = "manual".into();
            snap.manual_freq_mhz = *freq_mhz as u32;
            snap.manual_voltage_mv = *voltage_mv as u32;
        }
        TunerMode::Efficiency { .. } => snap.mode = "efficiency".into(),
        TunerMode::Heater { target_watts, .. } => {
            snap.mode = "heater".into();
            snap.power_target_watts = *target_watts;
        }
        TunerMode::HashrateQuota { .. } => snap.mode = "hashrate_quota".into(),
    }
    snap
}

// ---------------------------------------------------------------------------
// SW-02 — gRPC write control-plane delegate.
//
// `dcentrald-api-grpc` defines the `GrpcWriteDelegate` trait + the
// `install_write_delegate` OnceLock but cannot itself reach the gated write
// surfaces (it intentionally does NOT depend on the HAL-bound `dcentrald-api`).
// The daemon depends on both, so the concrete bridge lives here and is
// installed in `Daemon::run` (next to `install_runtime_snapshot_rx`).
//
// LIVE-DEFAULT / safety posture (load-bearing):
//   * The delegate is installed ONLY when `[api.grpc].enabled` AND the
//     default-OFF env gate `DCENT_GRPC_WRITE_CONTROL=1` is set. With the gate
//     unset (the compiled default), NO delegate is installed and every gRPC
//     write RPC keeps returning `UNIMPLEMENTED` — byte-identical to the prior
//     read-only contract. So this wave changes no live default.
//   * `set_tuner_mode` bridges to the SAME live autotuner command channel the
//     REST/cgminer-LuxOS surface uses (`AppState::autotuner_command_tx` →
//     `AutoTunerCommand::ApplyMode`). All the autotuner's own clamps (≤14500 mV
//     dsPIC cap, PVT envelope, `pin_am2_bm1362_frequency_only` band, fan-cap)
//     are enforced downstream exactly as on the REST path — the delegate adds
//     no new bypass.
//   * `locate_device` bridges to the daemon-owned LED channel (`led_tx` →
//     `LedCommand::Locate`/`StopLocate`). LED-only; no hash/power/thermal/PSU
//     effect.
//   * `set_fan_mode` — bridges to `dcentrald_api::rest::grpc_bridge_set_fan`,
//     the SAME fan envelope + HAL write `POST /api/fan` uses. The load-bearing
//     PWM-30 HOME cap is enforced there against the daemon's live
//     `OperatingMode` (read from `AppState::mode_rx`) AND independently at the
//     gRPC `FanSvc` layer (which pre-clamps on home_mode) — belt-and-suspenders.
//     `allow_loud` is `false` on this path, so a home unit can never exceed
//     PWM 30. The delegate reports the POST-clamp applied PWM in
//     `applied_value` (a home unit asking for 100 sees 30).
//   * `set_pools` — bridges to `dcentrald_api::rest::grpc_bridge_set_pools`,
//     the SAME `validate_and_write_pool_config` core `POST /api/pools` uses
//     (≤3 pools, non-empty primary URL, V1 pool-URL support, atomic TOML write).
//   * `reboot` — bridges to `dcentrald_api::rest::grpc_bridge_reboot`, the same
//     non-destructive persistent-session refusal `POST /api/action/restart`
//     returns until typed hardware-disposition receipts exist.
//
// All three now return a real ack with applied values on success and a real
// reject (the verbatim validation/cap/IO message) on rejection — never a silent
// ack and never a bypass; every validation + safety cap lives in the one shared
// `dcentrald-api` helper. The narrow `pub async fn grpc_bridge_*` hooks are the
// only public surface added; the raw axum handlers stay `pub(crate)`.
//
// Net effect with the gate ON: all five gRPC write RPCs work through the same
// gated runtime/REST paths. With the gate OFF (compiled default), NO delegate is
// installed → every write RPC stays UNIMPLEMENTED — byte-identical to the prior
// read-only contract. No live default changes.
// ---------------------------------------------------------------------------

/// Env gate that opts the gRPC WRITE control plane in. Default-OFF: when unset
/// the daemon installs no [`GrpcWriteDelegate`] and every gRPC write RPC stays
/// `UNIMPLEMENTED` (byte-identical to the prior read-only contract).
const ENV_GRPC_WRITE_CONTROL: &str = "DCENT_GRPC_WRITE_CONTROL";

/// True iff the gRPC write control plane is opted in via
/// [`ENV_GRPC_WRITE_CONTROL`]. Reuses the shared autotuner env-truthiness
/// helper so the daemon and tests agree on what counts as "on".
fn grpc_write_control_enabled() -> bool {
    std::env::var(ENV_GRPC_WRITE_CONTROL)
        .ok()
        .map(|v| dcentrald_autotuner::config::env_flag_is_truthy(&v))
        .unwrap_or(false)
}

/// Map a gRPC `set_tuner_mode` request (the mode-discriminant string + numeric
/// fields produced by [`grpc_tuner_snapshot_from_config`]) into the autotuner
/// runtime's [`dcentrald_autotuner::config::TunerMode`] that
/// `AutoTunerCommand::ApplyMode` accepts. Returns `None` for an unrecognized
/// mode string so the delegate can reject cleanly instead of guessing.
///
/// This is the gRPC-side equivalent of the (private) REST mode parsing: the
/// resulting mode is dispatched on the SAME live command channel, so every
/// downstream clamp applies identically.
fn grpc_tuner_mode_to_autotuner(
    req: &dcentrald_api_grpc::GrpcSetTunerMode,
) -> Option<dcentrald_autotuner::config::TunerMode> {
    use dcentrald_autotuner::config::TunerMode;
    match req.mode.trim().to_ascii_lowercase().as_str() {
        "performance" => Some(TunerMode::Performance),
        "efficiency" => Some(TunerMode::Efficiency),
        "power_target" | "powertarget" | "power" => Some(TunerMode::PowerTarget {
            watts: req.power_target_watts,
        }),
        "hashrate_target" | "hashratetarget" | "hashrate" => Some(TunerMode::HashrateTarget {
            ths: req.hashrate_target_ths,
        }),
        "heater" => Some(TunerMode::Heater {
            // gRPC reuses `power_target_watts` for the heater wattage target
            // (same field `grpc_tuner_snapshot_from_config` populates on the
            // read side); convert to the heater BTU/h discriminant the runtime
            // expects. 1 W ≈ 3.412 BTU/h.
            btu_h: ((req.power_target_watts as f64) * 3.412).round() as u32,
        }),
        // Manual is an explicit fixed operating point. The autotuner runtime
        // clamps freq/voltage downstream (≤14500 mV dsPIC cap, PVT envelope) —
        // same as the REST path — so we pass the requested values straight
        // through and let the gated runtime own the safety clamp.
        "manual" => Some(TunerMode::Manual {
            freq_mhz: req.manual_freq_mhz.min(u16::MAX as u32) as u16,
            voltage_mv: req.manual_voltage_mv,
        }),
        _ => None,
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(i2c_service) = self.i2c_service.as_ref() {
            let _ = i2c_service.latch_terminal_safe_off();
        }
        if let Some(voltage_mailbox) = self.voltage_cmd_tx.as_ref() {
            voltage_mailbox.latch_terminal();
        }
        // Last-resort cancellation safety. Normal paths bounded-join this
        // thread explicitly, but dropping a partially initialized daemon must
        // never leave an initialization heartbeat producer intentionally live.
        self.signal_init_heartbeat_stop();
        self.heartbeat_shutdown_token.cancel();
    }
}

/// SW-02 concrete delegate. Holds an `Arc<AppState>` (cloned from the one the
/// API servers run on) so it can reach the SAME gated runtime channels the REST
/// handlers use. Constructed + installed only when the gate is on (see
/// [`grpc_write_control_enabled`]).
struct DaemonGrpcWriteDelegate {
    app_state: Arc<dcentrald_api::AppState>,
}

#[tonic::async_trait]
impl dcentrald_api_grpc::GrpcWriteDelegate for DaemonGrpcWriteDelegate {
    async fn set_pools(
        &self,
        req: dcentrald_api_grpc::GrpcSetPools,
    ) -> Result<dcentrald_api_grpc::GrpcWriteOutcome, tonic::Status> {
        // SW-02: bridge to the SAME validate-and-write core `POST /api/pools`
        // uses (`dcentrald_api::rest::grpc_bridge_set_pools` →
        // `validate_and_write_pool_config`). All validation (≤3 pools, non-empty
        // primary URL, V1 pool-URL support) + the atomic TOML write happen
        // there — this delegate adds no new bypass. Honest outcome: a real ack
        // with the applied pool count on success, a real reject (the verbatim
        // validation/IO message) on failure — never a silent ack.
        match dcentrald_api::rest::grpc_bridge_set_pools(&self.app_state, req.pools).await {
            Ok(ok) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::ack(format!(
                "pool configuration saved ({} pool(s), primary {})",
                ok.pool_count, ok.primary_url
            ))),
            Err(message) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(message)),
        }
    }

    async fn set_fan_mode(
        &self,
        req: dcentrald_api_grpc::GrpcSetFanMode,
    ) -> Result<dcentrald_api_grpc::GrpcWriteOutcome, tonic::Status> {
        // SW-02: bridge to the SAME fan envelope + HAL write `POST /api/fan`
        // uses (`dcentrald_api::rest::grpc_bridge_set_fan` →
        // `compute_commanded_fan_pwm` then `set_fan_pwm_via_hal`). The
        // load-bearing HOME PWM-30 hard cap is enforced there against the
        // daemon's live `OperatingMode` (read from `AppState::mode_rx`) — so
        // even though the gRPC FanSvc layer already pre-clamps on home_mode,
        // this bridge independently re-enforces the cap (belt-and-suspenders).
        // `allow_loud` is `false` on this path: a home unit can NEVER exceed
        // PWM 30 here.
        // CE-052: the bridge now derives the live mode from `AppState` itself and
        // runs the fail-closed `PowerControl` capability guard first; the local
        // copy here is kept only for the honest ack message below.
        let current_mode = *self.app_state.mode_rx.borrow();
        match dcentrald_api::rest::grpc_bridge_set_fan(&self.app_state, req.manual_pwm) {
            Ok(applied_pwm) => {
                // Honest: report the POST-clamp value actually written (e.g. a
                // home unit asked for 100 → applied 30), not the request.
                let mut out = dcentrald_api_grpc::GrpcWriteOutcome::ack(format!(
                    "fan PWM set to {applied_pwm} (mode '{}', requested {}, clamped to the \
                     {:?}-mode envelope)",
                    req.mode, req.manual_pwm, current_mode
                ));
                out.applied_value = Some(applied_pwm as u32);
                Ok(out)
            }
            Err(message) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(message)),
        }
    }

    async fn set_tuner_mode(
        &self,
        req: dcentrald_api_grpc::GrpcSetTunerMode,
    ) -> Result<dcentrald_api_grpc::GrpcWriteOutcome, tonic::Status> {
        // CE-052: fail-closed `AsicOptions` capability guard BEFORE any dispatch —
        // the tuner-mode write historically skipped the gate its REST twins hold.
        if let Err(m) =
            dcentrald_api::rest::bridge_guard_asic_options(&self.app_state, "grpc:set_tuner_mode")
        {
            return Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(m));
        }
        let Some(mode) = grpc_tuner_mode_to_autotuner(&req) else {
            return Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(format!(
                "unrecognized tuner mode '{}'",
                req.mode
            )));
        };
        let Some(tx) = self.app_state.autotuner_command_tx.as_ref() else {
            return Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(
                "live autotuner command channel is not available in this runtime",
            ));
        };
        // Mirror `rest::dispatch_autotuner_mode_command`: send ApplyMode and
        // wait briefly for the ack. All clamps live downstream in the autotuner
        // runtime — this dispatch adds no new bypass.
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let command = dcentrald_autotuner::AutoTunerCommand::ApplyMode { mode, ack_tx };
        if tx.send(command).await.is_err() {
            return Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(
                "live autotuner command channel is closed",
            ));
        }
        match tokio::time::timeout(Duration::from_secs(2), ack_rx).await {
            Ok(Ok(result)) => {
                let detail = format!(
                    "autotuner mode '{}' {} ({})",
                    req.mode,
                    if result.applied_runtime {
                        "applied live"
                    } else {
                        "persisted for next cycle"
                    },
                    result.message
                );
                Ok(dcentrald_api_grpc::GrpcWriteOutcome::ack(detail))
            }
            Ok(Err(_)) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(
                "live autotuner command channel closed before acknowledgement",
            )),
            Err(_) => {
                // Sent but no ack in time — the runtime will pick it up on its
                // next cycle. Honest: acknowledged (queued), not "applied".
                Ok(dcentrald_api_grpc::GrpcWriteOutcome::ack(format!(
                    "autotuner mode '{}' queued (no ack within 2s)",
                    req.mode
                )))
            }
        }
    }

    async fn reboot(&self) -> Result<dcentrald_api_grpc::GrpcWriteOutcome, tonic::Status> {
        // SW-02: bridge to the SAME persistent-session policy
        // `POST /api/action/restart` enforces. It preserves the live owner and
        // returns an explicit refusal; there is no independent shell-out that
        // could diverge from the gated REST action.
        match dcentrald_api::rest::grpc_bridge_reboot(&self.app_state) {
            Ok(detail) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::ack(detail)),
            Err(message) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(message)),
        }
    }

    async fn locate_device(
        &self,
        req: dcentrald_api_grpc::GrpcLocate,
    ) -> Result<dcentrald_api_grpc::GrpcWriteOutcome, tonic::Status> {
        // CE-052: fail-closed `Identify` capability guard BEFORE any LED dispatch.
        if let Err(m) =
            dcentrald_api::rest::bridge_guard_identify(&self.app_state, "grpc:locate_device")
        {
            return Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(m));
        }
        let Some(led_tx) = self.app_state.led_tx.as_ref() else {
            return Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(
                "LED engine not available (GPIO controller not initialized on this hardware)",
            ));
        };
        let cmd = if req.off {
            LedCommand::StopLocate
        } else {
            LedCommand::Locate {
                pattern_id: String::new(),
            }
        };
        match led_tx.try_send(cmd) {
            Ok(()) => {
                let state = if req.off { "off" } else { "blinking" };
                let mut out =
                    dcentrald_api_grpc::GrpcWriteOutcome::ack(format!("locate LED {}", state));
                out.applied_text = Some(state.to_string());
                Ok(out)
            }
            Err(e) => Ok(dcentrald_api_grpc::GrpcWriteOutcome::reject(format!(
                "failed to send LED command: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod td003_destructive_write_guard_tests {
    use super::*;

    const DAEMON_RS: &str = include_str!("daemon.rs");

    fn offset_after(haystack: &str, start: usize, needle: &str) -> usize {
        haystack[start..]
            .find(needle)
            .map(|idx| start + idx)
            .unwrap_or_else(|| panic!("missing source marker: {needle}"))
    }

    #[test]
    fn td003_signal_classifier_blocks_in_development_and_ambiguous_am2() {
        assert_eq!(
            td003_destructive_write_refusal_from_signals(Some("s19xp"), "", "", "")
                .unwrap()
                .source,
            "config-model"
        );
        assert_eq!(
            td003_destructive_write_refusal_from_signals(None, "am2-s17p", "", "")
                .unwrap()
                .model_name,
            "Antminer S17 / S17 Pro"
        );
        assert_eq!(
            td003_destructive_write_refusal_from_signals(None, "", "am3-s19xp", "")
                .unwrap()
                .model_name,
            "Antminer S19 XP"
        );
        assert_eq!(
            td003_destructive_write_refusal_from_signals(None, "", "zynq-bm3-am2", "")
                .unwrap()
                .model_name,
            "generic AM2 platform without exact board target"
        );
        assert_eq!(
            td003_destructive_write_refusal_from_signals(None, "unknown", "zynq-bm3-am2", "",)
                .unwrap()
                .source,
            "platform-marker+board-target"
        );
    }

    #[test]
    fn td003_signal_classifier_allows_promoted_or_non_td003_identities() {
        for (model, board_target, platform) in [
            (Some("s9"), "am1-s9", "zynq-bm1-s9"),
            (Some("s19jpro"), "am2-s19jpro-zynq", "zynq-bm3-am2"),
            (Some("s19pro"), "am2-s19pro", "zynq-bm3-am2"),
            (Some("s19k"), "am3-s19k", "amlogic-a113d"),
            (Some("s21"), "am3-s21", "amlogic-a113d"),
            (None, "am2-s19jpro-xil", "zynq-bm3-am2"),
        ] {
            assert_eq!(
                td003_destructive_write_refusal_from_signals(model, board_target, platform, ""),
                None,
                "model={model:?} board_target={board_target:?} platform={platform:?} must not be caught by TD-003"
            );
        }
    }

    #[test]
    fn td003_run_lifecycle_guard_precedes_hardware_init() {
        let start = DAEMON_RS
            .find("async fn run_lifecycle(&mut self)")
            .expect("run_lifecycle missing");
        let guard = offset_after(DAEMON_RS, start, "self.td003_destructive_write_refusal(");
        let hardware_snapshot =
            offset_after(DAEMON_RS, start, "collect_hardware_info(&self.config)");
        let boot_progress = offset_after(DAEMON_RS, start, "let boot_progress");
        let init_call = offset_after(DAEMON_RS, start, "initialize_or_recover(");
        assert!(
            guard < boot_progress,
            "TD-003 guard must run before boot-progress mining init starts"
        );
        assert!(
            guard < init_call,
            "TD-003 guard must run before the injected platform lifecycle"
        );
        assert!(
            guard < hardware_snapshot && hardware_snapshot < init_call,
            "hardware-info probing must run after TD-003 refusal but before runtime fabric reservation"
        );

        let guard_body = &DAEMON_RS[guard..boot_progress];
        assert!(
            guard_body.contains("return self.run_api_only().await"),
            "run_lifecycle TD-003 refusal must park API-only, not exit or continue"
        );
    }

    #[test]
    fn bootstrap_i2c_policy_capture_follows_topology_admission_and_precedes_service() {
        let init_start = DAEMON_RS
            .find("async fn init(")
            .expect("init definition missing");
        let profile_admission = offset_after(DAEMON_RS, init_start, "self.miner_profile = Some(");
        let capture = offset_after(
            DAEMON_RS,
            init_start,
            "self.capture_bootstrap_i2c_observations()?",
        );
        let service = offset_after(
            DAEMON_RS,
            init_start,
            "ProductionSerializedI2cFactory.open_serialized_i2c",
        );
        assert!(
            profile_admission < capture && capture < service,
            "bootstrap sysfs I2C reads require admitted topology and must finish before service reservation"
        );
    }

    #[test]
    fn td003_init_guard_precedes_every_destructive_hardware_open() {
        let init_start = DAEMON_RS
            .find("async fn init(")
            .expect("init definition missing");
        let guard = offset_after(
            DAEMON_RS,
            init_start,
            "self.td003_destructive_write_refusal(",
        );
        for marker in [
            "ProductionSerializedI2cFactory.open_serialized_i2c",
            "FanController::open_discovered",
            "GpioController::new",
            "FpgaChain::open",
            "cold_boot_init",
            "assign_addresses",
            "init_with_driver",
        ] {
            let pos = offset_after(DAEMON_RS, init_start, marker);
            assert!(guard < pos, "TD-003 init guard must precede {marker}");
        }
    }

    #[test]
    fn standard_non_s9_i2c_registers_hashboard_eeprom_denylist() {
        let factory_start = DAEMON_RS
            .find("impl SerializedI2cOperations for ProductionSerializedI2cOperations")
            .expect("production serialized-I2C operations missing");
        let devmem = offset_after(
            DAEMON_RS,
            factory_start,
            "spawn_am1_s9_i2c0_service(recover_bus)",
        );
        let amlogic = offset_after(
            DAEMON_RS,
            factory_start,
            "spawn_amlogic_protected_i2c0_service()",
        );
        let generic = offset_after(
            DAEMON_RS,
            factory_start,
            "spawn_i2c_service_no_register_touch_with_denylist",
        );
        assert!(devmem < amlogic);
        assert!(
            amlogic < generic,
            "standard non-S9/non-Amlogic daemon I2C must use the denylist constructor"
        );
        assert!(
            DAEMON_RS[generic..].contains("HASHBOARD_EEPROM_WRITE_DENYLIST.to_vec()"),
            "standard non-S9 I2C service must register the 0x50-0x57 EEPROM write denylist"
        );
    }

    #[test]
    fn daemon_has_no_direct_axi_iic_recovery_or_unbind_capability() {
        for (prefix, suffix) in [
            ("bus_recovery_", "devmem("),
            ("reset_axi_iic_", "controller("),
            ("devmem_i2c_", "read("),
            ("devmem_clear_isr_", "tx_error("),
            ("unbind_kernel_i2c_", "driver("),
        ] {
            let forbidden = [prefix, suffix].concat();
            assert!(
                !DAEMON_RS.contains(&forbidden),
                "daemon must not bypass the HAL-owned fabric lifecycle via {forbidden}"
            );
        }
        let worker_recovery = ["i2c_svc.recover_", "unmanaged_bus()"].concat();
        assert!(DAEMON_RS.contains(&worker_recovery));
    }
}

#[cfg(test)]
mod sw02_perf004_wiring_tests {
    use super::*;

    // ----- SW-02: gRPC write-control gate (DCENT_GRPC_WRITE_CONTROL) -----
    //
    // These tests touch a process-global env var. They are serialized via a
    // module-local mutex and always restore the prior value, so they cannot
    // race each other or leak state into other tests. The DEFAULT (gate unset)
    // path is the load-bearing one: the daemon installs NO write delegate, so
    // the gRPC write RPCs stay UNIMPLEMENTED (proven in dcentrald-api-grpc).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env<R>(key: &str, value: Option<&str>, f: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        let out = f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        out
    }

    #[test]
    fn grpc_write_control_default_off_is_unset() {
        // The compiled default (env unset) keeps the write control plane OFF —
        // the daemon installs no delegate and gRPC writes stay UNIMPLEMENTED.
        with_env(ENV_GRPC_WRITE_CONTROL, None, || {
            assert!(!grpc_write_control_enabled());
        });
        with_env(ENV_GRPC_WRITE_CONTROL, Some("0"), || {
            assert!(!grpc_write_control_enabled());
        });
        with_env(ENV_GRPC_WRITE_CONTROL, Some("false"), || {
            assert!(!grpc_write_control_enabled());
        });
    }

    #[test]
    fn grpc_write_control_opt_in_truthy() {
        for truthy in ["1", "true", "yes", "on", "ON", "Yes"] {
            with_env(ENV_GRPC_WRITE_CONTROL, Some(truthy), || {
                assert!(
                    grpc_write_control_enabled(),
                    "{truthy:?} should enable the gRPC write control plane"
                );
            });
        }
    }

    #[test]
    fn grpc_tuner_mode_string_maps_to_autotuner_mode() {
        use dcentrald_api_grpc::GrpcSetTunerMode;
        use dcentrald_autotuner::config::TunerMode;

        let eff = grpc_tuner_mode_to_autotuner(&GrpcSetTunerMode {
            mode: "efficiency".into(),
            ..Default::default()
        });
        assert_eq!(eff, Some(TunerMode::Efficiency));

        let perf = grpc_tuner_mode_to_autotuner(&GrpcSetTunerMode {
            mode: "performance".into(),
            ..Default::default()
        });
        assert_eq!(perf, Some(TunerMode::Performance));

        let pwr = grpc_tuner_mode_to_autotuner(&GrpcSetTunerMode {
            mode: "power_target".into(),
            power_target_watts: 1200,
            ..Default::default()
        });
        assert_eq!(pwr, Some(TunerMode::PowerTarget { watts: 1200 }));

        let hr = grpc_tuner_mode_to_autotuner(&GrpcSetTunerMode {
            mode: "hashrate_target".into(),
            hashrate_target_ths: 100.0,
            ..Default::default()
        });
        assert_eq!(hr, Some(TunerMode::HashrateTarget { ths: 100.0 }));

        // Manual passes through; the autotuner runtime owns the ≤14500 mV /
        // PVT clamp (same as the REST path), so the mapping itself is faithful.
        let man = grpc_tuner_mode_to_autotuner(&GrpcSetTunerMode {
            mode: "manual".into(),
            manual_freq_mhz: 500,
            manual_voltage_mv: 13_700,
            ..Default::default()
        });
        assert_eq!(
            man,
            Some(TunerMode::Manual {
                freq_mhz: 500,
                voltage_mv: 13_700,
            })
        );

        // Unknown mode → None so the delegate rejects cleanly (never guesses).
        let unknown = grpc_tuner_mode_to_autotuner(&GrpcSetTunerMode {
            mode: "definitely_not_a_mode".into(),
            ..Default::default()
        });
        assert_eq!(unknown, None);
    }

    // ----- PERF-004: SKU-aware ceiling gate (DCENT_AM2_SKU_AWARE_CEILING) -----

    #[test]
    fn sku_aware_ceiling_default_off_is_unset() {
        with_env(ENV_AM2_SKU_AWARE_CEILING, None, || {
            assert!(!am2_sku_aware_ceiling_enabled());
        });
        with_env(ENV_AM2_SKU_AWARE_CEILING, Some("0"), || {
            assert!(!am2_sku_aware_ceiling_enabled());
        });
    }

    #[test]
    fn sku_aware_ceiling_opt_in_truthy() {
        for truthy in ["1", "true", "yes", "on"] {
            with_env(ENV_AM2_SKU_AWARE_CEILING, Some(truthy), || {
                assert!(am2_sku_aware_ceiling_enabled(), "{truthy:?} should enable");
            });
        }
    }

    #[test]
    fn perf004_standard_pin_is_545_regardless_of_gate() {
        // Gate-OFF path: the daemon always pins Standard (545). Prove the
        // Standard class the gate-off branch passes resolves to the historical
        // 545 ceiling — byte-identical to the pre-PERF-004 behavior.
        use dcentrald_autotuner::config::AutoTunerConfig;
        use dcentrald_autotuner::Bm1362SkuClass;

        let mut cfg = AutoTunerConfig::default();
        cfg.max_freq_mhz = 700; // operator-inflated; must clamp DOWN to 545
        cfg.pin_am2_bm1362_frequency_only_for_sku(Bm1362SkuClass::Standard);
        assert_eq!(cfg.max_freq_mhz, 545);
        assert!(!cfg.voltage_optimization);
        assert!(!cfg.dvfs_enabled);

        // And an unknown/standard SKU label classifies back to Standard, so
        // even with the gate ON a `a lab unit`/`a lab unit` BHB42601 home unit keeps 545.
        assert_eq!(
            Bm1362SkuClass::from_sku_label("BHB42601"),
            Bm1362SkuClass::Standard
        );
        assert_eq!(Bm1362SkuClass::from_sku_label(""), Bm1362SkuClass::Standard);
    }

    // ----- GROUP B: scheduled-curtailment wiring safety -----
    //
    // These prove the time-of-day curtailment driver only ever moves in the
    // SAFE direction (cut hash, lower fans) and that the PWM-30 cap still bounds
    // the sleep fan command it drives. The driver itself just calls
    // `enter_sleep()` / `wake()` on the shared controller; the thermal loop
    // consumer (already audited) does the hardware work. We exercise the same
    // controller + the same `clamp_fan_pwm` the loop uses.

    #[test]
    fn scheduled_curtailment_sleep_fan_is_within_pwm30_cap() {
        // The thermal loop reads `clamp_fan_pwm(curt.sleep_fan_pwm())` when the
        // controller is sleeping. Prove that the sleep fan command this driver
        // ultimately produces NEVER exceeds the home PWM-30 cap — i.e. entering
        // the curtailment window can only LOWER fans, never raise them.
        let mut curt = dcentrald_thermal::curtailment::CurtailmentController::new();
        // Driver behavior in-window: enter_sleep().
        assert!(
            curt.enter_sleep(),
            "fresh controller must accept enter_sleep"
        );
        let sleep_pwm = clamp_fan_pwm(curt.sleep_fan_pwm());
        assert!(
            sleep_pwm <= dcentrald_hal::fan::PWM_SAFETY_MAX,
            "curtailment sleep fan PWM {} must be within the PWM-{} home cap",
            sleep_pwm,
            dcentrald_hal::fan::PWM_SAFETY_MAX
        );
    }

    #[test]
    fn scheduled_curtailment_only_drives_sleep_then_wake() {
        // Mirror the driver's state machine: in-window -> enter_sleep (the only
        // power-down direction); the thermal loop completes the transition; then
        // out-of-window -> wake. The controller can never be driven into a
        // hash-UP / fan-UP state by this driver — there is no such call site.
        use dcentrald_thermal::curtailment::CurtailmentState;
        let mut curt = dcentrald_thermal::curtailment::CurtailmentController::new();
        assert_eq!(curt.state(), CurtailmentState::Active);

        // Window opens: driver calls enter_sleep().
        assert!(curt.enter_sleep());
        assert_eq!(curt.state(), CurtailmentState::EnteringSleep);
        // Thermal loop finishes the power-down.
        curt.sleep_complete();
        assert_eq!(curt.state(), CurtailmentState::Sleeping);
        assert!(curt.is_sleeping());

        // Window closes: driver calls wake() (only succeeds from Sleeping).
        assert!(curt.wake());
        assert_eq!(curt.state(), CurtailmentState::Waking);
        curt.wake_complete();
        assert_eq!(curt.state(), CurtailmentState::Active);
        assert!(!curt.is_sleeping());
    }

    #[test]
    fn scheduled_curtailment_window_uses_pure_config_predicate() {
        // The async driver computes `in_window` via the same pure predicate the
        // config tests cover, so a configured window deterministically maps the
        // current hour to sleep/run with no hidden state.
        use crate::config::CurtailmentScheduleConfig;
        let cfg = CurtailmentScheduleConfig {
            enabled: true,
            start_hour: 22,
            end_hour: 6,
            poll_interval_s: 60,
            timezone_offset_hours: 0,
        };
        assert!(cfg.is_active_at_hour(23)); // inside off-peak -> sleep
        assert!(cfg.is_active_at_hour(2)); // wraps midnight -> sleep
        assert!(!cfg.is_active_at_hour(12)); // daytime -> run normally

        // FWSTAB-1: the daemon evaluates the window against the operator's LOCAL
        // hour (UTC + offset). With an EST (-5) offset the same 22:00-06:00 LOCAL
        // window means 03:00 UTC == 22:00 EST is inside; 18:00 UTC == 13:00 EST is
        // outside — i.e. the window no longer fires at the wrong UTC wall-clock.
        use dcentrald_common::time::local_hour_from_utc;
        let est = -5i8;
        assert!(cfg.is_active_at_hour(local_hour_from_utc(3, est))); // 22:00 local
        assert!(!cfg.is_active_at_hour(local_hour_from_utc(18, est))); // 13:00 local
    }

    // ----- ATM (Advanced Thermal Management) profile-step wiring -----
    //
    // These pin the dormant-advisory wiring: the thermal supervisor emits
    // `RequestProfileStepDown` / `RequestProfileStepUp`, and the daemon now
    // consumes them by driving the autotuner's `FrequencyLimitSource::AtmStep`
    // ceiling. The decision math lives in the pure `atm_step_ceiling_decision`
    // helper; the gate-off contract is pinned against the real supervisor FSM.

    // Mirrors the daemon's loop constants so the tests exercise the same math.
    const TEST_ATM_NOMINAL: u16 = 545;
    const TEST_ATM_STEP: u16 = TEST_ATM_NOMINAL / 12; // 45
    const TEST_ATM_FLOOR: u16 = 200;

    fn atm_down(current: Option<u16>, cutting_hash: bool, debounced: bool) -> Option<u16> {
        atm_step_ceiling_decision(
            AtmStepDir::Down,
            current,
            TEST_ATM_NOMINAL,
            TEST_ATM_STEP,
            TEST_ATM_FLOOR,
            cutting_hash,
            debounced,
        )
    }

    fn atm_up(current: Option<u16>, cutting_hash: bool, debounced: bool) -> Option<u16> {
        atm_step_ceiling_decision(
            AtmStepDir::Up,
            current,
            TEST_ATM_NOMINAL,
            TEST_ATM_STEP,
            TEST_ATM_FLOOR,
            cutting_hash,
            debounced,
        )
    }

    // -- Gate-off: a DISABLED supervisor emits NO profile-step advisories, so
    //    the daemon capture stays None and NO AtmStep command is ever sent. --
    #[test]
    fn atm_gate_off_disabled_supervisor_emits_no_profile_steps() {
        use dcentrald_thermal::supervisor::{
            BoardSensors, SupervisorAction, ThermalSupervisor, ThermalSupervisorConfig, ThermalTick,
        };
        // Default config => enabled = false (operator opt-in).
        let mut sup = ThermalSupervisor::new(ThermalSupervisorConfig::default());
        assert!(!sup.is_enabled(), "supervisor must be default-OFF");
        // Even a chain hot enough to step-down produces ZERO actions while off.
        let tick = ThermalTick {
            board_sensors: vec![BoardSensors {
                chain_id: 0,
                pcb_temps_c: vec![95.0, 95.0],
                chip_temps_c: vec![99.0],
                powered_on: true,
            }],
            fan_tach_rpms: vec![1000],
            current_fan_pwm: 30,
            hydro_inlet_c: None,
            hydro_outlet_c: None,
            tick_elapsed_secs: 5,
        };
        let actions = sup.tick(&tick);
        assert!(
            actions.is_empty(),
            "disabled supervisor must emit no actions (no RequestProfileStep*) — got {actions:?}"
        );
        // The daemon's capture only fires on a RequestProfileStep* in the action
        // set; with none present, no AtmStep command is dispatched.
        assert!(!actions.iter().any(|a| matches!(
            a,
            SupervisorAction::RequestProfileStepDown { .. }
                | SupervisorAction::RequestProfileStepUp
        )));
    }

    // -- Step-DOWN lowers the ceiling (the safe cut-hash-before-noise dir). --
    #[test]
    fn atm_step_down_lowers_ceiling() {
        // From no constraint, the first step-down starts at nominal − one step.
        let first = atm_down(None, false, false);
        assert_eq!(first, Some(TEST_ATM_NOMINAL - TEST_ATM_STEP));
        // A subsequent step-down lowers further.
        let second = atm_down(first, false, false);
        assert_eq!(second, Some(TEST_ATM_NOMINAL - 2 * TEST_ATM_STEP));
        assert!(
            second.unwrap() < first.unwrap(),
            "each step-down must strictly lower the ceiling"
        );
    }

    // -- Step-DOWN never drives the ceiling below the minable floor. --
    #[test]
    fn atm_step_down_is_floored() {
        // Start near the floor; many step-downs must clamp at the floor, never
        // underflow to an unminable frequency.
        let mut ceiling = Some(TEST_ATM_FLOOR + 10);
        for _ in 0..20 {
            ceiling = atm_down(ceiling, false, false);
        }
        assert_eq!(ceiling, Some(TEST_ATM_FLOOR));
    }

    // -- Step-DOWN is the SAFE direction: honored even while hash is cut. --
    #[test]
    fn atm_step_down_honored_even_when_cutting_hash() {
        let lowered = atm_down(Some(TEST_ATM_NOMINAL), /*cutting_hash=*/ true, false);
        assert_eq!(lowered, Some(TEST_ATM_NOMINAL - TEST_ATM_STEP));
    }

    // -- Step-UP is BOUNDED by the configured nominal ceiling. --
    #[test]
    fn atm_step_up_is_ceiling_bounded() {
        // From two steps down, stepping up rises but stays below nominal.
        let two_down = Some(TEST_ATM_NOMINAL - 2 * TEST_ATM_STEP);
        let up_once = atm_up(two_down, false, false);
        assert_eq!(up_once, Some(TEST_ATM_NOMINAL - TEST_ATM_STEP));
        // One more step-up reaches/exceeds nominal => the ATM ceiling CLEARS
        // (None) rather than commanding ABOVE the configured/SKU max.
        let up_twice = atm_up(up_once, false, false);
        assert_eq!(
            up_twice, None,
            "at/above nominal the ATM ceiling must clear, never exceed nominal"
        );
        // An already-unconstrained ceiling stays unconstrained on step-up —
        // it can never be pushed above the configured max.
        assert_eq!(atm_up(None, false, false), None);
    }

    // -- A hot event during a step-up WINS: no step-up while hash is cut. --
    #[test]
    fn atm_hot_event_during_step_up_wins() {
        let two_down = Some(TEST_ATM_NOMINAL - 2 * TEST_ATM_STEP);
        // Same tick the reconciled thermal action is cutting hash (hot) => the
        // step-up is refused and the ceiling is left unchanged (stays low).
        let held = atm_up(two_down, /*cutting_hash=*/ true, false);
        assert_eq!(
            held, two_down,
            "a hot/hash-cut tick must SUPPRESS step-up (thermal safety wins)"
        );
    }

    // -- Debounce/rate-limit: a step (either dir) is held inside the window. --
    #[test]
    fn atm_step_is_debounced() {
        // Within the debounce window both directions return `current` unchanged
        // so a flapping temperature cannot thrash the profile.
        let cur = Some(TEST_ATM_NOMINAL - TEST_ATM_STEP);
        assert_eq!(atm_down(cur, false, /*debounced=*/ true), cur);
        assert_eq!(atm_up(cur, false, /*debounced=*/ true), cur);
    }

    // -- The live supervisor DOES emit a step-down advisory when hot + enabled,
    //    confirming the capture path has something real to consume. --
    #[test]
    fn atm_enabled_supervisor_emits_step_down_when_hot() {
        use dcentrald_thermal::supervisor::{
            BoardSensors, SupervisorAction, ThermalSupervisor, ThermalSupervisorConfig, ThermalTick,
        };
        // Enabled + zero grace so the step-down can fire immediately.
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            atm_startup_grace_secs: 0,
            atm_post_ramp_grace_secs: 0,
            ..ThermalSupervisorConfig::default()
        };
        let mut sup = ThermalSupervisor::new(cfg);
        let tick = ThermalTick {
            board_sensors: vec![BoardSensors {
                chain_id: 0,
                pcb_temps_c: vec![66.0, 66.0], // above board_hot 65
                chip_temps_c: vec![80.0],
                powered_on: true,
            }],
            fan_tach_rpms: vec![1000],
            current_fan_pwm: 30,
            hydro_inlet_c: None,
            hydro_outlet_c: None,
            tick_elapsed_secs: 1,
        };
        let actions = sup.tick(&tick);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestProfileStepDown { .. })),
            "an enabled, hot supervisor must emit RequestProfileStepDown for the daemon to consume — got {actions:?}"
        );
    }

    // ----- THERM-2: curtailment-SLEEP fan respects the per-profile quiet cap -----
    //
    // The sleep arm of the thermal loop now drives
    // `effective_sleep_fan_pwm(curt.sleep_fan_pwm(), cfg_fan_max_pwm)`. Prove the
    // pure helper never exceeds EITHER the home PWM-30 cap OR a lower per-profile
    // ceiling, and that it can only ever LOWER the requested PWM (never raise it).

    #[test]
    fn therm2_sleep_fan_respects_min_of_safety_and_profile_cap() {
        // A profile quieter than the home cap (fan_max_pwm = 15): a sleep request
        // of 30 must clamp DOWN to the profile's 15, not stay at the 30 home cap.
        assert_eq!(effective_sleep_fan_pwm(30, 15), 15);
        // A profile at the home cap (30): a sleep request of 30 stays at 30.
        assert_eq!(effective_sleep_fan_pwm(30, 30), 30);
        // A profile ABOVE the home cap (e.g. an --allow-loud 100 ceiling): the
        // PWM-30 home cap still wins for the quiet sleep state.
        assert_eq!(effective_sleep_fan_pwm(100, 100), FAN_PWM_SAFETY_MAX);
        // A sleep request below both caps passes through unchanged.
        assert_eq!(effective_sleep_fan_pwm(10, 30), 10);
    }

    #[test]
    fn normalize_fan_pwm_bounds_always_yields_min_le_max_le_ceiling() {
        // The thermal-action handlers do `pwm.clamp(cfg_fan_min_pwm, cfg_fan_max_pwm)`,
        // and u8::clamp PANICS when min > max. cfg_fan_min/max are derived from
        // normalize_fan_pwm_bounds, so ITS min <= max guarantee is what keeps an
        // inverted or out-of-range fan config from crashing the daemon (panic=abort
        // -> boards energized with NO thermal loop). Pin the invariant.
        assert_eq!(normalize_fan_pwm_bounds(0, 30), (0, 30)); // valid pair unchanged
        assert_eq!(normalize_fan_pwm_bounds(10, 30), (10, 30));
        assert_eq!(normalize_fan_pwm_bounds(80, 30), (30, 30)); // inverted -> min pulled to max
        assert_eq!(normalize_fan_pwm_bounds(0, 200), (0, FAN_PWM_MAX)); // over-range max clamped
        assert_eq!(
            normalize_fan_pwm_bounds(255, 200),
            (FAN_PWM_MAX, FAN_PWM_MAX)
        );

        // Exhaustive over the whole u8 x u8 space: min <= max <= FAN_PWM_MAX and it
        // never panics — so the handler clamp can never see min > max.
        for lo in 0u16..=255 {
            for hi in 0u16..=255 {
                let (m, x) = normalize_fan_pwm_bounds(lo as u8, hi as u8);
                assert!(
                    m <= x,
                    "min {m} > max {x} for input ({lo},{hi}) — u8::clamp would panic"
                );
                assert!(
                    x <= FAN_PWM_MAX,
                    "max {x} exceeds the hardware ceiling for input ({lo},{hi})"
                );
            }
        }
    }

    #[test]
    fn therm2_sleep_fan_never_exceeds_either_bound() {
        // Exhaustive over every (sleep request, profile cap) pair: the effective
        // sleep PWM is always <= the home cap, <= the profile cap, and never
        // greater than the requested value (sleep can only LOWER fans).
        for sleep_req in 0u8..=255 {
            for cap in 0u8..=100 {
                let eff = effective_sleep_fan_pwm(sleep_req, cap);
                assert!(
                    eff <= FAN_PWM_SAFETY_MAX,
                    "sleep {sleep_req} cap {cap} -> {eff} > safety"
                );
                assert!(
                    eff <= cap,
                    "sleep {sleep_req} cap {cap} -> {eff} > profile cap"
                );
                assert!(
                    eff <= clamp_fan_pwm(sleep_req),
                    "sleep {sleep_req} cap {cap} -> {eff} raised above the request"
                );
            }
        }
    }

    // ----- THERM-3: thermal-emergency voltage-disable is fail-closed -----
    //
    // The EmergencyShutdown / FanFailure arms now compute the per-round result
    // via `thermal_disable_round_ok(channel_present, all_addrs_acked)`. Prove the
    // round can only be reported successful when the voltage channel exists AND
    // every controller acked — and specifically that a MISSING channel (None tx)
    // fails closed instead of silently claiming all boards disabled.

    #[test]
    fn therm3_round_fails_closed_without_voltage_channel() {
        // No channel (thermal_voltage_tx == None): even if nothing reported a
        // per-addr failure, the round MUST be false — no command could be sent.
        assert!(!thermal_disable_round_ok(false, true));
        assert!(!thermal_disable_round_ok(false, false));
    }

    #[test]
    fn therm3_round_ok_only_when_channel_present_and_all_acked() {
        // Happy path (S9): channel present + all controllers acked -> success.
        assert!(thermal_disable_round_ok(true, true));
        // Channel present but a controller failed/timed out -> retry (false).
        assert!(!thermal_disable_round_ok(true, false));
    }

    // ----- SAF-4: thermal-emergency voltage re-enable interlock -----
    //
    // Runtime SetVoltage is refused while this latch is set. The thermal loop
    // sets it when hash is cut for EmergencyShutdown/FanFailure and clears it
    // only when the controller emits RestartInit after cooldown.

    #[test]
    fn saf4_thermal_emergency_latch_blocks_voltage_until_restart_clears() {
        let latch = AtomicBool::new(false);
        assert!(!thermal_emergency_active(&latch));

        mark_thermal_emergency_active(&latch);
        assert!(thermal_emergency_active(&latch));

        clear_thermal_emergency_active(&latch);
        assert!(!thermal_emergency_active(&latch));
    }

    // THERMAL-8: the non-XADC (Amlogic) fail-closed escalation. The decision math
    // lives in the pure `thermal8_board_blind_tick(has_xadc, had_board_temp_proof,
    // prev_failures, limit, action_is_emergency) -> (new_failures, escalate)`.
    // Prove: sustained TOTAL board-temp blindness on a non-XADC platform escalates
    // to EmergencyShutdown; a single real board temp resets the counter; the XADC
    // (Zynq beta) path is a permanent no-op; a single empty tick never escalates;
    // and a more-severe action already chosen is never weakened.
    #[test]
    fn thermal8_escalates_only_on_sustained_total_blindness_non_xadc() {
        const LIMIT: u32 = 5;
        // Non-XADC + no board-temp proof: counter climbs but does NOT escalate
        // until it REACHES the limit (a single/early empty tick is never enough).
        let mut failures = 0u32;
        for tick in 1..LIMIT {
            let (next, escalate) = thermal8_board_blind_tick(false, false, failures, LIMIT, false);
            failures = next;
            assert_eq!(
                failures, tick,
                "counter must climb one per fully-blind tick"
            );
            assert!(
                !escalate,
                "must NOT escalate before {LIMIT} ticks (never on a single empty tick)"
            );
        }
        // The limit-th consecutive blind tick escalates to fail-closed shutdown.
        let (next, escalate) = thermal8_board_blind_tick(false, false, failures, LIMIT, false);
        assert_eq!(next, LIMIT);
        assert!(
            escalate,
            "sustained TOTAL blindness must escalate at the limit"
        );
    }

    #[test]
    fn thermal8_real_board_temp_resets_counter() {
        const LIMIT: u32 = 5;
        // Climb close to the limit on a non-XADC platform...
        let (failures, escalate) = thermal8_board_blind_tick(false, false, LIMIT - 1, LIMIT, false);
        assert_eq!(failures, LIMIT - 1 + 1);
        assert!(escalate, "would escalate at the limit");
        // ...but a single tick WITH real board-temp proof zeroes the counter and
        // never escalates — the "never emergency on empty board temps ALONE" rule.
        let (reset, escalate) = thermal8_board_blind_tick(false, true, LIMIT, LIMIT, false);
        assert_eq!(reset, 0, "any real board temp resets the blindness counter");
        assert!(!escalate);
    }

    #[test]
    fn thermal7_escalates_only_on_sustained_total_xadc_blindness() {
        const LIMIT: u32 = 5;
        // Not an XADC platform -> THERMAL-8 covers it; THERMAL-7 never fires.
        assert!(!thermal7_xadc_blind_escalates(
            false, true, false, 99, LIMIT, false
        ));
        // A good XADC read this tick -> not blind -> no escalation.
        assert!(!thermal7_xadc_blind_escalates(
            true, false, false, 99, LIMIT, false
        ));
        // A real board temp covered this tick -> not blind -> no escalation.
        assert!(!thermal7_xadc_blind_escalates(
            true, true, true, 99, LIMIT, false
        ));
        // Blind but under the limit -> a single/short blind streak can NEVER trip it.
        assert!(!thermal7_xadc_blind_escalates(
            true,
            true,
            false,
            LIMIT - 1,
            LIMIT,
            false
        ));
        // Sustained TOTAL blindness at/over the limit -> ESCALATE (fail-closed).
        assert!(thermal7_xadc_blind_escalates(
            true, true, false, LIMIT, LIMIT, false
        ));
        assert!(thermal7_xadc_blind_escalates(
            true,
            true,
            false,
            LIMIT + 3,
            LIMIT,
            false
        ));
        // Never WEAKEN an action that is already an emergency response.
        assert!(!thermal7_xadc_blind_escalates(
            true,
            true,
            false,
            LIMIT + 3,
            LIMIT,
            true
        ));
    }

    #[test]
    fn thermal8_is_noop_on_xadc_platform() {
        const LIMIT: u32 = 5;
        // On an XADC (Zynq) platform the counter is pinned at 0 and never
        // escalates regardless of board-temp state — the beta path is byte-identical.
        for proof in [true, false] {
            let (failures, escalate) =
                thermal8_board_blind_tick(true, proof, LIMIT + 10, LIMIT, false);
            assert_eq!(failures, 0, "XADC platform: counter must stay 0");
            assert!(!escalate, "XADC platform: THERMAL-8 must never fire");
        }
    }

    #[test]
    fn thermal8_never_weakens_an_existing_emergency() {
        const LIMIT: u32 = 5;
        // Sustained blindness, but the action is ALREADY an emergency this tick:
        // do not re-escalate (never WEAKEN a more-severe action already chosen).
        let (failures, escalate) = thermal8_board_blind_tick(
            false, false, LIMIT, LIMIT, /* already_emergency */ true,
        );
        assert_eq!(failures, LIMIT + 1, "counter still advances");
        assert!(
            !escalate,
            "must not double-escalate when already in EmergencyShutdown"
        );
    }
}

#[cfg(test)]
mod pic_backoff_tests {
    //! WAVE-0 STABILIZE: per-PIC heartbeat back-off state machine.
    //!
    //! These tests pin the documented contract ("skip after 10 failures,
    //! reprobe every 30s, declare dead") and the NACK-log rate-limiting that
    //! stops the ~33x/s storm seen on the live S9 (audit B2/B3). Pure logic —
    //! the clock is injected, so no hardware or sleeping is involved.
    use super::{
        HbAction, PicBackoff, PicHbState, PIC_BACKOFF_FAIL_THRESHOLD, PIC_BACKOFF_LOG_BURST,
        PIC_BACKOFF_REPROBE_SECS,
    };

    #[test]
    fn fresh_pic_is_active_and_beats() {
        let b = PicBackoff::new();
        assert_eq!(b.state(), PicHbState::Active);
        assert!(b.is_healthy());
        assert_eq!(b.decide(0), HbAction::Beat);
    }

    #[test]
    fn success_keeps_active_and_healthy() {
        let mut b = PicBackoff::new();
        // First success is not a "recovery" (was already healthy).
        assert!(!b.record_success());
        assert_eq!(b.decide(5), HbAction::Beat);
        assert!(b.is_healthy());
    }

    #[test]
    fn stays_in_hot_path_below_threshold() {
        let mut b = PicBackoff::new();
        for i in 1..PIC_BACKOFF_FAIL_THRESHOLD {
            b.record_failure(0, false);
            assert_eq!(
                b.state(),
                PicHbState::Active,
                "still Active before threshold (fail #{i})"
            );
            assert_eq!(b.decide(0), HbAction::Beat, "still beats before threshold");
        }
        assert_eq!(b.consecutive_failures(), PIC_BACKOFF_FAIL_THRESHOLD - 1);
        assert!(!b.is_healthy(), "a failing-but-Active PIC is not healthy");
    }

    #[test]
    fn transitions_to_backing_off_at_threshold_and_schedules_reprobe() {
        let mut b = PicBackoff::new();
        let mut transition_logged = false;
        for _ in 0..PIC_BACKOFF_FAIL_THRESHOLD {
            transition_logged = b.record_failure(0, false);
        }
        assert_eq!(b.state(), PicHbState::BackingOff);
        assert_eq!(b.consecutive_failures(), PIC_BACKOFF_FAIL_THRESHOLD);
        assert!(
            transition_logged,
            "the Active->BackingOff transition must be logged"
        );
        // Immediately after backing off, the PIC is skipped (not due to reprobe).
        assert_eq!(b.decide(0), HbAction::Skip);
        // ... until the reprobe interval elapses.
        assert_eq!(b.decide(PIC_BACKOFF_REPROBE_SECS - 1), HbAction::Skip);
        assert_eq!(b.decide(PIC_BACKOFF_REPROBE_SECS), HbAction::Reprobe);
    }

    #[test]
    fn backed_off_pic_is_skipped_silently_between_reprobes() {
        // This is the core NACK-storm fix: while backed off and not due, we
        // neither poke the bus (Skip) nor would we ever log a failure for it.
        let mut b = PicBackoff::new();
        for _ in 0..PIC_BACKOFF_FAIL_THRESHOLD {
            b.record_failure(0, false);
        }
        assert_eq!(b.state(), PicHbState::BackingOff);
        for t in 0..PIC_BACKOFF_REPROBE_SECS {
            assert_eq!(b.decide(t), HbAction::Skip, "skipped at t={t}");
        }
    }

    #[test]
    fn failed_reprobe_declares_dead_and_keeps_reprobing() {
        let mut b = PicBackoff::new();
        for _ in 0..PIC_BACKOFF_FAIL_THRESHOLD {
            b.record_failure(0, false);
        }
        // First reprobe is due at REPROBE_SECS and fails -> Dead.
        assert_eq!(b.decide(PIC_BACKOFF_REPROBE_SECS), HbAction::Reprobe);
        let logged = b.record_failure(PIC_BACKOFF_REPROBE_SECS, true);
        assert_eq!(b.state(), PicHbState::Dead);
        assert!(logged, "the BackingOff->Dead transition is logged");
        // Dead still reprobes on the same cadence (re-seated board recovers).
        assert_eq!(b.decide(PIC_BACKOFF_REPROBE_SECS), HbAction::Skip);
        assert_eq!(
            b.decide(2 * PIC_BACKOFF_REPROBE_SECS),
            HbAction::Reprobe,
            "a dead PIC is still reprobed every interval"
        );
    }

    #[test]
    fn reprobe_success_recovers_to_active() {
        let mut b = PicBackoff::new();
        for _ in 0..PIC_BACKOFF_FAIL_THRESHOLD {
            b.record_failure(0, false);
        }
        assert_eq!(b.decide(PIC_BACKOFF_REPROBE_SECS), HbAction::Reprobe);
        // The reprobe succeeds -> full recovery, logged as recovery.
        assert!(
            b.record_success(),
            "recovery from back-off must be reported"
        );
        assert_eq!(b.state(), PicHbState::Active);
        assert!(b.is_healthy());
        assert_eq!(b.decide(PIC_BACKOFF_REPROBE_SECS), HbAction::Beat);
    }

    #[test]
    fn log_is_rate_limited_to_burst_then_quiet_until_transition() {
        let mut b = PicBackoff::new();
        let mut logged_count = 0u32;
        // The hot-path streak below threshold: only the first LOG_BURST log.
        for n in 1..PIC_BACKOFF_FAIL_THRESHOLD {
            if b.record_failure(0, false) {
                logged_count += 1;
            }
            // After the burst, hot-path failures are silent.
            if n > PIC_BACKOFF_LOG_BURST {
                assert_eq!(
                    logged_count, PIC_BACKOFF_LOG_BURST,
                    "no extra logs between the burst and the transition (n={n})"
                );
            }
        }
        // The threshold failure (the transition) IS logged.
        assert!(b.record_failure(0, false));
        logged_count += 1;
        assert_eq!(
            logged_count,
            PIC_BACKOFF_LOG_BURST + 1,
            "burst ({PIC_BACKOFF_LOG_BURST}) + 1 transition log over a {PIC_BACKOFF_FAIL_THRESHOLD}-fail streak"
        );
    }

    #[test]
    fn a_skipped_non_reprobe_failure_is_never_logged() {
        // decide() returns Skip for a backed-off PIC, so the loop never even
        // calls heartbeat() for it — but defensively, record_failure with
        // was_reprobe=false while backed off must not log.
        let mut b = PicBackoff::new();
        for _ in 0..PIC_BACKOFF_FAIL_THRESHOLD {
            b.record_failure(0, false);
        }
        assert_eq!(b.state(), PicHbState::BackingOff);
        assert!(
            !b.record_failure(5, false),
            "a non-reprobe failure while backed off must be silent"
        );
    }
}
