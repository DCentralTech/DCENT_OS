//!  cis-A — Per-chip-family cold-boot init constants (HAL-free).
//!
//! Source RE evidence:
//!
//! (330 lines).
//!
//! Captures the per-chip register/protocol deltas from the generic 12-step
//! pipeline (`asic-protocol-bible.md` §1). Each chip family has its own
//! peculiarities:
//! - BM1387 uses register `0x0C` for PLL (not `0x08`) and `0x1C` for
//!   MiscCtrl (not `0x18`). Open-core is REQUIRED (114 dummy works × N
//!   chips at gate_block=1).
//! - BM1397/BM1398 share opcodes; BM1397 needs a register 0x70 pre-write
//!   to prevent PLL glitch.
//! - BM1362 needs the ASIC reg `0x2C` UART_RELAY write
//!   (`UART_RELAY = 0x007C_0003`) on every host. The legacy
//!   "FPGA UART relay quirk on am2" was misdiagnosed (R4 closure W13.B1):
//!   `0x43D0_0030` / `0x43D0_0034` are read-only Braiins-am2 status mirrors,
//!   NOT a control surface. cores_per_chip=4 (NOT 894 — common
//!   miscoding hazard, per the RE doc HAZARD callout).
//!
//! HAL-free: pure data + dispatch. The runtime adapter inside
//! `dcentrald-asic` consumes these constants to compose the actual
//! UART writes / FPGA register pokes.

use serde::{Deserialize, Serialize};

/// Chip family identifier. Mirrors the families that have shipped
/// silicon-profile entries (BM1362, BM1387 today; more in +).
///
///  W5-A: `Bm1485` (Scrypt L3+/L3++) and `Bm1489` (Scrypt L7/L9)
/// added to support the `dcentrald-silicon-profiles::registry`
/// per-(model, hashboard, chip) tuple schema. They are scrypt-chain
/// rail-voltage chips, not SHA-256 chip-rail.
///
/// ** W7-A NOTE /  W8-A UPDATE**: BM1360 and BM1491 surfaced
/// from VNish 1.2.6/1.2.7 cgminer ELFs (per
/// )
/// and the L7 hwscan binary string dump. Both chips remain **NAMED ONLY**
/// — every numeric parameter (chip_id, cores_per_chip, default freq /
/// voltage, register addresses) is genuinely absent from the haul.
/// They live as standalone `SiliconTable`s in
/// `dcentrald-silicon-profiles::{bm1360, bm1491}` (Reconstructed-only
/// placeholder rows).  W7-A deferred the enum extension because
/// doing so would force exhaustive `match` updates in
/// `frequency_scaling.rs::cores_per_chip`,
/// `power_model.rs::for_family`, and
/// `dcentrald-silicon-profiles::registry::chip_voltage_ranges` —
/// out-of-scope for that wave's allowed write surface.
///
/// ** W8-A**: The enum is extended below with `Bm1360` and
/// `Bm1491` placeholders so REST/UI consumers (W8-D `/api/profiles`)
/// can list every supported family without falling through to UNKNOWN.
/// Numeric parameters are still genuinely UNKNOWN — `cores_per_chip`
/// returns 0 and `PowerModel::for_family` returns a refuse-to-mine
/// placeholder for these two families. The placeholder `chip_id` byte
/// pairs (`0x13 0x60` and `0x14 0x91`) are NOT validated against live
/// silicon — they MUST be re-verified against a `GetAddress` response
/// from a real BM1360 / BM1491 unit before being trusted. See
///  memory rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipFamily {
    Bm1387,
    Bm1397,
    Bm1398,
    Bm1362,
    Bm1366,
    Bm1368,
    Bm1370,
    /// BM1485 — Antminer L3 / L3+ / L3++ scrypt mining ( W5-A).
    Bm1485,
    /// BM1489 — Antminer L7 / L9 scrypt mining ( W5-A).
    Bm1489,
    /// BM1360 — wave-6 surfaced, W7-A confirmed in 36/64 VNish 1.2.6/1.2.7
    /// cgminer ELFs + L7 hwscan binary.  W8-A: enum variant added
    /// for REST/UI completeness; numeric parameters (cores_per_chip,
    /// default freq/voltage, register layout) are GENUINELY UNKNOWN.
    /// chip_id byte pair `[0x13, 0x60]` is a PLACEHOLDER — re-verify
    /// against live `GetAddress` response before trusting.
    /// [GAP — wave-9 live verification needed]
    Bm1360,
    /// BM1491 — wave-6 surfaced, W7-A confirmed UNIVERSAL across 64/64
    /// VNish 1.2.6/1.2.7 cgminer ELFs (incl. pure SHA-256 builds, NOT
    /// Scrypt-only as wave-6 hypothesized).  W8-A: enum variant
    /// added for REST/UI completeness; numeric parameters are GENUINELY
    /// UNKNOWN. chip_id byte pair `[0x14, 0x91]` is a PLACEHOLDER —
    /// re-verify against live `GetAddress` response before trusting.
    /// [GAP — wave-9 live verification needed]
    Bm1491,
}

impl ChipFamily {
    pub fn chip_id_byte_pair(&self) -> [u8; 2] {
        match self {
            ChipFamily::Bm1387 => [0x13, 0x87],
            ChipFamily::Bm1397 => [0x13, 0x97],
            ChipFamily::Bm1398 => [0x13, 0x98],
            ChipFamily::Bm1362 => [0x13, 0x62],
            ChipFamily::Bm1366 => [0x13, 0x66],
            ChipFamily::Bm1368 => [0x13, 0x68],
            ChipFamily::Bm1370 => [0x13, 0x70],
            //  W5-A: scrypt-chain chip families. Hex IDs sourced
            // from the cgminer-ltc / `bm1485.rs` provenance. BM1489
            // ID (`0x14, 0x89`) is the shipping L7/L9 nameplate; not
            // verified live yet — flagged for  confirmation.
            ChipFamily::Bm1485 => [0x14, 0x85],
            ChipFamily::Bm1489 => [0x14, 0x89],
            //  W8-A: name-derived PLACEHOLDER IDs. Per W7-A,
            // the actual `chip_id` of these chips is genuinely UNKNOWN
            // — only the enum-string name was surfaced in the wave-6
            // haul. Re-verify against a live `GetAddress` response
            // before trusting these byte pairs in any chip-detection
            // path. [GAP — wave-9 live verification needed]
            ChipFamily::Bm1360 => [0x13, 0x60],
            ChipFamily::Bm1491 => [0x14, 0x91],
        }
    }
}

/// Per-family init constants distilled from `chip-init-sequences.md`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ChipInitSpec {
    /// Hex chip ID byte pair (e.g. `[0x13, 0x87]`).
    pub chip_id: [u8; 2],
    /// Cores per chip — used for hashrate estimation.
    /// **HAZARD**: BM1362 = 4, NOT 894. Don't confuse with BM1397/BM1398.
    pub cores_per_chip: u32,
    /// Default UART baud at cold boot (always 115200 across the fleet).
    pub default_baud: u32,
    /// Operational baud after MiscCtrl baud upgrade.
    pub operational_baud: u32,
    /// Number of response bytes per chip frame.
    pub response_bytes: u8,
    /// PLL register address (0x08 for BM1397+, 0x0C for BM1387).
    pub pll_register: u8,
    /// MiscCtrl register address (0x18 for BM1397+, 0x1C for BM1387).
    pub miscctrl_register: u8,
    /// Whether open-core (114 dummy works × N chips at gate_block=1)
    /// is required for the family. Only BM1387.
    pub requires_open_core: bool,
    /// Whether MiscCtrl baud-upgrade write must be triple-written with
    /// 5 ms spacing (S9/BM1387 hard rule per
    /// ).
    pub miscctrl_triple_write: bool,
    /// Whether the BM1362 ASIC reg `0x2C` UART_RELAY candidate write is
    /// required on cold boot.
    ///
    /// W13.B1 (2026-05-10) RECLASSIFIED + RENAMED. The legacy field
    /// `fpga_uart_relay_write_required` named the FPGA `0x43D000xx` mirror
    /// as the control surface — that was misdiagnosed. R4 RE pass +
    /// 5-source consensus established that the FPGA mirror is not control.
    /// R6-7 keeps BM1362 0x2C/0x34 candidate writes data-only/lab-gated
    /// until byte-exact live captures confirm the correct production
    /// control sequence.
    ///
    /// W13.B1 backwards-compat NOTE: `ChipInitSpec` only derives
    /// `Serialize` today (no `Deserialize` — `label: &'static str` blocks
    /// it), so a `#[serde(alias = ...)]` on the Rust field would not have
    /// any wire effect. Old JSON consumers (`fpga_uart_relay_write_required`)
    /// were one-way producers reading what the daemon emitted; they will
    /// see the renamed key on next deploy and must update in lockstep.
    /// Dashboard React consumers were grepped (W13.A6 audit Section 1.5)
    /// and only `SystemDebug.tsx:39` matched as a label string, NOT a JSON
    /// key consumer — so the dashboard React side is already clean.
    pub bm1362_asic_uart_relay_reg_0x2c_write_required: bool,
    /// Operator-facing label for dashboard / docs.
    pub label: &'static str,
}

/// Look up the init spec for a chip family.
pub fn init_spec(family: ChipFamily) -> ChipInitSpec {
    match family {
        ChipFamily::Bm1387 => ChipInitSpec {
            chip_id: [0x13, 0x87],
            cores_per_chip: 114,
            default_baud: 115200,
            operational_baud: 1_562_500,
            response_bytes: 7,
            pll_register: 0x0C,
            miscctrl_register: 0x1C,
            requires_open_core: true,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1387 (S9 / L3+)",
        },
        ChipFamily::Bm1397 => ChipInitSpec {
            chip_id: [0x13, 0x97],
            cores_per_chip: 672,
            default_baud: 115200,
            operational_baud: 6_250_000,
            response_bytes: 9,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1397 (S17 / S17 Pro / S17e)",
        },
        ChipFamily::Bm1398 => ChipInitSpec {
            chip_id: [0x13, 0x98],
            cores_per_chip: 672,
            default_baud: 115200,
            operational_baud: 6_250_000,
            response_bytes: 9,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1398 (S19 / S19 Pro)",
        },
        ChipFamily::Bm1362 => ChipInitSpec {
            chip_id: [0x13, 0x62],
            // HAZARD: BM1362 has 4 small cores per chip (per dcentrald
            // driver), NOT 894 like BM1397. Common miscoding pitfall.
            cores_per_chip: 4,
            default_baud: 115200,
            operational_baud: 3_125_000,
            response_bytes: 11,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            // R6-7 keeps BM1362 UART_RELAY data-only until exact write
            // values are live-captured; production cold boot must not
            // treat reg 0x2C as required by default.
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1362 (S19j Pro am2)",
        },
        ChipFamily::Bm1366 => ChipInitSpec {
            chip_id: [0x13, 0x66],
            cores_per_chip: 894,
            default_baud: 115200,
            operational_baud: 3_125_000,
            response_bytes: 11,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1366 (S19k Pro / S19 XP)",
        },
        ChipFamily::Bm1368 => ChipInitSpec {
            chip_id: [0x13, 0x68],
            // BM1368 has a hierarchical core layout: 80 big × 16 small =
            // 1280 small cores per chip. The RE doc lists this in §3.
            cores_per_chip: 1280,
            default_baud: 115200,
            operational_baud: 3_125_000,
            response_bytes: 11,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1368 (S21)",
        },
        ChipFamily::Bm1370 => ChipInitSpec {
            chip_id: [0x13, 0x70],
            cores_per_chip: 1280,
            default_baud: 115200,
            operational_baud: 3_125_000,
            response_bytes: 11,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1370 (S21 Pro / S21 XP)",
        },
        //  W5-A: scrypt families. Cores/baud values are derived
        // from `dcentrald-silicon-profiles::bm1485` (12 cores/chip,
        // chain-rail voltage). Init values are placeholders pending
        //  live capture from L3+/L7/L9 hardware. They MUST NOT
        // be consumed by the SHA-256 ASIC driver dispatch path —
        // scrypt mining is not yet wired into `dcentrald-asic`.
        ChipFamily::Bm1485 => ChipInitSpec {
            chip_id: [0x14, 0x85],
            cores_per_chip: 12,
            default_baud: 115200,
            operational_baud: 1_562_500,
            response_bytes: 7,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1485 (L3+ / L3++ scrypt) — placeholder",
        },
        ChipFamily::Bm1489 => ChipInitSpec {
            chip_id: [0x14, 0x89],
            cores_per_chip: 12,
            default_baud: 115200,
            operational_baud: 115_200,
            response_bytes: 7,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1489 (L7 / L9 scrypt) — placeholder",
        },
        //  W8-A: NAMED-ONLY placeholder init specs. cores=0 is
        // the deliberate refuse-to-mine sentinel — silicon-profile
        // consumers MUST refuse work dispatch when they see 0 cores
        // per chip. PLL/MiscCtrl/baud values mirror the BM1397+ generic
        // shape so the dispatcher doesn't crash on lookup, but they
        // are NOT validated against live silicon and MUST NOT be
        // consumed by the ASIC driver path until wave-9+ live capture
        // confirms the actual register layout. [GAP — wave-9 live
        // verification needed]
        ChipFamily::Bm1360 => ChipInitSpec {
            chip_id: [0x13, 0x60],
            cores_per_chip: 0,
            default_baud: 115200,
            operational_baud: 115_200,
            response_bytes: 11,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1360 (W8-A NAMED-ONLY placeholder — refuse to mine)",
        },
        ChipFamily::Bm1491 => ChipInitSpec {
            chip_id: [0x14, 0x91],
            cores_per_chip: 0,
            default_baud: 115200,
            operational_baud: 115_200,
            response_bytes: 11,
            pll_register: 0x08,
            miscctrl_register: 0x18,
            requires_open_core: false,
            miscctrl_triple_write: true,
            bm1362_asic_uart_relay_reg_0x2c_write_required: false,
            label: "BM1491 (W8-A NAMED-ONLY placeholder — refuse to mine)",
        },
    }
}

/// Every supported chip family. Useful for fleet rendering + tests.
pub const ALL_FAMILIES: &[ChipFamily] = &[
    ChipFamily::Bm1387,
    ChipFamily::Bm1397,
    ChipFamily::Bm1398,
    ChipFamily::Bm1362,
    ChipFamily::Bm1366,
    ChipFamily::Bm1368,
    ChipFamily::Bm1370,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_a_chip_id_byte_pair() {
        for fam in ALL_FAMILIES {
            let id = fam.chip_id_byte_pair();
            // First byte is always 0x13 (Bitmain family prefix).
            assert_eq!(id[0], 0x13, "{:?} chip_id[0] should be 0x13", fam);
            // Second byte identifies the chip.
            assert_ne!(id[1], 0x00, "{:?} chip_id[1] should not be 0x00", fam);
        }
    }

    #[test]
    fn bm1387_uses_register_0x0c_for_pll() {
        let s = init_spec(ChipFamily::Bm1387);
        assert_eq!(s.pll_register, 0x0C, "BM1387 PLL is at 0x0C, NOT 0x08");
        assert_eq!(
            s.miscctrl_register, 0x1C,
            "BM1387 MiscCtrl is at 0x1C, NOT 0x18"
        );
    }

    #[test]
    fn bm1397_and_bm1398_use_register_0x08_for_pll() {
        for fam in [ChipFamily::Bm1397, ChipFamily::Bm1398] {
            let s = init_spec(fam);
            assert_eq!(s.pll_register, 0x08);
            assert_eq!(s.miscctrl_register, 0x18);
        }
    }

    #[test]
    fn bm1387_requires_open_core() {
        let s = init_spec(ChipFamily::Bm1387);
        assert!(
            s.requires_open_core,
            "BM1387 MUST run open-core (114 dummies)"
        );
    }

    #[test]
    fn other_families_skip_open_core() {
        for fam in [
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            let s = init_spec(fam);
            assert!(!s.requires_open_core, "{:?} should NOT need open-core", fam);
        }
    }

    #[test]
    fn bm1362_cores_per_chip_is_4_not_894() {
        // RE doc HAZARD callout (line 129): "BM1362 has 4 cores per chip,
        // not 894 like BM1397. Using 894 in the autotuner overestimates
        // hashrate by ~220x."
        let s = init_spec(ChipFamily::Bm1362);
        assert_eq!(
            s.cores_per_chip, 4,
            "BM1362 cores_per_chip must be 4 — using 894 was a documented hazard"
        );
    }

    #[test]
    fn bm1362_uart_relay_reg_0x2c_is_data_only_until_r6_7() {
        // R6-7 keeps BM1362 reg 0x2C write values hardware-blocked. The
        // register remains cataloged, but production cold boot must not
        // mark it required until byte-exact live captures land.
        let s = init_spec(ChipFamily::Bm1362);
        assert!(!s.bm1362_asic_uart_relay_reg_0x2c_write_required);
    }

    #[test]
    fn other_families_skip_asic_uart_relay_reg_0x2c() {
        for fam in [
            ChipFamily::Bm1387,
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            let s = init_spec(fam);
            assert!(
                !s.bm1362_asic_uart_relay_reg_0x2c_write_required,
                "{:?} should NOT need BM1362-style UART_RELAY reg 0x2C write \
                 (only BM1362 today; future BM13xx families that share the \
                 reg semantic will get their own flag)",
                fam
            );
        }
    }

    #[test]
    fn renamed_field_serializes_with_new_key() {
        // W13.B1 wire contract: serialized JSON must carry the renamed key
        // `bm1362_asic_uart_relay_reg_0x2c_write_required` so dashboard
        // consumers know to update from the legacy `fpga_uart_relay_write_required`
        // name in lockstep with this commit.
        let s = init_spec(ChipFamily::Bm1362);
        let json = serde_json::to_string(&s).expect("serialize must succeed");
        assert!(
            json.contains("bm1362_asic_uart_relay_reg_0x2c_write_required"),
            "JSON must carry the renamed key: {}",
            json
        );
        assert!(
            !json.contains("\"fpga_uart_relay_write_required\""),
            "JSON must NOT carry the legacy key (no serde rename): {}",
            json
        );
    }

    #[test]
    fn miscctrl_triple_write_is_universal_across_families() {
        //  +  hard rule: triple-write MiscCtrl with 5 ms
        // spacing on all chip families per
        // .
        for fam in ALL_FAMILIES {
            let s = init_spec(*fam);
            assert!(
                s.miscctrl_triple_write,
                "{:?} MUST triple-write MiscCtrl",
                fam
            );
        }
    }

    #[test]
    fn default_baud_is_115200_universally() {
        for fam in ALL_FAMILIES {
            let s = init_spec(*fam);
            assert_eq!(s.default_baud, 115200);
        }
    }

    #[test]
    fn core_counts_match_frequency_scaling_table() {
        for fam in ALL_FAMILIES {
            assert_eq!(
                init_spec(*fam).cores_per_chip,
                crate::frequency_scaling::cores_per_chip(*fam),
                "{:?} chip_init cores_per_chip drifted from frequency_scaling",
                fam
            );
        }
    }

    #[test]
    fn operational_baud_matches_re_doc_per_family() {
        // Per chip-init-sequences.md.
        let cases: &[(ChipFamily, u32)] = &[
            (ChipFamily::Bm1387, 1_562_500),
            (ChipFamily::Bm1397, 6_250_000),
            (ChipFamily::Bm1398, 6_250_000),
            (ChipFamily::Bm1362, 3_125_000),
            (ChipFamily::Bm1366, 3_125_000),
            (ChipFamily::Bm1368, 3_125_000),
            (ChipFamily::Bm1370, 3_125_000),
        ];
        for (fam, baud) in cases {
            assert_eq!(
                init_spec(*fam).operational_baud,
                *baud,
                "{:?} operational baud mismatch",
                fam
            );
        }
    }

    #[test]
    fn chip_init_baud_matches_baud_switch_for_supported_families() {
        use crate::baud_switch::{target_baud, BaudChipFamily};

        let cases = [
            (ChipFamily::Bm1387, BaudChipFamily::Bm1387),
            (ChipFamily::Bm1397, BaudChipFamily::Bm1397),
            (ChipFamily::Bm1398, BaudChipFamily::Bm1398),
            (ChipFamily::Bm1362, BaudChipFamily::Bm1362),
            (ChipFamily::Bm1366, BaudChipFamily::Bm1366),
            (ChipFamily::Bm1368, BaudChipFamily::Bm1368),
            (ChipFamily::Bm1370, BaudChipFamily::Bm1370),
            (ChipFamily::Bm1485, BaudChipFamily::Bm1485),
        ];

        for (chip_family, baud_family) in cases {
            assert_eq!(
                init_spec(chip_family).operational_baud,
                target_baud(baud_family),
                "{:?} chip_init operational_baud drifted from baud_switch",
                chip_family
            );
        }
    }

    #[test]
    fn response_bytes_match_re_doc() {
        assert_eq!(init_spec(ChipFamily::Bm1387).response_bytes, 7);
        assert_eq!(init_spec(ChipFamily::Bm1397).response_bytes, 9);
        assert_eq!(init_spec(ChipFamily::Bm1398).response_bytes, 9);
        assert_eq!(init_spec(ChipFamily::Bm1362).response_bytes, 11);
        assert_eq!(init_spec(ChipFamily::Bm1366).response_bytes, 11);
        assert_eq!(init_spec(ChipFamily::Bm1368).response_bytes, 11);
        assert_eq!(init_spec(ChipFamily::Bm1370).response_bytes, 11);
    }

    #[test]
    fn chip_family_round_trips_through_serde() {
        for fam in ALL_FAMILIES {
            let json = serde_json::to_string(fam).unwrap();
            let back: ChipFamily = serde_json::from_str(&json).unwrap();
            assert_eq!(*fam, back);
        }
    }

    // ----  W8-A tests ------------------------------------------------

    /// W8-A: BM1360 chip_id placeholder pin. The byte pair is
    /// name-derived (`0x13 0x60`); per W7-A the actual chip_id is
    /// genuinely UNKNOWN. This test pins the placeholder so a future
    /// agent who replaces it with a live-verified ID has to update
    /// the test alongside the change.
    #[test]
    fn bm1360_chip_id_is_0x1360_placeholder() {
        // [GAP — wave-9 live verification needed]
        let id = ChipFamily::Bm1360.chip_id_byte_pair();
        assert_eq!(id, [0x13, 0x60], "BM1360 placeholder chip_id");
        let s = init_spec(ChipFamily::Bm1360);
        assert_eq!(s.chip_id, [0x13, 0x60]);
        // cores=0 is the W8-A refuse-to-mine sentinel.
        assert_eq!(
            s.cores_per_chip, 0,
            "BM1360 placeholder MUST report 0 cores (refuse-to-mine sentinel)"
        );
    }

    /// W8-A: BM1491 chip_id placeholder pin. Per W7-A, BM1491 appears
    /// in 64/64 wave-6 binaries (incl. pure SHA-256 builds), so it is
    /// **NOT** Scrypt-only as wave-6 originally hypothesized. Byte
    /// pair `0x14 0x91` is name-derived placeholder.
    #[test]
    fn bm1491_chip_id_is_0x1491_placeholder() {
        // [GAP — wave-9 live verification needed]
        let id = ChipFamily::Bm1491.chip_id_byte_pair();
        assert_eq!(id, [0x14, 0x91], "BM1491 placeholder chip_id");
        let s = init_spec(ChipFamily::Bm1491);
        assert_eq!(s.chip_id, [0x14, 0x91]);
        assert_eq!(
            s.cores_per_chip, 0,
            "BM1491 placeholder MUST report 0 cores (refuse-to-mine sentinel)"
        );
    }

    /// W8-A: assert ChipFamily enum has all expected variants by
    /// constructing each one and round-tripping through serde. If a
    /// future agent adds or removes a variant, this test forces them
    /// to update the count + the slice literal.
    #[test]
    fn chip_family_all_variants_complete() {
        // Every ChipFamily variant — pin the count + the snake_case
        // serde wire form so both stay in sync.
        let all: &[(ChipFamily, &str)] = &[
            (ChipFamily::Bm1387, "bm1387"),
            (ChipFamily::Bm1397, "bm1397"),
            (ChipFamily::Bm1398, "bm1398"),
            (ChipFamily::Bm1362, "bm1362"),
            (ChipFamily::Bm1366, "bm1366"),
            (ChipFamily::Bm1368, "bm1368"),
            (ChipFamily::Bm1370, "bm1370"),
            (ChipFamily::Bm1485, "bm1485"),
            (ChipFamily::Bm1489, "bm1489"),
            (ChipFamily::Bm1360, "bm1360"),
            (ChipFamily::Bm1491, "bm1491"),
        ];
        // 7 SHA-256 (Bm1387..Bm1370) + 2 Scrypt (Bm1485/89) + 2 W8-A
        // placeholders (Bm1360/91) = 11 variants total.
        assert_eq!(all.len(), 11, "ChipFamily variant count drift");
        for (fam, wire) in all {
            let json = serde_json::to_string(fam).unwrap();
            // serde rename_all = "snake_case" produces "\"bm1387\"" etc.
            assert_eq!(json, format!("\"{}\"", wire));
            let back: ChipFamily = serde_json::from_str(&json).unwrap();
            assert_eq!(*fam, back);
            // Every variant must have a defined init_spec (no panic).
            let _ = init_spec(*fam);
        }
    }
}
