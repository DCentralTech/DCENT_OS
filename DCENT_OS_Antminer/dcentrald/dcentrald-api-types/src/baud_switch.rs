//!  baud-A — per-chip baud-upgrade plan + triple-write rule (HAL-free).
//!
//! Source RE evidence:
//!
//! (237 lines).
//!
//! All BM13xx / BM14xx chips boot at **115200 8N1** (true rate 115740 bps
//! from `25 MHz / (26+1) / 8`) and must upgrade to a higher operational
//! baud (1.5625 / 3.125 / 6.25 Mbaud) for sustained mining. The transition
//! is the single most fragile step in chain init — a single dropped
//! MiscCtrl byte was the root cause of the 75-second zero-nonce stall on
//! S9.
//!
//! This module exposes a HAL-free per-chip plan: which register holds the
//! baud divisor, what value to write, what FPGA divider matches the new
//! baud, the triple-write rule, and the inter-/post-write timing. The
//! BM1362 wire frame is byte-pinned because we have a verified live
//! capture from .139 (2026-04-24).
//!
//! Second witness (provenance hardening, A53 / knowledge-goldmine lane s18):
//! an INDEPENDENT 2023-12-06 Saleae Logic logic-analyzer capture of a live
//! Antminer S19j Pro (BM1362) chain UART decodes the running chain baud as
//! exactly 3,125,000 (3.125 Mbaud) on BOTH chain wires, with the 115200
//! enumeration window confirmed by a paired 420 MHz-vs-545 MHz capture pair
//! (baud is decoupled from PLL freq). That corroborates `target_baud(Bm1362)`
//! from a source entirely separate from the `a lab unit` capture. See
//!
//! (facts S18-F3/F4) and the reusable parser `tools/sal_decode.py`.
//!
//! HAZARD pinned by tests:
//! - `0x40C100B7` is FORBIDDEN for BM1362 — `MISC_CONTROL_INIT | (1 << 16)`
//!   is a no-op because bit 16 is already set; the chain stays at 115200
//!   silently.
//! - Single-write MiscCtrl is BANNED across all chips. Triple-write with
//!   ≥5 ms spacing is the only safe pattern.

use serde::{Deserialize, Serialize};

/// Chip family covered by the baud-upgrade catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaudChipFamily {
    /// S9 am1: 115200 → 1.5625 Mbaud, FPGA divider 0x07.
    Bm1387,
    /// S17 / S19: 115200 → 6.25 Mbaud, PLL3-derived divider.
    Bm1397,
    /// S19 (Pro): 115200 → 6.25 Mbaud, PLL3-derived divider.
    Bm1398,
    /// S19j Pro am2: 115200 → 3.125 Mbaud, FPGA divider 0x03.
    Bm1362,
    /// S19k Pro / S19 XP / variants: 115200 → 3.125 Mbaud (Bitmain) or 1 Mbaud (ESP-Miner).
    Bm1366,
    /// S19j Pro+: 115200 → 3.125 Mbaud (Bitmain) or 1 Mbaud (ESP-Miner).
    Bm1368,
    /// S21 / S21 Pro: 115200 → 3.125 Mbaud (Bitmain) or 1 Mbaud (ESP-Miner).
    Bm1370,
    /// L3+ / L7 scrypt: 115200 → 1.5625 Mbaud, BT8D divider.
    Bm1485,
}

/// Canonical boot baud for every BM13xx/BM14xx chip.
pub const BOOT_BAUD: u32 = 115_200;

/// Triple-write parameters (load-bearing safety contract).
pub const TRIPLE_WRITE_COUNT: u8 = 3;
pub const INTER_WRITE_DELAY_MS: u32 = 5;
pub const POST_THIRD_WAIT_MS: u32 = 5;
pub const HOST_SWITCH_SETTLE_MS: u32 = 50;

/// HAZARD value forbidden for BM1362: bit 16 already set in
/// `MISC_CONTROL_INIT`, so OR-ing it again is a silent no-op.
pub const FORBIDDEN_BM1362_MISCCTRL: u32 = 0x40C100B7;

/// Per-chip baud-upgrade plan. All fields are HAL-free; runtime adapter
/// turns this into actual MMIO/UART writes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BaudPlan {
    pub family: BaudChipFamily,
    pub boot_baud: u32,
    pub target_baud: u32,
    /// ASIC register that holds the baud divisor (e.g. `0x18` for MiscCtrl).
    pub tx_register: u8,
    /// 32-bit value to write into `tx_register` to request the target baud.
    pub register_value: u32,
    /// FPGA-side divider to apply AFTER the chain commits the baud change.
    /// `None` means the host kernel UART is used (Amlogic / direct termios).
    pub fpga_divider: Option<u32>,
    /// Number of times to write the baud register at boot baud.
    pub triple_write_count: u8,
    /// Delay between successive writes.
    pub inter_write_delay_ms: u32,
    /// Wait between third write and host-side baud switch.
    pub post_third_wait_ms: u32,
    /// Wait after host-side switch before next register write.
    pub host_switch_settle_ms: u32,
}

/// Canonical Bitmain operational baud per chip.
pub const fn target_baud(family: BaudChipFamily) -> u32 {
    match family {
        BaudChipFamily::Bm1387 => 1_562_500,
        BaudChipFamily::Bm1397 | BaudChipFamily::Bm1398 => 6_250_000,
        BaudChipFamily::Bm1362
        | BaudChipFamily::Bm1366
        | BaudChipFamily::Bm1368
        | BaudChipFamily::Bm1370 => 3_125_000,
        BaudChipFamily::Bm1485 => 1_562_500,
    }
}

/// FPGA-side baud divider that matches the operational baud, when an FPGA
/// UART is in the path (Zynq am1/am2). Amlogic platforms return None.
pub const fn fpga_divider(family: BaudChipFamily) -> Option<u32> {
    match family {
        BaudChipFamily::Bm1387 | BaudChipFamily::Bm1485 => Some(0x07),
        BaudChipFamily::Bm1362 => Some(0x03),
        // BM1397/BM1398 use a PLL3-derived divider not encoded as a flat
        // value — runtime adapter computes it from the PLL config.
        BaudChipFamily::Bm1397 | BaudChipFamily::Bm1398 => None,
        // BM1366/68/70 are Amlogic (kernel UART termios) — no FPGA divider.
        BaudChipFamily::Bm1366 | BaudChipFamily::Bm1368 | BaudChipFamily::Bm1370 => None,
    }
}

/// Per-chip register where the baud divisor lives (Bitmain canonical).
pub const fn baud_register(family: BaudChipFamily) -> u8 {
    match family {
        // BM1387 / BM1485: MiscCtrl @ 0x1C
        BaudChipFamily::Bm1387 | BaudChipFamily::Bm1485 => 0x1C,
        // BM1397 / BM1398: MiscCtrl @ 0x18
        BaudChipFamily::Bm1397 | BaudChipFamily::Bm1398 => 0x18,
        // BM1362: broadcast 0x28 then triple 0x18 (we surface 0x18 as the
        // register the triple-write targets — 0x28 is a separate one-shot).
        BaudChipFamily::Bm1362 => 0x18,
        // BM1366 / BM1368 / BM1370: 0x28 (Bitmain) — single field per
        // chip-family RE doc.
        BaudChipFamily::Bm1366 | BaudChipFamily::Bm1368 | BaudChipFamily::Bm1370 => 0x28,
    }
}

/// Canonical 32-bit register value for the operational baud.
pub const fn register_value(family: BaudChipFamily) -> u32 {
    match family {
        // BM1387: MiscCtrl with `baud_div=1`, `not_set_baud=0` — the gate-block
        // exact value is composed by the runtime; surface the canonical
        // 0x4020_0180 anchor pinned by the S9 75-s cliff fix.
        BaudChipFamily::Bm1387 => 0x4020_0180,
        // BM1397 / BM1398 — verified 6.25 Mbaud value
        BaudChipFamily::Bm1397 | BaudChipFamily::Bm1398 => 0x0000_6031,
        // BM1362 — verified live .139: `0x18 = 0x00C100B0`
        BaudChipFamily::Bm1362 => 0x00C1_00B0,
        // BM1366/68/70 Bitmain 3.125 Mbaud
        BaudChipFamily::Bm1366 | BaudChipFamily::Bm1368 | BaudChipFamily::Bm1370 => 0x0000_3001,
        // BM1485 — BT8D pattern (RE-derived)
        BaudChipFamily::Bm1485 => 0x0000_6031,
    }
}

impl BaudPlan {
    /// Build the canonical plan for a chip family.
    pub fn canonical(family: BaudChipFamily) -> Self {
        Self {
            family,
            boot_baud: BOOT_BAUD,
            target_baud: target_baud(family),
            tx_register: baud_register(family),
            register_value: register_value(family),
            fpga_divider: fpga_divider(family),
            triple_write_count: TRIPLE_WRITE_COUNT,
            inter_write_delay_ms: INTER_WRITE_DELAY_MS,
            post_third_wait_ms: POST_THIRD_WAIT_MS,
            host_switch_settle_ms: HOST_SWITCH_SETTLE_MS,
        }
    }

    /// True iff the host UART is driven by a Zynq FPGA divider (am1/am2)
    /// rather than a kernel termios `BOTHER` (Amlogic).
    pub fn uses_fpga_divider(&self) -> bool {
        self.fpga_divider.is_some()
    }

    /// True iff this is the BM1362 path that REQUIRES the broadcast 0x28
    /// preamble before the triple-write of 0x18.
    pub fn requires_bm1362_preamble(&self) -> bool {
        matches!(self.family, BaudChipFamily::Bm1362)
    }
}

// ---------------------------------------------------------------------------
// BM1362 byte-correct preamble + triple-write frames (verified live .139)
// ---------------------------------------------------------------------------

/// BM1362 broadcast Fast UART preamble at 115200 baud.
/// `55 AA 51 09 00 28 00 00 30 11 12`
pub const BM1362_PREAMBLE_FRAME: [u8; 11] = [
    0x55, 0xAA, 0x51, 0x09, 0x00, 0x28, 0x00, 0x00, 0x30, 0x11, 0x12,
];

/// BM1362 MiscCtrl baud frame at 115200 baud (write three times).
/// `55 AA 51 09 00 18 00 C1 00 B0 0B`
pub const BM1362_MISCCTRL_FRAME: [u8; 11] = [
    0x55, 0xAA, 0x51, 0x09, 0x00, 0x18, 0x00, 0xC1, 0x00, 0xB0, 0x0B,
];

/// True iff a candidate value is the forbidden BM1362 hazard pattern.
pub fn is_forbidden_bm1362_value(candidate: u32) -> bool {
    candidate == FORBIDDEN_BM1362_MISCCTRL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_baud_is_115200() {
        assert_eq!(BOOT_BAUD, 115_200);
    }

    #[test]
    fn triple_write_constants_match_re_doc() {
        // baud-switching-analysis.md: write 3x with 5 ms spacing, then
        // 5 ms wait, then host-side switch, then 50 ms PLL settle.
        assert_eq!(TRIPLE_WRITE_COUNT, 3);
        assert_eq!(INTER_WRITE_DELAY_MS, 5);
        assert_eq!(POST_THIRD_WAIT_MS, 5);
        assert_eq!(HOST_SWITCH_SETTLE_MS, 50);
    }

    #[test]
    fn target_baud_matches_re_doc_per_family() {
        assert_eq!(target_baud(BaudChipFamily::Bm1387), 1_562_500);
        assert_eq!(target_baud(BaudChipFamily::Bm1397), 6_250_000);
        assert_eq!(target_baud(BaudChipFamily::Bm1398), 6_250_000);
        assert_eq!(target_baud(BaudChipFamily::Bm1362), 3_125_000);
        assert_eq!(target_baud(BaudChipFamily::Bm1366), 3_125_000);
        assert_eq!(target_baud(BaudChipFamily::Bm1368), 3_125_000);
        assert_eq!(target_baud(BaudChipFamily::Bm1370), 3_125_000);
        assert_eq!(target_baud(BaudChipFamily::Bm1485), 1_562_500);
    }

    #[test]
    fn fpga_divider_present_for_zynq_chips_only() {
        // Zynq am1 (BM1387) + am2 (BM1362) + scrypt am1 (BM1485) use FPGA UART.
        assert_eq!(fpga_divider(BaudChipFamily::Bm1387), Some(0x07));
        assert_eq!(fpga_divider(BaudChipFamily::Bm1362), Some(0x03));
        assert_eq!(fpga_divider(BaudChipFamily::Bm1485), Some(0x07));
        // BM1397/98 use PLL3-derived (not a flat divider value).
        assert_eq!(fpga_divider(BaudChipFamily::Bm1397), None);
        assert_eq!(fpga_divider(BaudChipFamily::Bm1398), None);
        // Amlogic chips use kernel termios — no FPGA divider.
        assert_eq!(fpga_divider(BaudChipFamily::Bm1366), None);
        assert_eq!(fpga_divider(BaudChipFamily::Bm1368), None);
        assert_eq!(fpga_divider(BaudChipFamily::Bm1370), None);
    }

    #[test]
    fn baud_register_matches_per_family_table() {
        assert_eq!(baud_register(BaudChipFamily::Bm1387), 0x1C);
        assert_eq!(baud_register(BaudChipFamily::Bm1485), 0x1C);
        assert_eq!(baud_register(BaudChipFamily::Bm1397), 0x18);
        assert_eq!(baud_register(BaudChipFamily::Bm1398), 0x18);
        assert_eq!(baud_register(BaudChipFamily::Bm1362), 0x18);
        assert_eq!(baud_register(BaudChipFamily::Bm1366), 0x28);
        assert_eq!(baud_register(BaudChipFamily::Bm1368), 0x28);
        assert_eq!(baud_register(BaudChipFamily::Bm1370), 0x28);
    }

    #[test]
    fn bm1362_register_value_matches_live_capture() {
        // baud-switching-analysis.md §"BM1362 exact byte sequence": 0x18 =
        // 0x00C100B0. Verified live .139 2026-04-24.
        assert_eq!(register_value(BaudChipFamily::Bm1362), 0x00C1_00B0);
    }

    #[test]
    fn bm1362_target_baud_has_independent_second_witness() {
        // A53 / knowledge-goldmine lane s18 (findings/s18-sal-capture-decode.md
        // S18-F3): an INDEPENDENT 2023-12-06 Saleae capture of a live S19j Pro
        // decodes the running BM1362 chain UART at exactly 3,125,000 baud on
        // BOTH chain wires — a second witness for target_baud(Bm1362), beyond
        // the .139 capture the module doc already cites. Pin the value so a
        // future "is 3.125 M right?" edit is caught (two independent RE sources
        // agree).
        assert_eq!(target_baud(BaudChipFamily::Bm1362), 3_125_000);
        // S18-F4: the boot/enumeration baud is decoupled from PLL freq (the
        // 420 MHz capture rode 115200, the 545 MHz capture rode 3.125 M).
        assert_eq!(BOOT_BAUD, 115_200);
    }

    #[test]
    fn bm1397_and_bm1398_share_the_same_misctrl_value() {
        // Both go to 6.25 Mbaud with the same 0x18 = 0x00006031 value.
        assert_eq!(
            register_value(BaudChipFamily::Bm1397),
            register_value(BaudChipFamily::Bm1398)
        );
        assert_eq!(register_value(BaudChipFamily::Bm1397), 0x0000_6031);
    }

    #[test]
    fn bm1366_68_70_share_3125m_value() {
        let v = register_value(BaudChipFamily::Bm1366);
        assert_eq!(v, 0x0000_3001);
        assert_eq!(register_value(BaudChipFamily::Bm1368), v);
        assert_eq!(register_value(BaudChipFamily::Bm1370), v);
    }

    #[test]
    fn forbidden_bm1362_hazard_pinned() {
        // baud-switching-analysis.md HAZARD: 0x40C100B7 is a no-op because
        // bit 16 is already set in MISC_CONTROL_INIT. Chain stays at 115200.
        assert_eq!(FORBIDDEN_BM1362_MISCCTRL, 0x40C1_00B7);
        assert!(is_forbidden_bm1362_value(0x40C1_00B7));
        // The CORRECT value 0x00C100B0 must NOT trip the hazard predicate.
        assert!(!is_forbidden_bm1362_value(0x00C1_00B0));
    }

    #[test]
    fn bm1362_preamble_frame_byte_pinned() {
        // Live-verified .139 2026-04-24: broadcast Fast UART preamble.
        let expected: [u8; 11] = [
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x28, 0x00, 0x00, 0x30, 0x11, 0x12,
        ];
        assert_eq!(BM1362_PREAMBLE_FRAME, expected);
    }

    #[test]
    fn bm1362_misctrl_frame_byte_pinned() {
        // Live-verified .139 2026-04-24: MiscCtrl triple-write frame.
        // 0x55 0xAA preamble, 0x51 cmd, 0x09 length, 0x00 chip-addr,
        // 0x18 register, 0x00 0xC1 0x00 0xB0 = 0x00C100B0 big-endian,
        // 0x0B CRC trailer.
        let expected: [u8; 11] = [
            0x55, 0xAA, 0x51, 0x09, 0x00, 0x18, 0x00, 0xC1, 0x00, 0xB0, 0x0B,
        ];
        assert_eq!(BM1362_MISCCTRL_FRAME, expected);
    }

    #[test]
    fn baud_plan_canonical_round_trips_through_serde() {
        // BaudPlan is Serialize-only (constructed from const tables) — cover
        // each family.
        for family in [
            BaudChipFamily::Bm1387,
            BaudChipFamily::Bm1397,
            BaudChipFamily::Bm1398,
            BaudChipFamily::Bm1362,
            BaudChipFamily::Bm1366,
            BaudChipFamily::Bm1368,
            BaudChipFamily::Bm1370,
            BaudChipFamily::Bm1485,
        ] {
            let plan = BaudPlan::canonical(family);
            assert_eq!(plan.boot_baud, 115_200);
            assert_eq!(plan.triple_write_count, 3);
            // Serialize-only sanity: produces a valid object with all
            // canonical fields.
            let json = serde_json::to_value(&plan).unwrap();
            assert!(json.get("family").is_some());
            assert!(json.get("target_baud").is_some());
            assert!(json.get("triple_write_count").is_some());
        }
    }

    #[test]
    fn bm1362_plan_flags_preamble_requirement() {
        let plan = BaudPlan::canonical(BaudChipFamily::Bm1362);
        assert!(plan.requires_bm1362_preamble());
        // Other families do NOT.
        for other in [
            BaudChipFamily::Bm1387,
            BaudChipFamily::Bm1397,
            BaudChipFamily::Bm1398,
            BaudChipFamily::Bm1366,
            BaudChipFamily::Bm1368,
            BaudChipFamily::Bm1370,
            BaudChipFamily::Bm1485,
        ] {
            assert!(!BaudPlan::canonical(other).requires_bm1362_preamble());
        }
    }

    #[test]
    fn uses_fpga_divider_matches_zynq_chips() {
        // Zynq am1/am2 use FPGA divider; Amlogic + PLL-derived chains do not.
        assert!(BaudPlan::canonical(BaudChipFamily::Bm1387).uses_fpga_divider());
        assert!(BaudPlan::canonical(BaudChipFamily::Bm1362).uses_fpga_divider());
        assert!(BaudPlan::canonical(BaudChipFamily::Bm1485).uses_fpga_divider());
        assert!(!BaudPlan::canonical(BaudChipFamily::Bm1397).uses_fpga_divider());
        assert!(!BaudPlan::canonical(BaudChipFamily::Bm1366).uses_fpga_divider());
        assert!(!BaudPlan::canonical(BaudChipFamily::Bm1370).uses_fpga_divider());
    }

    #[test]
    fn baud_chip_family_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&BaudChipFamily::Bm1387).unwrap(),
            "\"bm1387\""
        );
        assert_eq!(
            serde_json::to_string(&BaudChipFamily::Bm1362).unwrap(),
            "\"bm1362\""
        );
        assert_eq!(
            serde_json::to_string(&BaudChipFamily::Bm1485).unwrap(),
            "\"bm1485\""
        );
    }
}
