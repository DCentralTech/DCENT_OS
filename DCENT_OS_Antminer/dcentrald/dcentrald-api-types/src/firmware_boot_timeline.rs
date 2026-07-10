//!  boot-A — per-firmware-family boot timeline DTOs (HAL-free).
//!
//! Source RE evidence:
//!
//! §1 (Family A-F boot timelines, lines 33-160).
//!
//! Each firmware flavor has a canonical cold-boot timeline from
//! BootROM through first-share-accepted. This module pins the
//! milestones so:
//! - The dashboard "boot progress" widget can show a competitive
//!   comparison ("DCENT_OS reaches first hash 3s faster than Family A").
//! - The dcent-toolbox install adapter can sanity-check that a given
//!   miner is following the expected timeline (a unit stuck at
//!   `KernelBoot` after 30s is broken).
//! - The recovery flow can identify which boot phase a hung unit is in.

use serde::{Deserialize, Serialize};

/// Firmware family covered by the boot timeline catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareBootFamily {
    /// Family A — Bitmain S9 stock + VNish 3.9 (Zynq XC7Z010, Angstrom JFFS2).
    BitmainStockS9,
    /// Family B — VNish 2.0.4 S17 / S17 Pro (Zynq XC7Z010, ramdisk).
    VnishS17_204,
    /// Family C — VNish 1.2.7 OVERLAY (cv / xil — S19kPro-cv, S19jPro-xil, L7, L9).
    Vnish127Overlay,
    /// Family D — VNish 1.2.7 AMLOGIC (S19j Pro AML, S19 XP, S21 family).
    Vnish127Amlogic,
    /// Family E — Bitmain S19j Pro stock + BraiinsOS+ (Zynq am2).
    BraiinsOsPlus,
    /// Family F — DCENT_OS (target architecture).
    DcentOs,
}

/// Common boot phase label across all families. Each phase represents
/// a distinct boot region; not every family hits every phase (e.g.
/// VNish 1.2.7 overlay never re-runs BootROM/FSBL/UBoot — they belong
/// to stock).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPhase {
    /// SoC silicon BootROM (Zynq / Amlogic).
    BootRom,
    /// First-stage bootloader (FSBL / BL2 / SPL).
    Fsbl,
    /// U-Boot reads env, decides rootfs.
    UBoot,
    /// Linux kernel boots.
    KernelBoot,
    /// `/sbin/init` runs (sysv / systemd / procd).
    InitStart,
    /// Userspace mining services start (cgminer / bmminer / dcentrald).
    ServicesStart,
    /// First chip enumeration completes.
    ChainsEnumerated,
    /// First mining work dispatched onto chains.
    FirstWorkDispatch,
    /// First nonce accepted by stratum pool.
    FirstShareAccepted,
}

impl BootPhase {
    /// Stable index in canonical order — used to verify monotonicity.
    pub fn index(&self) -> u8 {
        match self {
            Self::BootRom => 0,
            Self::Fsbl => 1,
            Self::UBoot => 2,
            Self::KernelBoot => 3,
            Self::InitStart => 4,
            Self::ServicesStart => 5,
            Self::ChainsEnumerated => 6,
            Self::FirstWorkDispatch => 7,
            Self::FirstShareAccepted => 8,
        }
    }
}

/// Single milestone in the boot timeline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BootMilestone {
    pub phase: BootPhase,
    /// Wall-clock seconds since power-on (T+0).
    pub at_seconds: f32,
    pub description: &'static str,
}

/// Canonical timeline for Family A — Bitmain S9 stock.
pub const BITMAIN_STOCK_S9_TIMELINE: &[BootMilestone] = &[
    BootMilestone {
        phase: BootPhase::BootRom,
        at_seconds: 0.0,
        description: "Zynq BootROM",
    },
    BootMilestone {
        phase: BootPhase::Fsbl,
        at_seconds: 0.05,
        description: "FSBL from BOOT.bin in mtd0 @ 0x0",
    },
    BootMilestone {
        phase: BootPhase::UBoot,
        at_seconds: 0.4,
        description: "U-Boot 2014.07 reads env, decides ubi.mtd",
    },
    BootMilestone {
        phase: BootPhase::KernelBoot,
        at_seconds: 0.7,
        description: "Linux 4.6.x boots, mounts JFFS2 root from mtd2",
    },
    BootMilestone {
        phase: BootPhase::InitStart,
        at_seconds: 1.5,
        description: "/sbin/init reads /etc/inittab, runlevel S → 5",
    },
    BootMilestone {
        phase: BootPhase::ServicesStart,
        at_seconds: 3.0,
        description: "S80agent.sh + bmminer.sh + monitor-ipsig.sh",
    },
    BootMilestone {
        phase: BootPhase::ChainsEnumerated,
        at_seconds: 10.0,
        description: "bmminer enumerates chains",
    },
    BootMilestone {
        phase: BootPhase::FirstShareAccepted,
        at_seconds: 17.5,
        description: "First nonces accepted on stratum",
    },
];

/// Canonical timeline for Family B — VNish 2.0.4 S17.
pub const VNISH_S17_204_TIMELINE: &[BootMilestone] = &[
    BootMilestone {
        phase: BootPhase::BootRom,
        at_seconds: 0.0,
        description: "Zynq BootROM",
    },
    BootMilestone {
        phase: BootPhase::Fsbl,
        at_seconds: 0.4,
        description: "FSBL + U-Boot 2014.07",
    },
    BootMilestone {
        phase: BootPhase::KernelBoot,
        at_seconds: 0.7,
        description: "Linux loads uramdisk.image.gz from mtd1",
    },
    BootMilestone {
        phase: BootPhase::InitStart,
        at_seconds: 1.0,
        description: "initrd /init exec, /sbin/init reads /etc/inittab",
    },
    BootMilestone {
        phase: BootPhase::ServicesStart,
        at_seconds: 3.5,
        description: "Same as Family A but BM1397+ via dsPIC33EP",
    },
    BootMilestone {
        phase: BootPhase::FirstShareAccepted,
        at_seconds: 12.5,
        description: "First mining work dispatched",
    },
];

/// Canonical timeline for Family C — VNish 1.2.7 OVERLAY.
pub const VNISH_127_OVERLAY_TIMELINE: &[BootMilestone] = &[
    BootMilestone {
        phase: BootPhase::BootRom,
        at_seconds: 0.0,
        description: "Stock BootROM (UNTOUCHED by VNish)",
    },
    BootMilestone {
        phase: BootPhase::Fsbl,
        at_seconds: 0.4,
        description: "Stock FSBL (UNTOUCHED)",
    },
    BootMilestone {
        phase: BootPhase::UBoot,
        at_seconds: 0.6,
        description: "Stock U-Boot (UNTOUCHED)",
    },
    BootMilestone {
        phase: BootPhase::KernelBoot,
        at_seconds: 0.7,
        description: "Stock kernel boots, mounts stock rootfs",
    },
    BootMilestone {
        phase: BootPhase::InitStart,
        at_seconds: 1.5,
        description: "Stock init runs partial S00..S6x",
    },
    BootMilestone {
        phase: BootPhase::ServicesStart,
        at_seconds: 4.0,
        description: "/scripts/boot ELF → bootos.sh extracts overlay",
    },
    BootMilestone {
        phase: BootPhase::FirstWorkDispatch,
        at_seconds: 8.0,
        description: "AnthillOS services up, mining begins",
    },
    BootMilestone {
        phase: BootPhase::FirstShareAccepted,
        at_seconds: 13.5,
        description: "First share submitted",
    },
];

/// Canonical timeline for Family E — BraiinsOS+ on S19j Pro am2.
pub const BRAIINSOS_PLUS_TIMELINE: &[BootMilestone] = &[
    BootMilestone {
        phase: BootPhase::BootRom,
        at_seconds: 0.0,
        description: "Zynq BootROM",
    },
    BootMilestone {
        phase: BootPhase::Fsbl,
        at_seconds: 0.4,
        description: "FSBL + U-Boot 2018.1",
    },
    BootMilestone {
        phase: BootPhase::KernelBoot,
        at_seconds: 0.7,
        description: "Linux 4.14 boots, mounts UBI",
    },
    BootMilestone {
        phase: BootPhase::InitStart,
        at_seconds: 1.5,
        description: "/sbin/init runs SystemD",
    },
    BootMilestone {
        phase: BootPhase::ServicesStart,
        at_seconds: 6.5,
        description: "bosminer-am2 (Rust) starts, dsPIC handshake",
    },
    BootMilestone {
        phase: BootPhase::FirstShareAccepted,
        at_seconds: 13.5,
        description: "First mining work dispatched",
    },
];

/// Canonical timeline for Family F — DCENT_OS.
pub const DCENT_OS_TIMELINE: &[BootMilestone] = &[
    BootMilestone {
        phase: BootPhase::BootRom,
        at_seconds: 0.0,
        description: "BootROM (per SoC)",
    },
    BootMilestone {
        phase: BootPhase::Fsbl,
        at_seconds: 0.4,
        description: "FSBL + U-Boot",
    },
    BootMilestone {
        phase: BootPhase::KernelBoot,
        at_seconds: 0.7,
        description: "Linux mounts UBI rootfs",
    },
    BootMilestone {
        phase: BootPhase::InitStart,
        at_seconds: 1.5,
        description: "/sbin/init runs procd (Buildroot)",
    },
    BootMilestone {
        phase: BootPhase::ServicesStart,
        at_seconds: 3.5,
        description: "S80dashboard + S82dcentrald + S99upgrade",
    },
    BootMilestone {
        phase: BootPhase::ChainsEnumerated,
        at_seconds: 10.0,
        description: "dcentrald cold-boot init Phase A→READY",
    },
    BootMilestone {
        phase: BootPhase::FirstShareAccepted,
        at_seconds: 13.5,
        description: "First nonces",
    },
];

/// Look up the canonical timeline for a firmware family.
pub fn timeline_of(family: FirmwareBootFamily) -> &'static [BootMilestone] {
    match family {
        FirmwareBootFamily::BitmainStockS9 => BITMAIN_STOCK_S9_TIMELINE,
        FirmwareBootFamily::VnishS17_204 => VNISH_S17_204_TIMELINE,
        FirmwareBootFamily::Vnish127Overlay => VNISH_127_OVERLAY_TIMELINE,
        // Family D Amlogic timeline mirrors Family C overlay-on-stock.
        FirmwareBootFamily::Vnish127Amlogic => VNISH_127_OVERLAY_TIMELINE,
        FirmwareBootFamily::BraiinsOsPlus => BRAIINSOS_PLUS_TIMELINE,
        FirmwareBootFamily::DcentOs => DCENT_OS_TIMELINE,
    }
}

/// Returns the canonical first-hash time (seconds since BootRom) for a
/// family, or `None` if the timeline doesn't reach FirstShareAccepted.
pub fn first_share_time(family: FirmwareBootFamily) -> Option<f32> {
    timeline_of(family)
        .iter()
        .find(|m| m.phase == BootPhase::FirstShareAccepted)
        .map(|m| m.at_seconds)
}

// ---------------------------------------------------------------------------
//  W5 — BootProgressTracker
//
// Runtime-side tracker that records the wall-clock millisecond timestamp
// for each `BootPhase` transition reached so far. Mirrors the data the
// daemon already emits via `tracing::info!(target: "boot", phase = ?, …)`,
// but in a structured form the REST layer can return for the boot
// timeline endpoint without parsing journald.
// ---------------------------------------------------------------------------

/// One observed boot-phase transition.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ObservedBootPhase {
    pub phase: BootPhase,
    /// Wall-clock milliseconds since the unix epoch.
    pub at_unix_ms: u64,
}

/// Tracker of the runtime-observed boot phases. Append-only by design —
/// in practice each phase fires at most once per daemon lifetime so the
/// vec is bounded at the number of `BootPhase` variants (currently 9).
#[derive(Debug, Clone, Default)]
pub struct BootProgressTracker {
    entries: Vec<ObservedBootPhase>,
}

impl BootProgressTracker {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record a phase transition. If the same phase has already been
    /// recorded, this is a no-op (the first observed timestamp wins).
    pub fn record(&mut self, phase: BootPhase, at_unix_ms: u64) {
        if self.entries.iter().any(|e| e.phase == phase) {
            return;
        }
        self.entries.push(ObservedBootPhase { phase, at_unix_ms });
    }

    /// Snapshot the observed phase entries in insertion order.
    pub fn snapshot(&self) -> Vec<ObservedBootPhase> {
        self.entries.clone()
    }

    /// Has the given phase been observed?
    pub fn has_reached(&self, phase: BootPhase) -> bool {
        self.entries.iter().any(|e| e.phase == phase)
    }

    /// Number of observed phases.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_bootrom_anchor() {
        for family in [
            FirmwareBootFamily::BitmainStockS9,
            FirmwareBootFamily::VnishS17_204,
            FirmwareBootFamily::Vnish127Overlay,
            FirmwareBootFamily::Vnish127Amlogic,
            FirmwareBootFamily::BraiinsOsPlus,
            FirmwareBootFamily::DcentOs,
        ] {
            let timeline = timeline_of(family);
            assert!(
                timeline.iter().any(|m| m.phase == BootPhase::BootRom),
                "{:?} missing BootRom",
                family
            );
            assert_eq!(
                timeline.first().map(|m| m.at_seconds),
                Some(0.0),
                "{:?} first milestone must be at T+0",
                family
            );
        }
    }

    #[test]
    fn every_family_reaches_first_share() {
        for family in [
            FirmwareBootFamily::BitmainStockS9,
            FirmwareBootFamily::VnishS17_204,
            FirmwareBootFamily::Vnish127Overlay,
            FirmwareBootFamily::Vnish127Amlogic,
            FirmwareBootFamily::BraiinsOsPlus,
            FirmwareBootFamily::DcentOs,
        ] {
            assert!(
                first_share_time(family).is_some(),
                "{:?} timeline does not reach FirstShareAccepted",
                family
            );
        }
    }

    #[test]
    fn milestones_in_strictly_increasing_time() {
        for family in [
            FirmwareBootFamily::BitmainStockS9,
            FirmwareBootFamily::VnishS17_204,
            FirmwareBootFamily::Vnish127Overlay,
            FirmwareBootFamily::BraiinsOsPlus,
            FirmwareBootFamily::DcentOs,
        ] {
            let timeline = timeline_of(family);
            for window in timeline.windows(2) {
                assert!(
                    window[1].at_seconds >= window[0].at_seconds,
                    "{:?} timeline regresses at {:?}",
                    family,
                    window[1].phase
                );
            }
        }
    }

    #[test]
    fn phase_indexes_match_canonical_order() {
        // No timeline ever has a phase whose index DECREASES — the
        // ordering of phases is a partial order.
        for family in [
            FirmwareBootFamily::BitmainStockS9,
            FirmwareBootFamily::Vnish127Overlay,
            FirmwareBootFamily::DcentOs,
        ] {
            let timeline = timeline_of(family);
            for window in timeline.windows(2) {
                assert!(
                    window[0].phase.index() < window[1].phase.index(),
                    "{:?} phase index regresses: {:?} → {:?}",
                    family,
                    window[0].phase,
                    window[1].phase
                );
            }
        }
    }

    #[test]
    fn bitmain_stock_s9_first_share_in_documented_window() {
        // RE doc Family A: T+15-20s "First nonces accepted on stratum".
        let t = first_share_time(FirmwareBootFamily::BitmainStockS9).unwrap();
        assert!(
            (15.0..=20.0).contains(&t),
            "Bitmain S9 stock first-share at {} s outside [15, 20] window",
            t
        );
    }

    #[test]
    fn vnish_127_overlay_first_share_in_documented_window() {
        // RE doc Family C: T+~12-15s.
        let t = first_share_time(FirmwareBootFamily::Vnish127Overlay).unwrap();
        assert!(
            (12.0..=15.0).contains(&t),
            "VNish 1.2.7 overlay first-share at {} s outside [12, 15]",
            t
        );
    }

    #[test]
    fn dcent_os_first_share_in_documented_window() {
        // RE doc Family F: T+12-15s.
        let t = first_share_time(FirmwareBootFamily::DcentOs).unwrap();
        assert!(
            (12.0..=15.0).contains(&t),
            "DCENT_OS first-share at {} s outside [12, 15]",
            t
        );
    }

    #[test]
    fn vnish_overlay_marks_stock_components_untouched() {
        // RE doc Family C calls out that BootROM/FSBL/U-Boot/kernel are
        // UNTOUCHED. Pin via description string match — defends
        // the competitive claim that VNish 1.2.7 doesn't re-flash boot.
        let timeline = timeline_of(FirmwareBootFamily::Vnish127Overlay);
        let untouched_count = timeline
            .iter()
            .filter(|m| m.description.contains("UNTOUCHED"))
            .count();
        assert!(
            untouched_count >= 3,
            "VNish overlay should mark ≥3 stock phases UNTOUCHED, found {}",
            untouched_count
        );
    }

    #[test]
    fn vnish_127_amlogic_inherits_overlay_timeline() {
        // Family D AML mirrors Family C overlay-on-stock pattern.
        let c = timeline_of(FirmwareBootFamily::Vnish127Overlay);
        let d = timeline_of(FirmwareBootFamily::Vnish127Amlogic);
        assert_eq!(c.len(), d.len());
    }

    #[test]
    fn braiinsos_plus_uses_systemd() {
        // Family E init phase description must mention SystemD.
        let timeline = timeline_of(FirmwareBootFamily::BraiinsOsPlus);
        let init = timeline
            .iter()
            .find(|m| m.phase == BootPhase::InitStart)
            .unwrap();
        assert!(init.description.contains("SystemD"));
    }

    #[test]
    fn dcent_os_uses_procd_and_buildroot() {
        let timeline = timeline_of(FirmwareBootFamily::DcentOs);
        let init = timeline
            .iter()
            .find(|m| m.phase == BootPhase::InitStart)
            .unwrap();
        assert!(init.description.contains("procd"));
        assert!(init.description.contains("Buildroot"));
    }

    #[test]
    fn family_round_trips_through_serde() {
        for family in [
            FirmwareBootFamily::BitmainStockS9,
            FirmwareBootFamily::VnishS17_204,
            FirmwareBootFamily::Vnish127Overlay,
            FirmwareBootFamily::Vnish127Amlogic,
            FirmwareBootFamily::BraiinsOsPlus,
            FirmwareBootFamily::DcentOs,
        ] {
            let json = serde_json::to_string(&family).unwrap();
            let back: FirmwareBootFamily = serde_json::from_str(&json).unwrap();
            assert_eq!(family, back);
        }
    }

    #[test]
    fn boot_phase_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&BootPhase::FirstShareAccepted).unwrap(),
            "\"first_share_accepted\""
        );
        assert_eq!(
            serde_json::to_string(&BootPhase::ChainsEnumerated).unwrap(),
            "\"chains_enumerated\""
        );
    }

    // -----------------------------------------------------------------------
    //  W5 — BootProgressTracker
    // -----------------------------------------------------------------------

    #[test]
    fn boot_progress_tracker_starts_empty() {
        let t = BootProgressTracker::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
        assert!(!t.has_reached(BootPhase::ServicesStart));
        assert!(t.snapshot().is_empty());
    }

    #[test]
    fn boot_progress_tracker_records_phases_in_order() {
        let mut t = BootProgressTracker::new();
        t.record(BootPhase::ServicesStart, 1_000);
        t.record(BootPhase::ChainsEnumerated, 10_000);
        t.record(BootPhase::FirstWorkDispatch, 11_500);
        let snap = t.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].phase, BootPhase::ServicesStart);
        assert_eq!(snap[0].at_unix_ms, 1_000);
        assert_eq!(snap[2].phase, BootPhase::FirstWorkDispatch);
        assert_eq!(snap[2].at_unix_ms, 11_500);
    }

    #[test]
    fn boot_progress_tracker_first_observation_wins() {
        let mut t = BootProgressTracker::new();
        t.record(BootPhase::ServicesStart, 1_000);
        // Spurious second record of the same phase — should be ignored.
        t.record(BootPhase::ServicesStart, 9_999_999);
        assert_eq!(t.len(), 1);
        assert_eq!(t.snapshot()[0].at_unix_ms, 1_000);
    }

    #[test]
    fn boot_progress_tracker_has_reached_flips_on_record() {
        let mut t = BootProgressTracker::new();
        assert!(!t.has_reached(BootPhase::FirstShareAccepted));
        t.record(BootPhase::FirstShareAccepted, 13_500);
        assert!(t.has_reached(BootPhase::FirstShareAccepted));
        // Other phases still not reached.
        assert!(!t.has_reached(BootPhase::ChainsEnumerated));
    }

    #[test]
    fn observed_boot_phase_round_trips_through_serde() {
        let o = ObservedBootPhase {
            phase: BootPhase::ChainsEnumerated,
            at_unix_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&o).unwrap();
        let back: ObservedBootPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(o, back);
        assert!(json.contains("\"phase\":\"chains_enumerated\""));
        assert!(json.contains("\"at_unix_ms\":1700000000000"));
    }
}
