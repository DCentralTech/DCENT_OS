//! G5 — Native Prometheus text-exposition encoder (HAL-free).
//!
//! Background: the `/metrics` HTTP route (in `dcentrald-api`) historically
//! hand-rolled its Prometheus line buffer inline inside an `async`
//! handler that takes `State<Arc<AppState>>`. That handler pulls HAL
//! modules transitively, so the exposition format could never be unit
//! tested on the Windows dev host (the documented `dcentrald-api`
//! Windows/HAL test blocker).
//!
//! This module owns a **pure, no-HAL snapshot DTO + encoder**. The
//! runtime handler builds a [`PrometheusSnapshot`] from the same watch
//! channels the dashboard/REST already read (`MinerState`,
//! `LivePowerEstimate`, `HardwareInfo`) and calls
//! [`PrometheusSnapshot::to_exposition`]. The encoder produces a valid
//! `text/plain; version=0.0.4` document: every metric family carries a
//! `# HELP` and `# TYPE` line, samples come after their family header,
//! and label values are escaped per the Prometheus spec.
//!
//! Competitive context: BraiinsOS ships a canonical Prometheus exporter;
//! a Grafana/Prometheus/VictoriaMetrics stack expects this exact format.
//! The LuxOS-style 3-tier CSV ring (`metrics_csv`) is retained and
//! written to `/data/metrics/{5s,1m,5m}.csv` independently — these are
//! additive, non-overlapping surfaces (file CSV history vs. scrape).
//!
//! This module is intentionally dependency-light (`serde` only, same as
//! the rest of `dcentrald-api-types`) and **never** mutates state. It is
//! a pure transform: snapshot in, text out.

use serde::{Deserialize, Serialize};

/// Prometheus exposition format version this encoder targets. Used in the
/// `Content-Type` header by the HTTP layer.
pub const EXPOSITION_VERSION: &str = "0.0.4";

/// Canonical `Content-Type` for the `/metrics` response.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Escape a Prometheus label *value* per the text exposition spec:
/// backslash, double-quote and newline are escaped; carriage returns are
/// normalised to spaces so a stray CR can't split a sample line.
pub fn escape_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Hardware-error rate as a **percent** (`0.0..=100.0`): the fraction of all
/// submitted work units that came back as a hardware / CRC error.
///
/// `hw_errors / (accepted + rejected + hw_errors) * 100`.
///
/// Returns `0.0` when there is no work yet (`accepted + rejected + hw_errors
/// == 0`) — a fresh miner with no submitted work is honestly "0% errors", and
/// this also guards the divide-by-zero. A *healthy* miner with real accepted
/// shares and zero errors is likewise `0.0` (true, not fabricated).
///
/// This is the SINGLE SOURCE OF TRUTH for two surfaces, so they can never
/// drift:
///   - the CGMiner `SUMMARY` `Device Hardware%` field (a `0..100` percent),
///   - the Prometheus `dcentrald_hw_error_rate` gauge (a `0..1` rate, derived
///     here as `hw_error_percent(..) / 100.0`).
///
/// Sums are widened to `f64` before adding so the denominator cannot overflow.
pub fn hw_error_percent(hw_errors: u64, accepted: u64, rejected: u64) -> f64 {
    let denom = accepted as f64 + rejected as f64 + hw_errors as f64;
    if denom > 0.0 {
        hw_errors as f64 / denom * 100.0
    } else {
        0.0
    }
}

/// Per-chain (per-board) sample. On multi-board Antminers this is one
/// entry per hash board; `chips` is the per-board responding chip count
/// (the closest available proxy for per-chip health on the serial
/// transport, which does not expose per-chip telemetry frames).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainMetric {
    /// Chain / board id (e.g. 6/7/8 on S9, 0/1/2 on serial platforms).
    pub id: u8,
    /// Number of responding chips on this board.
    pub chips: u32,
    /// ASIC frequency in MHz.
    pub frequency_mhz: u32,
    /// Chip-rail voltage in millivolts.
    pub voltage_mv: u32,
    /// Board / chip temperature in °C.
    pub temp_c: f32,
    /// Per-board hashrate in GH/s.
    pub hashrate_ghs: f64,
    /// Cumulative CRC / hardware errors observed on this board.
    pub errors: u64,
}

/// Per-fan sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanMetric {
    /// Fan index (0-based).
    pub id: u8,
    /// Tachometer RPM (0 = not connected / not spinning).
    pub rpm: u32,
    /// PWM duty cycle percent (0..=100).
    pub pwm_percent: u8,
}

/// A complete, HAL-free snapshot of everything the Prometheus exporter
/// publishes. Built by the HTTP handler from existing watch channels;
/// consumed only by [`Self::to_exposition`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PrometheusSnapshot {
    // ---- identity / info ------------------------------------------------
    /// Firmware version string (e.g. `0.6.0`).
    pub firmware_version: String,
    /// ASIC / chip model label (e.g. `BM1362`). Empty => `unknown`.
    pub chip_model: String,
    /// Provenance for `chip_model` (e.g. `hardware_info.chip_type` or `unknown`).
    #[serde(default)]
    pub chip_model_source: String,
    /// Operating mode label (e.g. `standard`, `heater`, `hacker`).
    pub mode: String,

    // ---- hashrate -------------------------------------------------------
    /// Instantaneous hashrate in GH/s.
    pub hashrate_ghs: f64,
    /// 5-second rolling average hashrate in GH/s.
    pub hashrate_5s_ghs: f64,
    /// 15-minute rolling average hashrate in GH/s, if available.
    pub hashrate_15m_ghs: Option<f64>,
    /// 24-hour rolling average hashrate in GH/s, if available.
    pub hashrate_24h_ghs: Option<f64>,

    // ---- power ----------------------------------------------------------
    /// Board-side power in watts.
    pub board_watts: f64,
    /// Wall-side power in watts.
    pub wall_watts: f64,
    /// Energy efficiency in J/TH.
    pub efficiency_jth: f64,
    /// Heat output in BTU/h.
    pub btu_h: f64,
    /// Whether the power gauges are backed by currently available power
    /// telemetry. When false, numeric power families are omitted rather than
    /// exporting modeled/static fallback watts as live scrape data.
    #[serde(default)]
    pub power_live_available: bool,
    /// Whether the current power source is modeled rather than measured.
    #[serde(default)]
    pub power_modeled: bool,
    /// Coarse source for power telemetry, e.g. `pmbus`, `adc`,
    /// `live_model`, or `static_model_fallback`.
    #[serde(default)]
    pub power_source: String,
    /// Detailed source/provenance marker for power telemetry.
    #[serde(default)]
    pub power_source_detail: String,

    // ---- shares ---------------------------------------------------------
    /// Cumulative accepted shares.
    pub shares_accepted: u64,
    /// Cumulative rejected shares.
    pub shares_rejected: u64,

    // ---- pool -----------------------------------------------------------
    /// Pool transport connected (subscribed+authorized).
    pub pool_connected: bool,
    /// Pool connection attempt in progress (NOT yet connected).
    pub pool_connecting: bool,
    /// Current pool target difficulty.
    pub pool_difficulty: f64,
    /// Last measured pool round-trip latency in ms (share submit -> response).
    /// 0 = not yet measured. VNish `pools[].ping` parity (HLA-9).
    pub pool_latency_ms: u64,

    // ---- uptime ---------------------------------------------------------
    /// Daemon uptime in seconds.
    pub uptime_seconds: u64,

    // ---- per-board / per-fan -------------------------------------------
    /// Per-board (per-chain) samples.
    pub chains: Vec<ChainMetric>,
    /// Per-fan samples.
    pub fans: Vec<FanMetric>,

    // ---- autotuner (W9/W15 silicon telemetry — fleet/Grafana parity) ---
    /// Whether the autotuner is enabled.
    #[serde(default)]
    pub autotuner_enabled: bool,
    /// Autotuner convergence progress (0-100), if a run is active.
    #[serde(default)]
    pub autotuner_percent_complete: Option<f64>,
    /// Chips actively being tuned, if known.
    #[serde(default)]
    pub autotuner_active_chips: Option<u32>,
    /// Total chips the autotuner is managing, if known.
    #[serde(default)]
    pub autotuner_total_chips: Option<u32>,
    /// Effective silicon-grade chip counts `[A, B, C, D]` (autotuner-measured).
    /// PURE TELEMETRY — `None` until the autotuner has characterized the chips,
    /// so the family stays out of the exposition rather than reporting a
    /// fabricated all-zero / all-grade-A distribution.
    #[serde(default)]
    pub silicon_grade_counts: Option<[u32; 4]>,

    // ---- thermal supervisor (Wave-G — diagnostic only) -----------------
    /// Whether the thermal supervisor is enabled.
    #[serde(default)]
    pub thermal_supervisor_enabled: bool,
    /// Worst inter-chip temperature spread across boards (°C), if >= 2 chip
    /// sensors have been read. DIAGNOSTIC ONLY — never a control input.
    #[serde(default)]
    pub chip_imbalance_worst_c: Option<f64>,
    /// Inter-chip imbalance flag threshold (°C), present when the supervisor
    /// is active.
    #[serde(default)]
    pub chip_imbalance_threshold_c: Option<f64>,
    /// Whether any board's inter-chip spread exceeded the threshold.
    #[serde(default)]
    pub chip_imbalance_flagged: bool,

    // ---- pool identity labels (P2-6 §4.C — fleet-grade {pool,worker}) ---
    /// Active pool URL, used as the `pool` label on the per-pool share
    /// counters. Empty => the labeled `dcentrald_pool_shares_*` families are
    /// omitted entirely (the unlabeled global `dcentrald_shares_*_total`
    /// counters still publish — never a fabricated empty-label series).
    #[serde(default)]
    pub pool_label: String,
    /// Active worker, used as the `worker` label on the per-pool share
    /// counters. May be empty (`worker=""`) for pools that authorize on a
    /// bare wallet/username with no worker suffix.
    #[serde(default)]
    pub worker_label: String,

    // ---- donation (P2-6 §4.C) ------------------------------------------
    /// Whether a transparent donation window is currently active (mirrors
    /// `pool.donating`). Always emitted as a 0/1 gauge.
    #[serde(default)]
    pub donation_active: bool,

    // ---- optional integrations (P2-6 §4.C) -----------------------------
    /// MQTT publisher integration state for the `dcentrald_integration_up`
    /// family: `Some(true)` = enabled with a broker target (the publisher
    /// task is active), `Some(false)` = a `[mqtt]` block exists but is
    /// disabled / has no broker, `None` = not configured (the `kind="mqtt"`
    /// sample is omitted). Reflects CONFIGURED + ENABLED intent — NOT a
    /// verified live broker TCP/connack handshake.
    #[serde(default)]
    pub mqtt_integration_up: Option<bool>,
    /// Webhook dispatcher integration state for `dcentrald_integration_up`,
    /// same tri-state semantics as `mqtt_integration_up` (enabled + non-empty
    /// URL => up; reflects configured intent, not a probed delivery).
    #[serde(default)]
    pub webhook_integration_up: Option<bool>,
}

/// Internal: append a metric family header (HELP + TYPE) to `buf`.
fn family(buf: &mut String, name: &str, help: &str, kind: &str) {
    buf.push_str("# HELP ");
    buf.push_str(name);
    buf.push(' ');
    // HELP text: backslash and newline escaped (Prometheus spec). No
    // double-quote escaping in HELP (it is line-terminated, not quoted).
    for ch in help.chars() {
        match ch {
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push(' '),
            c => buf.push(c),
        }
    }
    buf.push('\n');
    buf.push_str("# TYPE ");
    buf.push_str(name);
    buf.push(' ');
    buf.push_str(kind);
    buf.push('\n');
}

impl PrometheusSnapshot {
    /// Render the full Prometheus text-exposition document. The result
    /// always ends with a trailing newline (required by some scrapers).
    pub fn to_exposition(&self) -> String {
        let mut b = String::with_capacity(2048);

        // ---- info -------------------------------------------------------
        let model = if self.chip_model.trim().is_empty() {
            "unknown"
        } else {
            self.chip_model.trim()
        };
        let model_source = if self.chip_model_source.trim().is_empty() {
            "unknown"
        } else {
            self.chip_model_source.trim()
        };
        family(
            &mut b,
            "dcentrald_info",
            "Firmware build / hardware identity (always 1)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_info{{version=\"{}\",model=\"{}\",model_source=\"{}\",mode=\"{}\",firmware=\"DCENT_OS\"}} 1\n",
            escape_label_value(&self.firmware_version),
            escape_label_value(model),
            escape_label_value(model_source),
            escape_label_value(&self.mode),
        ));

        // ---- hashrate ---------------------------------------------------
        family(
            &mut b,
            "dcentrald_hashrate_ghs",
            "Instantaneous hashrate in GH/s",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_hashrate_ghs {:.2}\n",
            self.hashrate_ghs
        ));

        family(
            &mut b,
            "dcentrald_hashrate_5s_ghs",
            "5-second rolling average hashrate in GH/s",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_hashrate_5s_ghs {:.2}\n",
            self.hashrate_5s_ghs
        ));

        if let Some(v) = self.hashrate_15m_ghs {
            family(
                &mut b,
                "dcentrald_hashrate_15m_ghs",
                "15-minute rolling average hashrate in GH/s",
                "gauge",
            );
            b.push_str(&format!("dcentrald_hashrate_15m_ghs {:.2}\n", v));
        }
        if let Some(v) = self.hashrate_24h_ghs {
            family(
                &mut b,
                "dcentrald_hashrate_24h_ghs",
                "24-hour rolling average hashrate in GH/s",
                "gauge",
            );
            b.push_str(&format!("dcentrald_hashrate_24h_ghs {:.2}\n", v));
        }

        // ---- power ------------------------------------------------------
        family(
            &mut b,
            "dcentrald_power_live_available",
            "Power telemetry availability (1=live/current, 0=unavailable or static fallback)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_power_live_available{{source=\"{}\",source_detail=\"{}\",modeled=\"{}\"}} {}\n",
            escape_label_value(if self.power_source.is_empty() {
                "unknown"
            } else {
                &self.power_source
            }),
            escape_label_value(if self.power_source_detail.is_empty() {
                "unknown"
            } else {
                &self.power_source_detail
            }),
            if self.power_modeled { "true" } else { "false" },
            if self.power_live_available { 1 } else { 0 }
        ));

        if self.power_live_available {
            family(
                &mut b,
                "dcentrald_power_watts",
                "Current board power in watts",
                "gauge",
            );
            b.push_str(&format!("dcentrald_power_watts {:.0}\n", self.board_watts));

            family(
                &mut b,
                "dcentrald_wall_watts",
                "Current wall power in watts",
                "gauge",
            );
            b.push_str(&format!("dcentrald_wall_watts {:.0}\n", self.wall_watts));

            family(
                &mut b,
                "dcentrald_efficiency_jth",
                "Current power efficiency in joules per terahash",
                "gauge",
            );
            b.push_str(&format!(
                "dcentrald_efficiency_jth {:.1}\n",
                self.efficiency_jth
            ));

            family(
                &mut b,
                "dcentrald_btu_h",
                "Current heat output in BTU per hour",
                "gauge",
            );
            b.push_str(&format!("dcentrald_btu_h {:.0}\n", self.btu_h));
        }

        // ---- per-board temp / hashrate / chips / freq / volt / errors --
        family(
            &mut b,
            "dcentrald_temp_c",
            "Board/chip temperature per chain in Celsius",
            "gauge",
        );
        for c in &self.chains {
            b.push_str(&format!(
                "dcentrald_temp_c{{chain=\"{}\"}} {:.1}\n",
                c.id, c.temp_c
            ));
        }

        family(
            &mut b,
            "dcentrald_chain_hashrate_ghs",
            "Per-chain hashrate in GH/s",
            "gauge",
        );
        for c in &self.chains {
            b.push_str(&format!(
                "dcentrald_chain_hashrate_ghs{{chain=\"{}\"}} {:.2}\n",
                c.id, c.hashrate_ghs
            ));
        }

        family(
            &mut b,
            "dcentrald_chain_chips",
            "Number of responding chips per chain (per-chip health proxy)",
            "gauge",
        );
        for c in &self.chains {
            b.push_str(&format!(
                "dcentrald_chain_chips{{chain=\"{}\"}} {}\n",
                c.id, c.chips
            ));
        }

        family(
            &mut b,
            "dcentrald_chain_frequency_mhz",
            "ASIC frequency per chain in MHz",
            "gauge",
        );
        for c in &self.chains {
            b.push_str(&format!(
                "dcentrald_chain_frequency_mhz{{chain=\"{}\"}} {}\n",
                c.id, c.frequency_mhz
            ));
        }

        family(
            &mut b,
            "dcentrald_chain_voltage_mv",
            "Chip-rail voltage per chain in millivolts",
            "gauge",
        );
        for c in &self.chains {
            b.push_str(&format!(
                "dcentrald_chain_voltage_mv{{chain=\"{}\"}} {}\n",
                c.id, c.voltage_mv
            ));
        }

        family(
            &mut b,
            "dcentrald_chain_errors_total",
            "Cumulative CRC / hardware errors per chain",
            "counter",
        );
        let mut hw_errors_total: u64 = 0;
        for c in &self.chains {
            hw_errors_total = hw_errors_total.saturating_add(c.errors);
            b.push_str(&format!(
                "dcentrald_chain_errors_total{{chain=\"{}\"}} {}\n",
                c.id, c.errors
            ));
        }

        // Aggregate HW-error rate: cumulative errors over
        // (accepted + rejected + errors) work units. 0 when no work yet.
        family(
            &mut b,
            "dcentrald_hw_errors_total",
            "Cumulative hardware/CRC errors across all chains",
            "counter",
        );
        b.push_str(&format!("dcentrald_hw_errors_total {}\n", hw_errors_total));

        family(
            &mut b,
            "dcentrald_hw_error_rate",
            "Hardware-error fraction: hw_errors / (accepted + rejected + hw_errors)",
            "gauge",
        );
        // Single-source the rate with the CGMiner `Device Hardware%` percent:
        // the gauge is a 0..1 rate, so divide the shared percent by 100.
        let hw_rate =
            hw_error_percent(hw_errors_total, self.shares_accepted, self.shares_rejected) / 100.0;
        b.push_str(&format!("dcentrald_hw_error_rate {:.6}\n", hw_rate));

        // ---- fans -------------------------------------------------------
        family(
            &mut b,
            "dcentrald_fan_rpm",
            "Fan speed in RPM per fan",
            "gauge",
        );
        for f in &self.fans {
            b.push_str(&format!(
                "dcentrald_fan_rpm{{fan=\"{}\"}} {}\n",
                f.id, f.rpm
            ));
        }

        family(
            &mut b,
            "dcentrald_fan_pwm",
            "Fan PWM duty cycle percent (0-100) per fan",
            "gauge",
        );
        for f in &self.fans {
            b.push_str(&format!(
                "dcentrald_fan_pwm{{fan=\"{}\"}} {}\n",
                f.id, f.pwm_percent
            ));
        }

        // ---- shares -----------------------------------------------------
        family(
            &mut b,
            "dcentrald_shares_accepted_total",
            "Total accepted shares since daemon start",
            "counter",
        );
        b.push_str(&format!(
            "dcentrald_shares_accepted_total {}\n",
            self.shares_accepted
        ));

        family(
            &mut b,
            "dcentrald_shares_rejected_total",
            "Total rejected shares since daemon start",
            "counter",
        );
        b.push_str(&format!(
            "dcentrald_shares_rejected_total {}\n",
            self.shares_rejected
        ));

        // ---- per-pool / per-worker shares (P2-6 — fleet-grade labels) ---
        // The same accepted/rejected totals, additionally carved by the
        // active {pool,worker}. Prometheus aggregates these across a fleet
        // (sum by (pool), rejects by (worker), etc.). Omitted entirely when
        // no pool is configured so we never emit a fabricated empty-label
        // series. The unlabeled global counters above are kept for back-compat
        // with existing scrape configs / the shipped Grafana dashboard.
        if !self.pool_label.trim().is_empty() {
            let pool = escape_label_value(self.pool_label.trim());
            let worker = escape_label_value(self.worker_label.trim());
            family(
                &mut b,
                "dcentrald_pool_shares_accepted_total",
                "Accepted shares since daemon start, labeled by active pool + worker",
                "counter",
            );
            b.push_str(&format!(
                "dcentrald_pool_shares_accepted_total{{pool=\"{}\",worker=\"{}\"}} {}\n",
                pool, worker, self.shares_accepted
            ));
            family(
                &mut b,
                "dcentrald_pool_shares_rejected_total",
                "Rejected shares since daemon start, labeled by active pool + worker",
                "counter",
            );
            b.push_str(&format!(
                "dcentrald_pool_shares_rejected_total{{pool=\"{}\",worker=\"{}\"}} {}\n",
                pool, worker, self.shares_rejected
            ));
        }

        // ---- pool -------------------------------------------------------
        family(
            &mut b,
            "dcentrald_pool_connected",
            "Pool connection status (1=connected, 0=disconnected)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_pool_connected {}\n",
            if self.pool_connected { 1 } else { 0 }
        ));

        family(
            &mut b,
            "dcentrald_pool_connecting",
            "Pool connection attempt status (1=connecting, 0=not connecting)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_pool_connecting {}\n",
            if self.pool_connecting { 1 } else { 0 }
        ));

        family(
            &mut b,
            "dcentrald_pool_difficulty",
            "Current pool target difficulty",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_pool_difficulty {}\n",
            self.pool_difficulty
        ));

        family(
            &mut b,
            "dcentrald_pool_latency_ms",
            "Last measured pool round-trip latency in ms (0 = not yet measured)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_pool_latency_ms {}\n",
            self.pool_latency_ms
        ));

        // ---- uptime -----------------------------------------------------
        family(
            &mut b,
            "dcentrald_uptime_seconds",
            "Daemon uptime in seconds",
            // O4: gauge, not counter — uptime is the current instantaneous
            // seconds-since-start (resets to 0 on restart), so `rate()`/`increase()`
            // must not treat a restart as a counter reset. Mirrors node_exporter's
            // gauge convention for a time-since-boot reading.
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_uptime_seconds {}\n",
            self.uptime_seconds
        ));

        // ---- donation (P2-6) --------------------------------------------
        family(
            &mut b,
            "dcentrald_donation_active",
            "Transparent donation window currently active (1=donating, 0=not)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_donation_active {}\n",
            if self.donation_active { 1 } else { 0 }
        ));

        // ---- optional integrations (P2-6) -------------------------------
        // One `dcentrald_integration_up` family, one sample per configured
        // integration `kind`. CONTRACT: this reflects CONFIGURED + ENABLED
        // intent (the publisher/dispatcher task is active with a target), NOT
        // a verified live broker/endpoint handshake. A `None` (integration not
        // present in config) omits that kind's sample rather than fabricating
        // a 0 for an integration the operator never set up. The family header
        // is only emitted when at least one kind is present.
        if self.mqtt_integration_up.is_some() || self.webhook_integration_up.is_some() {
            family(
                &mut b,
                "dcentrald_integration_up",
                "Optional integration enabled with a target (1=up, 0=down). Reflects configured+enabled intent, not a verified live connection.",
                "gauge",
            );
            if let Some(up) = self.mqtt_integration_up {
                b.push_str(&format!(
                    "dcentrald_integration_up{{kind=\"mqtt\"}} {}\n",
                    if up { 1 } else { 0 }
                ));
            }
            if let Some(up) = self.webhook_integration_up {
                b.push_str(&format!(
                    "dcentrald_integration_up{{kind=\"webhook\"}} {}\n",
                    if up { 1 } else { 0 }
                ));
            }
        }

        // ---- autotuner (W9/W15 silicon telemetry — fleet/Grafana parity) ---
        // The subsystem-enabled gauge is always emitted (0/1); the detail
        // families stay out of the exposition until there is real data (same
        // "never a fabricated 0" rule as the 15m/24h hashrate families).
        family(
            &mut b,
            "dcentrald_autotuner_enabled",
            "Autotuner enabled (1=enabled, 0=disabled)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_autotuner_enabled {}\n",
            if self.autotuner_enabled { 1 } else { 0 }
        ));
        if let Some(pct) = self.autotuner_percent_complete {
            family(
                &mut b,
                "dcentrald_autotuner_percent_complete",
                "Autotuner convergence progress (0-100)",
                "gauge",
            );
            b.push_str(&format!(
                "dcentrald_autotuner_percent_complete {:.1}\n",
                pct
            ));
        }
        if let Some(n) = self.autotuner_active_chips {
            family(
                &mut b,
                "dcentrald_autotuner_active_chips",
                "Chips currently being tuned",
                "gauge",
            );
            b.push_str(&format!("dcentrald_autotuner_active_chips {}\n", n));
        }
        if let Some(n) = self.autotuner_total_chips {
            family(
                &mut b,
                "dcentrald_autotuner_total_chips",
                "Total chips the autotuner is managing",
                "gauge",
            );
            b.push_str(&format!("dcentrald_autotuner_total_chips {}\n", n));
        }
        if let Some(grades) = self.silicon_grade_counts {
            family(
                &mut b,
                "dcentrald_silicon_grade_chips",
                "Effective silicon-grade chip counts (autotuner-measured). PURE TELEMETRY: absent until characterized.",
                "gauge",
            );
            for (label, count) in [
                ("a", grades[0]),
                ("b", grades[1]),
                ("c", grades[2]),
                ("d", grades[3]),
            ] {
                b.push_str(&format!(
                    "dcentrald_silicon_grade_chips{{grade=\"{}\"}} {}\n",
                    label, count
                ));
            }
        }

        // ---- thermal supervisor (Wave-G — diagnostic uniformity signal) ---
        family(
            &mut b,
            "dcentrald_thermal_supervisor_enabled",
            "Thermal supervisor enabled (1=enabled, 0=disabled)",
            "gauge",
        );
        b.push_str(&format!(
            "dcentrald_thermal_supervisor_enabled {}\n",
            if self.thermal_supervisor_enabled {
                1
            } else {
                0
            }
        ));
        if let Some(worst) = self.chip_imbalance_worst_c {
            family(
                &mut b,
                "dcentrald_chip_imbalance_worst_celsius",
                "Worst inter-chip temperature spread across boards in Celsius (DIAGNOSTIC — never a control input)",
                "gauge",
            );
            b.push_str(&format!(
                "dcentrald_chip_imbalance_worst_celsius {:.1}\n",
                worst
            ));
        }
        if let Some(thr) = self.chip_imbalance_threshold_c {
            family(
                &mut b,
                "dcentrald_chip_imbalance_threshold_celsius",
                "Inter-chip imbalance flag threshold in Celsius",
                "gauge",
            );
            b.push_str(&format!(
                "dcentrald_chip_imbalance_threshold_celsius {:.1}\n",
                thr
            ));
        }
        // The flagged gauge is only meaningful while the supervisor is running.
        if self.thermal_supervisor_enabled {
            family(
                &mut b,
                "dcentrald_chip_imbalance_flagged",
                "Any board's inter-chip spread exceeded the diagnostic threshold (1=flagged, 0=ok)",
                "gauge",
            );
            b.push_str(&format!(
                "dcentrald_chip_imbalance_flagged {}\n",
                if self.chip_imbalance_flagged { 1 } else { 0 }
            ));
        }

        // S4-1: coerce non-finite float values to the Prometheus-legal tokens
        // `+Inf`/`-Inf` (`{}` / `{:.N}` render a non-finite f64 as `inf`/`-inf`,
        // which the exposition format REJECTS; `NaN` is already valid). One
        // non-finite reading (e.g. an unguarded `pool_difficulty`) would otherwise
        // make the WHOLE scrape unparseable. Every metric value is rendered as
        // ` <value>\n`, so coercing that exact pattern never matches a label value
        // (inside `{}`) or HELP/TYPE text. Mirrors the ESP metrics_render fix (ES-4).
        b.replace(" inf\n", " +Inf\n").replace(" -inf\n", " -Inf\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_snapshot() -> PrometheusSnapshot {
        PrometheusSnapshot {
            firmware_version: "0.6.0".into(),
            chip_model: "BM1362".into(),
            chip_model_source: "hardware_info.chip_type".into(),
            mode: "standard".into(),
            hashrate_ghs: 104_250.5,
            hashrate_5s_ghs: 103_900.0,
            hashrate_15m_ghs: Some(104_000.0),
            hashrate_24h_ghs: Some(103_500.0),
            board_watts: 3210.0,
            wall_watts: 3450.0,
            efficiency_jth: 33.1,
            btu_h: 11_771.0,
            power_live_available: true,
            power_modeled: false,
            power_source: "pmbus".into(),
            power_source_detail: "pmbus_measured".into(),
            shares_accepted: 40,
            shares_rejected: 1,
            pool_connected: true,
            pool_connecting: false,
            pool_difficulty: 512.0,
            pool_latency_ms: 42,
            uptime_seconds: 7_800,
            chains: vec![
                ChainMetric {
                    id: 0,
                    chips: 110,
                    frequency_mhz: 525,
                    voltage_mv: 13_700,
                    temp_c: 49.5,
                    hashrate_ghs: 52_000.0,
                    errors: 3,
                },
                ChainMetric {
                    id: 1,
                    chips: 108,
                    frequency_mhz: 525,
                    voltage_mv: 13_700,
                    temp_c: 50.2,
                    hashrate_ghs: 52_250.5,
                    errors: 7,
                },
            ],
            fans: vec![
                FanMetric {
                    id: 0,
                    rpm: 1260,
                    pwm_percent: 30,
                },
                FanMetric {
                    id: 1,
                    rpm: 0,
                    pwm_percent: 30,
                },
            ],
            autotuner_enabled: true,
            autotuner_percent_complete: Some(87.5),
            autotuner_active_chips: Some(12),
            autotuner_total_chips: Some(218),
            silicon_grade_counts: Some([180, 28, 8, 2]),
            thermal_supervisor_enabled: true,
            chip_imbalance_worst_c: Some(4.5),
            chip_imbalance_threshold_c: Some(8.0),
            chip_imbalance_flagged: false,
            // OBS-1/OBS-2: build_prometheus_snapshot masks the worker (wallet)
            // + sanitizes the pool URL BEFORE they reach these labels, so the
            // exposition only ever carries the masked/sanitized forms. Fixture
            // uses the masked worker form (mask_wallet `<first6>…<last4>`).
            pool_label: "stratum+tcp://pool.example.com:3333".into(),
            worker_label: "bc1qex\u{2026}rig1".into(),
            donation_active: true,
            mqtt_integration_up: Some(true),
            webhook_integration_up: Some(false),
        }
    }

    /// Minimal Prometheus exposition validator: every non-comment,
    /// non-blank line must be `<metric>[{labels}] <value>` and there must
    /// be a preceding `# TYPE <metric> ...` for that family. Every
    /// `# HELP` must be paired with a `# TYPE` for the same name. Returns
    /// the set of declared metric families.
    fn assert_parseable(text: &str) -> std::collections::BTreeSet<String> {
        use std::collections::{BTreeMap, BTreeSet};
        let mut help: BTreeMap<String, ()> = BTreeMap::new();
        let mut types: BTreeMap<String, String> = BTreeMap::new();
        let mut declared: BTreeSet<String> = BTreeSet::new();

        assert!(text.ends_with('\n'), "exposition must end with newline");

        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("# HELP ") {
                let name = rest.split(' ').next().expect("HELP needs a name");
                assert!(!name.is_empty(), "empty HELP metric name");
                help.insert(name.to_string(), ());
                continue;
            }
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let mut it = rest.splitn(2, ' ');
                let name = it.next().expect("TYPE needs a name").to_string();
                let kind = it.next().expect("TYPE needs a kind").to_string();
                assert!(
                    matches!(
                        kind.as_str(),
                        "gauge" | "counter" | "histogram" | "summary" | "untyped"
                    ),
                    "bad TYPE kind: {kind:?}"
                );
                types.insert(name.clone(), kind);
                declared.insert(name);
                continue;
            }
            assert!(!line.starts_with('#'), "unexpected comment line: {line:?}");
            // Sample line: metric[{labels}] value
            let (metric_part, value_part) =
                line.rsplit_once(' ').expect("sample must be 'name value'");
            let metric_name = metric_part
                .split_once('{')
                .map(|(n, _)| n)
                .unwrap_or(metric_part);
            assert!(
                types.contains_key(metric_name),
                "sample {metric_name:?} has no preceding # TYPE (line {line:?})"
            );
            // Value must parse as f64 (Prometheus numeric sample).
            value_part
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("non-numeric sample value: {line:?}"));
            // If labels are present, the brace section must be balanced.
            if let Some((_, lbl)) = metric_part.split_once('{') {
                assert!(lbl.ends_with('}'), "unterminated label set: {line:?}");
            }
        }

        // Every HELP must have a matching TYPE and vice-versa.
        for k in help.keys() {
            assert!(types.contains_key(k), "HELP without TYPE: {k}");
        }
        for k in types.keys() {
            assert!(help.contains_key(k), "TYPE without HELP: {k}");
        }
        declared
    }

    #[test]
    fn exposition_is_parseable_and_well_formed() {
        let txt = full_snapshot().to_exposition();
        let families = assert_parseable(&txt);
        // Spot-check the headline families exist.
        for must in [
            "dcentrald_info",
            "dcentrald_hashrate_ghs",
            "dcentrald_hashrate_5s_ghs",
            "dcentrald_hashrate_15m_ghs",
            "dcentrald_hashrate_24h_ghs",
            "dcentrald_power_live_available",
            "dcentrald_power_watts",
            "dcentrald_wall_watts",
            "dcentrald_efficiency_jth",
            "dcentrald_btu_h",
            "dcentrald_temp_c",
            "dcentrald_chain_hashrate_ghs",
            "dcentrald_chain_chips",
            "dcentrald_chain_frequency_mhz",
            "dcentrald_chain_voltage_mv",
            "dcentrald_chain_errors_total",
            "dcentrald_hw_errors_total",
            "dcentrald_hw_error_rate",
            "dcentrald_fan_rpm",
            "dcentrald_fan_pwm",
            "dcentrald_shares_accepted_total",
            "dcentrald_shares_rejected_total",
            "dcentrald_pool_connected",
            "dcentrald_pool_connecting",
            "dcentrald_pool_difficulty",
            "dcentrald_pool_latency_ms",
            "dcentrald_uptime_seconds",
        ] {
            assert!(families.contains(must), "missing metric family: {must}");
        }
        // HLA-9: the pool latency gauge renders the snapshot value verbatim.
        assert!(
            txt.contains("dcentrald_pool_latency_ms 42\n"),
            "pool latency gauge must render the measured value"
        );
    }

    #[test]
    fn optional_avg_hashrate_omitted_when_absent() {
        let mut s = full_snapshot();
        s.hashrate_15m_ghs = None;
        s.hashrate_24h_ghs = None;
        let txt = s.to_exposition();
        assert_parseable(&txt);
        assert!(!txt.contains("dcentrald_hashrate_15m_ghs"));
        assert!(!txt.contains("dcentrald_hashrate_24h_ghs"));
        // The mandatory ones still present.
        assert!(txt.contains("dcentrald_hashrate_ghs "));
    }

    #[test]
    fn non_finite_float_renders_as_valid_prometheus_inf() {
        // S4-1: a non-finite pool_difficulty must render as `+Inf`/`-Inf` (the
        // exposition format rejects bare `inf`/`-inf`), so one bad reading can't
        // make the whole scrape unparseable.
        let mut s = full_snapshot();
        s.pool_difficulty = f64::INFINITY;
        let txt = s.to_exposition();
        assert_parseable(&txt);
        assert!(
            txt.contains("dcentrald_pool_difficulty +Inf\n"),
            "positive inf must render as +Inf: {txt}"
        );
        assert!(!txt.contains(" inf\n"), "no bare inf may remain: {txt}");

        let mut s2 = full_snapshot();
        s2.pool_difficulty = f64::NEG_INFINITY;
        let txt2 = s2.to_exposition();
        assert_parseable(&txt2);
        assert!(
            txt2.contains("dcentrald_pool_difficulty -Inf\n"),
            "negative inf must render as -Inf: {txt2}"
        );
        assert!(!txt2.contains(" -inf\n"), "no bare -inf may remain: {txt2}");
    }

    #[test]
    fn power_families_omitted_when_live_power_unavailable() {
        let mut s = full_snapshot();
        s.power_live_available = false;
        s.power_modeled = true;
        s.power_source = "static_model_fallback".into();
        s.power_source_detail = "static_power_fallback_from_miner_state".into();
        let txt = s.to_exposition();
        let families = assert_parseable(&txt);

        assert!(families.contains("dcentrald_power_live_available"));
        assert!(!families.contains("dcentrald_power_watts"));
        assert!(!families.contains("dcentrald_wall_watts"));
        assert!(!families.contains("dcentrald_efficiency_jth"));
        assert!(!families.contains("dcentrald_btu_h"));
        assert!(txt.contains(
            "dcentrald_power_live_available{source=\"static_model_fallback\",source_detail=\"static_power_fallback_from_miner_state\",modeled=\"true\"} 0"
        ));
    }

    #[test]
    fn per_chain_and_per_fan_labels_emitted() {
        let txt = full_snapshot().to_exposition();
        assert!(txt.contains("dcentrald_temp_c{chain=\"0\"} 49.5"));
        assert!(txt.contains("dcentrald_temp_c{chain=\"1\"} 50.2"));
        assert!(txt.contains("dcentrald_chain_chips{chain=\"0\"} 110"));
        assert!(txt.contains("dcentrald_fan_rpm{fan=\"0\"} 1260"));
        assert!(txt.contains("dcentrald_fan_rpm{fan=\"1\"} 0"));
        assert!(txt.contains("dcentrald_fan_pwm{fan=\"0\"} 30"));
    }

    #[test]
    fn hw_error_rate_is_fraction_over_total_work() {
        let txt = full_snapshot().to_exposition();
        // errors 3+7=10; accepted 40; rejected 1 => 10 / 51 = 0.196078
        assert!(
            txt.contains("dcentrald_hw_errors_total 10"),
            "exposition: {txt}"
        );
        assert!(
            txt.contains("dcentrald_hw_error_rate 0.196078"),
            "exposition: {txt}"
        );
    }

    #[test]
    fn hw_error_rate_zero_when_no_work_yet() {
        let mut s = full_snapshot();
        s.shares_accepted = 0;
        s.shares_rejected = 0;
        s.chains.iter_mut().for_each(|c| c.errors = 0);
        let txt = s.to_exposition();
        assert!(txt.contains("dcentrald_hw_error_rate 0.000000"));
    }

    // ── hw_error_percent: the single-source helper (Device Hardware% +
    //    dcentrald_hw_error_rate) ──────────────────────────────────────────

    #[test]
    fn hw_error_percent_is_errors_over_total_work_times_100() {
        // 10 errors out of (40 accepted + 1 rejected + 10 errors) = 51 work
        // units => 10/51*100 ≈ 19.6078 %. This is the CGMiner Device Hardware%
        // value; the Prometheus gauge is the same number / 100.
        let pct = hw_error_percent(10, 40, 1);
        assert!((pct - 19.607_843).abs() < 1e-5, "got {pct}");
        // Cross-pin: the rate the Prometheus encoder emits is pct/100.
        assert!((pct / 100.0 - 0.196_078).abs() < 1e-6, "rate from {pct}");
    }

    #[test]
    fn hw_error_percent_zero_when_no_work_yet() {
        // No accepted, no rejected, no errors => 0.0, and crucially NO
        // divide-by-zero. A fresh miner is honestly "0% errors".
        assert_eq!(hw_error_percent(0, 0, 0), 0.0);
    }

    #[test]
    fn hw_error_percent_zero_for_healthy_miner_with_real_work() {
        // 100 accepted shares, no rejects, no hardware errors => a genuine
        // 0.0% — this is the honest "perfect board health" case, and it must
        // be indistinguishable from a real measured 0, because it IS one.
        assert_eq!(hw_error_percent(0, 100, 0), 0.0);
    }

    #[test]
    fn hw_error_percent_high_error_case_and_full_error_bound() {
        // Mostly-broken board: 90 errors vs 10 accepted => 90/100*100 = 90%.
        let pct = hw_error_percent(90, 10, 0);
        assert!((pct - 90.0).abs() < 1e-9, "got {pct}");
        // All work is hardware error => exactly 100%, never above.
        assert!((hw_error_percent(5, 0, 0) - 100.0).abs() < 1e-9);
        // Some rejects too: 20 err / (70 acc + 10 rej + 20 err = 100) = 20%.
        assert!((hw_error_percent(20, 70, 10) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn autotuner_and_thermal_families_emitted_when_present() {
        // W17 fleet/Grafana parity: the W9/W15 autotuner-silicon + Wave-G
        // chip-imbalance telemetry must appear in the exposition when present.
        let txt = full_snapshot().to_exposition();
        let families = assert_parseable(&txt);
        for must in [
            "dcentrald_autotuner_enabled",
            "dcentrald_autotuner_percent_complete",
            "dcentrald_autotuner_active_chips",
            "dcentrald_autotuner_total_chips",
            "dcentrald_silicon_grade_chips",
            "dcentrald_thermal_supervisor_enabled",
            "dcentrald_chip_imbalance_worst_celsius",
            "dcentrald_chip_imbalance_threshold_celsius",
            "dcentrald_chip_imbalance_flagged",
        ] {
            assert!(families.contains(must), "missing metric family: {must}");
        }
        assert!(txt.contains("dcentrald_autotuner_enabled 1"));
        assert!(txt.contains("dcentrald_autotuner_percent_complete 87.5"));
        assert!(txt.contains("dcentrald_silicon_grade_chips{grade=\"a\"} 180"));
        assert!(txt.contains("dcentrald_silicon_grade_chips{grade=\"d\"} 2"));
        assert!(txt.contains("dcentrald_chip_imbalance_worst_celsius 4.5"));
        assert!(txt.contains("dcentrald_chip_imbalance_threshold_celsius 8.0"));
        assert!(txt.contains("dcentrald_chip_imbalance_flagged 0"));
    }

    #[test]
    fn autotuner_and_thermal_detail_omitted_when_absent() {
        // Defaults: subsystems disabled, no telemetry. The detail families
        // (incl. the silicon grade distribution) must be ABSENT — never a
        // fabricated all-zero / all-grade-A distribution — while the two
        // subsystem `*_enabled` gauges are still emitted (0). The flagged
        // gauge is suppressed because the supervisor is disabled.
        let txt = PrometheusSnapshot::default().to_exposition();
        assert_parseable(&txt);
        assert!(txt.contains("dcentrald_autotuner_enabled 0"));
        assert!(txt.contains("dcentrald_thermal_supervisor_enabled 0"));
        assert!(!txt.contains("dcentrald_silicon_grade_chips"));
        assert!(!txt.contains("dcentrald_autotuner_percent_complete"));
        assert!(!txt.contains("dcentrald_autotuner_active_chips"));
        assert!(!txt.contains("dcentrald_chip_imbalance_worst_celsius"));
        assert!(!txt.contains("dcentrald_chip_imbalance_flagged"));
    }

    #[test]
    fn partial_telemetry_present_emits_enabled_and_flag_but_omits_missing_detail() {
        // Production-likely state: subsystems ENABLED but their measured
        // detail not available yet (autotuner running before characterization;
        // thermal supervisor running before 2 valid chip sensors are read).
        // The `*_enabled` + `*_flagged` gauges (gated on enabled) must emit,
        // while the Option-backed measured families stay OMITTED (never a
        // fabricated 0 / grade distribution).
        let mut s = full_snapshot();
        s.silicon_grade_counts = None; // autotuner on, not yet characterized
        s.chip_imbalance_worst_c = None; // supervisor on, < 2 chip sensors read
        let txt = s.to_exposition();
        assert_parseable(&txt);
        // enabled + flagged (gated on supervisor enabled) present:
        assert!(txt.contains("dcentrald_autotuner_enabled 1"));
        assert!(txt.contains("dcentrald_thermal_supervisor_enabled 1"));
        assert!(txt.contains("dcentrald_chip_imbalance_flagged 0"));
        // measured detail omitted (not fabricated):
        assert!(!txt.contains("dcentrald_silicon_grade_chips"));
        assert!(!txt.contains("dcentrald_chip_imbalance_worst_celsius"));
        // threshold is a static config value, still present while enabled:
        assert!(txt.contains("dcentrald_chip_imbalance_threshold_celsius"));
    }

    #[test]
    fn p2_6_per_pool_share_counters_and_donation_and_integration_emitted() {
        // P2-6 §4.C: per-{pool,worker} share counters, the donation-active
        // gauge, and the per-kind integration_up gauge must all appear and be
        // well-formed when populated.
        let txt = full_snapshot().to_exposition();
        let families = assert_parseable(&txt);
        for must in [
            "dcentrald_pool_shares_accepted_total",
            "dcentrald_pool_shares_rejected_total",
            "dcentrald_donation_active",
            "dcentrald_integration_up",
        ] {
            assert!(families.contains(must), "missing metric family: {must}");
        }
        // Labeled per-pool counters carry BOTH the pool and worker labels and
        // mirror the global totals (accepted 40 / rejected 1).
        assert!(txt.contains(
            "dcentrald_pool_shares_accepted_total{pool=\"stratum+tcp://pool.example.com:3333\",worker=\"bc1qex\u{2026}rig1\"} 40"
        ));
        assert!(txt.contains(
            "dcentrald_pool_shares_rejected_total{pool=\"stratum+tcp://pool.example.com:3333\",worker=\"bc1qex\u{2026}rig1\"} 1"
        ));
        // OBS-1 regression: a raw bech32-style wallet must never reach the
        // exposition — the worker label is masked upstream.
        assert!(!txt.contains("bc1qexampleworker"));
        // Donation active => 1.
        assert!(txt.contains("dcentrald_donation_active 1"));
        // mqtt up=Some(true) => 1; webhook up=Some(false) => 0; both present.
        assert!(txt.contains("dcentrald_integration_up{kind=\"mqtt\"} 1"));
        assert!(txt.contains("dcentrald_integration_up{kind=\"webhook\"} 0"));
    }

    #[test]
    fn p2_6_per_pool_counters_omitted_without_pool_label() {
        // No configured pool => no fabricated empty-label series. The global
        // unlabeled counters still publish.
        let mut s = full_snapshot();
        s.pool_label = String::new();
        let txt = s.to_exposition();
        assert_parseable(&txt);
        assert!(!txt.contains("dcentrald_pool_shares_accepted_total"));
        assert!(!txt.contains("dcentrald_pool_shares_rejected_total"));
        // Global totals remain.
        assert!(txt.contains("dcentrald_shares_accepted_total 40"));
        assert!(txt.contains("dcentrald_shares_rejected_total 1"));
    }

    #[test]
    fn p2_6_integration_family_omitted_when_no_integration_configured() {
        // Both integrations None (not configured) => the whole
        // `dcentrald_integration_up` family is omitted (no fabricated 0 for an
        // integration the operator never set up). Donation gauge always emits.
        let mut s = full_snapshot();
        s.mqtt_integration_up = None;
        s.webhook_integration_up = None;
        s.donation_active = false;
        let txt = s.to_exposition();
        assert_parseable(&txt);
        assert!(!txt.contains("dcentrald_integration_up"));
        assert!(txt.contains("dcentrald_donation_active 0"));
    }

    #[test]
    fn p2_6_integration_family_emits_only_configured_kinds() {
        // Only webhook configured => only the webhook sample appears; the mqtt
        // kind is absent (None omitted).
        let mut s = full_snapshot();
        s.mqtt_integration_up = None;
        s.webhook_integration_up = Some(true);
        let txt = s.to_exposition();
        assert_parseable(&txt);
        assert!(txt.contains("dcentrald_integration_up{kind=\"webhook\"} 1"));
        assert!(!txt.contains("dcentrald_integration_up{kind=\"mqtt\"}"));
    }

    #[test]
    fn p2_6_default_snapshot_omits_pool_and_integration_but_keeps_donation() {
        // The HAL-free default (cold daemon) must stay parseable: no pool label,
        // no integrations, donation gauge present and 0.
        let txt = PrometheusSnapshot::default().to_exposition();
        assert_parseable(&txt);
        assert!(!txt.contains("dcentrald_pool_shares_accepted_total"));
        assert!(!txt.contains("dcentrald_integration_up"));
        assert!(txt.contains("dcentrald_donation_active 0"));
    }

    #[test]
    fn label_values_are_escaped() {
        let mut s = full_snapshot();
        s.firmware_version = "0.6.0\"weird\\\nbuild".into();
        let txt = s.to_exposition();
        // Must remain parseable despite the nasty version string.
        assert_parseable(&txt);
        assert!(txt.contains("version=\"0.6.0\\\"weird\\\\\\nbuild\""));
    }

    #[test]
    fn empty_snapshot_still_parseable() {
        let s = PrometheusSnapshot::default();
        let txt = s.to_exposition();
        let fams = assert_parseable(&txt);
        // Even with no chains/fans the scalar families are present.
        assert!(fams.contains("dcentrald_hashrate_ghs"));
        assert!(fams.contains("dcentrald_info"));
        // Default chip model renders as "unknown".
        assert!(txt.contains("model=\"unknown\""));
    }

    #[test]
    fn content_type_constant_matches_version() {
        assert!(CONTENT_TYPE.contains(EXPOSITION_VERSION));
        assert_eq!(EXPOSITION_VERSION, "0.0.4");
    }

    #[test]
    fn snapshot_round_trips_through_serde_json() {
        let s = full_snapshot();
        let j = serde_json::to_string(&s).unwrap();
        let back: PrometheusSnapshot = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
