//! W13.D1: Live cold-boot phase enum for `/api/boot/phase` and the
//! per-phase timeline for `/api/boot/timeline` (dev-mode only).
//!
//! # Substate taxonomy (CV1835 — `bmminer_init_trace_cv1835.md`)
//!
//! The R4 RE3 trace gives a 6-substate taxonomy for CV1835 cold boot:
//!
//! | substate                  | window     | notes                                      |
//! |---------------------------|------------|--------------------------------------------|
//! | `boot_psu_init`           | t=7.0-7.5s | APW12 SMBus 5-step init                    |
//! | `boot_pic_dc_dc_enable`   | t=7.5-8.0s | PIC1704 DC-DC enable (chain rails up)      |
//! | `boot_asic_enum`          | t=8.0-8.5s | BM1362 enumeration (GetAddress broadcast)  |
//! | `boot_misc_ctrl_triple_write` | t=8.5-9.0s | BM1362 MiscCtrl 3× w/ 5ms spacing      |
//! | `boot_first_work_tx`      | t=9.0s     | First WORK_TX dispatched                   |
//! | `boot_awaiting_first_nonce` | post-9.0s | Until first WORK_RX                        |
//!
//! Non-CV1835 platforms get a generic 3-substate fallback (`booting` /
//! `starting` / `mining`) — the R4 trace only covers CV1835.
//!
//! `--s19j-hybrid` mode bypasses `daemon.rs::Daemon::run()` entirely (and
//! therefore never starts the API server), so the synthetic
//! `hybrid_mode_no_api` variant is served by `server.py` as a fallback —
//! this enum still needs the variant so the dashboard can decode it.
//!
//! # Cross-references
//! - See `~/
//! - See `~/
//! - (last-known fallback)

use serde::{Deserialize, Serialize};

/// CV1835 cold-boot substate (per R4 `bmminer_init_trace_cv1835.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cv1835BootPhase {
    /// t=7.0-7.5s — APW12 SMBus 5-step PSU init.
    BootPsuInit,
    /// t=7.5-8.0s — PIC1704 DC-DC enable (chain rails up to nominal).
    BootPicDcDcEnable,
    /// t=8.0-8.5s — BM1362 ASIC enumeration (GetAddress broadcast).
    BootAsicEnum,
    /// t=8.5-9.0s — BM1362 MiscCtrl triple-write (3× w/ 5ms spacing).
    BootMiscCtrlTripleWrite,
    /// t=9.0s — first WORK_TX dispatched to the chain.
    BootFirstWorkTx,
    /// post-9.0s — first WORK_TX sent, awaiting first WORK_RX (nonce).
    BootAwaitingFirstNonce,
}

/// Generic 3-substate fallback for non-CV1835 platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenericBootPhase {
    /// Daemon up, hardware probe in progress.
    Booting,
    /// Hardware probe complete, work dispatch starting.
    Starting,
    /// First nonce observed; mining loop running.
    Mining,
}

/// Top-level boot-phase enum served by `/api/boot/phase`.
///
/// `serde(tag = "kind", content = "phase")` so the JSON shape stays
/// dashboard-friendly:
///
/// ```text
/// {"kind":"cv1835","phase":"boot_asic_enum"}
/// {"kind":"generic","phase":"booting"}
/// {"kind":"hybrid_mode_no_api"}
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "phase", rename_all = "snake_case")]
pub enum BootPhase {
    /// CV1835 cold-boot 6-substate taxonomy.
    Cv1835(Cv1835BootPhase),
    /// Generic 3-substate fallback (non-CV1835 platforms).
    Generic(GenericBootPhase),
    /// `--s19j-hybrid` mode bypasses the daemon's API server. Served
    /// by `server.py` as a fallback when the daemon is hybrid-mode-down.
    HybridModeNoApi,
}

impl Default for BootPhase {
    fn default() -> Self {
        Self::Generic(GenericBootPhase::Booting)
    }
}

/// Wrapper for `/api/boot/phase` — pairs the live phase with last-known
/// metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BootPhaseResponse {
    /// Live phase, or last-known phase when the daemon is down.
    pub phase: BootPhase,
    /// Wall-clock unix timestamp (ms) when this phase was entered.
    /// `None` when no phase has been published yet.
    pub started_at_unix_ms: Option<u64>,
    /// `true` when the daemon is reachable and the phase is current.
    /// `false` when the dashboard is rendering the last-known fallback.
    pub is_live: bool,
}

/// One row of the boot-timeline (dev-mode only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootTimelineEntry {
    /// Phase that started.
    pub phase: BootPhase,
    /// Wall-clock unix timestamp (ms) when the phase was entered.
    pub started_at_unix_ms: u64,
    /// Wall-clock unix timestamp (ms) when the phase ended (left for
    /// the next phase). `None` for the currently-active phase.
    pub ended_at_unix_ms: Option<u64>,
}

/// Response payload for `GET /api/boot/timeline`. Dev-mode only —
/// gated behind `ApiConfig::expose_boot_timeline`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BootTimelineResponse {
    /// Per-phase timeline entries, ordered oldest-first.
    pub entries: Vec<BootTimelineEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_phase_default_is_generic_booting() {
        assert_eq!(
            BootPhase::default(),
            BootPhase::Generic(GenericBootPhase::Booting)
        );
        let r = BootPhaseResponse::default();
        assert_eq!(r.phase, BootPhase::Generic(GenericBootPhase::Booting));
        assert_eq!(r.started_at_unix_ms, None);
        assert!(!r.is_live);
    }

    #[test]
    fn boot_phase_cv1835_substates_serialize() {
        let cases = [
            (Cv1835BootPhase::BootPsuInit, "boot_psu_init"),
            (Cv1835BootPhase::BootPicDcDcEnable, "boot_pic_dc_dc_enable"),
            (Cv1835BootPhase::BootAsicEnum, "boot_asic_enum"),
            (
                Cv1835BootPhase::BootMiscCtrlTripleWrite,
                "boot_misc_ctrl_triple_write",
            ),
            (Cv1835BootPhase::BootFirstWorkTx, "boot_first_work_tx"),
            (
                Cv1835BootPhase::BootAwaitingFirstNonce,
                "boot_awaiting_first_nonce",
            ),
        ];
        for (phase, expected) in cases {
            let bp = BootPhase::Cv1835(phase);
            let j = serde_json::to_value(bp).unwrap();
            assert_eq!(j["kind"], "cv1835");
            assert_eq!(j["phase"], expected, "wrong serialization for {phase:?}");
            // round-trip
            let s = serde_json::to_string(&bp).unwrap();
            let back: BootPhase = serde_json::from_str(&s).unwrap();
            assert_eq!(back, bp);
        }
    }

    #[test]
    fn boot_phase_generic_substates_serialize() {
        let cases = [
            (GenericBootPhase::Booting, "booting"),
            (GenericBootPhase::Starting, "starting"),
            (GenericBootPhase::Mining, "mining"),
        ];
        for (phase, expected) in cases {
            let bp = BootPhase::Generic(phase);
            let j = serde_json::to_value(bp).unwrap();
            assert_eq!(j["kind"], "generic");
            assert_eq!(j["phase"], expected);
            let s = serde_json::to_string(&bp).unwrap();
            let back: BootPhase = serde_json::from_str(&s).unwrap();
            assert_eq!(back, bp);
        }
    }

    #[test]
    fn boot_phase_hybrid_mode_no_api_serializes() {
        let bp = BootPhase::HybridModeNoApi;
        let j = serde_json::to_value(bp).unwrap();
        assert_eq!(j["kind"], "hybrid_mode_no_api");
        // No `phase` field for hybrid_mode_no_api (unit variant).
        assert!(j.get("phase").is_none());
        // Round-trip via string (`serde_json::to_value` for adjacently-tagged
        // unit variant emits no content, which is what we want).
        let s = serde_json::to_string(&bp).unwrap();
        let back: BootPhase = serde_json::from_str(&s).unwrap();
        assert_eq!(back, bp);
    }

    #[test]
    fn boot_phase_response_round_trips() {
        let r = BootPhaseResponse {
            phase: BootPhase::Cv1835(Cv1835BootPhase::BootMiscCtrlTripleWrite),
            started_at_unix_ms: Some(1_715_000_000_500),
            is_live: true,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: BootPhaseResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
        let j: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(j["phase"]["kind"], "cv1835");
        assert_eq!(j["phase"]["phase"], "boot_misc_ctrl_triple_write");
        assert_eq!(j["started_at_unix_ms"], 1_715_000_000_500u64);
        assert_eq!(j["is_live"], true);
    }

    #[test]
    fn boot_timeline_response_serializes_ordered_entries() {
        let r = BootTimelineResponse {
            entries: vec![
                BootTimelineEntry {
                    phase: BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit),
                    started_at_unix_ms: 7_000,
                    ended_at_unix_ms: Some(7_500),
                },
                BootTimelineEntry {
                    phase: BootPhase::Cv1835(Cv1835BootPhase::BootPicDcDcEnable),
                    started_at_unix_ms: 7_500,
                    ended_at_unix_ms: Some(8_000),
                },
                BootTimelineEntry {
                    phase: BootPhase::Cv1835(Cv1835BootPhase::BootAwaitingFirstNonce),
                    started_at_unix_ms: 9_000,
                    ended_at_unix_ms: None,
                },
            ],
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["entries"].as_array().unwrap().len(), 3);
        assert_eq!(j["entries"][0]["phase"]["phase"], "boot_psu_init");
        assert_eq!(j["entries"][2]["ended_at_unix_ms"], serde_json::Value::Null);
    }
}
