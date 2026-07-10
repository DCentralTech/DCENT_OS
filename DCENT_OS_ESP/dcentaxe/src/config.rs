// DCENT_axe Configuration
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0

use dcentaxe_hal::board::{
    BitAxeModel, BoardConfig, BoardHardwareConfig, BoardVersionProfile, FanControllerKind,
    PowerControllerKind, TempSensorKind,
};
use dcentaxe_stratum::StratumConfig;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

fn default_schedule_entry_enabled() -> bool {
    true
}

pub(crate) fn default_model_for_build() -> BitAxeModel {
    if cfg!(feature = "bitaxe-gt-touch") {
        BitAxeModel::GtTouch
    } else if cfg!(feature = "bitaxe-touch") {
        BitAxeModel::Touch
    } else if cfg!(feature = "bitaxe-max") {
        BitAxeModel::Max
    } else if cfg!(feature = "bitaxe-ultra") {
        BitAxeModel::Ultra
    } else if cfg!(feature = "bitaxe-supra") {
        BitAxeModel::Supra
    } else if cfg!(feature = "bitaxe-gamma-duo") {
        BitAxeModel::GammaDuo
    } else if cfg!(feature = "bitaxe-gamma") {
        BitAxeModel::Gamma
    } else if cfg!(feature = "bitaxe-gt") {
        BitAxeModel::GammaTurbo
    } else if cfg!(feature = "bitaxe-hex-ultra") {
        BitAxeModel::HexUltra
    } else if cfg!(feature = "bitaxe-hex-supra") {
        BitAxeModel::HexSupra
    } else if cfg!(feature = "nerdnos") {
        BitAxeModel::NerdNOS
    } else if cfg!(feature = "nerdaxe") {
        BitAxeModel::NerdAxe
    } else if cfg!(feature = "nerdqaxe-plus") {
        BitAxeModel::NerdQaxePlus
    } else if cfg!(feature = "nerdqaxe-pp") {
        BitAxeModel::NerdQaxePP
    } else if cfg!(feature = "dcent-axe-bm1397") {
        BitAxeModel::DcentAxeBm1397
    } else if cfg!(feature = "dcent-axe-quad-bm1397") {
        BitAxeModel::DcentAxeQuadBm1397
    } else if cfg!(feature = "dcent-axe-hex-bm1397") {
        BitAxeModel::DcentAxeHexBm1397
    } else {
        BitAxeModel::Gamma
    }
}

pub(crate) fn default_profile_for_build() -> &'static BoardVersionProfile {
    BoardVersionProfile::default_for_model(default_model_for_build())
}

// ── CFG-7 — single source of truth for the fan auto-control default ──────────
//
// Three sites used to hardcode the fan target temp independently
// (`DcentAxeConfig::default`, `migrate_axeos_config`, and the provisioning
// `build_submission`). The value `0` means MANUAL fan mode (runtime checks
// `cfg.fan_target_temp_c == 0` and falls back to `fan_speed_pct`); any non-zero
// value enables the auto curve at that target °C. `0` is the maximally
// default-preserving reconciliation: factory-default and AxeOS migration already
// used `0`, so centralizing on `0` changes only the provisioning outlier (which
// previously diverged at 65) and never alters the persisted factory default.
//
// Operator policy lever: flipping this to ~60-65 adopts the  home-unit
// "quiet-first" auto-curve posture fleet-wide in ONE line — and because all three
// paths now reference this const, they move together and can never drift again.
pub const DEFAULT_FAN_TARGET_TEMP_C: u8 = 0;

// ── CFG-5 — single classifier for BIP320/ASICBoost version-rolling ───────────
//
// The BM1397 (BitAxe Max) does NOT support BIP320 version rolling; every other
// supported chip (BM1366/1368/1370) does. This used to be re-derived at two
// sites with divergent sources of truth (`migrate_axeos_config` keyed off the
// RESOLVED profile while persisting the STORED asic_model string;
// `build_submission` hardcoded `true`). Centralize the decision so both compute
// it from the SAME final asic_model string they persist, eliminating the case
// where a stored asicmodel and a board_version-resolved profile disagree. The
// magic string "BM1397" is preserved verbatim.
pub fn chip_rolls_versions(asic_model: &str) -> bool {
    asic_model.trim() != "BM1397"
}

// ── CFG-2 — bounded body-accumulation decision helpers ───────────────────────
//
// The provisioning POST handler must accumulate a possibly-multi-segment request
// body up to a hard cap (`nvs_config::MAX_CONFIG_SIZE`) instead of doing a single
// fixed-size `req.read`, which embedded-io can truncate. The req I/O loop itself
// is esp-idf and review-only, but the accept/overflow DECISION is pure and pinned
// here so the cap boundary is host-tested.

/// True iff `incoming` more bytes can be appended to a buffer that already holds
/// `received` bytes without exceeding `max`. Used to REJECT (not silently
/// truncate) an over-cap body.
pub fn body_read_capacity_ok(received: usize, incoming: usize, max: usize) -> bool {
    received.saturating_add(incoming) <= max
}

/// Clamp the next per-read request length so it never overshoots the remaining
/// capacity (`max - received`). Returns 0 once the cap is reached, which the
/// loop treats as "stop reading / reject if more data is pending".
pub fn next_take(received: usize, max: usize) -> usize {
    max.saturating_sub(received)
}

// ── CFG-3 — NVS blob read-back-verify decision ───────────────────────────────
//
// `save_config` writes the config blob then reads it straight back and compares
// byte-for-byte, surfacing a torn/short write as an error rather than a silent
// "saved OK". The byte-compare is pure and pinned here.

/// True iff the bytes read back from NVS exactly equal the bytes written
/// (length + content). A short/truncated or mutated read-back verifies false.
pub fn blob_write_verified(written: &[u8], read_back: &[u8]) -> bool {
    written == read_back
}

// ── CFG-10 — config-schema forward/backward version decision ─────────────────
//
// `migrate_config` must make an explicit decision when a stored blob's
// `schema_version` differs from the firmware's `SCHEMA_VERSION`, including the
// downgrade case where a FUTURE blob (stored > current) is read by older
// firmware. The decision is pure and pinned here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaAction {
    /// Stored version equals firmware version — nothing to do.
    Current,
    /// Stored version is OLDER than firmware — run additive upgrade arms.
    MigrateForward(u8),
    /// Stored version is NEWER than firmware (a downgrade) — refuse to trust the
    /// future shape; clamp the marker and rely on serde to have dropped unknown
    /// fields.
    RefuseFuture,
}

/// Classify a stored schema version against the firmware's current version.
pub fn schema_action(stored: u8, current: u8) -> SchemaAction {
    use std::cmp::Ordering;
    match stored.cmp(&current) {
        Ordering::Equal => SchemaAction::Current,
        Ordering::Less => SchemaAction::MigrateForward(stored),
        Ordering::Greater => SchemaAction::RefuseFuture,
    }
}

/// Bring a just-loaded config up to the current schema version.
///
/// Per-version steps go here. The function is a no-op when the stored blob
/// already matches `SCHEMA_VERSION`. Keep the arms additive — old firmware
/// may still be running in the field, so migrations must stay idempotent.
///
/// CFG-10 (moved here 2026-06-29): the real forward-migration MUTATION now lives
/// in this host-compiled `config` module (the single-source `#[path]` pattern)
/// so it is unit-tested on the host gate, not only string-matched. The NVS
/// loader (`nvs_config::load_config`, which pulls in `esp-idf-svc` and so cannot
/// host-compile) calls `crate::config::migrate_config` on every loaded blob.
pub fn migrate_config(config: &mut DcentAxeConfig) {
    let from = config.schema_version;
    // CFG-10: make the forward/backward decision explicit, including the
    // downgrade case (a FUTURE blob whose schema_version > SCHEMA_VERSION read
    // by older firmware) which the old code loaded as-is.
    match schema_action(from, SCHEMA_VERSION) {
        SchemaAction::Current => {}
        SchemaAction::MigrateForward(_) => {
            // Schema 0 → 1 — introduce the `schema_version` field itself.
            // Any config written by a pre-schema build deserialises with
            // schema_version = 0 (serde default), so we just stamp the current
            // version. No data shape changes required.
            if from < 1 {
                log::info!("NVS: migrating config schema {} → 1", from);
                config.schema_version = 1;
            }
            // Future additive upgrade arms: if from < 2 { ... }

            // Defensive: if a future arm forgets to advance the marker, stamp it
            // so the loader never believes it round-tripped an older shape.
            if config.schema_version < SCHEMA_VERSION {
                config.schema_version = SCHEMA_VERSION;
            }
        }
        SchemaAction::RefuseFuture => {
            // A blob written by NEWER firmware. We cannot know its added fields;
            // serde has already dropped any keys our struct doesn't declare
            // during deserialize. Clamp the stored marker DOWN to our current
            // version so the loader never claims to round-trip the future shape
            // (least-destructive: known fields — WiFi creds, stratum — survive).
            // The stricter escalation, when a real field-MEANING change lands, is
            // to treat the affected subset as first-boot / re-provision.
            log::warn!(
                "NVS: config schema_version={} is NEWER than firmware {} — refusing \
                 future shape, clamping marker (known fields preserved)",
                from,
                SCHEMA_VERSION
            );
            config.schema_version = SCHEMA_VERSION;
        }
    }
}

// ── CFG-12 — reject an unconnectable pool endpoint at provisioning time ───────
//
// `build_submission` validated `wifi_ssid` and `worker` but did NO check on the
// pool endpoint. The JSON path defaults an absent `pool_url` to "" and BOTH
// paths accept `pool_port == 0` (form `"0".parse().unwrap_or(...)` → 0; JSON
// `get_u16` → 0 when absent). Those raw values were saved to NVS and the captive
// portal rendered "Configuration Saved!" even though the miner can NEVER connect
// (`StratumClient::check_pool_reachable` builds "host:port" and `TcpStream`
// connects). That is a silent-failure + dishonest-success defect.
//
// Reject the two unconnectable shapes at SUBMIT time so the caller surfaces the
// error (HTTP 400) instead of a false success. Conservative on purpose: it
// accepts every shape the live endpoint parser
// (`dcentaxe_stratum::endpoint_host_from_url`) resolves to a non-empty host —
// bare `host`, `host:port`, `stratum+tcp://host`, `sv2://host`, `user:pass@host`,
// `host/path`, etc. — and ONLY adds three rejects:
//   * empty / whitespace-only url               → reject
//   * port == 0                                 → reject
//   * a scheme with no host (e.g. "stratum+tcp://") → reject
// It deliberately does NOT reject a url merely containing a space or other
// "looks odd" shapes: DNS / `connect` is the real authority and over-rejection
// would lock a legitimate operator value out of provisioning.
pub fn validate_pool_endpoint(url: &str, port: u16) -> Result<(), String> {
    if url.trim().is_empty() {
        return Err("Pool URL is required".to_string());
    }
    if port == 0 {
        return Err("Pool port must be non-zero".to_string());
    }
    if dcentaxe_stratum::endpoint_host_from_url(url).is_empty() {
        return Err("Pool URL has no host (scheme without a hostname)".to_string());
    }
    Ok(())
}

// ── CFG-8 — UTF-8-correct percent/`+` URL decoder ────────────────────────────
//
// The provisioning form path (`application/x-www-form-urlencoded`) decodes SSIDs
// / passwords / pool URLs. The old decoder pushed each percent-decoded byte as a
// Unicode scalar (`byte as char`, Latin-1), corrupting any multi-byte UTF-8
// value encoded as several `%XX`. Decode into a byte buffer and reassemble UTF-8
// at the end via `from_utf8_lossy`, so multi-byte sequences round-trip and a
// malformed sequence yields the replacement char instead of mojibake. ASCII
// input is byte-identical to the old behavior. Pure logic, host-tested.
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                // Need exactly two hex digits following '%'.
                if i + 2 < bytes.len() {
                    let hi = (bytes[i + 1] as char).to_digit(16);
                    let lo = (bytes[i + 2] as char).to_digit(16);
                    if let (Some(hi), Some(lo)) = (hi, lo) {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                        continue;
                    }
                }
                // Malformed or truncated `%XX` — emit the literal bytes we saw so
                // the value is not silently lost (mirrors the old fallback).
                out.push(b'%');
                if i + 1 < bytes.len() {
                    out.push(bytes[i + 1]);
                    if i + 2 < bytes.len() {
                        out.push(bytes[i + 2]);
                        i += 3;
                    } else {
                        i += 2;
                    }
                } else {
                    i += 1;
                }
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, Copy)]
pub struct StockAsicSettings {
    pub default_frequency: u16,
    pub frequency_options: &'static [u16],
    pub default_voltage_mv: u16,
    pub voltage_options: &'static [u16],
}

const BM1397_FREQUENCIES: &[u16] = &[400, 425, 450, 475, 485, 500, 525, 550, 575, 600];
const BM1397_VOLTAGES: &[u16] = &[1100, 1150, 1200, 1250, 1300, 1350, 1400, 1450, 1500];
const BM1366_FREQUENCIES: &[u16] = &[400, 425, 450, 475, 485, 500, 525, 550, 575];
const BM1366_VOLTAGES: &[u16] = &[1100, 1150, 1200, 1250, 1300];
const BM1368_FREQUENCIES: &[u16] = &[400, 425, 450, 475, 485, 490, 500, 525, 550, 575];
const BM1368_VOLTAGES: &[u16] = &[1100, 1150, 1166, 1200, 1250, 1300];
const BM1370_FREQUENCIES: &[u16] = &[400, 490, 525, 550, 600, 625];
const BM1370XP_FREQUENCIES: &[u16] = &[350, 375, 380, 400, 410];
const BM1370_VOLTAGES: &[u16] = &[1000, 1060, 1100, 1150, 1200, 1250];

pub fn stock_asic_settings(model: BitAxeModel) -> StockAsicSettings {
    match model {
        BitAxeModel::Max
        | BitAxeModel::DcentAxeBm1397
        | BitAxeModel::DcentAxeQuadBm1397
        | BitAxeModel::DcentAxeHexBm1397 => StockAsicSettings {
            default_frequency: 425,
            frequency_options: BM1397_FREQUENCIES,
            default_voltage_mv: 1400,
            voltage_options: BM1397_VOLTAGES,
        },
        BitAxeModel::Ultra | BitAxeModel::HexUltra => StockAsicSettings {
            default_frequency: 485,
            frequency_options: BM1366_FREQUENCIES,
            default_voltage_mv: 1200,
            voltage_options: BM1366_VOLTAGES,
        },
        BitAxeModel::Supra | BitAxeModel::HexSupra | BitAxeModel::NerdQaxePlus => {
            StockAsicSettings {
                default_frequency: 490,
                frequency_options: BM1368_FREQUENCIES,
                default_voltage_mv: 1166,
                voltage_options: BM1368_VOLTAGES,
            }
        }
        BitAxeModel::Gamma
        | BitAxeModel::GammaTurbo
        | BitAxeModel::Touch
        | BitAxeModel::GtTouch
        | BitAxeModel::NerdAxe
        | BitAxeModel::NerdQaxePP => StockAsicSettings {
            default_frequency: 525,
            frequency_options: BM1370_FREQUENCIES,
            default_voltage_mv: 1150,
            voltage_options: BM1370_VOLTAGES,
        },
        BitAxeModel::GammaDuo => StockAsicSettings {
            default_frequency: 400,
            frequency_options: BM1370XP_FREQUENCIES,
            default_voltage_mv: 1150,
            voltage_options: BM1370_VOLTAGES,
        },
        BitAxeModel::NerdNOS => StockAsicSettings {
            default_frequency: 400,
            frequency_options: &[300, 400],
            default_voltage_mv: 1200,
            voltage_options: &[1200],
        },
    }
}

/// Full DCENT_axe configuration.
///
/// Persisted to NVS as JSON. On first boot (no NVS config), the captive
/// portal collects this from the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcentAxeConfig {
    /// WiFi SSID
    pub wifi_ssid: String,
    /// WiFi password.
    ///
    /// CFG-4 at-rest threat-model note: this PSK is persisted in cleartext in
    /// the default (unencrypted) NVS partition, alongside `stratum.password`.
    /// True at-rest protection is `CONFIG_NVS_ENCRYPTION` + ESP-IDF flash
    /// encryption / secure-boot configured in `sdkconfig.defaults` + eFuse —
    /// that is operator-gated hardware provisioning, NOT something these source
    /// files can deliver, so we do not claim it here. The mitigation that IS in
    /// effect: GET surfaces must keep redacting (e.g. `api.rs` exposes
    /// `password_set: bool` and logs "redacted", never the value), and the
    /// provisioning path must never log the user PSK/pool password.
    pub wifi_password: String,
    /// Pool/Stratum configuration
    pub stratum: StratumConfig,
    /// Board model (e.g., "gamma", "ultra", "max", "hexultra")
    pub board_model: String,
    /// Runtime board version read from AxeOS/ESP-Miner NVS when available.
    #[serde(default)]
    pub board_version: String,
    /// Runtime ASIC model read from AxeOS/ESP-Miner NVS when available.
    #[serde(default)]
    pub asic_model: String,
    /// User-configurable hostname (persisted to NVS)
    #[serde(default)]
    pub hostname: String,
    /// Target hash frequency (MHz)
    pub target_frequency: f32,
    /// Target core voltage (mV)
    pub target_voltage_mv: u16,
    /// Fan speed (0-100%)
    pub fan_speed_pct: u8,
    /// ASIC chip count (auto-detect if 0)
    pub asic_count: u8,
    /// Overclocking mode enabled (assumes 5V 10A PSU instead of 5V 6A)
    #[serde(default)]
    pub overclock_enabled: bool,
    /// Display flipped 180 degrees
    #[serde(default)]
    pub display_inverted: bool,
    /// Fan auto-control target temperature (0 = manual mode, use fan_speed_pct)
    #[serde(default)]
    pub fan_target_temp_c: u8,
    /// Backup/fallback pool configuration
    #[serde(default)]
    pub fallback_pool: Option<dcentaxe_stratum::StratumConfig>,
    /// Optional secondary pool with hashrate splitting.
    /// If present, hashrate is split between primary (stratum) and this pool.
    /// Primary gets (100 - hashrate_pct)%, secondary gets hashrate_pct%.
    #[serde(default)]
    pub split_pool: Option<SplitPoolConfig>,
    /// SV2 own-template proxy hint. DCENT_axe still mines via a standard SV2
    /// endpoint; DCENT_OS or another proxy owns Template Distribution/JD.
    #[serde(default)]
    pub sv2_own_templates: Sv2OwnTemplateConfig,
    /// Optional pinned SV2 pool authority public key for the PRIMARY pool.
    ///
    /// Accepts a base58check token (`base58check([0x01,0x00] || pubkey32)`) or a
    /// full SV2 URL whose path carries it. When set, the SV2 Noise handshake
    /// verifies the server's certificate fail-closed (BIP340 Schnorr) — the
    /// MITM defense. Default `None` keeps trust-on-first-use (TOFU), so existing
    /// behavior is byte-identical when the operator does not pin a key. A
    /// malformed value logs a warning and falls back to TOFU (never bricks the
    /// connection). `#[serde(default)]` ⇒ legacy NVS blobs round-trip with no
    /// schema bump (same pattern as `fallback_pool`).
    #[serde(default)]
    pub sv2_authority_pubkey: Option<String>,
    /// MQTT + Home Assistant auto-discovery config (default-OFF, opt-in).
    /// `#[serde(default)]` ⇒ legacy NVS blobs round-trip with no schema bump.
    #[serde(default)]
    pub mqtt: MqttConfig,
    /// Enable the daily power/autotune schedule.
    #[serde(default = "default_true")]
    pub schedule_enabled: bool,
    /// Local timezone offset in minutes from UTC for schedule matching.
    /// The dashboard seeds this from the browser; firmware falls back to uptime
    /// if SNTP has not set a valid wall clock yet.
    #[serde(default)]
    pub schedule_timezone_offset_minutes: i16,
    /// Scheduled power profiles (e.g., low power at night)
    #[serde(default)]
    pub power_schedule: Vec<PowerSchedule>,
    /// Explicit ESP-Miner custom-board hardware override set.
    #[serde(default)]
    pub hardware: Option<BoardHardwareConfig>,
    /// Require bearer authentication for /metrics once the owner password is set.
    #[serde(default = "default_true")]
    pub metrics_require_auth: bool,
    /// Allow unsigned OTA uploads even when a signing key is compiled in.
    #[serde(default)]
    pub allow_unsigned_ota: bool,
    /// Which temperature input drives the Space Heater autotuner.
    /// `Local` — firmware reads its own chip temperature (default).
    /// `SwarmAverage` — average of peer-reported room temps (Queen decides).
    /// `External` — only the value last POSTed to `/api/swarm/room-temp`.
    #[serde(default)]
    pub room_temp_source: RoomTempSource,
    /// Config blob schema version. Bumped whenever we change the shape in a
    /// way that `#[serde(default)]` alone can't round-trip. Load path in
    /// `nvs_config::load_config` runs per-version migration steps; `0` or
    /// the current `SCHEMA_VERSION` both round-trip cleanly.
    #[serde(default)]
    pub schema_version: u8,
}

fn default_mqtt_port() -> u16 {
    1883
}

fn default_mqtt_publish_interval_s() -> u16 {
    30
}

/// MQTT + Home Assistant auto-discovery config (default-OFF, opt-in).
///
/// When `enabled`, the firmware connects (outbound) to the configured broker and
/// publishes HA MQTT discovery configs + periodic telemetry (see `mqtt_ha.rs` for
/// the payload schema and `mqtt.rs` for the transport). MQTT is publish-only and
/// fail-soft — it NEVER affects mining or the safety paths.
///
/// `password` at-rest threat-model note mirrors `wifi_password`/`stratum.password`:
/// it is persisted in cleartext in the default (unencrypted) NVS partition. The
/// mitigation in effect is the same — GET surfaces redact it (`/api/system/info`
/// exposes `password_set: bool`, `bitaxe://config` masks it to `***`, the apply
/// path logs "redacted"). True at-rest protection is operator-gated NVS/flash
/// encryption, not something this struct can deliver, so we do not claim it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttConfig {
    /// Master toggle. DEFAULT-OFF: a fresh/legacy unit publishes nothing.
    #[serde(default)]
    pub enabled: bool,
    /// Broker hostname or IP (no scheme; scheme is derived from `tls`).
    #[serde(default)]
    pub broker_host: String,
    /// Broker port (1883 plaintext / 8883 typical TLS).
    #[serde(default = "default_mqtt_port")]
    pub broker_port: u16,
    /// Optional broker username (empty = anonymous).
    #[serde(default)]
    pub username: String,
    /// Optional broker password (empty = none). Redacted on every read surface.
    #[serde(default)]
    pub password: String,
    /// Use TLS (`mqtts://`). Cert provisioning is operator/sdkconfig-gated; the
    /// proven path is a plaintext LAN broker. Default false.
    #[serde(default)]
    pub tls: bool,
    /// Telemetry publish cadence in seconds (clamped to a sane floor at runtime).
    #[serde(default = "default_mqtt_publish_interval_s")]
    pub publish_interval_s: u16,
    /// Opt-in operator-CONTROL surface. When true (default FALSE), the publisher
    /// ALSO advertises + subscribes the HA `number`/`select`/`climate` command
    /// entities (target watts / autotuner mode / target chip temperature) and
    /// applies inbound setpoints through the SAME clamped autotuner path the REST
    /// API uses (`mqtt_ha::parse_command` clamps every value to the safety
    /// envelope; the autotuner re-clamps freq/voltage). DEFAULT-OFF so a remote
    /// write surface is never exposed unless the operator explicitly enables it.
    /// `#[serde(default)]` ⇒ legacy NVS blobs load `false`.
    #[serde(default)]
    pub commands_enabled: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            broker_host: String::new(),
            broker_port: default_mqtt_port(),
            username: String::new(),
            password: String::new(),
            tls: false,
            publish_interval_s: default_mqtt_publish_interval_s(),
            commands_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Sv2OwnTemplateConfig {
    /// Whether the primary SV2 endpoint is intended to be a local template/JD proxy.
    #[serde(default)]
    pub enabled: bool,
    /// Standard SV2 mining endpoint exposed by the local proxy/DCENT_OS.
    #[serde(default)]
    pub mining_proxy_url: String,
    /// Optional Template Provider endpoint for operator visibility.
    #[serde(default)]
    pub template_provider_url: String,
    /// Optional Job Declarator Server endpoint for operator visibility.
    #[serde(default)]
    pub job_declarator_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomTempSource {
    #[default]
    Local,
    SwarmAverage,
    External,
}

/// Bump this (and add a migration arm in `nvs_config::migrate_config`) every
/// time the saved config shape changes in a way older firmware can't read.
pub const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardProfileSource {
    BoardVersion,
    DeviceModelDefault,
    AsicModelFallback,
    BuildDefault,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardProfileResolution {
    pub profile: &'static BoardVersionProfile,
    pub source: BoardProfileSource,
    pub identity_recognized: bool,
    pub family_consistent: bool,
    pub mining_allowed_without_lab_bypass: bool,
}

impl DcentAxeConfig {
    /// Returns true if this config has WiFi credentials set.
    pub fn is_configured(&self) -> bool {
        !self.wifi_ssid.is_empty()
    }

    /// Parse the board model string into a BitAxeModel.
    pub fn bitaxe_model(&self) -> BitAxeModel {
        self.exact_model()
    }

    pub fn exact_model(&self) -> BitAxeModel {
        BitAxeModel::from_device_model(&self.board_model)
            .unwrap_or_else(|| self.board_profile().model)
    }

    pub fn board_profile_resolution(&self) -> BoardProfileResolution {
        let (profile, source) =
            if let Some(profile) = BoardVersionProfile::find(&self.board_version) {
                (profile, BoardProfileSource::BoardVersion)
            } else if let Some(model) = BitAxeModel::from_device_model(&self.board_model) {
                (
                    BoardVersionProfile::default_for_model(model),
                    BoardProfileSource::DeviceModelDefault,
                )
            } else if !self.asic_model.trim().is_empty() {
                (
                    BoardVersionProfile::infer("", "", self.asic_model.trim()),
                    BoardProfileSource::AsicModelFallback,
                )
            } else {
                (
                    default_profile_for_build(),
                    BoardProfileSource::BuildDefault,
                )
            };

        let identity_recognized = self.board_identity_recognized();
        let family_consistent = self.board_identity_family_consistent();
        let resolved_model =
            BitAxeModel::from_device_model(&self.board_model).unwrap_or(profile.model);
        let mut board = BoardConfig::for_profile_with_model(profile, resolved_model);
        if !self.asic_model.trim().is_empty() {
            board.asic_model = self.asic_model.trim().to_string();
        }
        if self.asic_count > 0 {
            board.asic_count = self.asic_count;
        }
        if let Some(hw) = &self.hardware {
            board.apply_hardware_config(hw);
        }
        let board_safe = board.validate().is_ok()
            && board
                .validate_accessory_mode(board.accessory_mode())
                .is_ok();
        let custom_board_requires_bypass = self.hardware.is_some()
            && !self.board_version.trim().is_empty()
            && BoardVersionProfile::find(&self.board_version).is_none()
            && board.mining_capable();
        BoardProfileResolution {
            profile,
            source,
            identity_recognized,
            family_consistent,
            mining_allowed_without_lab_bypass: identity_recognized
                && family_consistent
                && board_safe
                && !custom_board_requires_bypass,
        }
    }

    pub fn board_profile(&self) -> &'static BoardVersionProfile {
        self.board_profile_resolution().profile
    }

    pub fn board_version_recognized(&self) -> bool {
        let board_version = self.board_version.trim();
        board_version.is_empty() || BoardVersionProfile::find(board_version).is_some()
    }

    pub fn board_identity_recognized(&self) -> bool {
        let board_version = self.board_version.trim();
        if !board_version.is_empty() {
            return BoardVersionProfile::find(board_version).is_some();
        }

        BitAxeModel::from_device_model(&self.board_model).is_some()
    }

    pub fn board_identity_family_consistent(&self) -> bool {
        let board_version = self.board_version.trim();
        let Some(profile) = BoardVersionProfile::find(board_version) else {
            return true;
        };

        let Some(model) = BitAxeModel::from_device_model(&self.board_model) else {
            return true;
        };

        model == profile.model
            || BoardVersionProfile::default_for_model(model).model == profile.model
    }

    pub fn support_status(&self) -> &'static str {
        if self.board_identity_recognized() {
            self.board_config().model.support_status()
        } else {
            "unknown"
        }
    }

    pub fn asic_model_name(&self) -> &str {
        if self.asic_model.trim().is_empty() {
            self.board_profile().asic_model
        } else {
            self.asic_model.trim()
        }
    }

    pub fn board_config(&self) -> BoardConfig {
        let profile = self.board_profile();
        let mut board = BoardConfig::for_profile_with_model(profile, self.exact_model());

        if !self.board_version.trim().is_empty() {
            board.board_version = self.board_version.trim().to_string();
        }
        if !self.asic_model.trim().is_empty() {
            board.asic_model = self.asic_model.trim().to_string();
        }
        if self.asic_count > 0 {
            board.asic_count = self.asic_count;
        }
        if let Some(hw) = &self.hardware {
            board.apply_hardware_config(hw);
        }

        board
    }

    pub fn validate_safety(&self, unsafe_lab_bypass: bool) -> Result<(), String> {
        let board = self.board_config();
        board.validate().map_err(|e| e.to_string())?;
        board
            .validate_accessory_mode(board.accessory_mode())
            .map_err(|e| e.to_string())?;

        if !self.board_identity_recognized() && !unsafe_lab_bypass {
            return Err(
                "unrecognized board identity requires explicit unsafe lab safety bypass"
                    .to_string(),
            );
        }
        if !self.board_identity_family_consistent() {
            return Err(format!(
                "board_version '{}' is inconsistent with device_model '{}'",
                self.board_version.trim(),
                self.board_model.trim()
            ));
        }

        let custom_board = self.hardware.is_some()
            && !self.board_version.trim().is_empty()
            && BoardVersionProfile::find(&self.board_version).is_none();
        if custom_board && board.mining_capable() && !unsafe_lab_bypass {
            if board.fan_controller == FanControllerKind::None {
                return Err(
                    "custom mining-capable board requires an explicit fan controller".to_string(),
                );
            }
            if board.temp_sensor == TempSensorKind::None {
                return Err(
                    "custom mining-capable board requires an explicit temperature sensor"
                        .to_string(),
                );
            }
            if board.power_controller == PowerControllerKind::None {
                return Err(
                    "custom mining-capable board requires an explicit power controller".to_string(),
                );
            }
        }

        Ok(())
    }

    pub fn canonicalize_identity(&mut self) {
        if !self.board_version.trim().is_empty()
            && BoardVersionProfile::find(&self.board_version).is_none()
        {
            return;
        }

        let exact_model = self.exact_model();
        let profile = self.board_profile();
        self.board_model = exact_model.canonical_key().to_string();
        self.board_version = profile.board_version.to_string();
        self.asic_model = profile.asic_model.to_string();
    }

    pub fn board_target(&self) -> &'static str {
        self.board_config().model.board_target()
    }

    /// Get the ASIC model for driver creation.
    pub fn asic_model(&self) -> dcentaxe_asic::AsicModel {
        match self.asic_model_name() {
            "BM1397" => dcentaxe_asic::AsicModel::BM1397,
            "BM1368" => dcentaxe_asic::AsicModel::BM1368,
            "BM1370" => dcentaxe_asic::AsicModel::BM1370,
            _ => dcentaxe_asic::AsicModel::BM1366,
        }
    }

    /// Get expected ASIC count for this board model.
    pub fn expected_asic_count(&self) -> u8 {
        self.board_config().asic_count
    }

    /// Get safe power limits based on overclock mode.
    pub fn power_limits(&self) -> PowerLimits {
        let model = self.board_config().model;
        if self.overclock_enabled {
            PowerLimits::overclock(model)
        } else {
            PowerLimits::safe(model)
        }
    }

    pub fn qualify_operating_point(
        &self,
        frequency_mhz: f32,
        voltage_mv: u16,
        _surface: ControlSurface,
    ) -> QualifiedOperatingPoint {
        let board = self.board_config();
        let stock = stock_asic_settings(board.model);
        let limits = self.power_limits();
        let mut min_frequency = stock.frequency_options.iter().copied().min().unwrap_or(50) as f32;
        let mut max_frequency = limits.max_frequency.min(
            stock
                .frequency_options
                .iter()
                .copied()
                .max()
                .unwrap_or(limits.max_frequency.round() as u16) as f32,
        );
        let min_voltage_mv = board.min_voltage_mv.max(
            stock
                .voltage_options
                .iter()
                .copied()
                .min()
                .unwrap_or(board.min_voltage_mv),
        );
        let mut max_voltage_mv = limits.max_voltage_mv.min(
            stock
                .voltage_options
                .iter()
                .copied()
                .max()
                .unwrap_or(limits.max_voltage_mv),
        );

        // Gamma Turbo is qualified in live testing at 625 MHz / 1150 mV in
        // safe mode. Keep higher voltages behind explicit overclock mode.
        if board.model == BitAxeModel::GammaTurbo && !self.overclock_enabled {
            max_voltage_mv = max_voltage_mv.min(board.default_voltage_mv);
        }

        if max_frequency < min_frequency {
            min_frequency = max_frequency;
        }

        let qualified_frequency = frequency_mhz.clamp(min_frequency, max_frequency);
        let qualified_voltage = voltage_mv.clamp(min_voltage_mv, max_voltage_mv);
        QualifiedOperatingPoint {
            frequency_mhz: qualified_frequency,
            voltage_mv: qualified_voltage,
            clamped: (qualified_frequency - frequency_mhz).abs() > f32::EPSILON
                || qualified_voltage != voltage_mv,
        }
    }

    pub fn qualified_frequency_options(&self) -> Vec<u16> {
        let board = self.board_config();
        stock_asic_settings(board.model)
            .frequency_options
            .iter()
            .copied()
            .filter(|frequency| {
                let point = self.qualify_operating_point(
                    *frequency as f32,
                    board.default_voltage_mv,
                    ControlSurface::RestPatch,
                );
                point.frequency_mhz.round() as u16 == *frequency
            })
            .collect()
    }

    pub fn qualified_voltage_options(&self) -> Vec<u16> {
        let board = self.board_config();
        let base_frequency = self
            .qualify_operating_point(
                self.target_frequency,
                self.target_voltage_mv,
                ControlSurface::RestPatch,
            )
            .frequency_mhz;
        stock_asic_settings(board.model)
            .voltage_options
            .iter()
            .copied()
            .filter(|voltage_mv| {
                let point = self.qualify_operating_point(
                    base_frequency,
                    *voltage_mv,
                    ControlSurface::RestPatch,
                );
                point.voltage_mv == *voltage_mv
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ControlSurface {
    Provisioning,
    RestPatch,
    LegacyRest,
    Mcp,
    Autotuner,
    Schedule,
    BootRestore,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct QualifiedOperatingPoint {
    pub frequency_mhz: f32,
    pub voltage_mv: u16,
    pub clamped: bool,
}

/// Default config used by the provisioning portal form.
impl Default for DcentAxeConfig {
    fn default() -> Self {
        let profile = default_profile_for_build();
        let board = BoardConfig::for_profile_with_model(profile, default_model_for_build());
        Self {
            wifi_ssid: String::new(),
            wifi_password: String::new(),
            stratum: StratumConfig::default(),
            board_model: default_model_for_build().canonical_key().into(),
            board_version: profile.board_version.into(),
            asic_model: profile.asic_model.into(),
            hostname: String::new(),
            target_frequency: board.default_frequency,
            target_voltage_mv: board.default_voltage_mv,
            fan_speed_pct: 100,
            asic_count: board.asic_count,
            overclock_enabled: false,
            display_inverted: false,
            fan_target_temp_c: DEFAULT_FAN_TARGET_TEMP_C, // 0 = manual mode (CFG-7)
            fallback_pool: None,
            split_pool: None,
            sv2_own_templates: Sv2OwnTemplateConfig::default(),
            sv2_authority_pubkey: None,
            mqtt: MqttConfig::default(),
            schedule_enabled: true,
            schedule_timezone_offset_minutes: 0,
            power_schedule: Vec::new(),
            hardware: None,
            metrics_require_auth: true,
            allow_unsigned_ota: false,
            room_temp_source: RoomTempSource::Local,
            schema_version: SCHEMA_VERSION,
        }
    }
}

/// Power limits based on PSU rating.
///
/// Safe mode: assumes 5V 6A PSU (30W max, ~25W usable after losses)
/// Overclock mode: assumes 5V 10A PSU (50W max, ~45W usable)
#[derive(Debug, Clone)]
pub struct PowerLimits {
    /// Maximum power draw in watts
    pub max_power_w: f32,
    /// Maximum current in amps
    pub max_current_a: f32,
    /// Maximum safe frequency for this power envelope (MHz)
    pub max_frequency: f32,
    /// Recommended default frequency (MHz)
    pub default_frequency: f32,
    /// Maximum safe voltage (mV)
    pub max_voltage_mv: u16,
}

impl PowerLimits {
    /// Safe limits for 5V 6A PSU (standard USB-C, most common)
    pub fn safe(model: BitAxeModel) -> Self {
        match model {
            BitAxeModel::GammaDuo => Self {
                max_power_w: 40.0,
                max_current_a: 8.0,
                max_frequency: 410.0,
                default_frequency: 400.0,
                max_voltage_mv: 1250,
            },
            BitAxeModel::GammaTurbo => Self {
                max_power_w: 60.0,
                max_current_a: 5.0,
                max_frequency: 625.0,
                default_frequency: 525.0,
                max_voltage_mv: 1350,
            },
            BitAxeModel::Gamma => Self {
                max_power_w: 25.0,
                max_current_a: 5.5,
                max_frequency: 475.0,
                default_frequency: 525.0,
                max_voltage_mv: 1200,
            },
            BitAxeModel::Supra => Self {
                max_power_w: 25.0,
                max_current_a: 5.5,
                max_frequency: 450.0,
                default_frequency: 490.0,
                max_voltage_mv: 1250,
            },
            BitAxeModel::Ultra => Self {
                max_power_w: 25.0,
                max_current_a: 5.5,
                max_frequency: 450.0,
                default_frequency: 400.0,
                max_voltage_mv: 1250,
            },
            BitAxeModel::Max => Self {
                max_power_w: 25.0,
                max_current_a: 5.5,
                max_frequency: 400.0,
                default_frequency: 425.0,
                max_voltage_mv: 1400,
            },
            BitAxeModel::HexUltra => Self {
                // ESP-Miner: FAMILY_HEX.max_power = 90W, 12V input
                max_power_w: 90.0,
                max_current_a: 8.0,
                max_frequency: 500.0,
                default_frequency: 485.0,
                max_voltage_mv: 1300,
            },
            BitAxeModel::HexSupra => Self {
                // ESP-Miner: FAMILY_SUPRA_HEX.max_power = 120W, 12V input
                max_power_w: 120.0,
                max_current_a: 10.0,
                max_frequency: 525.0,
                default_frequency: 490.0,
                max_voltage_mv: 1300,
            },
            BitAxeModel::NerdNOS => Self {
                max_power_w: 8.0,
                max_current_a: 1.6,
                max_frequency: 400.0,
                default_frequency: 400.0,
                max_voltage_mv: 1200,
            },
            BitAxeModel::NerdAxe => Self {
                max_power_w: 25.0,
                max_current_a: 5.5,
                max_frequency: 475.0,
                default_frequency: 400.0,
                max_voltage_mv: 1200,
            },
            BitAxeModel::NerdQaxePlus => Self {
                max_power_w: 55.0,
                max_current_a: 12.0,
                max_frequency: 450.0,
                default_frequency: 400.0,
                max_voltage_mv: 1250,
            },
            BitAxeModel::NerdQaxePP => Self {
                max_power_w: 80.0,
                max_current_a: 16.0,
                max_frequency: 475.0,
                default_frequency: 400.0,
                max_voltage_mv: 1200,
            },
            // Touch variants share limits with their mining-board base.
            BitAxeModel::Touch => Self::safe(BitAxeModel::Gamma),
            BitAxeModel::GtTouch => Self::safe(BitAxeModel::GammaTurbo),
            // ── DCENT_axe BM1397 family ──
            // Single shares the BitAxe Max BM1397 envelope.
            BitAxeModel::DcentAxeBm1397 => Self::safe(BitAxeModel::Max),
            // Quad/Hex: same BM1397 chip envelope, scaled wall power for the chain.
            BitAxeModel::DcentAxeQuadBm1397 => Self {
                max_power_w: 90.0,
                max_current_a: 8.0,
                max_frequency: 400.0,
                default_frequency: 425.0,
                max_voltage_mv: 1400,
            },
            BitAxeModel::DcentAxeHexBm1397 => Self {
                max_power_w: 130.0,
                max_current_a: 11.0,
                max_frequency: 400.0,
                default_frequency: 425.0,
                max_voltage_mv: 1400,
            },
        }
    }

    /// Overclock limits for 5V 10A PSU (high-power USB-C or barrel jack)
    pub fn overclock(model: BitAxeModel) -> Self {
        match model {
            BitAxeModel::GammaDuo => Self {
                max_power_w: 50.0,
                max_current_a: 10.0,
                max_frequency: 490.0,
                default_frequency: 410.0,
                max_voltage_mv: 1300,
            },
            BitAxeModel::GammaTurbo => Self {
                max_power_w: 80.0,
                max_current_a: 6.7,
                max_frequency: 650.0,
                default_frequency: 550.0,
                max_voltage_mv: 1350,
            },
            BitAxeModel::Gamma => Self {
                max_power_w: 45.0,
                max_current_a: 9.5,
                max_frequency: 600.0,
                default_frequency: 525.0,
                max_voltage_mv: 1300,
            },
            BitAxeModel::Supra => Self {
                max_power_w: 45.0,
                max_current_a: 9.5,
                max_frequency: 575.0,
                default_frequency: 490.0,
                max_voltage_mv: 1350,
            },
            BitAxeModel::Ultra => Self {
                max_power_w: 45.0,
                max_current_a: 9.5,
                max_frequency: 550.0,
                default_frequency: 485.0,
                max_voltage_mv: 1350,
            },
            BitAxeModel::Max => Self {
                max_power_w: 45.0,
                max_current_a: 9.5,
                max_frequency: 500.0,
                default_frequency: 400.0,
                max_voltage_mv: 1600,
            },
            BitAxeModel::HexUltra => Self {
                max_power_w: 110.0,
                max_current_a: 10.0,
                max_frequency: 550.0,
                default_frequency: 500.0,
                max_voltage_mv: 1350,
            },
            BitAxeModel::HexSupra => Self {
                max_power_w: 150.0,
                max_current_a: 13.0,
                max_frequency: 575.0,
                default_frequency: 525.0,
                max_voltage_mv: 1350,
            },
            // Nerd boards: overclock = same as safe (USB-powered, limited headroom)
            BitAxeModel::NerdNOS => Self::safe(model),
            BitAxeModel::NerdAxe | BitAxeModel::NerdQaxePlus | BitAxeModel::NerdQaxePP => Self {
                max_power_w: Self::safe(model).max_power_w * 1.5,
                max_current_a: Self::safe(model).max_current_a * 1.5,
                max_frequency: Self::safe(model).max_frequency + 100.0,
                default_frequency: Self::safe(model).default_frequency + 50.0,
                max_voltage_mv: Self::safe(model).max_voltage_mv + 100,
            },
            // Touch variants reuse the overclock profile of their mining-board base.
            BitAxeModel::Touch => Self::overclock(BitAxeModel::Gamma),
            BitAxeModel::GtTouch => Self::overclock(BitAxeModel::GammaTurbo),
            // ── DCENT_axe BM1397 family ──
            // Single shares the BitAxe Max BM1397 overclock envelope; Quad/Hex
            // scale their own safe envelope the same way the Nerd chains do.
            BitAxeModel::DcentAxeBm1397 => Self::overclock(BitAxeModel::Max),
            BitAxeModel::DcentAxeQuadBm1397 | BitAxeModel::DcentAxeHexBm1397 => Self {
                max_power_w: Self::safe(model).max_power_w * 1.5,
                max_current_a: Self::safe(model).max_current_a * 1.5,
                max_frequency: Self::safe(model).max_frequency + 100.0,
                default_frequency: Self::safe(model).default_frequency,
                max_voltage_mv: Self::safe(model).max_voltage_mv + 100,
            },
        }
    }

    /// Warning message for enabling overclock mode
    pub const OVERCLOCK_WARNING: &'static str = concat!(
        "WARNING: Overclocking mode increases power draw significantly. ",
        "Ensure your PSU is rated for at least 5V 10A (50W). ",
        "Using an inadequate PSU may cause: voltage drops, USB disconnects, ",
        "ASIC damage, fire risk, or permanent device failure. ",
        "D-Central Technologies is not responsible for damage caused by overclocking. ",
        "Proceed at your own risk."
    );
}

/// Configuration for a secondary pool with hashrate splitting.
///
/// When configured, the firmware maintains two simultaneous Stratum connections
/// and alternates work dispatch between them using a deficit-based scheduler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitPoolConfig {
    /// Pool connection details for the secondary pool.
    pub pool: dcentaxe_stratum::StratumConfig,

    /// Percentage of hashrate directed to this secondary pool (1-99).
    /// The primary pool receives (100 - hashrate_pct)% of hashrate.
    pub hashrate_pct: u8,
}

/// A scheduled power profile that activates at a specific hour of the day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PowerSchedule {
    /// Whether this schedule entry is active.
    #[serde(default = "default_schedule_entry_enabled")]
    pub enabled: bool,
    /// Hour to activate this profile (0-23, local time)
    pub hour: u8,
    /// Minute within the hour to activate this profile (0-59).
    #[serde(default)]
    pub minute: u8,
    /// Target frequency (MHz)
    pub frequency: f32,
    /// Target voltage (mV)
    #[serde(alias = "voltageMv")]
    pub voltage_mv: u16,
    /// Optional autotuner override for this slot.
    /// None = leave current autotuner state alone, Some(false) = fixed freq/volt,
    /// Some(true) = enable autotuner using the optional mode/target below.
    #[serde(default, alias = "autotuneEnabled")]
    pub autotune_enabled: Option<bool>,
    /// Autotuner mode when `autotune_enabled = true`.
    /// API string values: max_hashrate, best_efficiency, target_watts, target_temp.
    #[serde(default, alias = "autotuneMode")]
    pub autotune_mode: Option<String>,
    /// Autotuner target value for target_watts / target_temp style policies.
    #[serde(default, alias = "autotuneTarget")]
    pub autotune_target: Option<f32>,
    /// Human label
    #[serde(default)]
    pub label: String,
}

impl PowerSchedule {
    pub fn start_minute_of_day(&self) -> u16 {
        self.hour.min(23) as u16 * 60 + self.minute.min(59) as u16
    }
}

/// Return the current local schedule minute and the source used to derive it.
/// SNTP-backed wall clock is preferred; uptime fallback preserves useful behavior
/// on isolated setups without NTP/DNS.
pub fn schedule_minute_of_day(
    timezone_offset_minutes: i16,
    uptime_secs: u64,
) -> (u16, &'static str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if now >= 1_600_000_000 {
        let local_secs = now as i64 + timezone_offset_minutes as i64 * 60;
        let day_secs = local_secs.rem_euclid(86_400) as u64;
        return ((day_secs / 60) as u16, "wall_clock");
    }

    (((uptime_secs % 86_400) / 60) as u16, "uptime_fallback")
}

/// Pick the latest enabled schedule entry whose start time is <= current local time.
/// If all entries start later than now, wrap to the latest entry from yesterday.
pub fn active_power_schedule<'a>(
    entries: &'a [PowerSchedule],
    minute_of_day: u16,
) -> Option<(usize, &'a PowerSchedule)> {
    let mut best_before: Option<(usize, u16, &PowerSchedule)> = None;
    let mut best_wrap: Option<(usize, u16, &PowerSchedule)> = None;

    for (idx, entry) in entries.iter().enumerate() {
        if !entry.enabled {
            continue;
        }
        let start = entry.start_minute_of_day();
        if start <= minute_of_day {
            if best_before
                .map(|(_, prev, _)| start >= prev)
                .unwrap_or(true)
            {
                best_before = Some((idx, start, entry));
            }
        }
        if best_wrap.map(|(_, prev, _)| start >= prev).unwrap_or(true) {
            best_wrap = Some((idx, start, entry));
        }
    }

    best_before
        .or(best_wrap)
        .map(|(idx, _, entry)| (idx, entry))
}

pub fn next_schedule_change_minutes(entries: &[PowerSchedule], minute_of_day: u16) -> Option<u16> {
    let mut next_today: Option<u16> = None;
    let mut first_tomorrow: Option<u16> = None;

    for entry in entries.iter().filter(|entry| entry.enabled) {
        let start = entry.start_minute_of_day();
        if start > minute_of_day {
            if next_today.map(|prev| start < prev).unwrap_or(true) {
                next_today = Some(start);
            }
        }
        if first_tomorrow.map(|prev| start < prev).unwrap_or(true) {
            first_tomorrow = Some(start);
        }
    }

    next_today
        .map(|start| start - minute_of_day)
        .or_else(|| first_tomorrow.map(|start| 1_440 - minute_of_day + start))
}

/// A fixed frequency/voltage preset for a specific BitAxe model.
#[derive(Debug, Clone)]
pub struct MiningPreset {
    /// Human-readable name
    pub name: &'static str,
    /// Target frequency (MHz)
    pub frequency: f32,
    /// Target core voltage (mV)
    pub voltage_mv: u16,
    /// Expected hashrate (GH/s, approximate)
    pub expected_hashrate_ghs: f32,
    /// Expected power draw (watts, approximate)
    pub expected_power_w: f32,
    /// Requires overclock mode?
    pub requires_overclock: bool,
}

/// Get available mining presets for a board model.
/// Returns 3-5 presets from Low Power → Max Performance.
pub fn mining_presets(model: BitAxeModel) -> Vec<MiningPreset> {
    match model {
        BitAxeModel::GammaDuo => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 350.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 700.0,
                expected_power_w: 8.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 400.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 850.0,
                expected_power_w: 12.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "High Perf",
                frequency: 410.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 900.0,
                expected_power_w: 15.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 490.0,
                voltage_mv: 1250,
                expected_hashrate_ghs: 1000.0,
                expected_power_w: 20.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::Gamma => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 400.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 800.0,
                expected_power_w: 10.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 525.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 1000.0,
                expected_power_w: 15.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Efficient",
                frequency: 490.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 1100.0,
                expected_power_w: 13.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "High Perf",
                frequency: 575.0,
                voltage_mv: 1260,
                expected_hashrate_ghs: 1200.0,
                expected_power_w: 20.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 600.0,
                voltage_mv: 1300,
                expected_hashrate_ghs: 1250.0,
                expected_power_w: 24.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::GammaTurbo => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 400.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 1700.0,
                expected_power_w: 28.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Balanced",
                frequency: 525.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 2200.0,
                expected_power_w: 36.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Recommended Safe",
                frequency: 625.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 2600.0,
                expected_power_w: 42.5,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 650.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 2800.0,
                expected_power_w: 48.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::Supra => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 400.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 500.0,
                expected_power_w: 10.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 490.0,
                voltage_mv: 1166,
                expected_hashrate_ghs: 575.0,
                expected_power_w: 15.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Efficient",
                frequency: 485.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 625.0,
                expected_power_w: 13.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "High Perf",
                frequency: 550.0,
                voltage_mv: 1300,
                expected_hashrate_ghs: 700.0,
                expected_power_w: 20.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 575.0,
                voltage_mv: 1350,
                expected_hashrate_ghs: 775.0,
                expected_power_w: 24.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::Ultra => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 400.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 400.0,
                expected_power_w: 10.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 450.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 450.0,
                expected_power_w: 12.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Efficient",
                frequency: 485.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 500.0,
                expected_power_w: 14.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "High Perf",
                frequency: 525.0,
                voltage_mv: 1300,
                expected_hashrate_ghs: 550.0,
                expected_power_w: 18.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 550.0,
                voltage_mv: 1350,
                expected_hashrate_ghs: 575.0,
                expected_power_w: 22.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::Max => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 300.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 300.0,
                expected_power_w: 8.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 425.0,
                voltage_mv: 1400,
                expected_hashrate_ghs: 425.0,
                expected_power_w: 14.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "High Perf",
                frequency: 450.0,
                voltage_mv: 1400,
                expected_hashrate_ghs: 450.0,
                expected_power_w: 18.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 500.0,
                voltage_mv: 1600,
                expected_hashrate_ghs: 500.0,
                expected_power_w: 25.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::HexUltra => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 350.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 2100.0,
                expected_power_w: 30.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 485.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 3000.0,
                expected_power_w: 48.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 485.0,
                voltage_mv: 1350,
                expected_hashrate_ghs: 3000.0,
                expected_power_w: 48.0,
                requires_overclock: true,
            },
        ],
        BitAxeModel::HexSupra => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 350.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 2400.0,
                expected_power_w: 30.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 490.0,
                voltage_mv: 1166,
                expected_hashrate_ghs: 3600.0,
                expected_power_w: 48.0,
                requires_overclock: true,
            },
            MiningPreset {
                name: "Max (OC)",
                frequency: 490.0,
                voltage_mv: 1350,
                expected_hashrate_ghs: 3600.0,
                expected_power_w: 48.0,
                requires_overclock: true,
            },
        ],
        // NerdNOS: BM1397 underclocked, USB-powered ~8W
        BitAxeModel::NerdNOS => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 300.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 80.0,
                expected_power_w: 5.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 400.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 110.0,
                expected_power_w: 8.0,
                requires_overclock: false,
            },
        ],
        // NerdAxe: BM1370, same as Gamma but Nerd hardware
        BitAxeModel::NerdAxe => mining_presets(BitAxeModel::Gamma),
        // NerdQaxe+: 4x BM1368
        BitAxeModel::NerdQaxePlus => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 400.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 2000.0,
                expected_power_w: 35.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 450.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 2400.0,
                expected_power_w: 50.0,
                requires_overclock: false,
            },
        ],
        // NerdQaxe++: 4x BM1370
        BitAxeModel::NerdQaxePP => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 400.0,
                voltage_mv: 1150,
                expected_hashrate_ghs: 3200.0,
                expected_power_w: 50.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 525.0,
                voltage_mv: 1200,
                expected_hashrate_ghs: 4800.0,
                expected_power_w: 75.0,
                requires_overclock: false,
            },
        ],
        // Touch variants reuse the presets of their mining-board base.
        BitAxeModel::Touch => mining_presets(BitAxeModel::Gamma),
        BitAxeModel::GtTouch => mining_presets(BitAxeModel::GammaTurbo),
        // ── DCENT_axe BM1397 family ──
        // Single reuses the BitAxe Max BM1397 presets.
        BitAxeModel::DcentAxeBm1397 => mining_presets(BitAxeModel::Max),
        // Quad 4x BM1397: per-chip Max figures scaled to the 4-chip chain.
        BitAxeModel::DcentAxeQuadBm1397 => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 300.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 1200.0,
                expected_power_w: 32.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 425.0,
                voltage_mv: 1400,
                expected_hashrate_ghs: 1700.0,
                expected_power_w: 56.0,
                requires_overclock: false,
            },
        ],
        // Hex 6x BM1397: per-chip Max figures scaled to the 6-chip chain.
        BitAxeModel::DcentAxeHexBm1397 => vec![
            MiningPreset {
                name: "Low Power",
                frequency: 300.0,
                voltage_mv: 1100,
                expected_hashrate_ghs: 1800.0,
                expected_power_w: 48.0,
                requires_overclock: false,
            },
            MiningPreset {
                name: "Default",
                frequency: 425.0,
                voltage_mv: 1400,
                expected_hashrate_ghs: 2550.0,
                expected_power_w: 84.0,
                requires_overclock: false,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a custom (non-table) hardware override with the three safety
    /// controllers chosen explicitly. `BoardHardwareConfig` has no `Default`,
    /// so every field is spelled out; only the controller kinds are
    /// load-bearing for these tests.
    fn custom_hw(
        fan: FanControllerKind,
        temp: TempSensorKind,
        power: PowerControllerKind,
    ) -> BoardHardwareConfig {
        BoardHardwareConfig {
            plug_sense: false,
            asic_enable: true,
            fan_controller: fan,
            temp_sensor: temp,
            power_controller: power,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 19,
        }
    }

    // ── XPH-1 — validate_safety() master mining-permission gate ──
    //
    // A custom mining-capable board (board_version not in the profile table +
    // an explicit hardware override) must be REFUSED when any one of the three
    // required safety controllers (fan / temperature / power) is absent, and
    // PERMITTED only under the explicit unsafe lab bypass. Each case keeps a
    // trusted thermal source present so `BoardConfig::validate()` itself passes
    // and the custom-board gate is the deciding factor.
    #[test]
    fn validate_safety_fails_closed_when_required_controller_absent() {
        // Baseline default config (host build => Gamma, a KNOWN profile) passes.
        let base = DcentAxeConfig::default();
        assert!(
            BoardVersionProfile::find(&base.board_version).is_some(),
            "default board_version should resolve to a known profile"
        );
        assert!(base.validate_safety(false).is_ok());

        // (missing fan, trusted temp + power), (missing temp, trusted fan +
        // power == Tps546), (missing power, trusted fan + temp).
        let cases = [
            custom_hw(
                FanControllerKind::None,
                TempSensorKind::Emc2101,
                PowerControllerKind::Tps546,
            ),
            custom_hw(
                FanControllerKind::Emc2101,
                TempSensorKind::None,
                PowerControllerKind::Tps546,
            ),
            custom_hw(
                FanControllerKind::Emc2101,
                TempSensorKind::Emc2101,
                PowerControllerKind::None,
            ),
        ];

        for hw in cases {
            let mut cfg = DcentAxeConfig::default();
            cfg.board_version = "DCENT-NOPROFILE-TEST".to_string();
            assert!(
                BoardVersionProfile::find(&cfg.board_version).is_none(),
                "test board_version must be absent from the profile table"
            );
            cfg.hardware = Some(hw);

            // Sanity: this really is a custom, mining-capable board.
            let board = cfg.board_config();
            assert!(board.mining_capable());

            // Fail-closed: the master gate refuses without the bypass.
            assert!(
                cfg.validate_safety(false).is_err(),
                "master gate must refuse a custom mining board missing a required controller"
            );
            // Explicit lab bypass permits the bench exception.
            assert!(
                cfg.validate_safety(true).is_ok(),
                "explicit unsafe lab bypass must permit the bench exception"
            );
        }
    }

    // ── XPH-2 — qualify_operating_point() voltage/freq clamp + PowerLimits ──
    #[test]
    fn board_profile_resolution_records_provenance_and_mining_gate() {
        let cfg = DcentAxeConfig::default();
        let resolved = cfg.board_profile_resolution();
        assert_eq!(resolved.source, BoardProfileSource::BoardVersion);
        assert_eq!(resolved.profile.board_version, cfg.board_version);
        assert!(resolved.identity_recognized);
        assert!(resolved.family_consistent);
        assert!(resolved.mining_allowed_without_lab_bypass);

        let mut unknown_version = DcentAxeConfig::default();
        unknown_version.board_version = "DCENT-UNKNOWN-BOARD".to_string();
        let resolved = unknown_version.board_profile_resolution();
        assert_eq!(resolved.source, BoardProfileSource::DeviceModelDefault);
        assert_eq!(resolved.profile.model, default_model_for_build());
        assert!(!resolved.identity_recognized);
        assert!(resolved.family_consistent);
        assert!(
            !resolved.mining_allowed_without_lab_bypass,
            "unknown board_version fallback must be provenance-visible and non-mining"
        );

        let mut asic_only = DcentAxeConfig::default();
        asic_only.board_version.clear();
        asic_only.board_model = "garbage-model".to_string();
        asic_only.asic_model = "BM1370".to_string();
        let resolved = asic_only.board_profile_resolution();
        assert_eq!(resolved.source, BoardProfileSource::AsicModelFallback);
        assert_eq!(resolved.profile.model, BitAxeModel::Gamma);
        assert!(!resolved.identity_recognized);
        assert!(resolved.family_consistent);
        assert!(
            !resolved.mining_allowed_without_lab_bypass,
            "ASIC-only inference must not become an automatic mining path"
        );
    }

    #[test]
    fn unknown_board_version_is_mining_gated_without_lab_bypass() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_version = "DCENT-UNKNOWN-BOARD".to_string();
        cfg.hardware = None;

        assert!(!cfg.board_version_recognized());
        assert_eq!(cfg.support_status(), "unknown");
        assert!(
            cfg.validate_safety(false).is_err(),
            "unknown board_version alone must not silently mine on a default profile"
        );
        assert!(
            cfg.validate_safety(true).is_ok(),
            "the explicit lab bypass remains available for bench-only unknown boards"
        );
    }

    #[test]
    fn recognized_model_without_board_version_stays_supported() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_version.clear();
        cfg.board_model = "gamma".to_string();

        assert!(cfg.board_version_recognized());
        assert!(cfg.board_identity_recognized());
        assert_eq!(cfg.board_profile().model, BitAxeModel::Gamma);
        assert_eq!(cfg.support_status(), "supported");
        assert!(
            cfg.validate_safety(false).is_ok(),
            "older configs with a recognized device_model must remain eligible"
        );
    }

    #[test]
    fn inferred_asic_profile_without_identity_is_unknown_and_mining_gated() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_version.clear();
        cfg.board_model = "garbage-model".to_string();
        cfg.asic_model = "BM1370".to_string();
        cfg.hardware = None;

        assert!(cfg.board_version_recognized());
        assert!(!cfg.board_identity_recognized());
        assert_eq!(cfg.board_profile().model, BitAxeModel::Gamma);
        assert!(cfg.board_config().mining_capable());
        assert_eq!(cfg.support_status(), "unknown");
        assert!(
            cfg.validate_safety(false).is_err(),
            "ASIC-only inference must not become a mining path"
        );
        assert!(
            cfg.validate_safety(true).is_ok(),
            "the explicit lab bypass remains available for bench-only inference"
        );
    }

    #[test]
    fn board_version_and_device_model_family_must_agree() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_version = "801".to_string(); // Gamma Turbo / GT
        cfg.board_model = "gamma".to_string();

        assert!(cfg.board_version_recognized());
        assert!(!cfg.board_identity_family_consistent());
        assert!(
            cfg.validate_safety(false).is_err(),
            "cross-family board_version/device_model disagreement must not mine"
        );
        assert!(
            cfg.validate_safety(true).is_err(),
            "unsafe lab bypass is for unknown/custom boards, not contradictory identities"
        );
    }

    #[test]
    fn accessory_models_can_reuse_their_underlying_mining_profile() {
        let mut touch = DcentAxeConfig::default();
        touch.board_version = "601".to_string(); // Gamma mining board profile
        touch.board_model = "touch".to_string();
        assert!(touch.board_identity_family_consistent());
        assert!(touch.validate_safety(false).is_ok());

        let mut nerd = DcentAxeConfig::default();
        nerd.board_version = "601".to_string(); // Gamma-family profile
        nerd.board_model = "nerdaxe".to_string();
        assert!(nerd.board_identity_family_consistent());
        assert!(nerd.validate_safety(false).is_ok());
    }

    #[test]
    fn support_status_is_recognized_and_conservative() {
        let mut gamma = DcentAxeConfig::default();
        gamma.board_version = "601".to_string();
        gamma.board_model = "gamma".to_string();
        assert!(gamma.board_version_recognized());
        assert_eq!(gamma.support_status(), "supported");

        let mut gt = DcentAxeConfig::default();
        gt.board_version = "801".to_string();
        gt.board_model = "gammaturbo".to_string();
        assert!(gt.board_version_recognized());
        assert_eq!(gt.support_status(), "experimental");

        let mut dcent_bm1397 = DcentAxeConfig::default();
        dcent_bm1397.board_version = "900".to_string();
        dcent_bm1397.board_model = "dcentaxe_bm1397".to_string();
        assert!(dcent_bm1397.board_version_recognized());
        assert_eq!(
            dcent_bm1397.support_status(),
            "experimental",
            "DCENT_axe 900 stays experimental until retained live proof exists"
        );
    }

    #[test]
    fn qualify_operating_point_clamps_and_powerlimits_constants_hold() {
        let cfg = DcentAxeConfig::default(); // Gamma, safe mode
        let limits = cfg.power_limits();

        // A wildly out-of-range request clamps DOWN to the per-model safe
        // envelope and reports `clamped`.
        let over = cfg.qualify_operating_point(99_999.0, u16::MAX, ControlSurface::Autotuner);
        assert!(over.clamped);
        assert!(over.frequency_mhz <= limits.max_frequency);
        assert!(over.voltage_mv <= limits.max_voltage_mv);

        // An explicitly in-range request is returned unchanged (NOT clamped).
        // Gamma safe max_frequency is 475 MHz, so 450/1100 is inside the window;
        // note the default target of 525 MHz would itself clamp.
        let inb = cfg.qualify_operating_point(450.0, 1100, ControlSurface::Autotuner);
        assert!(!inb.clamped);
        assert_eq!(inb.frequency_mhz, 450.0);
        assert_eq!(inb.voltage_mv, 1100);

        // Pin the verbatim magic safety constants — any future edit to the
        // tables breaks CI rather than silently shipping a different envelope.
        let safe_gamma = PowerLimits::safe(BitAxeModel::Gamma);
        let oc_gamma = PowerLimits::overclock(BitAxeModel::Gamma);
        assert_eq!(safe_gamma.max_voltage_mv, 1200);
        assert_eq!(safe_gamma.max_frequency, 475.0);
        assert_eq!(oc_gamma.max_voltage_mv, 1300);
        assert_eq!(oc_gamma.max_frequency, 600.0);
        // Invariant: the overclock envelope is never below the safe envelope.
        assert!(oc_gamma.max_frequency >= safe_gamma.max_frequency);
        assert!(oc_gamma.max_voltage_mv >= safe_gamma.max_voltage_mv);
    }

    // ── XPH-2 (GammaTurbo special case, config.rs:405-407) ──
    //
    // Gamma Turbo without explicit overclock must cap voltage at the board
    // default (1150 mV), even though the raw safe envelope would otherwise
    // allow more.
    #[test]
    fn qualify_operating_point_gammaturbo_caps_voltage_without_overclock() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_model = "gammaturbo".to_string();
        cfg.board_version = "801".to_string();
        cfg.overclock_enabled = false;

        let board = cfg.board_config();
        assert_eq!(board.model, BitAxeModel::GammaTurbo);
        let default_v = board.default_voltage_mv;
        assert_eq!(default_v, 1150);

        // A high voltage request is capped at default_voltage_mv in safe mode.
        let qp = cfg.qualify_operating_point(550.0, 1300, ControlSurface::Autotuner);
        assert!(qp.clamped);
        assert_eq!(qp.voltage_mv, default_v);
    }

    // Source text of the two NON-re-included lane files. `include_str!` resolves
    // relative to THIS file's physical location (dcentaxe/src/config.rs) in BOTH
    // the binary-crate build and the dcentaxe-core `#[path]`-reincluded build, so
    // these structural guards run in CI. They pin call-site wiring that lives in
    // files the host crate cannot compile (esp-idf I/O).
    const PROVISIONING_RS: &str = include_str!("provisioning.rs");
    const NVS_CONFIG_RS: &str = include_str!("nvs_config.rs");

    // The config-size cap lives in nvs_config.rs (which is NOT re-included into
    // the host crate), so the host tests pin the magic value locally and assert
    // nvs_config.rs still declares it — keeping the two from drifting.
    const MAX_CONFIG_SIZE_PIN: usize = 3584;

    #[test]
    fn cfg2_max_config_size_constant_is_pinned() {
        assert!(
            NVS_CONFIG_RS.contains("pub const MAX_CONFIG_SIZE: usize = 3584;"),
            "nvs_config.rs must declare pub const MAX_CONFIG_SIZE: usize = 3584 (CFG-2 cap)"
        );
    }

    // ── CFG-2 — bounded body-accumulation decision helpers ──
    #[test]
    fn cfg2_body_read_capacity_and_take_clamp() {
        let max = MAX_CONFIG_SIZE_PIN;
        // Room remains while received + incoming stays at or below the cap.
        assert!(body_read_capacity_ok(0, max, max));
        assert!(body_read_capacity_ok(max - 1, 1, max));
        // One byte past the cap is rejected.
        assert!(!body_read_capacity_ok(max, 1, max));
        assert!(!body_read_capacity_ok(max - 1, 2, max));
        // Saturating: a huge incoming never wraps to "fits".
        assert!(!body_read_capacity_ok(1, usize::MAX, max));

        // next_take clamps the per-read request to remaining capacity, 0 at cap.
        assert_eq!(next_take(0, max), max);
        assert_eq!(next_take(max - 10, max), 10);
        assert_eq!(next_take(max, max), 0);
        assert_eq!(next_take(max + 5, max), 0); // saturating, never underflows
    }

    /// Simulate the accumulation loop over a sequence of segments using ONLY the
    /// pure decision helpers (the real loop's req.read is esp-idf, review-only).
    /// Returns None when the total would exceed `max` (the reject path).
    fn accumulate(segments: &[&[u8]], max: usize) -> Option<Vec<u8>> {
        let mut body: Vec<u8> = Vec::new();
        for seg in segments {
            if next_take(body.len(), max) == 0 && !seg.is_empty() {
                return None;
            }
            if !body_read_capacity_ok(body.len(), seg.len(), max) {
                return None;
            }
            body.extend_from_slice(seg);
        }
        Some(body)
    }

    #[test]
    fn cfg2_chunked_reassembly_and_overflow_reject() {
        let max = MAX_CONFIG_SIZE_PIN;
        // A >1024-byte payload spread across multiple segments reassembles exactly.
        let big = vec![b'A'; 1500];
        let segs: Vec<&[u8]> = big.chunks(300).collect();
        let got = accumulate(&segs, max).expect("multi-segment body must reassemble");
        assert_eq!(got.len(), 1500);
        assert_eq!(got, big);

        // A body exceeding the cap is rejected (None), not truncated.
        let huge = vec![b'B'; max + 1];
        let huge_segs: Vec<&[u8]> = huge.chunks(512).collect();
        assert!(accumulate(&huge_segs, max).is_none());

        // Exactly at the cap fits.
        let exact = vec![b'C'; max];
        let exact_segs: Vec<&[u8]> = exact.chunks(512).collect();
        assert_eq!(accumulate(&exact_segs, max).map(|v| v.len()), Some(max));
    }

    #[test]
    fn cfg2_provisioning_uses_bounded_reader() {
        // The POST handler must call the bounded reader with the MAX_CONFIG_SIZE
        // cap and no longer do a fixed 1024-byte single read.
        assert!(
            PROVISIONING_RS.contains("read_full_body(&mut req, nvs_config::MAX_CONFIG_SIZE)"),
            "provisioning POST handler must use the bounded read_full_body with the config-size cap"
        );
        assert!(
            !PROVISIONING_RS.contains("let mut body = vec![0u8; 1024];"),
            "provisioning POST handler must not keep the fixed 1024-byte single-read footgun"
        );
        // Over-cap bodies are rejected (HTTP 413), never truncated.
        assert!(
            PROVISIONING_RS.contains("BodyReadError::TooLarge") && PROVISIONING_RS.contains("413"),
            "an over-cap body must be rejected with HTTP 413"
        );
    }

    // ── CFG-3 — NVS blob read-back-verify ──
    #[test]
    fn cfg3_blob_write_verified_byte_for_byte() {
        assert!(blob_write_verified(b"hello", b"hello"));
        // Truncated / short read-back fails.
        assert!(!blob_write_verified(b"hello", b"hell"));
        // Single-bit (here single-byte) flip fails.
        assert!(!blob_write_verified(b"hello", b"hellp"));
        // Empty read-back fails for a non-empty write.
        assert!(!blob_write_verified(b"hello", b""));
        // Two empties verify (degenerate but consistent).
        assert!(blob_write_verified(b"", b""));
    }

    #[test]
    fn cfg3_save_config_reads_back_and_verifies() {
        // save_config must read the blob back and route the compare through the
        // host-tested decision fn so a future edit can't strip the verify.
        assert!(
            NVS_CONFIG_RS.contains("blob_write_verified"),
            "save_config must call the host-tested blob_write_verified decision"
        );
        // The read-back get_blob must appear AFTER the set_blob write.
        let set_idx = NVS_CONFIG_RS
            .find("nvs.set_blob(NVS_KEY_CONFIG, &json)")
            .expect("save_config must set_blob the config");
        let verify_idx = NVS_CONFIG_RS
            .find("nvs.get_blob(NVS_KEY_CONFIG, &mut verify_buf)")
            .expect("save_config must read the blob back into verify_buf");
        assert!(
            set_idx < verify_idx,
            "the read-back-verify get_blob (byte {verify_idx}) must follow the set_blob write \
             (byte {set_idx})"
        );
    }

    // ── CFG-5 — version-rolling classifier ──
    #[test]
    fn cfg5_chip_rolls_versions_classifier() {
        assert!(!chip_rolls_versions("BM1397"));
        assert!(!chip_rolls_versions(" BM1397 ")); // trims
        assert!(chip_rolls_versions("BM1370"));
        assert!(chip_rolls_versions("BM1368"));
        assert!(chip_rolls_versions("BM1366"));
        // Empty resolves to a rolling default elsewhere; the helper is purely the
        // string test, so an empty (non-BM1397) string rolls.
        assert!(chip_rolls_versions(""));
    }

    #[test]
    fn cfg5_migrate_computes_single_final_asic_model() {
        // The migrate path must compute one final_asic_model and derive both the
        // persisted asic_model and version_rolling from it (not from the resolved
        // profile), closing the divergence.
        assert!(
            NVS_CONFIG_RS.contains("let final_asic_model ="),
            "migrate_axeos_config must compute a single final_asic_model"
        );
        assert!(
            NVS_CONFIG_RS.contains("chip_rolls_versions(&final_asic_model)"),
            "version_rolling must be derived from final_asic_model via chip_rolls_versions"
        );
        assert!(
            NVS_CONFIG_RS.contains("asic_model: final_asic_model"),
            "the persisted asic_model must BE final_asic_model"
        );
        // The old divergent source of truth is gone.
        assert!(
            !NVS_CONFIG_RS.contains("resolved_profile.asic_model != \"BM1397\""),
            "the old version_rolling = resolved_profile.asic_model != BM1397 divergence must be removed"
        );
    }

    // ── CFG-7 — single fan-default source of truth ──
    #[test]
    fn cfg7_default_fan_target_is_centralized() {
        assert_eq!(DEFAULT_FAN_TARGET_TEMP_C, 0); // 0 = manual mode, magic preserved
        assert_eq!(
            DcentAxeConfig::default().fan_target_temp_c,
            DEFAULT_FAN_TARGET_TEMP_C
        );
        // All three sites reference the const; no divergent literal remains.
        assert!(
            PROVISIONING_RS.contains("DEFAULT_FAN_TARGET_TEMP_C"),
            "provisioning build_submission must reference DEFAULT_FAN_TARGET_TEMP_C"
        );
        assert!(
            NVS_CONFIG_RS.contains("DEFAULT_FAN_TARGET_TEMP_C"),
            "migrate_axeos_config must reference DEFAULT_FAN_TARGET_TEMP_C"
        );
        assert!(
            !PROVISIONING_RS.contains("fan_target_temp_c: 65"),
            "the divergent provisioning literal fan_target_temp_c: 65 must be removed"
        );
    }

    // ── CFG-6 — submit-time safety gate wiring ──
    #[test]
    fn cfg6_provisioning_gates_before_save() {
        // The POST handler must call validate_safety with the lab bypass BEFORE
        // it calls save_config, mirroring the safety_guards install<enable order.
        let gate_idx = PROVISIONING_RS
            .find(".validate_safety(unsafe_lab_safety_bypass_enabled())")
            .expect("provisioning POST handler must call validate_safety with the lab bypass");
        let save_idx = PROVISIONING_RS
            .find("nvs_config::save_config(&mut nvs, &submission.config)")
            .expect("provisioning POST handler must save_config");
        assert!(
            gate_idx < save_idx,
            "validate_safety gate (byte {gate_idx}) must run BEFORE save_config (byte {save_idx})"
        );
        // The bypass helper mirrors the documented env flag.
        assert!(
            PROVISIONING_RS.contains("DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS"),
            "the submit-time gate must honor the documented DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS escape"
        );
    }

    // ── CFG-12 — reject an unconnectable pool endpoint at provisioning ──
    #[test]
    fn cfg12_validate_pool_endpoint_rejects_unconnectable() {
        // Empty / whitespace-only url → reject (the JSON path defaults an absent
        // pool_url to "").
        assert!(validate_pool_endpoint("", 21496).is_err());
        assert!(validate_pool_endpoint("   ", 21496).is_err());
        // port == 0 → reject (form `"0".parse().unwrap_or(...)` and JSON get_u16
        // both yield 0; "host:0" can never connect).
        assert!(validate_pool_endpoint("public-pool.io", 0).is_err());
        // A scheme with NO host can never resolve → reject.
        assert!(validate_pool_endpoint("stratum+tcp://", 3333).is_err());
        // Legit endpoints pass — every shape the live endpoint parser
        // (dcentaxe_stratum::endpoint_host_from_url) resolves to a non-empty host:
        // bare host, scheme://host, and host:port.
        assert!(validate_pool_endpoint("public-pool.io", 21496).is_ok());
        assert!(validate_pool_endpoint("stratum+tcp://public-pool.io", 3333).is_ok());
        assert!(validate_pool_endpoint("solo.ckpool.org:3333", 3333).is_ok());
        // Conservative (NOT rejected on purpose): a url carrying creds resolves to
        // a host, so it passes — DNS/connect is the real authority and
        // over-rejection would lock out a legitimate operator value.
        assert!(validate_pool_endpoint("user:pass@pool.example.com", 3333).is_ok());
    }

    #[test]
    fn cfg12_provisioning_validates_pool_endpoint_before_save() {
        // build_submission must reject an unconnectable pool endpoint BEFORE it
        // constructs + returns the config the POST handler saves and renders
        // "Configuration Saved!" for. Mirror cfg6's byte-ordering pin.
        let call_idx = PROVISIONING_RS
            .find("crate::config::validate_pool_endpoint(&pool_url, pool_port)")
            .expect("build_submission must call validate_pool_endpoint(&pool_url, pool_port)");
        // The config the handler saves is constructed at `let base_config = DcentAxeConfig {`.
        let build_idx = PROVISIONING_RS
            .find("let base_config = DcentAxeConfig {")
            .expect("build_submission must construct the config to save");
        assert!(
            call_idx < build_idx,
            "validate_pool_endpoint (byte {call_idx}) must run BEFORE the saved config is built (byte {build_idx})"
        );
        // …and AFTER the worker check (task: right after the worker check).
        let worker_idx = PROVISIONING_RS
            .find("Bitcoin address required as worker name")
            .expect("build_submission must keep the worker-name check");
        assert!(
            worker_idx < call_idx,
            "validate_pool_endpoint (byte {call_idx}) must run after the worker check (byte {worker_idx})"
        );
    }

    // ── CFG-8 — UTF-8-correct URL decoder ──
    #[test]
    fn cfg8_url_decode_utf8_and_fallbacks() {
        // ASCII unchanged.
        assert_eq!(url_decode("abc"), "abc");
        // '+' becomes space.
        assert_eq!(url_decode("a+b"), "a b");
        // A 2-byte UTF-8 char (é = C3 A9) reassembles, not Latin-1 mojibake.
        assert_eq!(url_decode("%C3%A9"), "é");
        // A 3-byte UTF-8 char (€ = E2 82 AC) round-trips.
        assert_eq!(url_decode("%E2%82%AC"), "€");
        // Mixed literal + multi-byte.
        assert_eq!(url_decode("caf%C3%A9+bar"), "café bar");
        // Invalid hex falls back to literal.
        assert_eq!(url_decode("%ZZ"), "%ZZ");
        // Truncated percent at end-of-string does not panic and re-emits literally.
        assert_eq!(url_decode("ab%A"), "ab%A");
        assert_eq!(url_decode("%"), "%");
        // A lone invalid byte sequence still yields a valid UTF-8 String (lossy),
        // never panics: "%FF" is a lone 0xFF -> replacement char.
        let lossy = url_decode("%FF");
        assert!(std::str::from_utf8(lossy.as_bytes()).is_ok());
    }

    #[test]
    fn cfg8_provisioning_uses_shared_url_decode() {
        assert!(
            PROVISIONING_RS.contains("crate::config::url_decode(value)"),
            "the form parser must call the host-tested crate::config::url_decode"
        );
        assert!(
            !PROVISIONING_RS.contains("fn url_decode(s: &str) -> String {"),
            "provisioning must not keep a private divergent url_decode"
        );
    }

    // ── CFG-4 — credentials are not leaked on the provisioning path ──
    // No at-rest encryption is claimed here (that is operator-gated
    // sdkconfig/eFuse work). The deliverable guard is a NEGATIVE one: ensure the
    // provisioning path never formats the user WiFi PSK / pool password into a
    // log line, so a future edit can't introduce a secret leak. The only password
    // shown by design is the ephemeral CSPRNG hotspot password on the OLED.
    #[test]
    fn cfg4_provisioning_never_logs_user_secrets() {
        for line in PROVISIONING_RS.lines() {
            let l = line.trim_start();
            let is_log = l.starts_with("info!")
                || l.starts_with("warn!")
                || l.starts_with("error!")
                || l.starts_with("log::info!")
                || l.starts_with("log::warn!")
                || l.starts_with("log::error!")
                || l.starts_with("debug!")
                || l.starts_with("log::debug!");
            if !is_log {
                continue;
            }
            // The user WiFi PSK / pool password variables must never appear in a
            // log statement. (`ap_password` — the ephemeral OLED recovery code —
            // is shown on the display, not via the log macros guarded here.)
            assert!(
                !line.contains("wifi_password"),
                "provisioning must never log the user WiFi PSK: {line}"
            );
            assert!(
                !line.contains("pool_pass") && !line.contains("stratum.password"),
                "provisioning must never log the pool password: {line}"
            );
        }
    }

    // ── CFG-10 — schema forward/backward decision ──
    #[test]
    fn cfg10_schema_action_classifies() {
        assert_eq!(schema_action(0, 1), SchemaAction::MigrateForward(0));
        assert_eq!(schema_action(1, 1), SchemaAction::Current);
        assert_eq!(schema_action(2, 1), SchemaAction::RefuseFuture);
        assert_eq!(schema_action(255, 1), SchemaAction::RefuseFuture);
    }

    #[test]
    fn cfg10_future_blob_does_not_claim_future_shape() {
        // A simulated FUTURE blob (schema_version = 2) deserialized through the
        // current loader struct, then run through the schema decision, must NOT
        // be left claiming the future shape: schema_action says RefuseFuture and
        // the marker is clamped to the current SCHEMA_VERSION. Known fields
        // (wifi_ssid, stratum) survive serde round-trip.
        let mut cfg = DcentAxeConfig::default();
        cfg.wifi_ssid = "homenet".to_string();
        cfg.schema_version = 2; // pretend a newer firmware wrote this
        let json = serde_json::to_string(&cfg).unwrap();
        let mut loaded: DcentAxeConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(
            schema_action(loaded.schema_version, SCHEMA_VERSION),
            SchemaAction::RefuseFuture
        );
        // Apply the clamp the migrate path performs on RefuseFuture.
        if schema_action(loaded.schema_version, SCHEMA_VERSION) == SchemaAction::RefuseFuture {
            loaded.schema_version = SCHEMA_VERSION;
        }
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.wifi_ssid, "homenet"); // known field preserved
    }

    // CFG-10 (2026-06-29): migrate_config moved into this host-compiled module,
    // so we now behavior-test the real mutation instead of string-matching it.
    #[test]
    fn cfg10_migrate_config_stamps_legacy_schema_to_current() {
        // A legacy blob (schema_version = 0, the serde default for a pre-schema
        // config) is forward-migrated to the current schema while every known
        // field — WiFi creds, stratum endpoint — survives untouched.
        let mut cfg = DcentAxeConfig::default();
        cfg.schema_version = 0;
        cfg.wifi_ssid = "homenet".to_string();
        cfg.wifi_password = "secret".to_string();
        cfg.stratum.url = "public-pool.io".to_string();
        cfg.stratum.port = 21496;

        migrate_config(&mut cfg);

        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert_eq!(cfg.wifi_ssid, "homenet");
        assert_eq!(cfg.wifi_password, "secret");
        assert_eq!(cfg.stratum.url, "public-pool.io");
        assert_eq!(cfg.stratum.port, 21496);
    }

    #[test]
    fn cfg10_migrate_config_clamps_future_blob_refusefuture() {
        // A blob written by NEWER firmware (schema_version > SCHEMA_VERSION) is
        // the RefuseFuture arm: the marker is clamped DOWN to the current version
        // so the loader never claims to round-trip the future shape, and known
        // fields survive.
        let mut cfg = DcentAxeConfig::default();
        cfg.schema_version = SCHEMA_VERSION.saturating_add(1);
        cfg.wifi_ssid = "homenet".to_string();
        assert_eq!(
            schema_action(cfg.schema_version, SCHEMA_VERSION),
            SchemaAction::RefuseFuture
        );

        migrate_config(&mut cfg);

        assert_eq!(cfg.schema_version, SCHEMA_VERSION);
        assert_eq!(cfg.wifi_ssid, "homenet");
    }

    #[test]
    fn cfg10_migrate_config_is_idempotent() {
        // Running migrate_config twice equals running it once (additive,
        // idempotent migrations — old firmware may still be in the field).
        let mut once = DcentAxeConfig::default();
        once.schema_version = 0;
        once.wifi_ssid = "homenet".to_string();
        migrate_config(&mut once);

        let mut twice = DcentAxeConfig::default();
        twice.schema_version = 0;
        twice.wifi_ssid = "homenet".to_string();
        migrate_config(&mut twice);
        migrate_config(&mut twice);

        assert_eq!(twice.schema_version, SCHEMA_VERSION);
        assert_eq!(once.schema_version, twice.schema_version);
        assert_eq!(once.wifi_ssid, twice.wifi_ssid);
    }

    #[test]
    fn cfg10_migrate_config_still_invoked_from_nvs_loader() {
        // The mutation moved into config.rs (host-tested above). Pin that the NVS
        // loader — which cannot host-compile (esp-idf-svc) — still calls it on
        // every load, so the behavior actually runs on device.
        assert!(
            NVS_CONFIG_RS.contains("crate::config::migrate_config(&mut config)"),
            "nvs_config::load_config must call crate::config::migrate_config on the loaded blob"
        );
    }

    // ── XPSAFE-3 — extended safety-gate regression pins (beyond Phase-1 XPH-1/2) ──

    /// Build a custom (non-table) hardware override for XPSAFE-3, reusing the
    /// XPH-1 controller-kind pattern.
    fn xpsafe3_custom_hw(
        fan: FanControllerKind,
        temp: TempSensorKind,
        power: PowerControllerKind,
    ) -> BoardHardwareConfig {
        custom_hw(fan, temp, power)
    }

    #[test]
    fn xpsafe3_custom_board_each_missing_controller_fails_closed() {
        // Each of the three required controllers, individually absent on a custom
        // mining-capable board, must Err without the bypass and Ok with it.
        let cases = [
            xpsafe3_custom_hw(
                FanControllerKind::None,
                TempSensorKind::Emc2101,
                PowerControllerKind::Tps546,
            ),
            xpsafe3_custom_hw(
                FanControllerKind::Emc2101,
                TempSensorKind::None,
                PowerControllerKind::Tps546,
            ),
            xpsafe3_custom_hw(
                FanControllerKind::Emc2101,
                TempSensorKind::Emc2101,
                PowerControllerKind::None,
            ),
        ];
        for hw in cases {
            let mut cfg = DcentAxeConfig::default();
            cfg.board_version = "DCENT-XPSAFE3-NOPROFILE".to_string();
            assert!(BoardVersionProfile::find(&cfg.board_version).is_none());
            cfg.hardware = Some(hw);
            assert!(cfg.board_config().mining_capable());
            assert!(
                cfg.validate_safety(false).is_err(),
                "missing required controller must fail-closed without bypass"
            );
            assert!(
                cfg.validate_safety(true).is_ok(),
                "explicit lab bypass must permit the bench exception"
            );
        }
    }

    #[test]
    fn xpsafe3_qualify_operating_point_clamp_invariants() {
        // Across several models, any input is clamped into [min, max] and the
        // clamped flag is set iff a clamp actually happened.
        for (model, ver) in [
            (BitAxeModel::Gamma, "601"),
            (BitAxeModel::GammaTurbo, "801"),
            (BitAxeModel::Max, ""),
            (BitAxeModel::HexSupra, ""),
        ] {
            let mut cfg = DcentAxeConfig::default();
            cfg.board_model = model.canonical_key().to_string();
            if !ver.is_empty() {
                cfg.board_version = ver.to_string();
            } else {
                // For models without an explicit version here, drive selection by
                // asic to land on the intended model.
                cfg.board_version = String::new();
            }
            cfg.canonicalize_identity();

            let board = cfg.board_config();
            let stock = stock_asic_settings(board.model);
            let limits = cfg.power_limits();
            let min_f = stock.frequency_options.iter().copied().min().unwrap_or(50) as f32;
            let max_f = limits.max_frequency.min(
                stock
                    .frequency_options
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(limits.max_frequency.round() as u16) as f32,
            );

            // Above-max freq AND above-max voltage clamps BOTH down + clamped.
            let over = cfg.qualify_operating_point(99_999.0, u16::MAX, ControlSurface::Autotuner);
            assert!(over.clamped, "{model:?}: out-of-range must report clamped");
            assert!(over.frequency_mhz <= max_f + f32::EPSILON);
            assert!(over.voltage_mv <= limits.max_voltage_mv);

            // Below-min freq clamps UP and is reported clamped.
            let under = cfg.qualify_operating_point(1.0, 1, ControlSurface::Autotuner);
            assert!(under.clamped, "{model:?}: below-min must report clamped");
            assert!(under.frequency_mhz >= min_f.min(max_f) - f32::EPSILON);

            // Fuzz: every output is inside the envelope.
            for f in [50.0_f32, 200.0, 400.0, 600.0, 1200.0] {
                for v in [800u16, 1100, 1300, 1600] {
                    let qp = cfg.qualify_operating_point(f, v, ControlSurface::Autotuner);
                    assert!(qp.frequency_mhz >= min_f.min(max_f) - f32::EPSILON);
                    assert!(qp.frequency_mhz <= max_f + f32::EPSILON);
                    assert!(qp.voltage_mv <= limits.max_voltage_mv);
                }
            }
        }
    }

    #[test]
    fn xpsafe3_gammaturbo_voltage_cap_holds_without_overclock() {
        let mut cfg = DcentAxeConfig::default();
        cfg.board_model = "gammaturbo".to_string();
        cfg.board_version = "801".to_string();
        cfg.overclock_enabled = false;
        let board = cfg.board_config();
        assert_eq!(board.model, BitAxeModel::GammaTurbo);
        let default_v = board.default_voltage_mv;
        // Even a request at the raw safe envelope max is capped to default in
        // non-overclock mode.
        let safe_max_v = PowerLimits::safe(BitAxeModel::GammaTurbo).max_voltage_mv;
        assert!(
            safe_max_v > default_v,
            "test premise: safe max exceeds default"
        );
        let qp = cfg.qualify_operating_point(500.0, safe_max_v, ControlSurface::Autotuner);
        assert_eq!(qp.voltage_mv, default_v);
        assert!(qp.clamped);
    }
}
