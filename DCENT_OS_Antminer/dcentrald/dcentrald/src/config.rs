//! Configuration system for dcentrald.
//!
//! Loads and validates the dcentrald.toml configuration file. The config
//! structure matches the TOML schema defined in the architecture document
//! Section 11. All fields have sane defaults for home mining.
//!
//! File locations:
//!   /data/dcentrald.toml   - Primary config (persistent UBIFS storage)
//!   /etc/dcentrald.toml    - Default config (read-only squashfs, fallback)

use anyhow::{Context, Result};
use dcentrald_api::{solar_provider_support, supported_solar_providers, NetworkBlockConfig};
use dcentrald_stratum::types::{
    DonationConfig as StratumDonationConfig, PoolConfig as StratumPoolConfig, StratumConfig,
};
use dcentrald_stratum::url_validator::{validate_sv2_pool_url, validate_v1_pool_url};
use serde::{Deserialize, Serialize};
use std::path::Path;

const AM1_S9_MAX_CHIP_RAIL_MV: u16 = 9_400;
const MAX_PERSISTED_CONFIG_BYTES: usize = 1024 * 1024;

use crate::model;

/// Publish daemon configuration through the common bounded crash-durable
/// state-file contract. Unique sibling staging, target type and symlink
/// rejection, metadata preservation, file synchronization, atomic rename, and
/// parent-directory synchronization are shared with the other daemon state
/// writers. The one-mebibyte ceiling makes the accepted persistence envelope
/// explicit before any target replacement.
pub(crate) fn atomic_write(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    dcentrald_common::atomic_file::atomic_write(
        dest,
        bytes,
        dcentrald_common::atomic_file::AtomicWriteOptions::state_file(MAX_PERSISTED_CONFIG_BYTES),
    )
    .map(|_| ())
    .map_err(dcentrald_common::atomic_file::AtomicWriteError::into_io_error)
}

/// Convert daemon donation settings into the Stratum router contract.
pub fn stratum_donation_config(config: &DonationConfig) -> StratumDonationConfig {
    StratumDonationConfig {
        enabled: config.enabled,
        percent: config.percent,
        pool_url: config.pool_url.clone(),
        worker: config.worker.clone(),
        password: config.password.clone(),
        fallback_enabled: config.fallback_enabled,
        fallback_pool_url: config.fallback_pool_url.clone(),
        fallback_worker: config.fallback_worker.clone(),
        fallback_password: config.fallback_password.clone(),
        cycle_duration_s: config.cycle_duration_s,
    }
}

/// Donation-off Stratum config for diagnostics that must not time-slice pools.
pub fn disabled_stratum_donation_config() -> StratumDonationConfig {
    StratumDonationConfig {
        enabled: false,
        percent: 0.0,
        pool_url: String::new(),
        worker: String::new(),
        password: String::from("x"),
        fallback_enabled: false,
        fallback_pool_url: String::new(),
        fallback_worker: String::new(),
        fallback_password: String::from("x"),
        cycle_duration_s: 7200,
    }
}

fn primary_stratum_pool(config: &PoolConfig) -> StratumPoolConfig {
    StratumPoolConfig {
        url: config.url.clone(),
        worker: config.worker.clone(),
        password: config.password.clone(),
        sv2_url: config.sv2_url.clone(),
        protocol: config.protocol.clone(),
        split_bps: config.split_bps,
    }
}

fn failover_stratum_pool(config: &PoolEndpoint) -> StratumPoolConfig {
    StratumPoolConfig {
        url: config.url.clone(),
        worker: config.worker.clone(),
        password: config.password.clone(),
        sv2_url: config.sv2_url.clone(),
        protocol: config.protocol.clone(),
        split_bps: config.split_bps,
    }
}

/// Build one Stratum router config shape for every daemon mining path.
///
/// Runtime-only lanes used to duplicate this by hand and several paths dropped
/// `[pool.failover1]` / `[pool.failover2]`. Keep pool routing centralized so
/// accepted config is what the mining loop actually uses.
pub fn build_stratum_config(
    config: &DcentraldConfig,
    donation: StratumDonationConfig,
    version_rolling: bool,
    sv2_extended_channel: bool,
) -> StratumConfig {
    StratumConfig {
        pool1: primary_stratum_pool(&config.pool),
        pool2: config.pool.failover1.as_ref().map(failover_stratum_pool),
        pool3: config.pool.failover2.as_ref().map(failover_stratum_pool),
        routing_mode: config.pool.routing_mode.clone(),
        split_cycle_duration_s: config.pool.split_cycle_duration_s,
        // Operator-settable since armada 2026-06-09; the unwrap_or values are
        // the previously-hardcoded shipping defaults -- unset configs are
        // byte-identical to the pre-knob daemon.
        primary_return_stability_secs: config.pool.primary_return_stability_secs.unwrap_or(900),
        no_notify_failover_secs: config.pool.no_notify_failover_secs.unwrap_or(300),
        reject_rate_failover_pct: config.pool.reject_rate_failover_pct.unwrap_or(0),
        reject_rate_failover_min_samples: config
            .pool
            .reject_rate_failover_min_samples
            .unwrap_or(100),
        smart_failover_enabled: config.pool.smart_failover_enabled,
        smart_failover_drive: config.pool.smart_failover_drive,
        sv2_max_inbound_frame_bytes: 1_048_576,
        v1_max_inbound_line_bytes: 65_536,
        donation,
        version_rolling,
        version_rolling_mask: config.mining.version_rolling_mask,
        suggest_difficulty: Some(config.mining.suggest_difficulty),
        hash_on_disconnect: config.hash_on_disconnect.enabled,
        protocol: config.pool.protocol.clone(),
        nominal_hashrate_ghs: 0.0,
        sv2_extended_channel,
    }
}

/// Top-level dcentrald configuration.
///
/// `deny_unknown_fields` is set on every struct in this file. Any TOML key
/// that doesn't map to a named field becomes a load-time error instead of
/// silently getting discarded. This catches typos and stale templates
/// BEFORE they cause bring-up regressions. See CE agent audit 06-ce.md
/// finding #1: a phantom `[platform]` section silently zeroed 8 safety
/// flags on the am2 template.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DcentraldConfig {
    #[serde(default)]
    pub general: GeneralConfig,

    /// W1.4: structured-logging hardening. Defaults to masking wallet
    /// addresses on the log-tail passthrough. See `LoggingConfig`.
    #[serde(default)]
    pub logging: LoggingConfig,

    #[serde(default)]
    pub pool: PoolConfig,

    #[serde(default)]
    pub mining: MiningConfig,

    #[serde(default)]
    pub power: PowerConfig,

    #[serde(default)]
    pub thermal: ThermalConfig,

    #[serde(default)]
    pub api: ApiConfig,

    #[serde(default)]
    pub network_block: NetworkBlockConfig,

    #[serde(default)]
    pub donation: DonationConfig,

    #[serde(default)]
    pub mqtt: MqttConfig,

    #[serde(default)]
    pub watchdog: WatchdogConfig,

    #[serde(default)]
    pub hash_on_disconnect: HashOnDisconnectConfig,

    #[serde(default)]
    pub mode: ModeConfig,

    /// Legacy top-level heater section from older flashed images. Current
    /// config moved this under `mode.home`, but field-reality S9 units still
    /// persist `[heater]` in `/data/dcentrald.toml`.
    #[serde(default, rename = "heater", skip_serializing)]
    legacy_heater: Option<LegacyHeaterConfig>,

    #[serde(default)]
    pub autotuner: dcentrald_autotuner::AutoTunerConfig,

    /// Supremacy S4-03 + S5-02: closed-loop power-target controller +
    /// 6-variant `TunerMode` strategy enum.
    ///
    /// Distinct from `[autotuner]` (per-chip frequency search via the
    /// `dcentrald-autotuner` crate). The `[autotune.power_target]`
    /// subsection drives a PI controller that adjusts frequency to
    /// hit a wall/chip wattage target while respecting HARD voltage
    /// (≤14500 mV am2) and fan (≤30 in home mode) clamps.
    ///
    /// S5-02 adds `[autotune] mode = "manual" | "performance" |
    /// "power-target" | "hashrate-target" | "efficiency" | "heater"`
    /// — a unified strategy enum with `Efficiency` + `Heater` as
    /// DCENT_OS-unique modes (Braiins lacks them). All non-`Manual`
    /// modes enforce voltage ≤14500 mV, ±5 MHz/tick slew, and
    /// envelope clamps; `Heater` additionally enforces fan ≤30 PWM
    /// (intrinsic home-mode). See `crate::autotune::tuner_mode` for
    /// the implementation. The default is `Manual { freq: 0, voltage: 0 }`
    /// — least-surprise; the daemon should re-seed via
    /// `TunerMode::default_manual_at(current_freq, current_voltage)`
    /// at startup.
    #[serde(default)]
    pub autotune: crate::autotune::AutotuneConfig,

    #[serde(default)]
    pub led: LedConfig,

    #[serde(default)]
    pub sv2: Sv2Config,

    #[serde(default)]
    pub job_declaration: JobDeclarationConfig,

    #[serde(default)]
    pub webhook: Option<WebhookConfig>,

    #[serde(default)]
    pub psu: PsuConfig,

    #[serde(default)]
    pub hashboard: HashboardConfig,

    /// Stratum V1 TCP relay (Phase 11B MVP). Optional — only used when
    /// dcentrald is launched with `--stratum-proxy`. Leaving this unset has
    /// no effect on any other mode.
    #[serde(default)]
    pub stratum_proxy: Option<StratumProxyConfig>,

    /// DCENT Expansion Pack ("dcent-pack") bridge client. Disabled by default;
    /// a missing `[bridge]` block deserializes to `BridgeConfig::default()`
    /// (enabled = false), so existing configs and `validate()` are unaffected.
    /// See `dcentrald_bridge::BridgeConfig`.
    #[serde(default)]
    pub bridge: dcentrald_bridge::BridgeConfig,
}

impl DcentraldConfig {
    /// Load configuration from a TOML file.
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn load(path: &str) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path))?;
        let mut config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file: {}", path))?;
        config.normalize_legacy_fields()?;
        config.validate()?;
        Ok(config)
    }

    /// A safe, fully-defaulted, MINING-DISABLED config for fail-closed
    /// management-only boot when no valid config can be loaded.
    ///
    /// Every field of `DcentraldConfig` is `#[serde(default)]`, so an empty TOML
    /// document deserializes to the baked-in defaults; we then force
    /// `mining.enabled = false` so the daemon enters management-only
    /// (`mining_start_enabled() == false`) — API / dashboard / wizard /
    /// re-flash-detection reachable, NO PSU or chain I/O — instead of
    /// exiting into a persistent-session admission refusal when both config
    /// paths are missing or corrupt. The empty-document parse is infallible by
    /// construction; `management_only_default_is_fail_closed` pins that invariant
    /// so a future field that drops `#[serde(default)]` fails CI rather than the
    /// `expect()` firing in the field on a real unit. (gap-swarm daemon-startup #1/#9)
    pub fn management_only_default() -> Self {
        let mut cfg: Self = toml::from_str("").expect(
            "empty TOML must parse to all-serde-defaults — every DcentraldConfig field is \
             #[serde(default)] (pinned by management_only_default_is_fail_closed)",
        );
        cfg.mining.enabled = false;
        cfg
    }

    /// Save configuration to a TOML file.
    ///
    /// WAVE 0 STABILIZE (2026-06-05) — atomic write. The previous
    /// `std::fs::write(path, ..)` truncated the live config in place: a crash
    /// or power loss mid-write (the S9 is a space heater that gets
    /// power-cycled) could leave `/data/dcentrald.toml` truncated/garbage,
    /// which then fails to parse on the next boot. Route every config write
    /// through a sibling tmp file + fsync + atomic rename so the on-disk config
    /// is always either the complete old contents or the complete new contents
    /// — never a partial. The directory is fsync'd too so the rename itself
    /// survives a power loss.
    pub fn save(&self, path: &str) -> Result<()> {
        let contents = toml::to_string_pretty(self).context("failed to serialize config")?;
        atomic_write(Path::new(path), contents.as_bytes())
            .with_context(|| format!("failed to write config file: {}", path))?;
        Ok(())
    }

    pub fn has_configured_pool(&self) -> bool {
        !self.pool.url.trim().is_empty() && !self.pool.worker.trim().is_empty()
    }

    pub fn mining_start_enabled(&self) -> bool {
        self.mining.enabled && self.has_configured_pool()
    }

    fn normalize_legacy_fields(&mut self) -> Result<()> {
        self.power.normalize_legacy_fields()?;

        if self.mode.active.eq_ignore_ascii_case("heater") {
            self.mode.active = "home".to_string();
        }

        if let Some(heater) = self.legacy_heater.take() {
            if heater.target_watts > 0 {
                self.mode.home.target_watts = heater.target_watts;
            }

            self.mode.home.night_mode.enabled = heater.night_mode;
            self.mode.home.night_mode.start_hour = heater.night_start_hour;
            self.mode.home.night_mode.end_hour = heater.night_end_hour;

            if heater.target_watts > 0
                && heater.night_target_watts > 0
                && heater.night_target_watts < heater.target_watts
            {
                let reduction = 100.0
                    - ((heater.night_target_watts as f32 / heater.target_watts as f32) * 100.0);
                self.mode.home.night_mode.power_reduction_pct =
                    reduction.round().clamp(0.0, 100.0) as u8;
            }
        }

        Ok(())
    }

    /// Validate configuration values.
    pub fn validate(&self) -> Result<()> {
        if self.thermal.target_temp_c >= self.thermal.hot_temp_c {
            anyhow::bail!(
                "thermal.target_temp_c ({}) must be less than thermal.hot_temp_c ({})",
                self.thermal.target_temp_c,
                self.thermal.hot_temp_c
            );
        }
        if self.thermal.hot_temp_c >= self.thermal.dangerous_temp_c {
            anyhow::bail!(
                "thermal.hot_temp_c ({}) must be less than thermal.dangerous_temp_c ({})",
                self.thermal.hot_temp_c,
                self.thermal.dangerous_temp_c
            );
        }
        // Hard ceiling on dangerous_temp_c for residential safety.
        // No mode may exceed 90C — PCB trace delamination starts at ~100C,
        // and solder joint weakening at ~95C.
        if self.thermal.dangerous_temp_c > 90 {
            anyhow::bail!(
                "thermal.dangerous_temp_c ({}) must be <= 90 (residential safety limit)",
                self.thermal.dangerous_temp_c
            );
        }
        if self.mode.hacker.dangerous_temp_override > 90 {
            anyhow::bail!(
                "mode.hacker.dangerous_temp_override ({}) must be <= 90 (residential safety limit)",
                self.mode.hacker.dangerous_temp_override
            );
        }
        if self.donation.percent < 0.0 || self.donation.percent > 5.0 {
            anyhow::bail!(
                "donation.percent ({}) must be between 0.0 and 5.0",
                self.donation.percent
            );
        }
        if self.donation.cycle_duration_s < 60 || self.donation.cycle_duration_s > 86400 {
            anyhow::bail!(
                "donation.cycle_duration_s ({}) must be between 60 and 86400",
                self.donation.cycle_duration_s
            );
        }
        if let Err(e) = self.network_block.validate() {
            anyhow::bail!(e);
        }
        if self.api.http_bind.parse::<std::net::IpAddr>().is_err() {
            anyhow::bail!(
                "api.http_bind ('{}') must be an IP address such as 0.0.0.0, 127.0.0.1, or ::",
                self.api.http_bind
            );
        }
        if self.mining.pipeline_snapshot.enabled
            && self.mining.pipeline_snapshot.stale_after_ms == 0
        {
            anyhow::bail!(
                "mining.pipeline_snapshot.stale_after_ms must be > 0 when mining.pipeline_snapshot.enabled is true"
            );
        }
        // PH-3: the recovery ladder validates its bounds only when enabled
        // (a disabled/default ladder always passes, so it never blocks boot).
        if let Err(e) = self.mining.recovery_ladder.validate() {
            anyhow::bail!(e);
        }
        self.mining.validate_serial_devices()?;
        // serial_chip_count is the chain address-assignment denominator
        // (256 / chip_count) on the BM1362/BM1398 serial + XIL hybrid paths.
        // A hand-edited 0 would divide-by-zero panic at chain init (panic=abort
        // aborts the daemon). Fail closed at load instead.
        if let Some(0) = self.mining.serial_chip_count {
            anyhow::bail!(
                "mining.serial_chip_count must be >= 1 (0 chips is not a valid chain geometry; \
                 it divide-by-zero panics chain address assignment at init)"
            );
        }
        if let Some(min_chip_fraction) = self.mining.min_chip_fraction {
            if !min_chip_fraction.is_finite() || !(0.0..=1.0).contains(&min_chip_fraction) {
                anyhow::bail!(
                    "mining.min_chip_fraction ({}) must be a finite fraction between 0.0 and 1.0",
                    min_chip_fraction
                );
            }
        }
        if self.power.target_watts > self.power.max_watts {
            anyhow::bail!(
                "power.target_watts ({}) cannot exceed power.max_watts ({})",
                self.power.target_watts,
                self.power.max_watts
            );
        }
        // PSU-override rail sanity gate (fail-closed). `[power.psu_override].voltage_v`
        // is the operator-declared non-smart-PSU OUTPUT rail (APW3/APW7, Loki
        // bypass). It is used for power/efficiency estimation and recorded rail
        // telemetry only — it does NOT drive a hardware SetVoltage — but a value
        // far outside any real Antminer PSU rail (0, negative, or e.g. 128.0) is
        // a typo that would silently mis-estimate power. The hybrid path only
        // soft-warns (11.0-14.5 V) then proceeds; reject the obviously-impossible
        // range here so it fails closed at load. Shipped values are 12.8 / 14.0;
        // the default is 12.0.
        if let Some(ref ovr) = self.power.psu_override {
            if ovr.voltage_v <= 5.0 || ovr.voltage_v > 20.0 {
                anyhow::bail!(
                    "power.psu_override.voltage_v ({:.2}) is outside the sane PSU rail range \
                     (5.0-20.0 V). This is the PSU OUTPUT rail (APW3/APW7 ~12.0-14.0 V), NOT \
                     the ~1.3 V chip voltage. A value this far out is almost always a typo; \
                     set it to the PSU's physically-set output (default 12.0).",
                    ovr.voltage_v
                );
            }
        }
        if let Err(e) = self.autotuner.validate() {
            anyhow::bail!(e);
        }
        // Off-grid voltage threshold validation
        if let Some(ref og) = self.power.offgrid {
            if og.enabled {
                // Resolve thresholds from preset + custom overrides
                let preset = match og.battery_preset.as_str() {
                    "lifepo4_48v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_48V,
                    "lifepo4_24v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_24V,
                    "lifepo4_12v" => dcentrald_thermal::battery::BatteryPreset::LiFePO4_12V,
                    "lead_acid_48v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_48V,
                    "lead_acid_24v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_24V,
                    "lead_acid_12v" => dcentrald_thermal::battery::BatteryPreset::LeadAcid_12V,
                    _ => dcentrald_thermal::battery::BatteryPreset::Custom,
                };
                let mut t = preset.thresholds();
                if let Some(v) = og.custom_critical_v {
                    t.critical_v = v;
                }
                if let Some(v) = og.custom_low_v {
                    t.low_v = v;
                }
                if let Some(v) = og.custom_high_v {
                    t.high_v = v;
                }
                if let Some(v) = og.custom_full_v {
                    t.full_v = v;
                }
                if let Some(v) = og.custom_recovery_v {
                    t.recovery_v = v;
                }

                if t.critical_v >= t.low_v {
                    anyhow::bail!(
                        "offgrid: critical_v ({:.1}) must be less than low_v ({:.1})",
                        t.critical_v,
                        t.low_v
                    );
                }
                if t.low_v >= t.high_v {
                    anyhow::bail!(
                        "offgrid: low_v ({:.1}) must be less than high_v ({:.1})",
                        t.low_v,
                        t.high_v
                    );
                }
                if t.high_v >= t.full_v {
                    anyhow::bail!(
                        "offgrid: high_v ({:.1}) must be less than full_v ({:.1})",
                        t.high_v,
                        t.full_v
                    );
                }
                if t.recovery_v <= t.critical_v {
                    anyhow::bail!("offgrid: recovery_v ({:.1}) must be greater than critical_v ({:.1}) to prevent permanent sleep", t.recovery_v, t.critical_v);
                }
                if t.critical_v < 5.0 {
                    anyhow::bail!("offgrid: critical_v ({:.1}) is dangerously low — no battery can safely discharge below 5V", t.critical_v);
                }
                // loop_interval_ms is fed verbatim into tokio::time::interval(), which
                // PANICS on a zero period — and the release profile is panic=abort, so a
                // hand-edited /data config with loop_interval_ms = 0 (bypassing the REST
                // handler's >= 500 floor) would abort the daemon at startup right after
                // off-grid curtailment is armed. Fail closed at load instead, mirroring the
                // watchdog.kick_interval_s / thermal.pid_interval_s zero-interval gates.
                if og.loop_interval_ms < 500 || og.loop_interval_ms > 600_000 {
                    anyhow::bail!(
                        "power.offgrid.loop_interval_ms ({}) must be 500-600000 ms when off-grid \
                         is enabled — a zero/too-small interval panics tokio::time::interval at \
                         daemon startup (panic=abort aborts the daemon). Matches the REST handler's \
                         500 ms floor. Default is 2000.",
                        og.loop_interval_ms
                    );
                }
            }
        }
        // Fan PWM bounds
        if self.thermal.fan_min_pwm > self.thermal.fan_max_pwm {
            anyhow::bail!(
                "thermal.fan_min_pwm ({}) must be <= thermal.fan_max_pwm ({})",
                self.thermal.fan_min_pwm,
                self.thermal.fan_max_pwm
            );
        }
        if self.thermal.fan_max_pwm > dcentrald_hal::fan::PWM_MAX {
            anyhow::bail!(
                "thermal.fan_max_pwm ({}) must be <= {} (verified fan-control PWM range)",
                self.thermal.fan_max_pwm,
                dcentrald_hal::fan::PWM_MAX
            );
        }
        // Quiet-idle park PWM is only ever driven DOWN. Fail closed if the
        // configured idle duty exceeds the fan ceiling or the absolute home
        // safety max — never let a config raise it. The runtime setter
        // additionally clamps `min(fan_max_pwm).min(PWM_SAFETY_MAX)`; this
        // bail is the fail-closed config-load gate (mirrors the fan_max_pwm
        // clamp above). See
        // .
        if self.thermal.fan_idle_pwm > self.thermal.fan_max_pwm {
            anyhow::bail!(
                "thermal.fan_idle_pwm ({}) must be <= thermal.fan_max_pwm ({})",
                self.thermal.fan_idle_pwm,
                self.thermal.fan_max_pwm
            );
        }
        if self.thermal.fan_idle_pwm > dcentrald_hal::fan::PWM_SAFETY_MAX {
            anyhow::bail!(
                "thermal.fan_idle_pwm ({}) must be <= {} (PWM_SAFETY_MAX — home-mining absolute fan cap)",
                self.thermal.fan_idle_pwm,
                dcentrald_hal::fan::PWM_SAFETY_MAX
            );
        }
        // Night-mode hour-window sanity gate (fail-closed) — only when enabled.
        // `thermal.night_mode.start_hour`/`end_hour` are compared directly
        // against the 0-23 hour-of-day at daemon.rs (`hour >= start_hour ...`).
        // Any value >= 24 can never be matched, silently disabling the home
        // quiet-mode fan/freq cap with no feedback — a regression of the
        // home/night/space-heater quiet-first contract. Reject the impossible
        // hours so a typo fails closed. Shipped night hours are 22/7; the
        // section is disabled by default so a disabled garbage section loads.
        if self.thermal.night_mode.enabled
            && (self.thermal.night_mode.start_hour >= 24 || self.thermal.night_mode.end_hour >= 24)
        {
            anyhow::bail!(
                "thermal.night_mode start_hour ({}) / end_hour ({}) must each be 0-23 (24h clock) \
                 when night_mode is enabled — an out-of-range hour silently disables the night \
                 fan/frequency cap.",
                self.thermal.night_mode.start_hour,
                self.thermal.night_mode.end_hour
            );
        }
        // FWSTAB-1: the night window is compared against LOCAL hour-of-day
        // (UTC + timezone_offset_hours); reject an out-of-range offset so a typo
        // fails closed rather than firing the quiet window at the wrong hour.
        if self.thermal.night_mode.enabled
            && !dcentrald_common::time::is_valid_tz_offset(
                self.thermal.night_mode.timezone_offset_hours,
            )
        {
            anyhow::bail!(
                "thermal.night_mode.timezone_offset_hours ({}) must be in [-12, 14] \
                 (whole-hour UTC offset, e.g. -5 for EST).",
                self.thermal.night_mode.timezone_offset_hours
            );
        }
        // Scheduled-curtailment window sanity gate (fail-closed) — only when
        // enabled. Hours are compared against the 0-23 hour-of-day; any value
        // >= 24 can never match, which would silently make the window
        // inert/ambiguous. Reject impossible hours and a degenerate cadence so
        // a typo fails closed rather than silently disabling demand-response.
        if let Some(curt) = self.power.curtailment.as_ref() {
            if curt.enabled {
                if curt.start_hour >= 24 || curt.end_hour >= 24 {
                    anyhow::bail!(
                        "power.curtailment start_hour ({}) / end_hour ({}) must each be 0-23 \
                         (24h clock) when curtailment is enabled.",
                        curt.start_hour,
                        curt.end_hour
                    );
                }
                if curt.start_hour == curt.end_hour {
                    anyhow::bail!(
                        "power.curtailment start_hour and end_hour are both {} — an empty window \
                         never curtails. Set distinct hours (the window may wrap past midnight, \
                         e.g. start_hour=22, end_hour=6).",
                        curt.start_hour
                    );
                }
                // FWSTAB-1: the window is compared against LOCAL hour-of-day
                // (UTC + timezone_offset_hours); reject an out-of-range offset.
                if !dcentrald_common::time::is_valid_tz_offset(curt.timezone_offset_hours) {
                    anyhow::bail!(
                        "power.curtailment.timezone_offset_hours ({}) must be in [-12, 14] \
                         (whole-hour UTC offset, e.g. -5 for EST).",
                        curt.timezone_offset_hours
                    );
                }
                if curt.poll_interval_s < 5 || curt.poll_interval_s > 3600 {
                    anyhow::bail!(
                        "power.curtailment.poll_interval_s ({}) must be between 5 and 3600 seconds.",
                        curt.poll_interval_s
                    );
                }
            }
        }
        if self.mining.enabled && !self.has_configured_pool() {
            anyhow::bail!(
                "mining.enabled requires both pool.url and pool.worker — configure a pool before enabling mining"
            );
        }
        if self.mining.enabled {
            if let Some(model_name) = self
                .mining
                .model
                .as_deref()
                .and_then(model::td003_management_only_model)
            {
                anyhow::bail!(
                    "mining.enabled is blocked for {model_name}: this platform is an Experimental feature / In development and must boot management-only until its platform promotion gates are complete"
                );
            }
        }
        validate_pool_endpoint_urls(
            "pool",
            &self.pool.url,
            self.pool.sv2_url.as_deref(),
            self.pool.protocol.as_deref(),
        )?;
        if let Some(endpoint) = self.pool.failover1.as_ref() {
            validate_pool_endpoint_urls(
                "pool.failover1",
                &endpoint.url,
                endpoint.sv2_url.as_deref(),
                endpoint.protocol.as_deref(),
            )?;
        }
        if let Some(endpoint) = self.pool.failover2.as_ref() {
            validate_pool_endpoint_urls(
                "pool.failover2",
                &endpoint.url,
                endpoint.sv2_url.as_deref(),
                endpoint.protocol.as_deref(),
            )?;
        }

        // PSF-1/PSF-3 (2026-06-20): the failover list is positional with no
        // compaction (`pool2 <- failover1`, `pool3 <- failover2`). A config with
        // `[pool.failover2]` but no `[pool.failover1]` would deserialize fine, then
        // silently strand the configured failover2 pool — `pool_count()` reports 2
        // (a phantom slot), but round-robin selection only ever yields {0,1} and
        // index 2 is never connected. Reject the gap fail-closed so the operator's
        // explicitly-configured backup is never silently dropped. (weighted_split
        // already rejects failover2 below; this also covers the default `failover`
        // mode.)
        if self.pool.failover2.is_some() && self.pool.failover1.is_none() {
            anyhow::bail!(
                "pool.failover2 is set without pool.failover1 — the failover list must \
                 have no gaps (configure pool.failover1 first, or move this pool to \
                 pool.failover1). The failover2 pool would otherwise never be connected."
            );
        }

        let pool_routing_mode = self.pool.routing_mode.trim();
        if pool_routing_mode != "failover" && pool_routing_mode != "weighted_split" {
            anyhow::bail!(
                "pool.routing_mode ('{}') must be 'failover' or 'weighted_split'",
                self.pool.routing_mode
            );
        }
        if pool_routing_mode == "weighted_split" {
            let Some(secondary) = self.pool.failover1.as_ref() else {
                anyhow::bail!("pool.routing_mode='weighted_split' requires [pool.failover1]");
            };
            if self.pool.failover2.is_some() {
                anyhow::bail!(
                    "pool.routing_mode='weighted_split' currently supports pool.url plus pool.failover1 only"
                );
            }
            if self.pool.url.trim().is_empty()
                || self.pool.worker.trim().is_empty()
                || secondary.url.trim().is_empty()
                || secondary.worker.trim().is_empty()
            {
                anyhow::bail!(
                    "pool.routing_mode='weighted_split' requires both primary and secondary pool url/worker"
                );
            }

            let primary_bps = self.pool.split_bps.unwrap_or(8000);
            let secondary_bps = secondary.split_bps.unwrap_or(2000);
            if primary_bps == 0 || secondary_bps == 0 {
                anyhow::bail!("pool weighted split weights must both be greater than 0 bps");
            }
            if primary_bps as u32 + secondary_bps as u32 != 10_000 {
                anyhow::bail!(
                    "pool weighted split weights must sum to 10000 bps; got {} + {}",
                    primary_bps,
                    secondary_bps
                );
            }
            if self.pool.split_cycle_duration_s < 120 || self.pool.split_cycle_duration_s > 86400 {
                anyhow::bail!(
                    "pool.split_cycle_duration_s ({}) must be between 120 and 86400",
                    self.pool.split_cycle_duration_s
                );
            }
            let min_window_s = (self.pool.split_cycle_duration_s
                * u64::from(primary_bps.min(secondary_bps)))
                / 10_000;
            if min_window_s < 60 {
                anyhow::bail!(
                    "pool weighted split smallest route window is {}s; increase pool.split_cycle_duration_s or route weight so every route is at least 60s",
                    min_window_s
                );
            }

            let split_endpoint_is_v1 =
                |protocol: Option<&String>, sv2_url: &Option<String>| -> bool {
                    let protocol = protocol
                        .map(|value| value.trim().to_ascii_lowercase())
                        .unwrap_or_default();
                    let v1_protocol = protocol.is_empty() || protocol == "sv1" || protocol == "v1";
                    v1_protocol && sv2_url.as_deref().unwrap_or_default().trim().is_empty()
                };
            if !split_endpoint_is_v1(self.pool.protocol.as_ref(), &self.pool.sv2_url)
                || !split_endpoint_is_v1(secondary.protocol.as_ref(), &secondary.sv2_url)
            {
                anyhow::bail!(
                    "pool.routing_mode='weighted_split' is V1-only in this build; remove SV2 URLs and use protocol='sv1' or omit protocol"
                );
            }
        }
        if let Some(pct) = self.pool.reject_rate_failover_pct {
            if pct > 100 {
                anyhow::bail!(
                    "pool.reject_rate_failover_pct ({}) must be between 0 and 100 (0 = disabled)",
                    pct
                );
            }
        }
        if self.psu.transport != "kernel_i2c" && self.psu.transport != "gpio_bitbang" {
            anyhow::bail!(
                "psu.transport ('{}') must be 'kernel_i2c' or 'gpio_bitbang'",
                self.psu.transport
            );
        }
        // PSU rail voltage sanity gate (fail-closed). `psu.voltage_mv` is the
        // upstream PSU OUTPUT rail (APW121215a runs 15200 mV) and is consumed
        // by the am2 hybrid path at `s19j_hybrid_mining.rs` -> the live APW
        // SetVoltage path (`psu.cold_boot_sequence*()`). The HAL clamps the
        // actual write to 11.96-15.20 V, so a gross typo (e.g. a dropped
        // decimal -> 152000, or 1520) would be SILENTLY clamped at write time
        // rather than surfaced. Reject the obviously-impossible range at load
        // so a misconfigured rail fails closed loudly instead. All shipped
        // `[psu].voltage_mv` values are 15200 (inside this band); the serde
        // default is 15200. NOT the per-chain `mining.voltage_mv` (validated
        // above) — this is the PSU rail.
        if self.psu.voltage_mv < 10_000 || self.psu.voltage_mv > 16_000 {
            anyhow::bail!(
                "psu.voltage_mv ({}) is outside the sane PSU rail range (10000-16000 mV). \
                 This is the PSU OUTPUT rail (APW121215a = 15200 mV), NOT the per-chain \
                 mining.voltage_mv. A value this far out is almost always a typo (dropped \
                 or extra decimal); the HAL would silently clamp it to 11.96-15.20 V at \
                 write time. Set psu.voltage_mv to the real rail target (default 15200).",
                self.psu.voltage_mv
            );
        }
        if let Err(e) = self.hashboard.validate() {
            anyhow::bail!(e);
        }
        if let Some(ref solar) = self.power.solar {
            let provider = solar.inverter_brand.trim();
            let support = solar_provider_support(provider);

            if solar.battery_threshold_pct > 100 {
                anyhow::bail!(
                    "power.solar.battery_threshold_pct ({}) must be between 0 and 100",
                    solar.battery_threshold_pct
                );
            }

            if solar.battery_wake_hysteresis_pct > 50 {
                anyhow::bail!(
                    "power.solar.battery_wake_hysteresis_pct ({}) must be between 0 and 50",
                    solar.battery_wake_hysteresis_pct
                );
            }

            if solar.battery_threshold_pct as u16 + solar.battery_wake_hysteresis_pct as u16 > 100 {
                anyhow::bail!(
                    "power.solar battery threshold {}% plus wake hysteresis {}% must stay at or below 100%",
                    solar.battery_threshold_pct,
                    solar.battery_wake_hysteresis_pct
                );
            }

            if solar.provider_max_sample_age_ms > 300_000 {
                anyhow::bail!(
                    "power.solar.provider_max_sample_age_ms ({}) must be 0-300000 ms",
                    solar.provider_max_sample_age_ms
                );
            }

            if solar.provider_failure_hysteresis_samples == 0
                || solar.provider_failure_hysteresis_samples > 10
            {
                anyhow::bail!(
                    "power.solar.provider_failure_hysteresis_samples ({}) must be between 1 and 10",
                    solar.provider_failure_hysteresis_samples
                );
            }

            if solar.hybrid_import_deadband_watts > 5_000 {
                anyhow::bail!(
                    "power.solar.hybrid_import_deadband_watts ({}) must be between 0 and 5000 W",
                    solar.hybrid_import_deadband_watts
                );
            }

            if provider.is_empty() {
                anyhow::bail!("power.solar.inverter_brand cannot be empty");
            }

            if !supported_solar_providers().contains(&provider) {
                anyhow::bail!(
                    "power.solar provider '{}' is not supported; use manual, victron, bridge, ecoflow, enphase, solaredge, or tesla",
                    provider
                );
            }

            if solar.enabled && !support.live_backend {
                let reason = support
                    .stage_reason
                    .as_deref()
                    .unwrap_or("This provider is staged only.");
                anyhow::bail!(
                    "power.solar provider '{}' cannot be enabled for active enforcement yet. {}",
                    provider,
                    reason
                );
            }

            if provider != "manual" && solar.api_endpoint.trim().is_empty() {
                anyhow::bail!(
                    "power.solar.api_endpoint is required for non-manual solar providers"
                );
            }

            if provider == "ecoflow"
                && !(solar.api_endpoint.starts_with("http://")
                    || solar.api_endpoint.starts_with("https://")
                    || solar.api_endpoint.starts_with("mqtt://")
                    || solar.api_endpoint.starts_with("mqtts://")
                    || solar.api_endpoint.starts_with("ws://")
                    || solar.api_endpoint.starts_with("wss://"))
            {
                anyhow::bail!(
                    "power.solar provider 'ecoflow' requires an HTTP(S) or MQTT/WS endpoint that serves one of the supported normalized EcoFlow bridge payload shapes; direct EcoFlow auth/protocol coverage is intentionally out of scope"
                );
            }
        }
        if self.donation.enabled
            && (self.donation.pool_url.trim().is_empty() || self.donation.worker.trim().is_empty())
        {
            anyhow::bail!("donation.enabled requires both donation.pool_url and donation.worker");
        }
        if self.donation.enabled {
            validate_v1_pool_url(&self.donation.pool_url)
                .map_err(|e| anyhow::anyhow!("donation.pool_url is invalid: {}", e))?;
        }
        if self.donation.enabled
            && self.donation.fallback_enabled
            && (self.donation.fallback_pool_url.trim().is_empty()
                || self.donation.fallback_worker.trim().is_empty())
        {
            anyhow::bail!(
                "donation.fallback_enabled requires both donation.fallback_pool_url and donation.fallback_worker"
            );
        }
        if self.donation.enabled && self.donation.fallback_enabled {
            validate_v1_pool_url(&self.donation.fallback_pool_url)
                .map_err(|e| anyhow::anyhow!("donation.fallback_pool_url is invalid: {}", e))?;
        }
        // Frequency sanity bounds (all known Bitmain ASICs operate between 100-1000 MHz)
        if self.mining.frequency_mhz > 0
            && (self.mining.frequency_mhz < 50 || self.mining.frequency_mhz > 1200)
        {
            anyhow::bail!(
                "mining.frequency_mhz ({}) is outside safe range (50-1200 MHz)",
                self.mining.frequency_mhz
            );
        }
        // Voltage sanity bounds (all known hash boards operate between 5000-20000 mV)
        if self.mining.voltage_mv > 0
            && (self.mining.voltage_mv < 5000 || self.mining.voltage_mv > 20000)
        {
            anyhow::bail!(
                "mining.voltage_mv ({}) is outside safe range (5000-20000 mV)",
                self.mining.voltage_mv
            );
        }
        // AM1/S9 PIC16F1704 boards are capped to the 9.40 V chip-rail safety
        // boundary at the HAL write path too. Validate the S9 config loudly so
        // a typo such as 15000 mV is not silently lowered at runtime.
        if self.mining.voltage_mv > 0
            && Self::is_am1_s9_platform()
            && self.mining.voltage_mv > AM1_S9_MAX_CHIP_RAIL_MV
        {
            anyhow::bail!(
                "mining.voltage_mv ({}) exceeds am1-s9 PIC16 chip-rail ceiling {} mV. \
                 S9/BM1387 voltage is commanded through a PIC DAC whose safe maximum is \
                 9.40 V; reduce voltage_mv to <= {} or leave it at 0 for profile auto-detect.",
                self.mining.voltage_mv,
                AM1_S9_MAX_CHIP_RAIL_MV,
                AM1_S9_MAX_CHIP_RAIL_MV
            );
        }
        // HLA-10: the degraded-hashrate alert floor must be a finite, non-negative
        // GH/s value (0 = disabled). A NaN/negative floor would make the
        // sustained-below comparison nonsensical.
        if !self.mining.degraded_hashrate_alert_floor_ghs.is_finite()
            || self.mining.degraded_hashrate_alert_floor_ghs < 0.0
        {
            anyhow::bail!(
                "mining.degraded_hashrate_alert_floor_ghs ({}) must be a finite, non-negative GH/s value (0 = disabled)",
                self.mining.degraded_hashrate_alert_floor_ghs
            );
        }
        // HLA-10 %-form: 0 = disabled, otherwise a percent in (0, 100].
        if !self.mining.degraded_hashrate_alert_pct.is_finite()
            || self.mining.degraded_hashrate_alert_pct < 0.0
            || self.mining.degraded_hashrate_alert_pct > 100.0
        {
            anyhow::bail!(
                "mining.degraded_hashrate_alert_pct ({}) must be a finite value in 0-100 (0 = disabled)",
                self.mining.degraded_hashrate_alert_pct
            );
        }
        // Phase 4C / EE Finding 5 #4 (2026-05-15): am2-class boards (Zynq am2
        // S19 Pro / S19j Pro on BHB42xxx hashboards via APW121215a or
        // Loki-modded APW3 + dsPIC chip-rail regulators) have a hard ceiling
        // at 14_500 mV chip-rail. Above that:
        //   - APW121215a's voltage envelope (15.2 V rail → dsPIC steps down to
        //     ~13.7 V chip-rail) does not safely tolerate >14.5 V chip-rail
        //     for steady-state operation
        //   - The dsPIC fw=0x89 SetVoltage path silently saturates at the
        //     silicon-profile max but the rail can spike during the transient
        //   - The .74 hb2 EEPROM corruption incident on 2026-04-29 was
        //     downstream of an out-of-envelope voltage write
        //
        // This clamp is platform-aware: it fires when /etc/dcentos/board_family
        // identifies "am2" (live miner) OR the active config indicates am2 via
        // mining.am2_pll_ramp / mining.am2_no_nonce_timeout_s defaults being
        // overridden. On non-am2 platforms (S9 am1 at 9.1 V chip-rail, am3-aml
        // / am3-bb / cv1835 at ~13.7 V via different topologies) the existing
        // 5000-20000 mV envelope above still applies.
        if Self::is_am2_platform() && self.mining.voltage_mv > 14_500 {
            anyhow::bail!(
                "mining.voltage_mv ({}) exceeds am2 chip-rail ceiling 14500 mV. \
                 BHB42xxx hashboards via APW121215a/dsPIC regulators are not \
                 specified above 14.5 V chip-rail and exceeding it risks dsPIC \
                 corruption and EEPROM damage (see .74 hb2 incident 2026-04-29). \
                 Reduce voltage_mv to <= 14500. Note: voltage_mv is the per-CHAIN \
                 chip rail the dsPIC regulates to — NOT the PSU rail (which is \
                 set via [psu].voltage_mv at ~15.2 V upstream).",
                self.mining.voltage_mv
            );
        }
        //  (2026-05-22) — CE §5 hardening: clamp `am2_post_eeprom_dspic_grace_ms`
        // at config-load time. The value is used directly in
        // `std::thread::sleep(Duration::from_millis(dspic_boot_grace_ms))` on
        // the hybrid-mining run thread; a typo like `2000000` (= 2000 s) would
        // stall the daemon for half an hour. Upper bound 10_000 ms is the
        // CE-recommended DoS-prevention ceiling — bosminer's implicit minimum
        // on the same hardware is ~48 s (incidental, not protocol-required),
        // and 10 s is a conservative-but-non-blocking ceiling well above the
        // 2 s default and the 4× BraiinsOS `RESET_DELAY` (500 ms × 4 = 2 s)
        // floor cited in the field doc-comment. Value `0` is permitted —
        // disables the grace sleep entirely (legacy timing).
        if self.mining.am2_post_eeprom_dspic_grace_ms > 10_000 {
            anyhow::bail!(
                "mining.am2_post_eeprom_dspic_grace_ms ({}) exceeds the 10_000 ms \
                 (10 s) safety ceiling — values above this stall the hybrid-mining \
                 run thread for an unbounded window and are almost always a typo. \
                 The default is 2000 ms (4× BraiinsOS RESET_DELAY); set to 0 to disable \
                 the grace sleep entirely. See CE review \
                  §5.",
                self.mining.am2_post_eeprom_dspic_grace_ms
            );
        }
        // prod-readiness hunt #2: the two sibling am2 cold-boot timing fields feed
        // verbatim blocking sleeps on the hybrid-mining RUN thread, same footgun
        // class as the grace field above but never bounded. A typo (e.g.
        // am2_post_enable_settle_ms = 4000000 for 4000) stalls bring-up for ~67 min;
        // am2_post_enable_settle_ms runs in Phase 3c AFTER PWR_CONTROL + dsPIC
        // ENABLE, so the stall leaves the boards energized at 13.7 V with the
        // per-work nonce-timeout guard not yet armed. am2_reset_hold_ms holds all
        // four chain RESET lines LOW. Both default to 4000; 0 stays permitted.
        // W1 RE (2026-06-13, strace-backed): the bosminer cold-boot trace holds
        // HB_RESET (gpio897/899) LOW *continuously for ~92 s* before the wake pulse
        // (re018-cold-strace 17:28:50.444 -> 17:30:22.772, zero intervening HIGH
        // writes) — an UNTESTED matrix cell vs DCENT's 4 s default / 10 s ceiling
        // and a candidate class-B reset/settle lever (W1_DEEP_RE_FINDINGS.md #1).
        // Holding RESET LOW = chips held OFF (cut-hash, home-safe), so the long
        // hold is NOT a power/thermal risk; the 10 s ceiling is pure typo-guard.
        // Allow the bosminer-faithful long hold ONLY when the operator explicitly
        // opts in via DCENT_AM2_HB_RESET_LONG_HOLD=1 (so a bare 92000 typo still
        // fails closed). Ceiling 120_000 ms covers the observed ~92 s.
        let am2_reset_hold_long = std::env::var("DCENT_AM2_HB_RESET_LONG_HOLD")
            .map(|v| {
                matches!(
                    v.trim(),
                    "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
                )
            })
            .unwrap_or(false);
        let am2_reset_hold_ceiling_ms: u64 = if am2_reset_hold_long { 120_000 } else { 10_000 };
        if self.mining.am2_reset_hold_ms > am2_reset_hold_ceiling_ms {
            anyhow::bail!(
                "mining.am2_reset_hold_ms ({}) exceeds the {} ms safety ceiling — it \
                 holds all four chain RESET lines LOW for that long on the cold-boot \
                 run thread and is almost always a typo. Default 4000; 0 disables the \
                 extended hold. For the bosminer-faithful ~92 s cold-reset hold (W1 RE \
                 2026-06-13), set DCENT_AM2_HB_RESET_LONG_HOLD=1 to raise the ceiling \
                 to 120_000 ms.",
                self.mining.am2_reset_hold_ms,
                am2_reset_hold_ceiling_ms
            );
        }
        if self.mining.am2_post_enable_settle_ms > 10_000 {
            anyhow::bail!(
                "mining.am2_post_enable_settle_ms ({}) exceeds the 10_000 ms (10 s) \
                 safety ceiling — values above this stall the run thread AFTER the \
                 chip rail is energized (Phase 3c), leaving boards at 13.7 V with no \
                 nonce-timeout feedback for the stall window. Default 4000; 0 to disable.",
                self.mining.am2_post_enable_settle_ms
            );
        }
        // prod-readiness hunt #6: thermal.pid_interval_s sets the thermal PID loop
        // cadence and is fed to tokio::time::interval(Duration::from_secs_f32(..))
        // (daemon.rs). The only runtime guard is `.max(0.5)` which floors negatives
        // and NaN but does NOT bound the upper end: a TOML value that casts to
        // f32::INFINITY (e.g. 1e40) makes Duration::from_secs_f32 PANIC -> on a
        // panic=abort build that aborts the daemon + thermal supervisor with boards
        // powered; a merely-huge finite value (86400) silently defeats thermal
        // cadence. Same fail-closed class as the watchdog/grace gates. Reject
        // non-finite / <=0 / >60 s; all shipped configs use 2.0 or the 5.0 default.
        if !self.thermal.pid_interval_s.is_finite()
            || self.thermal.pid_interval_s <= 0.0
            || self.thermal.pid_interval_s > 60.0
        {
            anyhow::bail!(
                "thermal.pid_interval_s ({}) must be finite and in (0, 60] s — a \
                 non-finite/overflowing value panics the thermal PID loop \
                 (Duration::from_secs_f32) which on a panic=abort build aborts the \
                 daemon with boards powered, and a too-large value defeats thermal \
                 reaction time. Default is 5.0 (the BM1387 time constant is ~2-4 s).",
                self.thermal.pid_interval_s
            );
        }
        // prod-readiness hunt #9: mode.active is a free-form String coerced to
        // Standard by every consumer's `_ =>` arm, so a typo ("hacking", "HACKER",
        // trailing space) silently discards operator intent on the field that gates
        // the quiet/home thermal posture. normalize_legacy_fields() already ran
        // (heater->home), so validate against the known set here, fail-loud like the
        // routing_mode/transport gates. Tolerate the "mining" alias (autotuner
        // synonym for standard) so an existing flashed unit carrying it isn't
        // over-rejected.
        {
            let m = self.mode.active.trim().to_ascii_lowercase();
            if !matches!(m.as_str(), "home" | "standard" | "hacker" | "mining") {
                anyhow::bail!(
                    "mode.active ('{}') is not a known mode — expected one of \
                     home | standard | hacker (alias: mining). An unknown value is \
                     silently coerced to Standard, discarding operator intent on the \
                     field that gates the quiet/home thermal posture.",
                    self.mode.active
                );
            }
        }
        // Watchdog cadence sanity gate (fail-closed). `watchdog.kick_interval_s`
        // is fed verbatim into `tokio::time::interval(Duration::from_secs(...))`
        // on the hardware-watchdog kicker task (daemon.rs). `tokio::time::interval`
        // PANICS when the period is zero — and on a `panic = "abort"` build that
        // takes the whole daemon down right after the HW watchdog has been armed,
        // so the SoC then auto-reboots once `timeout_s` elapses (no kicks ever
        // sent). Reject `kick_interval_s == 0` so the typo fails closed at load
        // instead of crashing a daemon that just armed the watchdog. Only gated
        // when the watchdog is enabled (the default); a disabled watchdog never
        // builds the kicker. All shipped configs use kick_interval_s = 5.
        if self.watchdog.enabled {
            if self.watchdog.kick_interval_s == 0 {
                anyhow::bail!(
                    "watchdog.kick_interval_s must be > 0 when watchdog.enabled is true — \
                     a zero kick interval panics the watchdog kicker task at startup \
                     (tokio::time::interval rejects a zero period), which on a \
                     panic=abort build aborts the daemon right after the hardware \
                     watchdog is armed, leaving the SoC to auto-reboot. Default is 5."
                );
            }
            // The kicker must fire strictly before the hardware timeout, or a
            // perfectly healthy daemon never kicks in time and the SoC reboots
            // anyway. Require a margin: kick_interval_s < timeout_s. Shipped
            // configs are kick=5 / timeout=30.
            if self.watchdog.kick_interval_s >= self.watchdog.timeout_s {
                anyhow::bail!(
                    "watchdog.kick_interval_s ({}) must be less than watchdog.timeout_s ({}) — \
                     a kick interval at or above the hardware timeout means even a healthy \
                     daemon cannot kick before the watchdog fires, so the SoC reboots on a \
                     working unit. Default is kick=5 / timeout=30.",
                    self.watchdog.kick_interval_s,
                    self.watchdog.timeout_s
                );
            }
        }
        Ok(())
    }

    /// Phase 4C / EE Finding 5 #4: detect whether this build is running on
    /// an am2-class board (Zynq am2 S19 Pro / S19j Pro). The voltage clamp
    /// at `validate()` uses this to refuse out-of-envelope chip-rail
    /// targets that would otherwise reach the dsPIC SetVoltage path.
    ///
    /// Detection rules, in order:
    ///   1. `/etc/dcentos/board_family` reads "am2" — authoritative on a
    ///      Buildroot-baked image. Stamped by `am2-s19jpro` post-build.sh.
    ///   2. `/etc/dcentos/platform` starts with "zynq-bm3-am2" — secondary
    ///      authority same source.
    ///
    /// When neither file exists (Windows dev host, unit tests without a
    /// fixture, runtime-only `/tmp` deploy on top of BraiinsOS before the
    /// am2-s19jpro overlay is flashed), this returns `false` so the broader
    /// 5000-20000 mV envelope is the only gate. The runtime-only XIL path
    /// is still safe because the operator-supplied config sets
    /// voltage_mv=13700 and the hybrid path's dsPIC service will refuse a
    /// SetVoltage above the silicon-profile max regardless.
    ///
    /// A second unconditional rail-program boundary cap also exists in
    /// `dcentrald-asic/src/dspic/mod.rs` via
    /// `clamp_dspic_voltage_to_hard_cap()` (`DSPIC_VOLTAGE_HARD_CAP_MV =
    /// 14_500`), including runtime `/tmp` deploys. This config check is the
    /// earlier fail-loud layer, not the only safety boundary.
    ///
    /// For unit tests, override via the `DCENT_FORCE_AM2_VOLTAGE_CLAMP=1`
    /// environment variable.
    fn is_am2_platform() -> bool {
        if std::env::var("DCENT_FORCE_AM2_VOLTAGE_CLAMP").as_deref() == Ok("1") {
            return true;
        }
        if let Ok(family) = std::fs::read_to_string("/etc/dcentos/board_family") {
            if family.trim().eq_ignore_ascii_case("am2") {
                return true;
            }
        }
        if let Ok(platform) = std::fs::read_to_string("/etc/dcentos/platform") {
            if platform.trim().starts_with("zynq-bm3-am2") {
                return true;
            }
        }
        false
    }

    /// Detect the AM1/S9 image so config validation can reject impossible
    /// PIC16/BM1387 chip-rail targets before the HAL clamps them at write time.
    ///
    /// For unit tests, override via `DCENT_FORCE_AM1_S9_VOLTAGE_CLAMP=1`.
    fn is_am1_s9_platform() -> bool {
        if std::env::var("DCENT_FORCE_AM1_S9_VOLTAGE_CLAMP").as_deref() == Ok("1") {
            return true;
        }
        if let Ok(target) = std::fs::read_to_string("/etc/dcentos/board_target") {
            if target.trim().eq_ignore_ascii_case("am1-s9") {
                return true;
            }
        }
        if let Ok(platform) = std::fs::read_to_string("/etc/dcentos/platform") {
            let platform = platform.trim().to_ascii_lowercase();
            if platform == "am1-s9" || platform.contains("bm1-s9") {
                return true;
            }
        }
        false
    }

    /// Check if a configuration file exists at the given path.
    pub fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }
}

fn validate_pool_endpoint_urls(
    label: &str,
    url: &str,
    sv2_url: Option<&str>,
    protocol: Option<&str>,
) -> Result<()> {
    let has_url = !url.trim().is_empty();
    let sv2_url = sv2_url.filter(|value| !value.trim().is_empty());

    if has_url {
        if endpoint_uses_sv2_primary(protocol, sv2_url) {
            validate_sv2_pool_url(url)
                .map_err(|e| anyhow::anyhow!("{}.url is invalid: {}", label, e))?;
        } else {
            validate_v1_pool_url(url)
                .map_err(|e| anyhow::anyhow!("{}.url is invalid: {}", label, e))?;
        }
    }

    if let Some(sv2_url) = sv2_url {
        validate_sv2_pool_url(sv2_url)
            .map_err(|e| anyhow::anyhow!("{}.sv2_url is invalid: {}", label, e))?;
    }

    Ok(())
}

fn endpoint_uses_sv2_primary(protocol: Option<&str>, sv2_url: Option<&str>) -> bool {
    let protocol = protocol
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(protocol.as_str(), "sv2" | "v2") && sv2_url.is_none()
}

// ---------------------------------------------------------------------------
// General settings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneralConfig {
    /// Miner hostname for network identification.
    #[serde(default = "default_hostname")]
    pub hostname: String,

    /// Log level: trace, debug, info, warn, error.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Config-schema version marker (SW-13). The REST read-modify-write path
    /// (`rest::migrate_config_schema`) stamps `[general].schema_version` into
    /// `/data/dcentrald.toml` for drift detection / future migrations. Because
    /// every struct in this file is `deny_unknown_fields`, the STRICT loader
    /// (`DcentraldConfig::load`) reads the SAME file and must tolerate the
    /// marker — without this field a stamped config would be rejected and the
    /// daemon would silently fall back to management-only (mining disabled),
    /// discarding the operator's config. `default` accepts an un-stamped
    /// (pre-SW-13) config; `skip_serializing` keeps the daemon's own struct
    /// writes from emitting a possibly-stale value (the REST path owns the marker).
    #[serde(default, skip_serializing)]
    pub schema_version: Option<i64>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            hostname: default_hostname(),
            log_level: default_log_level(),
            schema_version: None,
        }
    }
}

fn default_hostname() -> String {
    "dcentos-miner".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

// ---------------------------------------------------------------------------
// Logging configuration (W1.4 — wallet-mask + log-tail sanitization)
// ---------------------------------------------------------------------------

/// Wallet-address masking + log-tail sanitization controls.
///
/// Per-call `mask_wallet()` substitutions on `worker=` / `username=` /
/// `wallet=` fields are performed unconditionally at log-emission time
/// inside the Stratum V1 + V2 clients. They cannot be disabled — see the
/// hard rule in `dcentrald-common/src/wallet_mask.rs` (TRACE-level can
/// still inspect raw wires when `RUST_LOG=trace` is set).
///
/// `mask_logs` controls the *passthrough* sanitizer applied to the
/// `/api/debug/log` REST endpoint. With `mask_logs = true` (the default),
/// every line returned to the dashboard is scanned with
/// `mask_in_string()` and any wallet-shaped substring (e.g. left-over from
/// a third-party library log line that didn't go through our masked
/// fields) is replaced before serialization.
///
/// Operators with structured-log collectors (Loki, Splunk, Promtail) that
/// need raw addresses can set `mask_logs = false` — but the per-call
/// substitutions above still apply, so accepting full addresses requires
/// `RUST_LOG=trace` plus the correct collector pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// W1.4: mask wallet-shaped substrings in the log-tail passthrough.
    /// Default `true`. Setting this to `false` disables the passthrough
    /// sanitizer ONLY — per-call masking inside the Stratum clients is
    /// still active.
    #[serde(default = "default_mask_logs")]
    pub mask_logs: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            mask_logs: default_mask_logs(),
        }
    }
}

fn default_mask_logs() -> bool {
    // W1.4 HARD RULE: default MUST be true. Privacy is the default;
    // operators must opt OUT explicitly (with full understanding that the
    // log-tail will then leak wallet addresses to the dashboard / API).
    true
}

// ---------------------------------------------------------------------------
// Pool configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Primary pool URL (e.g., "stratum+tcp://pool.example.com:3333").
    #[serde(default)]
    pub url: String,

    /// Worker name sent to pool.
    #[serde(default = "default_worker")]
    pub worker: String,

    /// Pool password (usually "x").
    #[serde(default = "default_pool_password")]
    pub password: String,

    /// SV2 endpoint URL (e.g., "stratum2+tcp://v2.braiins.com:3336").
    /// When set with protocol="auto", the router will prefer SV2.
    #[serde(default)]
    pub sv2_url: Option<String>,

    /// Protocol selection: "sv1"/"v1" (default), "sv2"/"v2", or "auto".
    /// When absent, defaults to V1 for backward compatibility.
    #[serde(default)]
    pub protocol: Option<String>,

    /// User-pool routing mode: "failover" (default) or "weighted_split".
    #[serde(default = "default_pool_routing_mode")]
    pub routing_mode: String,

    /// Weighted split cycle duration in seconds.
    #[serde(default = "default_pool_split_cycle_duration_s")]
    pub split_cycle_duration_s: u64,

    /// Primary route split weight in basis points when weighted_split is active.
    #[serde(default)]
    pub split_bps: Option<u16>,

    /// Legacy dashboard-written priority key. Older API builds wrote this into
    /// `[pool]` even though runtime ordering is defined by table position.
    #[serde(default, skip_serializing)]
    pub priority: Option<u8>,

    /// First failover pool.
    #[serde(default)]
    pub failover1: Option<PoolEndpoint>,

    /// Second failover pool.
    #[serde(default)]
    pub failover2: Option<PoolEndpoint>,

    /// **Default false.** Opt-in toggle for the LuxOS-shape SmartSwitch
    /// pool-failover FSM (RE-006, `dcentrald_stratum::pool_failover`).
    /// Threads into `StratumConfig::smart_failover_enabled` at every
    /// stratum-client construction site. With this false (the shipped
    /// default) the existing user-pool failover machinery is the sole
    /// driver of pool selection and runtime behavior is byte-identical to
    /// the pre-toggle daemon. With this ON the FSM runs in shadow (logs the
    /// decision it WOULD make); it only actually *drives* pool selection when
    /// this is ON AND a drive arm (`[pool].smart_failover_drive` or the
    /// `DCENT_POOL_FAILOVER_FSM_DRIVE` env gate) is also set — see
    /// `smart_failover_drive`. Named `[pool].smart_failover_enabled` (the
    /// operator failover knobs all live in `[pool]`, alongside
    /// `routing_mode`/`failover1`/`failover2`).
    #[serde(default)]
    pub smart_failover_enabled: bool,

    /// **Default false.** SW-01 drive arm (config form). When this is `true`
    /// AND `smart_failover_enabled` is `true`, the SmartSwitch FSM DRIVES live
    /// pool selection (its recommended pool index is applied to the V1
    /// client's `current_pool_index`) instead of only logging in shadow. The
    /// `dcentrald.toml` equivalent of the `DCENT_POOL_FAILOVER_FSM_DRIVE` env
    /// gate; either arm enables drive. Threads into
    /// `StratumConfig::smart_failover_drive`. NO hardware / voltage /
    /// frequency / fan path — drive only changes which already-configured pool
    /// the client connects to. Promoting drive to a fleet default is gated on
    /// an operator soak, not host tests alone.
    #[serde(default)]
    pub smart_failover_drive: bool,

    /// Stable-primary-return anti-flap cool-down (seconds). After failover
    /// off the primary, the primary is only re-preferred once this cool-down
    /// has fully elapsed AND the active backup itself faults. `0` = disabled
    /// (legacy round-robin). Unset = the shipped default (900).
    /// Armada 2026-06-09: previously hardcoded in `build_stratum_config`;
    /// now operator-settable (unset configs are byte-identical).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_return_stability_secs: Option<u64>,

    /// No-`mining.notify` failover timeout (seconds). Post-handshake, no new
    /// job for this long fails the session into the existing failover
    /// machinery. `0` = disabled. Unset = the shipped default (300), well
    /// beyond normal notify/vardiff cadence (no false failover on a
    /// quiet-but-healthy pool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_notify_failover_secs: Option<u64>,

    /// Reject-rate failover threshold (percent, 0-100). HIGHEST flap risk
    /// (vardiff transitions / transient pool blips spike rejects) — OPT-IN:
    /// unset or `0` = DISABLED (the shipped default). Acts only over at
    /// least `reject_rate_failover_min_samples` post-handshake shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_rate_failover_pct: Option<u8>,

    /// Minimum post-handshake shares before reject-rate failover may act.
    /// Unset = the shipped default (100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject_rate_failover_min_samples: Option<u64>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            worker: default_worker(),
            password: default_pool_password(),
            sv2_url: None,
            protocol: None,
            routing_mode: default_pool_routing_mode(),
            split_cycle_duration_s: default_pool_split_cycle_duration_s(),
            split_bps: None,
            priority: None,
            failover1: None,
            failover2: None,
            smart_failover_enabled: false,
            smart_failover_drive: false,
            primary_return_stability_secs: None,
            no_notify_failover_secs: None,
            reject_rate_failover_pct: None,
            reject_rate_failover_min_samples: None,
        }
    }
}

/// A single pool endpoint (used for failover pools).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolEndpoint {
    pub url: String,
    #[serde(default = "default_worker")]
    pub worker: String,
    #[serde(default = "default_pool_password")]
    pub password: String,
    #[serde(default)]
    pub sv2_url: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub split_bps: Option<u16>,
    #[serde(default, skip_serializing)]
    pub priority: Option<u8>,
}

fn default_worker() -> String {
    String::new()
}

fn default_pool_routing_mode() -> String {
    "failover".to_string()
}

fn default_pool_split_cycle_duration_s() -> u64 {
    1800
}

fn default_false() -> bool {
    false
}

fn default_pool_password() -> String {
    "x".to_string()
}

// ---------------------------------------------------------------------------
// Mining configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiningConfig {
    /// Whether hashboard mining should start automatically.
    ///
    /// Clean production images default to false so DCENT_OS can cold-boot into
    /// its own dashboard and service stack without surprise mining.
    #[serde(default = "default_false")]
    pub enabled: bool,

    /// Target ASIC frequency in MHz.
    #[serde(default = "default_frequency")]
    pub frequency_mhz: u16,

    /// Target chain voltage in millivolts (PIC value).
    #[serde(default = "default_voltage")]
    pub voltage_mv: u16,

    /// Suggested difficulty to send to pool via mining.suggest_difficulty.
    /// Default 8192 keeps bring-up and dashboard use calmer than the old 256 hint.
    #[serde(default = "default_suggest_difficulty")]
    pub suggest_difficulty: u64,

    /// Passthrough mode: skip ASIC init and use inherited ASIC/FPGA state.
    ///
    /// When true: assumes a compatible runtime already configured ASICs. Skips
    /// PLL, MiscCtrl, baud upgrade, open-core, and full enumeration.
    ///
    /// When false (default): full cold boot init sequence. Resets hash boards,
    /// configures ASICs from scratch, and runs open-core activation. This is the
    /// intended clean-image DCENT_OS runtime path.
    #[serde(default = "default_false")]
    pub passthrough: bool,

    /// Enable ASICBoost version rolling (overt, BIP 310).
    /// Requested mask is `version_rolling_mask`; the pool may negotiate a
    /// narrower mask. FPGA uses distinct midstates per work item.
    #[serde(default = "default_true")]
    pub version_rolling: bool,

    /// Requested BIP310/SV2 version-rolling mask.
    #[serde(default = "default_version_rolling_mask")]
    pub version_rolling_mask: u32,

    /// Degraded-hashrate alert floor in GH/s (HLA-10 v1, DETECTION-ONLY).
    ///
    /// When `> 0` AND mining has produced hashrate at least once, a SUSTAINED
    /// total hashrate below this floor — but above the idle epsilon, i.e. a true
    /// degradation rather than a full stop (which `MiningStopped` already covers)
    /// — fires a `HashrateDegraded` webhook/browser alert via the existing
    /// `MiningAlertMonitor` (same 30-tick confirm + 15-min repeat-suppress).
    ///
    /// `0.0` = DISABLED (the shipped default — zero behavior change). This is
    /// purely an ALERT: it NEVER throttles, reboots, cuts power, or touches any
    /// hardware path. The bounded auto-recovery (restart/reboot) ladder that
    /// VNish/LuxOS/stock ship is an operator-gated follow-up, deliberately NOT
    /// in this detection-only increment.
    #[serde(default)]
    pub degraded_hashrate_alert_floor_ghs: f64,

    /// Degraded-hashrate alert threshold as a PERCENT of the rated nominal
    /// hashrate (HLA-10 %-form — the VNish/LuxOS-matching form, since the
    /// operator need not know their exact GH/s). `0.0` = DISABLED (default).
    ///
    /// When `> 0` it takes PRECEDENCE over `degraded_hashrate_alert_floor_ghs`:
    /// the effective alert floor becomes `pct/100 * rated_nominal_ghs` (the
    /// daemon resolves nominal from the detected `MinerProfile` + live chip
    /// count at the publisher). Same detection-only semantics: alert only,
    /// never touches hardware. Example: `80.0` alerts when the miner sustains
    /// below 80% of its rated nameplate hashrate.
    #[serde(default)]
    pub degraded_hashrate_alert_pct: f64,

    /// Reserved gate for the future mining-pipeline-owned snapshot publisher.
    ///
    /// The runtime publisher is not wired in this build. Config validation
    /// rejects `enabled = true` so operators cannot accidentally imply live
    /// job/nonce/drop telemetry before S9/S19 Pro/S21 smoke validation.
    #[serde(default)]
    pub pipeline_snapshot: MiningPipelineSnapshotConfig,

    /// PH-3 (/12): operator-gated, DEFAULT-OFF hashrate recovery
    /// ladder. Layers on the SAME resolved degraded floor (never a new
    /// threshold). Automatic process replacement is currently suspended for
    /// every platform until typed hardware-disposition receipts exist; the
    /// historical allowlist remains data for that future resolver. On AM2-Zynq/Amlogic/BB a restart leaves the
    /// chip un-enumerable, so the ladder degrades to alert-only there. The pure
    /// FSM + every safety gate live in `dcentrald_api_types::hashrate_recovery`.
    #[serde(default)]
    pub recovery_ladder: dcentrald_api_types::hashrate_recovery::HashrateRecoveryLadderConfig,

    /// Serial device path for UART-based ASIC communication (e.g., "/dev/ttyS2").
    /// When set, enables the serial mining path (bypasses FPGA work FIFOs).
    #[serde(default)]
    pub serial_device: Option<String>,

    /// Ordered serial device paths for future multi-chain UART mining.
    ///
    /// Phase 1 parses and validates the list but the S19j hybrid runtime still
    /// selects the first entry. Keep `serial_device` as the legacy single-chain
    /// fallback until the dispatcher is fully vectorized.
    #[serde(default)]
    pub serial_devices: Option<Vec<String>>,

    /// Number of ASIC chips on the serial chain.
    #[serde(default)]
    pub serial_chip_count: Option<u8>,

    /// Optional minimum fraction of the expected chips that must enumerate on
    /// a chain before Phase 7 is allowed to mine that chain.
    ///
    /// `None` keeps historical behavior. `Some(0.0)` is also permissive and
    /// preserves partial-enum bring-up recipes such as the proven `a lab unit` 28/126
    /// path; higher values are an operator/fleet hardening knob.
    #[serde(default)]
    pub min_chip_fraction: Option<f32>,

    /// `a lab unit`-class XIL safety rollout switch for HAL-2 mixed-chip refusal.
    ///
    /// Non-`a lab unit` platforms refuse divergent production chip IDs by default.
    /// `a lab unit` stays log-only unless this explicit operator/fleet gate is set,
    /// preserving the proven fingerprint while the refusal path is bench-soaked.
    #[serde(default)]
    pub enforce_mixed_chip_id_refusal_on_xil25: bool,

    /// ASIC chip type on the serial chain (e.g., "BM1362").
    #[serde(default)]
    pub serial_chip_type: Option<String>,

    /// FPGA chain physical base address for /dev/mem access (e.g., "0x43C10000").
    /// Used by S19j hybrid mode when UIO devices are not available for a chain.
    #[serde(default)]
    pub fpga_chain_base: Option<String>,

    /// FPGA chain ID number (e.g., 2 for chain 2 on S19j Pro).
    #[serde(default)]
    pub fpga_chain_id: Option<u8>,

    /// Miner model hint for forcing chip-family selection (bypasses auto-detection).
    /// Common values include sibling aliases like `t9`, `s19j`, `s19jpro`,
    /// `s19kpro`, `t21`, and `s21+`. Aliases are normalized centrally.
    /// When set, loads the corresponding MinerProfile at daemon startup.
    /// Required for passthrough mode on non-S9 models (chip enumeration is skipped).
    #[serde(default)]
    pub model: Option<String>,

    /// Diagnostic: skip WORK TX/RX FIFO reset during hot start.
    /// When true + passthrough, FIFOs are NOT reset after bosminer handoff.
    /// This allows reading residual nonces from bosminer's pipeline to verify
    /// that WORK_RX is physically connected to hash boards.
    /// Default false (normal operation resets FIFOs for clean start).
    #[serde(default)]
    pub skip_fifo_reset: bool,

    /// Diagnostic: skip board temperature reads via BM1387 I2C passthrough.
    /// Prevents MiscCtrl corruption at the cost of no board temp data.
    /// Fans stay at boot PWM (10-30). XADC die temp still available.
    #[serde(default)]
    pub skip_board_temp: bool,

    /// Diagnostic: enable AM2/BM1362 first-work timeline sampling.
    /// This emits dense FPGA/glitch-monitor snapshots around the first work
    /// writes and is intentionally opt-in because it adds sleeps and log volume.
    #[serde(default)]
    pub am2_first_work_timeline: bool,

    /// **W13.B1 (2026-05-10) RECLASSIFIED + RENAMED.**
    ///
    /// Diagnostic-only force-write of the Braiins glitch-monitor mirror
    /// registers (`0x43D00030` / `0x43D00034`). These FPGA registers are
    /// read-only mirrors of BM1362 UART_RELAY candidate state. They exist
    /// ONLY in the Braiins-am2 custom bitstream — stock CV1835 / AM335x /
    /// Amlogic / S9 hardware has zero response there.
    ///
    /// Phase 9A live tests proved the mirror writes are silently rejected
    /// from userspace. R6-7 keeps BM1362 reg `0x2C`/`0x34` candidate relay
    /// broadcasts disabled by default behind `DCENT_BM1362_ENABLE_UART_RELAY_LAB`
    /// until live captures confirm exact control semantics.
    ///
    /// This flag remains for telemetry-parity with bosminer (lab-only).
    /// Default `false` per W13.B1 — was `true` before reclassification.
    /// Backwards-compat with existing TOML configs preserved via
    /// `#[serde(alias = "am2_force_uart_relay_init")]`.
    #[serde(default, alias = "am2_force_uart_relay_init")]
    pub am2_force_braiins_glitch_mirror_write: bool,

    /// Phase 2b: HBx_RESET pulse LOW hold duration in ms (default 4000ms; S9 pattern uses 4000ms).
    /// Per  S9 cold-boot agent: BM1362 may need a longer reset hold to flush
    /// previous-firmware state cleanly, mirroring the BraiinsOS BM1387 `init_and_split`
    /// pattern that holds chains in UART BREAK reset for ~4 seconds before any UART
    /// traffic. Set to 4000 (default) for S9-pattern parity; set to 20 to revert to
    /// the historical short pulse if the longer hold breaks something on a given unit.
    #[serde(default = "default_am2_reset_hold_ms")]
    pub am2_reset_hold_ms: u64,

    /// Phase 3c: post-ENABLE settle window before chain UART probe (default 4000ms; S9 pattern).
    /// Per  S9 cold-boot agent: BM1362 PLL + clock distribution may need more
    /// time to stabilise at 13.7V than the legacy 1.2 s window allowed. S9 BraiinsOS
    /// waits ~4 s after PIC voltage enable before any chain UART traffic. Set to 1200
    /// to revert to the legacy short settle if the longer wait causes regressions.
    #[serde(default = "default_am2_post_enable_settle_ms")]
    pub am2_post_enable_settle_ms: u64,

    /// Stage the BM1362 PLL ramp instead of slamming the target frequency in
    /// two writes.
    ///
    ///  BM1368-vs-BM1362 comparison agent identified BM1362's direct
    /// PLL slam (default ~50 MHz → 525 MHz in two writes with 10 ms spacing,
    /// no lock-check) as the most likely root cause of `a lab unit`'s chain UART
    /// silence — even at 13.7 V rail engaged. BM1368 (working on `a lab unit`)
    /// ramps PLL `200 → 525 MHz` in 25 MHz steps × 100 ms settle. With this
    /// flag ON, BM1362 replicates the BM1368 cadence (`bm1362::pll_ramp_sequence`
    /// → `[400, 425, 450, 475, 500, 525] MHz`, 100 ms settle per step,
    /// best-effort PLL-lock readback after each step).
    ///
    /// Default ON. Belt-and-suspenders rollback: set `false` to fall back to
    /// the legacy two-write slam if the ramp introduces a regression on a
    /// known-good unit.
    #[serde(default = "default_true")]
    pub am2_pll_ramp: bool,

    /// AM2 home/lab fail-closed guard. After the first FPGA work dispatch,
    /// force PWR_CONTROL low and leave the mining loop if no nonce arrives
    /// within this many seconds. Set to `0` to disable for deep lab captures.
    #[serde(default = "default_am2_no_nonce_timeout_s")]
    pub am2_no_nonce_timeout_s: u64,

    /// Phase 0p (2026-05-22 XIL `a lab unit` recovery, Layer 3) — post-EEPROM dsPIC
    /// firmware-boot grace window in milliseconds.
    ///
    /// After EEPROM 0x50 first-ACKs in Phase 0a (proving the hashboard 3.3 V
    /// manageability rail is up), wait this many ms before any dsPIC opcode.
    /// Bosminer's implicit minimum on the `a lab unit`/`a lab unit` boot timeline is ~48 s
    /// (EEPROM at T+10 → first PIC touch at T+58); the dsPIC firmware-boot +
    /// I²C MSSP ISR registration takes a few hundred ms by datasheet. Default
    /// 2000 = 4× BraiinsOS `RESET_DELAY` (500 ms × 4). Set to 0 to disable.
    ///
    /// See
    /// .
    #[serde(default = "default_am2_post_eeprom_dspic_grace_ms")]
    pub am2_post_eeprom_dspic_grace_ms: u64,

    /// Phase 0d (2026-05-22 XIL `a lab unit` recovery, Layer 1+3) — run the
    /// bosminer-faithful PIC reset+start-app warmup BEFORE the first
    /// GET_VERSION.
    ///
    /// When `true` the daemon emits the bosminer-canonical chain
    /// (16-byte parser flush + `[55 AA 07]` + 500 ms + `[55 AA 06]` + 100 ms)
    /// through the safe-by-construction `dspic::bosminer_warmup` wrapper just
    /// before Phase 1's `pic_read_fw_version_service`. The wrapper always
    /// includes the parser flush — the `a lab unit` 2026-04-24 bare-RESET-without-flush
    /// corruption pattern is structurally impossible here. Default `true`;
    /// set `false` to keep the legacy "race straight to GET_VERSION" behaviour
    /// for known-good warm-boot units.
    ///
    /// An additional opt-in env gate `DCENT_AM2_PIC_RESET_AND_START_APP=1`
    /// must ALSO be set for the wrapper to actually emit bytes on the first
    /// A/B run. The env-gate exists so the very first deploy stays
    /// byte-identical to today's behaviour on `a lab unit` — the operator flips
    /// the env on `a lab unit` for the validation run; promote to default after
    /// A/B success.
    ///
    /// See
    /// .
    #[serde(default = "default_true")]
    pub am2_dspic_warmup_before_get_version: bool,

    /// Phase 0b move (2026-05-22 XIL `a lab unit` recovery, Layer 3) — run the fan
    /// autoconfig + RPM gate BEFORE the first dsPIC GET_VERSION, not after
    /// Phase 2b.
    ///
    /// Bosminer-faithful ordering: fans-OK at T+22 well before first PIC
    /// opcode at T+58. The current Phase 2c-pre location (after PIC
    /// GET_VERSION + Phase 2b HBx_RESET) is reverse vs bosminer and means
    /// the C49→C52 `board-control` mode WRITE (inside
    /// `FanController::open_with_variant`) happens AFTER PIC probe. Default
    /// `true`; set `false` to revert to the historical order.
    ///
    /// See
    ///  §(b).
    #[serde(default = "default_true")]
    pub am2_fan_gate_before_pic: bool,

    ///  W1 — divisor for the work-table stale-age eviction
    /// threshold in `WorkDispatcher::tick()`.
    ///
    /// The dispatcher evicts a work-table entry when
    /// `dispatch_generation - entry.generation >= work_id_space / N`,
    /// where `N` is this divisor. Default `4` → threshold is one-quarter
    /// of the chip's work-id space (= 64 cycles for the BM1387 8-bit
    /// ring).
    ///
    ///  capture on `.39` (8484 rejects, all `hash_above_target`
    /// with hashes uniformly random across the byte range, generation_age
    /// median 101 of max 256) showed that the legacy `>= work_id_space`
    /// guard (= 256) is too loose for the 8-bit BM1387 ring: aged work
    /// entries get overwritten by newer dispatches before nonces from
    /// the older generation arrive, so those nonces validate against
    /// the WRONG midstate → hash > target → local reject.
    ///
    /// Tightening to threshold = 64 (divisor 4) trades false-stale
    /// rejection of nonces that were going to fail validation anyway
    /// for accepted shares from the ones that would have validated
    /// correctly without the aliasing.
    ///
    /// Operators can override to `1` (= legacy behavior, threshold =
    /// work_id_space) in `/data/dcentrald.toml` if a regression appears.
    #[serde(default = "default_stale_age_divisor")]
    pub stale_age_divisor: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MiningPipelineSnapshotConfig {
    /// Default-off bounded publisher gate. When enabled, the daemon publishes
    /// a read-only watch snapshot from session mining_sync events.
    pub enabled: bool,
    /// Freshness threshold for a published snapshot.
    pub stale_after_ms: u64,
}

impl Default for MiningPipelineSnapshotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            stale_after_ms: 5_000,
        }
    }
}

impl MiningConfig {
    /// Convert model string to chip_id for MinerProfile lookup.
    pub fn model_chip_id(&self) -> Option<u16> {
        self.model.as_deref().and_then(model::model_chip_id)
    }

    pub fn resolved_serial_devices(&self, default_device: &str) -> Vec<String> {
        if let Some(devices) = self
            .serial_devices
            .as_ref()
            .filter(|devices| !devices.is_empty())
        {
            return devices.clone();
        }

        self.serial_device
            .clone()
            .map(|device| vec![device])
            .unwrap_or_else(|| vec![default_device.to_string()])
    }

    fn validate_serial_devices(&self) -> Result<()> {
        let Some(devices) = self.serial_devices.as_ref() else {
            return Ok(());
        };

        if devices.is_empty() {
            anyhow::bail!(
                "mining.serial_devices cannot be empty; remove it or list at least one tty path"
            );
        }

        for (idx, device) in devices.iter().enumerate() {
            if device.trim().is_empty() {
                anyhow::bail!("mining.serial_devices[{}] cannot be empty", idx);
            }
            if !device.starts_with("/dev/ttyS") {
                anyhow::bail!(
                    "mining.serial_devices[{}] ('{}') must be a /dev/ttyS* chain UART path",
                    idx,
                    device
                );
            }
            if devices[..idx]
                .iter()
                .any(|prior| prior.trim() == device.trim())
            {
                anyhow::bail!(
                    "mining.serial_devices contains duplicate path '{}'",
                    device.trim()
                );
            }
        }

        Ok(())
    }
}

impl Default for MiningConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            frequency_mhz: default_frequency(),
            voltage_mv: default_voltage(),
            suggest_difficulty: default_suggest_difficulty(),
            passthrough: false,
            // P0 FIX (2026-03-25, DCENT_Swarm): Default to true — ASICBoost provides
            // ~20-25% more effective hashrate. Pools that don't support it gracefully
            // degrade (mining.configure is optional). Default false cost ~$49/yr per miner.
            version_rolling: true,
            version_rolling_mask: default_version_rolling_mask(),
            degraded_hashrate_alert_floor_ghs: 0.0,
            degraded_hashrate_alert_pct: 0.0,
            pipeline_snapshot: MiningPipelineSnapshotConfig::default(),
            recovery_ladder:
                dcentrald_api_types::hashrate_recovery::HashrateRecoveryLadderConfig::default(),
            serial_device: None,
            serial_devices: None,
            serial_chip_count: None,
            min_chip_fraction: None,
            enforce_mixed_chip_id_refusal_on_xil25: false,
            serial_chip_type: None,
            fpga_chain_base: None,
            fpga_chain_id: None,
            model: None,
            skip_fifo_reset: false,
            skip_board_temp: false,
            am2_first_work_timeline: false,
            // W13.B1 (2026-05-10): default `false` per reclassification.
            // The FPGA `0x43D000xx` mirror is NOT a control surface; R6-7
            // keeps BM1362 0x2C/0x34 candidate relay broadcasts lab-gated.
            // The legacy field name + default were misleading; opt-in only
            // for lab telemetry parity.
            am2_force_braiins_glitch_mirror_write: false,
            am2_reset_hold_ms: default_am2_reset_hold_ms(),
            am2_post_enable_settle_ms: default_am2_post_enable_settle_ms(),
            // Default ON: ramp BM1362 PLL in 25 MHz steps with 100 ms settle
            // per step (BM1368 .135 cadence).  hypothesis for `a lab unit`
            // chain UART silence at engaged rail.
            am2_pll_ramp: true,
            am2_no_nonce_timeout_s: default_am2_no_nonce_timeout_s(),
            // 2026-05-22 XIL `a lab unit` dsPIC recovery defaults (consolidated fix).
            am2_post_eeprom_dspic_grace_ms: default_am2_post_eeprom_dspic_grace_ms(),
            am2_dspic_warmup_before_get_version: true,
            am2_fan_gate_before_pic: true,
            //  W1 default — threshold = work_id_space / 4.
            stale_age_divisor: default_stale_age_divisor(),
        }
    }
}

///  W1 default stale-age divisor — `4` → threshold is one-
/// quarter of the chip's work-id space. For BM1387/BM1397/BM1398
/// (8-bit ring, `work_id_space = 256`) the effective threshold is 64
/// cycles. For BM1362/BM1366/BM1368/BM1370 (16-bit ring,
/// `work_id_space = 16384`) it's 4096 cycles — still 16× more than the
/// chip's typical pipeline depth.
fn default_stale_age_divisor() -> u32 {
    4
}

fn default_am2_no_nonce_timeout_s() -> u64 {
    90
}

/// Phase 2b reset hold default — 4 s, matching S9 BraiinsOS `init_and_split`.
fn default_am2_reset_hold_ms() -> u64 {
    4000
}

/// Phase 3c post-ENABLE settle default — 4 s, matching the S9 cold-boot pattern.
fn default_am2_post_enable_settle_ms() -> u64 {
    4000
}

/// Phase 0p post-EEPROM dsPIC firmware-boot grace default — 2 s.
///
///:
/// 4× BraiinsOS `RESET_DELAY` (500 ms × 4 = 2 s) is the minimum to let the
/// dsPIC complete firmware boot + I²C MSSP slave ISR registration. Bosminer's
/// implicit minimum on the same hardware is ~48 s (incidental from its
/// serial fan-autoconfig + PSU service tasks), so 2 s is a conservative-but-
/// non-blocking lower bound.
fn default_am2_post_eeprom_dspic_grace_ms() -> u64 {
    2000
}

fn default_suggest_difficulty() -> u64 {
    8192
}

fn default_version_rolling_mask() -> u32 {
    0x1fff_e000
}

fn default_frequency() -> u16 {
    650
}

fn default_voltage() -> u16 {
    // FIX (2026-04-13, swarm #2): 0 = auto-detect from MinerProfile.
    // Was 8600 (S9-specific). Daemon voltage reduce step now checks for 0
    // and uses MinerProfile.default_voltage_mv instead.
    0
}

// ---------------------------------------------------------------------------
// Power configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerConfig {
    /// Target power consumption in watts. 0 = no limit (full speed).
    #[serde(default)]
    pub target_watts: u32,

    /// Skip PSU I2C validation for 120V operation.
    #[serde(default)]
    pub psu_bypass: bool,

    /// Legacy persisted configs used `mode = "bypass"` before `psu_bypass`
    /// became the explicit field. Keep accepting that field for older flashed
    /// units and shipped templates, then normalize it into `psu_bypass`.
    #[serde(default, rename = "mode", skip_serializing)]
    legacy_mode: Option<String>,

    /// Absolute maximum power consumption (safety limit).
    #[serde(default = "default_max_watts")]
    pub max_watts: u32,

    /// Circuit capacity in watts (configured during first-boot setup).
    /// Default: 1800 (120V × 15A). Used for circuit headroom display.
    #[serde(default = "default_circuit_capacity")]
    pub circuit_capacity_watts: u32,

    /// Declared circuit voltage in volts for AC-backed commissioning.
    #[serde(default)]
    pub circuit_voltage_v: Option<u16>,

    /// Declared circuit amperage in amps for AC-backed commissioning.
    #[serde(default)]
    pub circuit_amperage_a: Option<u16>,

    /// Commissioning source profile selected during onboarding.
    #[serde(default)]
    pub source_profile: Option<String>,

    /// PSU override: bypass auto-detection for non-smart PSUs (APW3, APW7).
    /// When enabled, dcentrald skips I2C PSU probing and uses the user-specified
    /// fixed voltage. Eliminates the need for a Pivotal Pleb Tech Loki device
    /// when running S19-S21 miners on APW3/APW7 PSUs.
    #[serde(default)]
    pub psu_override: Option<PsuOverride>,

    /// Off-grid / Direct DC power configuration.
    /// When enabled, monitors DC bus voltage and auto-adjusts mining frequency
    /// to match available power (battery, solar, generator, capacitor).
    #[serde(default)]
    pub offgrid: Option<OffGridConfig>,

    /// Solar / hybrid integration configuration.
    #[serde(default)]
    pub solar: Option<SolarConfig>,

    /// Scheduled (time-of-day) curtailment for off-peak / demand-response /
    /// quiet-night operation. When `None` (the default — no `[power.curtailment]`
    /// section in TOML) the daemon never constructs the schedule driver and the
    /// shared `CurtailmentController` is left entirely to the off-grid / solar /
    /// API owners, so the runtime path is byte-identical to today.
    #[serde(default)]
    pub curtailment: Option<CurtailmentScheduleConfig>,

    /// Optional wall-meter correction for estimate-only power paths.
    #[serde(default)]
    pub calibration: Option<dcentrald_autotuner::PowerCalibration>,
}

/// PSU override configuration for fixed-voltage PSUs (APW3, APW7).
///
/// These PSUs have a physically-set voltage (adjustable via potentiometer)
/// with no I2C/PMBus communication. DCENT_OS uses the declared voltage
/// for power estimation instead of reading from the PSU.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PsuOverride {
    /// Enable the override (false = auto-detect via I2C).
    #[serde(default)]
    pub enabled: bool,

    /// PSU model name: "APW3", "APW7", or custom string.
    #[serde(default = "default_psu_model")]
    pub model: String,

    /// Fixed output voltage in volts (12.0-13.5V typical range).
    /// This is what the PSU is physically set to deliver.
    #[serde(default = "default_psu_voltage")]
    pub voltage_v: f64,

    ///  (2026-05-22) — EE-LOKI-001 defense-in-depth hint: when
    /// `Some(true)`, the am2 hybrid path's Phase 0c smart-APW12 lenient
    /// probe at `s19j_hybrid_mining.rs:4581-4639` is HARD-SKIPPED. Use this
    /// on bare-APW3 fleet units (no Loki spoof daughter-board on i2c-0@0x10)
    /// to close the phantom-device-on-0x10 SMBus hazard described in EE
    /// review
    /// §5 (T3 threat). Default `None` = existing lenient-probe behaviour,
    /// byte-identical wire/log shape to today.
    ///
    /// Wire-serialization: `#[serde(skip_serializing_if = "Option::is_none")]`
    /// preserves byte-identical TOML for existing units that omit the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_smbus_peer: Option<bool>,

    ///  (2026-05-22) — operator-declared PSU hardware variant for
    /// dashboard display + future telemetry. Common values: `"loki"`
    /// (APW3 + Loki spoof board on i2c-0), `"bare-apw3"` (APW3 with NO
    /// Loki, the bare-modded fleet build-out target), `"stock-apw12"`
    /// (genuine APW121215a / APW12+; rarely used with override branch).
    /// Logged at daemon startup; surfaced in `/api/config/psu-override`
    /// and `/api/status` for dashboard display. NOT consumed by any
    /// mining decision path — operator metadata only.
    ///
    /// Default `None` = unset; byte-identical TOML for existing units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psu_hardware_variant: Option<String>,
}

fn default_psu_model() -> String {
    "APW7".to_string()
}

fn default_psu_voltage() -> f64 {
    12.0
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write, build_stratum_config, stratum_donation_config, CurtailmentScheduleConfig,
        DcentraldConfig, MiningConfig, PowerConfig, WatchdogConfig, MAX_PERSISTED_CONFIG_BYTES,
    };
    use dcentrald_api::NetworkBlockConfig;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn config_toml_parse_and_validate_never_panics_on_arbitrary_text(
            text in ".{0,4096}"
        ) {
            if let Ok(mut cfg) = toml::from_str::<DcentraldConfig>(&text) {
                let _ = cfg.normalize_legacy_fields();
                let _ = cfg.validate();
            }
        }
    }

    const FROZEN_BETA_CONFIG_FIXTURES: &[(&str, &str)] = &[
        (
            "frozen_beta_legacy_heater",
            include_str!("../tests/fixtures/config_migration/frozen_beta_legacy_heater.toml"),
        ),
        (
            "frozen_beta_legacy_serial",
            include_str!("../tests/fixtures/config_migration/frozen_beta_legacy_serial.toml"),
        ),
    ];

    fn load_migrated_fixture(name: &str, toml_text: &str) -> DcentraldConfig {
        let mut config: DcentraldConfig = toml::from_str(toml_text)
            .unwrap_or_else(|err| panic!("{name} fixture must deserialize: {err}"));
        config
            .normalize_legacy_fields()
            .unwrap_or_else(|err| panic!("{name} fixture must normalize: {err}"));
        config
            .validate()
            .unwrap_or_else(|err| panic!("{name} fixture must validate at HEAD: {err}"));
        config
    }

    #[test]
    fn frozen_beta_config_fixtures_load_normalize_and_validate() {
        for (name, toml_text) in FROZEN_BETA_CONFIG_FIXTURES {
            let config = load_migrated_fixture(name, toml_text);
            assert!(
                config.has_configured_pool(),
                "{name} fixture must keep its configured pool"
            );
        }
    }

    #[test]
    fn frozen_beta_config_fixture_migrations_preserve_current_fields() {
        let heater = load_migrated_fixture(
            "frozen_beta_legacy_heater",
            FROZEN_BETA_CONFIG_FIXTURES[0].1,
        );
        assert_eq!(heater.mode.active, "home");
        assert_eq!(heater.mode.home.target_watts, 800);
        assert!(heater.mode.home.night_mode.enabled);
        assert_eq!(heater.mode.home.night_mode.power_reduction_pct, 50);
        assert!(heater.power.psu_bypass);

        let serial = load_migrated_fixture(
            "frozen_beta_legacy_serial",
            FROZEN_BETA_CONFIG_FIXTURES[1].1,
        );
        assert_eq!(
            serial.mining.resolved_serial_devices("/dev/ttyS2"),
            vec!["/dev/ttyS1".to_string()]
        );
        assert!(serial.watchdog.enabled);
    }

    fn mining_config_for_model(model: &str, enabled: bool) -> DcentraldConfig {
        let mut cfg = DcentraldConfig::management_only_default();
        cfg.pool.url = "stratum+tcp://pool.example.com:3333".to_string();
        cfg.pool.worker = "worker.1".to_string();
        cfg.mining.enabled = enabled;
        cfg.mining.model = Some(model.to_string());
        cfg
    }

    /// gap-swarm daemon-startup #1/#9: the no-brick fallback config must be
    /// constructible AND fail-closed. Pins that (a) the empty-document parse the
    /// fallback relies on is infallible (every DcentraldConfig field is
    /// `#[serde(default)]`) — so a future field dropping the attr fails CI here
    /// instead of the `management_only_default()` expect() firing on a real unit —
    /// and (b) the resulting config never starts mining (management-only).
    #[test]
    fn management_only_default_is_fail_closed() {
        assert!(
            toml::from_str::<DcentraldConfig>("").is_ok(),
            "empty TOML must parse to all-serde-defaults (the no-brick fallback basis)"
        );
        let cfg = DcentraldConfig::management_only_default();
        assert!(
            !cfg.mining.enabled,
            "management-only default must have mining disabled"
        );
        assert!(
            !cfg.mining_start_enabled(),
            "management-only default must NOT start mining (no PSU/chain energize)"
        );
    }

    #[test]
    fn min_chip_fraction_defaults_absent_for_partial_enum_compat() {
        let cfg = MiningConfig::default();
        assert!(
            cfg.min_chip_fraction.is_none(),
            "min_chip_fraction must default absent so existing partial-enum recipes are byte-identical"
        );
        assert!(
            !cfg.enforce_mixed_chip_id_refusal_on_xil25,
            ".25 mixed-chip refusal must be default-off until operator bench soak"
        );

        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://pool.example.com:3333"
worker = "worker.1"

[mining]
model = "s19jpro"
"#,
        )
        .expect("config omitting min_chip_fraction must deserialize");
        assert!(
            config.mining.min_chip_fraction.is_none(),
            "omitted min_chip_fraction must stay None"
        );
    }

    #[test]
    fn min_chip_fraction_validates_operator_floor_bounds() {
        for value in ["0.0", "0.5", "1.0"] {
            let mut cfg = DcentraldConfig::management_only_default();
            cfg.mining.min_chip_fraction = Some(value.parse::<f32>().unwrap());
            cfg.validate()
                .unwrap_or_else(|err| panic!("min_chip_fraction={value} must validate: {err}"));
        }

        for value in [-0.01_f32, 1.01_f32, f32::INFINITY, f32::NAN] {
            let mut cfg = DcentraldConfig::management_only_default();
            cfg.mining.min_chip_fraction = Some(value);
            let err = cfg
                .validate()
                .expect_err("invalid min_chip_fraction must fail closed")
                .to_string();
            assert!(
                err.contains("mining.min_chip_fraction"),
                "error must name the invalid key: {err}"
            );
        }
    }

    #[test]
    fn td003_models_validate_when_management_only() {
        for model in [
            "s15", "t15", "s17", "s17pro", "s17+", "t17", "t17+", "t19", "s19xp",
        ] {
            let cfg = mining_config_for_model(model, false);
            cfg.validate().unwrap_or_else(|err| {
                panic!("{model} management-only config must validate: {err}")
            });
            assert!(
                !cfg.mining_start_enabled(),
                "{model} must remain management-only with mining disabled"
            );
        }
    }

    #[test]
    fn td003_models_reject_mining_enabled_until_promoted() {
        for model in [
            "s15", "t15", "s17", "s17pro", "s17+", "t17", "t17+", "t19", "s19xp",
        ] {
            let cfg = mining_config_for_model(model, true);
            let err = cfg
                .validate()
                .expect_err("TD-003 model must not validate with mining enabled by default")
                .to_string();
            assert!(
                err.contains("Experimental feature / In development"),
                "{model} error must use product-grade tier copy: {err}"
            );
            assert!(
                err.contains("management-only"),
                "{model} error must instruct management-only boot: {err}"
            );
        }
    }

    #[test]
    fn td003_gate_does_not_block_proven_or_existing_model_configs() {
        for model in ["s9", "s19jproam2", "s19jpro", "s19pro", "s21", "s19k"] {
            let cfg = mining_config_for_model(model, true);
            cfg.validate()
                .unwrap_or_else(|err| panic!("{model} must not be blocked by TD-003 gate: {err}"));
            assert!(
                cfg.mining_start_enabled(),
                "{model} mining start flag should keep existing semantics"
            );
        }
    }

    /// W-notify: the webhook `format` + Telegram fields are additive and
    /// default-OFF. A legacy `[webhook]` (enabled/url/events only — predating
    /// these fields) MUST still parse despite `deny_unknown_fields`, defaulting
    /// to `Generic` with no Telegram credentials (byte-identical delivery), and
    /// an omitted `[webhook]` stays `None`.
    #[test]
    fn webhook_config_round_trip_default_off_and_back_compat() {
        use super::WebhookConfig;
        use dcentrald_api::webhook::WebhookFormat;

        // Legacy section with none of the new fields — must parse (defaults).
        let legacy: WebhookConfig = toml::from_str("enabled = false\nurl = \"\"\nevents = []\n")
            .expect("legacy webhook config must still parse");
        assert!(!legacy.enabled);
        assert_eq!(legacy.format, WebhookFormat::Generic);
        assert_eq!(legacy.telegram_bot_token, None);
        assert_eq!(legacy.telegram_chat_id, None);

        // The shipped default is byte-identical Generic delivery, default-OFF.
        let def = WebhookConfig::default();
        assert!(!def.enabled);
        assert_eq!(def.format, WebhookFormat::Generic);
        assert!(def.telegram_bot_token.is_none());

        // A Telegram config round-trips through TOML.
        let tg_src = "enabled = true\nformat = \"telegram\"\ntelegram_bot_token = \"TOK\"\ntelegram_chat_id = \"42\"\n";
        let tg: WebhookConfig = toml::from_str(tg_src).expect("telegram webhook config parses");
        assert_eq!(tg.format, WebhookFormat::Telegram);
        assert_eq!(tg.telegram_bot_token.as_deref(), Some("TOK"));
        assert_eq!(tg.telegram_chat_id.as_deref(), Some("42"));
        let reser = toml::to_string(&tg).expect("re-serialize");
        let tg2: WebhookConfig = toml::from_str(&reser).expect("re-parse");
        assert_eq!(tg2.format, WebhookFormat::Telegram);

        // An omitted [webhook] table leaves the daemon config field None.
        let whole: DcentraldConfig = toml::from_str("").expect("empty parses");
        assert!(whole.webhook.is_none());
    }

    /// WAVE 0 STABILIZE (2026-06-05) — Task 3: the daemon watchdog MUST default
    /// to enabled on production so a wedged/hung daemon self-recovers (the HW
    /// watchdog resets the SoC when kicks stop), pairing with the fixed restart
    /// path. Pin the default-on invariant on BOTH default surfaces (the serde
    /// `default_true` used for an empty/omitted `[watchdog]` section AND the
    /// `impl Default`), plus the management-only fallback config — a future
    /// edit that flips either back to `false` for production fails CI here.
    #[test]
    fn watchdog_defaults_enabled_on_production() {
        // impl Default surface.
        assert!(
            WatchdogConfig::default().enabled,
            "WatchdogConfig::default() must have the watchdog enabled"
        );
        // serde surface: a config with NO [watchdog] section must still arm it.
        let no_watchdog_section = toml::from_str::<DcentraldConfig>("")
            .expect("empty TOML must parse to all-serde-defaults");
        assert!(
            no_watchdog_section.watchdog.enabled,
            "a config that omits [watchdog] must default the watchdog ENABLED"
        );
        // An empty inline [watchdog] table (present but no `enabled` key) must
        // also default enabled via the field-level serde default.
        let empty_watchdog_table = toml::from_str::<DcentraldConfig>("[watchdog]\n")
            .expect("an empty [watchdog] table must parse");
        assert!(
            empty_watchdog_table.watchdog.enabled,
            "an empty [watchdog] table must still default the watchdog ENABLED"
        );
        // The no-brick management-only fallback must keep the watchdog armed
        // even though mining is disabled (a hung management-only daemon still
        // needs the SoC reset path).
        assert!(
            DcentraldConfig::management_only_default().watchdog.enabled,
            "management-only fallback must keep the watchdog ENABLED"
        );
        // An operator can still explicitly opt out.
        let opted_out = toml::from_str::<DcentraldConfig>("[watchdog]\nenabled = false\n")
            .expect("explicit opt-out must parse");
        assert!(
            !opted_out.watchdog.enabled,
            "explicit [watchdog] enabled = false must be honored"
        );
    }

    /// WAVE 0 STABILIZE (2026-06-05) — Task 2: `Config::save` must be atomic so
    /// a crash/power loss mid-write cannot corrupt `/data/dcentrald.toml`. Prove
    /// the atomic-write helper (a) produces a byte-complete, re-loadable file,
    /// and (b) leaves NO `.tmp` sibling behind after a successful write.
    #[test]
    fn config_save_is_atomic_and_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "dcentrald_atomic_save_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("dcentrald.toml");
        let path_str = path.to_str().expect("utf8 temp path");

        let cfg = DcentraldConfig::management_only_default();
        cfg.save(path_str).expect("atomic save must succeed");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("saved config metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "new configuration files must be private by default"
            );
        }

        // The file exists, is non-empty, and re-loads to a valid config.
        let reloaded = DcentraldConfig::load(path_str)
            .expect("the atomically-written config must re-load and validate");
        assert!(
            !reloaded.mining.enabled,
            "round-tripped management-only config must keep mining disabled"
        );

        // No leftover temp sibling after a clean write.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read temp dir")
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.ends_with(".tmp"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "atomic_write must not leave a .tmp sibling: {leftovers:?}"
        );

        // A second save (overwrite) must also be atomic and leave the file
        // valid (the rename-over-existing path).
        cfg.save(path_str).expect("second atomic save must succeed");
        assert!(
            DcentraldConfig::load(path_str).is_ok(),
            "config must remain valid after an in-place atomic overwrite"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_atomic_write_rejects_oversize_without_replacing_target() {
        let dir = std::env::temp_dir().join(format!(
            "dcentrald_config_bound_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("dcentrald.toml");
        std::fs::write(&path, b"old = true\n").expect("seed old config");

        let error = atomic_write(&path, &vec![b'x'; MAX_PERSISTED_CONFIG_BYTES + 1])
            .expect_err("oversize config must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(&path).unwrap(), b"old = true\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn config_atomic_write_refuses_symlink_without_mutating_referent() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "dcentrald_config_symlink_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let referent = dir.join("referent.toml");
        let target = dir.join("dcentrald.toml");
        std::fs::write(&referent, b"old = true\n").expect("seed referent");
        symlink(&referent, &target).expect("create config symlink");

        atomic_write(&target, b"new = true\n").expect_err("symlink target must be rejected");
        assert_eq!(std::fs::read(&referent).unwrap(), b"old = true\n");
        assert!(std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SW-13 (W3 SW-13-CRIT-1 regression): the REST migration path stamps
    /// `[general].schema_version` into `/data/dcentrald.toml`, which the STRICT
    /// `deny_unknown_fields` loader reads on next start. Without a tolerant
    /// `schema_version` field on `GeneralConfig` the load would FAIL on the
    /// unknown key and the daemon would silently fall back to management-only
    /// (mining disabled), discarding the operator's whole config — the exact
    /// failure SW-13 was meant to prevent. Pin that a stamped config loads AND
    /// the operator's real settings survive the round-trip.
    #[test]
    fn stamped_schema_version_is_accepted_by_strict_loader() {
        let stamped = r#"
[general]
hostname = "rig-7"
schema_version = 1

[mining]
enabled = true
"#;
        let cfg = toml::from_str::<DcentraldConfig>(stamped)
            .expect("a config carrying [general].schema_version MUST parse (no field loss)");
        assert_eq!(
            cfg.general.hostname, "rig-7",
            "operator hostname must survive"
        );
        assert_eq!(
            cfg.general.schema_version,
            Some(1),
            "the stamped marker must round-trip in"
        );
        assert!(
            cfg.mining.enabled,
            "operator's mining setting must NOT be discarded"
        );
    }

    /// SAFETY (wave 8, 2026-04-28): The HW watchdog must default to ENABLED so a
    /// panicked or hung daemon resets the SoC instead of leaving the chains
    /// powered with no thermal supervision (documented thermal-runaway path).
    /// Per-board operators may still opt out by explicitly setting
    /// `[watchdog] enabled = false`.
    #[test]
    fn default_watchdog_enabled() {
        assert!(
            WatchdogConfig::default().enabled,
            "WatchdogConfig::default().enabled must be true — disabling it leaves \
             a panicked daemon with no thermal supervision"
        );
    }

    /// Same regression, but exercised through serde — confirms a TOML that omits
    /// the `[watchdog]` section gets the safe default.
    #[test]
    fn missing_watchdog_section_defaults_enabled() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"
"#,
        )
        .expect("config without [watchdog] should deserialize");

        assert!(
            config.watchdog.enabled,
            "missing [watchdog] section must yield enabled=true via serde default"
        );
    }

    #[test]
    fn build_stratum_config_preserves_failover_pools() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "primary.worker"
password = "x"
routing_mode = "weighted_split"
split_bps = 7000

[pool.failover1]
url = "stratum+tcp://backup1.example.com:3333"
worker = "backup1.worker"
password = "y"
split_bps = 3000

[mining]
version_rolling = true
suggest_difficulty = 2048
"#,
        )
        .expect("failover pool config should deserialize");

        let stratum_config = build_stratum_config(
            &config,
            stratum_donation_config(&config.donation),
            config.mining.version_rolling,
            false,
        );

        assert_eq!(
            stratum_config.pool1.url,
            "stratum+tcp://primary.example.com:3333"
        );
        assert_eq!(stratum_config.pool1.split_bps, Some(7000));
        let pool2 = stratum_config
            .pool2
            .as_ref()
            .expect("failover1 must reach StratumConfig.pool2");
        assert_eq!(pool2.url, "stratum+tcp://backup1.example.com:3333");
        assert_eq!(pool2.worker, "backup1.worker");
        assert_eq!(pool2.password, "y");
        assert_eq!(pool2.split_bps, Some(3000));
        assert!(stratum_config.pool3.is_none());
        assert_eq!(stratum_config.routing_mode, "weighted_split");
        assert_eq!(stratum_config.suggest_difficulty, Some(2048));
        assert!(stratum_config.version_rolling);
    }

    /// Armada 2026-06-09: the four failover-robustness knobs were hardcoded
    /// in build_stratum_config (SW-7 verification finding). Pin BOTH halves
    /// of the plumb-through: unset configs keep the previously-hardcoded
    /// shipping defaults byte-identical, and explicitly-set [pool] keys
    /// reach StratumConfig.
    #[test]
    fn pool_failover_knobs_default_to_prior_hardcoded_values() {
        let config: DcentraldConfig = toml::from_str("").expect("empty config should deserialize");
        let stratum_config = build_stratum_config(
            &config,
            stratum_donation_config(&config.donation),
            config.mining.version_rolling,
            false,
        );
        assert_eq!(stratum_config.primary_return_stability_secs, 900);
        assert_eq!(stratum_config.no_notify_failover_secs, 300);
        assert_eq!(stratum_config.reject_rate_failover_pct, 0);
        assert_eq!(stratum_config.reject_rate_failover_min_samples, 100);
    }

    #[test]
    fn pool_failover_knobs_are_operator_settable_from_toml() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "primary.worker"
primary_return_stability_secs = 600
no_notify_failover_secs = 240
reject_rate_failover_pct = 25
reject_rate_failover_min_samples = 200
"#,
        )
        .expect("failover-knob config should deserialize");
        let stratum_config = build_stratum_config(
            &config,
            stratum_donation_config(&config.donation),
            config.mining.version_rolling,
            false,
        );
        assert_eq!(stratum_config.primary_return_stability_secs, 600);
        assert_eq!(stratum_config.no_notify_failover_secs, 240);
        assert_eq!(stratum_config.reject_rate_failover_pct, 25);
        assert_eq!(stratum_config.reject_rate_failover_min_samples, 200);
    }

    #[test]
    fn reject_rate_failover_pct_over_100_fails_validation() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "w"
reject_rate_failover_pct = 101
"#,
        )
        .expect("config should deserialize before validation");
        let err = config
            .validate()
            .expect_err("pct > 100 must fail validation");
        assert!(
            err.to_string().contains("reject_rate_failover_pct"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn api_http_bind_defaults_to_existing_lan_visible_bind() {
        let config =
            toml::from_str::<DcentraldConfig>("").expect("empty TOML must parse to defaults");
        assert_eq!(config.api.http_bind, "0.0.0.0");
        assert!(!config.api.websocket_tickets);
        config.validate().expect("default http_bind must validate");
    }

    #[test]
    fn api_http_bind_rejects_non_ip_literal() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[api]
http_bind = "miner.local"
"#,
        )
        .expect("config should deserialize before validation");
        let err = config
            .validate()
            .expect_err("non-IP http_bind must fail validation");
        assert!(
            err.to_string().contains("api.http_bind"),
            "unexpected error: {err}"
        );
    }

    /// LANE R (PH-3 wiring): the hashrate recovery ladder is SAFETY-CRITICAL
    /// (its restart request is currently refused at the shared policy boundary)
    /// and MUST ship DEFAULT-OFF so the
    /// daemon's default behavior is byte-identical to a build without it. Pin the
    /// off-by-default invariant on every config surface — a future edit that flips
    /// the default ON fails CI here instead of silently arming auto-restart on a
    /// fresh unit.
    #[test]
    fn recovery_ladder_defaults_off() {
        // serde surface: a config that omits [mining.recovery_ladder] must leave
        // the ladder disabled.
        let no_section = toml::from_str::<DcentraldConfig>("")
            .expect("empty TOML must parse to all-serde-defaults");
        assert!(
            !no_section.mining.recovery_ladder.enabled,
            "a config that omits [mining.recovery_ladder] must default the ladder OFF"
        );
        // impl Default surface.
        assert!(
            !MiningConfig::default().recovery_ladder.enabled,
            "MiningConfig::default() must have the recovery ladder OFF"
        );
        // The no-brick management-only fallback must also keep it OFF.
        assert!(
            !DcentraldConfig::management_only_default()
                .mining
                .recovery_ladder
                .enabled,
            "management-only fallback must keep the recovery ladder OFF"
        );
    }

    /// LANE R (PH-3 wiring): when an operator ENABLES the ladder, its bounds are
    /// validated through `DcentraldConfig::validate()` (config.rs:440 wires
    /// `recovery_ladder.validate()` into the top-level validate). A nonsensical
    /// enabled config (max_attempts = 0 ⇒ an unbounded restart loop) must fail
    /// closed at load rather than arm a thrashing recovery loop on a real unit.
    #[test]
    fn recovery_ladder_enabled_with_bad_bounds_fails_validation() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "w"

[mining.recovery_ladder]
enabled = true
max_attempts = 0
"#,
        )
        .expect("config should deserialize before validation");
        let err = config
            .validate()
            .expect_err("enabled ladder with max_attempts = 0 must fail validation");
        assert!(
            err.to_string().contains("recovery_ladder.max_attempts"),
            "unexpected error: {err}"
        );
    }

    /// LANE R (PH-3 wiring): a DISABLED ladder must NEVER block boot, even with
    /// nonsensical bounds left in the file — the validate() gate is zero-cost when
    /// the ladder is off (mirrors the FSM's own `validate()` contract). Guards
    /// against a future change that validates the ladder unconditionally and
    /// bricks boot on a stale/garbage disabled section.
    #[test]
    fn recovery_ladder_disabled_with_bad_bounds_does_not_block_boot() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "w"

[mining.recovery_ladder]
enabled = false
max_attempts = 0
cooldown_s = 0
startup_grace_s = 0
"#,
        )
        .expect("config should deserialize before validation");
        config
            .validate()
            .expect("a disabled recovery ladder must never block boot");
    }

    /// PSF-3 (2026-06-20): the failover list is positional with no compaction,
    /// so `[pool.failover2]` without `[pool.failover1]` would silently strand the
    /// configured failover2 pool (phantom pool_count, never connected). validate()
    /// must reject the gap fail-closed.
    #[test]
    fn failover2_without_failover1_fails_validation() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "w"

[pool.failover2]
url = "stratum+tcp://backup2.example.com:3333"
worker = "w2"
"#,
        )
        .expect("config should deserialize before validation");
        let err = config
            .validate()
            .expect_err("failover2 without failover1 must fail validation");
        assert!(
            err.to_string().contains("pool.failover2")
                && err.to_string().contains("pool.failover1"),
            "unexpected error: {err}"
        );
    }

    /// Positive control: a contiguous failover list (failover1 present, with or
    /// without failover2) must still validate cleanly — the PSF-3 guard only
    /// rejects the gap, never a well-formed list.
    #[test]
    fn contiguous_failover_list_passes_validation() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://primary.example.com:3333"
worker = "w"

[pool.failover1]
url = "stratum+tcp://backup1.example.com:3333"
worker = "w1"

[pool.failover2]
url = "stratum+tcp://backup2.example.com:3333"
worker = "w2"
"#,
        )
        .expect("config should deserialize before validation");
        config
            .validate()
            .expect("a contiguous failover1+failover2 list must validate");
    }

    #[test]
    fn legacy_power_mode_bypass_maps_to_psu_bypass() {
        let mut config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[power]
mode = "bypass"
"#,
        )
        .expect("legacy power.mode should deserialize");

        config
            .normalize_legacy_fields()
            .expect("legacy power.mode should normalize");

        assert!(config.power.psu_bypass);
    }

    #[test]
    fn unknown_legacy_power_mode_is_rejected() {
        let mut config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[power]
mode = "mystery"
"#,
        )
        .expect("legacy power.mode field should deserialize before normalization");

        let err = config
            .normalize_legacy_fields()
            .expect_err("unknown legacy power.mode must fail loudly");

        assert!(err.to_string().contains("power.mode"));
    }

    // ---- Scheduled curtailment (Group B wiring) ------------------------------

    #[test]
    fn no_curtailment_section_is_none_and_inert() {
        // GROUP B SAFETY: absence of [power.curtailment] must leave the field
        // None so the daemon never spawns the schedule driver — byte-identical
        // to a build without the feature.
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"
"#,
        )
        .expect("config without curtailment section should deserialize");
        assert!(
            config.power.curtailment.is_none(),
            "absent [power.curtailment] must be None (inert / byte-identical)"
        );
        // And the default-constructed PowerConfig also leaves it None.
        assert!(PowerConfig::default().curtailment.is_none());
    }

    #[test]
    fn curtailment_window_predicate_is_pure_and_safe() {
        // Simple (non-wrapping) window 09..17.
        assert!(!CurtailmentScheduleConfig::window_active(9, 17, 8));
        assert!(CurtailmentScheduleConfig::window_active(9, 17, 9));
        assert!(CurtailmentScheduleConfig::window_active(9, 17, 16));
        assert!(!CurtailmentScheduleConfig::window_active(9, 17, 17)); // end exclusive
        assert!(!CurtailmentScheduleConfig::window_active(9, 17, 23));

        // Midnight-wrap window 22..06.
        assert!(CurtailmentScheduleConfig::window_active(22, 6, 22));
        assert!(CurtailmentScheduleConfig::window_active(22, 6, 23));
        assert!(CurtailmentScheduleConfig::window_active(22, 6, 0));
        assert!(CurtailmentScheduleConfig::window_active(22, 6, 5));
        assert!(!CurtailmentScheduleConfig::window_active(22, 6, 6)); // end exclusive
        assert!(!CurtailmentScheduleConfig::window_active(22, 6, 12));

        // Degenerate window never curtails (can't strand the miner asleep).
        for h in 0u8..24 {
            assert!(
                !CurtailmentScheduleConfig::window_active(3, 3, h),
                "empty window (start==end) must never be active, hour {h}"
            );
        }
    }

    #[test]
    fn enabled_curtailment_parses_and_validates() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[power.curtailment]
enabled = true
start_hour = 22
end_hour = 6
poll_interval_s = 60
"#,
        )
        .expect("enabled curtailment section should deserialize");
        let curt = config
            .power
            .curtailment
            .as_ref()
            .expect("curtailment section present");
        assert!(curt.enabled);
        assert_eq!(curt.start_hour, 22);
        assert_eq!(curt.end_hour, 6);
        assert_eq!(curt.poll_interval_s, 60);
        config
            .validate()
            .expect("valid curtailment window should pass validation");
    }

    #[test]
    fn curtailment_rejects_out_of_range_hours() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[power.curtailment]
enabled = true
start_hour = 25
end_hour = 6
"#,
        )
        .expect("deserialize (validation happens in validate())");
        let err = config.validate().expect_err("hour >= 24 must fail closed");
        assert!(err.to_string().contains("power.curtailment"));
    }

    #[test]
    fn curtailment_rejects_empty_window() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[power.curtailment]
enabled = true
start_hour = 3
end_hour = 3
"#,
        )
        .expect("deserialize");
        let err = config
            .validate()
            .expect_err("empty window (start==end) must fail closed");
        assert!(err.to_string().contains("empty window"));
    }

    #[test]
    fn disabled_curtailment_with_garbage_hours_still_loads() {
        // A disabled section is inert — validation must not reject garbage in
        // a section the operator has explicitly turned off.
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[power.curtailment]
enabled = false
start_hour = 99
end_hour = 99
poll_interval_s = 1
"#,
        )
        .expect("deserialize");
        config
            .validate()
            .expect("disabled curtailment section must not fail validation");
    }

    #[test]
    fn legacy_heater_section_maps_into_mode_home() {
        let mut config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[heater]
target_watts = 800
night_mode = true
night_start_hour = 22
night_end_hour = 7
night_target_watts = 400
"#,
        )
        .expect("legacy heater section should deserialize");

        config
            .normalize_legacy_fields()
            .expect("legacy heater section should normalize");

        assert_eq!(config.mode.home.target_watts, 800);
        assert!(config.mode.home.night_mode.enabled);
        assert_eq!(config.mode.home.night_mode.start_hour, 22);
        assert_eq!(config.mode.home.night_mode.end_hour, 7);
        assert_eq!(config.mode.home.night_mode.power_reduction_pct, 50);
    }

    #[test]
    fn legacy_mode_active_heater_maps_to_home() {
        let mut config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mode]
active = "heater"
"#,
        )
        .expect("legacy heater mode should deserialize");

        config
            .normalize_legacy_fields()
            .expect("legacy heater mode should normalize");

        assert_eq!(config.mode.active, "home");
    }

    #[test]
    fn network_block_defaults_are_disabled_and_offline() {
        let config = NetworkBlockConfig::default();

        assert!(!config.enabled);
        assert!(!config.public_fallback_enabled);
        assert_eq!(config.request_timeout_ms, 1200);
        assert_eq!(config.cache_ttl_ms, 30000);
        assert_eq!(config.credential_source(), "none");
        config
            .validate()
            .expect("default network block config should be valid");
    }

    #[test]
    fn network_block_rejects_url_embedded_credentials() {
        let config = NetworkBlockConfig {
            enabled: true,
            local_node_rpc_url: "http://user:password@127.0.0.1:8332".to_string(),
            ..NetworkBlockConfig::default()
        };

        let err = config
            .validate()
            .expect_err("embedded RPC credentials must be rejected");

        assert!(err.contains("must not embed credentials"));
        assert_eq!(config.redacted_rpc_url(), "http://127.0.0.1:8332");
    }

    #[test]
    fn mining_pipeline_snapshot_config_defaults_disabled() {
        let config = MiningConfig::default();

        assert!(!config.pipeline_snapshot.enabled);
        assert_eq!(config.pipeline_snapshot.stale_after_ms, 5_000);
    }

    #[test]
    fn mining_serial_device_legacy_resolves_to_single_device() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining]
serial_device = "/dev/ttyS1"
"#,
        )
        .expect("legacy serial_device should deserialize");

        assert_eq!(
            config.mining.resolved_serial_devices("/dev/ttyS2"),
            vec!["/dev/ttyS1".to_string()]
        );
    }

    #[test]
    fn mining_serial_devices_vector_precedes_legacy_single_device() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining]
serial_device = "/dev/ttyS2"
serial_devices = ["/dev/ttyS1", "/dev/ttyS3"]
"#,
        )
        .expect("serial_devices should deserialize");

        assert_eq!(
            config.mining.resolved_serial_devices("/dev/ttyS2"),
            vec!["/dev/ttyS1".to_string(), "/dev/ttyS3".to_string()],
            "future dual-chain list must take precedence over legacy serial_device"
        );
    }

    #[test]
    fn mining_serial_devices_validate_empty_and_duplicate_entries() {
        let empty: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining]
serial_devices = ["/dev/ttyS1", " "]
"#,
        )
        .expect("empty serial_devices entry should deserialize before validate");
        assert!(
            empty
                .validate()
                .unwrap_err()
                .to_string()
                .contains("serial_devices[1]"),
            "validate() must reject empty serial_devices entries"
        );

        let duplicate: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining]
serial_devices = ["/dev/ttyS1", "/dev/ttyS1"]
"#,
        )
        .expect("duplicate serial_devices entry should deserialize before validate");
        assert!(
            duplicate
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate path"),
            "validate() must reject duplicate serial_devices entries"
        );
    }

    #[test]
    fn mining_version_rolling_defaults_flow_through_serde() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"
"#,
        )
        .expect("config without [mining] version rolling fields should deserialize");

        assert!(config.mining.version_rolling);
        assert_eq!(config.mining.version_rolling_mask, 0x1fff_e000);
    }

    #[test]
    fn mining_pipeline_snapshot_config_false_is_accepted() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining.pipeline_snapshot]
enabled = false
stale_after_ms = 5000
"#,
        )
        .expect("default-off pipeline snapshot config should deserialize");

        config
            .validate()
            .expect("default-off pipeline snapshot config should validate");
    }

    #[test]
    fn mining_pipeline_snapshot_config_enabled_is_accepted_with_positive_stale_window() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining.pipeline_snapshot]
enabled = true
stale_after_ms = 5000
"#,
        )
        .expect("pipeline snapshot config should deserialize");

        config.validate().expect(
            "enabled pipeline snapshot publisher with a positive stale window should validate",
        );
    }

    #[test]
    fn mining_pipeline_snapshot_config_enabled_rejects_zero_stale_window() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[mining.pipeline_snapshot]
enabled = true
stale_after_ms = 0
"#,
        )
        .expect("pipeline snapshot config should deserialize");

        let err = config
            .validate()
            .expect_err("enabled pipeline snapshot publisher needs a nonzero stale window");

        assert!(err
            .to_string()
            .contains("mining.pipeline_snapshot.stale_after_ms must be > 0"));
    }

    #[test]
    fn pool_url_requires_strict_stratum_tcp_scheme() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "solo.ckpool.org:3333"
worker = "worker"
"#,
        )
        .expect("bare pool URL should deserialize before validation");

        let err = config
            .validate()
            .expect_err("bare pool URL must fail at config validation");

        assert!(err.to_string().contains("pool.url is invalid"));
    }

    #[test]
    fn pool_url_rejects_leading_or_trailing_whitespace() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = " stratum+tcp://solo.ckpool.org:3333"
worker = "worker"
"#,
        )
        .expect("whitespace-padded pool URL should deserialize before validation");

        let err = config
            .validate()
            .expect_err("pool URL whitespace must fail at config validation");

        assert!(err.to_string().contains("pool.url is invalid"));
    }

    #[test]
    fn pool_sv2_url_requires_strict_sv2_scheme() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"
sv2_url = "sv2://pool.example.com"
protocol = "auto"
"#,
        )
        .expect("SV2 shortcut should deserialize before validation");

        let err = config
            .validate()
            .expect_err("SV2 shortcut URL must fail at config validation");

        assert!(err.to_string().contains("pool.sv2_url is invalid"));
    }

    #[test]
    fn pool_protocol_sv2_accepts_strict_sv2_primary_url() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum2+tcp://v2.pool.example.com:3336"
worker = "worker"
protocol = "sv2"
"#,
        )
        .expect("strict SV2 primary URL should deserialize");

        config
            .validate()
            .expect("strict SV2 primary URL should validate");
    }

    #[test]
    fn hashboard_x21_aes_config_is_accepted_without_embedded_key() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[hashboard]
eeprom_parser = "x21_aes"
decoded_reference_file = "/etc/dcentos/hashboard_decoded.json"
"#,
        )
        .expect("x21_aes hashboard config should deserialize");

        assert_eq!(config.hashboard.eeprom_parser, "x21_aes");
        assert!(config.hashboard.eeprom_key_file.is_none());
        config
            .validate()
            .expect("x21_aes parser selector should validate without a bundled key");
    }

    #[test]
    fn unknown_hashboard_eeprom_parser_is_rejected() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[hashboard]
eeprom_parser = "hidden_luxos_key"
"#,
        )
        .expect("unknown parser should deserialize then fail validation");

        let err = config
            .validate()
            .expect_err("unknown hashboard parser must fail closed");
        assert!(err.to_string().contains("hashboard.eeprom_parser"));
    }

    #[test]
    fn s19k_psu_template_fields_are_schema_valid() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[psu]
model = "APW121215f"
power_target_w = 754
power_step_w = 300
hashrate_target_ths = 120.0
hashrate_step_ths = 11.0
"#,
        )
        .expect("S19k per-board PSU defaults should deserialize");

        assert_eq!(config.psu.model, "APW121215f");
        assert_eq!(config.psu.power_target_w, Some(754));
        assert_eq!(config.psu.hashrate_target_ths, Some(120.0));
        config
            .validate()
            .expect("S19k PSU template fields should validate");
    }

    // -------------------------------------------------------------------
    // Phase 4C / EE Finding 5 #4 — am2 voltage clamp tests (2026-05-15)
    //
    // The 14_500 mV ceiling on am2-class boards (Zynq am2 S19 Pro /
    // S19j Pro on BHB42xxx hashboards via APW121215a + dsPIC regulators)
    // is enforced in `DcentraldConfig::validate()` via
    // `is_am2_platform()`. The runtime detection uses files baked by
    // the am2-s19jpro Buildroot post-build script
    // (`/etc/dcentos/board_family` = "am2" and
    // `/etc/dcentos/platform` = "zynq-bm3-am2"); for tests we use the
    // `DCENT_FORCE_AM2_VOLTAGE_CLAMP=1` env-var override.
    //
    // Mutating process-wide env vars across parallel tests is unsound, so
    // platform-clamp tests acquire a per-module mutex to serialize them.
    // -------------------------------------------------------------------
    use std::sync::Mutex;
    static AM2_CLAMP_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper for am2 voltage-clamp tests: takes the env-mutation lock,
    /// sets the override, runs the closure, then unsets the override.
    /// Returns whatever the closure returns. Safe across parallel tests.
    fn with_forced_am2_clamp<R>(f: impl FnOnce() -> R) -> R {
        let _guard = AM2_CLAMP_ENV_LOCK
            .lock()
            .expect("AM2 clamp env lock poisoned");
        std::env::remove_var("DCENT_FORCE_AM1_S9_VOLTAGE_CLAMP");
        std::env::set_var("DCENT_FORCE_AM2_VOLTAGE_CLAMP", "1");
        let result = f();
        std::env::remove_var("DCENT_FORCE_AM2_VOLTAGE_CLAMP");
        result
    }

    /// Helper for am1-s9 voltage-clamp tests; shares the env lock with the
    /// am2 helper so process-global platform overrides cannot overlap.
    fn with_forced_am1_s9_clamp<R>(f: impl FnOnce() -> R) -> R {
        let _guard = AM2_CLAMP_ENV_LOCK
            .lock()
            .expect("platform clamp env lock poisoned");
        std::env::remove_var("DCENT_FORCE_AM2_VOLTAGE_CLAMP");
        std::env::set_var("DCENT_FORCE_AM1_S9_VOLTAGE_CLAMP", "1");
        let result = f();
        std::env::remove_var("DCENT_FORCE_AM1_S9_VOLTAGE_CLAMP");
        result
    }

    // SmartSwitch pool-failover toggle (matrix §7 #2 / §6 SmartSwitch row).
    // Pins the default-off config plumbing of `[pool].smart_failover_enabled`
    // (threads into `StratumConfig::smart_failover_enabled` at every
    // stratum-client construction site). A config that omits the key MUST
    // default to false (existing behavior byte-identical); an explicit
    // `true` MUST round-trip so the daemon can opt in.

    #[test]
    fn am1_s9_voltage_ceiling_rejects_nonsense_before_runtime_clamp() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "worker"

[mining]
enabled = false
voltage_mv = 15000
"#,
        )
        .expect("S9 over-voltage config must deserialize before validation");

        let err = with_forced_am1_s9_clamp(|| {
            config
                .validate()
                .expect_err("am1-s9 voltage_mv > 9400 must be refused")
                .to_string()
        });
        assert!(
            err.contains("am1-s9 PIC16 chip-rail ceiling"),
            "error should name the S9 ceiling, got: {err}"
        );
        assert!(
            err.contains("9400"),
            "error should include the exact 9400 mV boundary, got: {err}"
        );
    }

    #[test]
    fn am1_s9_voltage_ceiling_allows_boundary_and_default() {
        let boundary: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "worker"

[mining]
enabled = false
voltage_mv = 9400
"#,
        )
        .expect("S9 boundary config must deserialize");

        with_forced_am1_s9_clamp(|| {
            boundary
                .validate()
                .expect("am1-s9 voltage_mv exactly 9400 mV must validate");

            let default_auto: DcentraldConfig =
                toml::from_str("").expect("empty config must deserialize");
            default_auto
                .validate()
                .expect("voltage_mv=0 auto-detect must remain valid on am1-s9");
        });
    }

    #[test]
    fn smart_failover_defaults_off_when_absent() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""
"#,
        )
        .expect("config without smart_failover_enabled must deserialize");
        assert!(
            !config.pool.smart_failover_enabled,
            "SmartSwitch must default OFF when [pool].smart_failover_enabled is absent"
        );
    }

    #[test]
    fn smart_failover_parses_explicit_true() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""
smart_failover_enabled = true
"#,
        )
        .expect("config with smart_failover_enabled = true must deserialize");
        assert!(
            config.pool.smart_failover_enabled,
            "explicit [pool].smart_failover_enabled = true must round-trip"
        );
    }

    #[test]
    fn am2_voltage_clamp_refuses_above_14500_mv() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""

[mining]
enabled = false
frequency_mhz = 525
voltage_mv = 15000
"#,
        )
        .expect("config with voltage_mv=15000 must deserialize before validation");

        let err = with_forced_am2_clamp(|| {
            config
                .validate()
                .expect_err("am2 voltage_mv > 14500 must be refused")
        });

        let msg = err.to_string();
        assert!(
            msg.contains("14500") && msg.contains("15000"),
            "am2 clamp error must cite both the limit and the offending value; got: {}",
            msg
        );
        assert!(
            msg.contains("dsPIC") || msg.contains("BHB42xxx"),
            "am2 clamp error must explain the hardware envelope; got: {}",
            msg
        );
    }

    #[test]
    fn am2_voltage_clamp_accepts_14500_mv_boundary() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""

[mining]
enabled = false
frequency_mhz = 525
voltage_mv = 14500
"#,
        )
        .expect("voltage_mv at the 14500 boundary must deserialize");

        with_forced_am2_clamp(|| {
            config
                .validate()
                .expect("voltage_mv exactly at 14500 mV is allowed on am2")
        });
    }

    #[test]
    fn am2_voltage_clamp_accepts_proven_13700_mv() {
        // 13700 mV is the proven am2 chip-rail target (Loki-mod XIL milestone
        // + .139 cold-boot init). Must always be accepted.
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""

[mining]
enabled = false
frequency_mhz = 525
voltage_mv = 13700
"#,
        )
        .expect("13700 mV config must deserialize");

        with_forced_am2_clamp(|| {
            config
                .validate()
                .expect("voltage_mv=13700 is the proven am2 chip-rail target and must validate")
        });
    }

    #[test]
    fn am2_voltage_clamp_inactive_when_not_am2() {
        // Without `DCENT_FORCE_AM2_VOLTAGE_CLAMP=1` (and on a non-am2 host
        // like the Windows dev box where /etc/dcentos/board_family does not
        // exist), the wider 5000-20000 mV envelope is the only gate. This
        // test serves as a regression-pin: do NOT make the am2 clamp
        // platform-agnostic; S9 (9.1 V chip rail) and am3 (~13.7 V via a
        // different topology) are out of scope and have their own envelopes.
        let _guard = AM2_CLAMP_ENV_LOCK
            .lock()
            .expect("AM2 clamp env lock poisoned");
        // Ensure the override is OFF for this test.
        std::env::remove_var("DCENT_FORCE_AM2_VOLTAGE_CLAMP");

        // Skip if running on an actual am2 image (board_family file present).
        let on_am2_image = std::fs::read_to_string("/etc/dcentos/board_family")
            .map(|s| s.trim().eq_ignore_ascii_case("am2"))
            .unwrap_or(false);
        if on_am2_image {
            // We're on a Buildroot-baked am2 image; the clamp will fire even
            // without the env override. This is the correct production
            // behavior — skip the negative test rather than mutate prod
            // identity files. (The other 3 tests cover the positive path.)
            return;
        }

        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "worker"

[mining]
enabled = false
voltage_mv = 14800
"#,
        )
        .expect("14800 mV config must deserialize");

        // 14800 is below the wide 20000 ceiling and above the am2 14500
        // ceiling. With the am2 clamp inactive, validate() should succeed.
        config
            .validate()
            .expect("voltage_mv=14800 must validate on non-am2 hosts (wide 5000-20000 envelope)");
    }

    #[test]
    fn am2_baked_default_config_passes_voltage_clamp_and_safety_invariants() {
        // Phase 4B (2026-05-15): the baked default config installed at
        // /etc/dcentrald.toml on every am2-s19jpro Buildroot image. Must
        // pass the schema, the am2 voltage clamp, and ship with the EE
        // Finding 5 invariants intact.
        let config_text = include_str!("../../configs/dcentrald_s19jpro_am2_baked_default.toml");
        let mut config: DcentraldConfig = toml::from_str(config_text)
            .expect("am2 baked default config must deserialize against the schema");
        config
            .normalize_legacy_fields()
            .expect("am2 baked default must normalize cleanly");
        with_forced_am2_clamp(|| {
            config
                .validate()
                .expect("am2 baked default must pass validate() incl. am2 voltage clamp")
        });

        // Invariants the post-build.sh shell gates also enforce — duplicated
        // here as a Rust-side regression-pin against a future edit that
        // accidentally relaxes the EE Finding 5 safety envelope.
        assert!(
            config.mining.voltage_mv <= 14_500,
            "baked default mining.voltage_mv ({}) must stay <= 14500 mV am2 ceiling",
            config.mining.voltage_mv
        );
        assert_eq!(
            config.mining.voltage_mv, 13_700,
            "baked default must ship the proven 13700 mV am2 chip-rail target"
        );
        assert!(
            config.thermal.fan_max_pwm <= 30,
            "baked default thermal.fan_max_pwm ({}) must stay <= 30 (home fan-cap ceiling)",
            config.thermal.fan_max_pwm
        );
        assert_eq!(
            config.thermal.dangerous_temp_c, 80,
            "baked default dangerous_temp_c must match the proven Am2ThermalSupervisor threshold"
        );
        assert_eq!(
            config.thermal.supervisor.min_fans, 3,
            "am2 baked default must require at least 3 working fans when the thermal supervisor is active"
        );
        assert!(
            !config.hash_on_disconnect.enabled,
            "baked default hash_on_disconnect.enabled must be false for home/unattended safety"
        );
        assert!(
            !config.mining.enabled,
            "baked default mining.enabled must be false — operator configures pool first"
        );
        assert!(
            config.watchdog.enabled,
            "baked default watchdog.enabled must be true (workspace SAFETY rule)"
        );
        assert!(
            config.pool.url.trim().is_empty() && config.pool.worker.trim().is_empty(),
            "baked default pool.url/pool.worker must be empty — operator configures them"
        );
        // [power.psu_override] ships disabled (stock APW12 assumption). Operator
        // with a Loki-removed unit flips this via /data/dcentrald.toml.
        let psu_override = config
            .power
            .psu_override
            .as_ref()
            .expect("baked default must declare [power.psu_override] block (Loki-bypass path)");
        assert!(
            !psu_override.enabled,
            "baked default [power.psu_override].enabled must be false (stock APW12 assumed)"
        );
    }

    #[test]
    fn am2_baked_defaults_pin_four_fan_supervisor_floor() {
        for (label, config_text) in [
            (
                "am2-s19jpro",
                include_str!("../../configs/dcentrald_s19jpro_am2_baked_default.toml"),
            ),
            (
                "am2-s19pro",
                include_str!("../../configs/dcentrald_s19pro_am2_baked_default.toml"),
            ),
            (
                "am2-s17pro",
                include_str!("../../configs/dcentrald_s17pro_am2_baked_default.toml"),
            ),
        ] {
            let mut config: DcentraldConfig = toml::from_str(config_text)
                .unwrap_or_else(|err| panic!("{label} baked config must deserialize: {err}"));
            config
                .normalize_legacy_fields()
                .unwrap_or_else(|err| panic!("{label} baked config must normalize: {err}"));
            with_forced_am2_clamp(|| {
                config
                    .validate()
                    .unwrap_or_else(|err| panic!("{label} baked config must validate: {err}"))
            });

            assert!(
                !config.thermal.supervisor.enabled,
                "{label} must keep the supervisor dormant by default"
            );
            assert_eq!(
                config.thermal.supervisor.min_fans, 3,
                "{label} must pin the am2 four-fan quorum to 3"
            );
        }
    }

    #[test]
    fn expanded_antminer_runtime_configs_parse_validate_and_stay_management_only() {
        for (label, config_text, model, chip_type, chip_count) in [
            (
                "s15",
                include_str!("../../configs/dcentrald_s15.toml"),
                "s15",
                "BM1391",
                Some(84),
            ),
            (
                "t15",
                include_str!("../../configs/dcentrald_t15.toml"),
                "t15",
                "BM1391",
                Some(63),
            ),
            (
                "s17",
                include_str!("../../configs/dcentrald_s17.toml"),
                "s17",
                "BM1397",
                Some(48),
            ),
            (
                "s17pro",
                include_str!("../../configs/dcentrald_s17pro.toml"),
                "s17pro",
                "BM1397",
                Some(48),
            ),
            (
                "s17plus",
                include_str!("../../configs/dcentrald_s17plus.toml"),
                "s17+",
                "BM1396",
                Some(65),
            ),
            (
                "t17",
                include_str!("../../configs/dcentrald_t17.toml"),
                "t17",
                "BM1397",
                Some(30),
            ),
            (
                "t17plus",
                include_str!("../../configs/dcentrald_t17plus.toml"),
                "t17+",
                "BM1396",
                Some(44),
            ),
            (
                "t19",
                include_str!("../../configs/dcentrald_t19.toml"),
                "t19",
                "BM1398",
                None,
            ),
        ] {
            let mut config: DcentraldConfig = toml::from_str(config_text)
                .unwrap_or_else(|err| panic!("{label} runtime config must deserialize: {err}"));
            config
                .normalize_legacy_fields()
                .unwrap_or_else(|err| panic!("{label} runtime config must normalize: {err}"));
            with_forced_am2_clamp(|| {
                config
                    .validate()
                    .unwrap_or_else(|err| panic!("{label} runtime config must validate: {err}"))
            });

            assert_eq!(config.mining.model.as_deref(), Some(model));
            assert_eq!(config.mining.serial_chip_type.as_deref(), Some(chip_type));
            assert_eq!(config.mining.serial_chip_count, chip_count);
            assert!(
                !config.mining_start_enabled(),
                "{label} template must stay management-only"
            );
            assert!(
                config.thermal.fan_max_pwm <= 30,
                "{label} template must preserve the home fan cap"
            );
        }
    }

    #[test]
    fn s17pro_baked_default_uses_per_chain_chip_count() {
        let config_text = include_str!("../../configs/dcentrald_s17pro_am2_baked_default.toml");
        let mut config: DcentraldConfig =
            toml::from_str(config_text).expect("S17 Pro baked default must deserialize");
        config
            .normalize_legacy_fields()
            .expect("S17 Pro baked default must normalize");
        with_forced_am2_clamp(|| {
            config
                .validate()
                .expect("S17 Pro baked default must validate")
        });

        assert_eq!(config.mining.model.as_deref(), Some("s17pro"));
        assert_eq!(config.mining.serial_chip_type.as_deref(), Some("BM1397"));
        assert_eq!(
            config.mining.serial_chip_count,
            Some(48),
            "serial_chip_count is chips per chain, not the 144-chip unit total"
        );
    }

    #[test]
    fn s21xp_runtime_config_uses_dedicated_bm1370_geometry() {
        let config_text = include_str!("../../dcentrald_s21xp.toml");
        let mut config: DcentraldConfig =
            toml::from_str(config_text).expect("S21 XP runtime config must deserialize");
        config
            .normalize_legacy_fields()
            .expect("S21 XP runtime config must normalize");
        config
            .validate()
            .expect("S21 XP runtime config must validate");

        assert_eq!(config.mining.model.as_deref(), Some("s21xp"));
        assert_eq!(config.mining.serial_chip_type.as_deref(), Some("BM1370"));
        assert_eq!(
            config.mining.serial_chip_count,
            Some(230),
            "S21 XP must not ride S21 Pro's 65-chip row"
        );
        assert!(!config.mining_start_enabled());
    }

    #[test]
    fn am2_xil_config_passes_am2_voltage_clamp() {
        // End-to-end: load the proven XIL config from disk, simulate an am2
        // platform via the env-var override, and confirm the full validate()
        // pipeline succeeds. Regression-pins the milestone-config layout
        // (voltage_mv=13700, the 525 MHz traced PLL, fan_max_pwm=30, etc.)
        // against the new am2 voltage clamp.
        let config_text = include_str!("../../configs/dcentrald_s19jpro_xil.toml");
        let mut config: DcentraldConfig =
            toml::from_str(config_text).expect("XIL config must deserialize against the schema");
        config
            .normalize_legacy_fields()
            .expect("XIL config must normalize");

        with_forced_am2_clamp(|| {
            config
                .validate()
                .expect("XIL config must pass am2 voltage clamp at validate()")
        });

        assert_eq!(config.mining.voltage_mv, 13_700);
        assert!(
            config.mining.voltage_mv <= 14_500,
            "XIL voltage_mv must stay <= am2 14500 mV ceiling"
        );
    }

    #[test]
    fn s19jpro_xil_config_loads_and_round_trips_psu_override() {
        let config_text = include_str!("../../configs/dcentrald_s19jpro_xil.toml");
        let mut config: DcentraldConfig =
            toml::from_str(config_text).expect("XIL config should match the config schema");

        config
            .normalize_legacy_fields()
            .expect("XIL config should normalize cleanly");
        config.validate().expect("XIL config should validate");

        // Home-quiet + first-proof invariants the XIL bring-up depends on.
        assert_eq!(
            config.thermal.fan_max_pwm, 30,
            "XIL is a home unit — the fan ceiling must stay capped at PWM 30"
        );
        assert_eq!(
            config.thermal.dangerous_temp_c, 80,
            "XIL dangerous-temp threshold feeds the am2 thermal supervisor fail-closed gate"
        );
        assert_eq!(
            config.mining.voltage_mv, 13_700,
            "voltage_mv is the per-chain dsPIC chip-rail target — never the PSU rail"
        );
        assert_eq!(
            config.mining.frequency_mhz, 525,
            "XIL first-proof profile pins the RE-traced BM1362 525 MHz PLL condition"
        );

        let psu_override = config
            .power
            .psu_override
            .as_ref()
            .expect("XIL config should declare power.psu_override");
        assert!(
            !psu_override.enabled,
            "first XIL bring-up keeps the Loki board in place with psu_override disabled"
        );
        assert_eq!(psu_override.model, "APW3");
        assert!((psu_override.voltage_v - 12.8).abs() < 0.0001);

        let round_tripped_text =
            toml::to_string_pretty(&config).expect("XIL config should serialize");
        let round_tripped: DcentraldConfig =
            toml::from_str(&round_tripped_text).expect("serialized XIL config should deserialize");
        let round_tripped_override = round_tripped
            .power
            .psu_override
            .as_ref()
            .expect("round-tripped XIL config should preserve power.psu_override");
        assert!(!round_tripped_override.enabled);
        assert_eq!(round_tripped_override.model, "APW3");
        assert!((round_tripped_override.voltage_v - 12.8).abs() < 0.0001);
    }

    // ---- F4 + F5 (2026-05-17, .25 first-boot dcentrald-down rootcause) ------
    //
    // F4: post-build.sh bakes the conservative
    // dcentrald_s19jpro_xil_baked_default.toml (mining-disabled-until-
    // configured) as /etc/dcentrald/xil_override.toml — NOT the
    // .109-specific dcentrald_s19jpro_xil_override.toml runtime.
    //
    // F5: a fresh/unconfigured am2 unit has mining_start_enabled()==false
    // → main.rs parks it in management-only mode (no PIC preflight, no
    // crash). After the operator configures a pool + enables mining,
    // mining_start_enabled()==true.

    /// F4: the BAKED am2 first-boot default ships conservative
    /// (mining-disabled, no pool) so the F5 gate makes a fresh unit
    /// management-only by config — WHILE keeping the brick-safe
    /// [power.psu_override] enabled=true (the .109 Loki invariant).
    #[test]
    fn am2_xil_baked_default_is_conservative_but_brick_safe() {
        let config_text = include_str!("../../configs/dcentrald_s19jpro_xil_baked_default.toml");
        let mut config: DcentraldConfig =
            toml::from_str(config_text).expect("F4 baked default must match the config schema");
        config
            .normalize_legacy_fields()
            .expect("F4 baked default must normalize");
        with_forced_am2_clamp(|| {
            config
                .validate()
                .expect("F4 baked default must pass validate() incl. am2 voltage clamp")
        });

        // --- F5 precondition: a fresh unit must NOT auto-start mining ---
        assert!(
            !config.mining.enabled,
            "F4 baked default mining.enabled must be false (idle-first)"
        );
        assert!(
            config.pool.url.trim().is_empty() && config.pool.worker.trim().is_empty(),
            "F4 baked default must ship NO pool (operator configures via wizard)"
        );
        assert!(
            !config.mining_start_enabled(),
            "F5: a fresh am2 unit on the baked default must have \
             mining_start_enabled()==false → management-only first boot, \
             no PIC preflight, no crash"
        );

        // --- BRICK-SAFETY INVARIANT: psu_override stays enabled=true ---
        let psu_override = config
            .power
            .psu_override
            .as_ref()
            .expect("F4 baked default must declare [power.psu_override]");
        assert!(
            psu_override.enabled,
            "BRICK-SAFETY: F4 baked default [power.psu_override].enabled \
             MUST be true — enabled=false + Loki present + APW3 takes the \
             Apw121215a path which self-disables the rail in ~30s once the \
             operator enables mining (the .109 Loki invariant). F4 makes the \
             .109 POOL/WORKER opt-in, it does NOT remove the brick-safe override"
        );
        assert_eq!(psu_override.model, "APW3");
        assert!((psu_override.voltage_v - 12.8).abs() < 0.0001);

        // --- Home-safety knobs match the proven .109 run ---
        assert_eq!(config.mining.voltage_mv, 13_700);
        assert!(config.mining.voltage_mv <= 14_500);
        assert!(config.thermal.fan_max_pwm <= 30);
        assert_eq!(config.thermal.dangerous_temp_c, 80);
        assert!(!config.hash_on_disconnect.enabled);
        assert!(
            config.watchdog.enabled,
            "baked production image: HW watchdog ON by default"
        );
    }

    /// F5: once the operator configures a pool AND enables mining,
    /// `mining_start_enabled()` flips true so the s19j-hybrid arm runs the
    /// real (proven .109) mining bring-up — i.e. F5 only suppresses
    /// cold-boot on an *unconfigured* unit, it does not break the milestone
    /// path.
    #[test]
    fn am2_xil_baked_default_starts_mining_once_operator_configures() {
        let config_text = include_str!("../../configs/dcentrald_s19jpro_xil_baked_default.toml");
        let mut config: DcentraldConfig =
            toml::from_str(config_text).expect("F4 baked default must deserialize");

        assert!(
            !config.mining_start_enabled(),
            "precondition: fresh unit is management-only"
        );

        // Operator completes the wizard: sets a pool + enables mining.
        config.pool.url = "stratum+tcp://public-pool.io:21496".to_string();
        config.pool.worker = "bc1qexampleexampleexampleexample.s19jpro".to_string();
        config.mining.enabled = true;

        assert!(
            config.mining_start_enabled(),
            "F5: after the operator configures a pool + enables mining, \
             mining_start_enabled() must be true so the proven .109 \
             bring-up path runs (F5 must NOT permanently disable mining)"
        );
        // The brick-safe override is still in force for that real run.
        assert!(config.power.psu_override.as_ref().unwrap().enabled);
    }

    /// The .109 milestone runtime config (still the operator-opt-in /tmp
    /// overlay file) is UNCHANGED by F4
    /// and remains directly selectable: it carries the pool/worker +
    /// mining.enabled=true so `mining_start_enabled()` is true (the F5 gate
    /// does NOT suppress an explicitly-configured run).
    #[test]
    fn am2_xil_109_override_runtime_still_selectable() {
        let config_text = include_str!("../../configs/dcentrald_s19jpro_xil_override.toml");
        let config: DcentraldConfig = toml::from_str(config_text)
            .expect(".109 override runtime config must still deserialize");

        assert!(
            config.mining.enabled,
            ".109 override runtime keeps mining.enabled=true (the proven path)"
        );
        assert!(
            config.has_configured_pool(),
            ".109 override runtime keeps its public-pool.io worker"
        );
        assert!(
            config.mining_start_enabled(),
            "the .109 milestone path stays directly runnable — F4/F5 only \
             changed which file is BAKED as the default, not this opt-in file"
        );
        let psu_override = config
            .power
            .psu_override
            .as_ref()
            .expect(".109 override runtime declares [power.psu_override]");
        assert!(
            psu_override.enabled,
            ".109 override runtime keeps [power.psu_override].enabled=true \
             (brick-safe Loki path — )"
        );
        assert_eq!(psu_override.model, "APW3");
        assert!((psu_override.voltage_v - 12.8).abs() < 0.0001);
    }

    // -----------------------------------------------------------------------
    // R1 — am2 low-idle fan command: ThermalConfig.fan_idle_pwm
    //
    // -----------------------------------------------------------------------

    /// The default idle PWM is the low home-idle command (10) and is kept in
    /// lockstep with `dcentrald_hal::fan::PWM_QUIET_BOOT`. AM2/XIL acoustic
    /// proof still requires tach/RPM/operator confirmation.
    #[test]
    fn fan_idle_pwm_default_is_pwm_quiet_boot() {
        use super::ThermalConfig;
        assert_eq!(
            ThermalConfig::default().fan_idle_pwm,
            dcentrald_hal::fan::PWM_QUIET_BOOT,
            "fan_idle_pwm default must equal PWM_QUIET_BOOT (10)"
        );
        assert_eq!(ThermalConfig::default().fan_idle_pwm, 10);
    }

    /// A TOML that omits `fan_idle_pwm` under `[thermal]` gets the safe
    /// default via serde, and the default config validates clean.
    #[test]
    fn fan_idle_pwm_serde_default_and_validates() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"
"#,
        )
        .expect("config without [thermal] should deserialize");
        assert_eq!(
            config.thermal.fan_idle_pwm, 10,
            "missing fan_idle_pwm must default to 10 via serde"
        );
        config
            .validate()
            .expect("default fan_idle_pwm (10) must pass validate()");
    }

    /// validate() fail-closes when fan_idle_pwm > fan_max_pwm (clamps DOWN
    /// by refusing to start — never raises the cap).
    #[test]
    fn fan_idle_pwm_above_fan_max_is_rejected() {
        let mut config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[thermal]
fan_max_pwm = 20
fan_idle_pwm = 25
"#,
        )
        .expect("config should deserialize");
        config.normalize_legacy_fields().ok();
        let err = config
            .validate()
            .expect_err("fan_idle_pwm (25) > fan_max_pwm (20) must be rejected");
        assert!(
            err.to_string().contains("fan_idle_pwm") && err.to_string().contains("fan_max_pwm"),
            "error must name both fields, got: {err}"
        );
    }

    /// validate() fail-closes when fan_idle_pwm > PWM_SAFETY_MAX (30) even
    /// when fan_max_pwm permits it (fan_max_pwm=40 ≥ idle=35, so the
    /// fan_idle≤fan_max check passes — the absolute home cap is what stops
    /// it). This isolates the PWM_SAFETY_MAX clause specifically.
    #[test]
    fn fan_idle_pwm_above_safety_max_is_rejected() {
        let mut config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[thermal]
fan_max_pwm = 40
fan_idle_pwm = 35
"#,
        )
        .expect("config should deserialize");
        config.normalize_legacy_fields().ok();
        let err = config
            .validate()
            .expect_err("fan_idle_pwm (35) > PWM_SAFETY_MAX (30) must be rejected");
        assert!(
            err.to_string().contains("fan_idle_pwm") && err.to_string().contains("PWM_SAFETY_MAX"),
            "error must cite PWM_SAFETY_MAX, got: {err}"
        );
    }

    /// fan_idle_pwm == fan_max_pwm == PWM_SAFETY_MAX (boundary) is accepted.
    #[test]
    fn fan_idle_pwm_at_boundary_is_accepted() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "worker"

[thermal]
fan_max_pwm = 30
fan_idle_pwm = 30
"#,
        )
        .expect("config should deserialize");
        config
            .validate()
            .expect("fan_idle_pwm == fan_max_pwm == 30 (boundary) must validate");
    }

    /// The three shipped XIL configs each carry `fan_idle_pwm = 10` and pass
    /// the full validate() pipeline (regression-pins the config wiring).
    #[test]
    fn xil_configs_carry_fan_idle_pwm_10() {
        for (name, text) in [
            (
                "baked_default",
                include_str!("../../configs/dcentrald_s19jpro_xil_baked_default.toml"),
            ),
            (
                "xil",
                include_str!("../../configs/dcentrald_s19jpro_xil.toml"),
            ),
            (
                "override",
                include_str!("../../configs/dcentrald_s19jpro_xil_override.toml"),
            ),
        ] {
            let mut config: DcentraldConfig = toml::from_str(text)
                .unwrap_or_else(|e| panic!("XIL config {name} must deserialize: {e}"));
            config
                .normalize_legacy_fields()
                .unwrap_or_else(|e| panic!("XIL config {name} must normalize: {e}"));
            assert_eq!(
                config.thermal.fan_idle_pwm, 10,
                "XIL config {name} must declare fan_idle_pwm = 10"
            );
            with_forced_am2_clamp(|| {
                config
                    .validate()
                    .unwrap_or_else(|e| panic!("XIL config {name} must validate: {e}"));
            });
        }
    }

    // 2026-05-22 (XIL `a lab unit` recovery, Layer 3) — config-knob regression pins.
    //
    // A TOML config that omits the 3 new knobs MUST deserialize with the
    // canonical defaults:
    //   am2_post_eeprom_dspic_grace_ms = 2000
    //   am2_dspic_warmup_before_get_version = true
    //   am2_fan_gate_before_pic = true
    //
    // A TOML config that explicitly sets all three values MUST round-trip.

    #[test]
    fn xil25_consolidated_fix_knobs_default_when_absent() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""
"#,
        )
        .expect("config without xil knobs must deserialize with defaults");
        assert_eq!(
            config.mining.am2_post_eeprom_dspic_grace_ms, 2000,
            "am2_post_eeprom_dspic_grace_ms must default to 2000 ms (4× BraiinsOS RESET_DELAY)"
        );
        assert!(
            config.mining.am2_dspic_warmup_before_get_version,
            "am2_dspic_warmup_before_get_version must default to true"
        );
        assert!(
            config.mining.am2_fan_gate_before_pic,
            "am2_fan_gate_before_pic must default to true"
        );
    }

    #[test]
    fn xil25_consolidated_fix_knobs_round_trip_explicit_values() {
        let config: DcentraldConfig = toml::from_str(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = ""

[mining]
am2_post_eeprom_dspic_grace_ms = 5000
am2_dspic_warmup_before_get_version = false
am2_fan_gate_before_pic = false
"#,
        )
        .expect("explicit xil knob values must round-trip");
        assert_eq!(config.mining.am2_post_eeprom_dspic_grace_ms, 5000);
        assert!(!config.mining.am2_dspic_warmup_before_get_version);
        assert!(!config.mining.am2_fan_gate_before_pic);
    }

    #[test]
    fn xil25_consolidated_fix_knobs_default_via_struct_default() {
        // Programmatic Default impl matches the serde default fns.
        let cfg = MiningConfig::default();
        assert_eq!(cfg.am2_post_eeprom_dspic_grace_ms, 2000);
        assert!(cfg.am2_dspic_warmup_before_get_version);
        assert!(cfg.am2_fan_gate_before_pic);
    }

    // -----------------------------------------------------------------
    //  (2026-05-22, CE §5 hardening) — am2_post_eeprom_dspic_grace_ms
    // clamp behavioral tests (QA §10 CI-5 + CI-10).
    //
    // The clamp lives in `DcentraldConfig::validate()` and refuses values
    // above 10_000 ms (10 s safety ceiling). Value 0 is permitted
    // (disables the grace sleep).
    // -----------------------------------------------------------------

    fn parse_with_grace_ms(value: &str) -> Result<DcentraldConfig, String> {
        let toml_text = format!(
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[mining]
am2_post_eeprom_dspic_grace_ms = {value}
"#
        );
        let config: DcentraldConfig =
            toml::from_str(&toml_text).map_err(|e| format!("deserialize: {e}"))?;
        config.validate().map(|_| config).map_err(|e| e.to_string())
    }

    #[test]
    fn am2_post_eeprom_grace_ms_zero_is_permitted() {
        // Value 0 = "disable the grace sleep entirely (legacy timing)".
        let cfg = parse_with_grace_ms("0").expect("0 must validate (disables grace)");
        assert_eq!(cfg.mining.am2_post_eeprom_dspic_grace_ms, 0);
    }

    #[test]
    fn am2_post_eeprom_grace_ms_default_2000_validates() {
        // Default TOML (knob absent) validates and produces 2000 ms.
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"
"#;
        let config: DcentraldConfig =
            toml::from_str(toml_text).expect("default config must deserialize");
        assert_eq!(config.mining.am2_post_eeprom_dspic_grace_ms, 2000);
        config.validate().expect("default config must validate");
    }

    #[test]
    fn am2_post_eeprom_grace_ms_at_ceiling_10000_is_accepted() {
        // Exactly at the boundary (inclusive upper bound).
        let cfg = parse_with_grace_ms("10000").expect("10000 ms (the ceiling) must validate");
        assert_eq!(cfg.mining.am2_post_eeprom_dspic_grace_ms, 10_000);
    }

    #[test]
    fn am2_post_eeprom_grace_ms_just_above_ceiling_rejected() {
        // 10_001 ms is the first rejected value — proves the boundary is
        // inclusive at 10_000 and exclusive above.
        let err = parse_with_grace_ms("10001")
            .expect_err("10001 ms must be rejected by the Wave-23 clamp");
        assert!(
            err.contains("am2_post_eeprom_dspic_grace_ms"),
            "error must name the offending field; got: {err}"
        );
        assert!(
            err.contains("10_000") || err.contains("10000"),
            "error must cite the 10_000 ms safety ceiling; got: {err}"
        );
    }

    #[test]
    fn am2_post_eeprom_grace_ms_typo_2000000_rejected() {
        // The motivating CE §5 typo: `2000000` (2_000 s) — exactly what
        // the DoS-prevention clamp must reject.
        let err = parse_with_grace_ms("2000000")
            .expect_err("2000000 ms (2_000 s — operator typo) must be rejected");
        assert!(
            err.contains("am2_post_eeprom_dspic_grace_ms"),
            "error must name the field; got: {err}"
        );
    }

    #[test]
    fn am2_post_eeprom_grace_ms_u64_max_rejected() {
        // Pathological value — must fail closed (not stall the daemon).
        let err =
            parse_with_grace_ms(&u64::MAX.to_string()).expect_err("u64::MAX ms must be rejected");
        assert!(
            err.contains("am2_post_eeprom_dspic_grace_ms"),
            "error must name the field; got: {err}"
        );
    }

    // -----------------------------------------------------------------
    //  (2026-05-22, EE-LOKI-001) — PsuOverride serde-contract tests
    // for the new operator-declared `no_smbus_peer` and
    // `psu_hardware_variant` fields. Both default `None`; missing fields
    // produce byte-identical TOML serialization to pre- configs.
    // -----------------------------------------------------------------

    #[test]
    fn psu_override_no_smbus_peer_defaults_none_when_absent() {
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[power.psu_override]
enabled = true
model = "APW3"
voltage_v = 12.8
"#;
        let cfg: DcentraldConfig =
            toml::from_str(toml_text).expect("legacy psu_override must still deserialize");
        let ovr = cfg.power.psu_override.expect("override block must exist");
        assert_eq!(
            ovr.no_smbus_peer, None,
            "no_smbus_peer must default to None"
        );
        assert_eq!(
            ovr.psu_hardware_variant, None,
            "psu_hardware_variant must default to None"
        );
    }

    #[test]
    fn psu_override_no_smbus_peer_round_trips_true() {
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[power.psu_override]
enabled = true
model = "APW3"
voltage_v = 12.8
no_smbus_peer = true
psu_hardware_variant = "bare-apw3"
"#;
        let cfg: DcentraldConfig =
            toml::from_str(toml_text).expect("Wave-23 psu_override must deserialize");
        let ovr = cfg.power.psu_override.expect("override block must exist");
        assert_eq!(ovr.no_smbus_peer, Some(true));
        assert_eq!(ovr.psu_hardware_variant.as_deref(), Some("bare-apw3"));
    }

    #[test]
    fn psu_override_psu_hardware_variant_round_trip_loki() {
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[power.psu_override]
enabled = true
model = "APW3"
voltage_v = 12.8
psu_hardware_variant = "loki"
"#;
        let cfg: DcentraldConfig = toml::from_str(toml_text).expect("loki variant must parse");
        let ovr = cfg.power.psu_override.expect("override block must exist");
        assert_eq!(ovr.psu_hardware_variant.as_deref(), Some("loki"));
        assert_eq!(ovr.no_smbus_peer, None);
    }

    #[test]
    fn psu_override_none_fields_skip_serialization_byte_identical() {
        // The `skip_serializing_if = "Option::is_none"` contract:
        // round-tripping a legacy override config through to_string +
        // from_str MUST NOT add the new fields when they are None.
        let original = r#"[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[power.psu_override]
enabled = true
model = "APW3"
voltage_v = 12.8
"#;
        let cfg: DcentraldConfig = toml::from_str(original).expect("must parse");
        let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
        // The new fields MUST NOT appear in the round-tripped output when
        // they were absent in the source.
        assert!(
            !serialized.contains("no_smbus_peer"),
            "no_smbus_peer=None must NOT serialize (byte-identical contract); got:\n{serialized}"
        );
        assert!(
            !serialized.contains("psu_hardware_variant"),
            "psu_hardware_variant=None must NOT serialize; got:\n{serialized}"
        );
    }

    // -----------------------------------------------------------------------
    // W8 parity: [thermal.immersion] — default-OFF byte-identical + round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn thermal_immersion_default_is_off() {
        // A config with NO `[thermal.immersion]` section must deserialize to
        // the default (enabled = false, acknowledge_air_cooled_override =
        // false) — the air-cooled byte-identical path.
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"
"#;
        let cfg: DcentraldConfig =
            toml::from_str(toml_text).expect("config without [thermal.immersion] must parse");
        assert!(
            !cfg.thermal.immersion.enabled,
            "missing [thermal.immersion] must default to enabled = false (air-cooled path)"
        );
        assert!(
            !cfg.thermal.immersion.acknowledge_air_cooled_override,
            "acknowledge_air_cooled_override must default to false"
        );
        // The pure decision must be Disabled regardless of platform — the
        // controller never bypasses fans on the default config.
        use dcentrald_thermal::immersion::ImmersionDecision;
        assert_eq!(
            cfg.thermal.immersion.decide(true),
            ImmersionDecision::Disabled
        );
        assert_eq!(
            cfg.thermal.immersion.decide(false),
            ImmersionDecision::Disabled
        );
        assert!(!cfg.thermal.immersion.decide(true).fans_bypassed());
    }

    #[test]
    fn thermal_immersion_default_serializes_byte_identical_when_off() {
        // Round-tripping a config that never set `[thermal.immersion]` must NOT
        // change the immersion fields away from the all-false default. (We do
        // NOT assert the section is absent from the serialized output —
        // `ImmersionConfig` has no `skip_serializing_if`, matching the existing
        // `[thermal.supervisor]` sibling — but the VALUES must be byte-identical
        // to the default: a disabled section is behaviorally inert.)
        let original = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"
"#;
        let cfg: DcentraldConfig = toml::from_str(original).expect("must parse");
        let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
        let reparsed: DcentraldConfig =
            toml::from_str(&serialized).expect("round-trip must reparse");
        assert_eq!(
            reparsed.thermal.immersion,
            dcentrald_thermal::immersion::ImmersionConfig::default(),
            "an off immersion config must round-trip byte-identical to the default; got:\n{serialized}"
        );
        // Defense-in-depth: the round-tripped section must still be disabled.
        assert!(!reparsed.thermal.immersion.enabled);
        assert!(!reparsed.thermal.immersion.acknowledge_air_cooled_override);
    }

    #[test]
    fn thermal_immersion_config_round_trips_through_validate() {
        // An explicit, enabled `[thermal.immersion]` section must round-trip
        // through the full `DcentraldConfig` load+validate path (immersion does
        // not add any new validate() gate, so it must not regress validation).
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[thermal.immersion]
enabled = true
acknowledge_air_cooled_override = true
"#;
        let cfg: DcentraldConfig =
            toml::from_str(toml_text).expect("explicit [thermal.immersion] must parse");
        assert!(cfg.thermal.immersion.enabled);
        assert!(cfg.thermal.immersion.acknowledge_air_cooled_override);
        // validate() must still succeed — immersion adds no new failure mode.
        cfg.validate()
            .expect("config with immersion enabled must still validate");

        // Serialize → reparse must preserve both flags.
        let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
        let reparsed: DcentraldConfig =
            toml::from_str(&serialized).expect("round-trip must reparse");
        assert!(reparsed.thermal.immersion.enabled);
        assert!(reparsed.thermal.immersion.acknowledge_air_cooled_override);

        // On an air-cooled platform WITH the explicit acknowledgement the
        // pure decision activates with the override variant (matching what the
        // daemon passes: platform_looks_air_cooled = true).
        use dcentrald_thermal::immersion::ImmersionDecision;
        assert_eq!(
            reparsed.thermal.immersion.decide(true),
            ImmersionDecision::ActivatedAirCooledOverride
        );
        assert!(reparsed.thermal.immersion.decide(true).fans_bypassed());
    }

    #[test]
    fn thermal_immersion_enabled_without_ack_refuses_on_air_cooled() {
        // The fail-closed contract the daemon relies on: enabled WITHOUT the
        // air-cooled acknowledgement must REFUSE on an air-cooled platform
        // (platform_looks_air_cooled = true, which is what the daemon passes
        // for every current control board) — fans stay managed.
        let toml_text = r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[thermal.immersion]
enabled = true
"#;
        let cfg: DcentraldConfig = toml::from_str(toml_text).expect("must parse");
        assert!(cfg.thermal.immersion.enabled);
        assert!(!cfg.thermal.immersion.acknowledge_air_cooled_override);
        use dcentrald_thermal::immersion::ImmersionDecision;
        assert_eq!(
            cfg.thermal.immersion.decide(true),
            ImmersionDecision::RefusedAirCooled
        );
        assert!(
            !cfg.thermal.immersion.decide(true).fans_bypassed(),
            "an air-cooled unit without acknowledgement must KEEP fan management"
        );
    }

    #[test]
    fn thermal_immersion_round_trips_through_dcentrald_config_load() {
        // Faithfully exercise the file-backed `DcentraldConfig::load` path
        // (read_to_string → from_str → normalize_legacy_fields → validate) for
        // an explicit `[thermal.immersion]` section, and confirm a config with
        // NO immersion section loads identically to the default (byte-identical
        // off path) through the SAME entrypoint.
        let dir = std::env::temp_dir();
        let pid = std::process::id();

        // (a) Config WITHOUT [thermal.immersion] → default (off) after load.
        let off_path = dir.join(format!("dcentrald_immersion_off_{pid}.toml"));
        std::fs::write(
            &off_path,
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"
"#,
        )
        .expect("write off-config temp file");
        let off_cfg = DcentraldConfig::load(off_path.to_str().expect("utf8 path"))
            .expect("off config must load");
        assert_eq!(
            off_cfg.thermal.immersion,
            dcentrald_thermal::immersion::ImmersionConfig::default(),
            "load() of a config without [thermal.immersion] must yield the default (off) — byte-identical path"
        );
        let _ = std::fs::remove_file(&off_path);

        // (b) Config WITH an enabled+acknowledged [thermal.immersion] → both
        // flags survive the full load() path and validate() still passes.
        let on_path = dir.join(format!("dcentrald_immersion_on_{pid}.toml"));
        std::fs::write(
            &on_path,
            r#"
[pool]
url = "stratum+tcp://public-pool.io:21496"
worker = "test"

[thermal.immersion]
enabled = true
acknowledge_air_cooled_override = true
"#,
        )
        .expect("write on-config temp file");
        let on_cfg = DcentraldConfig::load(on_path.to_str().expect("utf8 path"))
            .expect("immersion-enabled config must load + validate");
        assert!(on_cfg.thermal.immersion.enabled);
        assert!(on_cfg.thermal.immersion.acknowledge_air_cooled_override);
        let _ = std::fs::remove_file(&on_path);
    }
}

/// Off-grid / Direct DC power configuration.
///
/// Enables voltage-based frequency curtailment for battery-powered mining.
/// Protects batteries from deep discharge and maximizes solar utilization.
/// Replaces $1K-2.5K hardware products (100 Acres Ranch, Gridless) with firmware.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OffGridConfig {
    /// Enable off-grid voltage monitoring and curtailment.
    #[serde(default)]
    pub enabled: bool,

    /// Battery chemistry preset (determines voltage thresholds).
    /// Options: "lifepo4_48v", "lifepo4_24v", "lifepo4_12v",
    ///          "lead_acid_48v", "lead_acid_24v", "lead_acid_12v", "custom".
    #[serde(default = "default_battery_preset")]
    pub battery_preset: String,

    /// ADC backend for voltage monitoring.
    /// Inline JSON: { "type": "ina226", "i2c_bus": 0, "i2c_addr": 64 }
    /// Or: { "type": "simulated", "voltage_v": 52.0 }
    #[serde(default)]
    pub adc: Option<dcentrald_hal::adc::AdcBackendConfig>,

    /// Frequency step per adjustment (MHz). Default: 25.
    #[serde(default = "default_freq_step")]
    pub freq_step_mhz: u16,

    /// Minimum frequency floor (MHz). Never go below this. Default: 200.
    #[serde(default = "default_min_freq")]
    pub min_frequency_mhz: u16,

    /// Control loop interval in milliseconds. Default: 2000.
    #[serde(default = "default_offgrid_interval")]
    pub loop_interval_ms: u64,

    /// Custom voltage thresholds (override battery preset).
    #[serde(default)]
    pub custom_critical_v: Option<f32>,
    #[serde(default)]
    pub custom_low_v: Option<f32>,
    #[serde(default)]
    pub custom_high_v: Option<f32>,
    #[serde(default)]
    pub custom_full_v: Option<f32>,
    #[serde(default)]
    pub custom_recovery_v: Option<f32>,
}

/// Scheduled (time-of-day) curtailment for off-peak / demand-response /
/// quiet-night operation.
///
/// This is the missing *time-scheduled* driver of the shared
/// `dcentrald_thermal::curtailment::CurtailmentController`. The off-grid and
/// solar paths already drive that controller from battery voltage / solar
/// surplus; this adds an operator-configured daily window (e.g. a utility
/// off-peak demand-response window, or a quiet-night curtailment block) during
/// which the daemon puts the miner into the controller's low-power sleep
/// (~25 W: hash boards de-energized, fans dropped to the controller's
/// `sleep_fan_pwm`), then wakes it again when the window ends.
///
/// **Safety contract (load-bearing):** curtailment only ever moves in the
/// safe direction — it CUTS hash power (cut-hash-before-noise) and drops fans to
/// the low standby PWM. It NEVER raises fan speed and NEVER pushes power/hash
/// up. Entering the window calls `CurtailmentController::enter_sleep()`; leaving
/// it calls `wake()`. All hardware effects flow through the existing,
/// already-audited thermal-loop curtailment-state consumer (voltage disable on
/// sleep, voltage restore on wake), so the PWM-30 cap and fail-closed thermal
/// behaviour still bound everything.
///
/// **Default-OFF:** when the `[power.curtailment]` section is absent the whole
/// `Option` is `None` and the schedule driver is never spawned — byte-identical
/// to a build without this feature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurtailmentScheduleConfig {
    /// Enable the scheduled-curtailment driver.
    #[serde(default)]
    pub enabled: bool,

    /// Window start hour (0-23, 24h clock). At this hour the miner enters the
    /// low-power curtailment sleep. A window that wraps past midnight is
    /// supported (e.g. `start_hour = 22`, `end_hour = 6`).
    #[serde(default = "default_curtail_start")]
    pub start_hour: u8,

    /// Window end hour (0-23, 24h clock, exclusive). At this hour the miner
    /// wakes back to normal mining. If `end_hour <= start_hour` the window is
    /// treated as wrapping past midnight.
    #[serde(default = "default_curtail_end")]
    pub end_hour: u8,

    /// Driver poll cadence in seconds. The schedule only changes on hour
    /// boundaries, so a coarse cadence is plenty; default 60 s. Bounded
    /// `[5, 3600]` by `validate()`.
    #[serde(default = "default_curtail_interval")]
    pub poll_interval_s: u64,

    /// Operator's whole-hour UTC offset (e.g. `-5` for EST, `+1` for CET)
    /// applied to `start_hour`/`end_hour` so the curtailment window fires at the
    /// right LOCAL wall-clock time, not UTC. Default `0` (UTC) preserves the
    /// prior behavior for existing configs. Validated to `[-12, 14]`. FWSTAB-1.
    #[serde(default)]
    pub timezone_offset_hours: i8,
}

fn default_curtail_start() -> u8 {
    // Conservative inert-ish default; the section is disabled by default so this
    // is only the placeholder a freshly-enabled section would inherit.
    0
}
fn default_curtail_end() -> u8 {
    0
}
fn default_curtail_interval() -> u64 {
    60
}

impl Default for CurtailmentScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: default_curtail_start(),
            end_hour: default_curtail_end(),
            poll_interval_s: default_curtail_interval(),
            timezone_offset_hours: 0,
        }
    }
}

impl CurtailmentScheduleConfig {
    /// Pure window predicate: is `hour` (0-23) inside the configured
    /// curtailment window? Supports midnight-wrap (`start > end`). When
    /// `start == end` the window is empty (never curtails) — a deliberately
    /// safe no-op so a misconfigured/degenerate window can never strand the
    /// miner asleep.
    ///
    /// Pulled out as a free-standing pure fn so the schedule logic is unit
    /// testable without spawning the async driver.
    pub fn window_active(start_hour: u8, end_hour: u8, hour: u8) -> bool {
        if start_hour == end_hour {
            // Degenerate / empty window — never curtail.
            return false;
        }
        if start_hour < end_hour {
            hour >= start_hour && hour < end_hour
        } else {
            // Wraps past midnight, e.g. 22..06.
            hour >= start_hour || hour < end_hour
        }
    }

    /// Convenience wrapper using this config's own hours.
    pub fn is_active_at_hour(&self, hour: u8) -> bool {
        Self::window_active(self.start_hour, self.end_hour, hour)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SolarConfig {
    pub enabled: bool,
    pub inverter_brand: String,
    pub api_endpoint: String,
    pub api_key: String,
    pub solar_only_mode: bool,
    pub base_load_watts: u32,
    pub battery_threshold_pct: u8,
    pub battery_wake_hysteresis_pct: u8,
    pub provider_max_sample_age_ms: u64,
    pub provider_failure_hysteresis_samples: u8,
    pub hybrid_import_deadband_watts: u32,
    pub manual_production_watts: u32,
    pub manual_site_load_watts: u32,
    pub manual_battery_soc_pct: Option<f32>,
}

impl Default for SolarConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            inverter_brand: "manual".to_string(),
            api_endpoint: String::new(),
            api_key: String::new(),
            solar_only_mode: false,
            base_load_watts: 500,
            battery_threshold_pct: 20,
            battery_wake_hysteresis_pct: 3,
            provider_max_sample_age_ms: 60_000,
            provider_failure_hysteresis_samples: 1,
            hybrid_import_deadband_watts: 75,
            manual_production_watts: 0,
            manual_site_load_watts: 0,
            manual_battery_soc_pct: None,
        }
    }
}

fn default_battery_preset() -> String {
    "lifepo4_48v".to_string()
}
fn default_freq_step() -> u16 {
    25
}
fn default_min_freq() -> u16 {
    200
}
fn default_offgrid_interval() -> u64 {
    2000
}

impl Default for OffGridConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            battery_preset: default_battery_preset(),
            adc: None,
            freq_step_mhz: default_freq_step(),
            min_frequency_mhz: default_min_freq(),
            loop_interval_ms: default_offgrid_interval(),
            custom_critical_v: None,
            custom_low_v: None,
            custom_high_v: None,
            custom_full_v: None,
            custom_recovery_v: None,
        }
    }
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            target_watts: 0,
            psu_bypass: false,
            legacy_mode: None,
            max_watts: default_max_watts(),
            circuit_capacity_watts: default_circuit_capacity(),
            circuit_voltage_v: None,
            circuit_amperage_a: None,
            source_profile: None,
            psu_override: None,
            offgrid: None,
            solar: None,
            curtailment: None,
            calibration: None,
        }
    }
}

impl PowerConfig {
    fn normalize_legacy_fields(&mut self) -> Result<()> {
        let Some(mode) = self.legacy_mode.take() else {
            return Ok(());
        };

        match mode.trim().to_ascii_lowercase().as_str() {
            "" => Ok(()),
            "bypass" => {
                self.psu_bypass = true;
                Ok(())
            }
            other => anyhow::bail!(
                "power.mode = {:?} is no longer supported; use power.psu_bypass or newer power fields",
                other
            ),
        }
    }
}

fn default_max_watts() -> u32 {
    1500
}

fn default_circuit_capacity() -> u32 {
    1800 // 120V × 15A
}

// ---------------------------------------------------------------------------
// Thermal configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThermalConfig {
    /// Normal operating temperature target (celsius).
    #[serde(default = "default_target_temp")]
    pub target_temp_c: u8,

    /// Begin frequency throttling at this temperature.
    #[serde(default = "default_hot_temp")]
    pub hot_temp_c: u8,

    /// Emergency shutdown threshold.
    #[serde(default = "default_dangerous_temp")]
    pub dangerous_temp_c: u8,

    /// Minimum fan duty cycle (0 = hardware min ~900 RPM).
    #[serde(default)]
    pub fan_min_pwm: u8,

    /// Maximum fan duty cycle (100 = ~5940 RPM).
    #[serde(default = "default_fan_max_pwm")]
    pub fan_max_pwm: u8,

    /// Idle / non-mining fan duty cycle (am2 low-idle park command path).
    ///
    /// Used by the am2 management-only park paths (`enter_management_only_idle`
    /// / `enter_management_only`) to drive the uio16-mmap `FanController` to a
    /// low PWM command when the unit is parked and NOT mining (devmem can't reach
    /// the am2 fan IP — see
    /// ). Default
    /// is `dcentrald_hal::fan::PWM_QUIET_BOOT` (10). This is only ever driven
    /// DOWN: `validate()` fail-closes if it exceeds `fan_max_pwm` or
    /// `PWM_SAFETY_MAX` (30), and the runtime setter additionally clamps
    /// `min(fan_max_pwm).min(PWM_SAFETY_MAX)`. S9/am1 + am3 paths never read
    /// this field (the low-idle call is am2-gated).
    #[serde(default = "default_fan_idle_pwm")]
    pub fan_idle_pwm: u8,

    /// Thermal control loop cadence in seconds.
    /// BM1387 thermal time constant is 2-4 s; the default of 5 s is tolerable
    /// at home-mode 500 MHz / PWM 30 / dangerous 80, but a shorter cadence
    /// (≤2 s) responds faster to sudden load transients. Change to 2.0 only
    /// after live-testing the PID response on target hardware.
    #[serde(default = "default_pid_interval_s")]
    pub pid_interval_s: f32,

    /// PWM hysteresis band in celsius — freq/fan state changes require the
    /// temperature to cross by this much to avoid oscillation around a
    /// threshold. Default 3 matches the historical hard-coded value.
    /// Matches `dcentrald-thermal::profiles::ThermalProfile::hysteresis_c` (u8).
    #[serde(default = "default_hysteresis_c")]
    pub hysteresis_c: u8,

    /// Night mode configuration.
    #[serde(default)]
    pub night_mode: NightModeConfig,

    /// LuxOS-shape thermal supervisor (RE-005 / Wave-E `ThermalSupervisor`).
    /// **Default-off** (`[thermal.supervisor].enabled = false`). When false,
    /// the existing `ThermalController` is the sole thermal authority and the
    /// runtime path is byte-identical to pre-Wave-G. When true (operator
    /// opt-in, Wave-H live-soak gated), the daemon thermal loop drives the
    /// supervisor's 6-layer FSM alongside the controller and reconciles via
    /// `dcentrald_thermal::controller::reconcile_with_supervisor`
    /// (strongest-safety-wins; the supervisor can only make the response
    /// more conservative, never weaken the controller's fail-closed floor).
    #[serde(default)]
    pub supervisor: dcentrald_thermal::supervisor::ThermalSupervisorConfig,

    /// Immersion / hydro cooling mode (W8 parity gap vs LuxOS `immersionswitch`
    /// / VNish `cooling_mode = "immersion"`). **Default-off**
    /// (`[thermal.immersion].enabled = false`). When false the daemon thermal
    /// loop is byte-identical to the pre-immersion path: the controller's
    /// `immersion_active()` returns false and every HAL fan write fires as
    /// before. When true (operator opt-in) the daemon calls
    /// `ThermalController::enable_immersion(&immersion, platform_looks_air_cooled)`
    /// right after constructing the controller; on a platform that looks
    /// air-cooled the controller REFUSES (fail-closed, keeps fan management)
    /// unless `acknowledge_air_cooled_override` is also set. When immersion is
    /// active the daemon SKIPS the HAL fan write in the `SetFanPwm` /
    /// `ThrottleAndFan` arms (no chassis fans on an immersion rig); the
    /// over-temp HASH-CUT safety net (`EmergencyShutdown` / `FanFailure`) is
    /// UNCHANGED. See `dcentrald_thermal::immersion::ImmersionConfig`.
    #[serde(default)]
    pub immersion: dcentrald_thermal::immersion::ImmersionConfig,

    /// Per-chip die-temperature calibration (R-13, BM1362 / am2-s19jpro-zynq).
    /// **Default-off** (`[thermal.die_temp_calibration].enabled = false`) — the
    /// `die_temp_calibration_enabled` gate. LuxOS calibrates each chip's on-die
    /// ADC against an absolute PCB sensor at a cold baseline; DCENT_OS reported
    /// the RAW die reading (`soc_die_fallback`). When false the am2 hybrid
    /// supervisor uses the raw XADC die temp exactly as before. When true
    /// (operator opt-in, OR the `DCENT_AM2_DIE_TEMP_CALIBRATION` env override)
    /// the supervisor captures a cold baseline at the pre-stratum poll and
    /// applies the per-chip offset — but the correction is fail-safe: a missing
    /// / implausible / not-cold baseline is rejected (falls back to raw), and
    /// the safety-facing reading is guaranteed NEVER below raw, so calibration
    /// can never suppress an over-temp trip. See
    /// `dcentrald_thermal::die_calibration::DieCalibrationConfig`.
    #[serde(default)]
    pub die_temp_calibration: dcentrald_thermal::die_calibration::DieCalibrationConfig,
}

impl Default for ThermalConfig {
    fn default() -> Self {
        Self {
            target_temp_c: default_target_temp(),
            hot_temp_c: default_hot_temp(),
            dangerous_temp_c: default_dangerous_temp(),
            fan_min_pwm: 0,
            fan_max_pwm: default_fan_max_pwm(),
            fan_idle_pwm: default_fan_idle_pwm(),
            pid_interval_s: default_pid_interval_s(),
            hysteresis_c: default_hysteresis_c(),
            night_mode: NightModeConfig::default(),
            supervisor: dcentrald_thermal::supervisor::ThermalSupervisorConfig::default(),
            immersion: dcentrald_thermal::immersion::ImmersionConfig::default(),
            die_temp_calibration: dcentrald_thermal::die_calibration::DieCalibrationConfig::default(
            ),
        }
    }
}

fn default_target_temp() -> u8 {
    55
}

fn default_hot_temp() -> u8 {
    65
}

fn default_dangerous_temp() -> u8 {
    75
}

fn default_fan_max_pwm() -> u8 {
    30 // Home mining default fan cap; acoustic proof requires tach/RPM.
}

fn default_fan_idle_pwm() -> u8 {
    // Reuse the low home-idle setpoint. Kept in lockstep with
    // `dcentrald_hal::fan::PWM_QUIET_BOOT` (10) — the same value the cold-boot
    // / hard-stop paths use. Idle/non-mining duty for the am2 low-idle park;
    // AM2/XIL acoustic proof comes from tach/RPM, not PWM echo.
    dcentrald_hal::fan::PWM_QUIET_BOOT
}

fn default_pid_interval_s() -> f32 {
    5.0 // matches historical hard-coded cadence at daemon.rs:3363
}

fn default_hysteresis_c() -> u8 {
    3 // matches historical hard-coded band at daemon.rs:3313
}

/// Night mode reduces noise during sleeping hours.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NightModeConfig {
    /// Whether night mode is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Start hour (24h format, e.g., 22 = 10 PM).
    #[serde(default = "default_night_start")]
    pub start_hour: u8,

    /// End hour (24h format, e.g., 7 = 7 AM).
    #[serde(default = "default_night_end")]
    pub end_hour: u8,

    /// Reduced fan ceiling during night hours.
    #[serde(default = "default_night_fan_pwm")]
    pub max_fan_pwm: u8,

    /// Reduced frequency during night hours.
    #[serde(default = "default_night_frequency")]
    pub max_frequency_mhz: u16,

    /// Operator's whole-hour UTC offset (e.g. `-5` for EST) applied to
    /// `start_hour`/`end_hour` so the quiet window fires at the right LOCAL
    /// wall-clock time, not UTC. Default `0` (UTC) preserves the prior behavior
    /// for existing configs. Validated to `[-12, 14]`. FWSTAB-1.
    #[serde(default)]
    pub timezone_offset_hours: i8,
}

impl Default for NightModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: default_night_start(),
            end_hour: default_night_end(),
            max_fan_pwm: default_night_fan_pwm(),
            max_frequency_mhz: default_night_frequency(),
            timezone_offset_hours: 0,
        }
    }
}

fn default_night_start() -> u8 {
    22
}

fn default_night_end() -> u8 {
    7
}

fn default_night_fan_pwm() -> u8 {
    30
}

fn default_night_frequency() -> u16 {
    400
}

// ---------------------------------------------------------------------------
// API configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// CGMiner-compatible TCP API port.
    #[serde(default = "default_cgminer_port")]
    pub cgminer_port: u16,

    /// HTTP REST API and dashboard port.
    #[serde(default = "default_http_port")]
    pub http_port: u16,

    /// HTTP REST API and dashboard bind address.
    ///
    /// Default stays LAN-visible (`0.0.0.0`) for existing deployed dashboard
    /// behavior. Operators that tunnel through SSH or a local supervisor can set
    /// `127.0.0.1` to keep the HTTP surface loopback-only.
    #[serde(default = "default_http_bind")]
    pub http_bind: String,

    /// Enable WebSocket on /ws (same port as HTTP).
    #[serde(default = "default_true")]
    pub websocket: bool,

    /// Enable one-time WebSocket auth tickets.
    ///
    /// Default false preserves the existing `?token=` browser compatibility path.
    /// When true, authenticated clients may POST `/api/auth/ws-ticket` and then
    /// connect to `/ws?ticket=...`; tickets are short-lived and one-use.
    #[serde(default)]
    pub websocket_tickets: bool,

    /// Legacy configs still persist these dashboard-auth fields. Current API
    /// auth wiring no longer reads them directly, but we must keep accepting
    /// them so older flashed units and shipped templates still load.
    #[serde(default, rename = "auth_enabled", skip_serializing)]
    _legacy_auth_enabled: Option<bool>,

    #[serde(default, rename = "auth_password", skip_serializing)]
    _legacy_auth_password: Option<String>,

    /// Bind CGMiner API to LAN (0.0.0.0) instead of localhost (127.0.0.1).
    /// Enable for pyasic/hass-miner remote monitoring. Default false for security.
    /// SECURITY: CGMiner protocol has NO authentication — LAN exposure allows
    /// any client to send addpool/switchpool commands and redirect hashrate.
    #[serde(default)]
    pub cgminer_bind_lan: bool,

    /// Allow MUTATING CGMiner/LuxOS verbs from NON-loopback peers.
    /// SECURITY (API-1): `cgminer_bind_lan=true` is a documented monitoring
    /// opt-in, but the same TCP listener also serves mutating LuxOS verbs
    /// gated only by a credential-less `logon` session. Without this flag,
    /// mutations stay loopback-only even when the listener is LAN-bound (reads
    /// stay open). Default false (fail-closed); set true ONLY on a trusted LAN.
    #[serde(default)]
    pub cgminer_lan_writes: bool,

    /// Supremacy S5.1 — gRPC server config. Default OFF until soak-proven.
    /// When `[api.grpc] enabled = true`, dcentrald spawns a tonic server on
    /// `port` (default 50051) alongside the REST/CGMiner APIs. See
    /// `dcentrald-api-grpc` for the v1 proto contract.
    #[serde(default)]
    pub grpc: GrpcApiConfig,

    /// Require authentication for /metrics endpoint.
    /// Default true so production images fail closed.
    #[serde(default = "default_true")]
    pub metrics_require_auth: bool,

    /// Optional upstream cgminer endpoint used by proxy/overlay mode when
    /// bosminer owns hardware telemetry.
    #[serde(default = "default_cgminer_scrape_url")]
    pub cgminer_scrape_url: String,

    /// W13.D1: expose `/api/boot/timeline` (dev-mode diagnostics).
    /// Default false. The dashboard's diagnostics tab flips this on
    /// once Hacker mode is engaged. Returning the timeline by default
    /// would leak per-boot timing fingerprints to LAN scanners.
    #[serde(default)]
    pub expose_boot_timeline: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            cgminer_port: default_cgminer_port(),
            http_port: default_http_port(),
            http_bind: default_http_bind(),
            websocket: true,
            websocket_tickets: false,
            _legacy_auth_enabled: None,
            _legacy_auth_password: None,
            cgminer_bind_lan: false,
            cgminer_lan_writes: false,
            grpc: GrpcApiConfig::default(),
            metrics_require_auth: true,
            cgminer_scrape_url: default_cgminer_scrape_url(),
            expose_boot_timeline: false,
        }
    }
}

/// Supremacy S5.1 — gRPC API server config. Scaffold-priority: default OFF.
/// When `enabled = true`, `dcentrald/src/main.rs` spawns the tonic server on
/// `port` (default 50051) alongside REST + CGMiner. Reflection is on by
/// default (no security cost; clients reflect over services already exposed).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrpcApiConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_grpc_port")]
    pub port: u16,

    #[serde(default = "default_grpc_bind")]
    pub bind: String,

    #[serde(default = "default_true")]
    pub reflection: bool,
}

impl Default for GrpcApiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: default_grpc_port(),
            bind: default_grpc_bind(),
            reflection: true,
        }
    }
}

fn default_grpc_port() -> u16 {
    50051
}

fn default_grpc_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_cgminer_port() -> u16 {
    4028
}

fn default_http_port() -> u16 {
    8080
}

fn default_http_bind() -> String {
    "0.0.0.0".to_string()
}

fn default_cgminer_scrape_url() -> String {
    "http://127.0.0.1:4028".to_string()
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Donation configuration — voluntary 2% default, fully transparent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DonationConfig {
    /// Whether donation is enabled. Default: true.
    #[serde(default = "default_donation_enabled")]
    pub enabled: bool,

    /// Donation percentage (0.0 to 5.0). Default: 2.0.
    #[serde(default = "default_donation_percent")]
    pub percent: f32,

    /// Donation pool URL.
    #[serde(default = "default_donation_pool")]
    pub pool_url: String,

    /// Donation worker name.
    #[serde(default = "default_donation_worker")]
    pub worker: String,

    /// Donation pool password.
    #[serde(default = "default_donation_password")]
    pub password: String,

    /// Enable a visible backup donation route if the primary donation endpoint
    /// is unreachable during a donation window.
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,

    /// Backup donation pool URL. Used only for donation windows.
    #[serde(default = "default_donation_fallback_pool")]
    pub fallback_pool_url: String,

    /// Backup donation worker name.
    #[serde(default = "default_donation_fallback_worker")]
    pub fallback_worker: String,

    /// Backup donation pool password.
    #[serde(default = "default_donation_password")]
    pub fallback_password: String,

    /// Cycle duration in seconds. Default: 3600 (1 hour).
    #[serde(default = "default_donation_cycle")]
    pub cycle_duration_s: u64,
}

impl Default for DonationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            percent: 2.0,
            pool_url: default_donation_pool(),
            worker: default_donation_worker(),
            password: default_donation_password(),
            fallback_enabled: true,
            fallback_pool_url: default_donation_fallback_pool(),
            fallback_worker: default_donation_fallback_worker(),
            fallback_password: default_donation_password(),
            cycle_duration_s: 3600,
        }
    }
}

fn default_donation_enabled() -> bool {
    true
}
fn default_donation_percent() -> f32 {
    2.0
}
fn default_donation_pool() -> String {
    String::from("stratum+tcp://pool.d-central.tech:3333")
}
fn default_donation_worker() -> String {
    String::from("DungeonMaster")
}
fn default_donation_fallback_pool() -> String {
    String::from("stratum+tcp://stratum.braiins.com:3333")
}
fn default_donation_fallback_worker() -> String {
    String::from("DungeonMaster")
}
fn default_donation_password() -> String {
    "x".to_string()
}
fn default_donation_cycle() -> u64 {
    3600
}

// ---------------------------------------------------------------------------
// MQTT / Home Assistant integration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttConfig {
    /// Whether MQTT publishing is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// MQTT broker URL (e.g., "mqtt://localhost:1883").
    #[serde(default = "default_mqtt_broker")]
    pub broker: String,

    /// Topic prefix for all published messages.
    #[serde(default = "default_mqtt_prefix")]
    pub topic_prefix: String,

    /// Enable Home Assistant MQTT auto-discovery.
    #[serde(default = "default_true")]
    pub discovery: bool,

    /// MQTT broker username (optional).
    #[serde(default)]
    pub username: Option<String>,

    /// MQTT broker password (optional).
    #[serde(default)]
    pub password: Option<String>,

    /// Publish interval in seconds (default: 5).
    #[serde(default = "default_mqtt_interval")]
    pub publish_interval_s: u16,
}

fn default_mqtt_interval() -> u16 {
    5
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            broker: default_mqtt_broker(),
            topic_prefix: default_mqtt_prefix(),
            discovery: true,
            username: None,
            password: None,
            publish_interval_s: default_mqtt_interval(),
        }
    }
}

fn default_mqtt_broker() -> String {
    "mqtt://localhost:1883".to_string()
}

fn default_mqtt_prefix() -> String {
    "dcentrald".to_string()
}

// ---------------------------------------------------------------------------
// Webhook alert configuration
// ---------------------------------------------------------------------------

/// Webhook alert configuration for push notifications.
///
/// When enabled, dcentrald POSTs JSON alert payloads to the configured URL
/// on critical events (emergency shutdown, fan failure, pool disconnect, etc.).
/// Compatible with Telegram bots, Discord webhooks, PagerDuty, ntfy.sh, etc.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    /// Enable webhook notifications.
    #[serde(default)]
    pub enabled: bool,

    /// URL to POST alert JSON to (e.g., Discord webhook, Telegram bot, ntfy.sh).
    #[serde(default)]
    pub url: String,

    /// Which events to send. Empty = all events.
    /// Supported: "emergency_shutdown", "fan_failure", "pool_disconnected",
    /// "mining_stopped", "hashboard_offline", "thermal_restart",
    /// "hashrate_degraded".
    #[serde(default)]
    pub events: Vec<String>,

    /// Delivery channel format. Default `generic` keeps the historical
    /// `{ miner, timestamp, alert }` POST body (byte-identical when unset);
    /// `discord` / `slack` / `telegram` reshape the body so DCENT_OS can deliver
    /// natively (no relay). Default-OFF: leaving this unset changes nothing.
    #[serde(default)]
    pub format: dcentrald_api::webhook::WebhookFormat,

    /// Telegram bot token (only used when `format = "telegram"`). SECRET — it is
    /// masked in the REST GET response and never logged.
    #[serde(default)]
    pub telegram_bot_token: Option<String>,

    /// Telegram chat id to deliver to (only used when `format = "telegram"`).
    #[serde(default)]
    pub telegram_chat_id: Option<String>,
}

// ---------------------------------------------------------------------------
// PSU configuration — APW121215a framed-I2C PSU on am2 / S19j Pro
// ---------------------------------------------------------------------------
//
// Phase 5 learning (investigation/10-psu-watchdog.md, 12-bosminer-startup-timeline.md):
// the APW121215a PSU on S19j Pro am2 runs at a 15.2 V rail and regulates down
// to per-chain 13.7 V via the dsPIC. Bosminer heartbeats the PSU at 1 Hz and
// arms a ~30 s hardware watchdog via opcode 0x81. These parameters now live in
// a dedicated [psu] section instead of being hard-coded in the am2 path so
// other PSU families (APW9/APW12) can override them without source edits.
//
// Consumers (Agent B in s19j_hybrid_mining.rs): read `config.psu.voltage_mv`
// as the rail target (NOT the per-chain value — that stays in
// `config.mining.voltage_mv` and is the dsPIC's responsibility).

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PsuConfig {
    /// Human-readable PSU model hint from the board template.
    #[serde(default = "PsuConfig::default_model")]
    pub model: String,

    /// Target rail voltage in millivolts (PSU level, NOT per-chain).
    /// APW121215a runs at 15200 mV; per-chain voltage is regulated by the dsPIC.
    #[serde(default = "PsuConfig::default_voltage_mv")]
    pub voltage_mv: u32,

    /// Heartbeat cadence in Hz. APW121215a watchdog kicks in if heartbeats
    /// pause for ≥30s, so 1 Hz is the bosminer-verified baseline.
    #[serde(default = "PsuConfig::default_heartbeat_hz")]
    pub heartbeat_hz: u8,

    /// When true, daemon arms the PSU hardware watchdog (~30s timeout) via
    /// opcode 0x81. Recommended for production; disable for lab bring-up only.
    #[serde(default = "PsuConfig::default_watchdog_enabled")]
    pub watchdog_enabled: bool,

    /// I2C address of the PSU on /dev/i2c-0 (Zynq am2: 0x10 for APW121215a).
    #[serde(default = "PsuConfig::default_i2c_address")]
    pub i2c_address: u8,

    /// APW transport for startup on experimental targets.
    ///
    /// - `kernel_i2c` (default): use `/dev/i2c-0`
    /// - `gpio_bitbang`: use `a lab unit`-style `gpio895/896`
    #[serde(default = "PsuConfig::default_transport")]
    pub transport: String,

    /// Optional `PWR_CONTROL` GPIO override for the am2 PSU gate.
    ///
    /// Supported forms:
    /// - `label:PWR_CONTROL` (preferred)
    /// - `gpio:901`
    /// - `901`
    ///
    /// When unset, the am2 path resolves the `PWR_CONTROL` device-tree label
    /// at runtime and falls back to the strongest live candidate (`gpio901`).
    #[serde(default)]
    pub pwr_control_gpio: Option<String>,

    /// Optional power-target defaults for board templates. Autotuner/power
    /// policy may consume these in later waves; parsing them here keeps
    /// per-board Buildroot configs schema-valid today.
    #[serde(default)]
    pub power_target_w: Option<u32>,

    #[serde(default)]
    pub power_step_w: Option<u32>,

    #[serde(default)]
    pub hashrate_target_ths: Option<f64>,

    #[serde(default)]
    pub hashrate_step_ths: Option<f64>,
}

impl PsuConfig {
    fn default_model() -> String {
        "APW121215a".to_string()
    }
    fn default_voltage_mv() -> u32 {
        15200
    }
    fn default_heartbeat_hz() -> u8 {
        1
    }
    fn default_watchdog_enabled() -> bool {
        true
    }
    fn default_i2c_address() -> u8 {
        0x10
    }
    fn default_transport() -> String {
        "kernel_i2c".to_string()
    }
}

impl Default for PsuConfig {
    fn default() -> Self {
        Self {
            model: Self::default_model(),
            voltage_mv: Self::default_voltage_mv(),
            heartbeat_hz: Self::default_heartbeat_hz(),
            watchdog_enabled: Self::default_watchdog_enabled(),
            i2c_address: Self::default_i2c_address(),
            transport: Self::default_transport(),
            pwr_control_gpio: None,
            power_target_w: None,
            power_step_w: None,
            hashrate_target_ths: None,
            hashrate_step_ths: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Hashboard configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashboardConfig {
    /// EEPROM parser selector.
    ///
    /// `x21_aes` identifies the BHB56xxx/BHB56902 encrypted EEPROM family seen
    /// on S19k Pro .78. DCENT_OS deliberately does not ship a hidden LuxOS key;
    /// any decrypt key or decoded reference must come from explicit operator
    /// configuration.
    #[serde(default = "HashboardConfig::default_eeprom_parser")]
    pub eeprom_parser: String,

    /// Optional operator-provided key file for lab decoding. Production images
    /// should leave this unset and use the raw preamble plus reference metadata.
    #[serde(default)]
    pub eeprom_key_file: Option<String>,

    /// Optional decoded metadata reference captured from bosminer/luxminer.
    #[serde(default)]
    pub decoded_reference_file: Option<String>,
}

impl HashboardConfig {
    fn default_eeprom_parser() -> String {
        "auto".to_string()
    }

    pub fn validate(&self) -> Result<()> {
        match self.eeprom_parser.as_str() {
            "auto" | "bhb42" | "x21_aes" => Ok(()),
            other => anyhow::bail!(
                "hashboard.eeprom_parser ('{}') must be one of: auto, bhb42, x21_aes",
                other
            ),
        }
    }
}

impl Default for HashboardConfig {
    fn default() -> Self {
        Self {
            eeprom_parser: Self::default_eeprom_parser(),
            eeprom_key_file: None,
            decoded_reference_file: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Watchdog configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchdogConfig {
    /// Whether watchdog is enabled.
    ///
    /// SAFETY (wave 8, 2026-04-28): Default flipped to `true`. With hashboards live,
    /// a panicked or hung daemon previously left the chain at full voltage with no
    /// thermal supervision — a documented thermal-runaway path. The hardware
    /// watchdog must reset the SoC if the daemon stops kicking it.
    /// Per-board operators can still opt out by explicitly setting `enabled = false`
    /// in the board's `[watchdog]` section.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Watchdog timeout in seconds.
    #[serde(default = "default_watchdog_timeout")]
    pub timeout_s: u32,

    /// How often to kick the watchdog (seconds).
    #[serde(default = "default_kick_interval")]
    pub kick_interval_s: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_s: default_watchdog_timeout(),
            kick_interval_s: default_kick_interval(),
        }
    }
}

fn default_watchdog_timeout() -> u32 {
    30
}

fn default_kick_interval() -> u32 {
    5
}

// ---------------------------------------------------------------------------
// Hash-on-disconnect configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HashOnDisconnectConfig {
    /// Keep mining with last job when pool disconnects.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Stop mining after this many seconds of disconnect.
    #[serde(default = "default_max_ntime")]
    pub max_ntime_advance_s: u32,
}

impl Default for HashOnDisconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_ntime_advance_s: default_max_ntime(),
        }
    }
}

fn default_max_ntime() -> u32 {
    7200
}

// ---------------------------------------------------------------------------
// Mode configuration (home / standard / hacker)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeConfig {
    /// Active operating mode: "home", "standard", or "hacker".
    #[serde(default = "default_mode")]
    pub active: String,

    /// Home mode settings.
    #[serde(default)]
    pub home: HomeModeConfig,

    /// Hacker mode settings.
    #[serde(default)]
    pub hacker: HackerModeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyHeaterConfig {
    #[serde(default)]
    target_watts: u32,

    #[serde(default)]
    night_mode: bool,

    #[serde(default = "default_night_start")]
    night_start_hour: u8,

    #[serde(default = "default_night_end")]
    night_end_hour: u8,

    #[serde(default)]
    night_target_watts: u32,
}

impl Default for ModeConfig {
    fn default() -> Self {
        Self {
            active: default_mode(),
            home: HomeModeConfig::default(),
            hacker: HackerModeConfig::default(),
        }
    }
}

// `mode_display_label("home" | "standard" | "hacker") -> "Space Heater" | "Mining" | "Hacker"`
// lives in `dcentrald-api::rest` where it is consumed. The rule
// is that backend enum values stay unchanged for
// API / config compatibility; any human-facing surface must map through the
// display helper. If a second caller inside the `dcentrald` crate ever needs
// the same mapping, promote it to `dcent-schema` instead of duplicating it.

impl std::fmt::Display for ModeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.active)
    }
}

fn default_mode() -> String {
    "standard".to_string()
}

/// Home mode configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeModeConfig {
    /// Power preset: "whisper", "low", "medium", "high", "max".
    /// The legacy "whisper" id means the lowest-power preset, not measured dB.
    #[serde(default = "default_home_preset")]
    pub preset: String,

    /// Exact wattage target (0 = use preset).
    #[serde(default)]
    pub target_watts: u32,

    /// Temperature source: "manual", "sensor", "homeassistant", "userinput".
    #[serde(default = "default_temp_source")]
    pub temp_source: String,

    /// Default room temperature for manual/userinput modes.
    #[serde(default = "default_room_temp")]
    pub room_temp_c: f32,

    /// Electricity rate in $/kWh for cost calculations.
    #[serde(default = "default_electricity_rate")]
    pub electricity_rate: f32,

    /// Currency for cost display.
    #[serde(default = "default_currency")]
    pub currency: String,

    /// Whether the operator has explicitly confirmed an electricity rate (e.g.
    /// at first-boot setup via `POST /api/setup/step-economics`). While `false`,
    /// `electricity_rate` above is the daemon DEFAULT guess — not an
    /// operator-confirmed value — so every cost/earnings surface must label
    /// itself "uncalibrated" rather than presenting the figure as truth. This is
    /// the single source of truth for that flag; the dashboard must read it back
    /// instead of guessing from its own localStorage.
    #[serde(default)]
    pub electricity_rate_calibrated: bool,

    /// External sensor configuration.
    #[serde(default)]
    pub sensor: HomeSensorConfig,

    /// Home Assistant integration configuration.
    #[serde(default)]
    pub homeassistant: HomeAssistantConfig,

    /// Night mode (Home-specific, separate from thermal night mode).
    #[serde(default)]
    pub night_mode: HomeNightModeConfig,

    /// Home-specific pool (defaults to solo mining).
    #[serde(default)]
    pub pool: HomePoolConfig,
}

impl Default for HomeModeConfig {
    fn default() -> Self {
        Self {
            preset: default_home_preset(),
            target_watts: 0,
            temp_source: default_temp_source(),
            room_temp_c: default_room_temp(),
            electricity_rate: default_electricity_rate(),
            currency: default_currency(),
            electricity_rate_calibrated: false,
            sensor: HomeSensorConfig::default(),
            homeassistant: HomeAssistantConfig::default(),
            night_mode: HomeNightModeConfig::default(),
            pool: HomePoolConfig::default(),
        }
    }
}

fn default_home_preset() -> String {
    "medium".to_string()
}

fn default_temp_source() -> String {
    "manual".to_string()
}

fn default_room_temp() -> f32 {
    21.0
}

fn default_electricity_rate() -> f32 {
    0.12
}

fn default_currency() -> String {
    "USD".to_string()
}

/// External sensor configuration for Home mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeSensorConfig {
    /// URL or device path for external temperature sensor.
    #[serde(default)]
    pub url: String,

    /// How often to poll the sensor (seconds).
    #[serde(default = "default_sensor_interval")]
    pub poll_interval_s: u16,
}

impl Default for HomeSensorConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            poll_interval_s: default_sensor_interval(),
        }
    }
}

fn default_sensor_interval() -> u16 {
    60
}

/// Home Assistant integration for Home mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeAssistantConfig {
    /// Home Assistant entity ID (e.g., "sensor.living_room_temperature").
    #[serde(default)]
    pub entity_id: String,
}

/// Night mode configuration specific to Home mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomeNightModeConfig {
    /// Whether Home mode night mode is active.
    #[serde(default)]
    pub enabled: bool,

    /// Start hour (24h format).
    #[serde(default = "default_night_start")]
    pub start_hour: u8,

    /// End hour (24h format).
    #[serde(default = "default_night_end")]
    pub end_hour: u8,

    /// Maximum fan PWM during night hours.
    #[serde(default = "default_night_fan_pwm")]
    pub max_fan_pwm: u8,

    /// Reduce power target by this percentage during night hours.
    #[serde(default = "default_power_reduction")]
    pub power_reduction_pct: u8,
}

impl Default for HomeNightModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            start_hour: default_night_start(),
            end_hour: default_night_end(),
            max_fan_pwm: default_night_fan_pwm(),
            power_reduction_pct: default_power_reduction(),
        }
    }
}

fn default_power_reduction() -> u8 {
    40
}

/// Pool configuration for Home mode (defaults to solo mining).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomePoolConfig {
    /// Pool URL for heater mode.
    #[serde(default = "default_home_pool_url")]
    pub url: String,

    /// Worker name.
    #[serde(default = "default_home_pool_worker")]
    pub worker: String,

    /// Pool password.
    #[serde(default = "default_pool_password")]
    pub password: String,
}

impl Default for HomePoolConfig {
    fn default() -> Self {
        Self {
            url: default_home_pool_url(),
            worker: default_home_pool_worker(),
            password: default_pool_password(),
        }
    }
}

fn default_home_pool_url() -> String {
    "stratum+tcp://solo.ckpool.org:3333".to_string()
}

fn default_home_pool_worker() -> String {
    "dcentos-home".to_string()
}

/// Hacker mode configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HackerModeConfig {
    /// Allow raw FPGA register read/write via API.
    #[serde(default = "default_true")]
    pub enable_raw_registers: bool,

    /// Allow raw I2C bus access via API.
    #[serde(default = "default_true")]
    pub enable_i2c_access: bool,

    /// Allow raw ASIC commands via API.
    #[serde(default = "default_true")]
    pub enable_asic_commands: bool,

    /// Allow PID parameter override via API.
    #[serde(default = "default_true")]
    pub enable_pid_override: bool,

    /// Maximum allowed frequency in hacker mode (MHz).
    #[serde(default = "default_hacker_max_freq")]
    pub max_frequency_mhz: u16,

    /// Minimum PIC value (= maximum voltage, ~9.1V at PIC value 50).
    #[serde(default = "default_hacker_max_voltage_pic")]
    pub max_voltage_pic: u8,

    /// Hacker mode thermal limit (absolute max, celsius).
    #[serde(default = "default_hacker_dangerous_temp")]
    pub dangerous_temp_override: u8,
}

impl Default for HackerModeConfig {
    fn default() -> Self {
        Self {
            enable_raw_registers: true,
            enable_i2c_access: true,
            enable_asic_commands: true,
            enable_pid_override: true,
            max_frequency_mhz: default_hacker_max_freq(),
            max_voltage_pic: default_hacker_max_voltage_pic(),
            dangerous_temp_override: default_hacker_dangerous_temp(),
        }
    }
}

fn default_hacker_max_freq() -> u16 {
    900
}

fn default_hacker_max_voltage_pic() -> u8 {
    50
}

fn default_hacker_dangerous_temp() -> u8 {
    85
}

// ---------------------------------------------------------------------------
// Stratum V2 configuration
// ---------------------------------------------------------------------------

/// Stratum V2 (SV2) specific configuration.
///
/// Controls SV2-specific behavior like encryption preferences,
/// pool authority verification, and channel type selection.
/// All fields have sane defaults — this entire section is optional in TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sv2Config {
    /// Prefer SV2 over V1 when both are available (for "auto" protocol mode).
    #[serde(default = "default_true")]
    pub prefer_sv2: bool,

    /// Pool authority public key for Noise_NX certificate validation.
    /// When set, the client verifies the pool's identity during handshake.
    /// When absent, any pool certificate is accepted (TOFU model).
    #[serde(default)]
    pub pool_authority_pubkey: Option<String>,

    /// SV2 channel type: "standard" (pool builds coinbase) or "extended"
    /// (miner builds coinbase, for solo mining or custom transactions).
    #[serde(default = "default_channel_type")]
    pub channel_type: String,
}

impl Default for Sv2Config {
    fn default() -> Self {
        Self {
            prefer_sv2: true,
            pool_authority_pubkey: None,
            channel_type: default_channel_type(),
        }
    }
}

fn default_channel_type() -> String {
    "standard".to_string()
}

// ---------------------------------------------------------------------------
// Job Declaration configuration (SV2 template construction via bitcoind)
// ---------------------------------------------------------------------------

/// Job Declaration Protocol (JDP) configuration for SV2.
///
/// When enabled, the miner connects to a local bitcoind to construct its own
/// block templates, enabling censorship resistance and custom transaction
/// selection. Requires SV2 extended channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobDeclarationConfig {
    /// Whether Job Declaration is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// JD operating mode: "coinbase_only" or "full_template".
    #[serde(default = "default_jd_mode")]
    pub mode: String,

    /// bitcoind JSON-RPC URL.
    #[serde(default = "default_bitcoind_url")]
    pub bitcoind_rpc_url: String,

    /// bitcoind RPC username.
    #[serde(default)]
    pub bitcoind_rpc_user: String,

    /// bitcoind RPC password.
    #[serde(default)]
    pub bitcoind_rpc_password: String,

    /// bitcoind RPC cookie file path (alternative to user/password).
    #[serde(default)]
    pub bitcoind_rpc_cookie: String,

    /// SV2 Template Provider endpoint, usually backed by the local Bitcoin node.
    #[serde(default = "default_template_provider_url")]
    pub template_provider_url: String,

    /// Pool-side Job Declarator Server endpoint.
    #[serde(default)]
    pub job_declarator_url: String,

    /// Coinbase output address (for solo mining / JDP).
    #[serde(default)]
    pub coinbase_output_address: String,

    /// How often to refresh block templates from bitcoind (seconds).
    #[serde(default = "default_template_refresh")]
    pub template_refresh_interval_s: u32,

    /// Continue mining pool-provided templates if JD cannot commit custom work.
    #[serde(default = "default_true")]
    pub fallback_to_pool_templates: bool,

    /// Enable full-template mode where JDC agrees to reveal transaction data.
    #[serde(default)]
    pub declare_tx_data: bool,

    /// Additional serialized coinbase-output bytes reserved with the Template Provider.
    #[serde(default = "default_coinbase_output_max_additional_size")]
    pub coinbase_output_max_additional_size: u32,

    /// Additional sigops reserved with the Template Provider.
    #[serde(default)]
    pub coinbase_output_max_additional_sigops: u16,
}

fn default_bitcoind_url() -> String {
    "http://127.0.0.1:8332".to_string()
}

fn default_template_provider_url() -> String {
    "sv2+tcp://127.0.0.1:8442".to_string()
}

fn default_jd_mode() -> String {
    "coinbase_only".to_string()
}

fn default_coinbase_output_max_additional_size() -> u32 {
    512
}

fn default_template_refresh() -> u32 {
    30
}

impl Default for JobDeclarationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_jd_mode(),
            bitcoind_rpc_url: default_bitcoind_url(),
            bitcoind_rpc_user: String::new(),
            bitcoind_rpc_password: String::new(),
            bitcoind_rpc_cookie: String::new(),
            template_provider_url: default_template_provider_url(),
            job_declarator_url: String::new(),
            coinbase_output_address: String::new(),
            template_refresh_interval_s: default_template_refresh(),
            fallback_to_pool_templates: true,
            declare_tx_data: false,
            coinbase_output_max_additional_size: default_coinbase_output_max_additional_size(),
            coinbase_output_max_additional_sigops: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// LED configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedConfig {
    /// Enable LED status indicators.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Mining heartbeat green LED on-time (milliseconds).
    #[serde(default = "default_heartbeat_on_ms")]
    pub heartbeat_on_ms: u16,

    /// Mining heartbeat green LED off-time (milliseconds).
    #[serde(default = "default_heartbeat_off_ms")]
    pub heartbeat_off_ms: u16,

    /// Default "Find My Miner" blink pattern ID from the built-in library.
    #[serde(default = "default_locate_pattern")]
    pub locate_pattern: String,

    /// How long the locate pattern plays before auto-stop (seconds).
    #[serde(default = "default_locate_duration_s")]
    pub locate_duration_s: u8,

    /// Flash green LED on accepted share.
    #[serde(default = "default_true")]
    pub flash_on_accepted_share: bool,

    /// Flash red LED on rejected share.
    #[serde(default = "default_true")]
    pub flash_on_rejected_share: bool,

    /// Disable user-facing LEDs during thermal night mode hours.
    #[serde(default)]
    pub night_mode_disable: bool,

    /// Celebrate lucky shares (10x+ above target difficulty).
    #[serde(default = "default_true")]
    pub celebration_on_lucky_share: bool,

    /// Chain status blink codes during init (flash count = chain number).
    #[serde(default = "default_true")]
    pub chain_status_blink_codes: bool,
}

impl Default for LedConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            heartbeat_on_ms: default_heartbeat_on_ms(),
            heartbeat_off_ms: default_heartbeat_off_ms(),
            locate_pattern: default_locate_pattern(),
            locate_duration_s: default_locate_duration_s(),
            flash_on_accepted_share: true,
            flash_on_rejected_share: true,
            night_mode_disable: false,
            celebration_on_lucky_share: true,
            chain_status_blink_codes: true,
        }
    }
}

fn default_heartbeat_on_ms() -> u16 {
    100
}

fn default_heartbeat_off_ms() -> u16 {
    900
}

fn default_locate_pattern() -> String {
    "imperial_march".to_string()
}

fn default_locate_duration_s() -> u8 {
    30
}

// ---------------------------------------------------------------------------
// Stratum V1 TCP relay (Phase 11B MVP)
// ---------------------------------------------------------------------------
//
// Dumb bidirectional TCP relay. bosminer connects to `listen_addr`; we forward
// the byte stream to `upstream_url` (a standard stratum+tcp:// pool). On
// S19j Pro `a lab unit` bosminer also opens a separate binary/Noise-like dev-fee
// session, so the relay can optionally route non-JSON first chunks to
// `binary_upstream_url`. Traffic stays byte-identical in both cases; only the
// session routing decision is local. See
// `DCENT_OS_Antminer/dcentrald/dcentrald/src/stratum_proxy.rs` for the full
// invariant list.

/// Stratum V1 TCP relay configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StratumProxyConfig {
    /// Address to listen on for bosminer connections, e.g. "127.0.0.1:3333".
    /// Default keeps the relay loopback-only so the LAN never sees it.
    #[serde(default = "default_proxy_listen")]
    pub listen_addr: String,

    /// Upstream pool URL, e.g. "stratum+tcp://btc.global.luxor.tech:700".
    /// Requires `stratum+tcp://host:port`. TLS schemes and bare host:port
    /// shortcuts are rejected.
    pub upstream_url: String,

    /// Optional alternate upstream for non-JSON first chunks.
    ///
    /// This exists for the S19j Pro `a lab unit` proxy path where bosminer keeps the
    /// user pool on Stratum V1 but also opens a separate 64-byte binary
    /// Noise/SV2-like dev-fee session. When set, first chunks that do not look
    /// like newline-delimited JSON-RPC are routed here instead of `upstream_url`.
    ///
    /// Example: `stratum2+tcp://a830bcc3.bos.braiins.com:3336`
    #[serde(default)]
    pub binary_upstream_url: Option<String>,
}

fn default_proxy_listen() -> String {
    "127.0.0.1:3333".to_string()
}
