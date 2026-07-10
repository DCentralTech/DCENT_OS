//!  boot-A — Boot-flow phase timeline DTO (HAL-free).
//!
//! Source RE evidence:
//!
//! (200 lines).
//!
//! Captures the canonical boot phases each firmware family transitions
//! through, from BootROM to first nonce, with expected wall-clock
//! windows per platform tier:
//!
//! ```text
//!   BootRom → Fsbl → UBoot → Kernel → Init → Services → MiningStarted → FirstNonce
//! ```
//!
//! HAL-free pure DTO + lookup. The runtime adapter inside `dcentrald`
//! / `dashboard` consumes the timeline to render boot progress and to
//! flag boot stages that exceed their expected window (operator-visible
//! "stuck" indicator).

use serde::{Deserialize, Serialize};

/// Discrete boot phases. Order matches the boot timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPhase {
    /// Pre-boot. Power applied; SoC silicon still in BootROM.
    BootRom,
    /// First-Stage Boot Loader (FSBL on Zynq, BL2 on Amlogic).
    Fsbl,
    /// U-Boot loaded; reading device tree + kernel image.
    UBoot,
    /// Linux kernel booting + mounting rootfs.
    Kernel,
    /// `/sbin/init` running; reading inittab; SysV rc.S transition.
    Init,
    /// User-space services starting (dropbear / lighttpd / dcentrald).
    Services,
    /// Mining process started; chains powering up.
    MiningStarted,
    /// First nonce accepted at the pool.
    FirstNonce,
}

/// Platform tier identifier (matches port-bos-lux PLATFORM_MATRIX).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformTier {
    /// S9 / S9i / T9 — Zynq XC7Z010, JFFS2 root, Linux 4.6.x.
    Am1Zynq,
    /// S17 / S19 family — Zynq XC7Z010, ramdisk, Linux 4.4.x.
    Am2Zynq,
    /// S19k Pro / S21 family — Amlogic SoC, NAND rootfs, Linux 4.9.x.
    Am3Aml,
    /// S19j Pro BB variant — TI AM335x BeagleBone Black, NAND.
    Am3Bb,
}

/// Expected timing window for one phase on one platform tier.
///
/// `min_ms` and `max_ms` capture the documented healthy range — outside
/// the band the runtime adapter should flag a "boot stuck" warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct PhaseWindow {
    pub phase: BootPhase,
    /// Lower bound of expected start (ms since power-on).
    pub min_ms: u32,
    /// Upper bound of expected start (ms since power-on).
    pub max_ms: u32,
    /// Operator-facing description of what happens in this phase.
    pub description: &'static str,
}

/// Am1Zynq (Bitmain S9 stock + VNish 3.9) timeline per RE doc Family A.
pub const AM1_ZYNQ_TIMELINE: &[PhaseWindow] = &[
    PhaseWindow {
        phase: BootPhase::BootRom,
        min_ms: 0,
        max_ms: 50,
        description: "Zynq BootROM (silicon)",
    },
    PhaseWindow {
        phase: BootPhase::Fsbl,
        min_ms: 50,
        max_ms: 400,
        description: "FSBL from BOOT.bin in mtd0 @ 0x0",
    },
    PhaseWindow {
        phase: BootPhase::UBoot,
        min_ms: 400,
        max_ms: 700,
        description: "U-Boot 2014.07 — DTB + uImage in mtd0",
    },
    PhaseWindow {
        phase: BootPhase::Kernel,
        min_ms: 700,
        max_ms: 1500,
        description: "Linux 4.6.x — JFFS2 root from mtd2",
    },
    PhaseWindow {
        phase: BootPhase::Init,
        min_ms: 1500,
        max_ms: 2500,
        description: "/sbin/init → rc.S → runlevel 5",
    },
    PhaseWindow {
        phase: BootPhase::Services,
        min_ms: 2000,
        max_ms: 8000,
        description: "dropbear + lighttpd + bmminer.sh + monitor-ipsig.sh",
    },
    PhaseWindow {
        phase: BootPhase::MiningStarted,
        min_ms: 8000,
        max_ms: 12000,
        description: "bmminer enumerates chains",
    },
    PhaseWindow {
        phase: BootPhase::FirstNonce,
        min_ms: 15000,
        max_ms: 20000,
        description: "First accepted shares on Stratum",
    },
];

/// Am2Zynq (S17 / S19 family Zynq variant) timeline per RE doc Family B.
pub const AM2_ZYNQ_TIMELINE: &[PhaseWindow] = &[
    PhaseWindow {
        phase: BootPhase::BootRom,
        min_ms: 0,
        max_ms: 50,
        description: "Zynq BootROM",
    },
    PhaseWindow {
        phase: BootPhase::Fsbl,
        min_ms: 50,
        max_ms: 400,
        description: "FSBL from BOOT.bin at mtd0 @ 0x0",
    },
    PhaseWindow {
        phase: BootPhase::UBoot,
        min_ms: 400,
        max_ms: 700,
        description: "U-Boot 2014.07 — DTB at mtd0 @ 0x1A00000",
    },
    PhaseWindow {
        phase: BootPhase::Kernel,
        min_ms: 700,
        max_ms: 1000,
        description: "Linux loads uramdisk from mtd1 (recovery: mtd4)",
    },
    PhaseWindow {
        phase: BootPhase::Init,
        min_ms: 1000,
        max_ms: 2000,
        description: "initrd /init → /sbin/init → rc 5",
    },
    PhaseWindow {
        phase: BootPhase::Services,
        min_ms: 2000,
        max_ms: 5000,
        description: "Same scripts as am1 but BM1397+ via dsPIC33EP",
    },
    PhaseWindow {
        phase: BootPhase::MiningStarted,
        min_ms: 5000,
        max_ms: 10000,
        description: "Chain enumeration + power-up",
    },
    PhaseWindow {
        phase: BootPhase::FirstNonce,
        min_ms: 10000,
        max_ms: 15000,
        description: "First mining work dispatched",
    },
];

/// Am3Aml (Amlogic A113D — S19k Pro / S21 family) — typical timeline.
/// Numbers reconstructed from operator-empirical observations
///.
pub const AM3_AML_TIMELINE: &[PhaseWindow] = &[
    PhaseWindow {
        phase: BootPhase::BootRom,
        min_ms: 0,
        max_ms: 100,
        description: "Amlogic BootROM → BL2",
    },
    PhaseWindow {
        phase: BootPhase::Fsbl,
        min_ms: 100,
        max_ms: 800,
        description: "BL2 / FIP",
    },
    PhaseWindow {
        phase: BootPhase::UBoot,
        min_ms: 800,
        max_ms: 1400,
        description: "U-Boot 2015.01 — kernel from NAND",
    },
    PhaseWindow {
        phase: BootPhase::Kernel,
        min_ms: 1400,
        max_ms: 3000,
        description: "Linux 4.9.113 — uImage-wrapped CPIO rootfs",
    },
    PhaseWindow {
        phase: BootPhase::Init,
        min_ms: 3000,
        max_ms: 4500,
        description: "BusyBox init → S37bitmainer_setup",
    },
    PhaseWindow {
        phase: BootPhase::Services,
        min_ms: 4500,
        max_ms: 12000,
        description: "dropbear + lighttpd + dcentrald",
    },
    PhaseWindow {
        phase: BootPhase::MiningStarted,
        min_ms: 12000,
        max_ms: 25000,
        description: "Chain cold-boot (Phase A→READY) — slow due to PSU PMBus",
    },
    PhaseWindow {
        phase: BootPhase::FirstNonce,
        min_ms: 25000,
        max_ms: 60000,
        description: "First accepted shares (S21 typical 30-45 s)",
    },
];

/// Look up the boot timeline for a platform tier.
pub fn timeline_for(tier: PlatformTier) -> &'static [PhaseWindow] {
    match tier {
        PlatformTier::Am1Zynq => AM1_ZYNQ_TIMELINE,
        PlatformTier::Am2Zynq => AM2_ZYNQ_TIMELINE,
        // am3-bb shares the Bitmain BB stock chain timing closely with
        // am1-zynq (same userspace pattern); use am1 as best-effort
        // placeholder until a live capture from `a lab unit` lands.
        PlatformTier::Am3Aml | PlatformTier::Am3Bb => AM3_AML_TIMELINE,
    }
}

/// Find the expected window for a specific (tier, phase) pair.
pub fn window_for(tier: PlatformTier, phase: BootPhase) -> Option<&'static PhaseWindow> {
    timeline_for(tier).iter().find(|w| w.phase == phase)
}

/// Classify a measured boot-phase timestamp against the documented
/// window. Returns:
/// - `Healthy` — within the documented band.
/// - `EarlyDelivery` — significantly faster than expected (likely
///   runtime adapter mis-reporting).
/// - `Stuck` — slower than expected, runtime should flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseTimingVerdict {
    Healthy,
    EarlyDelivery,
    Stuck,
    NotReferenced,
}

/// Classify a measured wall-clock timestamp (ms since power-on) for a
/// given (tier, phase) pair.
pub fn classify_phase_timing(
    tier: PlatformTier,
    phase: BootPhase,
    measured_ms: u32,
) -> PhaseTimingVerdict {
    let win = match window_for(tier, phase) {
        Some(w) => w,
        None => return PhaseTimingVerdict::NotReferenced,
    };
    if measured_ms < win.min_ms.saturating_sub(win.min_ms / 4) {
        // Significantly earlier than expected (more than 25% early).
        PhaseTimingVerdict::EarlyDelivery
    } else if measured_ms > win.max_ms.saturating_add(win.max_ms / 4) {
        // More than 25% over the upper bound — flag as stuck.
        PhaseTimingVerdict::Stuck
    } else {
        PhaseTimingVerdict::Healthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_lists_all_eight_phases() {
        for tier in [
            PlatformTier::Am1Zynq,
            PlatformTier::Am2Zynq,
            PlatformTier::Am3Aml,
            PlatformTier::Am3Bb,
        ] {
            let tl = timeline_for(tier);
            assert_eq!(tl.len(), 8, "{:?} should have 8 boot phases", tier);
        }
    }

    #[test]
    fn phase_order_is_monotonic_within_each_tier() {
        for tier in [
            PlatformTier::Am1Zynq,
            PlatformTier::Am2Zynq,
            PlatformTier::Am3Aml,
        ] {
            let tl = timeline_for(tier);
            for window in tl.windows(2) {
                assert!(
                    window[1].min_ms >= window[0].min_ms,
                    "{:?} phase {:?} starts before {:?}",
                    tier,
                    window[1].phase,
                    window[0].phase
                );
            }
        }
    }

    #[test]
    fn am1_zynq_first_nonce_window_anchors_at_15_20s() {
        // RE doc Family A: "T+15-20s First nonces accepted on stratum".
        let w = window_for(PlatformTier::Am1Zynq, BootPhase::FirstNonce).unwrap();
        assert_eq!(w.min_ms, 15000);
        assert_eq!(w.max_ms, 20000);
    }

    #[test]
    fn am3_aml_mining_started_is_slower_than_am1() {
        // S21 Amlogic boots are slower due to PMBus PSU init.
        let am1 = window_for(PlatformTier::Am1Zynq, BootPhase::MiningStarted).unwrap();
        let am3 = window_for(PlatformTier::Am3Aml, BootPhase::MiningStarted).unwrap();
        assert!(am3.min_ms > am1.min_ms);
    }

    #[test]
    fn classify_within_window_returns_healthy() {
        let v = classify_phase_timing(PlatformTier::Am1Zynq, BootPhase::Kernel, 1000);
        assert_eq!(v, PhaseTimingVerdict::Healthy);
    }

    #[test]
    fn classify_well_past_window_returns_stuck() {
        // Am1Zynq Kernel window 700-1500. 30 s in is way past.
        let v = classify_phase_timing(PlatformTier::Am1Zynq, BootPhase::Kernel, 30_000);
        assert_eq!(v, PhaseTimingVerdict::Stuck);
    }

    #[test]
    fn classify_at_zero_returns_early_delivery_for_late_phases() {
        // FirstNonce at 0 ms is impossibly early.
        let v = classify_phase_timing(PlatformTier::Am1Zynq, BootPhase::FirstNonce, 0);
        assert_eq!(v, PhaseTimingVerdict::EarlyDelivery);
    }

    #[test]
    fn boot_phase_round_trips_through_serde() {
        for p in [
            BootPhase::BootRom,
            BootPhase::Fsbl,
            BootPhase::UBoot,
            BootPhase::Kernel,
            BootPhase::Init,
            BootPhase::Services,
            BootPhase::MiningStarted,
            BootPhase::FirstNonce,
        ] {
            let json = serde_json::to_string(&p).unwrap();
            let back: BootPhase = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn platform_tier_round_trips_through_serde() {
        for t in [
            PlatformTier::Am1Zynq,
            PlatformTier::Am2Zynq,
            PlatformTier::Am3Aml,
            PlatformTier::Am3Bb,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: PlatformTier = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn phase_window_serializes_to_documented_shape() {
        let w = window_for(PlatformTier::Am1Zynq, BootPhase::FirstNonce).unwrap();
        let json = serde_json::to_string(w).unwrap();
        assert!(json.contains("\"phase\":\"first_nonce\""));
        assert!(json.contains("\"min_ms\":15000"));
        assert!(json.contains("\"max_ms\":20000"));
    }
}
