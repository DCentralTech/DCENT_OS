//! Pure API contract types for dcentrald.
//!
//! This crate is intentionally independent from HAL, async runtimes, sockets,
//! filesystem paths, and miner hardware. It is the host-safe boundary for API
//! DTOs and status classifiers that should compile on Windows and Linux.

///  psu-C: APW12 dual-output architecture DTOs (HAL-free).
pub mod apw_dual_output;
///  cmd-A: generic BM13xx ASIC command catalog (HAL-free).
pub mod asic_command;
/// Wave H: host-safe ASIC protocol selection specs (HAL-free).
pub mod asic_protocol_spec;
///  reg-A: BM13xx ASIC register-map catalog (HAL-free).
pub mod asic_register_map;
///  thm-A: LuxOS ATM (Advanced Thermal Management) state machine.
pub mod atm_stepper;
///  tel-B: NDJSON audit log encoder (HAL-free).
pub mod audit_log;
///  tune-G: VNish 5-phase autotuner state machine (HAL-free).
pub mod autotune_phase;
/// Wave I: host-side autotune policy DTOs and safety gates (HAL-free).
pub mod autotune_policy;
///  baud-A: per-chip baud-upgrade plan + triple-write rule (HAL-free).
pub mod baud_switch;
/// S21/BM1368 per-chip temperature readback DTO shape (HAL-free, not live-proven by default).
pub mod bm1368_temperature;
/// Evidence-scoped BM1398 chip, NBP1901 chain, and FPGA FIFO contracts.
pub mod bm1398_protocol;
/// Deterministic evidence-parameterized BM13xx four-divider PLL search.
pub mod bm13xx_pll;
///  boot-A: boot-flow phase timeline DTO (HAL-free).
pub mod boot_flow;
///  orch-A: system-orchestration FSM DTOs (HAL-free).
pub mod boot_orchestration;
/// W13.D1: live cold-boot phase enum + 6-substate CV1835 taxonomy
/// + generic 3-substate fallback for `/api/boot/phase` and
/// `/api/boot/timeline`. HAL-free.
pub mod boot_phase;
///  braiins-C: BraiinsOS+ Constraints DTO (HAL-free).
pub mod braiinsos_constraints;
///  braiins-E: BraiinsOS+ Cooling Mode + Auto Mode + Pause Mode + Pre-Heat DTOs (HAL-free).
pub mod braiinsos_cooling_mode;
///  braiins-D: BraiinsOS+ DPS (Dynamic Performance Scaling) configuration DTOs (HAL-free).
pub mod braiinsos_dps_configuration;
///  braiins-A: BraiinsOS+ gRPC service catalog (HAL-free).
pub mod braiinsos_grpc_catalog;
///  braiins-B: BraiinsOS+ MinerStatus state machine (HAL-free).
pub mod braiinsos_miner_status;
///  braiins-F: BraiinsOS+ NetworkService DTOs (HAL-free).
pub mod braiinsos_network_configuration;
///  braiins-G: BraiinsOS+ proto wire-type wrappers (HAL-free).
pub mod braiinsos_proto_wire_types;
///  api-A: LuxOS CGMiner command catalog (HAL-free DTO).
pub mod cgminer_catalog;
///  cgmsg-A: full CGMiner status code map (HAL-free).
pub mod cgminer_status_codes;
///  cis-A: per-chip-family cold-boot init constants (HAL-free).
pub mod chip_init;
///  thm-C: cold-environment auto-target adjuster (HAL-free).
pub mod cold_environment;
///  dvr-A: S17/S19 hashboard diode voltage reference (HAL-free).
pub mod diode_voltage;
///  dsp-A: dsPIC / PIC16F1704 / APW PSU wire format codec.
pub mod dspic_frame;
///  eep-A: EEPROM record DTOs (post-cipher; HAL-free).
pub mod eeprom_record;
///  fail-A: failure-mode classification + recovery action dispatch.
pub mod failure_mode;
///  boot-A: per-firmware-family boot timeline DTOs (HAL-free).
pub mod firmware_boot_timeline;
///  strat-A: cross-firmware Stratum + DevFee capability matrix (HAL-free).
pub mod firmware_stratum_matrix;
///  fpga-A: Zynq FPGA register-map catalog (HAL-free).
pub mod fpga_register_map;
///  frq-A: initial-frequency-ramp planner + cores_per_chip (HAL-free).
pub mod frequency_scaling;
///  diag-A: hashboard fault triage flowchart (HAL-free).
pub mod hashboard_diagnostics;
/// PH-3 (): pure default-OFF hashrate auto-recovery ladder FSM (HAL-free, host-tested).
pub mod hashrate_recovery;
///  ipr-A: Bitmain IP Reporter UDP protocol codec (HAL-free).
pub mod ip_reporter;
///  luxos-J: LuxOS `/luxor/audit.json` audit-log schema DTOs (HAL-free).
pub mod luxos_audit_log;
///  luxos-D: LuxOS REST error vocabulary catalog (HAL-free).
pub mod luxos_error_vocab;
///  luxos-K: LuxOS network attack-surface catalog (HAL-free).
pub mod luxos_network_exposure;
///  luxos-I: LuxOS partner / white-label branding system DTOs (HAL-free).
pub mod luxos_partner_branding;
///  luxos-M: LuxOS pool-failover state machine + smart switch + backoff (HAL-free).
pub mod luxos_pool_failover;
///  luxos-H: LuxOS recovery + uninstall flow DTOs (HAL-free).
pub mod luxos_recovery;
///  luxos-E: typed LuxOS REST response payload DTOs (HAL-free).
pub mod luxos_response_payloads;
///  luxos-B: full LuxOS REST command catalog (HAL-free).
pub mod luxos_rest_command;
///  luxos-C: LuxOS / CGMiner JSON response envelope (HAL-free).
pub mod luxos_rest_envelope;
///  luxos-G: LuxOS thermal sensor topology + threshold hierarchy (HAL-free).
pub mod luxos_sensor_topology;
///  luxos-L: LuxOS system architecture catalog — MTD layout + init scripts (HAL-free).
pub mod luxos_system_architecture;
///  luxos-F: LuxOS firmware-update flow DTOs (HAL-free).
pub mod luxos_update;
///  luxos-A: LuxOS web UI page + endpoint catalog (HAL-free).
pub mod luxos_web_pages;
///  tel-A: 3-tier metrics CSV format + ring buffer (HAL-free).
pub mod metrics_csv;
///  mln-A: top-level mining-loop state machine (HAL-free).
pub mod mining_loop_state;
///  sec-A: OTA rollback-protection policy (HAL-free).
pub mod ota_rollback_protection;
/// W9.4 perf-A: J/TH efficiency contract DTOs (HAL-free) — operator
/// wattmeter calibration source-of-truth, PMBus-derived live, model-only
/// fallback.
pub mod perf_efficiency;
///  pic-A: PIC firmware version catalog (HAL-free).
pub mod pic_firmware;
///  pwr-B: V²f power estimation model (HAL-free).
pub mod power_model;
///  prof-A: per-model power-profile preset catalog (HAL-free).
pub mod power_profile_preset;
///  pwr-A: cold-boot power state machine (HAL-free).
pub mod power_state;
///  W5-A: JSON profile-bundle schema constants for profile registry
/// consumers (dashboard, toolbox, REST). HAL-free.
pub mod profile_schema;
/// G5: native Prometheus text-exposition encoder (HAL-free). Pairs with
/// the LuxOS-style `metrics_csv` ring as an additive scrape surface.
pub mod prometheus_metrics;
///  psu-B: APW PSU 9-command protocol catalog (HAL-free).
pub mod psu_apw_protocol;
///  bypass-A: PSU + hashboard bypass policy (HAL-free).
pub mod psu_bypass;
///  psu-A: APW PSU family catalog (HAL-free).
pub mod psu_model;
/// W13.D1: PVT (Process-Voltage-Temperature) table contract for
/// `/api/miner/pvt-table`. HAL-free.
pub mod pvt_table;
///  ramp-A: LuxOS 10-min boot-to-mining ramp curve (HAL-free).
pub mod ramp_curve;
///  thm-B: MAD-based bad-sensor outlier detector (HAL-free).
pub mod sensor_outlier;
///  shv-A: share validation pipeline DTOs (HAL-free).
pub mod share_validation;
///  strat-B: full Stratum V1 wire-format JSON-RPC DTOs (HAL-free).
pub mod stratum_v1_messages;
///  strat-E: Stratum V2 channel-open response payloads (HAL-free).
pub mod stratum_v2_channel_responses;
///  strat-C: Stratum V2 wire-spec DTOs (HAL-free).
pub mod stratum_v2_messages;
///  strat-D: Stratum V2 mining-extension message DTOs (HAL-free).
pub mod stratum_v2_mining_messages;
///  therm-A: DCENT_OS + VNish thermal pull-back model (HAL-free).
pub mod thermal_model;
///  uart-A: BB-platform `uart_trans` frame layout DTOs (HAL-free).
pub mod uart_trans_layout;
///  vnish-C: VNish firmware archive layout DTOs (HAL-free).
pub mod vnish_firmware_archive;
///  vnish-B: VNish 1.2.7 overlay-on-stock layout DTOs (HAL-free).
pub mod vnish_overlay_layout;
///  vnish-A: VNish 1.2.x REST endpoint catalog (HAL-free).
pub mod vnish_rest_endpoints;
///  vnish-D: VNish typed REST response payloads (HAL-free).
pub mod vnish_settings;
///  wdg-A: watchdog policy + opcode constants (HAL-free).
pub mod watchdog_policy;
///  wm-A: Whatsminer BTMiner JSON API codec (HAL-free).
pub mod whatsminer_btminer;
///  wrk-A: chip-family work frame builder (HAL-free).
pub mod work_dispatch;

use serde::{Deserialize, Serialize};

/// Operating mode (determines API surface and safety limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperatingMode {
    /// Home mode: noise-optimized, thermostat-like, minimal UI for residential use.
    Home,
    /// Standard mining mode: full dashboard, normal safety limits.
    Standard,
    /// Mining Hacker mode: raw register access, relaxed limits.
    Hacker,
}

impl OperatingMode {
    /// Classify the persisted `[mode].active` string into the public API mode.
    ///
    /// `heater` is a config alias for Home mode. Keep this centralized so
    /// REST, runtime snapshots, and gRPC safety gates cannot drift.
    pub fn from_config_str(mode: &str) -> Self {
        if mode.eq_ignore_ascii_case("heater") || mode.eq_ignore_ascii_case("home") {
            OperatingMode::Home
        } else if mode.eq_ignore_ascii_case("hacker") {
            OperatingMode::Hacker
        } else {
            OperatingMode::Standard
        }
    }

    /// True when this mode must use Home-mode safety constraints.
    pub fn is_home(&self) -> bool {
        matches!(self, OperatingMode::Home)
    }

    /// Check if this mode allows access to debug endpoints.
    pub fn allows_debug(&self) -> bool {
        matches!(self, OperatingMode::Hacker)
    }

    /// Check if this mode allows access to advanced stats endpoints.
    pub fn allows_stats(&self) -> bool {
        matches!(self, OperatingMode::Standard | OperatingMode::Hacker)
    }

    /// Check if this mode requires write confirmation.
    pub fn requires_confirmation(&self) -> bool {
        matches!(self, OperatingMode::Hacker)
    }
}

impl std::fmt::Display for OperatingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperatingMode::Home => write!(f, "home"),
            OperatingMode::Standard => write!(f, "standard"),
            OperatingMode::Hacker => write!(f, "hacker"),
        }
    }
}

pub const MINING_PIPELINE_SNAPSHOT_SCHEMA: &str = "dcentos.mining.pipeline.snapshot.v1";
pub const MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA: &str =
    "dcentos.mining.pipeline.freshness.classifier.v1";
pub const RECENT_SHARE_ROW_SCHEMA: &str = "dcentos.rest.recent_share.row.v1";
pub const API_CONTRACT_VERSION: &str = "dcentos.api.v1";
pub const MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS: u64 = 5_000;
pub const MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS: u64 = 1_000;

/// Stable REST error-code vocabulary for machine clients.
///
/// Human-readable `error` strings may change for clarity; these code strings
/// are the compatibility contract dashboard/toolbox clients should branch on.
pub mod api_error_codes {
    pub const CONFIG_VALIDATION: &str = "config_validation";
    pub const ERROR_BODY_UNAVAILABLE: &str = "error_body_unavailable";
    pub const LEGACY_ERROR: &str = "legacy_error";
    pub const POOL_CONFIG_WRITE_FAILED: &str = "pool_config_write_failed";
    pub const POOL_VALIDATION: &str = "pool_validation";
    pub const UNCLASSIFIED_ERROR: &str = "unclassified_error";
}

/// Canonical REST error body for dashboard-facing JSON failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl ApiErrorBody {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            detail: None,
            code: None,
            suggestion: None,
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// REST row contract for recent share accept/reject events.
///
/// This mirrors the runtime `RecentShareEvent` into a HAL-free DTO so
/// `/api/history/shares` and `/api/mining/work/posture` can share one
/// snake-case serialization contract without compiling the HAL-dependent API
/// crate on host test runners.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RecentShareRow {
    pub timestamp_ms: u64,
    pub result: String,
    pub job_id: String,
    /// Locally computed achieved share difficulty, when proven from the exact
    /// accepted header/hash. Null means unknown, not "same as pool target".
    pub difficulty: Option<f64>,
    /// Pool-assigned target difficulty active when the share was submitted.
    pub target_difficulty: Option<f64>,
    pub error_code: Option<i64>,
    pub error_msg: Option<String>,
    pub worker_name: Option<String>,
    pub nonce: Option<String>,
    pub ntime: Option<String>,
    pub extranonce2: Option<String>,
    pub version_bits: Option<String>,
    pub version: Option<u32>,
    pub protocol_meta_present: bool,
}

/// Pure freshness classifier for future mining-pipeline publisher tests.
///
/// This is intentionally independent from runtime state, HAL, dispatcher state,
/// pool sockets, logs, and filesystem state. It lets API and dashboard contracts
/// describe fail-closed freshness semantics before any live publisher is wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiningPipelineFreshnessClassifierStatus {
    /// No timestamp source exists or the publisher did not provide a sample.
    Unavailable,
    /// Timestamp is present and inside the stale window.
    Live,
    /// Timestamp is present but older than the stale window.
    Stale,
    /// Timestamp is ahead of the response clock beyond the configured skew.
    FutureClockSkew,
    /// Inputs cannot safely produce an age, such as zero stale windows.
    Invalid,
}

impl Default for MiningPipelineFreshnessClassifierStatus {
    fn default() -> Self {
        Self::Unavailable
    }
}

impl MiningPipelineFreshnessClassifierStatus {
    /// Classify one nullable publisher-owned domain timestamp.
    pub fn classify_domain_timestamp(
        domain_last_update_ms: Option<u64>,
        generated_at_ms: u64,
        stale_after_ms: u64,
        max_future_skew_ms: u64,
    ) -> Self {
        if stale_after_ms == 0 {
            return Self::Invalid;
        }

        let Some(domain_last_update_ms) = domain_last_update_ms else {
            return Self::Unavailable;
        };

        if domain_last_update_ms > generated_at_ms {
            let future_skew_ms = domain_last_update_ms - generated_at_ms;
            if future_skew_ms > max_future_skew_ms {
                return Self::FutureClockSkew;
            }

            return Self::Invalid;
        }

        if generated_at_ms - domain_last_update_ms > stale_after_ms {
            Self::Stale
        } else {
            Self::Live
        }
    }

    /// Map the richer design-time classifier into the current public snapshot
    /// status without exposing unpromoted states as live telemetry.
    pub fn as_snapshot_status(self) -> MiningPipelineSnapshotStatus {
        match self {
            Self::Live => MiningPipelineSnapshotStatus::Live,
            Self::Stale => MiningPipelineSnapshotStatus::Stale,
            Self::Unavailable | Self::FutureClockSkew | Self::Invalid => {
                MiningPipelineSnapshotStatus::Unavailable
            }
        }
    }
}

/// Availability state for the future mining pipeline snapshot publisher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiningPipelineSnapshotStatus {
    /// The snapshot publisher is not wired or not enabled.
    Unavailable,
    /// The snapshot publisher has a current bounded snapshot.
    Live,
    /// The snapshot publisher exists but the newest sample is too old.
    Stale,
}

impl Default for MiningPipelineSnapshotStatus {
    fn default() -> Self {
        Self::Unavailable
    }
}

impl MiningPipelineSnapshotStatus {
    /// Classify caller-owned publisher timestamps without reading runtime state.
    ///
    /// This is a pure contract helper for future publisher tests. It does not
    /// subscribe to mining events, inspect dispatcher internals, or touch
    /// hardware.
    pub fn classify_freshness(
        publisher_enabled: bool,
        publisher_last_update_ms: Option<u64>,
        generated_at_ms: u64,
        stale_after_ms: u64,
    ) -> Self {
        if !publisher_enabled {
            return Self::Unavailable;
        }

        let Some(publisher_last_update_ms) = publisher_last_update_ms else {
            return Self::Unavailable;
        };

        if publisher_last_update_ms > generated_at_ms {
            return Self::Unavailable;
        }

        MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
            Some(publisher_last_update_ms),
            generated_at_ms,
            stale_after_ms,
            0,
        )
        .as_snapshot_status()
    }
}

/// Future mining pipeline snapshot contract.
///
/// This type is intentionally observability-only. Defaults represent the
/// disabled publisher state and must not be promoted to live evidence unless the
/// mining pipeline owns a nonblocking publisher and the target miner has passed
/// hardware smoke on S9, S19 Pro, and S21.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MiningPipelineSnapshot {
    /// Contract identifier for API/dashboard consumers.
    pub schema: String,
    /// Current snapshot state.
    pub status: MiningPipelineSnapshotStatus,
    /// Whether the publisher is enabled at runtime.
    pub publisher_enabled: bool,
    /// Whether a current snapshot is available to REST consumers.
    pub snapshot_available: bool,
    /// REST consumption is read-only.
    pub read_only: bool,
    /// Snapshot reads never issue mining control actions.
    pub control_actions: bool,
    /// Snapshot reads never write hardware.
    pub hardware_writes: bool,
    /// Snapshot reads never mutate filesystem state.
    pub filesystem_mutation: bool,
    /// Response generation timestamp.
    pub generated_at_ms: u64,
    /// Last publisher update timestamp, when a publisher exists.
    pub publisher_last_update_ms: Option<u64>,
    /// Age of the newest published snapshot, when available.
    pub snapshot_age_ms: Option<u64>,
    /// Last pool mining.notify timestamp.
    pub last_notify_timestamp_ms: Option<u64>,
    /// Age of the last mining.notify relative to response generation.
    pub last_notify_age_ms: Option<u64>,
    /// Pool authorization boolean published by the Stratum status loop.
    pub pool_authorized: Option<bool>,
    /// Redacted authorization state label such as `authorized` or `mining`.
    pub pool_authorize_state: Option<String>,
    /// Current pool job ID, only when published by the mining loop.
    pub current_job_id: Option<String>,
    /// Clean-job flush counter.
    pub clean_jobs_total: Option<u64>,
    /// Work dispatch burst counter.
    pub dispatch_bursts_total: Option<u64>,
    /// Nonce burst counter.
    pub nonce_bursts_total: Option<u64>,
    /// Stale nonce drop counter.
    pub stale_nonce_drops_total: Option<u64>,
    /// Unsupported version-bits drop counter.
    pub unsupported_version_drops_total: Option<u64>,
    /// Local validation drop counter.
    pub local_validation_drops_total: Option<u64>,
    /// Accepted share lifecycle counter copied from publisher events.
    pub shares_accepted_total: Option<u64>,
    /// Rejected share lifecycle counter copied from publisher events.
    pub shares_rejected_total: Option<u64>,
    /// Lucky-share counter copied from publisher events.
    pub lucky_shares_total: Option<u64>,
    /// Last share lifecycle event timestamp.
    pub last_share_timestamp_ms: Option<u64>,
    /// Last share result label, such as `accepted`, `rejected`, or `lucky`.
    pub last_share_result: Option<String>,
    /// Last share job ID when the publisher event carried one.
    pub last_share_job_id: Option<String>,
    /// Locally computed achieved difficulty for the last share, when proven.
    pub last_share_achieved_difficulty: Option<f64>,
    /// Pool target difficulty for the last share, kept separate from achieved.
    pub last_share_target_difficulty: Option<f64>,
    /// Pool rejection error code for the last rejected share.
    pub last_share_error_code: Option<i64>,
    /// Pool rejection message for the last rejected share.
    pub last_share_error_msg: Option<String>,
    /// Work ring occupancy, when the publisher reports it.
    pub work_ring_occupancy: Option<u32>,
    /// Dispatch queue depth, when the publisher reports it.
    pub dispatch_queue_depth: Option<u32>,
    /// Provenance label for the snapshot.
    pub source: String,
    /// Operator-facing constraints.
    pub limitations: Vec<String>,
}

impl Default for MiningPipelineSnapshot {
    fn default() -> Self {
        Self {
            schema: MINING_PIPELINE_SNAPSHOT_SCHEMA.to_string(),
            status: MiningPipelineSnapshotStatus::Unavailable,
            publisher_enabled: false,
            snapshot_available: false,
            read_only: true,
            control_actions: false,
            hardware_writes: false,
            filesystem_mutation: false,
            generated_at_ms: 0,
            publisher_last_update_ms: None,
            snapshot_age_ms: None,
            last_notify_timestamp_ms: None,
            last_notify_age_ms: None,
            pool_authorized: None,
            pool_authorize_state: None,
            current_job_id: None,
            clean_jobs_total: None,
            dispatch_bursts_total: None,
            nonce_bursts_total: None,
            stale_nonce_drops_total: None,
            unsupported_version_drops_total: None,
            local_validation_drops_total: None,
            shares_accepted_total: None,
            shares_rejected_total: None,
            lucky_shares_total: None,
            last_share_timestamp_ms: None,
            last_share_result: None,
            last_share_job_id: None,
            last_share_achieved_difficulty: None,
            last_share_target_difficulty: None,
            last_share_error_code: None,
            last_share_error_msg: None,
            work_ring_occupancy: None,
            dispatch_queue_depth: None,
            source: "disabled_pipeline_snapshot_gate".to_string(),
            limitations: vec![
                "Mining pipeline snapshot publisher is disabled by default.".to_string(),
                "No current job, notify age, nonce flow, queue depth, or drop counters are inferred.".to_string(),
                "Live fields require a mining-pipeline-owned nonblocking publisher and S9/S19 Pro/S21 hardware smoke before promotion.".to_string(),
            ],
        }
    }
}

impl MiningPipelineSnapshot {
    /// Build the default unavailable snapshot for manifest and API tests.
    pub fn unavailable(generated_at_ms: u64) -> Self {
        Self {
            generated_at_ms,
            ..Self::default()
        }
    }

    /// Normalize snapshot freshness from the snapshot's own publisher fields.
    ///
    /// Future timestamps, disabled publishers, and missing publisher timestamps
    /// all fail closed as unavailable. The method preserves the no-control
    /// safety flags even if a future fixture starts from a partially populated
    /// value.
    pub fn normalize_freshness(mut self, generated_at_ms: u64, stale_after_ms: u64) -> Self {
        self.generated_at_ms = generated_at_ms;
        self.read_only = true;
        self.control_actions = false;
        self.hardware_writes = false;
        self.filesystem_mutation = false;
        self.status = MiningPipelineSnapshotStatus::classify_freshness(
            self.publisher_enabled,
            self.publisher_last_update_ms,
            generated_at_ms,
            stale_after_ms,
        );
        self.snapshot_available = matches!(self.status, MiningPipelineSnapshotStatus::Live);
        self.snapshot_age_ms = match (self.status, self.publisher_last_update_ms) {
            (
                MiningPipelineSnapshotStatus::Live | MiningPipelineSnapshotStatus::Stale,
                Some(publisher_last_update_ms),
            ) if publisher_last_update_ms <= generated_at_ms => {
                Some(generated_at_ms - publisher_last_update_ms)
            }
            _ => None,
        };
        self.last_notify_age_ms = self
            .last_notify_timestamp_ms
            .filter(|last_notify_ms| *last_notify_ms <= generated_at_ms)
            .map(|last_notify_ms| generated_at_ms - last_notify_ms);

        if self.status == MiningPipelineSnapshotStatus::Unavailable
            && (!self.publisher_enabled
                || self
                    .publisher_last_update_ms
                    .map(|updated_at| updated_at > generated_at_ms)
                    .unwrap_or(false))
        {
            self.publisher_last_update_ms = None;
        }

        self
    }

    /// Build a read-only freshness fixture from explicit publisher timestamps.
    ///
    /// This helper exists to harden the future live publisher contract before
    /// any dispatcher wiring lands. It intentionally leaves job IDs, nonce
    /// counters, queue depth, and ring occupancy unavailable.
    pub fn freshness_fixture(
        generated_at_ms: u64,
        publisher_enabled: bool,
        publisher_last_update_ms: Option<u64>,
        stale_after_ms: u64,
    ) -> Self {
        let mut snapshot = Self {
            publisher_enabled,
            publisher_last_update_ms,
            ..Self::default()
        }
        .normalize_freshness(generated_at_ms, stale_after_ms);
        snapshot.source = match snapshot.status {
            MiningPipelineSnapshotStatus::Unavailable => "freshness_fixture_unavailable",
            MiningPipelineSnapshotStatus::Live => "freshness_fixture_live",
            MiningPipelineSnapshotStatus::Stale => "freshness_fixture_stale",
        }
        .to_string();
        snapshot.limitations = match snapshot.status {
            MiningPipelineSnapshotStatus::Unavailable => vec![
                "Publisher is disabled, missing a timestamp, or reported a future timestamp.".to_string(),
                "No current job, notify age, nonce flow, queue depth, or drop counters are inferred.".to_string(),
            ],
            MiningPipelineSnapshotStatus::Live => vec![
                "Freshness was classified from explicit publisher timestamps only.".to_string(),
                "Live status does not populate job IDs, nonce flow, queue depth, or drop counters.".to_string(),
            ],
            MiningPipelineSnapshotStatus::Stale => vec![
                "Publisher timestamp is older than the configured stale window.".to_string(),
                "Stale status is not treated as snapshot_available.".to_string(),
            ],
        };
        snapshot
    }
}

// ---------------------------------------------------------------------------
// I²C write denylist — EEPROM protection contract ( B4)
// ---------------------------------------------------------------------------
//
// Hashboard EEPROMs on am2 (S19j Pro / S19 Pro Zynq) and am3-aml (S21 / S19k
// Pro) sit at I²C addresses 0x50..=0x57 on `/dev/i2c-0`. Writing to these
// addresses corrupts the board identity and (per the 2026-04-29 .74 incident)
// cannot be recovered without physical EEPROM replacement.
//
// The HAL already enforces a write-deny via `I2cBus::set_write_denylist()` on
// am2/am3-aml platform startup. This crate re-exports the same address range
// as a host-safe constant so REST/MCP/web routes that *might* expose raw
// `i2cset`-style helpers can short-circuit at the API boundary BEFORE the
// request reaches HAL — defense in depth. S9 (am1-zynq) has PIC voltage
// controllers (NOT EEPROMs) at 0x55..=0x57, so the S9 denylist stays empty.
//
// See:  + B4 regression note at
//
pub const EEPROM_WRITE_DENYLIST: &[u16] = &[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];

/// Platforms that MUST register `EEPROM_WRITE_DENYLIST` on startup. S9
/// (am1-zynq) is intentionally absent — its 0x55-0x57 are PIC controllers.
pub const EEPROM_DENYLIST_PLATFORMS: &[&str] = &["am2-zynq", "am3-aml", "am3-bb"];

/// Returns true if writes to `addr` on the given platform must be rejected.
///
/// Platform names follow the `port-bos-lux` PLATFORM_MATRIX tier IDs.
/// Unknown platforms return `false` (fail-open at the API layer; HAL is
/// still authoritative).
pub fn eeprom_write_denied(platform: &str, addr: u16) -> bool {
    EEPROM_DENYLIST_PLATFORMS.contains(&platform) && EEPROM_WRITE_DENYLIST.contains(&addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operating_mode_contract_matches_api_behavior() {
        assert_eq!(OperatingMode::Home.to_string(), "home");
        assert_eq!(OperatingMode::Standard.to_string(), "standard");
        assert_eq!(OperatingMode::Hacker.to_string(), "hacker");

        assert!(!OperatingMode::Home.allows_debug());
        assert!(!OperatingMode::Home.allows_stats());
        assert!(!OperatingMode::Home.requires_confirmation());

        assert!(!OperatingMode::Standard.allows_debug());
        assert!(OperatingMode::Standard.allows_stats());
        assert!(!OperatingMode::Standard.requires_confirmation());

        assert!(OperatingMode::Hacker.allows_debug());
        assert!(OperatingMode::Hacker.allows_stats());
        assert!(OperatingMode::Hacker.requires_confirmation());
    }

    #[test]
    fn operating_mode_config_classifier_maps_aliases_case_insensitively() {
        for mode in ["home", "HOME", "heater", "HeAtEr"] {
            let classified = OperatingMode::from_config_str(mode);
            assert_eq!(classified, OperatingMode::Home);
            assert!(classified.is_home());
        }

        for mode in ["hacker", "HACKER"] {
            let classified = OperatingMode::from_config_str(mode);
            assert_eq!(classified, OperatingMode::Hacker);
            assert!(!classified.is_home());
        }

        for mode in ["standard", "mining", "", "unexpected"] {
            let classified = OperatingMode::from_config_str(mode);
            assert_eq!(classified, OperatingMode::Standard);
            assert!(!classified.is_home());
        }
    }

    #[test]
    fn operating_mode_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&OperatingMode::Home).unwrap(),
            "\"home\""
        );
        assert_eq!(
            serde_json::from_str::<OperatingMode>("\"standard\"").unwrap(),
            OperatingMode::Standard
        );
    }

    #[test]
    fn recent_share_row_serializes_snake_case_contract() {
        let row = RecentShareRow {
            timestamp_ms: 123_456,
            result: "accepted".to_string(),
            job_id: "job-7".to_string(),
            difficulty: Some(4096.0),
            target_difficulty: Some(2048.0),
            error_code: None,
            error_msg: None,
            worker_name: Some("worker.1".to_string()),
            nonce: Some("0000002a".to_string()),
            ntime: Some("66112233".to_string()),
            extranonce2: Some("00000001".to_string()),
            version_bits: Some("20000000".to_string()),
            version: Some(0x2000_0000),
            protocol_meta_present: true,
        };

        let body = serde_json::to_value(&row).unwrap();

        assert_eq!(RECENT_SHARE_ROW_SCHEMA, "dcentos.rest.recent_share.row.v1");
        assert_eq!(body["timestamp_ms"].as_u64(), Some(123_456));
        assert_eq!(body["job_id"].as_str(), Some("job-7"));
        assert_eq!(body["difficulty"].as_f64(), Some(4096.0));
        assert_eq!(body["target_difficulty"].as_f64(), Some(2048.0));
        assert_eq!(body["protocol_meta_present"].as_bool(), Some(true));
        assert!(body.get("timestampMs").is_none());
        assert!(body.get("jobId").is_none());
        assert!(body.get("targetDifficulty").is_none());
    }

    #[test]
    fn recent_share_row_defaults_unknown_difficulty_to_null() {
        let body = serde_json::to_value(RecentShareRow {
            timestamp_ms: 7,
            result: "rejected".to_string(),
            job_id: "job-8".to_string(),
            error_code: Some(21),
            error_msg: Some("low difficulty share".to_string()),
            ..RecentShareRow::default()
        })
        .unwrap();

        assert_eq!(body["difficulty"], serde_json::Value::Null);
        assert_eq!(body["target_difficulty"], serde_json::Value::Null);
        assert_eq!(body["error_code"].as_i64(), Some(21));
        assert_eq!(body["error_msg"].as_str(), Some("low difficulty share"));
    }

    #[test]
    fn mining_pipeline_snapshot_default_is_disabled_and_unavailable() {
        let snapshot = MiningPipelineSnapshot::unavailable(42);

        assert_eq!(snapshot.schema, MINING_PIPELINE_SNAPSHOT_SCHEMA);
        assert_eq!(snapshot.status, MiningPipelineSnapshotStatus::Unavailable);
        assert!(!snapshot.publisher_enabled);
        assert!(!snapshot.snapshot_available);
        assert!(snapshot.read_only);
        assert!(!snapshot.control_actions);
        assert!(!snapshot.hardware_writes);
        assert!(!snapshot.filesystem_mutation);
        assert_eq!(snapshot.generated_at_ms, 42);
        assert!(snapshot.current_job_id.is_none());
        assert!(snapshot.last_notify_timestamp_ms.is_none());
        assert!(snapshot.last_notify_age_ms.is_none());
        assert!(snapshot.pool_authorized.is_none());
        assert!(snapshot.pool_authorize_state.is_none());
        assert!(snapshot.dispatch_queue_depth.is_none());
        assert!(snapshot.work_ring_occupancy.is_none());
        assert!(snapshot.nonce_bursts_total.is_none());
        assert!(snapshot.stale_nonce_drops_total.is_none());
        assert!(snapshot.unsupported_version_drops_total.is_none());
        assert!(snapshot.local_validation_drops_total.is_none());
        assert!(snapshot.shares_accepted_total.is_none());
        assert!(snapshot.shares_rejected_total.is_none());
        assert!(snapshot.lucky_shares_total.is_none());
        assert!(snapshot.last_share_timestamp_ms.is_none());
        assert!(snapshot.last_share_result.is_none());
        assert!(snapshot.last_share_job_id.is_none());
        assert!(snapshot.last_share_achieved_difficulty.is_none());
        assert!(snapshot.last_share_target_difficulty.is_none());
        assert!(snapshot.last_share_error_code.is_none());
        assert!(snapshot.last_share_error_msg.is_none());
        assert!(snapshot
            .limitations
            .iter()
            .any(|item| item.contains("disabled by default")));
    }

    #[test]
    fn mining_pipeline_snapshot_freshness_classifies_unavailable_live_and_stale() {
        assert_eq!(
            MiningPipelineSnapshotStatus::classify_freshness(
                false,
                Some(9_500),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
            ),
            MiningPipelineSnapshotStatus::Unavailable
        );
        assert_eq!(
            MiningPipelineSnapshotStatus::classify_freshness(
                true,
                None,
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
            ),
            MiningPipelineSnapshotStatus::Unavailable
        );
        assert_eq!(
            MiningPipelineSnapshotStatus::classify_freshness(
                true,
                Some(5_000),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
            ),
            MiningPipelineSnapshotStatus::Live
        );
        assert_eq!(
            MiningPipelineSnapshotStatus::classify_freshness(
                true,
                Some(4_999),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
            ),
            MiningPipelineSnapshotStatus::Stale
        );
        assert_eq!(
            MiningPipelineSnapshotStatus::classify_freshness(
                true,
                Some(10_001),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
            ),
            MiningPipelineSnapshotStatus::Unavailable
        );
    }

    #[test]
    fn mining_pipeline_freshness_classifier_covers_fail_closed_states() {
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                None,
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
                MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
            ),
            MiningPipelineFreshnessClassifierStatus::Unavailable
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                Some(9_500),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
                MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
            ),
            MiningPipelineFreshnessClassifierStatus::Live
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                Some(4_999),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
                MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
            ),
            MiningPipelineFreshnessClassifierStatus::Stale
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                Some(11_001),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
                MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
            ),
            MiningPipelineFreshnessClassifierStatus::FutureClockSkew
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                Some(10_001),
                10_000,
                MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
                MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
            ),
            MiningPipelineFreshnessClassifierStatus::Invalid
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::classify_domain_timestamp(
                Some(10_000),
                10_000,
                0,
                MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS,
            ),
            MiningPipelineFreshnessClassifierStatus::Invalid
        );
    }

    #[test]
    fn mining_pipeline_freshness_classifier_maps_unpromoted_states_to_unavailable_snapshot() {
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::FutureClockSkew.as_snapshot_status(),
            MiningPipelineSnapshotStatus::Unavailable
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::Invalid.as_snapshot_status(),
            MiningPipelineSnapshotStatus::Unavailable
        );
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::Unavailable.as_snapshot_status(),
            MiningPipelineSnapshotStatus::Unavailable
        );
    }

    #[test]
    fn mining_pipeline_snapshot_freshness_fixture_never_infers_pipeline_values() {
        let snapshot = MiningPipelineSnapshot::freshness_fixture(
            10_000,
            true,
            Some(9_000),
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );

        assert_eq!(snapshot.status, MiningPipelineSnapshotStatus::Live);
        assert!(snapshot.publisher_enabled);
        assert!(snapshot.snapshot_available);
        assert_eq!(snapshot.publisher_last_update_ms, Some(9_000));
        assert_eq!(snapshot.snapshot_age_ms, Some(1_000));
        assert!(snapshot.read_only);
        assert!(!snapshot.control_actions);
        assert!(!snapshot.hardware_writes);
        assert!(snapshot.current_job_id.is_none());
        assert!(snapshot.last_notify_timestamp_ms.is_none());
        assert!(snapshot.last_notify_age_ms.is_none());
        assert!(snapshot.pool_authorized.is_none());
        assert!(snapshot.pool_authorize_state.is_none());
        assert!(snapshot.dispatch_queue_depth.is_none());
        assert!(snapshot.work_ring_occupancy.is_none());
        assert!(snapshot.nonce_bursts_total.is_none());
        assert!(snapshot.stale_nonce_drops_total.is_none());
        assert!(snapshot.unsupported_version_drops_total.is_none());
        assert!(snapshot.local_validation_drops_total.is_none());

        let stale_snapshot = MiningPipelineSnapshot::freshness_fixture(
            10_001,
            true,
            Some(5_000),
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );

        assert_eq!(stale_snapshot.status, MiningPipelineSnapshotStatus::Stale);
        assert!(!stale_snapshot.snapshot_available);
        assert_eq!(stale_snapshot.snapshot_age_ms, Some(5_001));

        let unavailable_snapshot = MiningPipelineSnapshot::freshness_fixture(
            10_000,
            false,
            Some(9_000),
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );

        assert_eq!(
            unavailable_snapshot.status,
            MiningPipelineSnapshotStatus::Unavailable
        );
        assert!(!unavailable_snapshot.publisher_enabled);
        assert!(!unavailable_snapshot.snapshot_available);
        assert!(unavailable_snapshot.publisher_last_update_ms.is_none());
        assert!(unavailable_snapshot.snapshot_age_ms.is_none());
    }

    #[test]
    fn mining_pipeline_snapshot_future_timestamp_fails_closed() {
        let snapshot = MiningPipelineSnapshot::freshness_fixture(
            10_000,
            true,
            Some(10_001),
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );

        assert_eq!(snapshot.status, MiningPipelineSnapshotStatus::Unavailable);
        assert!(!snapshot.snapshot_available);
        assert!(snapshot.publisher_last_update_ms.is_none());
        assert!(snapshot.snapshot_age_ms.is_none());
        assert!(snapshot.read_only);
        assert!(!snapshot.control_actions);
        assert!(!snapshot.hardware_writes);
        assert!(!snapshot.filesystem_mutation);
    }

    #[test]
    fn mining_pipeline_snapshot_normalizes_notify_age_without_inference() {
        let snapshot = MiningPipelineSnapshot {
            publisher_enabled: true,
            publisher_last_update_ms: Some(10_000),
            last_notify_timestamp_ms: Some(9_750),
            pool_authorized: Some(true),
            pool_authorize_state: Some("authorized".to_string()),
            ..MiningPipelineSnapshot::default()
        }
        .normalize_freshness(10_250, MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS);

        assert_eq!(snapshot.status, MiningPipelineSnapshotStatus::Live);
        assert_eq!(snapshot.last_notify_age_ms, Some(500));
        assert_eq!(snapshot.pool_authorized, Some(true));
        assert_eq!(snapshot.pool_authorize_state.as_deref(), Some("authorized"));

        let future_notify = MiningPipelineSnapshot {
            publisher_enabled: true,
            publisher_last_update_ms: Some(10_000),
            last_notify_timestamp_ms: Some(10_251),
            ..MiningPipelineSnapshot::default()
        }
        .normalize_freshness(10_250, MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS);
        assert_eq!(future_notify.last_notify_age_ms, None);
    }

    #[test]
    fn normalize_freshness_forces_safety_flags_even_when_inputs_were_unsafe() {
        // Build a snapshot that deliberately violates the read-only contract,
        // then prove normalize_freshness forces every safety flag back to
        // its safe default. This is load-bearing for the API: snapshots are
        // observability-only and must never report `control_actions=true`,
        // `hardware_writes=true`, or `filesystem_mutation=true` to consumers.
        let unsafe_snapshot = MiningPipelineSnapshot {
            read_only: false,
            control_actions: true,
            hardware_writes: true,
            filesystem_mutation: true,
            publisher_enabled: true,
            publisher_last_update_ms: Some(9_500),
            ..MiningPipelineSnapshot::default()
        };

        let normalized = unsafe_snapshot
            .normalize_freshness(10_000, MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS);

        assert!(normalized.read_only, "read_only must be forced true");
        assert!(
            !normalized.control_actions,
            "control_actions must be forced false"
        );
        assert!(
            !normalized.hardware_writes,
            "hardware_writes must be forced false"
        );
        assert!(
            !normalized.filesystem_mutation,
            "filesystem_mutation must be forced false"
        );
        assert_eq!(normalized.generated_at_ms, 10_000);
        assert_eq!(normalized.status, MiningPipelineSnapshotStatus::Live);
        assert!(normalized.snapshot_available);
    }

    #[test]
    fn normalize_freshness_clears_publisher_timestamp_when_publisher_disabled() {
        // A disabled publisher whose `publisher_last_update_ms` is still set
        // must end up Unavailable AND have its leaked timestamp cleared so
        // downstream consumers cannot infer a phantom snapshot age.
        let stale_disabled = MiningPipelineSnapshot {
            publisher_enabled: false,
            publisher_last_update_ms: Some(7_000),
            ..MiningPipelineSnapshot::default()
        };

        let normalized = stale_disabled
            .normalize_freshness(10_000, MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS);

        assert_eq!(normalized.status, MiningPipelineSnapshotStatus::Unavailable);
        assert!(!normalized.publisher_enabled);
        assert!(
            normalized.publisher_last_update_ms.is_none(),
            "leaked publisher timestamp must be cleared when publisher is disabled"
        );
        assert!(normalized.snapshot_age_ms.is_none());
        assert!(!normalized.snapshot_available);
    }

    #[test]
    fn normalize_freshness_keeps_age_for_stale_status() {
        // Stale snapshots must report their age so dashboards can show
        // "last updated N seconds ago" — the snapshot is unavailable but
        // age should still be derivable. Stale is the boundary case: status
        // is not Live (so snapshot_available is false) yet the publisher
        // timestamp is real and trustworthy for age reporting.
        let stale = MiningPipelineSnapshot {
            publisher_enabled: true,
            publisher_last_update_ms: Some(2_000),
            ..MiningPipelineSnapshot::default()
        };

        let normalized =
            stale.normalize_freshness(10_000, MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS);

        assert_eq!(normalized.status, MiningPipelineSnapshotStatus::Stale);
        assert!(!normalized.snapshot_available);
        assert_eq!(normalized.publisher_last_update_ms, Some(2_000));
        assert_eq!(normalized.snapshot_age_ms, Some(8_000));
    }

    #[test]
    fn operating_mode_round_trips_all_three_modes_through_json() {
        // Pin every mode's JSON contract in both directions so a regression
        // can't silently rename one (e.g. "Hacker" → "advanced") without
        // breaking the wire format clients depend on.
        for (mode, expected) in [
            (OperatingMode::Home, "\"home\""),
            (OperatingMode::Standard, "\"standard\""),
            (OperatingMode::Hacker, "\"hacker\""),
        ] {
            let serialized = serde_json::to_string(&mode).unwrap();
            assert_eq!(
                serialized, expected,
                "mode {:?} serialized to wrong JSON",
                mode
            );
            let deserialized: OperatingMode = serde_json::from_str(expected).unwrap();
            assert_eq!(
                deserialized, mode,
                "round trip for {:?} did not match",
                mode
            );
        }

        // Reject malformed input without panic. The API safety classifier
        // must fail closed if a config file ships an unknown mode name.
        assert!(serde_json::from_str::<OperatingMode>("\"advanced\"").is_err());
        assert!(serde_json::from_str::<OperatingMode>("\"HOME\"").is_err());
        assert!(serde_json::from_str::<OperatingMode>("null").is_err());
    }

    // -----------------------------------------------------------------------
    // Schema-string constants and wire-form pinning.
    //
    // dcentrald-api-types is the public contract used by the dashboard,
    // toolbox, REST clients, and prometheus exporters. Every schema
    // string and every wire field name is load-bearing for downstream
    // consumers. Pin the constants and snake_case wire form so a
    // refactor cannot silently flip schema versions or rename fields.
    // -----------------------------------------------------------------------

    #[test]
    fn schema_constants_are_pinned() {
        // Bump only when downstream consumers (dashboard, toolbox,
        // prometheus) are coordinated. A silent bump silently breaks
        // all of them.
        assert_eq!(
            MINING_PIPELINE_SNAPSHOT_SCHEMA,
            "dcentos.mining.pipeline.snapshot.v1"
        );
        assert_eq!(
            MINING_PIPELINE_FRESHNESS_CLASSIFIER_SCHEMA,
            "dcentos.mining.pipeline.freshness.classifier.v1"
        );
        assert_eq!(RECENT_SHARE_ROW_SCHEMA, "dcentos.rest.recent_share.row.v1");
        assert_eq!(API_CONTRACT_VERSION, "dcentos.api.v1");
    }

    #[test]
    fn freshness_default_constants_are_pinned() {
        // Stale after 5 seconds, future-skew tolerance 1 second. Pin so
        // a refactor that lowered the stale window to 1 second wouldn't
        // silently start marking everything stale.
        assert_eq!(MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS, 5_000);
        assert_eq!(MINING_PIPELINE_FRESHNESS_DEFAULT_MAX_FUTURE_SKEW_MS, 1_000);
    }

    #[test]
    fn mining_pipeline_snapshot_serializes_in_snake_case_wire_form() {
        // api-types uses snake_case (different from dcent-schema's
        // camelCase). The dashboard, toolbox, and prometheus exporters
        // all read snake_case; a refactor that flipped to camelCase
        // would break them all.
        let snapshot = MiningPipelineSnapshot::default();
        let json = serde_json::to_value(&snapshot).unwrap();

        // Positive pins for every snake_case field.
        for field in [
            "schema",
            "status",
            "publisher_enabled",
            "snapshot_available",
            "read_only",
            "control_actions",
            "hardware_writes",
            "filesystem_mutation",
            "generated_at_ms",
            "publisher_last_update_ms",
            "snapshot_age_ms",
            "last_notify_timestamp_ms",
            "last_notify_age_ms",
            "pool_authorized",
            "pool_authorize_state",
            "current_job_id",
            "clean_jobs_total",
            "dispatch_bursts_total",
            "nonce_bursts_total",
            "stale_nonce_drops_total",
            "unsupported_version_drops_total",
            "local_validation_drops_total",
            "shares_accepted_total",
            "shares_rejected_total",
            "lucky_shares_total",
            "last_share_timestamp_ms",
            "last_share_result",
            "last_share_job_id",
            "last_share_achieved_difficulty",
            "last_share_target_difficulty",
            "last_share_error_code",
            "last_share_error_msg",
            "work_ring_occupancy",
            "dispatch_queue_depth",
            "source",
            "limitations",
        ] {
            assert!(
                json.get(field).is_some(),
                "MiningPipelineSnapshot must expose {field} in snake_case"
            );
        }

        // Negative pins: camelCase must NOT appear.
        for forbidden in [
            "publisherEnabled",
            "snapshotAvailable",
            "readOnly",
            "controlActions",
            "hardwareWrites",
            "filesystemMutation",
            "generatedAtMs",
            "publisherLastUpdateMs",
            "snapshotAgeMs",
            "lastNotifyTimestampMs",
            "lastNotifyAgeMs",
            "poolAuthorized",
            "poolAuthorizeState",
            "currentJobId",
            "cleanJobsTotal",
            "dispatchBurstsTotal",
            "nonceBurstsTotal",
            "staleNonceDropsTotal",
            "unsupportedVersionDropsTotal",
            "localValidationDropsTotal",
            "sharesAcceptedTotal",
            "sharesRejectedTotal",
            "luckySharesTotal",
            "lastShareTimestampMs",
            "lastShareResult",
            "lastShareJobId",
            "lastShareAchievedDifficulty",
            "lastShareTargetDifficulty",
            "lastShareErrorCode",
            "lastShareErrorMsg",
            "workRingOccupancy",
            "dispatchQueueDepth",
        ] {
            assert!(
                json.get(forbidden).is_none(),
                "MiningPipelineSnapshot must NOT serialize {forbidden} (camelCase form)"
            );
        }
    }

    #[test]
    fn mining_pipeline_snapshot_default_carries_disabled_sentinel_strings() {
        // The default snapshot's `source` and `limitations` are operator-
        // facing diagnostic strings that the dashboard reads to explain
        // why fields are blank. Pin the exact sentinel text so a
        // refactor cannot silently change what operators see.
        let snapshot = MiningPipelineSnapshot::default();
        assert_eq!(snapshot.source, "disabled_pipeline_snapshot_gate");
        assert_eq!(snapshot.limitations.len(), 3);
        assert!(snapshot.limitations[0].contains("disabled by default"));
        assert!(snapshot.limitations[1].contains("No current job"));
        assert!(snapshot.limitations[2].contains("hardware smoke"));
    }

    #[test]
    fn freshness_fixture_source_strings_are_pinned() {
        // The fixture sets `source` to a specific sentinel string per
        // status. Dashboards filter on these strings to render the
        // right diagnostic banner. Pin all three.
        let live = MiningPipelineSnapshot::freshness_fixture(
            10_000,
            true,
            Some(9_000),
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );
        assert_eq!(live.source, "freshness_fixture_live");

        let stale = MiningPipelineSnapshot::freshness_fixture(
            10_001,
            true,
            Some(5_000),
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );
        assert_eq!(stale.source, "freshness_fixture_stale");

        let unavailable = MiningPipelineSnapshot::freshness_fixture(
            10_000,
            false,
            None,
            MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS,
        );
        assert_eq!(unavailable.source, "freshness_fixture_unavailable");
    }

    #[test]
    fn mining_pipeline_snapshot_status_serializes_in_snake_case() {
        // The enum is `#[serde(rename_all = "snake_case")]`. Pin every
        // variant's wire form.
        assert_eq!(
            serde_json::to_string(&MiningPipelineSnapshotStatus::Unavailable).unwrap(),
            "\"unavailable\""
        );
        assert_eq!(
            serde_json::to_string(&MiningPipelineSnapshotStatus::Live).unwrap(),
            "\"live\""
        );
        assert_eq!(
            serde_json::to_string(&MiningPipelineSnapshotStatus::Stale).unwrap(),
            "\"stale\""
        );
    }

    #[test]
    fn mining_pipeline_freshness_classifier_status_serializes_in_snake_case() {
        // Five variants — pin each. `future_clock_skew` is a multi-word
        // variant that must serialize with the underscore.
        assert_eq!(
            serde_json::to_string(&MiningPipelineFreshnessClassifierStatus::Unavailable).unwrap(),
            "\"unavailable\""
        );
        assert_eq!(
            serde_json::to_string(&MiningPipelineFreshnessClassifierStatus::Live).unwrap(),
            "\"live\""
        );
        assert_eq!(
            serde_json::to_string(&MiningPipelineFreshnessClassifierStatus::Stale).unwrap(),
            "\"stale\""
        );
        assert_eq!(
            serde_json::to_string(&MiningPipelineFreshnessClassifierStatus::FutureClockSkew)
                .unwrap(),
            "\"future_clock_skew\""
        );
        assert_eq!(
            serde_json::to_string(&MiningPipelineFreshnessClassifierStatus::Invalid).unwrap(),
            "\"invalid\""
        );
    }

    #[test]
    fn freshness_classifier_status_round_trips_through_json() {
        for variant in [
            MiningPipelineFreshnessClassifierStatus::Unavailable,
            MiningPipelineFreshnessClassifierStatus::Live,
            MiningPipelineFreshnessClassifierStatus::Stale,
            MiningPipelineFreshnessClassifierStatus::FutureClockSkew,
            MiningPipelineFreshnessClassifierStatus::Invalid,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let recovered: MiningPipelineFreshnessClassifierStatus =
                serde_json::from_str(&json).unwrap();
            assert_eq!(recovered, variant);
        }
    }

    #[test]
    fn freshness_classifier_default_is_unavailable() {
        // Default is unavailable — fail-closed. A refactor that flipped
        // the default to Live would silently make every freshly-instantiated
        // snapshot appear as live telemetry.
        assert_eq!(
            MiningPipelineFreshnessClassifierStatus::default(),
            MiningPipelineFreshnessClassifierStatus::Unavailable
        );
    }

    #[test]
    fn mining_pipeline_snapshot_status_default_is_unavailable() {
        // Same fail-closed default for the snapshot status enum.
        assert_eq!(
            MiningPipelineSnapshotStatus::default(),
            MiningPipelineSnapshotStatus::Unavailable
        );
    }

    #[test]
    fn recent_share_row_protocol_meta_present_round_trips() {
        // protocol_meta_present is a bool indicating whether the share
        // event has full protocol metadata available. Pin the field's
        // round trip and snake_case wire form.
        let row_with_meta = RecentShareRow {
            protocol_meta_present: true,
            ..RecentShareRow::default()
        };
        let json = serde_json::to_value(&row_with_meta).unwrap();
        assert_eq!(json["protocol_meta_present"].as_bool(), Some(true));
        assert!(json.get("protocolMetaPresent").is_none());

        let recovered: RecentShareRow = serde_json::from_value(json).unwrap();
        assert!(recovered.protocol_meta_present);
    }

    #[test]
    fn recent_share_row_default_initializes_safely() {
        // Default produces a minimal valid row with no share data — used
        // when an event has not been observed yet. Pin the default state.
        let default = RecentShareRow::default();
        assert_eq!(default.timestamp_ms, 0);
        assert_eq!(default.result, "");
        assert_eq!(default.job_id, "");
        assert!(default.difficulty.is_none());
        assert!(default.target_difficulty.is_none());
        assert!(default.error_code.is_none());
        assert!(default.error_msg.is_none());
        assert!(default.worker_name.is_none());
        assert!(default.nonce.is_none());
        assert!(default.ntime.is_none());
        assert!(default.extranonce2.is_none());
        assert!(default.version_bits.is_none());
        assert!(default.version.is_none());
        assert!(!default.protocol_meta_present);
    }

    #[test]
    fn mining_pipeline_snapshot_default_safety_invariants() {
        // The default state must be safe — read-only with no control
        // actions or hardware writes. Already partially pinned by Wave
        // 9 but explicitly redundant here to make the contract obvious.
        let snapshot = MiningPipelineSnapshot::default();
        assert!(snapshot.read_only);
        assert!(!snapshot.control_actions);
        assert!(!snapshot.hardware_writes);
        assert!(!snapshot.filesystem_mutation);
        assert!(!snapshot.publisher_enabled);
        assert!(!snapshot.snapshot_available);
        assert_eq!(snapshot.status, MiningPipelineSnapshotStatus::Unavailable);
    }

    #[test]
    fn mining_pipeline_snapshot_unavailable_carries_provided_timestamp() {
        // `unavailable(generated_at_ms)` should set ONLY the timestamp
        // and leave every other field at default. Pin so a refactor
        // doesn't accidentally start populating other fields.
        let snapshot = MiningPipelineSnapshot::unavailable(123_456);
        assert_eq!(snapshot.generated_at_ms, 123_456);
        assert_eq!(snapshot.status, MiningPipelineSnapshotStatus::Unavailable);
        assert_eq!(snapshot.source, "disabled_pipeline_snapshot_gate");
    }

    #[test]
    fn eeprom_denylist_covers_full_am2_am3_range() {
        // The 0x50..=0x57 range is ALL eight addresses inclusive — drop one and
        // the EEPROM corruption window opens. This pin makes that explicit.
        assert_eq!(EEPROM_WRITE_DENYLIST.len(), 8);
        for addr in 0x50..=0x57u16 {
            assert!(
                EEPROM_WRITE_DENYLIST.contains(&addr),
                "addr 0x{:02x} missing from denylist",
                addr
            );
        }
    }

    #[test]
    fn eeprom_denylist_excludes_pic_and_dac_addresses() {
        // 0x20/0x21/0x22 = dsPIC on am2/am3
        // 0x49/0x4A/0x4B = TAS5782M voltage DACs on S21
        // 0x55/0x56/0x57 are PICs on S9 — the per-platform helper covers that.
        for addr in [0x20u16, 0x21, 0x22, 0x49, 0x4A, 0x4B] {
            assert!(
                !EEPROM_WRITE_DENYLIST.contains(&addr),
                "addr 0x{:02x} must NOT be in EEPROM denylist (it is a controller, not EEPROM)",
                addr
            );
        }
    }

    #[test]
    fn eeprom_write_denied_blocks_eeprom_on_am2_am3() {
        // Hashboard EEPROM addresses on am2/am3-aml/am3-bb are denied.
        for platform in ["am2-zynq", "am3-aml", "am3-bb"] {
            for addr in 0x50u16..=0x57 {
                assert!(
                    eeprom_write_denied(platform, addr),
                    "platform {} addr 0x{:02x} must be denied",
                    platform,
                    addr
                );
            }
        }
    }

    #[test]
    fn eeprom_write_denied_allows_pic_on_s9() {
        // S9 (am1-zynq) has PIC voltage controllers at 0x55-0x57, NOT EEPROMs.
        // Writes to these addresses are LEGITIMATE on S9 (heartbeat, voltage).
        for addr in 0x55u16..=0x57 {
            assert!(
                !eeprom_write_denied("am1-zynq", addr),
                "S9 PIC at 0x{:02x} must NOT be on the denylist",
                addr
            );
        }
    }

    #[test]
    fn eeprom_write_denied_unknown_platform_fails_open() {
        // Unknown/empty platform IDs should return false; the HAL is still
        // authoritative and will deny via per-bus denylist if applicable.
        assert!(!eeprom_write_denied("", 0x50));
        assert!(!eeprom_write_denied("unknown-platform", 0x50));
    }

    #[test]
    fn eeprom_denylist_platforms_lists_known_tiers() {
        // Per port-bos-lux PLATFORM_MATRIX, am2-zynq + am3-aml + am3-bb need
        // the denylist. am1-zynq (S9) explicitly does NOT.
        assert!(EEPROM_DENYLIST_PLATFORMS.contains(&"am2-zynq"));
        assert!(EEPROM_DENYLIST_PLATFORMS.contains(&"am3-aml"));
        assert!(EEPROM_DENYLIST_PLATFORMS.contains(&"am3-bb"));
        assert!(!EEPROM_DENYLIST_PLATFORMS.contains(&"am1-zynq"));
    }
}
