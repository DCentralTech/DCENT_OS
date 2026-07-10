//!  reg-A — BM13xx ASIC register-map catalog (HAL-free).
//!
//! Source RE evidence:
//!  §6 (BM1387)
//! and §7 (BM1397+).
//!
//! Two distinct register address spaces:
//! - **BM1387** (S9 era): compact 6-register set (0x00, 0x08, 0x0C,
//!   0x18, 0x1C, 0x20). PLL at 0x0C, MiscCtrl at 0x1C.
//! - **BM1397+** (S17 / S19 / S19j / S21 / S21 Pro): rich 30+ register
//!   set with named slots for PLL0-3, Ticket Mask, UART Relay, Error
//!   Flag, Nonce Error Counter, Analog Mux, etc. PLL0 at 0x08,
//!   MiscCtrl at 0x18.
//!
//! HAL-free: pure data tables + lookup. The runtime adapter inside
//! `dcentrald-asic` consumes these to compose register reads/writes.
//! The `RegisterId` discriminant is family-tagged so callers can't
//! accidentally use a BM1387 register address on BM1397+ silicon
//! (or vice versa).

use crate::chip_init::ChipFamily;
use serde::Serialize;

/// One register entry in a chip's register map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RegisterEntry {
    /// Register address (byte offset).
    pub address: u8,
    /// Operator-facing register name.
    pub name: &'static str,
    /// Reset / power-on default value (best-known, may vary per chip).
    pub reset_value: u32,
    /// One-line description for dashboard / docs.
    pub description: &'static str,
}

/// BM1387 register map (RE doc §6 lines 390-401).
pub const BM1387_REGISTERS: &[RegisterEntry] = &[
    RegisterEntry {
        address: 0x00,
        name: "ChipAddress",
        reset_value: 0x13870000,
        description: "CHIP_ID[31:16]=0x1387 + ADDR[7:0] chip address",
    },
    RegisterEntry {
        address: 0x08,
        name: "Hashrate",
        reset_value: 0x80000000,
        description: "Hash rate in 2^24 units",
    },
    RegisterEntry {
        address: 0x0C,
        name: "PllParameter",
        reset_value: 0,
        description: "FBDIV[31:24] / REFDIV[23:16] / POSTDIV1[15:8] / POSTDIV2[7:0]",
    },
    RegisterEntry {
        address: 0x18,
        name: "TicketMask",
        reset_value: 0x00000000,
        description: "Difficulty mask (bit-reversed encoding)",
    },
    RegisterEntry {
        address: 0x1C,
        name: "MiscControl",
        reset_value: 0,
        description: "Baud divider + clock + GPIO + AsicBoost enable",
    },
    RegisterEntry {
        address: 0x20,
        name: "I2cControl",
        reset_value: 0x01000000,
        description: "I2C bus master register passthrough",
    },
];

/// BM1397+ register map (RE doc §7 lines 451-490). Used by BM1397,
/// BM1398, BM1366, BM1368, BM1370, BM1362.
pub const BM1397_REGISTERS: &[RegisterEntry] = &[
    RegisterEntry {
        address: 0x00,
        name: "ChipAddress",
        reset_value: 0x13971800,
        description: "CHIP_ID[31:16] + CORE_NUM[15:8] + ADDR[7:0]",
    },
    RegisterEntry {
        address: 0x04,
        name: "HashRate",
        reset_value: 0x80000000,
        description: "Hash rate in 2^24 units",
    },
    RegisterEntry {
        address: 0x08,
        name: "Pll0Parameter",
        reset_value: 0xC0600161,
        description: "PLL0 — hash clock configuration",
    },
    RegisterEntry {
        address: 0x0C,
        name: "ChipNonceOffset",
        reset_value: 0x00000000,
        description: "Per-chip nonce offset",
    },
    RegisterEntry {
        address: 0x10,
        name: "HashCountingNumber",
        reset_value: 0x00000000,
        description: "Nonce range / hash counting config",
    },
    RegisterEntry {
        address: 0x14,
        name: "TicketMask",
        reset_value: 0x00000000,
        description: "Difficulty filter mask",
    },
    RegisterEntry {
        address: 0x18,
        name: "MiscControl",
        reset_value: 0x00003A01,
        description: "Baud + clock select + GPIO config",
    },
    RegisterEntry {
        address: 0x1C,
        name: "I2cControl",
        reset_value: 0x01000000,
        description: "I2C interface passthrough",
    },
    RegisterEntry {
        address: 0x20,
        name: "OrderedClockEnable",
        reset_value: 0x0000FFFF,
        description: "16 clock domain enable flags",
    },
    RegisterEntry {
        address: 0x28,
        name: "FastUartConfig",
        reset_value: 0x0600000F,
        description: "High-speed UART dividers",
    },
    RegisterEntry {
        address: 0x2C,
        name: "UartRelay",
        reset_value: 0x000F0000,
        description: "UART relay with gap counter",
    },
    RegisterEntry {
        address: 0x38,
        name: "TicketMask2",
        reset_value: 0x00000000,
        description: "Extended ticket mask (post-AsicBoost)",
    },
    RegisterEntry {
        address: 0x3C,
        name: "CoreRegisterControl",
        reset_value: 0x00004000,
        description: "Indirect core-register access (read trigger)",
    },
    RegisterEntry {
        address: 0x40,
        name: "CoreRegisterValue",
        reset_value: 0x00000000,
        description: "Indirect core-register readback",
    },
    RegisterEntry {
        address: 0x44,
        name: "ExtTempSensorRead",
        reset_value: 0x00000100,
        description: "External temp sensor passthrough",
    },
    RegisterEntry {
        address: 0x48,
        name: "ErrorFlag",
        reset_value: 0xFF000000,
        description: "Command + work error counts",
    },
    RegisterEntry {
        address: 0x4C,
        name: "NonceErrorCounter",
        reset_value: 0x00000000,
        description: "Bad-nonce counter (HW errors)",
    },
    RegisterEntry {
        address: 0x50,
        name: "NonceOverflowCounter",
        reset_value: 0x00000000,
        description: "Nonce-space exhaustion counter",
    },
    RegisterEntry {
        address: 0x54,
        name: "AnalogMuxControl",
        reset_value: 0x00000000,
        description: "Temperature diode / VDD mux selector",
    },
    RegisterEntry {
        address: 0x58,
        name: "IoDriverStrength",
        reset_value: 0x02112111,
        description: "IO pad drive strength",
    },
    RegisterEntry {
        address: 0x5C,
        name: "TimeOut",
        reset_value: 0x0000FFFF,
        description: "Watchdog / timeout value",
    },
    RegisterEntry {
        address: 0x60,
        name: "Pll1Parameter",
        reset_value: 0x00640111,
        description: "PLL1 configuration",
    },
    RegisterEntry {
        address: 0x64,
        name: "Pll2Parameter",
        reset_value: 0x00680111,
        description: "PLL2 configuration",
    },
    RegisterEntry {
        address: 0x68,
        name: "Pll3Parameter",
        reset_value: 0x00700111,
        description: "PLL3 — baud-rate clock source",
    },
    RegisterEntry {
        address: 0x6C,
        name: "OrderedClockMonitor",
        reset_value: 0x00000000,
        description: "Clock monitoring",
    },
    RegisterEntry {
        address: 0x70,
        name: "Pll0Divider",
        reset_value: 0x03040607,
        description: "PLL0 output divider chain (PRE-WRITE 0x0F0F0F00 to prevent glitch)",
    },
    RegisterEntry {
        address: 0x74,
        name: "Pll1Divider",
        reset_value: 0x03040506,
        description: "PLL1 output divider chain",
    },
    RegisterEntry {
        address: 0x78,
        name: "Pll2Divider",
        reset_value: 0x03040506,
        description: "PLL2 output divider chain",
    },
    RegisterEntry {
        address: 0x7C,
        name: "Pll3Divider",
        reset_value: 0x03040505,
        description: "PLL3 output divider chain",
    },
    RegisterEntry {
        address: 0x80,
        name: "ClockOrderControl0",
        reset_value: 0xD95C8410,
        description: "Core clock ordering (low 32)",
    },
    RegisterEntry {
        address: 0x84,
        name: "ClockOrderControl1",
        reset_value: 0xFB73EA62,
        description: "Core clock ordering (high 32)",
    },
    RegisterEntry {
        address: 0x8C,
        name: "ClockOrderStatus",
        reset_value: 0x00000000,
        description: "Clock order readback",
    },
];

/// Look up the register-map slice for a chip family. Returns the
/// BM1387 set for BM1387 alone; BM1397+ for everything else.
pub fn registers_for(family: ChipFamily) -> &'static [RegisterEntry] {
    match family {
        ChipFamily::Bm1387 => BM1387_REGISTERS,
        _ => BM1397_REGISTERS,
    }
}

/// Look up a register by name within a family. Returns `None` if the
/// name doesn't appear in the family's table.
pub fn register_by_name(family: ChipFamily, name: &str) -> Option<&'static RegisterEntry> {
    registers_for(family).iter().find(|r| r.name == name)
}

/// Look up a register by address within a family.
pub fn register_by_address(family: ChipFamily, address: u8) -> Option<&'static RegisterEntry> {
    registers_for(family).iter().find(|r| r.address == address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm1387_table_has_six_registers_per_re_doc() {
        // RE doc §6 lines 394-401: 6 registers (0x00, 0x08, 0x0C,
        // 0x18, 0x1C, 0x20).
        assert_eq!(BM1387_REGISTERS.len(), 6);
    }

    #[test]
    fn bm1387_pll_at_0x0c_not_0x08() {
        // Critical contrast vs BM1397+ family.
        let pll = register_by_name(ChipFamily::Bm1387, "PllParameter").unwrap();
        assert_eq!(pll.address, 0x0C);
    }

    #[test]
    fn bm1387_miscctrl_at_0x1c_not_0x18() {
        let mc = register_by_name(ChipFamily::Bm1387, "MiscControl").unwrap();
        assert_eq!(mc.address, 0x1C);
    }

    #[test]
    fn bm1397_pll0_at_0x08() {
        let pll = register_by_name(ChipFamily::Bm1397, "Pll0Parameter").unwrap();
        assert_eq!(pll.address, 0x08);
    }

    #[test]
    fn bm1397_miscctrl_at_0x18() {
        let mc = register_by_name(ChipFamily::Bm1397, "MiscControl").unwrap();
        assert_eq!(mc.address, 0x18);
    }

    #[test]
    fn bm1397_table_includes_all_named_pll_registers() {
        for name in [
            "Pll0Parameter",
            "Pll1Parameter",
            "Pll2Parameter",
            "Pll3Parameter",
        ] {
            assert!(
                register_by_name(ChipFamily::Bm1397, name).is_some(),
                "BM1397+ table missing {}",
                name
            );
        }
    }

    #[test]
    fn bm1397_registers_dispatched_for_all_other_families() {
        // BM1398/BM1362/BM1366/BM1368/BM1370 all share the BM1397+ map.
        // Use the BM1397-only Pll0Parameter register as a discriminator
        // (it does NOT exist in BM1387's table, where 0x08 is Hashrate).
        for fam in [
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            assert!(
                register_by_name(fam, "Pll0Parameter").is_some(),
                "{:?} should dispatch to BM1397+ register set (Pll0Parameter expected)",
                fam
            );
            // And the BM1387-only PllParameter name should NOT exist.
            assert!(
                register_by_name(fam, "PllParameter").is_none(),
                "{:?} should NOT have BM1387's PllParameter name",
                fam
            );
        }
    }

    #[test]
    fn register_by_address_round_trips() {
        // BM1387 PllParameter at 0x0C.
        let r = register_by_address(ChipFamily::Bm1387, 0x0C).unwrap();
        assert_eq!(r.name, "PllParameter");
        // BM1397 Pll0Parameter at 0x08.
        let r = register_by_address(ChipFamily::Bm1397, 0x08).unwrap();
        assert_eq!(r.name, "Pll0Parameter");
        // Unknown address → None.
        assert!(register_by_address(ChipFamily::Bm1387, 0xFF).is_none());
    }

    #[test]
    fn no_duplicate_addresses_within_a_family() {
        for table in [BM1387_REGISTERS, BM1397_REGISTERS] {
            let mut addresses: Vec<u8> = table.iter().map(|r| r.address).collect();
            addresses.sort_unstable();
            let original_len = addresses.len();
            addresses.dedup();
            assert_eq!(
                addresses.len(),
                original_len,
                "duplicate register address in table"
            );
        }
    }

    #[test]
    fn bm1397_uart_relay_register_pinned_at_0x2c() {
        // Used by BM1362 cold-boot recovery (write 0x00000002 to relay).
        let r = register_by_name(ChipFamily::Bm1362, "UartRelay").unwrap();
        assert_eq!(r.address, 0x2C);
    }

    #[test]
    fn ticket_mask_address_differs_between_families() {
        // BM1387: TicketMask at 0x18.
        let bm1387 = register_by_name(ChipFamily::Bm1387, "TicketMask").unwrap();
        assert_eq!(bm1387.address, 0x18);
        // BM1397+: TicketMask at 0x14.
        let bm1397 = register_by_name(ChipFamily::Bm1397, "TicketMask").unwrap();
        assert_eq!(bm1397.address, 0x14);
    }

    #[test]
    fn bm1387_chip_id_reset_value_includes_family_tag() {
        let chip_addr = register_by_name(ChipFamily::Bm1387, "ChipAddress").unwrap();
        // High 16 bits are CHIP_ID = 0x1387.
        assert_eq!(chip_addr.reset_value & 0xFFFF_0000, 0x1387_0000);
    }

    #[test]
    fn register_entry_serializes_to_documented_json_shape() {
        // Serialize-only (struct holds &'static str). Verify wire shape.
        let r = register_by_name(ChipFamily::Bm1397, "Pll0Parameter").unwrap();
        let json = serde_json::to_string(r).unwrap();
        assert!(json.contains("\"name\":\"Pll0Parameter\""));
        assert!(json.contains("\"address\":8"));
    }
}
