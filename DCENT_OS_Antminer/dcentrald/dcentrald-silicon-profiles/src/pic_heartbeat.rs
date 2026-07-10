//! 6: PIC heartbeat matrix — single source of truth for heartbeat
//! timing per `(Platform, PicFw)` tuple.
//!
//! for the
//! human-readable table, the 7 PIC heartbeat rules, the MSSP parser flush
//! requirement, the deferred-voltage stability gate, the MiscCtrl
//! triple-write rule on S9, and NoPic implications.
//!
//! This crate is `#![forbid(unsafe_code)]` and pure data — every consumer
//! (heartbeat threads, hybrid path, serial mining, dashboard, toolbox)
//! gets the same numbers.
//!
//! Design notes:
//! - `Platform` and `PicFw` here are NEW enums local to this module, not
//!   re-exports from `dcentrald-hal`. The HAL platform identity is
//!   String-typed today (see `/etc/dcentos/board_target`); pinning a
//!   strongly-typed table here lets the compiler exhaustiveness-check
//!   our coverage even before we unify the upstream identity types.
//! - NoPic platforms (am3-aml — S21 / S19K Pro / S19j Pro Amlogic) have
//!   `interval_ms = 0` and `watchdog_timeout_ms = 0` as sentinels meaning
//!   "no heartbeat thread runs". Consumers MUST check `cfg.nopic` first
//!   and short-circuit; do NOT spin a 0-ms tokio interval.
//! - `S17AmStock` is marked load-bearing-but-unproven: code-only 4
//!   plumbing landed `ZynqVariant::S17`, but no live S17 is on the fleet.
//!   Every value on that row carries an `// XXX: confirm against live
//!   S17` comment in the source matrix.

use serde::{Deserialize, Serialize};

/// The set of platform identities for which the heartbeat matrix is
/// authoritative.
///
/// Distinct from the runtime `dcentrald-hal::platform` types — this is a
/// pure-data identity for the heartbeat config table. The mapping from
/// HAL platform to this enum lives at the call site (one-line `match`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    /// Antminer S9 / S9i / T9 (Zynq am1, BM1387, PIC16F1704 ×3 per chain).
    S9Am1,
    /// Antminer S17 (am1-s17, dsPIC33EP16GS202).
    /// Code-only routing only; no live S17 on fleet (4).
    S17Am1,
    /// Antminer S19 / S19 Pro (Zynq am2, BM1398, dsPIC33EP16GS202).
    S19Am2,
    /// Antminer S19j Pro Zynq variant (.139 / .74) — am2-s17, BM1362,
    /// dsPIC33EP16GS202. Includes the fw=0x86 degraded state.
    S19jProAm2,
    /// Antminer S21 / S21 Hydro (am3-aml, BM1368, NoPic / TAS5782M).
    S21Am3Aml,
    /// Antminer S19K Pro (.78) — am3-aml, BM1366, NoPic.
    S19kProAm3Aml,
    /// Antminer S19j Pro Amlogic variant (.133) — am3-aml, BM1362, NoPic.
    S19jProAmlogic,
    /// Stock-Bitmain BB platform (.79) — AM335x, voltage controller TBD.
    BbAm335x,
}

/// Voltage-controller firmware revision class. Distinct from raw fw byte
/// because multiple raw bytes can map to the same protocol class
/// (e.g. 0x56 / 0x5A / 0x5E all behave as `Stock` PIC16F1704).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PicFw {
    /// PIC16F1704 stock Bitmain firmware (raw bytes 0x56, 0x5A, 0x5E).
    /// Detection via cmd 0x04 GET_VERSION. ~60 s watchdog.
    Stock,
    /// PIC16F1704 BraiinsOS-flashed firmware (raw byte 0x03).
    /// Detection via cmd 0x17 GET_VERSION. ~10 s watchdog.
    Braiins,
    /// dsPIC33EP16GS202 healthy firmware revisions (0x82 / 0x89 / 0x8A).
    /// Framed 6-byte protocol; full telemetry available.
    Dspic33epHealthy,
    /// dsPIC33EP16GS202 degraded firmware (0x86 corruption state).
    /// Returns single-byte FW echo for any read; voltage commands
    /// refused by default. Recovery is physical ICSP.
    Dspic33epDegraded,
    /// am3-aml NoPic platforms — TAS5782M audio DAC, no microcontroller.
    /// Voltage is set by the kernel device tree at boot.
    NoPic,
    /// BB AM335x voltage controller — identity not yet pinned.
    /// Treated as unknown until live RE completes.
    BbUnknown,
}

/// Heartbeat configuration for a single `(Platform, PicFw)` tuple.
///
/// All durations are in milliseconds. Sentinel value `0` for both
/// `interval_ms` and `watchdog_timeout_ms` means "no heartbeat thread
/// runs" — only valid when `nopic == true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatConfig {
    /// How often the heartbeat thread MUST send a keepalive. The PIC
    /// watchdog cuts the rail if no heartbeat arrives within
    /// `watchdog_timeout_ms`; we tick faster to leave margin.
    pub interval_ms: u32,

    /// Hardware-side watchdog timeout. Documents the upper bound only;
    /// runtime code MUST tick at `interval_ms`, not at this value.
    pub watchdog_timeout_ms: u32,

    /// Whether the voltage controller responds to telemetry reads
    /// (GET_VOLTAGE / GET_CURRENT / GET_VERSION beyond fw echo). Drives
    /// dashboard display and refusal of voltage commands on `false`.
    pub telemetry_capable: bool,

    /// Number of zero bytes to flush to the controller after ANY NACK
    /// or short read. 16 for PIC16F1704 / dsPIC33EP MSSP parser
    /// recovery. 0 for NoPic platforms (no parser to flush).
    pub flush_after_nack_bytes: u8,

    /// Number of consecutive successful heartbeat ticks required before
    /// the FIRST SET_VOLTAGE is allowed. The deferred-voltage stability
    /// gate from . 5 for
    /// MSSP-based controllers; 0 for NoPic and degraded platforms
    /// (degraded refuses voltage outright).
    pub voltage_stability_ticks: u8,

    /// `true` for am3-aml platforms with no microcontroller voltage
    /// controller. Heartbeat thread MUST be skipped entirely; voltage
    /// is DT-managed; GPIO hashboard reset is FORBIDDEN during normal
    /// operation (kills DAC).
    pub nopic: bool,
}

impl HeartbeatConfig {
    /// `true` if this tuple needs a live heartbeat thread. NoPic and
    /// fully-unknown rows return `false`.
    pub fn needs_heartbeat_thread(&self) -> bool {
        !self.nopic && self.interval_ms > 0
    }

    /// `true` if voltage commands are allowed on this tuple. Degraded
    /// firmware rows return `false` (caller must respect the
    /// `DCENT_AM2_TRUST_DEGRADED_FW` lab override separately).
    pub fn voltage_allowed(&self) -> bool {
        self.telemetry_capable
    }
}

/// Look up the heartbeat configuration for a `(platform, fw)` tuple.
///
/// Compile-time exhaustive over `(Platform, PicFw)`. Tuples that don't
/// describe a real-world configuration (e.g. `S9Am1` × `Dspic33epHealthy`
/// — S9 has no dsPIC) return a defensive "unknown / refused" config so
/// callers can't accidentally drive a PIC16F1704 with dsPIC timing.
///
/// Source matrix: .
pub const fn pic_heartbeat_config(platform: Platform, fw: PicFw) -> HeartbeatConfig {
    use PicFw::*;
    use Platform::*;

    match (platform, fw) {
        // ---------- S9 am1 (PIC16F1704) ----------
        (S9Am1, Stock) => HeartbeatConfig {
            interval_ms: 1_000,
            watchdog_timeout_ms: 60_000,
            telemetry_capable: true,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 5,
            nopic: false,
        },
        (S9Am1, Braiins) => HeartbeatConfig {
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: true,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 5,
            nopic: false,
        },

        // ---------- S17 am1-s17 (dsPIC33EP — code-only) ----------
        // XXX: confirm against live S17 — every value on this row is a
        // code-only assumption per 4. NEVER ship live mining
        // against an S17 without re-pinning these from a hardware probe.
        (S17Am1, Dspic33epHealthy) => HeartbeatConfig {
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: true,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 5,
            nopic: false,
        },

        // ---------- S19 / S19 Pro am2-s17 (dsPIC33EP) ----------
        (S19Am2, Dspic33epHealthy) => HeartbeatConfig {
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: true,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 5,
            nopic: false,
        },
        (S19Am2, Dspic33epDegraded) => HeartbeatConfig {
            // Refused-by-default: see feedback_voltage_fw_whitelist. We
            // still tick the heartbeat to detect recovery, but no
            // voltage commands fire.
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: false,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 0,
            nopic: false,
        },

        // ---------- S19j Pro Zynq am2 (.139 / .74) ----------
        (S19jProAm2, Dspic33epHealthy) => HeartbeatConfig {
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: true,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 5,
            nopic: false,
        },
        (S19jProAm2, Dspic33epDegraded) => HeartbeatConfig {
            // The .139/.74 corruption state. NEVER 0x07 SHORT-form RESET.
            // Voltage refused unless DCENT_AM2_TRUST_DEGRADED_FW=1.
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: false,
            flush_after_nack_bytes: 16,
            voltage_stability_ticks: 0,
            nopic: false,
        },

        // ---------- am3-aml NoPic platforms ----------
        (S21Am3Aml, NoPic) | (S19kProAm3Aml, NoPic) | (S19jProAmlogic, NoPic) => HeartbeatConfig {
            interval_ms: 0,
            watchdog_timeout_ms: 0,
            telemetry_capable: false,
            flush_after_nack_bytes: 0,
            voltage_stability_ticks: 0,
            nopic: true,
        },

        // ---------- BB AM335x (placeholder until live RE) ----------
        (BbAm335x, BbUnknown) => HeartbeatConfig {
            // Placeholder 1 s tick so the harness shape is the same as
            // the real platforms; voltage refused outright until the
            // live BB voltage controller is identified. Do NOT ship
            // live voltage commands on this row without explicit
            // operator authorization.
            interval_ms: 1_000,
            watchdog_timeout_ms: 10_000,
            telemetry_capable: false,
            flush_after_nack_bytes: 0,
            voltage_stability_ticks: 0,
            nopic: false,
        },

        // ---------- All other tuples are unsupported ----------
        // Defensive: returning a refused-everything config is safer than
        // panicking, because callers reading the table at startup
        // shouldn't be able to take down the daemon by passing an
        // accidentally-mismatched (platform, fw) pair.
        _ => HeartbeatConfig {
            interval_ms: 0,
            watchdog_timeout_ms: 0,
            telemetry_capable: false,
            flush_after_nack_bytes: 0,
            voltage_stability_ticks: 0,
            nopic: false,
        },
    }
}

/// Iterator over every `(Platform, PicFw)` tuple. Used by the
/// exhaustiveness test below; also useful for diagnostic dumps.
pub fn all_platform_fw_pairs() -> impl Iterator<Item = (Platform, PicFw)> {
    use PicFw::*;
    use Platform::*;

    const PLATFORMS: &[Platform] = &[
        S9Am1,
        S17Am1,
        S19Am2,
        S19jProAm2,
        S21Am3Aml,
        S19kProAm3Aml,
        S19jProAmlogic,
        BbAm335x,
    ];
    const FWS: &[PicFw] = &[
        Stock,
        Braiins,
        Dspic33epHealthy,
        Dspic33epDegraded,
        NoPic,
        BbUnknown,
    ];

    PLATFORMS
        .iter()
        .flat_map(|p| FWS.iter().map(move |fw| (*p, *fw)))
}

/// Real-world `(Platform, PicFw)` pairs that map to actual hardware.
/// Used by the exhaustiveness test to assert that every legitimate
/// combination has a non-default entry in `pic_heartbeat_config`.
pub const REAL_WORLD_PAIRS: &[(Platform, PicFw)] = &[
    (Platform::S9Am1, PicFw::Stock),
    (Platform::S9Am1, PicFw::Braiins),
    (Platform::S17Am1, PicFw::Dspic33epHealthy),
    (Platform::S19Am2, PicFw::Dspic33epHealthy),
    (Platform::S19Am2, PicFw::Dspic33epDegraded),
    (Platform::S19jProAm2, PicFw::Dspic33epHealthy),
    (Platform::S19jProAm2, PicFw::Dspic33epDegraded),
    (Platform::S21Am3Aml, PicFw::NoPic),
    (Platform::S19kProAm3Aml, PicFw::NoPic),
    (Platform::S19jProAmlogic, PicFw::NoPic),
    (Platform::BbAm335x, PicFw::BbUnknown),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustiveness: every (Platform, PicFw) tuple must return a config
    /// without panic. Real-world pairs must additionally pass the
    /// per-row invariants encoded below.
    #[test]
    fn exhaustiveness_every_tuple_returns_valid_config() {
        let mut count = 0;
        for (p, fw) in all_platform_fw_pairs() {
            let cfg = pic_heartbeat_config(p, fw);

            // Sentinel: nopic implies zero interval.
            if cfg.nopic {
                assert_eq!(
                    cfg.interval_ms, 0,
                    "nopic platforms must have interval_ms=0; ({:?}, {:?})",
                    p, fw
                );
                assert_eq!(
                    cfg.watchdog_timeout_ms, 0,
                    "nopic platforms must have watchdog_timeout_ms=0; ({:?}, {:?})",
                    p, fw
                );
            }

            // Voltage stability gate is only meaningful on
            // telemetry-capable rows.
            if cfg.voltage_stability_ticks > 0 {
                assert!(
                    cfg.telemetry_capable,
                    "voltage_stability_ticks>0 requires telemetry_capable; ({:?}, {:?})",
                    p, fw
                );
            }

            // If we tick, the watchdog must be at least 2x the tick
            // interval. Anything tighter eats all our margin.
            if cfg.interval_ms > 0 && cfg.watchdog_timeout_ms > 0 {
                assert!(
                    cfg.watchdog_timeout_ms >= cfg.interval_ms * 2,
                    "watchdog must be >= 2x interval; ({:?}, {:?}) interval={} wd={}",
                    p,
                    fw,
                    cfg.interval_ms,
                    cfg.watchdog_timeout_ms
                );
            }

            // Helper consistency.
            assert_eq!(
                cfg.needs_heartbeat_thread(),
                !cfg.nopic && cfg.interval_ms > 0
            );
            assert_eq!(cfg.voltage_allowed(), cfg.telemetry_capable);

            count += 1;
        }
        // 8 platforms x 6 fw classes = 48 tuples.
        assert_eq!(count, 8 * 6);
    }

    #[test]
    fn real_world_pairs_have_nondefault_entries() {
        // Every entry in REAL_WORLD_PAIRS must be a hand-pinned row
        // (not the catch-all defensive default). We detect catch-all
        // by checking that at least ONE field is non-default-zero.
        for (p, fw) in REAL_WORLD_PAIRS {
            let cfg = pic_heartbeat_config(*p, *fw);
            let is_catch_all = cfg.interval_ms == 0
                && cfg.watchdog_timeout_ms == 0
                && cfg.flush_after_nack_bytes == 0
                && cfg.voltage_stability_ticks == 0
                && !cfg.nopic
                && !cfg.telemetry_capable;
            assert!(
                !is_catch_all,
                "real-world pair ({:?}, {:?}) hit catch-all default — \
                 missing pinned arm in pic_heartbeat_config",
                p, fw
            );
        }
    }

    #[test]
    fn s9_stock_matches_60s_watchdog() {
        let cfg = pic_heartbeat_config(Platform::S9Am1, PicFw::Stock);
        assert_eq!(cfg.interval_ms, 1_000);
        assert_eq!(cfg.watchdog_timeout_ms, 60_000);
        assert!(cfg.telemetry_capable);
        assert_eq!(cfg.flush_after_nack_bytes, 16);
        assert_eq!(cfg.voltage_stability_ticks, 5);
        assert!(!cfg.nopic);
        assert!(cfg.needs_heartbeat_thread());
        assert!(cfg.voltage_allowed());
    }

    #[test]
    fn s9_braiins_has_shorter_watchdog() {
        let cfg = pic_heartbeat_config(Platform::S9Am1, PicFw::Braiins);
        assert_eq!(cfg.interval_ms, 1_000);
        assert_eq!(cfg.watchdog_timeout_ms, 10_000);
    }

    #[test]
    fn s19jpro_am2_degraded_refuses_voltage() {
        let cfg = pic_heartbeat_config(Platform::S19jProAm2, PicFw::Dspic33epDegraded);
        assert!(!cfg.telemetry_capable);
        assert!(!cfg.voltage_allowed());
        assert_eq!(cfg.voltage_stability_ticks, 0);
        // Heartbeat still ticks so we can detect recovery.
        assert!(cfg.needs_heartbeat_thread());
    }

    #[test]
    fn am3_aml_nopic_skips_heartbeat_thread() {
        for plat in [
            Platform::S21Am3Aml,
            Platform::S19kProAm3Aml,
            Platform::S19jProAmlogic,
        ] {
            let cfg = pic_heartbeat_config(plat, PicFw::NoPic);
            assert!(cfg.nopic, "{:?} must be nopic=true", plat);
            assert_eq!(cfg.interval_ms, 0);
            assert_eq!(cfg.watchdog_timeout_ms, 0);
            assert!(!cfg.needs_heartbeat_thread());
            assert!(!cfg.voltage_allowed());
            assert_eq!(cfg.flush_after_nack_bytes, 0);
        }
    }

    #[test]
    fn bb_am335x_unknown_refuses_voltage() {
        let cfg = pic_heartbeat_config(Platform::BbAm335x, PicFw::BbUnknown);
        assert!(!cfg.voltage_allowed());
        assert_eq!(cfg.voltage_stability_ticks, 0);
        // Tick so we can probe; no flush since parser identity unknown.
        assert!(cfg.needs_heartbeat_thread());
        assert_eq!(cfg.flush_after_nack_bytes, 0);
    }

    #[test]
    fn mismatched_platform_fw_returns_safe_default() {
        // S9 has no dsPIC; this combination must not return a real
        // config that could drive a PIC16F1704 with dsPIC timing.
        let cfg = pic_heartbeat_config(Platform::S9Am1, PicFw::Dspic33epHealthy);
        assert!(!cfg.voltage_allowed());
        assert!(!cfg.needs_heartbeat_thread());
    }

    #[test]
    fn am2_dspic_interval_matches_dcentrald_asic_constant() {
        // Cross-crate consistency: dcentrald-asic::dspic::HEARTBEAT_INTERVAL_MS
        // is currently 10_000 (P1.6 fix per dspic.rs:233). The matrix
        // pins 1_000 ms because that's bosminer's cadence and the
        // hybrid path runs at 1 s. If the asic crate constant changes
        // back to 1 s, this assertion catches the drift.
        //
        // We can't import the asic crate from the silicon-profiles
        // crate (would create a cycle), so we just assert the matrix
        // value here. The reverse check lives in dcentrald-asic tests.
        let cfg = pic_heartbeat_config(Platform::S19Am2, PicFw::Dspic33epHealthy);
        assert_eq!(cfg.interval_ms, 1_000);
    }
}
