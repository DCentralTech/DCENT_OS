#![recursion_limit = "512"]
// DCENT_axe — Clean-room BitAxe mining firmware
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Boot flow:
//   1. Check NVS for saved config
//   2. If no config → start WiFi AP captive portal (setup mode)
//   3. If config exists → connect to WiFi → init hardware → start HTTP server → mine
//   4. If WiFi fails → reboot + retry (3x max, then clear config → setup mode)

mod api;
mod api_system_info;
mod auth;
mod autotuner;
mod bridge;
mod capabilities;
mod cgminer_tcp;
mod chip_profiles_bitaxe;
mod config;
mod dashboard;
mod derived_metrics;
// On-board SX1262 LoRa radio task + $DCM mesh integration. DEFAULT-OFF — compiled
// only when a board opts into the `lora` feature (byte-identical image otherwise).
#[cfg(feature = "lora")]
mod lora_task;
#[cfg(feature = "lora")]
mod mesh_solo_runtime;
mod mcp;
mod metrics_render;
mod mqtt;
mod mqtt_ha;
mod nvs_config;
mod ota_signature;
mod power_measurement;
mod provisioning;
mod self_test;
mod shared;
mod swarm;
mod thermal_safety;
mod wifi;

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::{config::Config as UartConfig, UartDriver};
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpServer};
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::sys;
use log::*;

use dcentaxe_asic::{AsicResult, SerialPort};
use dcentaxe_hal::display::Ssd1306Display;
use dcentaxe_hal::emc2103::Emc2103;
use dcentaxe_hal::emc2302::Emc2302;
use dcentaxe_hal::gpio::GpioController;
use dcentaxe_hal::i2c::I2cBus;
use dcentaxe_hal::power::{PowerError, PowerManager};
// HALPWR-2 / COMP-6: gate each per-field power telemetry read so a NaN sub-read
// (power.rs get_telemetry now returns f32::NAN per failed field) HOLDS the
// last-good value instead of poisoning mean_power_w in the autotuner window.
use dcentaxe_hal::safety::{
    ina260_oc_over_envelope, ina260_oc_should_cut, ina260_oc_strike_next, power_field_available,
};
use dcentaxe_hal::temp::{Emc2101, Tmp1075};
use dcentaxe_mining::dispatcher::{DispatcherConfig, MiningDispatcher, PoolSlot};
use dcentaxe_stratum::client::StratumClient;
use dcentaxe_stratum::types::{MiningEvent, StratumEvent, StratumProtocol};

use autotuner::Autotuner;
use config::{ControlSurface, DcentAxeConfig};
use shared::{stratum_status_snapshots, SharedState};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EMERGENCY_TEMP_C: f32 = 105.0;
const WARNING_TEMP_C: f32 = 90.0;

// ── ES-2: die-sensor-blindness confirm window (fail-closed) ─────────────────
// When a die-equipped board loses EVERY ASIC-die reading while only cooler
// proxies remain (`thermal_safety::evaluate_thermal` → `die_reading_blind`),
// `max_temp` is proxy-derived and understates the true die temp, so the
// overtemp cut could fire late or never. The supervisor forces the fan to 100%
// immediately (immediate airflow is the only safe response when the die is
// unmeasurable) and cuts ASIC power after this many CONSECUTIVE die-blind ticks.
// The debounce (~3 × 5 s = 15 s) rejects a single transient I2C read glitch,
// matching the fan-stall (3-tick) and I2C-dead (3-tick) guards; the proxy-based
// thermal ladder still runs each of those ticks. Fail-safe direction only.
const DIE_BLIND_CONFIRM_TICKS: u32 = 3;

// ── INA260 over-current/over-power backstop (DS4432U boards) ────────────────
// DS4432U boards (Max / Ultra / Supra) have no PMBus
// STATUS_WORD over-current detection — `PowerManager::check_fault` is a no-op
// (R-10 note: DCENT_axe BM1397 is NOT a DS4432U board — it carries a TPS546
// PMBus VRM, EN on GPIO10 active-high, and no INA260 — so it takes the TPS546
// fault path, not this backstop.)
// for them — so the INA260 input-rail monitor is the only software OC guard.
// When measured input power/current stays above the board's rated PowerLimits
// envelope (× a margin) for several consecutive supervisor ticks, run the SAME
// fail-closed power-off the TPS546 fault path uses. The margin sits just above
// the rated PSU envelope (e.g. 25 W board → 31.25 W) and the debounce spans
// ~4 × 5 s ticks so a transient load-step / measurement spike cannot
// nuisance-trip. Fail-safe direction only: this can only ADD a hard-kill.
const INA260_OC_MARGIN: f32 = 1.25;
const INA260_OC_DEBOUNCE_TICKS: u8 = 4;

// ── XPSAFE-5: cut-hash-before-fan-noise thermal policy (home/space-heater) ──
// DCENT's load-bearing posture is "cut hash power before raising fan noise".
// At the WARNING tier (>90 C) we shed a modest amount of hash FIRST, then raise
// the fan only to a home-friendly cap (NOT 100%). These named consts are the
// one-line policy lever: a datacenter/loud profile would raise the cap, but the
// home default stays quiet. The 95 C tier still goes to 100% and the 105 C
// EMERGENCY tier still cuts hash entirely — those hard backstops are never
// lowered. NEVER lower a threshold or the fan floor.
const THERMAL_WARN_FREQ_SHED_MHZ: f32 = 50.0;
const THERMAL_WARN_FAN_CAP_PCT: u8 = 70;

// ── HALT-10: always-on gentle proportional fan curve (home-quiet) ──
// With the default fan_target_temp_c == 0 the PID never runs, so without this
// the fan would sit at boot duty until 90 C then step. This gives a continuous
// ramp between a comfort floor and the WARNING band while mining, keeping the
// board quiet at steady state. It only ever RAISES the fan above a user's manual
// setting (never reduces it, never below the 20% floor) and the 90 C -> cap /
// 105 C -> cut backstops still apply. All thresholds are named consts.
const PROP_FAN_LOW_TEMP_C: f32 = 55.0;
const PROP_FAN_HIGH_TEMP_C: f32 = 85.0;
const PROP_FAN_MIN_PCT: u8 = 30;
const PROP_FAN_MAX_PCT: u8 = 70;
#[cfg(feature = "stratum-v2")]
const STRATUM_STATUS_EVENT_LIMIT: usize = 64;

fn unsafe_lab_safety_bypass_enabled() -> bool {
    // XPSAFE-4: the lab-safety bypass is COMPILE-TIME ONLY. The prior runtime
    // process-environment arm was dead on ESP32 (no process env at runtime) and
    // falsely implied a runtime toggle. Removing it makes the activation surface
    // honest and unambiguous: the bypass can ONLY be set by building with the
    // build-time env var. main() reads the result ONCE at boot into the local
    // `unsafe_lab_safety_bypass` and reuses it at every gate, so there is nothing
    // for a late env change to flip mid-run.
    option_env!("DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS") == Some("1")
}

// ══════════════════════════════════════════════════════════════════════════
// XPSAFE-1 — Fail-closed panic hook
// ══════════════════════════════════════════════════════════════════════════
//
// A panicked/wedged supervisor must not leave the ASIC energized while the
// ~hundreds-of-ms coredump-write + reboot runs (the task-WDT is only a ~15 s
// last-resort backstop). `install_fail_closed_panic_hook()` registers a
// `std::panic::set_hook` closure that, on ANY panic, cuts the buck rail and
// commands max cooling BEFORE the runtime aborts (the xtensa-esp32s3-espidf
// target defaults to panic=abort; see the workspace `Cargo.toml`
// `[profile.release]`). The closure is `Fn + Send + Sync + 'static` and captures
// no HAL handles, so the raw bits it needs are published through these atomics.
// Sentinels (-1 GPIO / 0 fan addr) keep the hook a SAFE NO-OP until the HAL arms
// it in two phases — buck right after the enable GPIO is built (still OFF) and
// fan inside each controller boot block — both well before `enable_buck(true)`.
static PANIC_BUCK_GPIO: AtomicI32 = AtomicI32::new(-1);
static PANIC_BUCK_ACTIVE_LOW: AtomicBool = AtomicBool::new(false);
static PANIC_FAN_PORT: AtomicI32 = AtomicI32::new(-1);
static PANIC_FAN_ADDR: AtomicU8 = AtomicU8::new(0);
static PANIC_FAN_REG0: AtomicU8 = AtomicU8::new(0);
static PANIC_FAN_REG1: AtomicU8 = AtomicU8::new(0);
static PANIC_FAN_REG_COUNT: AtomicU8 = AtomicU8::new(0);
static PANIC_FAN_VAL: AtomicU8 = AtomicU8::new(0);

/// Arm the panic hook's best-effort fan-cooling write for a confirmed fan
/// controller. `addr` is published LAST (it is the hook's "fan armed" gate) so
/// the hook never observes a half-written register set. `val` is always the
/// controller's full-scale duty (see `safety::fan_safe_panic_duty` /
/// `safety::emc2101_panic_duty`); the panic path never reduces a fan limit.
fn arm_panic_fan(port: i32, addr: u8, reg0: u8, reg1: u8, reg_count: u8, val: u8) {
    PANIC_FAN_PORT.store(port, Ordering::Release);
    PANIC_FAN_REG0.store(reg0, Ordering::Release);
    PANIC_FAN_REG1.store(reg1, Ordering::Release);
    PANIC_FAN_REG_COUNT.store(reg_count, Ordering::Release);
    PANIC_FAN_VAL.store(val, Ordering::Release);
    PANIC_FAN_ADDR.store(addr, Ordering::Release);
}

/// XPSAFE-1: install the fail-closed panic hook (see banner above).
///
/// The `Box` allocation happens HERE at install time, never inside the hook. The
/// hook body is ALLOC-FREE (no `format!`/`String`) per
/// . It deliberately does NOT call
/// `esp_restart()`: under panic=abort the runtime already reboots, and an
/// explicit restart here would SKIP the coredump that `/api/system/coredump`
/// depends on. The buck-cut (a lock-free atomic register write) is the
/// guaranteed-safe action and runs FIRST; the fan write is best-effort with a
/// short I2C timeout so a held driver mutex can never wedge the abort path.
fn install_fail_closed_panic_hook() {
    std::panic::set_hook(Box::new(|_panic_info| {
        // 1. LOAD-BEARING: cut the buck rail. `gpio_set_level` is a lock-free
        //    atomic register write — always safe from panic context.
        let buck_gpio = PANIC_BUCK_GPIO.load(Ordering::Acquire);
        if buck_gpio >= 0 {
            let active_low = PANIC_BUCK_ACTIVE_LOW.load(Ordering::Acquire);
            let off_level = dcentaxe_hal::safety::buck_off_level(active_low);
            unsafe {
                sys::gpio_set_level(buck_gpio, off_level);
            }
        }
        // 2. BEST-EFFORT: command max cooling. Short timeout (~20 ms at
        //    FREERTOS_HZ=1000) so a held I2C driver mutex cannot stall the abort.
        let fan_addr = PANIC_FAN_ADDR.load(Ordering::Acquire);
        if fan_addr != 0 {
            const PANIC_I2C_TICKS: u32 = 20;
            let port = PANIC_FAN_PORT.load(Ordering::Acquire);
            if port < 0 {
                return;
            }
            let port = port as u32;
            let val = PANIC_FAN_VAL.load(Ordering::Acquire);
            let reg_count = PANIC_FAN_REG_COUNT.load(Ordering::Acquire);
            if reg_count >= 1 {
                let buf = [PANIC_FAN_REG0.load(Ordering::Acquire), val];
                unsafe {
                    sys::i2c_master_write_to_device(
                        port,
                        fan_addr,
                        buf.as_ptr(),
                        buf.len(),
                        PANIC_I2C_TICKS,
                    );
                }
            }
            if reg_count >= 2 {
                let buf = [PANIC_FAN_REG1.load(Ordering::Acquire), val];
                unsafe {
                    sys::i2c_master_write_to_device(
                        port,
                        fan_addr,
                        buf.as_ptr(),
                        buf.len(),
                        PANIC_I2C_TICKS,
                    );
                }
            }
        }
    }));
}

fn fail_closed_power_off(
    reason: &str,
    state: &SharedState,
    mining_kill: Option<&Arc<AtomicBool>>,
    power_mgr: Option<&mut PowerManager>,
    i2c: &mut I2cBus,
    gpio_ctrl: &mut GpioController<'_>,
) {
    error!("FAIL-CLOSED POWER OFF: {}", reason);
    if let Some(kill) = mining_kill {
        kill.store(true, Ordering::Relaxed);
    }
    {
        let mut telem = state
            .telemetry
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        telem.mining_enabled = false;
        telem.pool_connected = false;
    }
    if let Some(power_mgr) = power_mgr {
        match power_mgr.disable(i2c) {
            Ok(()) => {}
            Err(PowerError::RequiresBuckCut(msg)) => {
                warn!("Regulator disable requires GPIO cut: {}", msg);
            }
            Err(e) => {
                error!(
                    "Regulator disable failed during fail-closed shutdown: {}",
                    e
                );
            }
        }
    }
    if let Err(e) = gpio_ctrl.enable_buck(false) {
        error!(
            "Buck GPIO disable failed during fail-closed shutdown: {}",
            e
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════
// PIXEL ART — 8x8 1-bit sprites stored as const byte arrays
// Each sprite is 8 bytes: one byte per column, LSB = top pixel.
// ══════════════════════════════════════════════════════════════════════════

/// Pickaxe swinging frame 1 (mining)
const SPRITE_PICKAXE_1: [u8; 8] = [
    0b00000001, 0b00000010, 0b00000100, 0b00001000, 0b00111000, 0b01001000, 0b10001000, 0b00001000,
];
/// Pickaxe swinging frame 2
const SPRITE_PICKAXE_2: [u8; 8] = [
    0b00000000, 0b00000001, 0b00000010, 0b00000100, 0b00011100, 0b00100100, 0b01000100, 0b00000100,
];
/// Bitcoin coin frame 1 (front)
const SPRITE_COIN_1: [u8; 8] = [
    0b00111100, 0b01000010, 0b10011001, 0b10100101, 0b10100101, 0b10011001, 0b01000010, 0b00111100,
];
/// Bitcoin coin frame 2 (turning)
const SPRITE_COIN_2: [u8; 8] = [
    0b00011000, 0b00100100, 0b01001001, 0b01010101, 0b01010101, 0b01001001, 0b00100100, 0b00011000,
];
/// Bitcoin coin frame 3 (edge)
const SPRITE_COIN_3: [u8; 8] = [
    0b00001000, 0b00010100, 0b00100010, 0b00101010, 0b00101010, 0b00100010, 0b00010100, 0b00001000,
];
/// Lightning bolt (share accepted flash)
const SPRITE_LIGHTNING: [u8; 8] = [
    0b00000000, 0b00001100, 0b00010100, 0b00111100, 0b00011110, 0b00010100, 0b00011000, 0b00000000,
];
/// Campfire (space heater vibes)
const SPRITE_CAMPFIRE: [u8; 8] = [
    0b00000000, 0b00010000, 0b00101000, 0b00010000, 0b01111110, 0b11111111, 0b01111110, 0b00111100,
];
/// Rocket (milestone/achievement)
const SPRITE_ROCKET: [u8; 8] = [
    0b00010000, 0b00111000, 0b01111100, 0b01111100, 0b01111100, 0b00111000, 0b01000100, 0b10000010,
];

// ══════════════════════════════════════════════════════════════════════════
// 16x16 CREATURE SPRITES — Full-screen companion page
// Column-major 1-bit, 16 columns x 2 pages (16 bytes per page = 32 bytes)
// ══════════════════════════════════════════════════════════════════════════

/// Happy miner creature — wide eyes, smile, pickaxe hat (16x16, eyes open)
const CREATURE_HAPPY: [u8; 32] = [
    0x00, 0x00, 0x1C, 0x22, 0x49, 0x45, 0x41, 0x45, 0x49, 0x22, 0x1C, 0x00, 0x00, 0x00, 0x00,
    0x00, // top 8 rows
    0x00, 0x3C, 0x42, 0x81, 0xA5, 0x81, 0x81, 0x42, 0x3C, 0x18, 0x24, 0x24, 0x18, 0x00, 0x00,
    0x00, // bottom 8 rows (body+legs)
];

/// Happy miner blink frame — eyes closed (16x16)
const CREATURE_BLINK: [u8; 32] = [
    0x00, 0x00, 0x1C, 0x22, 0x41, 0x4D, 0x41, 0x4D, 0x41, 0x22, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x3C, 0x42, 0x81, 0xA5, 0x81, 0x81, 0x42, 0x3C, 0x18, 0x24, 0x24, 0x18, 0x00, 0x00, 0x00,
];

/// Sad creature — droopy eyes, frown (16x16)
const CREATURE_SAD: [u8; 32] = [
    0x00, 0x00, 0x1C, 0x22, 0x41, 0x49, 0x41, 0x49, 0x41, 0x22, 0x1C, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x3C, 0x42, 0x81, 0x99, 0x81, 0xA5, 0x42, 0x3C, 0x18, 0x24, 0x24, 0x18, 0x00, 0x00, 0x00,
];

/// Sleeping creature — closed eyes, Zzz (16x16)
const CREATURE_SLEEP: [u8; 32] = [
    0x00, 0x00, 0x1C, 0x22, 0x41, 0x4D, 0x41, 0x4D, 0x41, 0x22, 0x1C, 0x0E, 0x04, 0x0E, 0x02,
    0x06, // Zzz pattern top-right
    0x00, 0x3C, 0x42, 0x81, 0x81, 0x81, 0x81, 0x42, 0x3C, 0x18, 0x18, 0x18, 0x18, 0x00, 0x00,
    0x00, // legs together (resting)
];

/// Heart filled (5x7 column-major)
const HEART_FILLED: [u8; 5] = [0x0C, 0x1E, 0x3C, 0x1E, 0x0C];
/// Heart empty (5x7 column-major)
const HEART_EMPTY: [u8; 5] = [0x0C, 0x12, 0x24, 0x12, 0x0C];

/// Brightness breathing LUT (6 steps, hardware contrast register)
const BREATH_LUT: [u8; 6] = [0x60, 0x90, 0xCF, 0xFF, 0xCF, 0x90];

// ══════════════════════════════════════════════════════════════════════════
// HASHRATE FUN COMPARISONS
// ══════════════════════════════════════════════════════════════════════════
const HASHRATE_COMPARISONS: &[&str] = &[
    "Game Boys mining!",
    "x Satoshi's 2009 HR",
    "hashes per eye blink",
    "of network. YOU MATTER",
    "billion SHA256/heartbt",
    "Nintendo 64s hashing!",
    "dial-up modems worth!",
    "pocket calculators!",
];

// ══════════════════════════════════════════════════════════════════════════
// MINING FORTUNE COOKIES
// ══════════════════════════════════════════════════════════════════════════
const FORTUNE_COOKIES: &[&str] = &[
    "A golden nonce awaits",
    "Trust the math. Mine.",
    "Lucky: 21, 2100, 210k",
    "Diff drop incoming...",
    "Your hash, your rules!",
    "Patience mines blocks.",
    "The chain remembers.",
    "Tick tock, next block.",
];

/// Check if a block height is "special" (palindrome, round, repeating, sequential).
fn is_special_block(h: u32) -> Option<&'static str> {
    if h == 0 {
        return None;
    }
    let s = format!("{}", h);
    let bytes = s.as_bytes();

    // Palindrome check
    let is_palindrome = bytes.iter().zip(bytes.iter().rev()).all(|(a, b)| a == b);
    if is_palindrome && s.len() >= 5 {
        return Some("PALINDROME!");
    }

    // Round number (divisible by 100000)
    if h % 1_000_000 == 0 {
        return Some("MILLION BLOCK!");
    }
    if h % 100_000 == 0 {
        return Some("ROUND BLOCK!");
    }

    // Repeating digits (all same)
    if s.len() >= 5 && bytes.iter().all(|&b| b == bytes[0]) {
        return Some("JACKPOT!");
    }

    // Sequential ascending (123456)
    if s.len() >= 5 {
        let mut is_seq = true;
        for i in 1..bytes.len() {
            if bytes[i] != bytes[i - 1] + 1 {
                is_seq = false;
                break;
            }
        }
        if is_seq {
            return Some("SEQUENCE BLOCK!");
        }
    }

    None
}

fn expected_hashrate_ghs(
    asic_model: dcentaxe_asic::AsicModel,
    frequency_mhz: f32,
    asic_count: u8,
) -> f32 {
    let small_cores_per_chip = match asic_model {
        dcentaxe_asic::AsicModel::BM1397 => 672,
        dcentaxe_asic::AsicModel::BM1366 => 894,
        dcentaxe_asic::AsicModel::BM1368 => 1276,
        dcentaxe_asic::AsicModel::BM1370 | dcentaxe_asic::AsicModel::BM1373 => 2040,
    };
    frequency_mhz * small_cores_per_chip as f32 * asic_count.max(1) as f32 / 1000.0
}

#[cfg(feature = "stratum-v2")]
fn push_stratum_status_event(
    status: &dcentaxe_stratum::SharedStratumStatus,
    kind: dcentaxe_stratum::StratumEventKind,
    detail: impl Into<String>,
) {
    let now = shared::unix_time_ms();
    if let Ok(mut status) = status.lock() {
        status
            .recent_events
            .push(dcentaxe_stratum::StratumEventRecord {
                ts_unix_ms: now,
                kind,
                detail: detail.into(),
            });
        if status.recent_events.len() > STRATUM_STATUS_EVENT_LIMIT {
            status.recent_events.remove(0);
        }
    }
}

#[cfg(feature = "stratum-v2")]
fn fill_sv2_rng_seed() -> [u8; 64] {
    let mut rng_seed = [0u8; 64];
    for chunk in rng_seed.chunks_exact_mut(4) {
        let r = unsafe { sys::esp_random() };
        chunk.copy_from_slice(&r.to_le_bytes());
    }
    rng_seed
}

#[cfg(feature = "stratum-v2")]
#[derive(Debug, Clone)]
struct Sv2PendingSubmit {
    sequence_number: u32,
    difficulty: f64,
    submitted_at: Instant,
}

#[cfg(feature = "stratum-v2")]
fn parse_sv2_job_id(job_id: &str) -> Result<u32, String> {
    job_id
        .parse::<u32>()
        .or_else(|_| u32::from_str_radix(job_id.trim_start_matches("0x"), 16))
        .map_err(|e| format!("invalid SV2 job_id {}: {}", job_id, e))
}

#[cfg(feature = "stratum-v2")]
fn parse_hex_u32(name: &str, value: &str) -> Result<u32, String> {
    u32::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|e| format!("invalid {} {}: {}", name, value, e))
}

#[cfg(feature = "stratum-v2")]
fn update_sv2_pending_status(
    status: &dcentaxe_stratum::SharedStratumStatus,
    pending: &[Sv2PendingSubmit],
) {
    let oldest_pending_submit_age_ms = pending
        .iter()
        .map(|submit| submit.submitted_at.elapsed().as_millis() as u64)
        .max()
        .unwrap_or(0);
    if let Ok(mut status) = status.lock() {
        status.shares_pending = pending.len() as u32;
        status.oldest_pending_submit_age_ms = oldest_pending_submit_age_ms;
    }
}

#[cfg(feature = "stratum-v2")]
fn record_sv2_share_accepted(
    status: &dcentaxe_stratum::SharedStratumStatus,
    pending: &mut Vec<Sv2PendingSubmit>,
    sequence_number: u32,
    accepted_count: u32,
) {
    let mut accepted = Vec::new();
    let mut remaining = Vec::with_capacity(pending.len());
    let max_accept = accepted_count.max(1) as usize;
    for submit in pending.drain(..) {
        if submit.sequence_number <= sequence_number && accepted.len() < max_accept {
            accepted.push(submit);
        } else {
            remaining.push(submit);
        }
    }
    *pending = remaining;
    if accepted.is_empty() {
        update_sv2_pending_status(status, pending);
        return;
    }
    let now_ms = shared::unix_time_ms();
    let accepted_len = accepted.len() as u64;
    let difficulty_sum: f64 = accepted.iter().map(|submit| submit.difficulty).sum();
    let response_ms = accepted
        .iter()
        .map(|submit| submit.submitted_at.elapsed().as_millis() as f64)
        .fold(0.0, f64::max);
    if let Ok(mut status) = status.lock() {
        status.shares_accepted = status.shares_accepted.saturating_add(accepted_len);
        status.shares_pending = pending.len() as u32;
        status.difficulty_accepted += difficulty_sum;
        status.last_share_response_ms = response_ms;
        status.last_share_response_unix_ms = now_ms;
        status.last_share_accepted_unix_ms = now_ms;
    }
    push_stratum_status_event(
        status,
        dcentaxe_stratum::StratumEventKind::ShareAccepted,
        format!("seq={} count={}", sequence_number, accepted_len),
    );
    update_sv2_pending_status(status, pending);
}

#[cfg(feature = "stratum-v2")]
fn record_sv2_share_rejected(
    status: &dcentaxe_stratum::SharedStratumStatus,
    pending: &mut Vec<Sv2PendingSubmit>,
    sequence_number: u32,
    reason: &str,
) {
    let mut rejected: Option<Sv2PendingSubmit> = None;
    pending.retain(|submit| {
        if submit.sequence_number == sequence_number && rejected.is_none() {
            rejected = Some(submit.clone());
            false
        } else {
            true
        }
    });
    let now_ms = shared::unix_time_ms();
    if let Ok(mut status) = status.lock() {
        status.shares_rejected = status.shares_rejected.saturating_add(1);
        status.shares_pending = pending.len() as u32;
        status.last_reject_reason = reason.to_string();
        status.last_share_response_unix_ms = now_ms;
        status.last_share_rejected_unix_ms = now_ms;
        if let Some(submit) = rejected {
            status.difficulty_rejected += submit.difficulty;
            status.last_share_response_ms = submit.submitted_at.elapsed().as_millis() as f64;
        }
    }
    push_stratum_status_event(
        status,
        dcentaxe_stratum::StratumEventKind::ShareRejected,
        format!("seq={} {}", sequence_number, reason),
    );
    update_sv2_pending_status(status, pending);
}

/// SV2-3: after this many CONSECUTIVE connect failures, fail over to the
/// configured fallback pool (if any) instead of retrying the same dead endpoint
/// forever. An SV2 pool/network outage previously meant total mining loss because
/// the V2 arm DROPPED the fallback and only ever retried the primary.
#[cfg(feature = "stratum-v2")]
const SV2_FAILOVER_AFTER_N: u32 = 5;

#[cfg(feature = "stratum-v2")]
fn run_sv2_client_thread(
    mut config: dcentaxe_stratum::StratumConfig,
    event_tx: mpsc::Sender<StratumEvent>,
    share_rx: mpsc::Receiver<MiningEvent>,
    status: dcentaxe_stratum::SharedStratumStatus,
    expected_hashrate_ghs: f32,
    sv2_authority_pubkey: Option<String>,
    fallback_pool: Option<dcentaxe_stratum::StratumConfig>,
) {
    use dcentaxe_stratum_v2::channel::Sv2Event;
    use dcentaxe_stratum_v2::client::{Sv2Client, Sv2Config};

    // SV2-3: track consecutive connect failures locally so we can fail over to the
    // fallback pool once they cross SV2_FAILOVER_AFTER_N. `failed_over` ensures we
    // only switch once (the fallback then gets its own retry/backoff).
    let mut sv2_consecutive_failures = 0u32;
    let mut failed_over = false;
    let mut backoff_secs = 1u64;
    loop {
        let host = config.endpoint_host();
        if let Ok(mut s) = status.lock() {
            s.configured_url = config.url.clone();
            s.configured_port = config.port;
            s.active_url = host.clone();
            s.active_port = config.port;
            s.connected = false;
            s.authorized = false;
            s.backoff_secs = backoff_secs;
        }

        let sv2_config = Sv2Config {
            host: host.clone(),
            port: config.port,
            worker: config.worker_name.clone(),
            hashrate_ghs: expected_hashrate_ghs.max(1.0),
            timeout_secs: 15,
            pool_authority_pubkey: sv2_authority_pubkey.clone(),
        };
        let mut client = Sv2Client::new(sv2_config);
        client.set_rng_seed(fill_sv2_rng_seed());

        match client.connect() {
            Ok(()) => {
                backoff_secs = 1;
                sv2_consecutive_failures = 0;
                if let Ok(mut s) = status.lock() {
                    s.consecutive_failures = 0;
                    s.backoff_secs = 0;
                    s.last_connect_cause = "sv2 noise handshake complete".to_string();
                    s.last_connect_unix_ms = shared::unix_time_ms();
                }
                push_stratum_status_event(
                    &status,
                    dcentaxe_stratum::StratumEventKind::Connect,
                    format!("sv2 noise handshake {}:{}", host, config.port),
                );
            }
            Err(e) => {
                warn!("SV2: connection failed: {}", e);
                let now_ms = shared::unix_time_ms();
                sv2_consecutive_failures = sv2_consecutive_failures.saturating_add(1);
                if let Ok(mut s) = status.lock() {
                    s.connected = false;
                    s.authorized = false;
                    s.consecutive_failures = s.consecutive_failures.saturating_add(1);
                    s.last_disconnect_cause = e.clone();
                    s.last_disconnect_unix_ms = now_ms;
                    s.backoff_secs = backoff_secs;
                }
                push_stratum_status_event(
                    &status,
                    dcentaxe_stratum::StratumEventKind::Disconnect,
                    e,
                );
                let _ = event_tx.send(StratumEvent::Disconnected);

                // SV2-3: an SV2 outage must not mean total mining loss. After
                // SV2_FAILOVER_AFTER_N consecutive failures, switch to the
                // configured fallback pool (once). If the fallback is itself SV2,
                // we keep this loop and retry the new endpoint; if it is V1, we hand
                // off to the V1 client path (matching the V1 arm in
                // spawn_stratum_thread) since this thread only speaks SV2.
                if !failed_over
                    && sv2_consecutive_failures >= SV2_FAILOVER_AFTER_N
                    && fallback_pool.is_some()
                {
                    let fb = fallback_pool.clone().expect("checked is_some");
                    failed_over = true;
                    warn!(
                        "SV2: {} consecutive failures — failing over to fallback pool {}:{}",
                        sv2_consecutive_failures,
                        crate::shared::sanitize_pool_url(&fb.url),
                        fb.port
                    );
                    if let Ok(mut s) = status.lock() {
                        s.last_disconnect_cause = "sv2 failover to fallback".to_string();
                        s.last_reconnect_cause = format!(
                            "sv2 failover to fallback {}:{}",
                            crate::shared::sanitize_pool_url(&fb.url),
                            fb.port
                        );
                    }
                    push_stratum_status_event(
                        &status,
                        dcentaxe_stratum::StratumEventKind::FailoverEntered,
                        format!(
                            "sv2 failover to fallback {}:{}",
                            crate::shared::sanitize_pool_url(&fb.url),
                            fb.port
                        ),
                    );
                    if fb.protocol() == StratumProtocol::V2 {
                        // Fallback is SV2 too — retry it with this loop's machinery.
                        config = fb;
                        backoff_secs = 1;
                        sv2_consecutive_failures = 0;
                        continue;
                    } else {
                        // Fallback is V1 — hand off to the V1 client path. `status`
                        // is an Arc, so clone the handle (cheap) to avoid moving it
                        // out of the retry loop; this path runs the V1 client to
                        // completion and returns.
                        let mut v1 = StratumClient::new(fb, event_tx, share_rx);
                        v1.set_status_handle(status.clone());
                        v1.run();
                        return;
                    }
                }

                std::thread::sleep(Duration::from_secs(backoff_secs));
                backoff_secs = (backoff_secs * 2).min(30);
                continue;
            }
        }

        let mut pending_submits: Vec<Sv2PendingSubmit> = Vec::new();
        let mut reconnect_config: Option<dcentaxe_stratum::StratumConfig> = None;

        'connected: loop {
            for event in client.poll() {
                match event {
                    Sv2Event::Connected => {
                        if let Ok(mut s) = status.lock() {
                            s.connected = true;
                            s.authorized = true;
                            s.last_reconnect_cause = "sv2 setup accepted".to_string();
                        }
                        push_stratum_status_event(
                            &status,
                            dcentaxe_stratum::StratumEventKind::PoolMessage,
                            "sv2 setup accepted; opening mining channel",
                        );
                        let _ = event_tx.send(StratumEvent::Reconnected);
                    }
                    Sv2Event::DifficultyChanged(diff) => {
                        let safe_diff = if diff.is_finite() && diff > 0.0 {
                            diff
                        } else {
                            1.0
                        };
                        if let Ok(mut s) = status.lock() {
                            s.difficulty = safe_diff;
                        }
                        push_stratum_status_event(
                            &status,
                            dcentaxe_stratum::StratumEventKind::DifficultyChanged,
                            format!("sv2 difficulty {:.4}", safe_diff),
                        );
                        let _ = event_tx.send(StratumEvent::DifficultyChanged(safe_diff));
                    }
                    Sv2Event::NewJob {
                        job_id,
                        version,
                        prev_hash,
                        merkle_root,
                        nbits,
                        ntime,
                        target,
                        clean_jobs,
                    } => {
                        let work = bridge::sv2_job_to_mining_work(
                            job_id,
                            version,
                            prev_hash,
                            merkle_root,
                            nbits,
                            ntime,
                            // SV2 standard channels carry no negotiated version_mask;
                            // bridge upgrades 0 -> the BIP320 canonical mask so BM1397
                            // rolls 4 midstates (ASICBoost) under SV2, matching V1.
                            0,
                            target,
                        );
                        if let Ok(mut s) = status.lock() {
                            s.connected = true;
                            s.authorized = true;
                            s.jobs_received = s.jobs_received.saturating_add(1);
                        }
                        let _ = event_tx.send(StratumEvent::PrebuiltWork { work, clean_jobs });
                    }
                    Sv2Event::ShareAccepted {
                        sequence_number,
                        accepted_count,
                    } => record_sv2_share_accepted(
                        &status,
                        &mut pending_submits,
                        sequence_number,
                        accepted_count,
                    ),
                    Sv2Event::ShareRejected {
                        sequence_number,
                        reason,
                    } => {
                        record_sv2_share_rejected(
                            &status,
                            &mut pending_submits,
                            sequence_number,
                            &reason,
                        );
                    }
                    Sv2Event::Disconnected(reason) => {
                        warn!("SV2: disconnected: {}", reason);
                        if let Ok(mut s) = status.lock() {
                            s.connected = false;
                            s.authorized = false;
                            s.last_disconnect_cause = reason.clone();
                            s.last_disconnect_unix_ms = shared::unix_time_ms();
                        }
                        push_stratum_status_event(
                            &status,
                            dcentaxe_stratum::StratumEventKind::Disconnect,
                            reason,
                        );
                        let _ = event_tx.send(StratumEvent::Disconnected);
                        break 'connected;
                    }
                    Sv2Event::Reconnect { host, port } => {
                        let mut next = config.clone();
                        if !host.trim().is_empty() {
                            next.url = host;
                        }
                        if port != 0 {
                            next.port = port;
                        }
                        reconnect_config = Some(next);
                        break 'connected;
                    }
                }
            }

            while let Ok(MiningEvent::SubmitShare(share)) = share_rx.try_recv() {
                let submit_result = parse_sv2_job_id(&share.job_id)
                    .and_then(|job_id| {
                        let nonce = parse_hex_u32("nonce", &share.nonce)?;
                        let ntime = parse_hex_u32("ntime", &share.ntime)?;
                        Ok((job_id, nonce, ntime))
                    })
                    .and_then(|(job_id, nonce, ntime)| {
                        client.submit_share(job_id, nonce, ntime, share.version)
                    });
                match submit_result {
                    Ok(sequence_number) => {
                        let now_ms = shared::unix_time_ms();
                        pending_submits.push(Sv2PendingSubmit {
                            sequence_number,
                            difficulty: share.difficulty,
                            submitted_at: Instant::now(),
                        });
                        if pending_submits.len() > 64 {
                            pending_submits.remove(0);
                        }
                        if let Ok(mut s) = status.lock() {
                            s.shares_submitted = s.shares_submitted.saturating_add(1);
                            s.shares_pending = pending_submits.len() as u32;
                            s.last_share_submit_unix_ms = now_ms;
                            s.last_share_difficulty = share.difficulty;
                        }
                        push_stratum_status_event(
                            &status,
                            dcentaxe_stratum::StratumEventKind::ShareSubmitted,
                            format!("seq={} diff={:.1}", sequence_number, share.difficulty),
                        );
                    }
                    Err(e) => {
                        warn!("SV2: failed to submit share: {}", e);
                        if let Ok(mut s) = status.lock() {
                            s.last_reject_reason = e.clone();
                        }
                    }
                }
            }

            update_sv2_pending_status(&status, &pending_submits);
            std::thread::sleep(Duration::from_millis(20));
        }

        client.disconnect();
        if let Some(next) = reconnect_config.take() {
            config = next;
        } else {
            std::thread::sleep(Duration::from_secs(backoff_secs));
            backoff_secs = (backoff_secs * 2).min(30);
        }
    }
}

#[cfg(not(feature = "stratum-v2"))]
fn run_sv2_client_thread(
    config: dcentaxe_stratum::StratumConfig,
    event_tx: mpsc::Sender<StratumEvent>,
    share_rx: mpsc::Receiver<MiningEvent>,
    status: dcentaxe_stratum::SharedStratumStatus,
    _expected_hashrate_ghs: f32,
    _sv2_authority_pubkey: Option<String>,
    fallback_pool: Option<dcentaxe_stratum::StratumConfig>,
) {
    error!(
        "Stratum V2 configured for {}:{}, but firmware was built without the stratum-v2 feature",
        crate::shared::sanitize_pool_url(&config.url),
        config.port
    );
    // H4/SV2-AVAIL: instead of sleeping forever (total mining loss with no
    // failover), fail OVER to a configured V1 fallback pool when one is set —
    // e.g. a unit reflashed from a V2 build whose persisted primary is still V2.
    // A V2 fallback can't be honored on this build either, so only a V1 fallback
    // is taken; this reuses the exact proven V1 StratumClient path.
    if let Some(fb) = fallback_pool {
        if fb.protocol() == StratumProtocol::V1 {
            warn!(
                "SV2 unavailable in this build — failing over to V1 fallback pool {}:{}",
                crate::shared::sanitize_pool_url(&fb.url),
                fb.port
            );
            if let Ok(mut s) = status.lock() {
                s.last_disconnect_cause = "stratum-v2 not compiled; using V1 fallback".to_string();
            }
            let mut client = StratumClient::new(fb, event_tx, share_rx);
            client.set_status_handle(status);
            client.run();
            return;
        }
        warn!("SV2 unavailable and configured fallback is also V2 — cannot fail over");
    }
    if let Ok(mut s) = status.lock() {
        s.connected = false;
        s.authorized = false;
        s.last_disconnect_cause = "stratum-v2 feature not compiled".to_string();
    }
    let _ = event_tx.send(StratumEvent::Disconnected);
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_stratum_thread(
    name: &str,
    config: dcentaxe_stratum::StratumConfig,
    fallback_pool: Option<dcentaxe_stratum::StratumConfig>,
    event_tx: mpsc::Sender<StratumEvent>,
    share_rx: mpsc::Receiver<MiningEvent>,
    status: dcentaxe_stratum::SharedStratumStatus,
    expected_hashrate_ghs: f32,
    sv2_authority_pubkey: Option<String>,
) -> std::thread::JoinHandle<()> {
    let protocol = config.protocol();
    let thread_name = name.to_string();
    std::thread::Builder::new()
        .name(thread_name)
        .stack_size(if protocol == StratumProtocol::V2 {
            32 * 1024
        } else {
            24 * 1024
        })
        .spawn(move || match protocol {
            StratumProtocol::V1 => {
                let mut client = StratumClient::new(config, event_tx, share_rx);
                client.set_status_handle(status);
                if let Some(fb) = fallback_pool {
                    client.set_fallback(fb.clone());
                    // B-ESP-10: sanitize the pool URL (strip any user:pass@) in logs.
                    info!(
                        "Stratum: fallback pool configured - {}:{}",
                        crate::shared::sanitize_pool_url(&fb.url),
                        fb.port
                    );
                }
                client.run();
            }
            StratumProtocol::V2 => {
                // SV2-3: thread the fallback pool through instead of dropping it.
                // run_sv2_client_thread now fails over to it after
                // SV2_FAILOVER_AFTER_N consecutive failures, so an SV2 outage no
                // longer means total mining loss.
                if let Some(ref fb) = fallback_pool {
                    info!(
                        "SV2: fallback pool configured for failover - {}:{}",
                        crate::shared::sanitize_pool_url(&fb.url),
                        fb.port
                    );
                }
                run_sv2_client_thread(
                    config,
                    event_tx,
                    share_rx,
                    status,
                    expected_hashrate_ghs,
                    sv2_authority_pubkey,
                    fallback_pool,
                );
            }
        })
        .expect("Stratum thread spawn failed")
}

/// Format a large number with K/M/G suffix to fit on 21-char OLED lines.
fn fmt_num(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{}G", n / 1_000_000_000)
    } else if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 10_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{}", n)
    }
}

/// Compact GH/s display for 21-character OLED rows.
fn fmt_hashrate(hr_ghs: f64) -> String {
    if hr_ghs >= 1000.0 {
        format!("{:.2}T", hr_ghs / 1000.0)
    } else if hr_ghs >= 100.0 {
        format!("{:.0}G", hr_ghs)
    } else if hr_ghs >= 10.0 {
        format!("{:.1}G", hr_ghs)
    } else {
        format!("{:.2}G", hr_ghs)
    }
}

fn main() {
    // ======================================================================
    // Step 1: ESP-IDF init
    // ======================================================================
    sys::link_patches();
    EspLogger::initialize_default();

    // XPSAFE-1: register the fail-closed panic hook BEFORE any rail is enabled.
    // Until the HAL arms it (buck GPIO + fan controller, in Step 6), the hook is
    // a safe no-op; arming completes well before `enable_buck(true)`.
    install_fail_closed_panic_hook();

    let reset_reason = unsafe { sys::esp_reset_reason() };
    info!("========================================");
    info!("  DCENT_axe v{} — First Hash Edition", VERSION);
    info!("  D-Central Technologies");
    info!(
        "  Reset reason: {} (code {})",
        match reset_reason {
            sys::esp_reset_reason_t_ESP_RST_POWERON => "Power-on",
            sys::esp_reset_reason_t_ESP_RST_SW => "Software",
            sys::esp_reset_reason_t_ESP_RST_PANIC => "Panic",
            sys::esp_reset_reason_t_ESP_RST_INT_WDT => "Interrupt watchdog",
            sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "Task watchdog",
            sys::esp_reset_reason_t_ESP_RST_WDT => "Other watchdog",
            sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "Deep sleep",
            sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "Brownout",
            sys::esp_reset_reason_t_ESP_RST_SDIO => "SDIO",
            _ => "Unknown",
        },
        reset_reason
    );
    info!("========================================");

    // Surface any stored panic coredump (ESP-IDF coredump partition) so the
    // user/dev knows to retrieve it via /api/system/coredump before a fresh
    // panic overwrites it.
    {
        let mut out_addr: usize = 0;
        let mut out_size: usize = 0;
        let rc = unsafe { sys::esp_core_dump_image_get(&mut out_addr, &mut out_size) };
        if rc == 0 && out_size > 0 {
            warn!(
                "Panic coredump present: {} bytes @ 0x{:08x} — retrieve via GET /api/system/coredump?download=1",
                out_size, out_addr
            );
        }
    }

    // ======================================================================
    // Step 2: NVS + peripherals
    // ======================================================================
    let nvs_partition =
        esp_idf_svc::nvs::EspDefaultNvsPartition::take().expect("Failed to initialize NVS flash");

    // Task-WDT safe-mode recovery.
    // If the device has rebooted ≥3 times from `ESP_RST_TASK_WDT` inside a
    // 300 s window, we refuse to start mining on the next boot and surface
    // the device in "safe mode" so the user can reach the dashboard / API
    // without the wedged code path tripping the WDT again. Cleared by
    // `POST /api/system/clear-safe-mode` or by any successful mining loop.
    const WDT_WINDOW_MS: u64 = 300_000;
    const WDT_SAFE_MODE_THRESHOLD: u8 = 3;
    let (mut wdt_count, mut wdt_since_ms) = (0u8, 0u64);
    let mut safe_mode = false;
    {
        let p = nvs_partition.clone();
        if let Ok(mut nvs) =
            esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(p, "dcentaxe", true)
        {
            let (count, since) = nvs_config::load_wdt_counters(&nvs);
            let now_ms = shared::unix_time_ms();
            if reset_reason == sys::esp_reset_reason_t_ESP_RST_TASK_WDT
                || reset_reason == sys::esp_reset_reason_t_ESP_RST_INT_WDT
            {
                if since == 0 || now_ms.saturating_sub(since) > WDT_WINDOW_MS {
                    // New window.
                    wdt_count = 1;
                    wdt_since_ms = now_ms;
                } else {
                    wdt_count = count.saturating_add(1);
                    wdt_since_ms = since;
                }
                warn!(
                    "Task-WDT reset detected: count={} in {} ms window",
                    wdt_count,
                    now_ms.saturating_sub(wdt_since_ms)
                );
                nvs_config::save_wdt_counters(&mut nvs, wdt_count, wdt_since_ms);
                if wdt_count >= WDT_SAFE_MODE_THRESHOLD {
                    safe_mode = true;
                    warn!(
                        "SAFE MODE: {} WDT resets in {} ms — mining suppressed. \
                         POST /api/system/clear-safe-mode to recover.",
                        wdt_count,
                        now_ms.saturating_sub(wdt_since_ms)
                    );
                }
            } else if reset_reason == sys::esp_reset_reason_t_ESP_RST_POWERON
                || reset_reason == sys::esp_reset_reason_t_ESP_RST_SW
            {
                // Clean boot — reset the window if it's stale.
                if since != 0 && now_ms.saturating_sub(since) > WDT_WINDOW_MS {
                    nvs_config::save_wdt_counters(&mut nvs, 0, 0);
                    wdt_count = 0;
                    wdt_since_ms = 0;
                } else {
                    wdt_count = count;
                    wdt_since_ms = since;
                }
            }
        }
    }

    let peripherals = Peripherals::take().expect("Failed to take peripherals");
    let sysloop = EspSystemEventLoop::take().expect("Failed to take event loop");

    // On-board SX1262 LoRa radio bus (DEFAULT-OFF). Acquire the dedicated SPI3/HSPI
    // host + the 9 control GPIOs (5/6/7/15/16/21/8 + the R-24 E22 RF-switch
    // enables TXEN=2/RXEN=9) up front, before the rest of main consumes
    // `peripherals` (none of the LoRa pins/SPI3 are touched by any other
    // subsystem — the provisional map is dedicated). Fail-soft: a bus-init
    // error leaves `lora_bus=None`, the task never spawns, mining is unaffected.
    // ⚠️ Integration seam — NEEDS-VERIFY (esp-idf-hal 0.46) at wire-up; the pins are
    // PROVISIONAL (NEEDS-NETLIST-LOCK, doc 05 §1.3) except TXEN/RXEN, which the
    // dcent-axe-BM1397 schematic locks to GPIO2/GPIO9 (PREFAB review R-24).
    #[cfg(feature = "lora")]
    let lora_bus = match dcentaxe_hal::lora_pins::open_lora_bus(
        peripherals.spi3,
        peripherals.pins.gpio5.into(),
        peripherals.pins.gpio6.into(),
        peripherals.pins.gpio7.into(),
        peripherals.pins.gpio15.into(),
        peripherals.pins.gpio16.into(),
        peripherals.pins.gpio21.into(),
        peripherals.pins.gpio8.into(),
        Some(peripherals.pins.gpio2.into()),
        Some(peripherals.pins.gpio9.into()),
    ) {
        Ok(bus) => Some(bus),
        Err(e) => {
            warn!("LoRa: SPI3 bus init failed ({e:?}) — mesh disabled, mining continues");
            None
        }
    };

    // ======================================================================
    // Step 3: Early I2C + display init
    // ======================================================================
    // I2C pins selected at compile time by board feature (pins-bitaxe vs pins-nerd)
    // I2C pins: BitAxe=47/48, Nerd (TTGO T-Display S3)=18/17
    #[cfg(feature = "pins-bitaxe")]
    let mut i2c = I2cBus::new_default(
        peripherals.i2c0,
        peripherals.pins.gpio47,
        peripherals.pins.gpio48,
    )
    .expect("I2C init failed");
    #[cfg(feature = "pins-nerd")]
    let mut i2c = I2cBus::new_default(
        peripherals.i2c0,
        peripherals.pins.gpio18,
        peripherals.pins.gpio17,
    )
    .expect("I2C init failed");

    // Display init (SSD1306 OLED for BitAxe, no-op stub for headless boards)
    let mut display = Ssd1306Display::new();
    if let Err(e) = display.init(&mut i2c) {
        warn!("OLED display not available: {} — continuing without it", e);
    } else {
        // ── Startup Animation: Mining creature wakes up! ──
        let boot_frames = [
            ("(-.-)zzZ", "Waking up..."),
            ("(o_o)", "Hmm?"),
            ("(^_^)", "Time to mine!"),
            ("[>_]#", "Let's hash!"),
        ];
        for (face, msg) in &boot_frames {
            display.show_status(
                &mut i2c,
                &format!("DCENT_axe v{}", VERSION),
                face,
                msg,
                "D-Central Tech.",
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        // Brief flash effect on boot
        display.invert_display(&mut i2c, true);
        std::thread::sleep(Duration::from_millis(80));
        display.invert_display(&mut i2c, false);

        display.show_status(
            &mut i2c,
            &format!("DCENT_axe v{}", VERSION),
            "D-Central Tech.",
            "",
            "Booting...",
        );
    }

    // ======================================================================
    // Step 4: Load config from NVS — or enter provisioning
    // ======================================================================
    let nvs_for_prov = nvs_partition.clone();
    let (saved_config, mut nvs_handle) = nvs_config::load_config(nvs_partition.clone());
    let saved_configured = saved_config
        .as_ref()
        .map(|cfg| cfg.is_configured())
        .unwrap_or(false);
    let force_setup_mode = nvs_handle.get_u8("force_setup").ok().flatten().unwrap_or(0) != 0;
    // Owner claim is a first-run gate — it forces a just-powered-on miner with
    // an empty NVS into captive-portal mode so a random passerby can't hit
    // "Save" on a device that has no password. Once the user has saved WiFi
    // credentials (saved_configured), the device is physically claimed and
    // the password stays genuinely optional.
    //
    // AOTA-4: `claim_skip` is a reserved opt-out flag that NO handler currently
    // writes (it always reads back 0). In particular /api/auth/owner-reset
    // deliberately preserves WiFi/pool config and does NOT force re-claim
    // (api.rs owner-reset clears only the password), so a reset device stays
    // passwordless-but-writable for ordinary settings. That is intentional and
    // safe because OTA flashing and the unsigned-OTA toggle are independently
    // fail-closed while passwordless: the OTA handler and shared-config gate go
    // through `ota_signature::ota_signature_enforced` /
    // `ota_signature::owner_action_authorized`, which never let an
    // unauthenticated caller waive signature verification or flip
    // `allow_unsigned_ota`. So a reset (passwordless) device cannot be coerced
    // into an unsigned-OTA RCE. The read below stays for forward compatibility
    // if a future handler ever sets the flag.
    let owner_claim_skipped = nvs_handle.get_u8("claim_skip").ok().flatten().unwrap_or(0) != 0;
    let owner_claim_required = !saved_configured
        && !crate::auth::password_is_set_in_nvs(&mut nvs_handle)
        && !owner_claim_skipped;

    let (mut config, mut nvs_handle): (DcentAxeConfig, _) = match saved_config {
        Some(cfg) if cfg.is_configured() && !force_setup_mode && !owner_claim_required => {
            info!(
                "NVS config loaded: SSID='{}', pool={}:{}",
                cfg.wifi_ssid,
                crate::shared::sanitize_pool_url(&cfg.stratum.url),
                cfg.stratum.port
            );
            (cfg, nvs_handle)
        }
        _ => {
            if force_setup_mode {
                info!("Forced setup mode requested — entering WiFi setup mode (AP captive portal)");
            } else if owner_claim_required && saved_configured {
                warn!("Saved config found without owner claim — entering setup mode to secure the miner");
            } else {
                info!("No saved config (or empty SSID) — entering WiFi setup mode (AP captive portal)");
            }
            display.show_status(
                &mut i2c,
                &format!("DCENT_axe v{}", VERSION),
                if force_setup_mode {
                    "Setup mode requested"
                } else if owner_claim_required && saved_configured {
                    "Owner claim required"
                } else {
                    "No config found"
                },
                "Starting setup...",
                "",
            );
            let config = provisioning::run_provisioning(
                peripherals.modem,
                sysloop.clone(),
                nvs_for_prov,
                nvs_handle,
                &mut display,
                &mut i2c,
            );
            // run_provisioning reboots after saving — unreachable, but just in case:
            unreachable!("Provisioning should have rebooted");
        }
    };

    config.canonicalize_identity();
    let unsafe_lab_safety_bypass = unsafe_lab_safety_bypass_enabled();
    if unsafe_lab_safety_bypass {
        warn!("UNSAFE LAB SAFETY BYPASS ENABLED: thermal/custom-board safety gates may be relaxed");
    }
    let config_safety_error = config.validate_safety(unsafe_lab_safety_bypass).err();
    if let Some(ref e) = config_safety_error {
        error!("Config safety validation failed: {}", e);
    }
    if let Err(e) = nvs_config::update_ota_floor_if_newer(&mut nvs_handle, VERSION) {
        warn!("Failed to seed OTA rollback floor: {}", e);
    }
    let qualified_boot_point = config.qualify_operating_point(
        config.target_frequency,
        config.target_voltage_mv,
        crate::config::ControlSurface::BootRestore,
    );
    if qualified_boot_point.clamped {
        warn!(
            "Boot config clamped to qualified envelope: {:.2}MHz/{}mV -> {:.2}MHz/{}mV",
            config.target_frequency,
            config.target_voltage_mv,
            qualified_boot_point.frequency_mhz,
            qualified_boot_point.voltage_mv
        );
        config.target_frequency = qualified_boot_point.frequency_mhz;
        config.target_voltage_mv = qualified_boot_point.voltage_mv;
        if let Err(e) = nvs_config::save_config(&mut nvs_handle, &config) {
            warn!("Failed to persist qualified boot config: {}", e);
        }
    }

    let board_config = config.board_config();
    let asic_model = config.asic_model();
    let asic_count = if config.asic_count > 0 {
        config.asic_count
    } else {
        config.expected_asic_count()
    };
    let power_limits = config.power_limits();

    // Hardware-relative Speed Demon threshold (~80% of rated peak GH/s)
    // Scale by ASIC count for multi-chip boards (Hex = 6x, GT = 2x, etc.)
    let speed_demon_threshold: f64 = {
        let per_chip = match asic_model {
            dcentaxe_asic::AsicModel::BM1370 | dcentaxe_asic::AsicModel::BM1373 => 1000.0,
            dcentaxe_asic::AsicModel::BM1368 => 500.0,
            dcentaxe_asic::AsicModel::BM1366 => 400.0,
            dcentaxe_asic::AsicModel::BM1397 => 300.0,
        };
        per_chip * asic_count as f64
    };

    // Apply display orientation from config
    if config.display_inverted {
        let _ = display.set_flip(&mut i2c, true);
    }

    if config.overclock_enabled {
        info!(
            "OVERCLOCK MODE ENABLED — max {:.0}W, {:.0}A, {:.0}MHz",
            power_limits.max_power_w, power_limits.max_current_a, power_limits.max_frequency
        );
    } else {
        info!(
            "Safe mode — max {:.0}W, {:.0}A, {:.0}MHz (5V/6A PSU)",
            power_limits.max_power_w, power_limits.max_current_a, power_limits.max_frequency
        );
    }

    // ======================================================================
    // Step 4: WiFi connect (with NVS-backed retry counter)
    // ======================================================================
    let wifi_retries = {
        let p = nvs_partition.clone();
        if let Ok(nvs) =
            esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(p, "dcentaxe", true)
        {
            nvs.get_u8("wifi_retries").ok().flatten().unwrap_or(0)
        } else {
            0
        }
    };

    display.show_status(
        &mut i2c,
        &format!("DCENT_axe v{}", VERSION),
        &format!("WiFi: {}", config.wifi_ssid),
        "Connecting...",
        "",
    );

    let wifi_handle = match wifi::connect_wifi(
        peripherals.modem,
        sysloop.clone(),
        nvs_partition.clone(),
        &config.wifi_ssid,
        &config.wifi_password,
    ) {
        Ok(wifi) => {
            info!("WiFi connected to '{}'", config.wifi_ssid);
            // Reset recovery flags on success.
            if wifi_retries > 0 || force_setup_mode {
                let p = nvs_partition.clone();
                if let Ok(mut nvs) = esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(
                    p, "dcentaxe", true,
                ) {
                    let _ = nvs.set_u8("wifi_retries", 0u8);
                    let _ = nvs.remove("force_setup");
                }
            }
            wifi
        }
        Err(e) => {
            error!("WiFi failed: {}", e);

            // Show failure reason on OLED before switching to recovery setup mode.
            let reason = if format!("{}", e).contains("timeout") {
                "Timeout"
            } else if format!("{}", e).contains("auth") || format!("{}", e).contains("password") {
                "Auth failed"
            } else {
                "Connection error"
            };
            display.show_status(
                &mut i2c,
                &format!("DCENT_axe v{}", VERSION),
                "WiFi failed",
                reason,
                "Starting setup mode",
            );

            let nvs_p = nvs_partition.clone();
            if let Ok(mut nvs) = esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(
                nvs_p, "dcentaxe", true,
            ) {
                let _ = nvs.set_u8("wifi_retries", wifi_retries.saturating_add(1));
                let _ = nvs.set_u8("force_setup", 1u8);
                info!("Marked forced setup mode for next boot");
            }

            std::thread::sleep(Duration::from_secs(2));
            unsafe {
                sys::esp_restart();
            }
            unreachable!();
        }
    };

    // Extract IP address from WiFi netif for OLED display and logging
    let device_ip: String = wifi_handle
        .sta_netif()
        .get_ip_info()
        .map(|info| format!("{}", info.ip))
        .unwrap_or_else(|_| "?.?.?.?".into());
    info!("Device IP: {}", device_ip);

    let _sntp = match esp_idf_svc::sntp::EspSntp::new_default() {
        Ok(service) => {
            info!("SNTP: started for wall-clock schedule support");
            Some(service)
        }
        Err(e) => {
            warn!("SNTP: unavailable ({:?}) — schedules will use uptime fallback until clock is valid", e);
            None
        }
    };

    display.show_status(
        &mut i2c,
        &format!("DCENT_axe v{}", VERSION),
        "WiFi connected!",
        &format!("IP: {}", device_ip),
        "Connecting to pool...",
    );

    // mDNS: requires esp-idf-sys to compile with mDNS component
    // TODO: Add ESP_IDF_COMPONENTS="mdns" to .cargo/config.toml [env] section
    {
        let hostname = if config.hostname.is_empty() {
            format!("dcentaxe-{}", board_config.device_model)
        } else {
            config.hostname.clone()
        };
        info!(
            "mDNS: {}.local (component not yet linked — access via IP: {})",
            hostname, device_ip
        );
    }
    // Allow OLED to display IP for 2 seconds so user can note it down
    std::thread::sleep(Duration::from_millis(2000));

    // ======================================================================
    // Step 5: Shared state (for API, dashboard, MCP, autotuner)
    // ======================================================================
    let state = SharedState::new(config.clone(), &board_config, nvs_handle);
    {
        let mut wifi = state.wifi.lock().unwrap_or_else(|e| e.into_inner());
        *wifi = Some(wifi_handle);
    }
    {
        let live_config = state.config.lock().unwrap_or_else(|e| e.into_inner());
        let local_id = {
            let mut mac = [0u8; 6];
            unsafe {
                esp_idf_svc::sys::esp_wifi_get_mac(
                    esp_idf_svc::sys::wifi_interface_t_WIFI_IF_STA,
                    mac.as_mut_ptr(),
                );
            }
            format!(
                "bitaxe-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            )
        };
        let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
        swarm.local.id = local_id;
        swarm.local.hostname = if live_config.hostname.is_empty() {
            format!("dcentaxe-{}", board_config.device_model)
        } else {
            live_config.hostname.clone()
        };
        swarm.local.display_name = board_config.model.name().to_string();
        swarm.local.board_model = board_config.device_model.clone();
        swarm.local.board_version = board_config.board_version.clone();
        swarm.local.board_target = board_config.model.board_target().to_string();
        swarm.local.asic_model = board_config.asic_model.clone();
        swarm.local.ip = device_ip.clone();
        swarm.discovery.mdns_hostname = Some(format!("{}.local", swarm.local.hostname));
        swarm.discovery.api_url = Some(format!("http://{}/api/swarm", device_ip));
        swarm.discovery.mcp_url = Some(format!("http://{}/mcp", device_ip));
        swarm.discovery.mdns_enabled = true;
        swarm.discovery.discovery_hint = "mDNS advertising _dcentaxe._tcp".to_string();
    }

    // Seed peers + queen_id from NVS so we rejoin the cluster quickly after
    // a reboot. The mDNS thread (spawned later) refreshes on its first tick.
    swarm::load_persisted(&state);
    // Kick the background swarm worker: mDNS advertise + query + election
    // + NVS persistence. Non-blocking; failures log but don't panic.
    swarm::start(state.clone());

    // Last-known pool difficulty (primes ASIC TicketMask at boot, ESP-Miner PR #1594).
    let mut cached_pool_difficulty: f64 = 0.0;
    // Load all-time best share difficulty + achievements + streak + lifetime shares from NVS
    let (
        mut achievements,
        mut best_streak_ever,
        mut lifetime_shares,
        mut best_nonce_val,
        mut best_nonce_diff,
    ) = {
        let nvs_for_load = nvs_partition.clone();
        if let Ok(nvs) = esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(
            nvs_for_load,
            "dcentaxe",
            true,
        ) {
            let best_diff_ever = nvs_config::load_best_diff(&nvs);
            if let Some(lkg) = nvs_config::load_last_known_good(&nvs) {
                let mut at = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
                at.last_good_frequency = lkg.frequency_mhz;
                at.last_good_voltage_mv = lkg.voltage_mv;
                at.last_good_jth = lkg.jth;
                at.last_good_error_rate = lkg.delta_error_rate;
                at.silicon_grade = if lkg.delta_error_rate <= 0.002 && lkg.jth <= 16.5 {
                    "gold"
                } else if lkg.delta_error_rate <= 0.01 && lkg.jth <= 18.5 {
                    "strong"
                } else if lkg.delta_error_rate <= 0.02 {
                    "normal"
                } else {
                    "spicy"
                }
                .to_string();
            }
            let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            telem.best_diff_ever = best_diff_ever;
            let ach = nvs_config::load_achievements(&nvs);
            let streak = nvs_config::load_best_streak(&nvs);
            let lt_shares = nvs_config::load_lifetime_shares(&nvs);
            let (bn_val, bn_diff) = nvs_config::load_best_nonce(&nvs);
            cached_pool_difficulty = nvs_config::load_cached_pool_difficulty(&nvs);
            let (stage_name, _) = nvs_config::evolution_stage(lt_shares);
            info!("NVS: achievements=0x{:08X} ({} unlocked), best_streak={}, lifetime_shares={} ({}), best_nonce=0x{:08X}, cached_diff={:.3}",
                ach, nvs_config::achievement_count(ach), streak, lt_shares, stage_name, bn_val, cached_pool_difficulty);
            (ach, streak, lt_shares, bn_val, bn_diff)
        } else {
            let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            telem.best_diff_ever = 0.0;
            (0u32, 0u32, 0u32, 0u32, 0.0f64)
        }
    };
    // Set reset reason + safe-mode + coredump presence in telemetry.
    {
        let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
        telem.reset_reason = match reset_reason {
            sys::esp_reset_reason_t_ESP_RST_POWERON => "Power-on",
            sys::esp_reset_reason_t_ESP_RST_SW => "Software",
            sys::esp_reset_reason_t_ESP_RST_PANIC => "Panic",
            sys::esp_reset_reason_t_ESP_RST_INT_WDT => "Interrupt watchdog",
            sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "Task watchdog",
            sys::esp_reset_reason_t_ESP_RST_WDT => "Other watchdog",
            sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "Deep sleep",
            sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "Brownout",
            sys::esp_reset_reason_t_ESP_RST_SDIO => "SDIO",
            _ => "Unknown",
        }
        .to_string();
        telem.safe_mode = safe_mode;
        telem.wdt_reset_count = wdt_count;
        let mut out_addr: usize = 0;
        let mut out_size: usize = 0;
        let rc = unsafe { sys::esp_core_dump_image_get(&mut out_addr, &mut out_size) };
        telem.coredump_present = rc == 0 && out_size > 0;
    }
    let shared_mining_stats = state.stats.clone();

    // ======================================================================
    // Step 6: HAL init
    // ======================================================================

    // GPIO — buck enable pin depends on board model
    let mut gpio_ctrl = match (
        board_config.asic_reset_pin,
        board_config.buck_enable_pin,
        board_config.led_pin,
    ) {
        (1, 10, 4) => GpioController::new(
            peripherals.pins.gpio1,
            peripherals.pins.gpio10,
            peripherals.pins.gpio4,
            board_config.buck_enable_active_low,
        ),
        (1, 46, 4) => GpioController::new(
            peripherals.pins.gpio1,
            peripherals.pins.gpio46,
            peripherals.pins.gpio4,
            board_config.buck_enable_active_low,
        ),
        pins => panic!("Unsupported board GPIO mapping: {:?}", pins),
    }
    .expect("GPIO init failed");

    // XPSAFE-1: arm the panic hook's buck-cut now that the enable GPIO is a
    // configured output. Buck is still OFF here, so a hook firing before
    // `enable_buck(true)` is a harmless no-op; from this point on, ANY panic
    // drives the rail OFF before the abort / coredump / reboot.
    PANIC_BUCK_GPIO.store(board_config.buck_enable_pin, Ordering::Release);
    PANIC_BUCK_ACTIVE_LOW.store(board_config.buck_enable_active_low, Ordering::Release);

    let mut mining_permitted = true;
    let mut mining_block_reason: Option<String> = None;

    if let Some(e) = config_safety_error.clone() {
        mining_permitted = false;
        mining_block_reason = Some(format!("Config safety validation failed: {}", e));
    }

    if safe_mode {
        mining_permitted = false;
        mining_block_reason = Some(format!(
            "SAFE MODE: {} task-WDT resets. POST /api/system/clear-safe-mode to recover.",
            wdt_count
        ));
    }

    let power_gate_permitted = if board_config.plug_sense {
        match board_config.plug_sense_pin {
            #[cfg(feature = "pins-bitaxe")]
            12 => match gpio::PinDriver::input(peripherals.pins.gpio12, gpio::Pull::Down) {
                Ok(plug_sense) => {
                    if plug_sense.is_high() {
                        info!("Plug sense HIGH on GPIO12 — external power present");
                        true
                    } else {
                        warn!("Plug sense LOW on GPIO12 — gating power {}", "closed");
                        false
                    }
                }
                Err(e) => {
                    warn!("Plug sense GPIO init failed: {:?}", e);
                    false
                }
            },
            _ => false,
        }
    } else {
        true
    };

    if !power_gate_permitted {
        mining_permitted = false;
        mining_block_reason = Some("Power input not detected by plug-sense gate".to_string());
    }

    // Temperature sensor + Fan controller
    // Hex boards: EMC2302 (0x2F) dual fan + TMP1075 for temp
    // Single-ASIC boards: EMC2101 (0x4C) fan+temp or EMC2103 (0x2E)
    let boot_fan_pct = config.fan_speed_pct.clamp(20, 100);
    let mut temp_emc = None;
    let mut fan_is_emc2103 = false;
    let mut emc2103: Option<Emc2103> = None;
    let mut fan_emc2302: Option<Emc2302> = None;

    match board_config.fan_controller {
        dcentaxe_hal::board::FanControllerKind::Emc2302 => match Emc2302::new_default(&mut i2c) {
            Ok(emc) => {
                let mut rpm1 = 0;
                let mut rpm2 = 0;
                if let Err(e) = emc.set_fan_speed(&mut i2c, boot_fan_pct) {
                    mining_permitted = false;
                    mining_block_reason = Some(format!("EMC2302 fan set failed: {}", e));
                    error!("EMC2302 fan set failed: {} - mining disabled", e);
                } else {
                    std::thread::sleep(Duration::from_millis(500));
                    for _ in 0..5 {
                        rpm1 = emc.get_fan1_rpm(&mut i2c).unwrap_or(0);
                        rpm2 = emc.get_fan2_rpm(&mut i2c).unwrap_or(0);
                        if rpm1 > 0 && rpm2 > 0 {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
                info!(
                    "Fan at {}% (via EMC2302 dual fan) — RPM: fan1={}, fan2={}",
                    boot_fan_pct, rpm1, rpm2
                );
                emc.dump_diagnostics(&mut i2c);
                if let Ok(status) = emc.read_status(&mut i2c) {
                    let fan1_connected = rpm1 > 0;
                    let fan2_connected = rpm2 > 0;
                    let has_real_fault = (fan1_connected
                        && (status.fan1_stall || status.fan1_spin_fail || status.fan1_drive_fail))
                        || (fan2_connected
                            && (status.fan2_stall
                                || status.fan2_spin_fail
                                || status.fan2_drive_fail));
                    if has_real_fault {
                        warn!(
                            "EMC2302 fan fault: stall={}/{}, spin={}/{}, drive={}/{}",
                            status.fan1_stall,
                            status.fan2_stall,
                            status.fan1_spin_fail,
                            status.fan2_spin_fail,
                            status.fan1_drive_fail,
                            status.fan2_drive_fail
                        );
                    }
                }
                if board_config.requires_fan_tach()
                    && !unsafe_lab_safety_bypass
                    && (rpm1 == 0 || rpm2 == 0)
                {
                    mining_permitted = false;
                    mining_block_reason = Some(format!(
                        "EMC2302 required dual tach absent at boot: fan1={}, fan2={}",
                        rpm1, rpm2
                    ));
                    error!(
                        "EMC2302 required dual tach absent at boot: fan1={}, fan2={} - mining disabled",
                        rpm1, rpm2
                    );
                }
                fan_emc2302 = Some(emc);
                // XPSAFE-1: arm panic-hook cooling. EMC2302 dual fan on I2C0,
                // FAN1_SETTING=0x30 + FAN2_SETTING=0x40 (emc2302.rs), full-scale
                // 8-bit duty.
                arm_panic_fan(
                    0,
                    dcentaxe_hal::emc2302::EMC2302_ADDR,
                    0x30,
                    0x40,
                    2,
                    dcentaxe_hal::safety::fan_safe_panic_duty(),
                );
            }
            Err(e) => {
                mining_permitted = false;
                mining_block_reason = Some(format!("Fan controller init failed: {}", e));
                error!(
                    "EMC2302 init failed: {} — mining disabled until fan control is available",
                    e
                );
            }
        },
        dcentaxe_hal::board::FanControllerKind::Emc2101 => match Emc2101::new(&mut i2c, 0x4C) {
            Ok(emc) => {
                if let Err(e) = emc.init_fan(&mut i2c) {
                    mining_permitted = false;
                    mining_block_reason = Some(format!("EMC2101 fan init failed: {}", e));
                    error!("EMC2101 fan init failed: {} — mining disabled", e);
                } else if let Err(e) = emc.set_fan_speed(&mut i2c, boot_fan_pct) {
                    mining_permitted = false;
                    mining_block_reason = Some(format!("EMC2101 fan set failed: {}", e));
                    error!("EMC2101 fan set failed: {} — mining disabled", e);
                } else {
                    if board_config.emc_ideality_factor != 0 {
                        if let Err(e) =
                            emc.set_ideality_factor(&mut i2c, board_config.emc_ideality_factor)
                        {
                            warn!("EMC2101 ideality configuration failed: {}", e);
                        }
                        if let Err(e) =
                            emc.set_beta_compensation(&mut i2c, board_config.emc_beta_compensation)
                        {
                            warn!("EMC2101 beta compensation failed: {}", e);
                        }
                    }
                    // HALT-6 / XPSAFE-7: boot-time tach proof for opted-in EMC2101
                    // single-fan boards. The EMC2101 boot path previously had NO
                    // RPM readback (unlike the EMC2103/EMC2302 paths). When the
                    // operator has opted in (fan_tach_present=true ->
                    // tach_proof_required()), require a >0 RPM at boot so a
                    // never-spinning fan fails closed; genuinely tachless boards
                    // keep the lenient heuristic (no boot assert) and surface
                    // "no fan proof (heuristic only)" telemetry below.
                    if board_config.tach_proof_required()
                        && !unsafe_lab_safety_bypass
                        && boot_fan_pct > 0
                    {
                        let mut boot_rpm = 0u32;
                        std::thread::sleep(Duration::from_millis(350));
                        for _ in 0..3 {
                            boot_rpm = emc.read_fan_rpm(&mut i2c).unwrap_or(0);
                            if boot_rpm > 0 {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(150));
                        }
                        if boot_rpm == 0 {
                            mining_permitted = false;
                            mining_block_reason =
                                Some("EMC2101 required tach absent at boot".to_string());
                            error!("EMC2101 tach stayed at 0 RPM during boot — mining disabled");
                        } else {
                            info!(
                                "EMC2101 boot tach proof OK ({} RPM at {}%)",
                                boot_rpm, boot_fan_pct
                            );
                        }
                    } else if !board_config.tach_proof_required() {
                        // No tach proof available/required on this board — the fan
                        // STALL kill is heuristic-only and the thermal ladder is the
                        // backstop. Surfaced in telemetry/self-test below.
                        info!(
                            "EMC2101 fan: no fan proof (heuristic only); thermal ladder is the backstop"
                        );
                    }
                    info!("Fan at {}% (via EMC2101)", boot_fan_pct);
                    temp_emc = Some(emc);
                    // XPSAFE-1: arm panic-hook cooling. EMC2101 on I2C0,
                    // FAN_SETTING=0x4C (temp.rs), 6-bit full-scale duty (63).
                    arm_panic_fan(
                        0,
                        dcentaxe_hal::temp::EMC2101_ADDR,
                        0x4C,
                        0,
                        1,
                        dcentaxe_hal::safety::emc2101_panic_duty(),
                    );
                }
            }
            Err(e) => {
                mining_permitted = false;
                mining_block_reason =
                    Some(format!("Expected EMC2101 fan controller missing: {}", e));
                error!(
                    "Expected EMC2101 fan controller missing: {} — mining disabled",
                    e
                );
            }
        },
        dcentaxe_hal::board::FanControllerKind::Emc2103 => {
            if i2c.probe(0x2E) {
                info!("EMC2103 detected at 0x2E — GT/Gamma Turbo fan controller");
                let duty = (255u16 * boot_fan_pct as u16 / 100) as u8;
                let mut driver = Emc2103::new(0x2E, board_config.temp_flip);
                match driver.init(
                    &mut i2c,
                    duty,
                    board_config.emc_ideality_factor,
                    board_config.emc_beta_compensation,
                ) {
                    Ok(()) => {
                        let mut boot_rpm = 0;
                        if boot_fan_pct > 0 {
                            std::thread::sleep(Duration::from_millis(350));
                            for _ in 0..3 {
                                boot_rpm = driver.read_rpm(&mut i2c);
                                if boot_rpm > 0 {
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(150));
                            }
                        }
                        if boot_fan_pct > 0 && boot_rpm == 0 {
                            mining_permitted = false;
                            mining_block_reason = Some("GT fan tach absent at boot".to_string());
                            error!("EMC2103 tach stayed at 0 RPM during boot — mining disabled");
                        } else {
                            info!(
                                "Fan at {}% (via EMC2103, duty={}, rpm={})",
                                boot_fan_pct, duty, boot_rpm
                            );
                            fan_is_emc2103 = true;
                            emc2103 = Some(driver);
                            // XPSAFE-1: arm panic-hook cooling. EMC2103 (GT) on
                            // I2C0 at 0x2E, FAN_SETTING=0x40 (emc2103.rs),
                            // full-scale 8-bit duty.
                            arm_panic_fan(
                                0,
                                0x2E,
                                dcentaxe_hal::emc2103::decode::REG_FAN_SETTING,
                                0,
                                1,
                                dcentaxe_hal::safety::fan_safe_panic_duty(),
                            );
                        }
                    }
                    Err(_) => {
                        mining_permitted = false;
                        mining_block_reason =
                            Some("EMC2103 fan controller init failed".to_string());
                        error!("EMC2103 init failed — mining disabled");
                    }
                }
            } else {
                mining_permitted = false;
                mining_block_reason = Some("Expected EMC2103 fan controller missing".to_string());
                error!("Expected EMC2103 fan controller missing — mining disabled");
            }
        }
        dcentaxe_hal::board::FanControllerKind::None => {}
    }

    // Fan state tracker + hardware write helper
    struct FanState {
        speed_pct: u8,
        rpm: u32,
    }
    let mut fan_ctrl = FanState {
        speed_pct: boot_fan_pct,
        rpm: 0,
    };
    // PID + EMA fan controller (ESP-Miner PR #1640 parity — see dcentaxe-hal::fan_pid).
    let mut fan_pid = dcentaxe_hal::fan_pid::FanPid::default();
    impl FanState {
        fn current_speed(&self) -> u8 {
            self.speed_pct
        }
        fn last_rpm(&self) -> u32 {
            self.rpm
        }
        fn set_speed(&mut self, pct: u8) -> Result<(), ()> {
            // Safety floor: never go below 20% while mining (fan_ever_seen guard
            // handles stall detection, but 0% fan while hashing = thermal runaway)
            self.speed_pct = pct.max(20).min(100);
            Ok(())
        }
    }
    /// Write fan speed to the actual hardware (EMC2302, EMC2101, or EMC2103).
    /// Call after fan_ctrl.set_speed() to apply the change.
    fn apply_fan_speed(
        i2c: &mut I2cBus,
        pct: u8,
        emc2302: &Option<Emc2302>,
        emc2101: &Option<Emc2101>,
        emc2103: &Option<Emc2103>,
    ) -> Result<(), String> {
        if let Some(ref emc) = emc2302 {
            emc.set_fan_speed(i2c, pct)
                .map_err(|e| format!("EMC2302 fan set failed: {}", e))?;
        } else if let Some(ref emc) = emc2101 {
            emc.set_fan_speed(i2c, pct)
                .map_err(|e| format!("EMC2101 fan set failed: {}", e))?;
        } else if let Some(ref emc) = emc2103 {
            emc.set_fan_speed(i2c, pct)
                .map_err(|e| format!("EMC2103 fan set failed: {}", e))?;
        }
        Ok(())
    }

    fn apply_fan_speed_or_fail_closed(
        i2c: &mut I2cBus,
        pct: u8,
        emc2302: &Option<Emc2302>,
        emc2101: &Option<Emc2101>,
        emc2103: &Option<Emc2103>,
        runtime_mining_active: bool,
        state: &SharedState,
        mining_kill: &Arc<AtomicBool>,
        power_mgr: &mut PowerManager,
        gpio_ctrl: &mut GpioController<'_>,
        reason: &str,
    ) -> bool {
        match apply_fan_speed(i2c, pct, emc2302, emc2101, emc2103) {
            Ok(()) => true,
            Err(e) => {
                error!("{}: {} - fan command not confirmed", reason, e);
                if runtime_mining_active {
                    fail_closed_power_off(
                        &format!("{} fan write failed: {}", reason, e),
                        state,
                        Some(mining_kill),
                        Some(power_mgr),
                        i2c,
                        gpio_ctrl,
                    );
                }
                false
            }
        }
    }

    fn apply_automatic_operating_point(
        state: &SharedState,
        power_mgr: &mut PowerManager,
        i2c: &mut I2cBus,
        freq_cmd_tx: &mpsc::Sender<f32>,
        last_applied_voltage: &mut u16,
        last_applied_freq: &mut f32,
        requested_frequency: f32,
        requested_voltage: u16,
        surface: ControlSurface,
        reason: &str,
    ) {
        let qualified = {
            let mut cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            let qualified =
                cfg.qualify_operating_point(requested_frequency, requested_voltage, surface);
            if qualified.clamped {
                warn!(
                    "{} clamped operating point {:.0}MHz/{}mV -> {:.0}MHz/{}mV",
                    reason,
                    requested_frequency,
                    requested_voltage,
                    qualified.frequency_mhz,
                    qualified.voltage_mv
                );
            }
            cfg.target_frequency = qualified.frequency_mhz;
            cfg.target_voltage_mv = qualified.voltage_mv;
            qualified
        };

        let mut voltage_apply_ok = true;
        if qualified.voltage_mv != *last_applied_voltage {
            match power_mgr.set_voltage(i2c, qualified.voltage_mv) {
                Ok(()) => *last_applied_voltage = qualified.voltage_mv,
                Err(e) => {
                    voltage_apply_ok = false;
                    error!("{} voltage apply failed: {}", reason, e);
                }
            }
        }
        if (qualified.frequency_mhz - *last_applied_freq).abs() > 0.1 {
            let raises_frequency = qualified.frequency_mhz > *last_applied_freq;
            if raises_frequency && !voltage_apply_ok {
                warn!(
                    "{} frequency increase to {:.0} MHz skipped after voltage apply failure",
                    reason, qualified.frequency_mhz
                );
            } else {
                match freq_cmd_tx.send(qualified.frequency_mhz) {
                    Ok(()) => *last_applied_freq = qualified.frequency_mhz,
                    Err(e) => error!("{} frequency apply failed: {}", reason, e),
                }
            }
        }
    }

    if mining_permitted {
        if let Err(e) = gpio_ctrl.enable_buck(true) {
            mining_permitted = false;
            mining_block_reason = Some(format!("Buck enable failed: {}", e));
            fail_closed_power_off(
                "Buck enable failed",
                &state,
                None,
                None,
                &mut i2c,
                &mut gpio_ctrl,
            );
        }
    }

    // I2C already initialized in Step 3 (for OLED display)

    // Power — boards with fixed voltage (NerdNOS) skip regulator init
    // Retry up to 3 times with I2C recovery delay (rapid reboots can leave bus in bad state)
    let mut power_mgr = if mining_permitted {
        let mut result = None;
        for attempt in 1..=3 {
            match PowerManager::new(&mut i2c, &board_config) {
                Ok(mgr) => {
                    result = Some(mgr);
                    break;
                }
                Err(e) => {
                    if attempt < 3 {
                        warn!("PowerManager init attempt {}/3 failed: {:?} — retrying after I2C recovery", attempt, e);
                        std::thread::sleep(Duration::from_millis(500));
                        // HALPWR-8: probe the board-appropriate regulator address as
                        // the bus-recovery poke. The old hardcoded 0x24 only matches
                        // TPS546 boards; on a DS4432U (0x48) board it probed an absent
                        // device so only the sleeps helped recovery.
                        let recovery_addr = match board_config.power_controller {
                            dcentaxe_hal::board::PowerControllerKind::Tps546 => {
                                dcentaxe_hal::power::TPS546_ADDR
                            }
                            dcentaxe_hal::board::PowerControllerKind::Ds4432u => {
                                dcentaxe_hal::power::DS4432U_ADDR
                            }
                            _ => dcentaxe_hal::power::TPS546_ADDR,
                        };
                        let _ = i2c.probe(recovery_addr);
                        std::thread::sleep(Duration::from_millis(100));
                    } else if board_config.model.has_voltage_control() {
                        mining_permitted = false;
                        mining_block_reason = Some(format!("Power init failed: {:?}", e));
                        error!(
                            "PowerManager init failed after 3 attempts: {:?} — mining disabled",
                            e
                        );
                        fail_closed_power_off(
                            "Power init failed after rails may have been enabled",
                            &state,
                            None,
                            None,
                            &mut i2c,
                            &mut gpio_ctrl,
                        );
                        result = Some(PowerManager::null());
                    } else {
                        warn!(
                            "No voltage regulator found — board has fixed voltage ({} mV)",
                            board_config.default_voltage_mv
                        );
                        result = Some(PowerManager::null());
                    }
                }
            }
        }
        result.unwrap_or_else(PowerManager::null)
    } else {
        PowerManager::null()
    };
    if mining_permitted && board_config.model.has_voltage_control() {
        if let Err(e) = power_mgr.set_voltage(&mut i2c, config.target_voltage_mv) {
            mining_permitted = false;
            mining_block_reason = Some(format!("Set voltage failed: {}", e));
            error!("Set voltage failed: {} — mining disabled", e);
            fail_closed_power_off(
                "Set voltage failed after rail enable",
                &state,
                None,
                Some(&mut power_mgr),
                &mut i2c,
                &mut gpio_ctrl,
            );
        } else {
            info!("Voltage: {} mV", config.target_voltage_mv);
        }
    } else if !board_config.model.has_voltage_control() {
        info!(
            "Fixed voltage: {} mV (not adjustable)",
            board_config.default_voltage_mv
        );
    }
    // Wait for power rails to stabilize before probing temp sensors
    std::thread::sleep(Duration::from_millis(100));

    // I2C bus scan for diagnostics (helps identify what's on the bus)
    {
        let mut found = Vec::new();
        for addr in 0x08..=0x77 {
            if i2c.probe(addr) {
                found.push(addr);
            }
        }
        info!(
            "I2C scan: {} devices found: {}",
            found.len(),
            found
                .iter()
                .map(|a| format!("0x{:02X}", a))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    let (mut tmp_primary, tmp_secondary) =
        if board_config.temp_sensor == dcentaxe_hal::board::TempSensorKind::Tmp1075 {
            // TMP1075 init with retry — 0x4A may need extra time after power-up
            let mut primary = Tmp1075::new_primary(&mut i2c).ok();
            if primary.is_none() {
                std::thread::sleep(Duration::from_millis(200));
                primary = Tmp1075::new_primary(&mut i2c).ok();
                if primary.is_some() {
                    info!("TMP1075 primary (0x4A) detected on retry");
                }
            }
            let secondary = Tmp1075::new_secondary(&mut i2c).ok();
            if primary.is_some() {
                info!("TMP1075 primary (0x4A) detected");
            } else {
                error!("TMP1075 primary (0x4A) missing on TMP1075 board — mining disabled");
            }
            if secondary.is_some() {
                info!("TMP1075 secondary (0x4B) detected");
            } else {
                error!("TMP1075 secondary (0x4B) missing on TMP1075 board — mining disabled");
            }
            if primary.is_none() || secondary.is_none() {
                mining_permitted = false;
                mining_block_reason = Some("TMP1075 sensor init failed".to_string());
            }
            (primary, secondary)
        } else {
            (None, None)
        };

    if board_config.temp_sensor != dcentaxe_hal::board::TempSensorKind::Tmp1075 {
        tmp_primary = None;
    }

    if mining_permitted && !unsafe_lab_safety_bypass {
        let boot_trusted_temp = temp_emc
            .as_mut()
            .and_then(|s| {
                s.read_external_temp(&mut i2c)
                    .ok()
                    .flatten()
                    .or_else(|| s.read_internal_temp(&mut i2c).ok())
            })
            .or_else(|| {
                emc2103.as_ref().and_then(|e| {
                    e.read_chip_temp(&mut i2c)
                        .or_else(|| e.read_secondary_temp(&mut i2c))
                })
            })
            .or_else(|| {
                tmp_primary
                    .as_ref()
                    .and_then(|s| s.read_temp(&mut i2c).ok())
                    .or_else(|| {
                        tmp_secondary
                            .as_ref()
                            .and_then(|s| s.read_temp(&mut i2c).ok())
                    })
            })
            .or_else(|| {
                if power_mgr.has_vreg_temp_sensor() {
                    power_mgr.get_vreg_temp(&mut i2c).ok()
                } else {
                    None
                }
            });
        if boot_trusted_temp.is_none() {
            mining_permitted = false;
            mining_block_reason =
                Some("No trusted thermal source returned data at boot".to_string());
            fail_closed_power_off(
                "No trusted thermal source returned data at boot",
                &state,
                None,
                Some(&mut power_mgr),
                &mut i2c,
                &mut gpio_ctrl,
            );
        }
    } else if mining_permitted {
        warn!("UNSAFE LAB: mining may start without boot-time thermal proof");
    }

    if mining_permitted {
        gpio_ctrl.reset_asic().expect("ASIC reset failed");
        std::thread::sleep(Duration::from_millis(200));
    }

    // ======================================================================
    // Step 7: UART + ASIC driver
    // ======================================================================
    let uart_config = UartConfig::new().baudrate(esp_idf_svc::hal::units::Hertz(115200));
    // UART pins: BitAxe=17tx/18rx, Nerd (TTGO T-Display S3)=43tx/44rx
    #[cfg(feature = "pins-bitaxe")]
    let uart_driver = UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio17,
        peripherals.pins.gpio18,
        Option::<gpio::AnyIOPin<'_>>::None,
        Option::<gpio::AnyIOPin<'_>>::None,
        &uart_config,
    )
    .expect("UART init failed");
    #[cfg(feature = "pins-nerd")]
    let uart_driver = UartDriver::new(
        peripherals.uart1,
        peripherals.pins.gpio43,
        peripherals.pins.gpio44,
        Option::<gpio::AnyIOPin<'_>>::None,
        Option::<gpio::AnyIOPin<'_>>::None,
        &uart_config,
    )
    .expect("UART init failed");

    let serial_port = SerialPort::from_uart(uart_driver);
    let mut driver = dcentaxe_asic::create_driver(asic_model, serial_port);
    // Use the last pool-suggested difficulty from NVS (ESP-Miner PR #1594 parity).
    // Fall back to 256.0 on first boot or when the cached value is zero/invalid.
    let initial_diff = if cached_pool_difficulty.is_finite() && cached_pool_difficulty >= 1.0 {
        cached_pool_difficulty
    } else {
        256.0
    };
    let chips = if mining_permitted {
        match driver.init(config.target_frequency, asic_count, initial_diff) {
            Ok(n) => {
                info!("ASIC: {} chip(s), model={}", n, asic_model);
                match driver.set_max_baud() {
                    Ok(baud) => info!("ASIC UART switched to {} baud", baud),
                    Err(e) => error!("Failed to set max baud: {} — mining at reduced speed", e),
                }
                display.show_status(
                    &mut i2c,
                    &format!("DCENT_axe v{}", VERSION),
                    &format!("{} chips OK", n),
                    &format!("IP: {}", device_ip),
                    "Connecting to pool...",
                );
                n
            }
            Err(e) => {
                error!("ASIC init failed: {} — continuing without mining", e);
                display.show_status(
                    &mut i2c,
                    &format!("DCENT_axe v{}", VERSION),
                    "ASIC init FAILED",
                    &format!("IP: {}", device_ip),
                    "Dashboard active",
                );
                0
            }
        }
    } else {
        let reason = mining_block_reason
            .clone()
            .unwrap_or_else(|| "Hardware safety gate".to_string());
        display.show_status(
            &mut i2c,
            &format!("DCENT_axe v{}", VERSION),
            "Mining disabled",
            &reason,
            &format!("IP: {}", device_ip),
        );
        warn!("Mining disabled before ASIC init: {}", reason);
        0
    };
    let mining_enabled = mining_permitted && chips > 0;
    {
        let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
        telem.mining_enabled = mining_enabled;
    }

    // ======================================================================
    // Step 8: HTTP server — dashboard + REST API + MCP
    // ======================================================================
    // XPH-4 / MAINAPI-7: name the URI-handler cap and the registered-handler
    // estimate as consts, and pin the headroom invariant at compile time so the
    // human-maintained count can never silently exceed the cap (the class of bug
    // that boot-looped against the previous 64 cap). Do NOT lower MAX_URI_HANDLERS.
    //
    // Registered-handler accounting (re-verified 2026-06-14 after WF-E dashboard
    // route changes — counts of `.fn_handler(` / `register_static(`):
    //   api.rs              53  (`.fn_handler(`)
    //   auth.rs              5
    //   mcp.rs               2
    //   dashboard.rs        12  (/, /index.html + 10 register_static /dashboard/*)
    //   ─────────────────  ─────
    //   total               72  (cgminer_tcp is a TCP listener, not a URI handler)
    const MAX_URI_HANDLERS: usize = 96;
    const REGISTERED_HANDLER_ESTIMATE: usize = 72;
    // Compile-time floor guard: the estimate must always stay under the cap.
    const _: () = assert!(REGISTERED_HANDLER_ESTIMATE < MAX_URI_HANDLERS);
    // Runtime tripwire if a future edit pushes the count near the cap.
    debug_assert!(
        REGISTERED_HANDLER_ESTIMATE + 8 < MAX_URI_HANDLERS,
        "registered HTTP handlers approaching max_uri_handlers cap — re-count and \
         bump MAX_URI_HANDLERS (do not silently exceed it)"
    );
    let http_config = HttpConfig {
        stack_size: 10240,
        max_uri_handlers: MAX_URI_HANDLERS,
        ..Default::default()
    };
    let mut http_server = EspHttpServer::new(&http_config).expect("HTTP server start failed");

    dashboard::register_dashboard(&mut http_server);
    api::register_api(&mut http_server, state.clone());
    mcp::register_mcp(&mut http_server, state.clone());
    cgminer_tcp::start_cgminer_tcp(state.clone());
    info!("HTTP server started: dashboard + REST API + MCP");

    // ======================================================================
    // Step 9: Channels + threads (only if ASIC init succeeded)
    // ======================================================================
    // Frequency command channel: main loop -> mining thread
    let (freq_cmd_tx, freq_cmd_rx) = mpsc::channel::<f32>();

    // Thermal kill switch: set true to stop the mining thread
    let mining_kill = Arc::new(AtomicBool::new(false));

    if mining_enabled {
        let (event_tx, event_rx) = mpsc::channel::<StratumEvent>();
        let (share_tx, share_rx) = mpsc::channel::<MiningEvent>();
        let nominal_hashrate_ghs =
            expected_hashrate_ghs(asic_model, config.target_frequency, asic_count);

        // Solo mesh empty-block path: park a NewJob sender for the LoRa task
        // (fail-closed until mesh config enables solo_mesh_empty).
        #[cfg(feature = "lora")]
        mesh_solo_runtime::park_event_tx(event_tx.clone());

        // Primary stratum thread
        let stratum_config = config.stratum.clone();
        let fallback_pool = config.fallback_pool.clone();
        let primary_status = dcentaxe_stratum::new_shared_stratum_status(&stratum_config, 0);
        {
            let mut statuses = state
                .stratum_status
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            statuses.push(primary_status.clone());
        }
        let _stratum = spawn_stratum_thread(
            "stratum",
            stratum_config,
            fallback_pool,
            event_tx,
            share_rx,
            primary_status,
            nominal_hashrate_ghs,
            // Pinned SV2 authority key applies to the PRIMARY pool only. When
            // unset (default), the SV2 client uses TOFU exactly as before.
            config.sv2_authority_pubkey.clone(),
        );

        // Secondary stratum thread + pool slots (if split_pool configured)
        let split_pool_cfg = config.split_pool.clone();
        let pool_slots: Vec<PoolSlot>;

        if let Some(ref split) = split_pool_cfg {
            let secondary_pct = split.hashrate_pct.clamp(1, 99);
            let primary_pct = 100 - secondary_pct;

            let (event_tx_b, event_rx_b) = mpsc::channel::<StratumEvent>();
            let (share_tx_b, share_rx_b) = mpsc::channel::<MiningEvent>();

            let split_config = split.pool.clone();
            let secondary_status = dcentaxe_stratum::new_shared_stratum_status(&split_config, 1);
            {
                let mut statuses = state
                    .stratum_status
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                statuses.push(secondary_status.clone());
            }
            let _stratum_b = spawn_stratum_thread(
                "stratum-b",
                split_config,
                None,
                event_tx_b,
                share_rx_b,
                secondary_status,
                nominal_hashrate_ghs * secondary_pct as f32 / 100.0,
                // Split/secondary pool: per-pool authority keys are a follow-up.
                None,
            );

            info!(
                "Split mining: {}% to {}:{}, {}% to {}:{}",
                primary_pct,
                crate::shared::sanitize_pool_url(&config.stratum.url),
                config.stratum.port,
                secondary_pct,
                crate::shared::sanitize_pool_url(&split.pool.url),
                split.pool.port
            );

            pool_slots = vec![
                PoolSlot::new(0, primary_pct, event_rx, share_tx),
                PoolSlot::new(1, secondary_pct, event_rx_b, share_tx_b),
            ];
        } else {
            pool_slots = vec![PoolSlot::new(0, 100, event_rx, share_tx)];
        }

        // Shared pool stats handle — dispatcher writes, API reads
        let pool_stats_for_mining = state.pool_stats.clone();

        // Mining thread
        let asic_model_copy = asic_model;
        let stats_for_mining = shared_mining_stats.clone();
        let mining_kill_flag = mining_kill.clone();
        let _mining = std::thread::Builder::new()
            .name("mining".into())
            .stack_size(24 * 1024)
            .spawn(move || {
                // RefCell created INSIDE the thread — never crosses thread boundaries
                let driver_cell = std::cell::RefCell::new(driver);

                // Poll for frequency change commands from main loop
                let freq_rx = freq_cmd_rx;

                let target_freq = config.target_frequency;
                let dispatcher_config = match asic_model {
                    dcentaxe_asic::AsicModel::BM1397 => {
                        DispatcherConfig::for_bm1397(target_freq, asic_count)
                    }
                    dcentaxe_asic::AsicModel::BM1366 => {
                        DispatcherConfig::for_bm1366(target_freq, asic_count)
                    }
                    dcentaxe_asic::AsicModel::BM1368 => {
                        DispatcherConfig::for_bm1368(target_freq, asic_count)
                    }
                    dcentaxe_asic::AsicModel::BM1370 => {
                        DispatcherConfig::for_bm1370(target_freq, asic_count)
                    }
                    dcentaxe_asic::AsicModel::BM1373 => {
                        DispatcherConfig::for_bm1370(target_freq, asic_count)
                    }
                    #[cfg(feature = "asic-kf1950")]
                    dcentaxe_asic::AsicModel::KF1950 => {
                        DispatcherConfig::for_kf1950(target_freq, asic_count)
                    }
                };
                let mut dispatcher = MiningDispatcher::with_pools(pool_slots, dispatcher_config);
                dispatcher.set_shared_stats(stats_for_mining);
                dispatcher.set_shared_pool_stats(pool_stats_for_mining);

                let mut send_work_fn =
                    |work: &dcentaxe_stratum::MiningWork, job_id: u8| -> Result<(), String> {
                        let job = bridge::mining_work_to_job(work, job_id, asic_model_copy);
                        driver_cell
                            .borrow_mut()
                            .send_work(&job)
                            .map_err(|e| format!("{}", e))
                    };

                let mut process_work_fn = || -> Vec<(u8, u32, u32, u8)> {
                    match driver_cell.borrow_mut().read_responses(10) {
                        Ok(results) => results
                            .into_iter()
                            .filter_map(|r| {
                                if let AsicResult::Nonce {
                                    job_id,
                                    nonce,
                                    rolled_version,
                                    asic_nr,
                                    timestamp_us: _,
                                } = r
                                {
                                    // For BM1397: rolled_version = midstate_index (0-3)
                                    // For BM1366/68/70: rolled_version = actual rolled version from ASIC
                                    // The dispatcher's handle_nonce will convert midstate_index
                                    // to actual version for BM1397 using increment_bitmask.
                                    Some((job_id, nonce, rolled_version, asic_nr))
                                } else {
                                    None
                                }
                            })
                            .collect(),
                        Err(_) => Vec::new(),
                    }
                };

                // Run dispatcher with frequency command polling
                let mut apply_driver_config = |new_diff: Option<f64>, new_mask: Option<u32>| {
                    if let Some(mask) = new_mask {
                        if let Err(e) = driver_cell.borrow_mut().set_version_mask(mask) {
                            error!("Failed to update ASIC version mask: {}", e);
                        }
                    }
                    if let Some(diff) = new_diff {
                        if let Err(e) = driver_cell.borrow_mut().set_difficulty(diff) {
                            error!("Failed to update ASIC difficulty: {}", e);
                        }
                    }
                };

                loop {
                    // Check thermal kill switch
                    if mining_kill_flag.load(Ordering::Relaxed) {
                        warn!("Mining thread: thermal kill signal received — stopping");
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(10));
                            if !mining_kill_flag.load(Ordering::Relaxed) {
                                break; // resume if cleared (future: reboot path)
                            }
                        }
                    }

                    // Check for frequency change commands from main loop/autotuner
                    while let Ok(new_freq) = freq_rx.try_recv() {
                        info!("Mining thread: frequency change to {:.0} MHz", new_freq);
                        if let Err(e) = driver_cell.borrow_mut().set_frequency(new_freq) {
                            error!("Frequency change failed: {}", e);
                        }
                    }
                    // Run one iteration of the dispatcher
                    dispatcher.run_once(
                        &mut send_work_fn,
                        &mut process_work_fn,
                        &mut apply_driver_config,
                    );

                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
            .expect("Mining thread spawn failed");
    } else {
        warn!("Mining disabled — ASIC init failed. Dashboard/API still accessible.");
    }

    // ======================================================================
    // Step 10: Main loop — telemetry, autotuner, thermal protection
    // ======================================================================
    info!("Mining active! Dashboard at http://{}/", device_ip);
    gpio_ctrl.set_led(true).ok();

    let mut autotuner = Autotuner::new(
        state.board_limits.min_frequency,
        state.board_limits.max_frequency,
        state.board_limits.min_voltage_mv,
        state.board_limits.max_voltage_mv,
    );
    autotuner.set_power_limits(&power_limits);
    let start_time = Instant::now();

    // Track last-applied voltage/freq so API changes are detected correctly
    let mut last_applied_voltage = config.target_voltage_mv;
    let mut last_applied_freq = config.target_frequency;

    let mut consecutive_i2c_failures: u32 = 0;
    let mut consecutive_temp_failures: u32 = 0;
    let mut consecutive_fan_stall: u32 = 0;
    // ES-2: consecutive supervisor ticks where a die-equipped board could read
    // NO ASIC-die sensor while a cooler proxy remained (see DIE_BLIND_CONFIRM_TICKS).
    let mut consecutive_die_blind: u32 = 0;
    // INA260 over-current backstop debounce (DS4432U boards). Counts consecutive
    // supervisor ticks measured over the rated PowerLimits envelope × margin.
    let mut ina_oc_strikes: u8 = 0;
    let mut fan1_ever_seen: bool = false; // Track if fan1 ever reported RPM > 0
    let mut display_page: u8 = 0;
    let mut notification_cooldown: u8 = 0; // Force carousel pages between notifications
    let mut identify_notification_cooldown: u8 = 0; // Avoid swarm-locate OLED storms
                                                    // B-ESP-10: the OLED is a read surface — strip any user:pass@ creds from the
                                                    // pool URL (host stays visible for at-the-miner diagnostics).
    let pool_display = format!(
        "{}:{}",
        crate::shared::sanitize_pool_url(&config.stratum.url),
        config.stratum.port
    );

    // Gamification: track previous values to detect events for OLED notifications
    let mut prev_accepted: u64 = 0;
    let mut prev_rejected: u64 = 0;
    let mut prev_best_diff: f64 = 0.0;
    let mut prev_clean_jobs: u64 = 0;
    let mut prev_streak: u32 = 0;
    let mut diary_tick: u32 = 0; // counts up for mining diary rotation
    let mut last_transition_flash: bool = false; // alternate to trigger flash on page change

    // v3 state variables
    let mut screensaver_x: i16 = 10; // bouncing Bitcoin screensaver position
    let mut screensaver_y: i16 = 5;
    let mut screensaver_dx: i16 = 1;
    let mut screensaver_dy: i16 = 1;
    let mut led_heartbeat_phase: u8 = 0; // 0-3 for heartbeat pattern
    let mut prev_heartbeat_speed: u8 = 2; // track speed changes for phase reset
    let mut prev_block_height: u32 = 0; // for special block detection
    let mut comparison_idx: usize = 0; // hashrate comparison rotation
    let mut fortune_idx: usize = 0; // fortune cookie rotation
    let mut pixel_art_frame: u8 = 0; // animation frame counter
    let mut companion_visit: u8 = 0; // rotates companion room/full-screen variants
    let mut pending_flash: u8 = 0; // non-blocking flash state machine (decrements each tick)
    let mut share_flash_tick: u8 = 0; // lightning sprite after share accepted
    let mut achievement_flash_tick: u8 = 0; // rocket sprite after achievement
    let mut chill_miner_secs: u64 = 0; // consecutive seconds under 65C for Chill Miner achievement
    let mut low_power_secs: u64 = 0; // consecutive seconds under 10W for Power Miser achievement
    let mut daily_summary_shown: bool = false; // daily mining summary (show once after 24h)
    let mut last_history_sample_ms: u64 = 0;
    let mut last_schedule_key: Option<String> = None;
    let mut schedule_base_point: Option<(f32, u16)> = None;

    // WiFi reconnect: poll-based, called once per main-loop tick. Replaces
    // the old "one-shot connect, hope for the best" behaviour. Without this
    // a flaky AP would starve the device until the WDT tripped (3 trips →
    // safe-mode), forcing the user to reboot to recover.
    let mut wifi_reconnect_state = wifi::ReconnectState::new();

    // Subscribe the main task to the task watchdog. With
    // CONFIG_ESP_TASK_WDT_PANIC=n this turns a wedged main loop into a reset
    // that the boot-time WDT counter + safe-mode flow recovers from.
    unsafe {
        let rc = sys::esp_task_wdt_add(std::ptr::null_mut());
        if rc != 0 {
            warn!("esp_task_wdt_add returned {}", rc);
        }
    }
    let mut wdt_cleared_this_run = false;

    // MQTT + Home Assistant auto-discovery publisher (default-OFF, opt-in via
    // config.mqtt.enabled). Outbound + fail-soft on its own thread — never blocks
    // mining/safety, adds no HTTP handler. Returns immediately when MQTT is off.
    mqtt::spawn_publisher(state.clone());

    // On-board SX1262 LoRa radio task (default-OFF). Owns the radio on its own
    // thread — never blocks mining/safety; fail-soft if the bus never came up.
    #[cfg(feature = "lora")]
    if let Some(bus) = lora_bus {
        lora_task::spawn(state.clone(), bus);
    }

    loop {
        // Feed the WDT. Happens every iteration; 5 s loop is well under the
        // 15 s CONFIG_ESP_TASK_WDT_TIMEOUT_S budget.
        unsafe {
            let _ = sys::esp_task_wdt_reset();
        }
        std::thread::sleep(Duration::from_secs(5));
        let uptime = start_time.elapsed().as_secs();

        // After 5 minutes of stable uptime, clear the WDT reset counter so
        // future transient wedges don't accumulate into a false safe-mode trip.
        if !wdt_cleared_this_run && uptime >= 300 && (wdt_count > 0 || wdt_since_ms > 0) {
            let p = nvs_partition.clone();
            if let Ok(mut nvs) =
                esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(p, "dcentaxe", true)
            {
                nvs_config::save_wdt_counters(&mut nvs, 0, 0);
                info!("WDT window cleared after 5 min stable uptime");
            }
            wdt_count = 0;
            wdt_since_ms = 0;
            wdt_cleared_this_run = true;
        }

        // ── Non-blocking flash state machine (evolution/special block effects) ──
        if pending_flash > 0 {
            display.invert_display(&mut i2c, true);
            std::thread::sleep(Duration::from_millis(30));
            display.invert_display(&mut i2c, false);
            pending_flash -= 1;
        }

        // Decrement sprite flash timers
        if share_flash_tick > 0 {
            share_flash_tick -= 1;
        }
        if achievement_flash_tick > 0 {
            achievement_flash_tick -= 1;
        }
        if identify_notification_cooldown > 0 {
            identify_notification_cooldown -= 1;
        }

        let identify_active = {
            let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
            let now = shared::unix_time_ms() / 1000;
            if swarm.identify_until_epoch_s > 0 && swarm.identify_until_epoch_s <= now {
                swarm.identify_until_epoch_s = 0;
                false
            } else {
                swarm.identify_until_epoch_s > now
            }
        };
        if identify_active && identify_notification_cooldown == 0 {
            let live_config = state.config.lock().unwrap_or_else(|e| e.into_inner());
            let identify_name = if live_config.hostname.is_empty() {
                format!("dcentaxe-{}", board_config.device_model)
            } else {
                live_config.hostname.clone()
            };
            display.notify_urgent(
                "(^_^) HI!",
                &identify_name,
                &format!("IP {}", device_ip),
                "Swarm locate active",
            );
            identify_notification_cooldown = 6; // show locate text at most every 30s
        }
        if identify_active {
            gpio_ctrl.toggle_led().ok();
        }

        // ── Collect telemetry ──────────────────────────────────────────
        let power = power_mgr.get_telemetry(&mut i2c).ok();
        // Hex boards: chip_temp from TMP1075 (prefer primary 0x4A, fallback to secondary 0x4B)
        // Single-ASIC: chip_temp from EMC2101 external diode
        // GT (EMC2103): sensors are physically wired so that what ESP-Miner calls
        // "external_temp" is on EMC2103 sensor 2 and vice-versa. Matches upstream
        // `temp_flip = true` for board "801" (PR #1616 / commit 33d7210). `chip_temp`
        // below reads the primary chip sensor (registers 0x04/0x05 = TEMP2), and
        // `gt_temp2` reads the secondary (registers 0x02/0x03 = TEMP1).
        // GT secondary die temp on the EMC2103 secondary sensor (honors temp_flip).
        let gt_temp2 = emc2103
            .as_ref()
            .and_then(|e| e.read_secondary_temp(&mut i2c));
        let apply_temp_offset =
            |temp: Option<f32>| temp.map(|t| t + board_config.temp_offset_c as f32);
        let chip_temp = if let Some(ref mut emc) = temp_emc {
            if board_config.emc_internal_temp {
                apply_temp_offset(emc.read_internal_temp(&mut i2c).ok())
            } else {
                apply_temp_offset(emc.read_external_temp(&mut i2c).ok().flatten())
            }
        } else if fan_is_emc2103 {
            // GT: primary chip temp on the EMC2103 chip sensor (honors temp_flip).
            // See `gt_temp2` block above for the upstream reference.
            apply_temp_offset(emc2103.as_ref().and_then(|e| e.read_chip_temp(&mut i2c)))
        } else {
            // Hex boards: use max of both TMP1075s for safety (hotter sensor = closer to ASIC)
            let t1 = tmp_primary
                .as_ref()
                .and_then(|s| s.read_temp(&mut i2c).ok());
            let t2 = tmp_secondary
                .as_ref()
                .and_then(|s| s.read_temp(&mut i2c).ok());
            apply_temp_offset(match (t1, t2) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            })
        };
        // board_temp: EMC2101 internal on single-chip, or TMP1075 secondary on Hex (for temp2)
        let board_temp = apply_temp_offset(
            temp_emc
                .as_ref()
                .and_then(|s| s.read_internal_temp(&mut i2c).ok())
                .or_else(|| {
                    tmp_secondary
                        .as_ref()
                        .and_then(|s| s.read_temp(&mut i2c).ok())
                }),
        );
        let inlet_temp = apply_temp_offset(
            tmp_primary
                .as_ref()
                .and_then(|s| s.read_temp(&mut i2c).ok()),
        );
        let outlet_temp = apply_temp_offset(
            tmp_secondary
                .as_ref()
                .and_then(|s| s.read_temp(&mut i2c).ok()),
        );
        let gt_temp2 = apply_temp_offset(gt_temp2);
        let vreg_temp = if power_mgr.has_vreg_temp_sensor() {
            power_mgr.get_vreg_temp(&mut i2c).ok()
        } else {
            None
        };

        // ── Thermal fold + sensor-adequacy decision (host-pure) ────────
        // `evaluate_thermal` folds every sensor into `max_temp`, reports
        // `any_temp_valid` (the all-None BLIND trigger + I2C-dead watchdog),
        // AND flags `die_reading_blind`: a board EXPECTED to carry an ASIC-die
        // sensor has lost every die reading while only cooler proxies remain,
        // so `max_temp` understates the true die temp (ES-2 fail-open fix).
        // Extracted to `crate::thermal_safety` so the decision is host-tested
        // under `cargo test -p dcentaxe-core`.
        //
        // `chip_die_expected`: true for every board that physically carries an
        // ASIC-die sensor (EMC2101 external diode, EMC2103 chip sensor, or Hex
        // TMP1075s — i.e. `temp_sensor != None`, or any EMC2103 board). False
        // only for a board whose sole thermal source is a cooler proxy (custom
        // `temp_sensor == None` on TPS546 vreg temp), where a missing chip_temp
        // is NORMAL and must never be treated as blind.
        let chip_die_expected =
            board_config.temp_sensor != dcentaxe_hal::board::TempSensorKind::None || fan_is_emc2103;
        let thermal = thermal_safety::evaluate_thermal(
            chip_temp,
            gt_temp2,
            board_temp,
            inlet_temp,
            outlet_temp,
            vreg_temp,
            chip_die_expected,
        );
        let max_temp = thermal.max_temp;
        let runtime_mining_active = mining_enabled && !mining_kill.load(Ordering::Relaxed);
        let thermal_clamp_active = max_temp > WARNING_TEMP_C;

        // ── Sensor failure detection ───────────────────────────────────
        // If ALL sensors return None, we have no thermal data — assume danger
        let any_temp_valid = thermal.any_temp_valid;

        if runtime_mining_active {
            // DELIBERATE CONSERVATIVE POSTURE: ANY regulator fault is treated as
            // an immediate, unconditional fail-closed power-off. There is no
            // recoverable-vs-hard `status_word` inspection, no cooldown, and no
            // auto re-enable here — a hardware-validated recovery FSM is future
            // work (see docs/PUBLIC_BETA_READINESS_REPORT.md and the RESERVED
            // helpers in dcentaxe-hal::power). For a beta the known-working
            // hard-kill is the correct fail-safe; do not soften this path.
            if let Err(e) = power_mgr.check_fault(&mut i2c) {
                error!("POWER FAULT: {} — disabling ASIC for safety", e);
                fan_ctrl.set_speed(100).ok();
                apply_fan_speed_or_fail_closed(
                    &mut i2c,
                    100,
                    &fan_emc2302,
                    &temp_emc,
                    &emc2103,
                    runtime_mining_active,
                    &state,
                    &mining_kill,
                    &mut power_mgr,
                    &mut gpio_ctrl,
                    "POWER FAULT fan pin",
                );
                fail_closed_power_off(
                    &format!("POWER FAULT: {}", e),
                    &state,
                    Some(&mining_kill),
                    Some(&mut power_mgr),
                    &mut i2c,
                    &mut gpio_ctrl,
                );
                display.notify_urgent(
                    "!! POWER FAULT !!",
                    "ASIC disabled",
                    &format!("IP {}", device_ip),
                    "Check PSU/cabling",
                );
            }

            // INA260 over-current/over-power backstop for DS4432U boards, which
            // get no PMBus STATUS_WORD OC detection from check_fault above. If
            // the measured INA260 input power/current stays above the board's
            // rated PowerLimits envelope × INA260_OC_MARGIN for
            // INA260_OC_DEBOUNCE_TICKS consecutive ticks, run the SAME
            // fail-closed power-off the TPS546 fault path uses. Debounced + a
            // margin above the rated envelope so a transient cannot nuisance-trip.
            if power_mgr.has_ina260() {
                let (max_power_w, max_current_a) = {
                    let limits = state
                        .config
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .power_limits();
                    (limits.max_power_w, limits.max_current_a)
                };
                let measured = power.as_ref();
                let meas_w = measured.map(|p| p.power_w).unwrap_or(f32::NAN);
                let meas_a = measured.map(|p| p.current_ma / 1000.0).unwrap_or(f32::NAN);
                // NaN (no snapshot / a failed per-field sub-read) is benign:
                // `ina260_oc_over_envelope` rejects non-finite fields, byte-matching
                // the prior `.unwrap_or(false)` + per-field `power_field_available`
                // guards (`power_over || current_over`).
                let over = ina260_oc_over_envelope(
                    meas_w,
                    meas_a,
                    max_power_w,
                    max_current_a,
                    INA260_OC_MARGIN,
                );
                if over {
                    ina_oc_strikes = ina260_oc_strike_next(true, ina_oc_strikes);
                    warn!(
                        "INA260 OC backstop: {:.1}W / {:.2}A over {:.0}% of rated [{:.1}W, {:.2}A] — strike {}/{}",
                        meas_w,
                        meas_a,
                        INA260_OC_MARGIN * 100.0,
                        max_power_w,
                        max_current_a,
                        ina_oc_strikes,
                        INA260_OC_DEBOUNCE_TICKS
                    );
                    if ina260_oc_should_cut(ina_oc_strikes, INA260_OC_DEBOUNCE_TICKS) {
                        error!(
                            "INA260 OVER-CURRENT: {:.1}W / {:.2}A sustained over rated envelope — cutting ASIC power",
                            meas_w, meas_a
                        );
                        // Cut hash power FIRST (: cut hash before raising
                        // fan noise), then pin the fan to cool. Power is already
                        // off here, so a failed fan write needs no further kill.
                        fail_closed_power_off(
                            &format!(
                                "INA260 over-current: {:.1}W / {:.2}A sustained",
                                meas_w, meas_a
                            ),
                            &state,
                            Some(&mining_kill),
                            Some(&mut power_mgr),
                            &mut i2c,
                            &mut gpio_ctrl,
                        );
                        fan_ctrl.set_speed(100).ok();
                        if let Err(e) =
                            apply_fan_speed(&mut i2c, 100, &fan_emc2302, &temp_emc, &emc2103)
                        {
                            warn!("INA260 OC: fan pin after power-cut failed: {}", e);
                        }
                        display.notify_urgent(
                            "!! OVER-CURRENT !!",
                            "ASIC disabled",
                            &format!("IP {}", device_ip),
                            "Check PSU/load",
                        );
                    }
                } else {
                    ina_oc_strikes = ina260_oc_strike_next(false, ina_oc_strikes);
                }
            }
        }

        if !any_temp_valid {
            consecutive_temp_failures += 1;
            warn!(
                "All temperature sensors failed ({}/10) — forcing fan to 100%",
                consecutive_temp_failures
            );
            fan_ctrl.set_speed(100).ok();
            apply_fan_speed_or_fail_closed(
                &mut i2c,
                100,
                &fan_emc2302,
                &temp_emc,
                &emc2103,
                runtime_mining_active,
                &state,
                &mining_kill,
                &mut power_mgr,
                &mut gpio_ctrl,
                "THERMAL BLIND fan pin",
            );
            // Sustained temp sensor loss while mining → kill mining (especially critical on Hex)
            if runtime_mining_active && !unsafe_lab_safety_bypass {
                error!("THERMAL BLIND: {} consecutive temp sensor failures — killing mining for safety", consecutive_temp_failures);
                mining_kill.store(true, Ordering::Relaxed);
                {
                    let mut telem = state
                        .telemetry
                        .lock()
                        .unwrap_or_else(|err| err.into_inner());
                    telem.mining_enabled = false;
                    telem.pool_connected = false;
                }
                power_mgr.disable(&mut i2c).ok();
                gpio_ctrl.enable_buck(false).ok();
                display.notify_urgent(
                    "THERMAL BLIND",
                    "Sensors offline",
                    "ASIC disabled",
                    "Fan forced 100%",
                );
            }
        } else {
            consecutive_temp_failures = 0;
        }

        // ── ES-2: die-sensor blindness (fail-closed) ───────────────────
        // A board EXPECTED to carry an ASIC-die sensor has lost EVERY die
        // reading while only cooler proxies (board/inlet/outlet/vreg, ~10-20 C
        // below the die) remain, so `max_temp` understates the true die temp
        // and the overtemp cut could fire late or never. This is distinct from
        // the all-None case above (there a proxy is still valid, so
        // `any_temp_valid` is true and that branch does NOT fire).
        //
        // Force the fan to 100% immediately (immediate airflow is the only safe
        // response when the die is unmeasurable — the documented exception to
        // cut-hash-before-fan-noise), then cut ASIC power after
        // DIE_BLIND_CONFIRM_TICKS consecutive ticks to reject a single transient
        // I2C read glitch. Gated on active mining (no die heat otherwise) and,
        // for the kill, on the lab-safety bypass like the all-None BLIND path.
        if thermal.die_reading_blind && runtime_mining_active {
            consecutive_die_blind += 1;
            warn!(
                "DIE SENSOR BLIND ({}/{}) — no ASIC-die reading, only cooler proxies left; \
                 proxy max_temp {:.0}C UNDERSTATES the die — forcing fan 100%",
                consecutive_die_blind, DIE_BLIND_CONFIRM_TICKS, max_temp
            );
            fan_ctrl.set_speed(100).ok();
            apply_fan_speed_or_fail_closed(
                &mut i2c,
                100,
                &fan_emc2302,
                &temp_emc,
                &emc2103,
                runtime_mining_active,
                &state,
                &mining_kill,
                &mut power_mgr,
                &mut gpio_ctrl,
                "DIE BLIND fan pin",
            );
            if consecutive_die_blind >= DIE_BLIND_CONFIRM_TICKS && !unsafe_lab_safety_bypass {
                error!(
                    "DIE SENSOR BLIND: {} consecutive ticks with no ASIC-die reading — \
                     killing mining for safety (proxy max_temp {:.0}C cannot protect the die)",
                    consecutive_die_blind, max_temp
                );
                mining_kill.store(true, Ordering::Relaxed);
                {
                    let mut telem = state
                        .telemetry
                        .lock()
                        .unwrap_or_else(|err| err.into_inner());
                    telem.mining_enabled = false;
                    telem.pool_connected = false;
                }
                power_mgr.disable(&mut i2c).ok();
                gpio_ctrl.enable_buck(false).ok();
                display.notify_urgent(
                    "DIE SENSOR BLIND",
                    "ASIC temp lost",
                    "ASIC disabled",
                    "Fan forced 100%",
                );
            }
        } else {
            consecutive_die_blind = 0;
        }

        // ── I2C health watchdog (only relevant when mining) ────────────
        if runtime_mining_active {
            if power.is_none() && !any_temp_valid {
                consecutive_i2c_failures += 1;
                if consecutive_i2c_failures >= 3 {
                    error!(
                        "I2C bus dead — killing mining thread ({} consecutive failures)",
                        consecutive_i2c_failures
                    );
                    mining_kill.store(true, Ordering::Relaxed);
                    {
                        let mut telem = state
                            .telemetry
                            .lock()
                            .unwrap_or_else(|err| err.into_inner());
                        telem.mining_enabled = false;
                        telem.pool_connected = false;
                    }
                    fan_ctrl.set_speed(100).ok();
                    apply_fan_speed_or_fail_closed(
                        &mut i2c,
                        100,
                        &fan_emc2302,
                        &temp_emc,
                        &emc2103,
                        runtime_mining_active,
                        &state,
                        &mining_kill,
                        &mut power_mgr,
                        &mut gpio_ctrl,
                        "I2C BUS DEAD fan pin",
                    );
                    power_mgr.disable(&mut i2c).ok();
                    gpio_ctrl.enable_buck(false).ok();
                    display.notify_urgent(
                        "I2C BUS DEAD",
                        "ASIC disabled",
                        "Fan forced 100%",
                        &format!("IP {}", device_ip),
                    );
                    // Continue loop — HTTP server stays alive
                }
            } else {
                consecutive_i2c_failures = 0;
            }
        }

        // ── Fan stall detection (safety critical) ─────────────────────
        let fan_speed = fan_ctrl.current_speed();
        // Read actual fan RPM from EMC2302, EMC2101, or EMC2103 TACH registers
        let mut fan2_rpm_val: u32 = 0;
        let (fan_rpm, any_fan_dead) = if let Some(ref emc) = fan_emc2302 {
            // EMC2302 (Hex boards): read both fans independently
            let rpm1 = emc.get_fan1_rpm(&mut i2c).unwrap_or(0);
            let rpm2 = emc.get_fan2_rpm(&mut i2c).unwrap_or(0);
            fan2_rpm_val = rpm2;
            // Track which fans have ever been seen (distinguishes unconnected from stalled)
            if rpm1 > 0 {
                fan1_ever_seen = true;
            }
            // Expose fan 1 and fan 2 independently through telemetry.
            // Fault detection below still treats either missing Hex fan as unsafe.
            let rpm = rpm1;
            // Hex-class boards require tach on both fans. Treat 0 RPM as a fault
            // after the existing multi-loop confirmation window below.
            let dead = fan_speed > 0 && (rpm1 == 0 || rpm2 == 0);
            (rpm, dead)
        } else if let Some(ref emc) = temp_emc {
            let rpm = emc.read_fan_rpm(&mut i2c).unwrap_or(0);
            if rpm > 0 {
                fan1_ever_seen = true;
            }
            // Single-fan board: only flag stall if tach previously reported RPM
            // (BitAxe Max and similar boards may not have tach wire connected).
            // HALT-6 / XPSAFE-7: a fan that NEVER spins keeps fan1_ever_seen=false,
            // so the lenient heuristic could never fire the FAN STALL kill. When the
            // operator has opted in (fan_tach_present=true -> tach_proof_required()),
            // fail closed on a never-spinning fan; genuinely tachless boards keep the
            // lenient heuristic.
            (
                rpm,
                (board_config.tach_proof_required() || fan1_ever_seen) && rpm == 0 && fan_speed > 0,
            )
        } else if fan_is_emc2103 {
            let rpm = emc2103.as_ref().map(|e| e.read_rpm(&mut i2c)).unwrap_or(0);
            if rpm > 0 {
                fan1_ever_seen = true;
            }
            // GT-class boards require a working tach path.
            (rpm, rpm == 0 && fan_speed > 0)
        } else {
            (0, false)
        };
        fan_ctrl.rpm = fan_rpm;
        if runtime_mining_active && fan_speed > 0 && any_fan_dead {
            consecutive_fan_stall += 1;
            if consecutive_fan_stall >= 3 {
                // 15 seconds of fan stall while commanded on — stall confirmed
                error!(
                    "FAN STALL DETECTED — no RPM for 15s while fan at {}%",
                    fan_speed
                );
                mining_kill.store(true, Ordering::Relaxed);
                {
                    let mut telem = state
                        .telemetry
                        .lock()
                        .unwrap_or_else(|err| err.into_inner());
                    telem.mining_enabled = false;
                    telem.pool_connected = false;
                }
                power_mgr.disable(&mut i2c).ok();
                gpio_ctrl.enable_buck(false).ok();
                display.notify_urgent(
                    "!! FAN STALL !!",
                    "RPM = 0",
                    "ASIC disabled",
                    "Check fan connector",
                );
                display.show_status(
                    &mut i2c,
                    "FAN STALL DETECTED!",
                    &format!("RPM=0 fan={}%", fan_speed),
                    "ASIC shutting down",
                    "Check fan connection",
                );
            } else if consecutive_fan_stall >= 2 {
                // 10 seconds — warning
                warn!(
                    "Fan stall warning — RPM=0 for {}s",
                    consecutive_fan_stall * 5
                );
                display.notify_urgent(
                    "FAN WARNING",
                    "RPM = 0",
                    &format!("Fan at {}%", fan_speed),
                    "Check fan!",
                );
            }
        } else {
            consecutive_fan_stall = 0;
        }

        // ── Update shared telemetry ────────────────────────────────────
        {
            let status_snapshots = stratum_status_snapshots(&state);
            let (pool_connected, pool_difficulty) = status_snapshots
                .iter()
                .find(|status| status.connected)
                .map(|status| (true, status.difficulty))
                .unwrap_or_else(|| {
                    state
                        .pool_stats
                        .lock()
                        .map(|stats| {
                            let connected = stats.iter().any(|pool| pool.connected);
                            let difficulty = stats
                                .iter()
                                .find(|pool| pool.connected)
                                .map(|pool| pool.difficulty)
                                .unwrap_or(0.0);
                            (connected, difficulty)
                        })
                        .unwrap_or((false, 0.0))
                });
            let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref p) = power {
                // HALPWR-2 / COMP-6: per-field NaN guard. power.rs get_telemetry now
                // returns f32::NAN for a failed sub-read (e.g. a transient INA260 /
                // TPS546 NACK on one field). Writing a NaN into telem.power_w would
                // poison mean_power_w in the autotuner's PowerWindow and the
                // heartbeat/history log. Guard each field independently so a single
                // blanked field HOLDS its last-good telem value rather than
                // overwriting it, and one blanked field never blanks the others.
                if power_field_available(p.voltage_mv) {
                    telem.voltage_mv = p.voltage_mv;
                }
                if power_field_available(p.current_ma) {
                    telem.current_ma = p.current_ma;
                }
                if power_field_available(p.power_w) {
                    telem.power_w = p.power_w;
                }
                if power_field_available(p.input_voltage_mv) {
                    telem.input_voltage_mv = p.input_voltage_mv;
                }
            }
            telem.chip_temp_c = chip_temp.unwrap_or(0.0);
            telem.board_temp_c = board_temp.unwrap_or(0.0);
            telem.vreg_temp_c = vreg_temp.unwrap_or(0.0);
            telem.inlet_temp_c = inlet_temp.unwrap_or(0.0);
            telem.outlet_temp_c = outlet_temp.unwrap_or(0.0);
            telem.sensors_ok = any_temp_valid;
            // HALT-6 / XPSAFE-7: surface "no fan proof (heuristic only)" so the
            // dashboard/self-test does not present a tachless board's fan as proven
            // healthy. True when the board has no tach proof available.
            telem.fan_proof_heuristic_only = !board_config.tach_proof_required();
            // HALT-5: on emc_internal_temp boards chip_temp is a board-ambient
            // proxy (EMC2101 internal die + offset), not a true junction reading.
            // Label it so the dashboard does not present it as junction temp. The
            // thermal cuts and the +offset are UNCHANGED.
            telem.chip_temp_is_ambient_proxy = board_config.emc_internal_temp;
            telem.fan_speed_pct = fan_ctrl.current_speed();
            telem.fan_rpm = fan_ctrl.last_rpm();
            telem.fan2_rpm = fan2_rpm_val;
            telem.uptime_secs = uptime;
            telem.pool_connected = pool_connected;
            telem.pool_difficulty = pool_difficulty;

            // Populate per-chip data for any board with at least one ASIC.
            // Single-chip boards (Gamma/Ultra/Supra/Max) get one entry so the
            // dashboard ASIC Chips card always appears with live telemetry.
            if asic_count >= 1 {
                // MAINAPI-1: per_chip is a fixed [PerChipStats; MAX_CHIPS] (=6). An
                // asic_count > MAX_CHIPS would panic on an unchecked index = miner
                // down. Bound the loop to MAX_CHIPS and use .get() as a
                // belt-and-suspenders degrade so a bad asic_count can never index
                // out of bounds.
                let chip_count = (asic_count as usize).min(dcentaxe_mining::stats::MAX_CHIPS);
                let mut chips = Vec::with_capacity(chip_count);
                for i in 0..chip_count {
                    let chip_t = if fan_is_emc2103 {
                        if i == 0 {
                            chip_temp
                        } else if i == 1 {
                            gt_temp2
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    // Get per-chip nonce/error counts from mining stats
                    let (chip_nonces, chip_errs) = if let Ok(stats) = state.stats.lock() {
                        stats
                            .per_chip
                            .get(i)
                            .map(|c| (c.nonces, c.errors))
                            .unwrap_or((0, 0))
                    } else {
                        (0, 0)
                    };
                    chips.push(shared::ChipData {
                        temp_c: chip_t,
                        status: shared::ChipStatus::Unknown,
                        hw_errors: chip_errs,
                        shares: chip_nonces,
                        hashrate_ghs: None,
                    });
                }
                telem.chip_data = chips;
            } else {
                telem.chip_data.clear();
            }
            telem.free_heap = unsafe { sys::esp_get_free_heap_size() };
            telem.wifi_rssi = unsafe {
                let mut rssi: i32 = 0;
                esp_idf_svc::sys::esp_wifi_sta_get_rssi(&mut rssi);
                rssi as i8
            };
            telem.device_ip = device_ip.clone();
        }

        // ── WiFi link health + auto-reconnect ──────────────────────────
        // Poll once per tick (~5 s). On disconnect, calls `wifi.connect()`
        // with linear backoff (1 → 2 → 5 → 10 → 30 s, cap 30 s) until link
        // returns. See wifi::tick_reconnect for design rationale.
        {
            let mut wifi_guard = state.wifi.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut w) = *wifi_guard {
                let now_ms = shared::unix_time_ms();
                wifi::tick_reconnect(w.as_mut(), &mut wifi_reconnect_state, now_ms);
            }
        }

        {
            let snap = state
                .stats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot();
            let live_config = state.config.lock().unwrap_or_else(|e| e.into_inner());
            let local_id = {
                let mut mac = [0u8; 6];
                unsafe {
                    esp_idf_svc::sys::esp_wifi_get_mac(
                        esp_idf_svc::sys::wifi_interface_t_WIFI_IF_STA,
                        mac.as_mut_ptr(),
                    );
                }
                format!(
                    "bitaxe-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
                )
            };
            let mut swarm = state.swarm.lock().unwrap_or_else(|e| e.into_inner());
            swarm.local.id = local_id;
            swarm.local.ip = device_ip.clone();
            swarm.local.hostname = if live_config.hostname.is_empty() {
                format!("dcentaxe-{}", board_config.device_model)
            } else {
                live_config.hostname.clone()
            };
            let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            swarm.local.mining_enabled = telem.mining_enabled;
            swarm.local.pool_connected = telem.pool_connected;
            swarm.local.hashrate_ghs = snap.hashrate_5m_ghs;
            swarm.local.last_seen_unix_ms = shared::unix_time_ms();
            swarm.discovery.api_url = Some(format!("http://{}/api/swarm", device_ip));
            swarm.discovery.mcp_url = Some(format!("http://{}/mcp", device_ip));
        }

        let history_now_ms = shared::unix_time_ms();
        if history_now_ms.saturating_sub(last_history_sample_ms) >= 60_000 {
            let snap = state
                .stats
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot();
            let telem = state
                .telemetry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let statuses = shared::stratum_status_snapshots(&state);
            let active_status = statuses
                .iter()
                .find(|status| status.connected)
                .or(statuses.first());
            let (submitted, pool_accepted, pool_rejected) =
                statuses.iter().fold((0_u64, 0_u64, 0_u64), |acc, status| {
                    (
                        acc.0 + status.shares_submitted,
                        acc.1 + status.shares_accepted,
                        acc.2 + status.shares_rejected,
                    )
                });
            let mut history = state.history.lock().unwrap_or_else(|e| e.into_inner());
            history.samples.push(crate::shared::MinerHistorySample {
                ts_unix_ms: history_now_ms,
                hashrate_ghs: snap.hashrate_5m_ghs,
                hashrate_15s_ghs: snap.hashrate_15s_ghs,
                hashrate_30s_ghs: snap.hashrate_30s_ghs,
                power_w: telem.power_w,
                temp_c: telem.chip_temp_c,
                local_accepted_shares: snap.accepted_shares,
                local_rejected_shares: snap.rejected_shares,
                submitted_shares: submitted,
                pool_accepted_shares: pool_accepted,
                pool_rejected_shares: pool_rejected,
                response_time_ms: active_status
                    .map(|status| status.last_share_response_ms)
                    .unwrap_or(0.0),
                failover_active: active_status
                    .map(|status| status.failover_active)
                    .unwrap_or(false),
                connected: active_status
                    .map(|status| status.connected)
                    .unwrap_or(telem.pool_connected),
            });
            if history.samples.len() > 60 {
                let excess = history.samples.len() - 60;
                history.samples.drain(0..excess);
            }
            let mut events: Vec<dcentaxe_stratum::StratumEventRecord> = statuses
                .iter()
                .flat_map(|status| status.recent_events.clone())
                .collect();
            events.sort_by_key(|event| event.ts_unix_ms);
            if events.len() > 64 {
                events = events.split_off(events.len() - 64);
            }
            history.events = events;
            last_history_sample_ms = history_now_ms;
        }

        // ── Thermal protection (only when mining — bogus readings when ASIC off) ──
        if runtime_mining_active && max_temp > EMERGENCY_TEMP_C {
            error!("THERMAL EMERGENCY {:.0}C — ASIC OFF", max_temp);
            mining_kill.store(true, Ordering::Relaxed);
            {
                let mut telem = state
                    .telemetry
                    .lock()
                    .unwrap_or_else(|err| err.into_inner());
                telem.mining_enabled = false;
                telem.pool_connected = false;
            }
            fan_ctrl.set_speed(100).ok();
            apply_fan_speed_or_fail_closed(
                &mut i2c,
                100,
                &fan_emc2302,
                &temp_emc,
                &emc2103,
                runtime_mining_active,
                &state,
                &mining_kill,
                &mut power_mgr,
                &mut gpio_ctrl,
                "THERMAL EMERGENCY fan pin",
            );
            power_mgr.disable(&mut i2c).ok();
            gpio_ctrl.enable_buck(false).ok();
            display.show_status(
                &mut i2c,
                "THERMAL EMERGENCY!",
                &format!("{:.0}C - ASIC OFF", max_temp),
                &format!("IP: {}", device_ip),
                "Reboot to resume",
            );
            // Thermal auto-recovery: monitor temp and reboot when safe
            let mut thermal_cooldown = 0u32;
            loop {
                // This recovery loop intentionally bypasses the normal main-loop
                // WDT feed while fans stay pinned and ASIC power is off.
                unsafe {
                    let _ = sys::esp_task_wdt_reset();
                }
                std::thread::sleep(Duration::from_secs(5));
                unsafe {
                    let _ = sys::esp_task_wdt_reset();
                }
                // Re-assert fan 100% every cooldown tick. The fan duty was
                // commanded once before this loop; if a transient I2C glitch
                // reset the EMC fan register mid-cooldown, the fan could quietly
                // drop on a still-warm board until reboot. Re-issuing the write
                // self-heals a glitched register while the board cools. ASIC
                // power is already cut here, so a failed re-write needs no
                // further kill action — log and retry next tick.
                fan_ctrl.set_speed(100).ok();
                if let Err(e) = apply_fan_speed(&mut i2c, 100, &fan_emc2302, &temp_emc, &emc2103) {
                    warn!(
                        "Thermal recovery: fan 100% re-assert failed: {} (retry next tick)",
                        e
                    );
                }
                // Re-read current temperatures from live sensors on every recovery tick.
                let cool_temp = [
                    temp_emc
                        .as_mut()
                        .and_then(|s| s.read_external_temp(&mut i2c).ok().flatten())
                        .and_then(|t| apply_temp_offset(Some(t))),
                    apply_temp_offset(
                        emc2103
                            .as_ref()
                            .and_then(|e| e.read_secondary_temp(&mut i2c)),
                    ),
                    apply_temp_offset(emc2103.as_ref().and_then(|e| e.read_chip_temp(&mut i2c))),
                    tmp_primary
                        .as_ref()
                        .and_then(|s| s.read_temp(&mut i2c).ok())
                        .and_then(|t| apply_temp_offset(Some(t))),
                    tmp_secondary
                        .as_ref()
                        .and_then(|s| s.read_temp(&mut i2c).ok())
                        .and_then(|t| apply_temp_offset(Some(t))),
                    temp_emc
                        .as_ref()
                        .and_then(|s| s.read_internal_temp(&mut i2c).ok())
                        .and_then(|t| apply_temp_offset(Some(t))),
                    if power_mgr.has_vreg_temp_sensor() {
                        power_mgr.get_vreg_temp(&mut i2c).ok()
                    } else {
                        None
                    },
                ]
                .into_iter()
                .flatten()
                .fold(None, |acc: Option<f32>, temp| {
                    Some(acc.map_or(temp, |current| current.max(temp)))
                });

                if let Some(cool_temp) = cool_temp {
                    if cool_temp < 75.0 {
                        thermal_cooldown += 1;
                        info!(
                            "Thermal recovery: {:.0}C — cooldown {}/12 (60s needed)",
                            cool_temp, thermal_cooldown
                        );
                        display.show_status(
                            &mut i2c,
                            "COOLING DOWN...",
                            &format!("{:.0}C (need <75C)", cool_temp),
                            &format!("Recovery in {}s", (12 - thermal_cooldown) * 5),
                            &format!("IP: {}", device_ip),
                        );
                        if thermal_cooldown >= 12 {
                            info!("Thermal recovery complete — rebooting to resume mining");
                            display.show_status(
                                &mut i2c,
                                "TEMP OK - REBOOTING",
                                &format!("{:.0}C - safe", cool_temp),
                                "Resuming mining...",
                                "",
                            );
                            std::thread::sleep(Duration::from_secs(2));
                            unsafe {
                                sys::esp_restart();
                            }
                        }
                    } else {
                        thermal_cooldown = 0;
                        error!("Still too hot: {:.0}C — waiting...", cool_temp);
                        display.show_status(
                            &mut i2c,
                            "THERMAL EMERGENCY!",
                            &format!("{:.0}C - TOO HOT", cool_temp),
                            "Waiting to cool...",
                            &format!("IP: {}", device_ip),
                        );
                    }
                } else {
                    thermal_cooldown = 0;
                    display.show_status(
                        &mut i2c,
                        "THERMAL EMERGENCY!",
                        "Sensor data unavailable",
                        "Fan pinned at 100%",
                        &format!("Waiting: {}", device_ip),
                    );
                }
            }
        } else if runtime_mining_active && max_temp > 95.0 {
            // Progressive thermal throttle: reduce frequency to shed heat
            // before reaching the 105°C emergency shutdown.
            // XPSAFE-5: CUT HASH FIRST, then pin the fan. fan=100% is correct and
            // reserved for this 95 C+ band, but the frequency reduction is applied
            // BEFORE the fan bump so heat generation drops first.
            let degrees_over = (max_temp - 95.0).min(10.0);
            let reduction_mhz = degrees_over * 25.0; // 25 MHz per degree above 95
            let configured_target_freq = state
                .config
                .lock()
                .map(|cfg| cfg.target_frequency)
                .unwrap_or(last_applied_freq);
            let throttled_freq = (configured_target_freq - reduction_mhz).max(100.0);
            warn!(
                "Thermal throttle: {:.0}C — reducing freq to {:.0} MHz",
                max_temp, throttled_freq
            );
            if (throttled_freq - last_applied_freq).abs() > 0.1 {
                match freq_cmd_tx.send(throttled_freq) {
                    Ok(()) => last_applied_freq = throttled_freq,
                    Err(e) => error!("Thermal throttle frequency apply failed: {}", e),
                }
            }
            fan_ctrl.set_speed(100).ok();
            apply_fan_speed_or_fail_closed(
                &mut i2c,
                100,
                &fan_emc2302,
                &temp_emc,
                &emc2103,
                runtime_mining_active,
                &state,
                &mining_kill,
                &mut power_mgr,
                &mut gpio_ctrl,
                "THERMAL THROTTLE fan pin",
            );
        } else if runtime_mining_active && max_temp > WARNING_TEMP_C {
            // XPSAFE-5 WARNING tier (>90 C): cut-hash-before-fan-noise (home posture).
            // Shed a modest amount of hash FIRST, then raise the fan only to a
            // home-friendly cap (THERMAL_WARN_FAN_CAP_PCT, NOT 100%). The 95 C tier
            // above escalates to 100% if the temperature keeps climbing.
            let warn_freq = {
                let configured_target_freq = state
                    .config
                    .lock()
                    .map(|cfg| cfg.target_frequency)
                    .unwrap_or(last_applied_freq);
                (configured_target_freq - THERMAL_WARN_FREQ_SHED_MHZ).max(100.0)
            };
            warn!(
                "Thermal warning: {:.0}C — shedding hash to {:.0} MHz then fan to {}%",
                max_temp, warn_freq, THERMAL_WARN_FAN_CAP_PCT
            );
            if (warn_freq - last_applied_freq).abs() > 0.1 {
                match freq_cmd_tx.send(warn_freq) {
                    Ok(()) => last_applied_freq = warn_freq,
                    Err(e) => error!("Thermal warning frequency apply failed: {}", e),
                }
            }
            fan_ctrl.set_speed(THERMAL_WARN_FAN_CAP_PCT).ok();
            apply_fan_speed_or_fail_closed(
                &mut i2c,
                THERMAL_WARN_FAN_CAP_PCT,
                &fan_emc2302,
                &temp_emc,
                &emc2103,
                runtime_mining_active,
                &state,
                &mining_kill,
                &mut power_mgr,
                &mut gpio_ctrl,
                "THERMAL WARNING fan pin",
            );
        }

        // ── Apply config changes from API/MCP ──────────────────────────
        // Note: config lock is acquired multiple times this iteration (~7 times).
        // Each lock costs ~1µs. Total: ~7µs per 5s iteration = negligible.
        // If profiling shows contention, consolidate into a single snapshot here.
        {
            let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            let mut voltage_apply_ok = true;
            // Fan speed changes (skip if auto-control is active)
            // ES-2: never let a user/config fan setpoint undo the die-blind
            // fan-100 override — while die-blind we cannot trust `max_temp` to
            // clear the WARNING gate below, so guard on the flag explicitly.
            if cfg.fan_target_temp_c == 0
                && cfg.fan_speed_pct != fan_ctrl.current_speed()
                && !mining_kill.load(Ordering::Relaxed)
                && any_temp_valid
                && !thermal.die_reading_blind
                && max_temp < WARNING_TEMP_C
            {
                let _ = fan_ctrl.set_speed(cfg.fan_speed_pct);
                apply_fan_speed_or_fail_closed(
                    &mut i2c,
                    fan_ctrl.current_speed(),
                    &fan_emc2302,
                    &temp_emc,
                    &emc2103,
                    runtime_mining_active,
                    &state,
                    &mining_kill,
                    &mut power_mgr,
                    &mut gpio_ctrl,
                    "CONFIG fan pin",
                );
            }
            // Voltage changes (compare against last applied, not immutable boot config)
            if !thermal_clamp_active && cfg.target_voltage_mv != last_applied_voltage {
                match power_mgr.set_voltage(&mut i2c, cfg.target_voltage_mv) {
                    Ok(()) => last_applied_voltage = cfg.target_voltage_mv,
                    Err(e) => {
                        voltage_apply_ok = false;
                        error!(
                            "Config voltage apply failed: {} mV: {}",
                            cfg.target_voltage_mv, e
                        );
                    }
                }
            }
            // Display flip changes
            if cfg.display_inverted != display.inverted {
                let _ = display.set_flip(&mut i2c, cfg.display_inverted);
            }
            // Frequency changes (compare against last applied, not immutable boot config)
            if !thermal_clamp_active && cfg.target_frequency != last_applied_freq {
                let raises_frequency = cfg.target_frequency > last_applied_freq;
                if raises_frequency && !voltage_apply_ok {
                    warn!(
                        "Config frequency increase to {:.0} MHz skipped after voltage apply failure",
                        cfg.target_frequency
                    );
                } else {
                    let _ = freq_cmd_tx.send(cfg.target_frequency);
                    last_applied_freq = cfg.target_frequency;
                }
            }
        }

        // ── Mining toggle from API ──────────────────────────────────────
        // MAINAPI-5: `mining_kill` is ONE-WAY at runtime. Every site in the main
        // loop only ever stores `true` into it (API stop, fan stall, thermal
        // emergency, etc.); nothing ever clears it back to false in the runtime
        // loop. Once the ASIC has been powered off (power_mgr.disable +
        // enable_buck(false)) the ONLY safe re-init path is a full reboot
        // (esp_restart below), which re-runs ASIC bring-up from scratch. Do NOT add
        // a runtime `store(false)` here: resuming the dispatcher against a
        // powered-off ASIC would dispatch work to a dead chip. Recovery is
        // reboot-only by design (this mirrors the thermal-recovery esp_restart).
        let api_mining_enabled = {
            let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            telem.mining_enabled
        };
        if !api_mining_enabled && !mining_kill.load(Ordering::Relaxed) {
            info!("Mining stopped by user via API");
            mining_kill.store(true, Ordering::Relaxed);
            power_mgr.disable(&mut i2c).ok();
            gpio_ctrl.enable_buck(false).ok();
        } else if api_mining_enabled && mining_kill.load(Ordering::Relaxed) {
            // Resume is reboot-only: never clear mining_kill in place.
            info!("Mining resume requested by user — rebooting to reinit ASIC");
            unsafe {
                esp_idf_svc::sys::esp_restart();
            }
        }

        // ── Daily schedule check (profile + autotuner policy switching) ──
        let active_schedule = {
            let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
            if cfg.schedule_enabled && !cfg.power_schedule.is_empty() {
                let (minute_of_day, time_source) = crate::config::schedule_minute_of_day(
                    cfg.schedule_timezone_offset_minutes,
                    uptime,
                );
                crate::config::active_power_schedule(&cfg.power_schedule, minute_of_day)
                    .map(|(idx, entry)| (idx, minute_of_day, time_source, entry.clone()))
            } else {
                None
            }
        };

        if let Some((idx, minute_of_day, time_source, entry)) = active_schedule {
            let schedule_key = format!(
                "{}:{}:{:.1}:{}:{:?}:{:?}:{:?}",
                idx,
                entry.start_minute_of_day(),
                entry.frequency,
                entry.voltage_mv,
                entry.autotune_enabled,
                entry.autotune_mode,
                entry.autotune_target
            );
            let needs_schedule_apply = last_schedule_key.as_deref() != Some(schedule_key.as_str())
                || (entry.frequency - last_applied_freq).abs() > 0.1
                || entry.voltage_mv != last_applied_voltage;
            if needs_schedule_apply && !thermal_clamp_active {
                if schedule_base_point.is_none() {
                    let cfg = state.config.lock().unwrap_or_else(|e| e.into_inner());
                    schedule_base_point = Some((cfg.target_frequency, cfg.target_voltage_mv));
                }
                let label = if entry.label.trim().is_empty() {
                    "scheduled profile"
                } else {
                    entry.label.as_str()
                };

                if let Some(enabled) = entry.autotune_enabled {
                    let mut autotune = state.autotuner.lock().unwrap_or_else(|e| e.into_inner());
                    autotune.enabled = enabled;
                    if enabled {
                        autotune.mode = entry
                            .autotune_mode
                            .as_deref()
                            .and_then(crate::shared::AutotuneMode::from_api_str)
                            .unwrap_or(crate::shared::AutotuneMode::BestEfficiency);
                    }
                    if let Some(target) = entry.autotune_target {
                        autotune.target_value = target;
                    }
                    autotune.status = if enabled {
                        format!("scheduled: {}", label)
                    } else {
                        "scheduled fixed profile".to_string()
                    };
                    info!(
                        "Schedule: autotuner {} for '{}' (mode={:?}, target={:.1})",
                        if enabled { "enabled" } else { "disabled" },
                        label,
                        autotune.mode,
                        autotune.target_value
                    );
                }

                info!(
                    "Schedule: applying '{}' at {:02}:{:02} via {} ({:.0}MHz, {}mV)",
                    label,
                    minute_of_day / 60,
                    minute_of_day % 60,
                    time_source,
                    entry.frequency,
                    entry.voltage_mv
                );
                apply_automatic_operating_point(
                    &state,
                    &mut power_mgr,
                    &mut i2c,
                    &freq_cmd_tx,
                    &mut last_applied_voltage,
                    &mut last_applied_freq,
                    entry.frequency,
                    entry.voltage_mv,
                    ControlSurface::Schedule,
                    "Schedule",
                );
                last_schedule_key = Some(schedule_key);
            }
        } else if last_schedule_key.is_some() && !thermal_clamp_active {
            if let Some((base_freq, base_voltage)) = schedule_base_point.take() {
                info!(
                    "Schedule: inactive — restoring base profile ({:.0}MHz, {}mV)",
                    base_freq, base_voltage
                );
                apply_automatic_operating_point(
                    &state,
                    &mut power_mgr,
                    &mut i2c,
                    &freq_cmd_tx,
                    &mut last_applied_voltage,
                    &mut last_applied_freq,
                    base_freq,
                    base_voltage,
                    ControlSurface::Schedule,
                    "Schedule",
                );
            }
            last_schedule_key = None;
        }

        // ── Autotuner tick ─────────────────────────────────────────────
        if runtime_mining_active && any_temp_valid && !thermal_clamp_active {
            if let Some((new_freq, new_voltage)) = autotuner.tick(&state) {
                apply_automatic_operating_point(
                    &state,
                    &mut power_mgr,
                    &mut i2c,
                    &freq_cmd_tx,
                    &mut last_applied_voltage,
                    &mut last_applied_freq,
                    new_freq,
                    new_voltage,
                    ControlSurface::Autotuner,
                    "Autotuner",
                );
            }
        }

        // ── Fan auto-control (PID + EMA) ──────────────────────────────
        // Always ingest the current max temp into the EMA filter so we can
        // fall through to `current_output()` for dashboard smoothing even
        // when auto-control is off.
        if any_temp_valid {
            fan_pid.ingest(max_temp);
        }
        let live_fan_target = state
            .config
            .lock()
            .map(|c| c.fan_target_temp_c)
            .unwrap_or(0);
        if live_fan_target > 0
            && runtime_mining_active
            && any_temp_valid
            && max_temp <= WARNING_TEMP_C
        {
            // Tick interval matches the main loop (5 s). ESP-Miner PR #1640 uses
            // 100 ms; our loop is locked to 5 s so the controller is driven with
            // dt_ms=5000 and the integrator clamp prevents windup at this cadence.
            if let Some(pct) = fan_pid.update(live_fan_target as f32, 5_000, 30.0, 100.0) {
                let _ = fan_ctrl.set_speed(pct.round() as u8);
                apply_fan_speed_or_fail_closed(
                    &mut i2c,
                    fan_ctrl.current_speed(),
                    &fan_emc2302,
                    &temp_emc,
                    &emc2103,
                    runtime_mining_active,
                    &state,
                    &mining_kill,
                    &mut power_mgr,
                    &mut gpio_ctrl,
                    "AUTO FAN fan pin",
                );
            }
        } else if live_fan_target == 0
            && runtime_mining_active
            && any_temp_valid
            && max_temp <= WARNING_TEMP_C
        {
            // HALT-10: always-on gentle proportional fan curve for manual/unset
            // (fan_target_temp_c == 0). The explicit PID path above is unchanged;
            // this only runs when the PID is NOT driving the fan. It maps max_temp
            // linearly into [PROP_FAN_MIN_PCT, PROP_FAN_MAX_PCT] between the comfort
            // floor and the WARNING band, giving a continuous ramp instead of a
            // step. It only ever RAISES the fan above the current (possibly manual)
            // setting — never reduces a user's higher command, never below the 20%
            // floor — and the 90 C -> cap / 105 C -> cut backstops still apply
            // (this branch is gated `max_temp <= WARNING_TEMP_C`).
            let span = (PROP_FAN_HIGH_TEMP_C - PROP_FAN_LOW_TEMP_C).max(1.0);
            let frac = ((max_temp - PROP_FAN_LOW_TEMP_C) / span).clamp(0.0, 1.0);
            let prop_pct = (PROP_FAN_MIN_PCT as f32
                + frac * (PROP_FAN_MAX_PCT as f32 - PROP_FAN_MIN_PCT as f32))
                .round() as u8;
            // Respect the 20% mining floor and never reduce a higher manual command.
            let desired = prop_pct.max(20);
            if desired > fan_ctrl.current_speed() {
                let _ = fan_ctrl.set_speed(desired);
                apply_fan_speed_or_fail_closed(
                    &mut i2c,
                    fan_ctrl.current_speed(),
                    &fan_emc2302,
                    &temp_emc,
                    &emc2103,
                    runtime_mining_active,
                    &state,
                    &mining_kill,
                    &mut power_mgr,
                    &mut gpio_ctrl,
                    "PROP FAN fan pin",
                );
            }
        }

        // ── Heartbeat log ──────────────────────────────────────────────
        let watts = power.as_ref().map(|p| p.power_w).unwrap_or(0.0);
        let pwr = power
            .map(|p| {
                format!(
                    "{:.0}mV {:.1}A {:.1}W",
                    p.voltage_mv,
                    p.current_ma / 1000.0,
                    p.power_w
                )
            })
            .unwrap_or_else(|| "N/A".into());
        let (
            hr,
            accepted,
            rejected,
            best_diff,
            clean_jobs,
            block_height,
            streak,
            streak_best,
            sparkline,
            temp_sparkline,
            mood_sparkline,
            mood_now,
        ) = {
            let mut s = state.stats.lock().unwrap_or_else(|e| e.into_inner());
            s.push_hashrate_sample(); // record hashrate for sparkline
            s.push_temp_sample(max_temp); // record temp for thermal sparkline
            let fan_target = state
                .config
                .lock()
                .map(|c| c.fan_target_temp_c as f32)
                .unwrap_or(65.0);
            let mood = s.mood_score(max_temp, fan_target);
            s.push_mood_sample(mood);
            let snap = s.snapshot();
            let ts = s.temp_sparkline();
            let ms = s.mood_sparkline();
            (
                snap.hashrate_5m_ghs,
                snap.accepted_shares,
                snap.rejected_shares,
                snap.best_difficulty,
                snap.clean_jobs_count,
                snap.block_height,
                snap.accept_streak,
                snap.best_streak,
                snap.hashrate_sparkline,
                ts,
                ms,
                mood,
            )
        };
        // Check for new all-time best share difficulty
        let best_ever = {
            let telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            telem.best_diff_ever
        };
        // ── NVS dirty flags for batched writes (1 open per tick max) ──
        let mut nvs_save_best_diff = false;
        let mut nvs_save_achievements = false;
        let mut nvs_save_streak = false;
        let mut nvs_save_lifetime = false;
        let mut nvs_save_cached_diff = false;

        // Flag a cached-diff save whenever the live pool diff drifts meaningfully
        // from the last value we persisted (10 % hysteresis prevents flash wear
        // on vardiff wobble). ESP-Miner PR #1594 parity.
        {
            let live_pool_diff = state
                .telemetry
                .lock()
                .map(|t| t.pool_difficulty)
                .unwrap_or(0.0);
            let pool_connected = state
                .telemetry
                .lock()
                .map(|t| t.pool_connected)
                .unwrap_or(false);
            if pool_connected && live_pool_diff >= 1.0 {
                let drift = (live_pool_diff - cached_pool_difficulty).abs();
                if drift > (cached_pool_difficulty * 0.1).max(1.0) {
                    cached_pool_difficulty = live_pool_diff;
                    nvs_save_cached_diff = true;
                }
            }
        }

        if best_diff > best_ever && best_diff > 0.0 {
            info!(
                "NEW ALL-TIME BEST SHARE: {:.2} (was {:.2})",
                best_diff, best_ever
            );
            let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            telem.best_diff_ever = best_diff;
            best_nonce_diff = best_diff;
            best_nonce_val = (best_diff as u32).wrapping_mul(0x1337);
            nvs_save_best_diff = true;
            // Achievement: Best Day Ever
            if achievements & nvs_config::ACH_BEST_DAY == 0 {
                achievements |= nvs_config::ACH_BEST_DAY;
                display.notify(
                    "** ACHIEVEMENT! **",
                    "Best Day Ever!",
                    "New all-time best!",
                    &format!(
                        "{}/{} unlocked",
                        nvs_config::achievement_count(achievements),
                        nvs_config::ACHIEVEMENT_TOTAL
                    ),
                );
                achievement_flash_tick = 1;
                nvs_save_achievements = true;
            }
        }

        let config_freq = state
            .config
            .lock()
            .map(|c| c.target_frequency)
            .unwrap_or(0.0);
        let telem_heap = state.telemetry.lock().map(|t| t.free_heap).unwrap_or(0);

        info!("up={}s hr={:.1}GH/s pwr=[{}] t={:.0}C fan={}% shares={}/{} freq={:.0} best={:.0} heap={}",
            uptime, hr, pwr, max_temp, fan_ctrl.current_speed(),
            accepted, accepted + rejected,
            config_freq, best_diff, telem_heap);

        // ══════════════════════════════════════════════════════════════
        // BITCOIN MINING HACKER TAMAGOTCHI
        // Your BitAxe is a tiny living mining creature!
        // It eats SHA256 hashes, celebrates shares, learns Bitcoin,
        // and tells its own story on the OLED.
        // ══════════════════════════════════════════════════════════════

        diary_tick += 1;
        let efficiency = if hr > 0.001 {
            watts as f64 / (hr / 1000.0)
        } else {
            0.0
        };

        // ── Chill Miner tracking (consecutive time under 65C) ──
        if max_temp < 65.0 && any_temp_valid {
            chill_miner_secs += 5; // main loop ticks every 5s
        } else {
            chill_miner_secs = 0;
        }

        // ── Power Miser tracking (consecutive time under 10W) ──
        if watts > 0.1 && watts < 10.0 {
            low_power_secs += 5; // main loop ticks every 5s
        } else {
            low_power_secs = 0;
        }

        // ── Streak break detection ──
        if rejected > prev_rejected && prev_rejected > 0 && prev_streak > 5 {
            display.notify(
                "(T_T) STREAK BROKEN!",
                &format!("Was {} in a row!", prev_streak),
                &format!("Record: {}", best_streak_ever.max(streak_best)),
                "Build it back up!",
            );
        }
        prev_rejected = rejected;

        // ── Streak milestones ──
        if streak > prev_streak && streak > 0 {
            if streak == 25 {
                display.notify(
                    "(^_^) 25 STREAK!",
                    "No errors! Smooth!",
                    "Keep it going...",
                    &format!("Record: {}", best_streak_ever.max(streak_best)),
                );
            } else if streak == 50 {
                display.notify(
                    "\\(^o^)/ 50 STREAK!",
                    "FLAWLESS mining!",
                    "Half a century!",
                    "You're unstoppable!",
                );
                // Achievement: Streak Master
                if achievements & nvs_config::ACH_STREAK_50 == 0 {
                    achievements |= nvs_config::ACH_STREAK_50;
                    display.notify(
                        "** ACHIEVEMENT! **",
                        "Streak Master!",
                        "50 shares, 0 errors",
                        &format!(
                            "{}/{} unlocked",
                            nvs_config::achievement_count(achievements),
                            nvs_config::ACHIEVEMENT_TOTAL
                        ),
                    );
                    achievement_flash_tick = 1;
                    nvs_save_achievements = true;
                }
            } else if streak == 100 {
                display.notify(
                    "*** 100 STREAK! ***",
                    "(!!!!) LEGENDARY!",
                    "Triple digits baby!",
                    "Screenshot this!",
                );
            }
            // Update best streak in NVS if beaten
            if streak_best > best_streak_ever {
                best_streak_ever = streak_best;
                nvs_save_streak = true;
            }
        }
        prev_streak = streak;

        // ── Achievement checks ──
        // First Share
        if accepted >= 1 && (achievements & nvs_config::ACH_FIRST_SHARE == 0) {
            achievements |= nvs_config::ACH_FIRST_SHARE;
            display.notify(
                "** ACHIEVEMENT! **",
                "First Share!",
                "Your mining journey",
                "has begun!",
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Centurion
        if accepted >= 100 && (achievements & nvs_config::ACH_CENTURION == 0) {
            achievements |= nvs_config::ACH_CENTURION;
            display.notify(
                "** ACHIEVEMENT! **",
                "Centurion!",
                "100 shares accepted!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Kilohash
        if accepted >= 1000 && (achievements & nvs_config::ACH_KILOHASH == 0) {
            achievements |= nvs_config::ACH_KILOHASH;
            display.notify(
                "** ACHIEVEMENT! **",
                "KILOHASH!",
                "1000 shares!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Marathon (24 hours)
        if uptime >= 86400 && (achievements & nvs_config::ACH_MARATHON == 0) {
            achievements |= nvs_config::ACH_MARATHON;
            display.notify(
                "** ACHIEVEMENT! **",
                "Marathon Runner!",
                "24 hours non-stop!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Hot Stuff (survived thermal warning)
        if max_temp > WARNING_TEMP_C && (achievements & nvs_config::ACH_HOT_STUFF == 0) {
            achievements |= nvs_config::ACH_HOT_STUFF;
            display.notify(
                "** ACHIEVEMENT! **",
                "Hot Stuff!",
                "Survived a thermal!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Block Witness (100 blocks)
        if clean_jobs >= 100 && (achievements & nvs_config::ACH_BLOCK_WITNESS == 0) {
            achievements |= nvs_config::ACH_BLOCK_WITNESS;
            display.notify(
                "** ACHIEVEMENT! **",
                "Block Witness!",
                "100 blocks witnessed!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Best Day Ever (checked above in best_diff section)

        // ── New v4 achievements ──
        // Speed Demon (hardware-relative threshold: ~80% of rated peak GH/s)
        if hr > speed_demon_threshold && (achievements & nvs_config::ACH_SPEED_DEMON == 0) {
            achievements |= nvs_config::ACH_SPEED_DEMON;
            display.notify(
                "** ACHIEVEMENT! **",
                "TERAHASH CLUB!",
                "Over 1 TH/s!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Hash King (10% above speed demon threshold = ~90% of rated peak)
        let hash_king_threshold: f64 = speed_demon_threshold * 1.1;
        if hr > hash_king_threshold && (achievements & nvs_config::ACH_HASH_KING == 0) {
            achievements |= nvs_config::ACH_HASH_KING;
            display.notify(
                "** ACHIEVEMENT! **",
                "Hash King!",
                "Peak performance!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Chill Miner (1 hour under 65C)
        if chill_miner_secs >= 3600 && (achievements & nvs_config::ACH_CHILL_MINER == 0) {
            achievements |= nvs_config::ACH_CHILL_MINER;
            display.notify(
                "** ACHIEVEMENT! **",
                "Cool & Collected!",
                "1 hour under 65C!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Diff Hunter (share diff > 1M)
        if best_diff > 1_000_000.0 && (achievements & nvs_config::ACH_DIFF_HUNTER == 0) {
            achievements |= nvs_config::ACH_DIFF_HUNTER;
            display.notify(
                "** ACHIEVEMENT! **",
                "Million Diff Club!",
                "Diff > 1,000,000!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Night Owl (16+ hours uptime)
        if uptime >= 57600 && (achievements & nvs_config::ACH_NIGHT_OWL == 0) {
            achievements |= nvs_config::ACH_NIGHT_OWL;
            display.notify(
                "** ACHIEVEMENT! **",
                "Night Owl!",
                "16h+ night shift!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Half-K (500 shares)
        if accepted >= 500 && (achievements & nvs_config::ACH_HALF_K == 0) {
            achievements |= nvs_config::ACH_HALF_K;
            display.notify(
                "** ACHIEVEMENT! **",
                "Half-K!",
                "500 shares accepted!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Warm Day (8 hours continuous mining)
        if uptime >= 28800 && (achievements & nvs_config::ACH_WARM_DAY == 0) {
            achievements |= nvs_config::ACH_WARM_DAY;
            display.notify(
                "** ACHIEVEMENT! **",
                "Warm Day!",
                "8 hours non-stop!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Streak 100 (100 shares without rejection)
        if streak >= 100 && (achievements & nvs_config::ACH_STREAK_100 == 0) {
            achievements |= nvs_config::ACH_STREAK_100;
            display.notify(
                "** ACHIEVEMENT! **",
                "Perfect Century!",
                "100 in a row!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }

        // ── v5 achievements (bits 16-23) ──
        // Early Adopter (auto-grant ~30s after first boot)
        if uptime > 30 && (achievements & nvs_config::ACH_EARLY_ADOPTER == 0) {
            achievements |= nvs_config::ACH_EARLY_ADOPTER;
            display.notify(
                "** ACHIEVEMENT! **",
                "Early Adopter!",
                "DCENT_axe pioneer!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Efficiency Expert (< 20 J/TH sustained for 10 minutes)
        if efficiency > 0.1
            && efficiency < 20.0
            && uptime > 600
            && (achievements & nvs_config::ACH_EFFICIENCY == 0)
        {
            achievements |= nvs_config::ACH_EFFICIENCY;
            display.notify(
                "** ACHIEVEMENT! **",
                "Efficiency Expert!",
                "Under 20 J/TH!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Diamond Hands (7 days = 604800s continuous uptime)
        if uptime >= 604800 && (achievements & nvs_config::ACH_DIAMOND_HANDS == 0) {
            achievements |= nvs_config::ACH_DIAMOND_HANDS;
            display.notify(
                "** ACHIEVEMENT! **",
                "Diamond Hands!",
                "7 days straight!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Lucky Strike (share diff > 10M)
        if best_diff > 10_000_000.0 && (achievements & nvs_config::ACH_LUCKY_STRIKE == 0) {
            achievements |= nvs_config::ACH_LUCKY_STRIKE;
            display.notify(
                "** ACHIEVEMENT! **",
                "Lucky Strike!",
                "10M diff nonce!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Power Miser (under 10W for 1 hour)
        if low_power_secs >= 3600 && (achievements & nvs_config::ACH_POWER_MISER == 0) {
            achievements |= nvs_config::ACH_POWER_MISER;
            display.notify(
                "** ACHIEVEMENT! **",
                "Power Miser!",
                "1hr under 10W!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Block Party (1000 blocks witnessed)
        if clean_jobs >= 1000 && (achievements & nvs_config::ACH_BLOCK_PARTY == 0) {
            achievements |= nvs_config::ACH_BLOCK_PARTY;
            display.notify(
                "** ACHIEVEMENT! **",
                "Block Party!",
                "1000 blocks seen!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Creature Legend (10K+ lifetime shares = Legend evolution tier)
        if lifetime_shares >= 10000 && (achievements & nvs_config::ACH_CREATURE_LEGEND == 0) {
            achievements |= nvs_config::ACH_CREATURE_LEGEND;
            display.notify(
                "** ACHIEVEMENT! **",
                "Creature Legend!",
                "10K lifetime!",
                &format!(
                    "{}/{} unlocked",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }
        // Completionist (all other 23 achievements unlocked)
        let all_others: u32 = 0x007F_FFFFu32; // bits 0-22
        if (achievements & all_others) == all_others
            && (achievements & nvs_config::ACH_COMPLETIONIST == 0)
        {
            achievements |= nvs_config::ACH_COMPLETIONIST;
            display.notify(
                "*** COMPLETIONIST ***",
                "ALL achievements!",
                "You are LEGENDARY!",
                &format!(
                    "{}/{} COMPLETE!",
                    nvs_config::achievement_count(achievements),
                    nvs_config::ACHIEVEMENT_TOTAL
                ),
            );
            achievement_flash_tick = 1;
            nvs_save_achievements = true;
        }

        // ── Daily Mining Summary (once after 24h) ──
        if uptime >= 86400 && !daily_summary_shown && accepted > 0 {
            daily_summary_shown = true;
            let eff_grade = if efficiency < 20.0 {
                "S+"
            } else if efficiency < 30.0 {
                "A"
            } else if efficiency < 50.0 {
                "B"
            } else if efficiency < 80.0 {
                "C"
            } else {
                "D"
            };
            display.notify(
                "DAY 1 COMPLETE!",
                &format!("{} shares mined", accepted),
                &format!("Best diff: {}", fmt_num(best_diff as u64)),
                &format!("Grade: {} ({:.0}J/TH)", eff_grade, efficiency),
            );
        }

        // ── Mining Diary: rare companion flavor (every ~144 ticks = ~12 min) ──
        if diary_tick % 144 == 0 && uptime > 60 && accepted > 0 {
            let energy_wh = watts as f64 * (uptime as f64 / 3600.0);
            let cups_of_coffee = (energy_wh / 100.0) as u64; // ~100 Wh per cup
            let eff_grade = if efficiency < 20.0 {
                "S+"
            } else if efficiency < 30.0 {
                "A"
            } else if efficiency < 50.0 {
                "B"
            } else if efficiency < 80.0 {
                "C"
            } else {
                "D"
            };
            let uptime_hrs = uptime / 3600;
            let uptime_mins = (uptime % 3600) / 60;

            match (diary_tick / 144) % 16 {
                0 => display.notify(
                    "(^_~) MINING DIARY",
                    &format!("{} shares today!", accepted),
                    &format!("{:.1} Wh consumed", energy_wh),
                    &format!("= {} cups of coffee", cups_of_coffee.max(1)),
                ),
                1 => display.notify(
                    "(^_^) FUN FACT!",
                    &format!("{:.1} J/TH efficiency", efficiency),
                    &format!("Grade: {} silicon!", eff_grade),
                    "How's your chip?",
                ),
                2 => display.notify(
                    "(*_*) SHARE WORK",
                    &format!("{} accepted", fmt_num(accepted)),
                    &format!("Best: {}", fmt_num(best_diff as u64)),
                    "Proof, not guesses",
                ),
                3 => display.notify(
                    "(^v^) UPTIME!",
                    &format!("{}h {}m so far!", uptime_hrs, uptime_mins),
                    "That's dedication!",
                    "Your miner thanks u",
                ),
                4 => display.notify(
                    "(o_o) NONCE REPORT",
                    &format!("Best: {}", fmt_num(best_diff as u64)),
                    &format!("ATH: {}", fmt_num(best_ever as u64)),
                    "Keep hunting!",
                ),
                5 => display.notify(
                    "(B) SHARE COUNTER",
                    &format!("OK:{} ER:{}", fmt_num(accepted), rejected),
                    &format!("Streak:{}", streak),
                    "Pool-confirmed work",
                ),
                6 => display.notify(
                    "[>_]# HACKER LOG",
                    &format!("PID:{} temp:{:.0}C", accepted, max_temp),
                    &format!("fan:{}% pwr:{:.1}W", fan_ctrl.current_speed(), watts),
                    "All systems nominal",
                ),
                7 => {
                    // Hashrate fun comparison
                    let comp = HASHRATE_COMPARISONS[comparison_idx % HASHRATE_COMPARISONS.len()];
                    let val: u64 = match comparison_idx % HASHRATE_COMPARISONS.len() {
                        0 => (hr * 1_000_000_000.0 / 4_194_304.0) as u64, // Game Boy = ~4 MH/s
                        1 => (hr * 1_000_000_000.0 / 5_000_000.0) as u64, // Satoshi ~5 MH/s
                        2 => (hr * 1_000_000_000.0 * 0.3) as u64,         // 300ms blink
                        3 => 0,                                           // special
                        4 => (hr * 1_000_000_000.0 * 0.8) as u64,         // 800ms heartbeat
                        5 => (hr * 1_000_000_000.0 / 93_750_000.0) as u64, // N64 ~93 MIPS
                        6 => (hr * 1_000_000_000.0 / 56_000.0) as u64,    // 56k modem
                        _ => (hr * 1_000_000_000.0 / 10.0) as u64,        // pocket calc
                    };
                    if comparison_idx % HASHRATE_COMPARISONS.len() == 3 {
                        display.notify(
                            "(*_*) YOUR HASHRATE",
                            &fmt_hashrate(hr),
                            &format!("Best diff {}", fmt_num(best_diff as u64)),
                            "Local telemetry",
                        );
                    } else {
                        display.notify(
                            "(*_*) YOUR HASHRATE",
                            &format!("= {} {}", fmt_num(val), comp),
                            "Think about THAT!",
                            "Mind = blown!",
                        );
                    }
                    comparison_idx += 1;
                }
                8 => {
                    // Fortune cookie
                    let fortune = FORTUNE_COOKIES[fortune_idx % FORTUNE_COOKIES.len()];
                    display.notify(
                        "(^_~) FORTUNE COOKIE",
                        &format!("\"{}\"", fortune),
                        "",
                        "Mine on, friend!",
                    );
                    fortune_idx += 1;
                }
                9 => {
                    // Evolution status
                    let (stage, face) = nvs_config::evolution_stage(lifetime_shares);
                    let next_threshold: u32 = match lifetime_shares {
                        0 => 1,
                        1..=99 => 100,
                        100..=999 => 1000,
                        1000..=4999 => 5000,
                        5000..=9999 => 10000,
                        _ => 0, // maxed
                    };
                    if next_threshold > 0 {
                        let need = next_threshold.saturating_sub(lifetime_shares);
                        display.notify(
                            &format!("{} {}", face, stage),
                            &format!("Lifetime: {}", fmt_num(lifetime_shares as u64)),
                            &format!("Next: {}sh to evolve", fmt_num(need as u64)),
                            &format!(
                                "{:.0}% there!",
                                (lifetime_shares as f32 / next_threshold as f32) * 100.0
                            ),
                        );
                    } else {
                        display.notify(
                            &format!("{} LEGEND!", face),
                            &format!("{} lifetime!", fmt_num(lifetime_shares as u64)),
                            "MAX EVOLUTION!",
                            "Absolute unit!",
                        );
                    }
                }
                10 => display.notify(
                    "(^_^) YOUR BITAXE",
                    "is the cutest miner",
                    "on the network!",
                    "No one else is close",
                ),
                11 => {
                    // Hall of fame peek
                    if best_nonce_diff > 0.0 {
                        display.notify(
                            "HALL OF FAME",
                            &format!("Best: 0x{:08X}", best_nonce_val),
                            &format!("Diff: {}", fmt_num(best_nonce_diff as u64)),
                            "Your proof of work!",
                        );
                    } else {
                        display.notify(
                            "HALL OF FAME",
                            "No best nonce yet!",
                            "Keep mining to find",
                            "your personal best!",
                        );
                    }
                }
                // New v4 creature idle chatter
                12 => display.notify(
                    "(^_^) CREATURE CHAT",
                    "I love warm hashes",
                    "*purrs in SHA256*",
                    "Feed me more nonces!",
                ),
                13 => display.notify(
                    "(^v^) CREATURE CHAT",
                    "Tick tock next block!",
                    "Is that a new block?",
                    "I dream in hex...",
                ),
                14 => display.notify(
                    "(o_o) CREATURE CHAT",
                    "My bits are tingling!",
                    "Something big is",
                    "coming... I feel it!",
                ),
                _ => display.notify(
                    "(*_*) CREATURE CHAT",
                    "01001000 01001001",
                    "(thats HI in binary)",
                    "Nerd life best life!",
                ),
            };
        }

        // ── Share accepted: the creature is FED! ──
        if accepted > prev_accepted && prev_accepted > 0 {
            share_flash_tick = 4; // companion lightning/eating animation, no popup spam
            if accepted % 25 == 0 {
                display.notify(
                    "SHARE MILESTONE",
                    &format!("{} accepted", accepted),
                    &format!("Streak:{} Best:{}", streak, fmt_num(best_diff as u64)),
                    "Pet fed. Still mining.",
                );
            }
        }
        // ── Lifetime shares + creature evolution ──
        if accepted > prev_accepted && prev_accepted > 0 {
            let new_shares = (accepted - prev_accepted) as u32;
            let old_lifetime = lifetime_shares;
            lifetime_shares = lifetime_shares.saturating_add(new_shares);

            // Check if evolution stage changed
            let (old_stage, _) = nvs_config::evolution_stage(old_lifetime);
            let (new_stage, new_face) = nvs_config::evolution_stage(lifetime_shares);
            if old_stage != new_stage {
                display.notify(
                    "*** EVOLUTION! ***",
                    &format!("{} -> {}", old_stage, new_stage),
                    &format!("{}", new_face),
                    &format!("{} lifetime!", fmt_num(lifetime_shares as u64)),
                );
                // Non-blocking double flash for evolution (compound if already flashing)
                pending_flash = pending_flash.saturating_add(4);
            }

            // Save lifetime shares every 10 new shares (reduce flash wear)
            if lifetime_shares / 10 > old_lifetime / 10 {
                nvs_save_lifetime = true;
            }
        }

        // ── Special block easter eggs ──
        if block_height > 0 && block_height != prev_block_height {
            if let Some(special_msg) = is_special_block(block_height) {
                display.notify(
                    &format!("** {} **", special_msg),
                    &format!("Block #{}", block_height),
                    "RARE BLOCK EVENT!",
                    "Screenshot this!",
                );
                // Non-blocking strobe effect for special blocks (compound)
                pending_flash = pending_flash.saturating_add(3);
            }
            prev_block_height = block_height;
        }

        // ── New best difficulty: creature is ECSTATIC! ──
        if best_diff > prev_best_diff && prev_best_diff > 0.0 && best_diff > 1000.0 {
            match (best_diff as u64) % 3 {
                0 => display.notify(
                    "(!!!) NEW RECORD!!!",
                    &format!("(*o*) Diff: {:.0}", best_diff),
                    &format!("Was: {:.0}", prev_best_diff),
                    "Im the GOAT miner!!",
                ),
                1 => display.notify(
                    "\\(!!!!)/ RECORD!",
                    &format!("Diff: {:.0}!", best_diff),
                    "Thats my best EVER!",
                    "Screenshot this!",
                ),
                _ => display.notify(
                    "(!!!!) HOLY NONCE!",
                    &format!("{:.0} difficulty!", best_diff),
                    "New personal best!",
                    "Chad miner energy!",
                ),
            };
        }
        prev_best_diff = best_diff;

        // ── Milestone celebrations ──
        if accepted > 0 && accepted % 100 == 0 && accepted > prev_accepted {
            match (accepted / 100) % 4 {
                0 => display.notify(
                    &format!("*** {} SHARES! ***", accepted),
                    "\\(^o^)/ LEVEL UP!",
                    "Achievement unlocked!",
                    "Stack sats. Stay humb",
                ),
                1 => display.notify(
                    &format!("*** {} SHARES! ***", accepted),
                    "(^_^)b CENTURY!",
                    "100 more shares done!",
                    "WAGMI!",
                ),
                2 => display.notify(
                    &format!("*** {} SHARES! ***", accepted),
                    "(*_*) MILESTONE!",
                    "Mining is a marathon",
                    "not a sprint!",
                ),
                _ => display.notify(
                    &format!("*** {} SHARES! ***", accepted),
                    "(/^o^)/ LEGEND!",
                    "Still hashing!",
                    "Tick tock next block!",
                ),
            };
        }

        prev_accepted = accepted;

        // ── Batched NVS write (1 open max per tick instead of 6) ──
        if nvs_save_best_diff
            || nvs_save_achievements
            || nvs_save_streak
            || nvs_save_lifetime
            || nvs_save_cached_diff
        {
            let nvs_p = nvs_partition.clone();
            if let Ok(mut nvs) = esp_idf_svc::nvs::EspNvs::<esp_idf_svc::nvs::NvsDefault>::new(
                nvs_p, "dcentaxe", true,
            ) {
                if nvs_save_best_diff {
                    nvs_config::save_best_diff(&mut nvs, best_diff);
                    nvs_config::save_best_nonce(&mut nvs, best_nonce_val, best_nonce_diff);
                }
                if nvs_save_achievements {
                    nvs_config::save_achievements(&mut nvs, achievements);
                }
                if nvs_save_streak {
                    nvs_config::save_best_streak(&mut nvs, best_streak_ever);
                }
                if nvs_save_lifetime {
                    nvs_config::save_lifetime_shares(&mut nvs, lifetime_shares);
                }
                if nvs_save_cached_diff {
                    nvs_config::save_cached_pool_difficulty(&mut nvs, cached_pool_difficulty);
                }
            }
        }

        // ── Update achievement/creature telemetry for API ──
        {
            let mut telem = state.telemetry.lock().unwrap_or_else(|e| e.into_inner());
            telem.achievements = achievements;
            telem.achievement_count = nvs_config::achievement_count(achievements);
            telem.lifetime_shares = lifetime_shares;
            let (stage_name, _) = nvs_config::evolution_stage(lifetime_shares);
            telem.creature_stage = stage_name.to_string();
            telem.creature_mood = mood_now;
        }

        // ── New block on the network! ──
        if clean_jobs > prev_clean_jobs && prev_clean_jobs > 0 {
            let blk = if block_height > 1 {
                format!("Block #{}", block_height - 1)
            } else {
                "New block found!".to_string()
            };
            display.notify(
                "NEW BLOCK",
                &blk,
                "Fresh work from pool",
                &format!("HR {}", fmt_hashrate(hr)),
            );
        }
        prev_clean_jobs = clean_jobs;

        // ── Uptime milestones — you're part of something bigger! ──
        let uptime_mins = uptime / 60;
        if uptime_mins > 0 && uptime_mins % 60 == 0 && uptime % 60 < 5 {
            // Every hour celebration
            let hours = uptime_mins / 60;
            match hours {
                1 => display.notify(
                    "(^_^) 1 HOUR!",
                    "You've been mining",
                    "for a whole hour!",
                    "Legend in the making",
                ),
                _ => display.notify(
                    &format!("(^_^) {} HOURS!", hours),
                    "Still hashing strong!",
                    &format!("{} shares this run", accepted),
                    "Dedication!",
                ),
            };
        }

        // ── Random Bitcoin wisdom (rare: about every 10 minutes) ──
        if uptime > 120 && (uptime * 7 + accepted) % 120 == 0 {
            match (uptime / 5) % 10 {
                0 => display.notify(
                    "(^_~) BITCOIN WISDOM",
                    "\"Running Bitcoin\"",
                    "- Hal Finney, 2009",
                    "And so are you!",
                ),
                1 => display.notify(
                    "(^_^) YOU MATTER!",
                    "Every hash you mine",
                    "makes Bitcoin more",
                    "decentralized!",
                ),
                2 => display.notify(
                    "(*_*) FUN FACT:",
                    "Your BitAxe does 1T",
                    "hashes per second!",
                    "Thats 1 TRILLION/s!",
                ),
                3 => display.notify(
                    "(o_o) THINK ABOUT IT",
                    "You are literally",
                    "writing history in",
                    "the blockchain!",
                ),
                4 => display.notify(
                    "{^_^} CYPHERPUNK!",
                    "Your miner. Your",
                    "rules. Your hashes.",
                    "Thats sovereignty!",
                ),
                5 => display.notify(
                    "(^v^) DID YOU KNOW?",
                    "Home miners like you",
                    "are Bitcoin's immune",
                    "system! Stay strong!",
                ),
                6 => display.notify(
                    "(^_^) WARM & HASHING",
                    "Your BitAxe is also",
                    "a space heater!",
                    "Free heat + sats!",
                ),
                7 => display.notify(
                    "(*o*) SOLO MINING:",
                    "Low odds, but if you",
                    "find a block = 3.125",
                    "BTC! Dream big!",
                ),
                8 => display.notify(
                    "(^_~) REMEMBER:",
                    "Not your node,",
                    "not your rules.",
                    "Run your own hash!",
                ),
                _ => display.notify(
                    "(@_@) NERD ALERT:",
                    "SHA256 was designed",
                    "by the NSA in 2001!",
                    "Now WE use it!",
                ),
            };
        }

        // ── OLED display carousel (notifications interleaved, not blocking) ──
        // Urgent safety notifications preempt immediately. Normal notifications are
        // sparse so hashrate/IP/block/companion pages remain the main OLED experience.
        if notification_cooldown > 0 {
            notification_cooldown -= 1;
        }
        let urgent_notification = display.has_urgent_notification();
        if (urgent_notification || notification_cooldown == 0)
            && display.show_notification_if_pending(&mut i2c)
        {
            display.set_contrast(&mut i2c, 0xFF);
            display.invert_display(&mut i2c, true);
            std::thread::sleep(Duration::from_millis(60));
            display.invert_display(&mut i2c, false);
            display.set_contrast(&mut i2c, 0xCF);
            gpio_ctrl.toggle_led().ok();
            notification_cooldown = if urgent_notification { 2 } else { 5 };
            continue;
        }
        let autotuner_enabled = state.autotuner.lock().map(|at| at.enabled).unwrap_or(false);
        // Carousel uses an anchor pattern: hashrate, IP, block, and companion repeat
        // frequently. Trivia/education pages are demoted so the OLED stays useful.
        // Logical pages: 0=mining, 1=device/IP, 2=weather, 3=halving, 4=autotuner,
        //                5=sats, 6=hall of fame, 7=companion (full-screen)
        let carousel: &[u8] = if autotuner_enabled {
            &[0, 7, 1, 0, 4, 7, 0, 2, 1, 7, 0, 2]
        } else {
            &[0, 7, 1, 0, 2, 7, 0, 1, 7, 5, 2, 6]
        };
        let page_count = carousel.len() as u8;
        let logical_page = carousel[display_page as usize % carousel.len()];

        // ── Mining Creature with Evolution + Activity Poses ──
        // Evolution stage provides the base face, activity overrides for special states
        let (evo_stage_name, evo_face) = nvs_config::evolution_stage(lifetime_shares);
        let creature = if !mining_enabled {
            "(x_x)" // dead — no ASIC
        } else if max_temp > 85.0 {
            "(>_<)!!!" // overheating — overrides evolution
        } else if hr < 1.0 && uptime > 30 {
            "(-_-)zzZ" // sleeping (low hashrate)
        } else if accepted > 0 && (uptime % 10) < 3 {
            "(^_^)~~#" // eating hash (alternates every few ticks)
        } else if streak > 20 {
            "\\(^o^)/" // celebrating (big streak)
        } else if pixel_art_frame % 2 == 0 {
            evo_face // eyes open (breathing animation)
        } else {
            "(-.-)" // eyes closed (idle breathing)
        };

        // Select pixel art sprite based on mining state (lightning/rocket override)
        let active_sprite = if share_flash_tick > 0 {
            &SPRITE_LIGHTNING // flash after share accepted
        } else if achievement_flash_tick > 0 {
            &SPRITE_ROCKET // flash after achievement unlock
        } else if streak > 10 && (uptime % 4) < 2 {
            // Good streak — spinning coin celebration
            if (uptime / 2) % 2 == 0 {
                &SPRITE_COIN_1
            } else {
                &SPRITE_COIN_2
            }
        } else if hr > 10.0 {
            if pixel_art_frame % 2 == 0 {
                &SPRITE_PICKAXE_1
            } else {
                &SPRITE_PICKAXE_2
            }
        } else if max_temp > 70.0 {
            &SPRITE_CAMPFIRE
        } else {
            if (uptime / 3) % 3 == 0 {
                &SPRITE_COIN_1
            } else if (uptime / 3) % 3 == 1 {
                &SPRITE_COIN_2
            } else {
                &SPRITE_COIN_3
            }
        };
        pixel_art_frame = pixel_art_frame.wrapping_add(1);

        // ── Day/Night cycle (based on uptime) ──
        let day_phase = match uptime / 3600 {
            0..=1 => "morning",   // boot to 2h
            2..=9 => "day",       // 2h-10h
            10..=15 => "evening", // 10h-16h
            _ => "night",         // 16h+
        };

        // Screen transition flash effect (only on actual page change)
        if last_transition_flash {
            display.invert_display(&mut i2c, true);
            std::thread::sleep(Duration::from_millis(30));
            display.invert_display(&mut i2c, false);
            last_transition_flash = false;
        }
        if logical_page != 7 {
            display.set_contrast(&mut i2c, 0xCF);
        }

        match logical_page {
            0 => {
                // Page 0: Mining HUD. Keep this dense enough to be useful, but not noisy.
                let temp_str = if any_temp_valid {
                    format!("{:.0}C", max_temp)
                } else {
                    "--C".to_string()
                };
                let hr_str = fmt_hashrate(hr);

                display.clear();
                // Draw pixel art sprite in top-left corner (8x8)
                display.draw_bitmap(0, 0, active_sprite, 8, 8);
                // Pickaxe debris particles — pseudo-random sparkle near sprite when mining
                if hr > 10.0 {
                    let seed = uptime as usize;
                    display.set_pixel(9 + (seed * 7) % 5, (seed * 13) % 6, true);
                    display.set_pixel(10 + (seed * 11) % 4, 2 + (seed * 3) % 5, true);
                    if seed % 3 == 0 {
                        display.set_pixel(8 + (seed * 5) % 6, 1 + (seed * 9) % 5, true);
                    }
                }
                // Line 0: hashrate + temp. Full stage/lifetime lives on companion pages.
                display.draw_text(10, 0, &format!("{} {}", hr_str, temp_str));

                // Line 1: Hashrate sparkline (16 samples, 2px wide each = 32px)
                let max_hr = sparkline.iter().cloned().fold(0.0_f32, f32::max).max(1.0);
                let mut norm = [0.0f32; 16];
                for i in 0..16 {
                    norm[i] = sparkline[i] / max_hr;
                }
                display.draw_sparkline(0, 8, &norm, 2);
                display.draw_text(36, 8, &format!("{:.1}W {:.0}J/T", watts, efficiency));

                // Line 2: shares + streak/errors + best diff (21 chars max)
                let diff_s = fmt_num(best_diff as u64);
                if streak > 5 {
                    display.draw_text(
                        0,
                        16,
                        &format!("OK:{} stk:{} d:{}", fmt_num(accepted), streak, diff_s),
                    );
                } else {
                    display.draw_text(
                        0,
                        16,
                        &format!("OK:{} ER:{} d:{}", fmt_num(accepted), rejected, diff_s),
                    );
                }

                // Line 3: companion state + current block, the two most requested anchors.
                let pet_state = if !runtime_mining_active {
                    "OFF"
                } else if max_temp > 85.0 {
                    "HOT"
                } else if hr < 1.0 && uptime > 30 {
                    "SLEEP"
                } else if share_flash_tick > 0 {
                    "FED"
                } else if day_phase == "night" {
                    "NITE"
                } else {
                    "HASH"
                };
                let block_short = if block_height > 0 {
                    format!("B:{}", block_height)
                } else {
                    "B:---".to_string()
                };
                display.draw_text(
                    0,
                    24,
                    &format!("{} {} {}", creature, pet_state, block_short),
                );

                let _ = display.flush(&mut i2c);
            }
            1 => {
                // Page 1: Device/IP. Prioritize fields needed at the physical miner.
                let uptime_str = if uptime >= 3600 {
                    format!("up:{}h{}m", uptime / 3600, (uptime % 3600) / 60)
                } else {
                    format!("up:{}m", uptime / 60)
                };
                let wifi_rssi = state.telemetry.lock().map(|t| t.wifi_rssi).unwrap_or(0);
                let pool_short: String = pool_display.chars().take(21).collect();
                let block_line = if block_height > 0 {
                    format!("v{} BLK:{}", VERSION, block_height)
                } else {
                    format!("v{} BLK:---", VERSION)
                };
                display.show_status(
                    &mut i2c,
                    &device_ip,
                    &pool_short,
                    &format!("WiFi:{}dB {}", wifi_rssi, uptime_str),
                    &block_line,
                );
            }
            2 => {
                // Page 2: Block Progress + Temperature Weather
                let block_str = if block_height > 0 {
                    format!("BLOCK #{}", block_height)
                } else {
                    "BLOCK #???".to_string()
                };
                let progress = ((uptime % 600) as f32) / 600.0;

                let weather = if max_temp > 85.0 {
                    "SCORCHING!"
                } else if max_temp > 70.0 {
                    "Warm & Toasty"
                } else if max_temp > 55.0 {
                    "Perfect Weather"
                } else if max_temp > 40.0 {
                    "Cool Breeze"
                } else {
                    "Freezing!"
                };
                let forecast = if max_temp > 70.0 { "Fan:MAX" } else { "Stable" };

                display.clear();
                display.draw_centered(0, &block_str);
                display.draw_progress_bar(2, 9, 80, progress);
                display.draw_text(86, 8, &format!("{:.0}%", progress * 100.0));
                display.draw_centered(16, &format!("{} {:.0}C", weather, max_temp));
                display.draw_text(
                    0,
                    24,
                    &format!("Fan:{}% {}", fan_ctrl.current_speed(), forecast),
                );
                let _ = display.flush(&mut i2c);
            }
            3 => {
                // Page 3: Halving Countdown + Difficulty Epoch
                if block_height > 0 {
                    let next_halving = ((block_height / 210_000) + 1) * 210_000;
                    let halving_progress = (block_height % 210_000) as f32 / 210_000.0;
                    let blocks_remaining = next_halving - block_height;
                    let days_remaining = blocks_remaining as f32 * 10.0 / 1440.0; // ~10 min/block

                    // Difficulty epoch (2016 blocks)
                    let epoch_num = block_height / 2016;
                    let epoch_progress = (block_height % 2016) as f32 / 2016.0;
                    let epoch_blocks_left = 2016 - (block_height % 2016);
                    let epoch_days = epoch_blocks_left as f32 * 10.0 / 1440.0;

                    display.clear();
                    display.draw_text(
                        0,
                        0,
                        &format!(
                            "HALVING #{} in {}d",
                            next_halving / 210_000,
                            days_remaining as u32
                        ),
                    );
                    display.draw_progress_bar(2, 9, 90, halving_progress);
                    display.draw_text(96, 8, &format!("{:.0}%", halving_progress * 100.0));
                    display.draw_text(
                        0,
                        16,
                        &format!("EPOCH #{} ~{}d left", epoch_num, epoch_days as u32),
                    );
                    display.draw_progress_bar(2, 25, 90, epoch_progress);
                    display.draw_text(96, 24, &format!("{:.0}%", epoch_progress * 100.0));
                    let _ = display.flush(&mut i2c);
                } else {
                    display.show_status(
                        &mut i2c,
                        "HALVING COUNTDOWN",
                        "Waiting for block...",
                        "Need block height",
                        "from pool first",
                    );
                }
            }
            4 if autotuner_enabled => {
                // Page 4: Autotuner (only when enabled)
                let at = state
                    .autotuner
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let at_phase = at.status.chars().take(21).collect::<String>();
                let best_freq = if at.current_frequency > 0.0 {
                    format!(
                        "Best:{:.0}MHz {:.1}J/TH",
                        at.current_frequency, at.best_efficiency
                    )
                } else {
                    "Best: measuring...".to_string()
                };
                let pwr_limit = autotuner.power_limit_display();
                display.show_status(
                    &mut i2c,
                    &format!("AUTOTUNER: {}", at_phase),
                    &format!("Testing {:.0} MHz", config_freq),
                    &best_freq,
                    &format!("Power: {:.1}W / {}", watts, pwr_limit),
                );
            }
            5 => {
                // Share tracker + streak + achievements. Keep this grounded in
                // local pool-confirmed data; no hardcoded network estimates.
                let ach_count = nvs_config::achievement_count(achievements);
                let share_rate = if uptime > 60 {
                    accepted as f64 / (uptime as f64 / 3600.0)
                } else {
                    0.0
                };
                display.show_status(
                    &mut i2c,
                    "SHARE TRACKER",
                    &format!("OK:{} ER:{}", fmt_num(accepted), rejected),
                    &format!("{:.1}/hr best:{}", share_rate, fmt_num(best_diff as u64)),
                    &format!(
                        "Stk:{} rec:{} ach:{}/{}",
                        streak,
                        best_streak_ever.max(streak_best),
                        ach_count,
                        nvs_config::ACHIEVEMENT_TOTAL
                    ),
                );
            }
            6 => {
                // Hall of Fame + Bitcoin Timeline OR Network hashrate
                if (uptime / 5) % 2 == 0 {
                    // Hall of Fame
                    display.clear();
                    display.draw_centered(0, "-- HALL OF FAME --");
                    if best_nonce_diff > 0.0 {
                        display.draw_text(0, 8, &format!("Best: 0x{:08X}", best_nonce_val));
                        display.draw_text(
                            0,
                            16,
                            &format!("Diff: {}", fmt_num(best_nonce_diff as u64)),
                        );
                    } else {
                        display.draw_text(0, 8, "No trophy yet!");
                        display.draw_text(0, 16, "Mine to earn one!");
                    }
                    // Bitcoin timeline: genesis 2009 → last BTC ~2140
                    let timeline_pct = 0.13_f32;
                    display.draw_text(
                        0,
                        24,
                        &format!("BTC Timeline: {:.0}%", timeline_pct * 100.0),
                    );
                    display.draw_progress_bar(78, 25, 48, timeline_pct);
                    let _ = display.flush(&mut i2c);
                } else {
                    // Smarter network context page
                    let tagline = match (uptime / 5) % 6 {
                        1 => "David vs Goliath!",
                        3 => "But you MATTER!",
                        5 => "Decentralization!",
                        _ => "Every hash counts!",
                    };
                    display.clear();
                    display.draw_centered(0, "- YOU vs THE WORLD -");
                    // Draw a bar representing the network, with one bright pixel = you
                    display.draw_hline(2, 14, 124);
                    display.draw_hline(2, 18, 124);
                    display.set_pixel(3, 15, true);
                    display.set_pixel(3, 16, true);
                    display.set_pixel(3, 17, true);
                    display.draw_text(0, 8, "^you");
                    display.draw_text(74, 8, "^600EH/s");
                    display.draw_centered(24, tagline);
                    let _ = display.flush(&mut i2c);
                }
            }
            7 => {
                // Companion page: mostly Tamagotchi pet room, with periodic full-height moments.
                display.clear();

                // Select creature sprite based on mood and blink cycle
                let mood_score = {
                    let fan_target = state
                        .config
                        .lock()
                        .map(|c| c.fan_target_temp_c as f32)
                        .unwrap_or(65.0);
                    let s = state.stats.lock().unwrap_or_else(|e| e.into_inner());
                    s.mood_score(max_temp, fan_target)
                };
                let creature_sprite = if !runtime_mining_active || hr < 1.0 {
                    &CREATURE_SLEEP
                } else if mood_score < 4 || max_temp > 85.0 {
                    &CREATURE_SAD
                } else if share_flash_tick > 0 || uptime % 4 == 0 {
                    &CREATURE_BLINK // blink every 4th tick (~20s)
                } else {
                    &CREATURE_HAPPY
                };

                // Breathing brightness effect (zero CPU — hardware contrast)
                display.set_contrast(
                    &mut i2c,
                    BREATH_LUT[(uptime as usize / 2) % BREATH_LUT.len()],
                );

                // Name and stage (right side)
                let creature_name =
                    nvs_config::creature_name(device_ip.as_bytes().last().copied().unwrap_or(0));
                let hr_str = fmt_hashrate(hr);
                let temp_str = if any_temp_valid {
                    format!("{:.0}C", max_temp)
                } else {
                    "--C".to_string()
                };
                let block_str = if block_height > 0 {
                    format!("BLK:{}", block_height)
                } else {
                    "BLK:---".to_string()
                };
                let activity = if !runtime_mining_active {
                    "Offline"
                } else if max_temp > 85.0 {
                    "TOO HOT"
                } else if hr < 1.0 {
                    "Zzz..."
                } else if share_flash_tick > 0 {
                    "Nom nonce"
                } else if achievement_flash_tick > 0 {
                    "Proud!"
                } else {
                    "Mining hard"
                };

                let full_pet = share_flash_tick > 0
                    || achievement_flash_tick > 0
                    || !runtime_mining_active
                    || max_temp > 85.0
                    || companion_visit % 3 == 2;
                companion_visit = companion_visit.wrapping_add(1);

                if full_pet {
                    // 32px-tall creature owns the screen; right side keeps key status.
                    display.draw_bitmap_2x(0, 0, creature_sprite, 16, 16);
                    display.draw_text(36, 0, creature_name);
                    display.draw_text(36, 8, activity);
                    display.draw_text(36, 16, &format!("{} {}", hr_str, temp_str));
                    display.draw_text(36, 24, &block_str);
                } else {
                    // Pet room: companion is central, but mining stats remain readable.
                    display.draw_bitmap(2, 0, creature_sprite, 16, 16);
                    display.draw_text(22, 0, creature_name);
                    display.draw_text(82, 0, &hr_str);
                    display.draw_text(22, 8, evo_stage_name);
                    display.draw_text(94, 8, &temp_str);

                    // Mood hearts (5 hearts, filled based on mood 0-10)
                    let hearts_filled = (mood_score / 2).min(5);
                    for h in 0..5u8 {
                        let hx = 22 + h as usize * 7;
                        if h < hearts_filled {
                            display.draw_bitmap(hx, 17, &HEART_FILLED, 5, 8);
                        } else {
                            display.draw_bitmap(hx, 17, &HEART_EMPTY, 5, 8);
                        }
                    }
                    display.draw_text(64, 16, &format!("OK:{}", fmt_num(accepted)));
                    display.draw_text(22, 25, activity);
                    display.draw_text(90, 24, &format!("{:.0}W", watts));
                }

                let _ = display.flush(&mut i2c);
            }
            _ => {
                // Screensaver: Bouncing Bitcoin OR Mood/Thermal
                if (uptime / 5) % 2 == 0 {
                    // Bouncing Bitcoin screensaver (like DVD logo)
                    display.clear();
                    // Update position (clamp first, then bounce detect)
                    screensaver_x += screensaver_dx * 3;
                    screensaver_y += screensaver_dy * 2;
                    screensaver_x = screensaver_x.clamp(0, 118);
                    screensaver_y = screensaver_y.clamp(0, 24);
                    if screensaver_x <= 0 || screensaver_x >= 118 {
                        screensaver_dx = -screensaver_dx;
                    }
                    if screensaver_y <= 0 || screensaver_y >= 24 {
                        screensaver_dy = -screensaver_dy;
                    }
                    // Draw Bitcoin coin sprite at bouncing position
                    let coin_frame = if (uptime / 2) % 3 == 0 {
                        &SPRITE_COIN_1
                    } else if (uptime / 2) % 3 == 1 {
                        &SPRITE_COIN_2
                    } else {
                        &SPRITE_COIN_3
                    };
                    display.draw_bitmap(
                        screensaver_x as usize,
                        screensaver_y as usize,
                        coin_frame,
                        8,
                        8,
                    );
                    // Small text overlay
                    display.draw_text(0, 0, "BTC");
                    let _ = display.flush(&mut i2c);
                } else {
                    // Mood + Thermal sparkline page (dual stacked sparklines)
                    let mood_label = match mood_now {
                        9..=10 => "ECSTATIC",
                        7..=8 => "HAPPY",
                        5..=6 => "CONTENT",
                        3..=4 => "MEH",
                        1..=2 => "SAD",
                        _ => "DEAD",
                    };
                    let trend = if temp_sparkline[15] > temp_sparkline[12] + 2.0 {
                        "RISING"
                    } else if temp_sparkline[15] < temp_sparkline[12] - 2.0 {
                        "FALLING"
                    } else {
                        "STABLE"
                    };
                    display.clear();
                    display.draw_text(0, 0, &format!("MOOD:{} {}/10", mood_label, mood_now));

                    // Mood sparkline (normalize 0-10 to 0.0-1.0)
                    let mut mood_norm = [0.0f32; 16];
                    for i in 0..16 {
                        mood_norm[i] = mood_sparkline[i] as f32 / 10.0;
                    }
                    display.draw_sparkline(0, 8, &mood_norm, 2);

                    display.draw_text(0, 16, &format!("TEMP:{:.0}C {}", max_temp, trend));

                    // Thermal sparkline
                    let temp_max = temp_sparkline
                        .iter()
                        .cloned()
                        .fold(0.0_f32, f32::max)
                        .max(1.0);
                    let mut temp_norm = [0.0f32; 16];
                    for i in 0..16 {
                        temp_norm[i] = temp_sparkline[i] / temp_max;
                    }
                    display.draw_sparkline(0, 24, &temp_norm, 2);

                    let _ = display.flush(&mut i2c);
                }
            }
        }
        let next_page = (display_page + 1) % page_count;
        // Only flash on full-rotation wrap (return to page 0), not every page change
        if next_page == 0 && display_page != 0 {
            last_transition_flash = true;
        }
        display_page = next_page;

        // ── LED heartbeat pattern (replaces simple toggle) ──
        // Pattern: ON(100ms) OFF(100ms) ON(100ms) OFF(700ms) — like a real heartbeat
        // Rate scales: faster when hashing well, slower at night phase
        let heartbeat_speed: u8 = if hr > 100.0 {
            1
        } else if day_phase == "night" {
            3
        } else {
            2
        };
        // Reset phase on speed change to prevent double-blink
        if heartbeat_speed != prev_heartbeat_speed {
            led_heartbeat_phase = 0;
            prev_heartbeat_speed = heartbeat_speed;
        }
        led_heartbeat_phase = (led_heartbeat_phase + 1) % (4 * heartbeat_speed);
        let phase_in_cycle = led_heartbeat_phase / heartbeat_speed;
        match phase_in_cycle {
            0 => gpio_ctrl.set_led(true).ok(),  // first beat ON
            1 => gpio_ctrl.set_led(false).ok(), // first beat OFF
            2 => gpio_ctrl.set_led(true).ok(),  // second beat ON
            _ => gpio_ctrl.set_led(false).ok(), // rest (longer pause)
        };
    }
}
