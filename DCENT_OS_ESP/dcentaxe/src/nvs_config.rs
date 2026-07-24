// DCENT_axe NVS Configuration Persistence
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Stores and retrieves DcentAxeConfig from ESP32 NVS (Non-Volatile Storage).
// Config is serialized as JSON into a single NVS blob.

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;
use log::*;

use crate::config::DcentAxeConfig;
use dcentaxe_hal::board::{
    BitAxeModel, BoardConfig, BoardHardwareConfig, BoardVersionProfile, FanControllerKind,
    PowerControllerKind, TempSensorKind,
};
use serde::{Deserialize, Serialize};

/// NVS namespace for DCENT_axe configuration.
const NVS_NAMESPACE: &str = "dcentaxe";

/// NVS key for the JSON config blob.
const NVS_KEY_CONFIG: &str = "config";

/// Maximum config JSON size. Schedule slots add real payload, so keep enough
/// headroom while staying below the practical single-blob NVS limit.
///
/// Exported `pub` (CFG-2) so the provisioning POST handler caps its bounded
/// body-accumulation loop at the SAME limit it would be saved under, rejecting
/// an over-cap body with HTTP 413 rather than silently truncating a fixed buffer.
pub const MAX_CONFIG_SIZE: usize = 3584;
const NVS_KEY_LKG_VF: &str = "lkg_vf";
const NVS_KEY_OTA_FLOOR: &str = "ota_floor";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastKnownGoodPoint {
    pub board_version: String,
    pub asic_model: String,
    pub overclock_enabled: bool,
    pub frequency_mhz: f32,
    pub voltage_mv: u16,
    pub hashrate_30s_ghs: f32,
    pub power_w: f32,
    pub jth: f32,
    pub delta_error_rate: f32,
}

/// Load configuration from NVS.
///
/// Returns `None` if no config has been saved yet (first boot).
pub fn load_config(
    nvs_partition: EspDefaultNvsPartition,
) -> (Option<DcentAxeConfig>, EspNvs<NvsDefault>) {
    let nvs_backup = nvs_partition.clone();
    let nvs = match EspNvs::new(nvs_partition, NVS_NAMESPACE, true) {
        Ok(nvs) => nvs,
        Err(e) => {
            let code = e.code();
            let recoverable = code == sys::ESP_ERR_NVS_NO_FREE_PAGES
                || code == sys::ESP_ERR_NVS_NEW_VERSION_FOUND;
            if !recoverable {
                error!(
                    "NVS open failed: {:?} (code={}) — preserving NVS and rebooting",
                    e, code
                );
                unsafe {
                    sys::esp_restart();
                }
            }

            warn!(
                "NVS open failed with recoverable code {} — erasing and retrying",
                code
            );
            unsafe {
                let erase_rc = sys::nvs_flash_erase();
                if erase_rc != sys::ESP_OK as i32 {
                    error!("NVS erase failed (rc={})", erase_rc);
                }
                let init_rc = sys::nvs_flash_init();
                if init_rc != sys::ESP_OK as i32 {
                    error!("NVS init failed after erase (rc={})", init_rc);
                }
            }
            match EspNvs::new(nvs_backup, NVS_NAMESPACE, true) {
                Ok(nvs) => return (None, nvs),
                Err(e2) => {
                    error!("NVS recovery failed: {:?} — rebooting", e2);
                    unsafe {
                        sys::esp_restart();
                    }
                }
            }
        }
    };

    let mut buf = [0u8; MAX_CONFIG_SIZE];
    match nvs.get_blob(NVS_KEY_CONFIG, &mut buf) {
        Ok(Some(data)) => match serde_json::from_slice::<DcentAxeConfig>(data) {
            Ok(mut config) => {
                config.canonicalize_identity();
                // CFG-10: the real forward-migration mutation now lives in the
                // host-compiled `config` module (single-source `#[path]`), where
                // it is unit-tested. Call it here on every loaded blob.
                crate::config::migrate_config(&mut config);
                info!(
                    "NVS: loaded config — model={}, pool={}:{}, schema={}",
                    config.board_model,
                    config.stratum.url,
                    config.stratum.port,
                    config.schema_version,
                );
                (Some(config), nvs)
            }
            Err(e) => {
                warn!(
                    "NVS: config JSON parse failed: {} — treating as first boot",
                    e
                );
                (None, nvs)
            }
        },
        Ok(None) => {
            // Try migrating from stock AxeOS NVS keys (different namespace: "main")
            info!("NVS: no DCENT_axe config — attempting AxeOS migration");
            let migrated = migrate_axeos_config(&nvs_backup);
            if let Some(config) = migrated {
                let mut config = config;
                config.canonicalize_identity();
                info!(
                    "NVS: migrated AxeOS config — model={}, pool={}:{}",
                    config.board_model, config.stratum.url, config.stratum.port
                );
                let mut nvs_mut = nvs;
                let _ = save_config(&mut nvs_mut, &config);
                (Some(config), nvs_mut)
            } else {
                info!("NVS: no config available after migration attempt");
                (None, nvs)
            }
        }
        Err(e) => {
            warn!("NVS: read failed: {:?} — treating as first boot", e);
            (None, nvs)
        }
    }
}

/// NVS key for the all-time best share difficulty.
const NVS_KEY_BEST_DIFF: &str = "best_diff";

/// Load the all-time best share difficulty from NVS.
pub fn load_best_diff(nvs: &esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>) -> f64 {
    let mut buf = [0u8; 8];
    match nvs.get_blob(NVS_KEY_BEST_DIFF, &mut buf) {
        Ok(Some(data)) if data.len() == 8 => {
            let val = f64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]);
            info!("NVS: loaded best diff ever: {:.2}", val);
            val
        }
        _ => 0.0,
    }
}

/// Save a new all-time best share difficulty to NVS.
pub fn save_best_diff(nvs: &mut esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>, diff: f64) {
    let bytes = diff.to_le_bytes();
    match nvs.set_blob(NVS_KEY_BEST_DIFF, &bytes) {
        Ok(()) => info!("NVS: saved new best diff ever: {:.2}", diff),
        Err(e) => warn!("NVS: failed to save best diff: {:?}", e),
    }
}

/// NVS keys for the task-watchdog safe-mode recovery flow.
/// `wdt_count` — number of task-WDT-triggered resets inside the current window.
/// `wdt_since` — uptime epoch (ms since UNIX epoch) that the window started.
const NVS_KEY_WDT_COUNT: &str = "wdt_count";
const NVS_KEY_WDT_SINCE: &str = "wdt_since";

/// Load WDT reset counters. Returns `(count, window_start_ms)`.
pub fn load_wdt_counters(
    nvs: &esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
) -> (u8, u64) {
    let count = nvs.get_u8(NVS_KEY_WDT_COUNT).ok().flatten().unwrap_or(0);
    let mut buf = [0u8; 8];
    let since = match nvs.get_blob(NVS_KEY_WDT_SINCE, &mut buf) {
        Ok(Some(d)) if d.len() == 8 => {
            u64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
        }
        _ => 0,
    };
    (count, since)
}

/// Save WDT reset counters.
pub fn save_wdt_counters(
    nvs: &mut esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
    count: u8,
    since_ms: u64,
) {
    let _ = nvs.set_u8(NVS_KEY_WDT_COUNT, count);
    let bytes = since_ms.to_le_bytes();
    let _ = nvs.set_blob(NVS_KEY_WDT_SINCE, &bytes);
}

/// NVS key for the last-seen pool difficulty. Used to prime the ASIC TicketMask at boot.
const NVS_KEY_CACHED_POOL_DIFF: &str = "cached_diff";

/// Load the cached pool difficulty from NVS. Returns 0.0 when absent, letting the
/// caller fall back to the driver default (256.0).
pub fn load_cached_pool_difficulty(
    nvs: &esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
) -> f64 {
    let mut buf = [0u8; 8];
    match nvs.get_blob(NVS_KEY_CACHED_POOL_DIFF, &mut buf) {
        Ok(Some(data)) if data.len() == 8 => f64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]),
        _ => 0.0,
    }
}

/// Persist the most recent pool-suggested difficulty so the next boot primes the
/// TicketMask at the right level (ESP-Miner PR #1594 parity).
pub fn save_cached_pool_difficulty(
    nvs: &mut esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
    diff: f64,
) {
    let bytes = diff.to_le_bytes();
    match nvs.set_blob(NVS_KEY_CACHED_POOL_DIFF, &bytes) {
        Ok(()) => log::debug!("NVS: cached pool diff {:.3}", diff),
        Err(e) => warn!("NVS: failed to save cached pool diff: {:?}", e),
    }
}

// ── Swarm persistence ─────────────────────────────────────────────────────
// Last-known peer list + Queen ID so a rebooted node rejoins the cluster in
// ~2 s rather than waiting a full mDNS discovery cycle.

const NVS_KEY_SWARM_PEERS: &str = "swarm_peers";
const NVS_KEY_SWARM_QUEEN: &str = "swarm_queen";
const SWARM_BLOB_MAX: usize = 3072; // 8 peers × ~300 B each.

pub fn save_swarm_peers(
    nvs: &mut esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
    peers_json: &str,
) {
    let bytes = peers_json.as_bytes();
    if bytes.len() > SWARM_BLOB_MAX {
        warn!(
            "NVS: swarm peers blob {} B > {} B cap — skipping persist",
            bytes.len(),
            SWARM_BLOB_MAX
        );
        return;
    }
    if let Err(e) = nvs.set_blob(NVS_KEY_SWARM_PEERS, bytes) {
        warn!("NVS: save swarm peers failed: {:?}", e);
    }
}

pub fn load_swarm_peers(nvs: &esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>) -> String {
    let mut buf = [0u8; SWARM_BLOB_MAX];
    match nvs.get_blob(NVS_KEY_SWARM_PEERS, &mut buf) {
        Ok(Some(data)) => String::from_utf8_lossy(data).into_owned(),
        _ => String::new(),
    }
}

pub fn save_swarm_queen_id(
    nvs: &mut esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
    queen_id: &str,
) {
    if let Err(e) = nvs.set_str(NVS_KEY_SWARM_QUEEN, queen_id) {
        warn!("NVS: save swarm queen_id failed: {:?}", e);
    }
}

pub fn load_swarm_queen_id(
    nvs: &esp_idf_svc::nvs::EspNvs<esp_idf_svc::nvs::NvsDefault>,
) -> Option<String> {
    let mut buf = [0u8; 64];
    match nvs.get_str(NVS_KEY_SWARM_QUEEN, &mut buf) {
        Ok(Some(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

// ─── Achievement system persistence ──────────────────────────────────────
// Achievements stored as a u32 bitfield in NVS. Each bit = one achievement.

/// Achievement bitflags.
pub const ACH_FIRST_SHARE: u32 = 1 << 0; // First ever accepted share
pub const ACH_CENTURION: u32 = 1 << 1; // 100 shares in one session
pub const ACH_MARATHON: u32 = 1 << 2; // 24 hours continuous mining
pub const ACH_HOT_STUFF: u32 = 1 << 3; // Survived a thermal warning
pub const ACH_BEST_DAY: u32 = 1 << 4; // New all-time best difficulty
pub const ACH_BLOCK_WITNESS: u32 = 1 << 5; // Witnessed 100 new blocks
pub const ACH_STREAK_50: u32 = 1 << 6; // 50 shares without rejection
pub const ACH_KILOHASH: u32 = 1 << 7; // 1000 shares in one session
pub const ACH_SPEED_DEMON: u32 = 1 << 8; // Hashrate > 1 TH/s
pub const ACH_CHILL_MINER: u32 = 1 << 9; // 1 hour under 55°C
pub const ACH_DIFF_HUNTER: u32 = 1 << 10; // Share diff > 1M
pub const ACH_NIGHT_OWL: u32 = 1 << 11; // 16+ hours uptime
pub const ACH_HALF_K: u32 = 1 << 12; // 500 shares in one session
pub const ACH_WARM_DAY: u32 = 1 << 13; // 8 hours continuous mining
pub const ACH_STREAK_100: u32 = 1 << 14; // 100 shares without rejection
pub const ACH_HASH_KING: u32 = 1 << 15; // Hashrate > 90% of board's rated max
pub const ACH_EARLY_ADOPTER: u32 = 1 << 16; // Flash DCENT_axe (auto-grant first boot)
pub const ACH_EFFICIENCY: u32 = 1 << 17; // Achieve < 20 J/TH for 10 minutes
pub const ACH_DIAMOND_HANDS: u32 = 1 << 18; // 7 days cumulative uptime
pub const ACH_LUCKY_STRIKE: u32 = 1 << 19; // Share diff > 10M
pub const ACH_POWER_MISER: u32 = 1 << 20; // Run under 10W for 1 hour
pub const ACH_BLOCK_PARTY: u32 = 1 << 21; // Witness 1000 blocks
pub const ACH_CREATURE_LEGEND: u32 = 1 << 22; // Evolve to Legend (10K+ lifetime)
pub const ACH_COMPLETIONIST: u32 = 1 << 23; // Unlock all other 23 achievements

/// Total number of achievements in the system.
pub const ACHIEVEMENT_TOTAL: u32 = 24;

/// Load achievement bitfield from NVS.
pub fn load_achievements(nvs: &EspNvs<NvsDefault>) -> u32 {
    let mut buf = [0u8; 4];
    match nvs.get_blob("achieve", &mut buf) {
        Ok(Some(data)) if data.len() == 4 => {
            u32::from_le_bytes([data[0], data[1], data[2], data[3]])
        }
        _ => 0,
    }
}

/// Save achievement bitfield to NVS.
pub fn save_achievements(nvs: &mut EspNvs<NvsDefault>, bits: u32) {
    let bytes = bits.to_le_bytes();
    match nvs.set_blob("achieve", &bytes) {
        Ok(()) => info!("NVS: saved achievements: 0x{:08X}", bits),
        Err(e) => warn!("NVS: failed to save achievements: {:?}", e),
    }
}

/// Load best streak from NVS.
pub fn load_best_streak(nvs: &EspNvs<NvsDefault>) -> u32 {
    let mut buf = [0u8; 4];
    match nvs.get_blob("bstreak", &mut buf) {
        Ok(Some(data)) if data.len() == 4 => {
            u32::from_le_bytes([data[0], data[1], data[2], data[3]])
        }
        _ => 0,
    }
}

/// Save best streak to NVS.
pub fn save_best_streak(nvs: &mut EspNvs<NvsDefault>, streak: u32) {
    let bytes = streak.to_le_bytes();
    match nvs.set_blob("bstreak", &bytes) {
        Ok(()) => info!("NVS: saved best streak: {}", streak),
        Err(e) => warn!("NVS: failed to save best streak: {:?}", e),
    }
}

/// Count the number of unlocked achievements.
pub fn achievement_count(bits: u32) -> u32 {
    bits.count_ones()
}

/// Get a human-readable name for an achievement.
pub fn achievement_name(bit: u32) -> &'static str {
    match bit {
        ACH_FIRST_SHARE => "First Share!",
        ACH_CENTURION => "Centurion (100)",
        ACH_MARATHON => "Marathon (24h)",
        ACH_HOT_STUFF => "Hot Stuff!",
        ACH_BEST_DAY => "Best Day Ever!",
        ACH_BLOCK_WITNESS => "Block Witness",
        ACH_STREAK_50 => "Streak Master",
        ACH_KILOHASH => "Kilohash (1000)",
        ACH_SPEED_DEMON => "TERAHASH CLUB!",
        ACH_CHILL_MINER => "Cool & Collected",
        ACH_DIFF_HUNTER => "Million Diff!",
        ACH_NIGHT_OWL => "Night Owl (16h)",
        ACH_HALF_K => "Half-K (500)",
        ACH_WARM_DAY => "Warm Day (8h)",
        ACH_STREAK_100 => "Perfect Century!",
        ACH_HASH_KING => "Hash King!",
        ACH_EARLY_ADOPTER => "Early Adopter!",
        ACH_EFFICIENCY => "Efficiency Expert",
        ACH_DIAMOND_HANDS => "Diamond Hands!",
        ACH_LUCKY_STRIKE => "Lucky Strike!",
        ACH_POWER_MISER => "Power Miser!",
        ACH_BLOCK_PARTY => "Block Party!",
        ACH_CREATURE_LEGEND => "Creature Legend!",
        ACH_COMPLETIONIST => "Completionist!",
        _ => "???",
    }
}

// ─── Creature Evolution System persistence ──────────────────────────────
// Lifetime shares persist across reboots for permanent creature evolution.

/// Load lifetime shares from NVS.
pub fn load_lifetime_shares(nvs: &EspNvs<NvsDefault>) -> u32 {
    let mut buf = [0u8; 4];
    match nvs.get_blob("lt_shares", &mut buf) {
        Ok(Some(data)) if data.len() == 4 => {
            u32::from_le_bytes([data[0], data[1], data[2], data[3]])
        }
        _ => 0,
    }
}

/// Save lifetime shares to NVS.
pub fn save_lifetime_shares(nvs: &mut EspNvs<NvsDefault>, shares: u32) {
    let bytes = shares.to_le_bytes();
    match nvs.set_blob("lt_shares", &bytes) {
        Ok(()) => info!("NVS: saved lifetime shares: {}", shares),
        Err(e) => warn!("NVS: failed to save lifetime shares: {:?}", e),
    }
}

/// Load best nonce (highest difficulty nonce found, for Hall of Fame).
pub fn load_best_nonce(nvs: &EspNvs<NvsDefault>) -> (u32, f64) {
    let mut buf = [0u8; 12];
    match nvs.get_blob("best_nonce", &mut buf) {
        Ok(Some(data)) if data.len() == 12 => {
            let nonce = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            let diff = f64::from_le_bytes([
                data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
            ]);
            (nonce, diff)
        }
        _ => (0, 0.0),
    }
}

/// Save best nonce to NVS (nonce value + its difficulty).
pub fn save_best_nonce(nvs: &mut EspNvs<NvsDefault>, nonce: u32, diff: f64) {
    let mut bytes = [0u8; 12];
    bytes[0..4].copy_from_slice(&nonce.to_le_bytes());
    bytes[4..12].copy_from_slice(&diff.to_le_bytes());
    match nvs.set_blob("best_nonce", &bytes) {
        Ok(()) => info!("NVS: saved best nonce: 0x{:08X} diff={:.0}", nonce, diff),
        Err(e) => warn!("NVS: failed to save best nonce: {:?}", e),
    }
}

pub fn load_last_known_good(nvs: &EspNvs<NvsDefault>) -> Option<LastKnownGoodPoint> {
    let mut buf = [0u8; 256];
    match nvs.get_blob(NVS_KEY_LKG_VF, &mut buf) {
        Ok(Some(data)) => serde_json::from_slice::<LastKnownGoodPoint>(data).ok(),
        _ => None,
    }
}

pub fn save_last_known_good(nvs: &mut EspNvs<NvsDefault>, point: &LastKnownGoodPoint) {
    match serde_json::to_vec(point) {
        Ok(json) => {
            if let Err(e) = nvs.set_blob(NVS_KEY_LKG_VF, &json) {
                warn!("NVS: failed to save last-known-good point: {:?}", e);
            } else {
                info!(
                    "NVS: saved last-known-good point {:.2} MHz / {} mV",
                    point.frequency_mhz, point.voltage_mv
                );
            }
        }
        Err(e) => warn!("NVS: failed to serialize last-known-good point: {}", e),
    }
}

pub fn load_ota_floor(nvs: &EspNvs<NvsDefault>) -> Option<String> {
    let mut buf = [0u8; 64];
    match nvs.get_str(NVS_KEY_OTA_FLOOR, &mut buf) {
        Ok(Some(value)) => Some(value.trim_end_matches('\0').to_string()),
        _ => None,
    }
}

pub fn save_ota_floor(nvs: &mut EspNvs<NvsDefault>, version: &str) -> Result<(), String> {
    nvs.set_str(NVS_KEY_OTA_FLOOR, version)
        .map_err(|e| format!("NVS write failed: {:?}", e))
}

pub fn update_ota_floor_if_newer(
    nvs: &mut EspNvs<NvsDefault>,
    version: &str,
) -> Result<String, String> {
    let current = load_ota_floor(nvs).unwrap_or_default();
    if current.is_empty() || crate::ota_signature::version_is_newer(version, &current) {
        save_ota_floor(nvs, version)?;
        Ok(version.to_string())
    } else {
        Ok(current)
    }
}

/// Get evolution stage name based on lifetime shares.
pub fn evolution_stage(lifetime_shares: u32) -> (&'static str, &'static str) {
    match lifetime_shares {
        0 => ("Egg", "(o)"),
        1..=99 => ("Hatchling", "(^_^)"),
        100..=999 => ("Miner", "[>_]#"),
        1000..=4999 => ("Veteran", "{^o^}=|"),
        5000..=9999 => ("Elder", "<*_*>=#"),
        _ => ("Legend", ">>(*_*)<<"),
    }
}

/// Generate a deterministic creature name from the device MAC address.
pub fn creature_name(mac_byte: u8) -> &'static str {
    const NAMES: &[&str] = &[
        "Sparky", "Nugget", "Bolt", "Pixel", "Chip", "Wattson", "Ohm", "Nonce", "Blocky", "Hashy",
        "Minty", "Satsy", "Bitsy", "Zippy", "Buzzy", "Diggy",
    ];
    NAMES[mac_byte as usize % NAMES.len()]
}

/// Migrate configuration from stock AxeOS NVS keys.
///
/// Stock AxeOS stores config as individual key-value pairs in the "main" namespace.
/// We read those keys and build a DcentAxeConfig from them.
fn migrate_axeos_config(nvs_partition: &EspDefaultNvsPartition) -> Option<DcentAxeConfig> {
    let nvs = EspNvs::<NvsDefault>::new(nvs_partition.clone(), "main", false).ok()?;

    // Helper to read a string from NVS
    let read_str = |key: &str| -> String {
        let mut buf = [0u8; 128];
        match nvs.get_str(key, &mut buf) {
            Ok(Some(s)) => s.trim_end_matches('\0').to_string(),
            _ => String::new(),
        }
    };

    // Helper to read a u16 from NVS
    let read_u16 = |key: &str| -> u16 { nvs.get_u16(key).ok().flatten().unwrap_or(0) };
    let read_bool = |key: &str| -> bool {
        nvs.get_u8(key)
            .ok()
            .flatten()
            .map(|v| v != 0)
            .or_else(|| nvs.get_u16(key).ok().flatten().map(|v| v != 0))
            .unwrap_or(false)
    };
    let read_i32 = |key: &str| -> i32 { nvs.get_i32(key).ok().flatten().unwrap_or(0) };

    let wifi_ssid = read_str("wifissid");
    if wifi_ssid.is_empty() {
        info!("NVS migration: no AxeOS wifissid found");
        return None;
    }

    let device_model = read_str("devicemodel");
    let board_version = read_str("boardversion");
    let asic_model = read_str("asicmodel");
    let hostname = read_str("hostname");
    let stratum_url = read_str("stratumurl");
    let stratum_port = read_u16("stratumport");
    let stratum_user = read_str("stratumuser");
    let stratum_pass = read_str("stratumpass");
    let frequency = read_u16("asicfrequency");
    let voltage = read_u16("asicvoltage");
    let fan_speed = read_u16("fanspeed");

    info!(
        "NVS migration: AxeOS model={}, pool={}:{}",
        device_model, stratum_url, stratum_port
    );

    let custom_board =
        !board_version.is_empty() && BoardVersionProfile::find(&board_version).is_none();
    let resolved_profile = if let Some(profile) = BoardVersionProfile::find(&board_version) {
        profile
    } else if let Some(model) = BitAxeModel::from_device_model(&device_model) {
        BoardVersionProfile::default_for_model(model)
    } else if !asic_model.is_empty() {
        BoardVersionProfile::infer("", "", &asic_model)
    } else {
        crate::config::default_profile_for_build()
    };
    let resolved_board = BoardConfig::for_profile(resolved_profile);

    let hardware_override = if custom_board {
        Some(BoardHardwareConfig {
            plug_sense: read_bool("plug_sense"),
            asic_enable: read_bool("asic_enable"),
            fan_controller: if read_bool("EMC2302") {
                FanControllerKind::Emc2302
            } else if read_bool("EMC2103") {
                FanControllerKind::Emc2103
            } else if read_bool("EMC2101") {
                FanControllerKind::Emc2101
            } else {
                FanControllerKind::None
            },
            temp_sensor: if read_bool("TMP1075") {
                TempSensorKind::Tmp1075
            } else if read_bool("EMC2103") {
                TempSensorKind::Emc2103
            } else if read_bool("EMC2101") {
                TempSensorKind::Emc2101
            } else {
                TempSensorKind::None
            },
            power_controller: if read_bool("TPS546") {
                PowerControllerKind::Tps546
            } else if read_bool("DS4432U") {
                PowerControllerKind::Ds4432u
            } else {
                PowerControllerKind::None
            },
            has_ina260: read_bool("INA260"),
            emc_internal_temp: read_bool("emc_int_temp"),
            emc_ideality_factor: read_u16("emc_ideality_f") as u8,
            emc_beta_compensation: read_u16("emc_beta_comp") as u8,
            temp_offset_c: read_i32("temp_offset") as i8,
            power_consumption_target_w: read_u16("power_cons_tgt"),
        })
    } else {
        None
    };

    // CFG-5: derive ONE final asic_model string and use it BOTH for the
    // persisted `asic_model` field AND for the version_rolling decision, so the
    // two can never disagree. Previously `version_rolling` keyed off the
    // resolved profile while the persisted/driver string kept the stored value
    // verbatim — a divergent AxeOS import (stored asicmodel differs from the
    // board_version-resolved chip) would drive one chip while rolling for the
    // other (losing ASICBoost or rolling a BM1397 that can't).
    let final_asic_model = if asic_model.is_empty() {
        resolved_profile.asic_model.to_string()
    } else {
        asic_model.clone()
    };

    Some(DcentAxeConfig {
        wifi_ssid,
        wifi_password: read_str("wifipass"),
        stratum: dcentaxe_stratum::StratumConfig {
            url: if stratum_url.is_empty() {
                "public-pool.io".into()
            } else {
                stratum_url
            },
            port: if stratum_port == 0 {
                21496
            } else {
                stratum_port
            },
            worker_name: stratum_user
                .trim()
                .trim_start_matches(',')
                .trim()
                .to_string(),
            password: if stratum_pass.is_empty() {
                "x".into()
            } else {
                stratum_pass
            },
            suggest_difficulty: read_u16("stratumdiff") as u32,
            version_rolling: crate::config::chip_rolls_versions(&final_asic_model),
        },
        mining_mode: crate::config::MiningMode::Pool,
        board_model: if device_model.is_empty() {
            resolved_profile.device_model.into()
        } else {
            device_model
        },
        board_version: if board_version.is_empty() {
            resolved_profile.board_version.into()
        } else {
            board_version
        },
        asic_model: final_asic_model,
        // MQTT/HA is a DCENT_axe-native feature with no legacy AxeOS NVS key —
        // default-OFF on migration; the operator opts in via Settings.
        mqtt: crate::config::MqttConfig::default(),
        donation: crate::config::DonationConfig::default(),
        notifications: crate::config::NotificationsConfig::default(),
        // W5500 LAN (PLAN-E): Ethernet-dark on AxeOS migration; the operator
        // opts in via config (and only an eth-w5500 build can act on it).
        network: crate::config::NetworkConfig::default(),
        #[cfg(feature = "lora")]
        mesh: dcentaxe_lora::config::MeshConfig::default(),
        hostname,
        target_frequency: if frequency == 0 {
            resolved_board.default_frequency
        } else {
            frequency as f32
        },
        target_voltage_mv: if voltage == 0 {
            resolved_board.default_voltage_mv
        } else {
            voltage
        },
        fan_speed_pct: if fan_speed == 0 {
            100
        } else {
            fan_speed.min(100) as u8
        },
        asic_count: if custom_board {
            0
        } else {
            resolved_board.asic_count
        },
        overclock_enabled: false,
        display_inverted: false,
        fan_target_temp_c: crate::config::DEFAULT_FAN_TARGET_TEMP_C, // CFG-7
        fallback_pool: None,
        sv2_own_templates: crate::config::Sv2OwnTemplateConfig::default(),
        sv2_authority_pubkey: None,
        split_pool: None,
        schedule_enabled: true,
        schedule_timezone_offset_minutes: 0,
        power_schedule: Vec::new(),
        hardware: hardware_override,
        metrics_require_auth: true,
        allow_unsigned_ota: false,
        room_temp_source: crate::config::RoomTempSource::Local,
        schema_version: crate::config::SCHEMA_VERSION,
    })
}

// CFG-10: `migrate_config` moved to `crate::config` (the host-compiled module)
// so its mutation is unit-tested rather than only string-matched. It is invoked
// from `load_config` above via `crate::config::migrate_config`.

/// Save configuration to NVS.
///
/// CFG-3 / CFG-11: after the blob write we read it straight back and compare
/// byte-for-byte. This is the NVS-layer analogue of the DCENT_OS
/// verify-before-persist / GATHER_STATE discipline (which there guards I²C-EEPROM
/// writes): a torn/short NVS write is surfaced as an Err so the provisioning/API
/// caller returns a 500 instead of believing the save succeeded. NVS itself is a
/// flash key-value store reached through `nvs_flash`, NOT an addressable I²C bus,
/// so the DCENT_OS 0x50-0x57 write-protect DENYLIST concept does not port here
/// (the platform analogue of that I²C write-protect is the TPS546 fault-limit
/// guard); only the verify-before-persist half of the discipline is applicable.
pub fn save_config(nvs: &mut EspNvs<NvsDefault>, config: &DcentAxeConfig) -> Result<(), String> {
    let json = serde_json::to_vec(config).map_err(|e| format!("Config serialize failed: {}", e))?;

    if json.len() > MAX_CONFIG_SIZE {
        return Err(format!(
            "Config too large: {} bytes (max {})",
            json.len(),
            MAX_CONFIG_SIZE
        ));
    }

    nvs.set_blob(NVS_KEY_CONFIG, &json)
        .map_err(|e| format!("NVS write failed: {:?}", e))?;

    // CFG-3: read-back-verify. Detect a torn/short write before reporting success.
    let mut verify_buf = [0u8; MAX_CONFIG_SIZE];
    match nvs.get_blob(NVS_KEY_CONFIG, &mut verify_buf) {
        Ok(Some(read_back)) => {
            if !crate::config::blob_write_verified(&json, read_back) {
                return Err(format!(
                    "NVS write verify failed: wrote {} bytes, read back {} bytes (torn/short write)",
                    json.len(),
                    read_back.len()
                ));
            }
        }
        Ok(None) => {
            return Err("NVS write verify failed: blob absent immediately after write".to_string());
        }
        Err(e) => {
            return Err(format!("NVS write verify read-back failed: {:?}", e));
        }
    }

    info!("NVS: config saved + verified ({} bytes)", json.len());
    Ok(())
}
