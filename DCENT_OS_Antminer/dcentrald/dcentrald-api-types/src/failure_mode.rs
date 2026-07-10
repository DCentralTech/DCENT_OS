//!  fail-A — Failure-mode classification + recommended-action
//! dispatch (HAL-free).
//!
//! Source RE evidence:
//!
//! (199 lines, consolidated from 5-system-orchestration + workspace memory).
//!
//! Each documented failure has:
//! - A canonical `FailureMode` discriminant.
//! - A severity classification (CRITICAL / HIGH / MEDIUM / LOW per RE
//!   doc lines 144-162).
//! - A recommended `RecoveryAction` for the runtime adapter.
//!
//! HAL-free: pure data + classifier function. The runtime adapter
//! consumes verdicts to decide whether to:
//! - Cut voltage (PIC heartbeat fail).
//! - Cap fans at mode-cap (sensor stale, fan failure).
//! - Re-init the chain (UART relay register wrong).
//! - Backoff + retry (Stratum stall).
//! - Halt + alert operator (CRITICAL — hardware damage risk).

use serde::{Deserialize, Serialize};

/// Canonical failure-mode catalog. Add new variants only at the end of
/// the enum to keep the wire form append-friendly across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    // --- Boot phase ---
    /// FSBL corruption — JTAG flash, factory only.
    FsblCorruption,
    /// U-Boot env corrupt → wrong slot mounts.
    UbootEnvCorrupt,
    /// uImage corrupt → kernel panic.
    UimageCorrupt,
    /// rootfs UBI corrupt → "VFS: Cannot open root device".
    RootfsUbiCorrupt,
    /// Missing libubootenv-tools on am2 DCENT_OS — brick-back latent.
    Am2MissingFwSetenv,
    /// Service script fails — partial-up state.
    ServiceScriptFail,
    /// dcentrald crashes during mining → dashboard "DEAD" banner.
    DcentraldCrash,
    /// dcentrald http_port=80 conflicts with server.py on am2.
    DcentraldHttpPortCollision,
    /// `--s19j-hybrid` bypasses daemon.rs::run() → :8080 + :4028 unbound.
    HybridSkipsApiServer,

    // --- Watchdog miss ---
    /// PSU watchdog miss (~30 s) — PSU latches OFF, all chains die.
    PsuWatchdogMiss,
    /// PIC heartbeat miss (~10 s BraiinsOS / ~60 s stock) — chain dies.
    PicHeartbeatMiss,

    // --- ASIC chain dead ---
    /// GetAddress returns 0 chips → wrong CMD ID for chip family.
    GetAddressZeroChips,
    /// GetAddress returns half the chips → single chip stuck mid-relay.
    GetAddressPartialChips,
    /// ChainInactive single-write didn't quiet the chain.
    ChainInactiveSingleWriteMiss,
    /// SetChipAddress only addresses chip 0 — wrong header.
    SetChipAddressBroadcastError,
    /// MiscCtrl single-write — chip parser dropped a byte.
    MiscCtrlSingleWriteRace,
    /// All chips enumerated but no nonces — BM1387 skipped open-core.
    Bm1387MissingOpenCore,
    /// All chips enumerated but no nonces — BM1362 ASIC reg `0x2C`
    /// UART_RELAY not written. This is the canonical control register on
    /// the chip (broadcast via SerialChainBackend). NOT the FPGA mirror at
    /// `0x43D000xx` (which is a Braiins-am2-only diagnostic mirror, NOT a
    /// control surface — see `dcentrald_hal::glitch_monitor` and W13.B1).
    /// Variant name kept for backwards-compat with consumers; only the
    /// doc was misleading.
    Bm1362UartRelayUnwritten,
    /// First-write SET_VOLTAGE NACKed PIC — heartbeat not stable enough.
    SetVoltageBeforeStableTickGate,
    /// 75-second cliff (S9): MiscCtrl single-write race after open-core.
    SeventyFiveSecCliff,

    // --- Sysupgrade ---
    /// Inactive UBI slot volume mismatch — "no such volume" on flip.
    UbiVolumeMismatch,
    /// switch_firmware.py crash — slot flip never happens.
    SwitchFirmwarePythonCrash,
    /// Stale dcentrald binary in overlay — sysupgrade ships old code.
    StaleOverlayBinary,
    /// Sysupgrade tarball name wrong → fails or bricks.
    SysupgradeTarballNameWrong,
    /// Hot binary swap killed PIC heartbeat task with SIGKILL.
    HotSwapSigkillKilledPic,

    // --- Stratum-side ---
    /// Pool silently drops `mining.configure` — no reply within 5 s.
    PoolMiningConfigureNoReply,
    /// Pool returned reject Code 21 (job not found / stale share).
    PoolRejectStale,
    /// Pool returned reject Code 22 (duplicate share — bug in dedup).
    PoolRejectDuplicate,
    /// Pool returned reject Code 23 (low difficulty).
    PoolRejectLowDifficulty,
    /// Pool returned reject Code 27 (invalid version mask).
    PoolRejectInvalidVersionMask,
    /// TCP drop / no notify >120 s.
    StratumStall,
    /// All pools exhausted.
    AllPoolsExhausted,
    /// Pool authentication fails (Code 24).
    PoolUnauthorized,
    /// SV2 Standard channel exhausts nonce range in 2.5 s on S9-class.
    Sv2StandardChannelNonceExhaustion,

    // --- Power / thermal ---
    /// Temp sensor error — reading 0 or wildly out of range.
    TempSensorError,
    /// Fan failure: RPM=0 with PWM>0.
    FanFailure,
    /// Amlogic spurious RPM=0 — brief glitch, NOT a failure.
    AmlogicSpuriousRpmZero,
    /// Emergency temp ≥ 75 °C — thermal runaway.
    EmergencyOvertemp,
    /// INA226 off-grid power monitor returns 0 V.
    Ina226Failure,

    // ---  fail-B additions ---
    /// Dashboard / API server.py failed to bind port 80 — UI lost. Per
    /// : dcentrald
    /// http_port=80 conflicts with always-on server.py on am2.
    DashboardFailedToBind,
    /// Sysupgrade tarball name doesn't match expected board (e.g.
    /// `sysupgrade-am3-s19kpro/` written for `am1-s9` target). Brick
    /// risk.
    FirmwareSlotMismatch,
    /// PSU OUT1 below 12 V threshold during cold boot — rail engaged
    /// but undervoltage. Distinct from `PsuWatchdogMiss` (which is the
    /// timeout-side cut).
    PsuRailUndervoltage,
    /// GPIO reset on S21 NoPic kills TAS5782M DAC voltage —
    /// recoverable only by AC power cycle. Per
    /// .
    S21NoPicGpioReset,
    /// Raw sysfs `HBx_RESET` write on am2 drops PWR_CONTROL — must use
    /// `BoardControl::pulse_reset()` RMW helper. Per
    /// .
    Am2RawSysfsHbReset,
    /// I2C unbind / SOFTR on S19 Pro breaks PSU. Reboot required. Per
    /// .
    S19ProI2cUnbindBreaksPsu,
    /// Kernel xiic_reinit zeroed the AXI IIC THIGH/TLOW/TBUF timing
    /// registers — restore to 1498/1498/499 (8 values, 300 MHz FCLK).
    ///.
    AxiIicClockDriftAfterSoftr,
    /// AM2/AM3 dsPIC reports fw=0x86, the degraded post-reset state.
    /// Voltage/enable must be refused by default; recovery is operator/lab
    /// only and never an automatic daemon action.
    Am2DegradedDspicFirmware,
}

/// Severity per RE doc lines 144-162.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSeverity {
    /// Cosmetic UI issue / log noise / rare edge case.
    Low,
    /// Mining degraded but recoverable. Specific chain dead, others fine.
    Medium,
    /// Mining stops, manual intervention OR re-init required.
    High,
    /// Hardware damage risk (PIC FW=0x89 RESET, PSU/PIC voltage misorder)
    /// OR brick-back latent OR repeated user burn (fan PWM 127).
    Critical,
}

/// Recommended runtime action for a given failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// No automated action — operator must intervene.
    OperatorIntervention,
    /// Re-init the entire daemon (e.g. cold-boot orchestration).
    DaemonReinit,
    /// Re-init only the affected chain (4-12 s).
    ChainReinit,
    /// Full PSU re-init sequence (60-90 s).
    PsuReinit,
    /// Cut voltage to ASICs and cap fans at mode-cap.
    CutVoltageAndCapFans,
    /// Triple-write the affected register with 5 ms spacing.
    TripleWriteRegister,
    /// Retry the operation with exponential backoff.
    BackoffRetry,
    /// Disable the affected feature; continue mining without it.
    DisableFeatureContinue,
    /// Count separately (expected) — no action.
    CountAndContinue,
    /// Boot recovery slot via U-Boot env switch.
    BootRecoverySlot,
    /// Reload daemon with SIGTERM (graceful) instead of SIGKILL.
    GracefulRestart,
    /// Re-apply AXI IIC THIGH/TLOW/TBUF timing registers after a
    /// kernel SOFTR / xiic_reinit zeroed them.  addition.
    RestoreClockTiming,
}

/// Verdict for a single failure mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FailureVerdict {
    pub severity: FailureSeverity,
    pub recovery: RecoveryAction,
}

pub const FAILURE_MODE_CATALOG_SCHEMA: &str = "dcentos.failure.catalog.v1";
pub const OBSERVED_FAILURE_SNAPSHOT_SCHEMA: &str = "dcentos.failure.observed.snapshot.v1";
pub const DCENT_RECOVERY_GUIDANCE_SCHEMA: &str = "dcentos.failure.recovery_guidance.v1";

pub const ALL_FAILURE_MODES: &[FailureMode] = &[
    FailureMode::FsblCorruption,
    FailureMode::UbootEnvCorrupt,
    FailureMode::UimageCorrupt,
    FailureMode::RootfsUbiCorrupt,
    FailureMode::Am2MissingFwSetenv,
    FailureMode::ServiceScriptFail,
    FailureMode::DcentraldCrash,
    FailureMode::DcentraldHttpPortCollision,
    FailureMode::HybridSkipsApiServer,
    FailureMode::PsuWatchdogMiss,
    FailureMode::PicHeartbeatMiss,
    FailureMode::GetAddressZeroChips,
    FailureMode::GetAddressPartialChips,
    FailureMode::ChainInactiveSingleWriteMiss,
    FailureMode::SetChipAddressBroadcastError,
    FailureMode::MiscCtrlSingleWriteRace,
    FailureMode::Bm1387MissingOpenCore,
    FailureMode::Bm1362UartRelayUnwritten,
    FailureMode::SetVoltageBeforeStableTickGate,
    FailureMode::SeventyFiveSecCliff,
    FailureMode::UbiVolumeMismatch,
    FailureMode::SwitchFirmwarePythonCrash,
    FailureMode::StaleOverlayBinary,
    FailureMode::SysupgradeTarballNameWrong,
    FailureMode::HotSwapSigkillKilledPic,
    FailureMode::PoolMiningConfigureNoReply,
    FailureMode::PoolRejectStale,
    FailureMode::PoolRejectDuplicate,
    FailureMode::PoolRejectLowDifficulty,
    FailureMode::PoolRejectInvalidVersionMask,
    FailureMode::StratumStall,
    FailureMode::AllPoolsExhausted,
    FailureMode::PoolUnauthorized,
    FailureMode::Sv2StandardChannelNonceExhaustion,
    FailureMode::TempSensorError,
    FailureMode::FanFailure,
    FailureMode::AmlogicSpuriousRpmZero,
    FailureMode::EmergencyOvertemp,
    FailureMode::Ina226Failure,
    FailureMode::DashboardFailedToBind,
    FailureMode::FirmwareSlotMismatch,
    FailureMode::PsuRailUndervoltage,
    FailureMode::S21NoPicGpioReset,
    FailureMode::Am2RawSysfsHbReset,
    FailureMode::S19ProI2cUnbindBreaksPsu,
    FailureMode::AxiIicClockDriftAfterSoftr,
    FailureMode::Am2DegradedDspicFirmware,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureCatalogEntry {
    pub mode: FailureMode,
    pub severity: FailureSeverity,
    pub recovery: RecoveryAction,
    pub static_catalog: bool,
    pub observed: bool,
}

pub fn catalog_entries() -> Vec<FailureCatalogEntry> {
    ALL_FAILURE_MODES
        .iter()
        .copied()
        .map(|mode| {
            let verdict = verdict(mode);
            FailureCatalogEntry {
                mode,
                severity: verdict.severity,
                recovery: verdict.recovery,
                static_catalog: true,
                observed: false,
            }
        })
        .collect()
}

/// Look up the canonical verdict for a failure mode.
pub fn verdict(mode: FailureMode) -> FailureVerdict {
    use FailureMode::*;
    use FailureSeverity::*;
    use RecoveryAction::*;
    let (severity, recovery) = match mode {
        // Boot — most are CRITICAL (operator needed).
        FsblCorruption => (Critical, OperatorIntervention),
        UbootEnvCorrupt => (High, BootRecoverySlot),
        UimageCorrupt => (High, BootRecoverySlot),
        RootfsUbiCorrupt => (High, BootRecoverySlot),
        Am2MissingFwSetenv => (Critical, OperatorIntervention),
        ServiceScriptFail => (Medium, DaemonReinit),
        DcentraldCrash => (High, DaemonReinit),
        DcentraldHttpPortCollision => (Medium, OperatorIntervention),
        HybridSkipsApiServer => (Medium, OperatorIntervention),
        // Watchdog — full re-init paths.
        PsuWatchdogMiss => (High, PsuReinit),
        PicHeartbeatMiss => (High, ChainReinit),
        // Chain dead — most are TripleWrite or ChainReinit.
        GetAddressZeroChips => (High, OperatorIntervention),
        GetAddressPartialChips => (Medium, ChainReinit),
        ChainInactiveSingleWriteMiss => (Medium, TripleWriteRegister),
        SetChipAddressBroadcastError => (High, ChainReinit),
        MiscCtrlSingleWriteRace => (High, TripleWriteRegister),
        Bm1387MissingOpenCore => (High, ChainReinit),
        Bm1362UartRelayUnwritten => (High, ChainReinit),
        SetVoltageBeforeStableTickGate => (High, ChainReinit),
        SeventyFiveSecCliff => (High, TripleWriteRegister),
        // Sysupgrade.
        UbiVolumeMismatch => (Critical, OperatorIntervention),
        SwitchFirmwarePythonCrash => (High, OperatorIntervention),
        StaleOverlayBinary => (Critical, OperatorIntervention),
        SysupgradeTarballNameWrong => (Critical, OperatorIntervention),
        HotSwapSigkillKilledPic => (High, GracefulRestart),
        // Stratum.
        PoolMiningConfigureNoReply => (Low, DisableFeatureContinue),
        PoolRejectStale => (Low, CountAndContinue),
        PoolRejectDuplicate => (Medium, OperatorIntervention),
        PoolRejectLowDifficulty => (High, OperatorIntervention),
        PoolRejectInvalidVersionMask => (Medium, BackoffRetry),
        StratumStall => (Medium, BackoffRetry),
        AllPoolsExhausted => (High, BackoffRetry),
        PoolUnauthorized => (High, OperatorIntervention),
        Sv2StandardChannelNonceExhaustion => (High, OperatorIntervention),
        // Power / thermal — safety paths.
        TempSensorError => (High, CutVoltageAndCapFans),
        FanFailure => (Critical, CutVoltageAndCapFans),
        AmlogicSpuriousRpmZero => (Low, CountAndContinue),
        EmergencyOvertemp => (Critical, CutVoltageAndCapFans),
        Ina226Failure => (Medium, DisableFeatureContinue),
        //  fail-B additions.
        DashboardFailedToBind => (Critical, OperatorIntervention),
        FirmwareSlotMismatch => (Critical, OperatorIntervention),
        PsuRailUndervoltage => (High, PsuReinit),
        S21NoPicGpioReset => (Critical, OperatorIntervention),
        Am2RawSysfsHbReset => (High, BackoffRetry),
        S19ProI2cUnbindBreaksPsu => (High, OperatorIntervention),
        AxiIicClockDriftAfterSoftr => (High, RestoreClockTiming),
        Am2DegradedDspicFirmware => (Critical, OperatorIntervention),
    };
    FailureVerdict { severity, recovery }
}

/// Returns true if the failure requires immediate voltage cut + fan cap
/// ( and friends).
pub fn requires_voltage_cut(mode: FailureMode) -> bool {
    matches!(verdict(mode).recovery, RecoveryAction::CutVoltageAndCapFans)
}

/// Returns true if the failure requires operator intervention (no
/// automated recovery available).
pub fn requires_operator(mode: FailureMode) -> bool {
    let v = verdict(mode);
    v.severity == FailureSeverity::Critical
        || matches!(v.recovery, RecoveryAction::OperatorIntervention)
}

/// Runtime evidence that can be classified without touching hardware.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FailureObservationInput {
    pub platform: Option<String>,
    pub target_label: Option<String>,
    pub dspic_fw: Option<u8>,
    pub pool_reject_code: Option<u32>,
    pub temp_sensor_valid: Option<bool>,
    pub temp_c: Option<f32>,
    pub fan_pwm: Option<u16>,
    pub fan_rpm: Option<u32>,
    pub psu_rail_mv: Option<u32>,
    pub expected_chips: Option<u16>,
    pub observed_chips: Option<u16>,
    pub stratum_notify_age_ms: Option<u64>,
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedFailure {
    pub mode: FailureMode,
    pub severity: FailureSeverity,
    pub recovery: RecoveryAction,
    pub static_catalog: bool,
    pub observed: bool,
    pub observed_at_ms: u64,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedFailureSnapshot {
    pub schema: String,
    pub generated_at_ms: u64,
    pub static_catalog_count: usize,
    pub observed_failures: Vec<ObservedFailure>,
}

pub fn classify_observed_failures(input: &FailureObservationInput) -> ObservedFailureSnapshot {
    let mut observed = Vec::new();

    if input.dspic_fw == Some(0x86) && is_am2_am3_context(input) {
        push_observed(
            &mut observed,
            FailureMode::Am2DegradedDspicFirmware,
            input.generated_at_ms,
            vec!["dsPIC firmware byte observed as 0x86 in AM2/AM3 context".to_string()],
        );
    }

    if let Some(code) = input.pool_reject_code {
        let mode = match code {
            21 => Some(FailureMode::PoolRejectStale),
            22 => Some(FailureMode::PoolRejectDuplicate),
            23 => Some(FailureMode::PoolRejectLowDifficulty),
            24 => Some(FailureMode::PoolUnauthorized),
            27 => Some(FailureMode::PoolRejectInvalidVersionMask),
            _ => None,
        };
        if let Some(mode) = mode {
            push_observed(
                &mut observed,
                mode,
                input.generated_at_ms,
                vec![format!("pool reject code {code}")],
            );
        }
    }

    if input.temp_sensor_valid == Some(false)
        || input
            .temp_c
            .is_some_and(|temp_c| temp_c <= 0.0 || temp_c > 120.0)
    {
        push_observed(
            &mut observed,
            FailureMode::TempSensorError,
            input.generated_at_ms,
            vec!["temperature sensor invalid or out of safe plausible range".to_string()],
        );
    }

    if input
        .temp_c
        .is_some_and(|temp_c| (75.0..=120.0).contains(&temp_c))
    {
        push_observed(
            &mut observed,
            FailureMode::EmergencyOvertemp,
            input.generated_at_ms,
            vec!["temperature at or above emergency threshold".to_string()],
        );
    }

    if input.fan_pwm.unwrap_or_default() > 0 && input.fan_rpm == Some(0) {
        let mode = if input
            .platform
            .as_deref()
            .is_some_and(|platform| platform.starts_with("am3"))
        {
            FailureMode::AmlogicSpuriousRpmZero
        } else {
            FailureMode::FanFailure
        };
        push_observed(
            &mut observed,
            mode,
            input.generated_at_ms,
            vec!["fan PWM is nonzero while observed RPM is zero".to_string()],
        );
    }

    if input
        .psu_rail_mv
        .is_some_and(|rail_mv| rail_mv > 0 && rail_mv < 12_000)
    {
        push_observed(
            &mut observed,
            FailureMode::PsuRailUndervoltage,
            input.generated_at_ms,
            vec!["PSU rail below 12 V threshold".to_string()],
        );
    }

    if let (Some(expected), Some(observed_chips)) = (input.expected_chips, input.observed_chips) {
        if expected > 0 && observed_chips == 0 {
            push_observed(
                &mut observed,
                FailureMode::GetAddressZeroChips,
                input.generated_at_ms,
                vec![format!("expected {expected} chips, observed 0")],
            );
        } else if observed_chips > 0 && observed_chips < expected {
            push_observed(
                &mut observed,
                FailureMode::GetAddressPartialChips,
                input.generated_at_ms,
                vec![format!(
                    "expected {expected} chips, observed {observed_chips}"
                )],
            );
        }
    }

    if input
        .stratum_notify_age_ms
        .is_some_and(|age_ms| age_ms > 120_000)
    {
        push_observed(
            &mut observed,
            FailureMode::StratumStall,
            input.generated_at_ms,
            vec!["no fresh stratum notify for more than 120 seconds".to_string()],
        );
    }

    ObservedFailureSnapshot {
        schema: OBSERVED_FAILURE_SNAPSHOT_SCHEMA.to_string(),
        generated_at_ms: input.generated_at_ms,
        static_catalog_count: ALL_FAILURE_MODES.len(),
        observed_failures: observed,
    }
}

fn push_observed(
    observed: &mut Vec<ObservedFailure>,
    mode: FailureMode,
    observed_at_ms: u64,
    evidence: Vec<String>,
) {
    let verdict = verdict(mode);
    observed.push(ObservedFailure {
        mode,
        severity: verdict.severity,
        recovery: verdict.recovery,
        static_catalog: false,
        observed: true,
        observed_at_ms,
        evidence,
    });
}

fn is_am2_am3_context(input: &FailureObservationInput) -> bool {
    input
        .platform
        .as_deref()
        .is_some_and(|platform| platform.starts_with("am2") || platform.starts_with("am3"))
        || input
            .target_label
            .as_deref()
            .is_some_and(|label| label.contains(".139") || label.contains("s19jpro"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RecoveryGuidanceContext {
    pub platform: Option<String>,
    pub target_label: Option<String>,
    pub degraded_dspic_fw: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannedRecoveryPath {
    LdPreloadShim,
    ProductionPicReset,
    ApwTelemetryGuess,
    ZynqIcsp,
    EepromWrite,
    DspicFlashErase,
    FpgaDirectPowerBypass,
    DspicUartBootloader,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryGuidanceStep {
    pub action: RecoveryAction,
    pub label: String,
    pub auto_executable: bool,
    pub destructive: bool,
    pub requires_operator: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DcentRecoveryGuidance {
    pub schema: String,
    pub mode: FailureMode,
    pub severity: FailureSeverity,
    pub catalog_recovery: RecoveryAction,
    pub actions: Vec<RecoveryGuidanceStep>,
    pub banned_paths_filtered: Vec<BannedRecoveryPath>,
}

pub fn dcent_recovery_guidance(
    mode: FailureMode,
    context: &RecoveryGuidanceContext,
) -> DcentRecoveryGuidance {
    let verdict = verdict(mode);
    let mut actions = guidance_steps_for_mode(mode, verdict);
    let banned_paths_filtered = banned_paths_for_context(context);

    if is_139_or_degraded_context(context) {
        for action in &mut actions {
            if matches!(
                mode,
                FailureMode::Bm1362UartRelayUnwritten | FailureMode::Am2DegradedDspicFirmware
            ) {
                action.auto_executable = false;
                action.requires_operator = true;
            }
        }
    }

    for action in &mut actions {
        if action.destructive {
            action.auto_executable = false;
            action.requires_operator = true;
        }
    }

    DcentRecoveryGuidance {
        schema: DCENT_RECOVERY_GUIDANCE_SCHEMA.to_string(),
        mode,
        severity: verdict.severity,
        catalog_recovery: verdict.recovery,
        actions,
        banned_paths_filtered,
    }
}

fn guidance_steps_for_mode(
    mode: FailureMode,
    verdict: FailureVerdict,
) -> Vec<RecoveryGuidanceStep> {
    let mut steps = Vec::new();
    match mode {
        FailureMode::Am2DegradedDspicFirmware => {
            steps.push(RecoveryGuidanceStep {
                action: RecoveryAction::OperatorIntervention,
                label: "Refuse voltage and enable commands; preserve diagnostics for lab review"
                    .to_string(),
                auto_executable: false,
                destructive: false,
                requires_operator: true,
            });
            steps.push(RecoveryGuidanceStep {
                action: RecoveryAction::OperatorIntervention,
                label: "Physical ICSP recovery only after explicit bench authorization".to_string(),
                auto_executable: false,
                destructive: true,
                requires_operator: true,
            });
        }
        FailureMode::Bm1362UartRelayUnwritten => steps.push(RecoveryGuidanceStep {
            action: RecoveryAction::ChainReinit,
            label: "Report AM2 relay state and require lab-gated cold-boot relay proof".to_string(),
            auto_executable: false,
            destructive: false,
            requires_operator: true,
        }),
        _ => steps.push(RecoveryGuidanceStep {
            action: verdict.recovery,
            label: format!("Apply catalog recovery action {:?}", verdict.recovery),
            auto_executable: !requires_operator(mode)
                && !matches!(verdict.recovery, RecoveryAction::BootRecoverySlot),
            destructive: matches!(verdict.recovery, RecoveryAction::BootRecoverySlot),
            requires_operator: requires_operator(mode),
        }),
    }
    steps
}

fn banned_paths_for_context(context: &RecoveryGuidanceContext) -> Vec<BannedRecoveryPath> {
    if !is_139_or_degraded_context(context) {
        return vec![BannedRecoveryPath::EepromWrite];
    }

    vec![
        BannedRecoveryPath::LdPreloadShim,
        BannedRecoveryPath::ProductionPicReset,
        BannedRecoveryPath::ApwTelemetryGuess,
        BannedRecoveryPath::ZynqIcsp,
        BannedRecoveryPath::EepromWrite,
        BannedRecoveryPath::DspicFlashErase,
        BannedRecoveryPath::FpgaDirectPowerBypass,
        BannedRecoveryPath::DspicUartBootloader,
    ]
}

fn is_139_or_degraded_context(context: &RecoveryGuidanceContext) -> bool {
    context.degraded_dspic_fw
        || context
            .target_label
            .as_deref()
            .is_some_and(|label| label.contains(".139") || label.contains("s19jpro"))
        || context
            .platform
            .as_deref()
            .is_some_and(|platform| platform == "am2-zynq")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_modes() -> Vec<FailureMode> {
        vec![
            FailureMode::FsblCorruption,
            FailureMode::UbootEnvCorrupt,
            FailureMode::UimageCorrupt,
            FailureMode::RootfsUbiCorrupt,
            FailureMode::Am2MissingFwSetenv,
            FailureMode::ServiceScriptFail,
            FailureMode::DcentraldCrash,
            FailureMode::DcentraldHttpPortCollision,
            FailureMode::HybridSkipsApiServer,
            FailureMode::PsuWatchdogMiss,
            FailureMode::PicHeartbeatMiss,
            FailureMode::GetAddressZeroChips,
            FailureMode::GetAddressPartialChips,
            FailureMode::ChainInactiveSingleWriteMiss,
            FailureMode::SetChipAddressBroadcastError,
            FailureMode::MiscCtrlSingleWriteRace,
            FailureMode::Bm1387MissingOpenCore,
            FailureMode::Bm1362UartRelayUnwritten,
            FailureMode::SetVoltageBeforeStableTickGate,
            FailureMode::SeventyFiveSecCliff,
            FailureMode::UbiVolumeMismatch,
            FailureMode::SwitchFirmwarePythonCrash,
            FailureMode::StaleOverlayBinary,
            FailureMode::SysupgradeTarballNameWrong,
            FailureMode::HotSwapSigkillKilledPic,
            FailureMode::PoolMiningConfigureNoReply,
            FailureMode::PoolRejectStale,
            FailureMode::PoolRejectDuplicate,
            FailureMode::PoolRejectLowDifficulty,
            FailureMode::PoolRejectInvalidVersionMask,
            FailureMode::StratumStall,
            FailureMode::AllPoolsExhausted,
            FailureMode::PoolUnauthorized,
            FailureMode::Sv2StandardChannelNonceExhaustion,
            FailureMode::TempSensorError,
            FailureMode::FanFailure,
            FailureMode::AmlogicSpuriousRpmZero,
            FailureMode::EmergencyOvertemp,
            FailureMode::Ina226Failure,
            //  fail-B additions.
            FailureMode::DashboardFailedToBind,
            FailureMode::FirmwareSlotMismatch,
            FailureMode::PsuRailUndervoltage,
            FailureMode::S21NoPicGpioReset,
            FailureMode::Am2RawSysfsHbReset,
            FailureMode::S19ProI2cUnbindBreaksPsu,
            FailureMode::AxiIicClockDriftAfterSoftr,
            FailureMode::Am2DegradedDspicFirmware,
        ]
    }

    #[test]
    fn every_failure_mode_has_a_verdict() {
        for m in all_modes() {
            let v = verdict(m);
            // All variants are populated; this is mostly checking
            // we don't accidentally drop an arm in the match.
            let _ = (v.severity, v.recovery);
        }
    }

    #[test]
    fn safety_critical_failures_cut_voltage() {
        for m in [
            FailureMode::TempSensorError,
            FailureMode::FanFailure,
            FailureMode::EmergencyOvertemp,
        ] {
            assert!(requires_voltage_cut(m), "{:?} must trigger voltage cut", m);
        }
    }

    #[test]
    fn brick_risk_failures_are_critical() {
        for m in [
            FailureMode::Am2MissingFwSetenv,
            FailureMode::FsblCorruption,
            FailureMode::UbiVolumeMismatch,
            FailureMode::StaleOverlayBinary,
            FailureMode::SysupgradeTarballNameWrong,
        ] {
            assert_eq!(
                verdict(m).severity,
                FailureSeverity::Critical,
                "{:?} should be Critical",
                m
            );
        }
    }

    #[test]
    fn fan_failure_is_critical_voltage_cut() {
        let v = verdict(FailureMode::FanFailure);
        assert_eq!(v.severity, FailureSeverity::Critical);
        assert_eq!(v.recovery, RecoveryAction::CutVoltageAndCapFans);
    }

    #[test]
    fn miscctrl_race_recommends_triple_write() {
        let v = verdict(FailureMode::MiscCtrlSingleWriteRace);
        assert_eq!(v.recovery, RecoveryAction::TripleWriteRegister);
    }

    #[test]
    fn pool_stale_share_is_count_and_continue() {
        // RE doc: Code 21 stale shares are expected — no action.
        let v = verdict(FailureMode::PoolRejectStale);
        assert_eq!(v.severity, FailureSeverity::Low);
        assert_eq!(v.recovery, RecoveryAction::CountAndContinue);
    }

    #[test]
    fn psu_watchdog_miss_recovery_is_full_psu_reinit() {
        let v = verdict(FailureMode::PsuWatchdogMiss);
        assert_eq!(v.recovery, RecoveryAction::PsuReinit);
    }

    #[test]
    fn pic_heartbeat_miss_recovery_is_chain_reinit() {
        let v = verdict(FailureMode::PicHeartbeatMiss);
        assert_eq!(v.recovery, RecoveryAction::ChainReinit);
    }

    #[test]
    fn amlogic_spurious_rpm_zero_is_low_count_continue() {
        // RE doc: brief glitch returns 0 — keep last_good_rpm, don't
        // trigger FanFailure.
        let v = verdict(FailureMode::AmlogicSpuriousRpmZero);
        assert_eq!(v.severity, FailureSeverity::Low);
        assert_eq!(v.recovery, RecoveryAction::CountAndContinue);
    }

    #[test]
    fn requires_operator_predicate() {
        assert!(requires_operator(FailureMode::FsblCorruption));
        assert!(requires_operator(FailureMode::UbiVolumeMismatch));
        // Stratum stall is automated (BackoffRetry).
        assert!(!requires_operator(FailureMode::StratumStall));
        // Pool reject stale is automated (CountAndContinue).
        assert!(!requires_operator(FailureMode::PoolRejectStale));
    }

    #[test]
    fn severity_ordering_matches_canonical() {
        // Per RE doc lines 144-162: Critical > High > Medium > Low.
        assert!(FailureSeverity::Critical > FailureSeverity::High);
        assert!(FailureSeverity::High > FailureSeverity::Medium);
        assert!(FailureSeverity::Medium > FailureSeverity::Low);
    }

    #[test]
    fn failure_mode_round_trips_through_serde() {
        for m in all_modes() {
            let json = serde_json::to_string(&m).unwrap();
            let back: FailureMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn verdict_round_trips_through_serde() {
        let v = verdict(FailureMode::FanFailure);
        let json = serde_json::to_string(&v).unwrap();
        let back: FailureVerdict = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    // -----------------------------------------------------------------------
    //  fail-B additions
    // -----------------------------------------------------------------------

    #[test]
    fn wave26_new_variants_have_verdicts() {
        // Pin every new variant returns a populated verdict (no fall-
        // through). If any of these arms is dropped from `verdict()`
        // the match will fail to compile.
        for m in [
            FailureMode::DashboardFailedToBind,
            FailureMode::FirmwareSlotMismatch,
            FailureMode::PsuRailUndervoltage,
            FailureMode::S21NoPicGpioReset,
            FailureMode::Am2RawSysfsHbReset,
            FailureMode::S19ProI2cUnbindBreaksPsu,
            FailureMode::AxiIicClockDriftAfterSoftr,
        ] {
            let v = verdict(m);
            // Any populated verdict counts.
            let _ = (v.severity, v.recovery);
        }
    }

    #[test]
    fn wave26_critical_variants_require_operator_intervention() {
        // DashboardFailedToBind / FirmwareSlotMismatch /
        // S21NoPicGpioReset are CRITICAL and need operator action.
        for m in [
            FailureMode::DashboardFailedToBind,
            FailureMode::FirmwareSlotMismatch,
            FailureMode::S21NoPicGpioReset,
        ] {
            let v = verdict(m);
            assert_eq!(
                v.severity,
                FailureSeverity::Critical,
                "{:?} should be CRITICAL",
                m
            );
            assert_eq!(
                v.recovery,
                RecoveryAction::OperatorIntervention,
                "{:?} should map to OperatorIntervention",
                m
            );
        }
    }

    #[test]
    fn psu_rail_undervoltage_recovers_via_psu_reinit() {
        // PsuRailUndervoltage is HIGH severity but recoverable via
        // cold-boot PSU re-init — distinct from the watchdog-cut path.
        let v = verdict(FailureMode::PsuRailUndervoltage);
        assert_eq!(v.severity, FailureSeverity::High);
        assert_eq!(v.recovery, RecoveryAction::PsuReinit);
        // Same recovery as PsuWatchdogMiss — both are cold-boot
        // re-init paths.
        let other = verdict(FailureMode::PsuWatchdogMiss);
        assert_eq!(v.recovery, other.recovery);
    }

    #[test]
    fn am2_raw_sysfs_hb_reset_recovers_via_backoff_retry() {
        // The pulse_reset RMW helper is the canonical recovery — the
        // adapter should retry with backoff after using it.
        let v = verdict(FailureMode::Am2RawSysfsHbReset);
        assert_eq!(v.severity, FailureSeverity::High);
        assert_eq!(v.recovery, RecoveryAction::BackoffRetry);
    }

    #[test]
    fn s19_pro_i2c_unbind_requires_operator_reboot() {
        // S19 Pro PSU break needs reboot — operator-level recovery.
        let v = verdict(FailureMode::S19ProI2cUnbindBreaksPsu);
        assert_eq!(v.severity, FailureSeverity::High);
        assert_eq!(v.recovery, RecoveryAction::OperatorIntervention);
    }

    #[test]
    fn axi_iic_clock_drift_uses_restore_clock_timing() {
        // The new RecoveryAction::RestoreClockTiming is reserved for
        // this specific case (re-apply THIGH/TLOW/TBUF after SOFTR).
        let v = verdict(FailureMode::AxiIicClockDriftAfterSoftr);
        assert_eq!(v.severity, FailureSeverity::High);
        assert_eq!(v.recovery, RecoveryAction::RestoreClockTiming);
    }

    #[test]
    fn restore_clock_timing_recovery_round_trips_through_serde() {
        let action = RecoveryAction::RestoreClockTiming;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"restore_clock_timing\"");
        let back: RecoveryAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, back);
    }

    #[test]
    fn s21_nopic_gpio_reset_is_critical_brick_class() {
        // Specifically: S21 NoPic GPIO reset kills TAS5782M voltage —
        // the unit is non-recoverable without AC power cycle. Pin both
        // CRITICAL severity and operator-action recovery.
        let v = verdict(FailureMode::S21NoPicGpioReset);
        assert_eq!(v.severity, FailureSeverity::Critical);
        assert!(requires_operator(FailureMode::S21NoPicGpioReset));
    }

    #[test]
    fn wave26_extended_catalog_has_47_variants_after_wave_b_degraded_fw() {
        //  shipped 39 variants;  adds 7 -> 46 total.
        // Wave B adds the degraded fw=0x86 identity/failure truth variant.
        // Pin so a refactor cannot silently drop one.
        assert_eq!(all_modes().len(), 47);
        assert_eq!(ALL_FAILURE_MODES.len(), 47);
    }

    #[test]
    fn wave26_new_variants_round_trip_through_serde() {
        for m in [
            FailureMode::DashboardFailedToBind,
            FailureMode::FirmwareSlotMismatch,
            FailureMode::PsuRailUndervoltage,
            FailureMode::S21NoPicGpioReset,
            FailureMode::Am2RawSysfsHbReset,
            FailureMode::S19ProI2cUnbindBreaksPsu,
            FailureMode::AxiIicClockDriftAfterSoftr,
        ] {
            let json = serde_json::to_string(&m).unwrap();
            let back: FailureMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn wave26_new_variants_serialize_in_snake_case() {
        // Pin wire form for every new variant — pyasic / dashboard
        // decode by name.
        assert_eq!(
            serde_json::to_string(&FailureMode::DashboardFailedToBind).unwrap(),
            "\"dashboard_failed_to_bind\""
        );
        assert_eq!(
            serde_json::to_string(&FailureMode::S19ProI2cUnbindBreaksPsu).unwrap(),
            "\"s19_pro_i2c_unbind_breaks_psu\""
        );
        assert_eq!(
            serde_json::to_string(&FailureMode::AxiIicClockDriftAfterSoftr).unwrap(),
            "\"axi_iic_clock_drift_after_softr\""
        );
    }

    #[test]
    fn catalog_entries_are_static_not_observed() {
        let entries = catalog_entries();

        assert_eq!(entries.len(), ALL_FAILURE_MODES.len());
        assert!(entries.iter().all(|entry| entry.static_catalog));
        assert!(entries.iter().all(|entry| !entry.observed));
        assert!(entries
            .iter()
            .any(|entry| entry.mode == FailureMode::Am2DegradedDspicFirmware));
    }

    #[test]
    fn observed_classifier_separates_runtime_evidence_from_static_catalog() {
        let snapshot = classify_observed_failures(&FailureObservationInput {
            platform: Some("am2-zynq".to_string()),
            target_label: Some("s19jpro".to_string()),
            dspic_fw: Some(0x86),
            pool_reject_code: Some(21),
            generated_at_ms: 123_456,
            ..FailureObservationInput::default()
        });

        assert_eq!(snapshot.schema, OBSERVED_FAILURE_SNAPSHOT_SCHEMA);
        assert_eq!(snapshot.static_catalog_count, ALL_FAILURE_MODES.len());
        assert!(snapshot
            .observed_failures
            .iter()
            .all(|failure| failure.observed && !failure.static_catalog));
        assert!(snapshot
            .observed_failures
            .iter()
            .any(|failure| failure.mode == FailureMode::Am2DegradedDspicFirmware));
        assert!(snapshot
            .observed_failures
            .iter()
            .any(|failure| failure.mode == FailureMode::PoolRejectStale));
    }

    #[test]
    fn degraded_fw_is_critical_operator_only() {
        let v = verdict(FailureMode::Am2DegradedDspicFirmware);
        assert_eq!(v.severity, FailureSeverity::Critical);
        assert_eq!(v.recovery, RecoveryAction::OperatorIntervention);
        assert!(requires_operator(FailureMode::Am2DegradedDspicFirmware));
        assert_eq!(
            serde_json::to_string(&FailureMode::Am2DegradedDspicFirmware).unwrap(),
            "\"am2_degraded_dspic_firmware\""
        );
    }

    #[test]
    fn active_classifier_maps_common_observed_inputs() {
        let snapshot = classify_observed_failures(&FailureObservationInput {
            platform: Some("am1-zynq".to_string()),
            temp_sensor_valid: Some(false),
            fan_pwm: Some(20),
            fan_rpm: Some(0),
            psu_rail_mv: Some(11_700),
            expected_chips: Some(63),
            observed_chips: Some(31),
            stratum_notify_age_ms: Some(121_000),
            generated_at_ms: 999,
            ..FailureObservationInput::default()
        });
        let modes: Vec<_> = snapshot
            .observed_failures
            .iter()
            .map(|failure| failure.mode)
            .collect();

        assert!(modes.contains(&FailureMode::TempSensorError));
        assert!(modes.contains(&FailureMode::FanFailure));
        assert!(modes.contains(&FailureMode::PsuRailUndervoltage));
        assert!(modes.contains(&FailureMode::GetAddressPartialChips));
        assert!(modes.contains(&FailureMode::StratumStall));
    }

    #[test]
    fn recovery_guidance_filters_banned_paths_for_139() {
        let guidance = dcent_recovery_guidance(
            FailureMode::Bm1362UartRelayUnwritten,
            &RecoveryGuidanceContext {
                platform: Some("am2-zynq".to_string()),
                target_label: Some("s19jpro".to_string()),
                degraded_dspic_fw: false,
            },
        );

        assert_eq!(guidance.schema, DCENT_RECOVERY_GUIDANCE_SCHEMA);
        for banned in [
            BannedRecoveryPath::LdPreloadShim,
            BannedRecoveryPath::ProductionPicReset,
            BannedRecoveryPath::ApwTelemetryGuess,
            BannedRecoveryPath::ZynqIcsp,
            BannedRecoveryPath::EepromWrite,
            BannedRecoveryPath::DspicFlashErase,
            BannedRecoveryPath::FpgaDirectPowerBypass,
            BannedRecoveryPath::DspicUartBootloader,
        ] {
            assert!(
                guidance.banned_paths_filtered.contains(&banned),
                "missing banned path {:?}",
                banned
            );
        }
        assert!(guidance
            .actions
            .iter()
            .all(|action| !action.auto_executable));
    }

    #[test]
    fn degraded_fw_guidance_never_auto_executes_destructive_action() {
        let guidance = dcent_recovery_guidance(
            FailureMode::Am2DegradedDspicFirmware,
            &RecoveryGuidanceContext {
                platform: Some("am2-zynq".to_string()),
                target_label: Some("s19jpro".to_string()),
                degraded_dspic_fw: true,
            },
        );

        assert!(guidance
            .banned_paths_filtered
            .contains(&BannedRecoveryPath::EepromWrite));
        assert!(guidance
            .banned_paths_filtered
            .contains(&BannedRecoveryPath::ProductionPicReset));
        assert!(guidance.actions.iter().any(|action| action.destructive));
        assert!(guidance
            .actions
            .iter()
            .all(|action| !(action.destructive && action.auto_executable)));
        assert!(guidance
            .actions
            .iter()
            .all(|action| action.requires_operator));
    }
}
