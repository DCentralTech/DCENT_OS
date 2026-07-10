//! Phase T: streaming Serialize-derive replacement for the /api/system/info
//! json!{} Value tree. Borrows from the handler's lock guards + computed locals
//! to eliminate per-request heap fragmentation.
//!
//! The OLD pattern allocated ~80 IndexMap nodes (each ~408 B align=8) per
//! request to build a `serde_json::Value` tree of 158 fields, then dropped
//! the whole tree after `to_writer`. Live captures (Phase F breadcrumb)
//! showed this was the dominant fragmentation source. With PSRAM
//! (Phase S) the device tolerates the churn, but Phase T eliminates the
//! waste entirely — the struct lives on the stack, all string fields are
//! `&'a str` borrows from the caller's lock guards, and serde_json walks
//! the struct fields directly into the response buffer.
//!
//! **Critical contract**: the JSON output of this struct MUST be
//! byte-identical to the legacy json!{} block at api.rs:984-1217. AxeOS-
//! compatible monitoring tools (BitAxeHQ, Swarm, etc.) consume it.

use crate::shared::{AutotunerState, ChipData};
use dcentaxe_stratum::CoinbaseOutput;
use serde::Serialize;

/// Top-level response struct — borrows from all lock guards held in the
/// handler. Lifetime `'a` ties to the shortest-lived guard.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfoResponse<'a> {
    // ── Power / Electrical ──
    pub power: f32,
    pub voltage: f32,
    pub current: f32,
    pub max_power: f32,
    pub nominal_voltage: u16,

    // ── Temperatures ──
    pub temp: f32,
    pub temp2: f32,
    #[serde(rename = "vrTemp")]
    pub vr_temp: f32,
    pub sensors_ok: bool,

    // ── Derived metrics ──
    pub uptime: u64,
    pub wifi_rssi: i8,
    pub efficiency: f64,
    /// Share-acceptance rate as a percentage in `[0.0, 100.0]` — a DCENT-original
    /// field (NOT an AxeOS key). When no shares have resolved yet this carries the
    /// documented sentinel `-1.0` (out of the percentage range) rather than a
    /// fabricated `100.0`; pair it with `acceptance_rate_known` to distinguish
    /// "no data" from a real 100% accept rate. See `derived_metrics::acceptance_rate_pct`.
    pub acceptance_rate: f64,
    /// data-honesty (M-dash-1): `true` once at least one share has resolved
    /// (`accepted + rejected > 0`), so `acceptance_rate` is a real measurement;
    /// `false` on a freshly-booted miner with zero confirmed shares (where
    /// `acceptance_rate` is the `-1.0` unknown sentinel). Additive companion —
    /// does NOT touch any AxeOS-compat wire key. Serialized as `acceptanceRateKnown`.
    pub acceptance_rate_known: bool,

    // ── Hashrate ──
    #[serde(rename = "hashRate")]
    pub hash_rate: f64,
    #[serde(rename = "hashRate_1m")]
    pub hash_rate_1m: f64,
    #[serde(rename = "hashRate_5m")]
    pub hash_rate_5m: f64,
    #[serde(rename = "hashRate_10m")]
    pub hash_rate_10m: f64,
    #[serde(rename = "hashRate_15m")]
    pub hash_rate_15m: f64,
    #[serde(rename = "hashRate_1h")]
    pub hash_rate_1h: Option<f64>, // always None to match current null
    pub expected_hashrate: f64,
    pub error_percentage: f64,

    // ── Difficulty ──
    pub best_diff: u64,
    pub best_session_diff: u64,
    pub best_ever_diff: u64,
    pub pool_difficulty: f64,
    pub block_height: u32,

    // ── Stratum status ──
    pub pool_connected: bool,
    pub is_using_fallback_stratum: u8,
    pub pool_connection_info: &'a str,

    // ── Memory ──
    #[serde(rename = "isPSRAMAvailable")]
    pub is_psram_available: u8,
    pub free_heap: u32,
    pub free_heap_internal: usize,
    pub free_heap_spiram: usize,

    // ── Voltage / Frequency ──
    pub core_voltage: u16,
    pub core_voltage_actual: u16,
    pub frequency: f32,

    // ── WiFi ──
    pub ssid: &'a str,
    pub mac_addr: &'a str,
    pub hostname: &'a str,
    pub ipv4: &'a str,
    pub ipv6: &'static str,
    pub wifi_status: &'static str,
    #[serde(rename = "wifiRSSI")]
    pub wifi_rssi_alias: i8,
    pub ap_enabled: u8,

    // ── Shares ──
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_rejected_reasons: &'a [String],
    pub stale_nonces: u64,
    pub slot_recoveries: u64,

    // ── Uptime ──
    pub uptime_seconds: u64,

    // ── ASIC info ──
    pub asic_count: u32,
    pub core_count: u32,
    pub small_core_count: u32,
    pub dcent_total_small_core_count: u32,
    #[serde(rename = "ASICModel")]
    pub asic_model: &'a str,
    pub device_model: &'a str,
    pub swarm_color: &'a str,

    // ── Primary stratum config ──
    #[serde(rename = "stratumURL")]
    pub stratum_url: &'a str,
    pub stratum_port: u16,
    pub stratum_user: &'a str,
    pub stratum_suggested_difficulty: u32,
    pub stratum_extranonce_subscribe: u8,
    #[serde(rename = "stratumTLS")]
    pub stratum_tls: u8,
    pub stratum_cert: &'static str,
    pub stratum_decode_coinbase: u8,
    pub stratum_protocol: &'static str,
    pub stratum_v2_available: bool,
    pub stratum_v2_experimental: bool,

    // ── Fallback stratum ──
    #[serde(rename = "fallbackStratumURL")]
    pub fallback_stratum_url: &'a str,
    pub fallback_stratum_port: u16,
    pub fallback_stratum_user: &'a str,
    pub fallback_stratum_suggested_difficulty: u32,
    pub fallback_stratum_extranonce_subscribe: u8,
    #[serde(rename = "fallbackStratumTLS")]
    pub fallback_stratum_tls: u8,
    pub fallback_stratum_cert: &'static str,
    pub fallback_stratum_decode_coinbase: u8,

    // ── Response time ──
    pub response_time: f64,

    // ── Firmware / build ──
    pub version: &'static str,
    #[serde(rename = "axeOSVersion")]
    pub axeos_version: &'static str,
    pub git_hash: &'static str,
    pub git_dirty: bool,
    pub build_epoch: u64,
    pub has_bap: bool,
    pub display_name: &'static str,
    pub idf_version: &'a str,
    pub board_version: &'a str,
    pub board_version_recognized: bool,
    pub support_status: &'a str,
    pub board_target: &'static str,
    pub reset_reason: &'a str,
    pub safe_mode: bool,
    pub wdt_reset_count: u8,
    pub coredump_present: bool,
    /// Optional — if `None`, omitted from JSON output to keep wire-format
    /// byte-identical to the legacy json!{} block (which never emitted these).
    /// A future handler can populate them from NVS to surface the last panic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_panic: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_restart_reason: Option<serde_json::Value>,
    pub running_partition: &'a str,
    pub scriptsig: &'a str,
    pub network_difficulty: u8,
    pub coinbase_outputs: &'a [CoinbaseOutput],
    pub coinbase_value_total_satoshis: u64,
    pub coinbase_value_user_satoshis: u64,

    // ── Display / Screen ──
    #[serde(rename = "overheat_mode")]
    pub overheat_mode: u8,
    pub overclock_enabled: u8,
    pub display: &'static str,
    pub rotation: u8,
    pub invertscreen: u8,
    pub display_timeout: u8,

    // ── Fan ──
    pub autofanspeed: u8,
    pub fanspeed: f32,
    pub manual_fan_speed: u8,
    pub min_fan_speed: u8,
    pub temptarget: u8,
    pub fanrpm: u32,
    pub fan2rpm: u32,

    // ── Stats / Block ──
    pub stats_frequency: u8,
    pub block_found: u8,
    pub show_new_block: bool,

    // ── Hashrate monitor ──
    pub hashrate_monitor: HashrateMonitor<'a>,

    // ── DCENT extensions ──
    pub firmware_type: &'static str,
    pub dcent_swarm: &'a dcent_schema::swarm::DcentSwarmInfo,
    pub dcentaxe: DcentaxeExt<'a>,
    /// MQTT + Home Assistant auto-discovery config (read surface). The broker
    /// password is NEVER serialized — only `password_set: bool`, mirroring the
    /// wifi/stratum redaction posture.
    pub mqtt: MqttView<'a>,

    /// On-board LoRa mesh status (feature-gated, additive). Present ⇒ the dashboard
    /// renders the inline mesh panel; `present:false` keeps it at "Radio pending"
    /// (honesty-gated — no live claim before on-hardware radio proof). Omitted
    /// entirely (no `lora` key, `skip_serializing_if`) until the radio task has
    /// published a snapshot, and the field itself is compiled out when the `lora`
    /// feature is OFF ⇒ byte-identical wire format for a non-LoRa image.
    #[cfg(feature = "lora")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lora: Option<LoraInfoView>,
}

/// Additive read-only view of the on-board LoRa mesh for `/api/system/info`.
/// Compiled only under the `lora` feature. All fields are derived from the live
/// `lora_task` snapshot; nothing here is fabricated when the radio is not proven
/// (`present:false` + null beacon/RSSI).
#[cfg(feature = "lora")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoraInfoView {
    /// Radio cold-boot proven on hardware (gates the dashboard panel).
    pub present: bool,
    /// "eu868" | "na915".
    pub region: &'static str,
    /// "uninitialized" | "standby" | "rx" | "tx" | "fault".
    pub radio_state: &'static str,
    pub peer_count: u32,
    pub last_beacon_unix_ms: Option<u64>,
    pub last_rx_rssi_dbm: Option<i16>,
}

/// Read-only MQTT config view for `/api/system/info`. The password is masked to
/// `password_set` so the dashboard can render the field state without ever
/// exposing the secret on the wire.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MqttView<'a> {
    pub enabled: bool,
    /// Whether the opt-in HA command surface (number/select/climate entities) is
    /// armed (`config.mqtt.commands_enabled`, default-OFF). Read-only visibility
    /// so tooling can tell whether the device will accept HA setpoint writes; no
    /// secret is exposed. (ES-6)
    pub commands_enabled: bool,
    pub broker_host: &'a str,
    pub broker_port: u16,
    pub username: &'a str,
    pub password_set: bool,
    pub tls: bool,
    pub publish_interval_s: u16,
}

/// hashrate_monitor mirrors the legacy json! shape exactly. The inner asics
/// array is a `Vec<serde_json::Value>` produced by the existing handler-side
/// helper (see api.rs `let hashrate_monitor_asics = ...` above) — keep that
/// pre-computation logic unchanged. Each entry has `{total, domains: [...],
/// errorCount}` when populated, OR an empty array if any chip is missing
/// hashrate_ghs.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashrateMonitor<'a> {
    pub asics: &'a [serde_json::Value],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DcentaxeExt<'a> {
    pub runtime_board_target: &'a str,
    pub build_board_target: &'static str,
    pub runtime_device_model: &'a str,
    pub build_device_model: &'a str,
    pub build_board_version: &'static str,
    pub autotuner: AutotunerView<'a>,
    pub power_limits: PowerLimitsView,
    pub schedule: serde_json::Value,
    pub ota: OtaView<'a>,
    pub pool_truth: PoolTruthView<'a>,
    pub dispatcher: DispatcherView,
    pub board_temp: f32,
    pub inlet_temp: f32,
    pub outlet_temp: f32,
    pub input_voltage: f32,
    pub achievements: u32,
    pub achievement_count: u32,
    pub lifetime_shares: u32,
    pub creature_stage: &'a str,
    pub creature_mood: u8,
    pub mining_enabled: bool,
    pub chips: ChipsView<'a>,
    pub voltage_domains: serde_json::Value,
    pub chip_health: serde_json::Value,
    /// data-model-fields §2/§4: power provenance + calibration honesty envelope.
    /// Additive `dcentaxe.power` object — does NOT touch the AxeOS-compat
    /// top-level `power`/`voltage`/`current` keys (which stay measured INA260).
    pub power: DcentaxePowerView,
    /// data-model-fields §1: temperature-provenance token, DERIVED from the
    /// existing `chip_temp_is_ambient_proxy` + `sensors_ok` booleans (no new
    /// measurement). `board_sensor` when a real read is available and not a
    /// proxy; `ambient_proxy` when the EMC2101 internal-die proxy is in use;
    /// omitted (honest null) when no temperature is available at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp_source: Option<&'static str>,
}

/// data-model-fields §2/§4: additive power-provenance + calibration honesty
/// object under `dcentaxe.power`. `source` ∈ {measured, estimated} carries the
/// §0 provenance enum; `calibrated` is the §4 honesty flag. axe has no
/// operator wall-meter / electricity-rate input, so any wall-watts / sats / cost
/// estimate is `estimated` + `calibrated:false` and STAYS false until a future
/// calibration endpoint lands — the contract forbids claiming `calibrated:true`.
/// `network_difficulty` pairs the economic estimate with the difficulty it was
/// anchored to (mirrors OS so the same canonical model runs on both); it reuses
/// the already-on-wire top-level `network_difficulty:u8`, not a second source.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DcentaxePowerView {
    /// Provenance of the board watts axe actually measures (INA260) — `measured`.
    pub source: &'static str,
    /// Whether the *estimated economic* values (wall-watts/sats/cost) are
    /// operator-calibrated. Always `false` today (no wall-meter input).
    pub calibrated: bool,
    /// Provenance of any wall-watts / sats / cost estimate axe derives from the
    /// measured board watts — `estimated` (no wall-meter), per §2.
    pub estimate_source: &'static str,
    /// The network difficulty the economic estimate is anchored to (the §4
    /// pairing; mirrors the existing top-level `networkDifficulty:u8`).
    pub network_difficulty: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutotunerView<'a> {
    pub enabled: bool,
    pub mode: &'a str, // pre-formatted in handler (Debug fmt)
    pub target_value: f32,
    pub current_frequency: f32,
    pub current_voltage_mv: u16,
    pub best_efficiency: f32,
    pub hashrate15s: f64,
    pub hashrate30s: f64,
    pub last_good_frequency: f32,
    pub last_good_voltage_mv: u16,
    pub last_good_jth: f32,
    pub last_good_error_rate: f32,
    pub silicon_grade: &'a str,
    pub status: &'a str,
    /// data-model-fields §7.4(b): stable autotuner-stage token
    /// (warmup|profiling|wattage_descent|optimizing|maintaining|idle) that the
    /// shared `COMP-AUTOTUNER` phase-ribbon maps into its canonical rung enum,
    /// without the brittle prefix-matching that deriving from `status` would
    /// require. Serialized as `dcentaxe.autotuner.phase`.
    pub phase: &'a str,
}

impl<'a> AutotunerView<'a> {
    /// Helper to construct from an &AutotunerState + pre-formatted mode string.
    pub fn from_state(
        autotune: &'a AutotunerState,
        mode_str: &'a str,
        hr15s: f64,
        hr30s: f64,
    ) -> Self {
        Self {
            enabled: autotune.enabled,
            mode: mode_str,
            target_value: autotune.target_value,
            current_frequency: autotune.current_frequency,
            current_voltage_mv: autotune.current_voltage_mv,
            best_efficiency: autotune.best_efficiency,
            hashrate15s: hr15s,
            hashrate30s: hr30s,
            last_good_frequency: autotune.last_good_frequency,
            last_good_voltage_mv: autotune.last_good_voltage_mv,
            last_good_jth: autotune.last_good_jth,
            last_good_error_rate: autotune.last_good_error_rate,
            silicon_grade: &autotune.silicon_grade,
            status: &autotune.status,
            phase: &autotune.phase,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerLimitsView {
    pub max_power_w: f32,
    pub max_current_a: f32,
    pub max_frequency: f32,
    pub max_voltage_mv: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OtaView<'a> {
    pub signature_capable: bool,
    pub signature_required: bool,
    pub allow_unsigned: bool,
    pub key_id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolTruthView<'a> {
    pub connected: bool,
    pub difficulty: f64,
    pub shares_submitted: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub shares_pending: u32,
    pub shares_unresolved: u64,
    pub oldest_pending_submit_age_ms: u64,
    pub response_time_ms: f64,
    pub last_share_submit_unix_ms: u64,
    pub last_share_response_unix_ms: u64,
    pub last_share_accepted_unix_ms: u64,
    pub last_share_rejected_unix_ms: u64,
    pub failover_active: bool,
    pub primary_failback_state: dcentaxe_stratum::PrimaryFailbackState,
    pub primary_failback_detail: &'a str,
    pub last_primary_reprobe_unix_ms: u64,
    pub last_primary_failback_unix_ms: u64,
    pub last_reject_reason: &'a str,
    pub active_pool: &'a str,
    pub protocol: &'a str,
    pub reject_reason_counts: &'a [dcentaxe_stratum::RejectReasonCount],
    pub recent_events: &'a [dcentaxe_stratum::StratumEventRecord],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatcherView {
    pub stale_nonces: u64,
    pub slot_recoveries: u64,
    pub filtered_nonces: u64,
    pub nonces_found: u64,
    pub ticket_difficulty: f64,
}

/// Phase R streaming chips serializer — moved here from api.rs so the
/// dcentaxe sub-struct can reference it via this module.
pub struct ChipsView<'a>(pub &'a [ChipData]);

impl<'a> Serialize for ChipsView<'a> {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        let mut seq = ser.serialize_seq(Some(self.0.len()))?;
        for (i, c) in self.0.iter().enumerate() {
            struct ChipEntry<'b> {
                id: usize,
                c: &'b ChipData,
            }
            impl<'b> Serialize for ChipEntry<'b> {
                fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                    let mut m = ser.serialize_map(Some(6))?;
                    m.serialize_entry("id", &self.id)?;
                    m.serialize_entry("temp", &self.c.temp_c)?;
                    m.serialize_entry("status", self.c.status_str())?;
                    m.serialize_entry("hwErrors", &self.c.hw_errors)?;
                    m.serialize_entry("shares", &self.c.shares)?;
                    m.serialize_entry("hashrate", &self.c.hashrate_ghs)?;
                    m.end()
                }
            }
            seq.serialize_element(&ChipEntry { id: i, c })?;
        }
        seq.end()
    }
}
