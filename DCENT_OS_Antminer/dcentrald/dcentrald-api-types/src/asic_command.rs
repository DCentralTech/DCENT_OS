//!  cmd-A — Generic BM13xx ASIC command catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! §4 (12-step cold-boot pipeline) + §5 (command-byte catalog).
//!
//! Captures the family-specific command-byte values used in the chain
//! UART protocol. The cold-boot pipeline (steps 3-5) requires the
//! correct family-specific header — using BM1387's `0x54` GetAddress
//! on a BM1397+ chip (or vice versa) results in silence (no NACK is
//! expected; silence IS the failure mode per RE doc §7).
//!
//! HAL-free: pure command-byte catalog + per-family lookup. The
//! runtime adapter inside `dcentrald-asic` consumes this to compose
//! framed UART writes.
//!
//! Distinct from  `asic_register_map` (register addresses for
//! `SETCONFIG`/`SET_ADDR` payloads). This module catalogs the
//! command-byte values that go in the UART command header.

use crate::chip_init::ChipFamily;
use serde::{Deserialize, Serialize};

/// Discrete ASIC command byte. Family-specific where the same operation
/// has different bytes per chip family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AsicCommand {
    /// SetChipAddress (single-chip) — `0x40` for BM1397+, `0x41` for BM1387.
    SetChipAddress,
    /// GetAddress (broadcast read of reg 0x00) — `0x52` for BM1397+, `0x54` for BM1387.
    GetAddress,
    /// ChainInactive (broadcast) — `0x53` for BM1397+, `0x55` for BM1387.
    ChainInactive,
    /// SetConfig — `0x51` for BM1397+ (write reg). BM1387 uses `0x58` for SETCONFIG.
    SetConfig,
}

impl AsicCommand {
    /// Look up the command byte for a given (command, chip family) pair.
    /// Returns `None` if the command isn't applicable to that family.
    pub fn byte_for_family(&self, family: ChipFamily) -> Option<u8> {
        use AsicCommand::*;
        use ChipFamily::*;
        match (self, family) {
            // BM1387: 0x41 / 0x54 / 0x55 / 0x58
            (SetChipAddress, Bm1387) => Some(0x41),
            (GetAddress, Bm1387) => Some(0x54),
            (ChainInactive, Bm1387) => Some(0x55),
            (SetConfig, Bm1387) => Some(0x58),
            // BM1397+: 0x40 / 0x52 / 0x53 / 0x51
            (SetChipAddress, _) => Some(0x40),
            (GetAddress, _) => Some(0x52),
            (ChainInactive, _) => Some(0x53),
            (SetConfig, _) => Some(0x51),
        }
    }

    /// Whether this command is broadcast (sent to all chips at once)
    /// vs single-chip targeted.
    pub fn is_broadcast(&self) -> bool {
        matches!(self, AsicCommand::GetAddress | AsicCommand::ChainInactive)
    }
}

/// Cold-boot pipeline step indicator. Mirrors the canonical 12-step
/// pipeline from RE doc §1 / §4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdBootStep {
    /// Step 1: FPGA reset + chain power-up.
    FpgaReset,
    /// Step 2: open UART at 115200 baud.
    OpenUart115200,
    /// Step 3: GetAddress broadcast → enumerate chips.
    GetAddress,
    /// Step 4: ChainInactive ×3 broadcast.
    ChainInactiveTriple,
    /// Step 5: SetChipAddress sequential (per-chip).
    SetChipAddressSeq,
    /// Step 6: family-specific preamble (Reg_A8 on BM1366+/BM1362+).
    FamilyPreamble,
    /// Step 7: PLL setup (per-chip frequency target).
    PllSetup,
    /// Step 8: MiscCtrl baud upgrade (TRIPLE-WRITE per
    /// ).
    MiscCtrlBaudUpgrade,
    /// Step 9: TicketMask + difficulty config.
    TicketMaskConfig,
    /// Step 10: open-core (BM1387 ONLY — 114 dummy works × N chips at
    /// gate_block=1).
    OpenCore,
    /// Step 11: HashCounting register init (BM1397+).
    HashCounting,
    /// Step 12: frequency ramp (5 MHz/step) to default.
    FrequencyRamp,
}

impl ColdBootStep {
    /// Whether this step is required for a given chip family.
    pub fn is_required(&self, family: ChipFamily) -> bool {
        match (*self, family) {
            // BM1387 requires open-core; nothing else does.
            (ColdBootStep::OpenCore, ChipFamily::Bm1387) => true,
            (ColdBootStep::OpenCore, _) => false,
            // BM1387 has no Reg_A8 family preamble; skip.
            (ColdBootStep::FamilyPreamble, ChipFamily::Bm1387) => false,
            (ColdBootStep::FamilyPreamble, _) => true,
            // BM1387 doesn't expose register 0x10 HashCounting; skip.
            (ColdBootStep::HashCounting, ChipFamily::Bm1387) => false,
            (ColdBootStep::HashCounting, _) => true,
            // Other steps universal across all families.
            (ColdBootStep::FpgaReset, _)
            | (ColdBootStep::OpenUart115200, _)
            | (ColdBootStep::GetAddress, _)
            | (ColdBootStep::ChainInactiveTriple, _)
            | (ColdBootStep::SetChipAddressSeq, _)
            | (ColdBootStep::PllSetup, _)
            | (ColdBootStep::MiscCtrlBaudUpgrade, _)
            | (ColdBootStep::TicketMaskConfig, _)
            | (ColdBootStep::FrequencyRamp, _) => true,
        }
    }
}

/// Validation failure while constructing a linear ASIC address plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum LinearAddressPlanError {
    ZeroChipCount,
    ZeroStride,
    TooManyChips {
        chip_count: u16,
    },
    LastAddressExceedsByteSpace {
        first_address: u8,
        chip_count: u16,
        address_interval: u8,
    },
}

/// Dense chip-index to strided one-byte hardware-address mapping.
///
/// Geometry owns this value; chip identity alone does not. The inverse helper
/// rejects odd/unassigned and out-of-range addresses instead of silently
/// treating a raw hardware address as a dense chip index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct LinearAddressPlan {
    first_address: u8,
    chip_count: u16,
    address_interval: u8,
}

impl LinearAddressPlan {
    pub const fn try_new(
        first_address: u8,
        chip_count: u16,
        address_interval: u8,
    ) -> Result<Self, LinearAddressPlanError> {
        if chip_count == 0 {
            return Err(LinearAddressPlanError::ZeroChipCount);
        }
        if chip_count > 256 {
            return Err(LinearAddressPlanError::TooManyChips { chip_count });
        }
        if address_interval == 0 {
            return Err(LinearAddressPlanError::ZeroStride);
        }
        let last_address = first_address as u32 + (chip_count as u32 - 1) * address_interval as u32;
        if last_address > u8::MAX as u32 {
            return Err(LinearAddressPlanError::LastAddressExceedsByteSpace {
                first_address,
                chip_count,
                address_interval,
            });
        }
        Ok(Self {
            first_address,
            chip_count,
            address_interval,
        })
    }

    /// Construct the historical BM1397+ `floor(256 / chip_count)` plan with
    /// explicit validation. Callers should persist the returned plan rather
    /// than recomputing its stride from mutable geometry.
    pub const fn from_truncated_byte_space(
        chip_count: u16,
    ) -> Result<Self, LinearAddressPlanError> {
        if chip_count == 0 {
            return Err(LinearAddressPlanError::ZeroChipCount);
        }
        if chip_count > 256 {
            return Err(LinearAddressPlanError::TooManyChips { chip_count });
        }
        // A one-chip repair fixture has only address 0; the mathematical
        // interval 256 is not representable on an 8-bit bus and must not wrap
        // to zero. Use the smallest canonical non-zero interval instead.
        let interval = if chip_count == 1 {
            1
        } else {
            (256 / chip_count) as u8
        };
        Self::try_new(0, chip_count, interval)
    }

    pub const fn first_address(self) -> u8 {
        self.first_address
    }

    pub const fn chip_count(self) -> u16 {
        self.chip_count
    }

    pub const fn address_interval(self) -> u8 {
        self.address_interval
    }

    pub const fn last_address(self) -> u8 {
        (self.first_address as u16 + (self.chip_count - 1) * self.address_interval as u16) as u8
    }

    pub const fn hardware_address(self, dense_index: u16) -> Option<u8> {
        if dense_index >= self.chip_count {
            return None;
        }
        Some((self.first_address as u16 + dense_index * self.address_interval as u16) as u8)
    }

    pub const fn dense_index(self, hardware_address: u8) -> Option<u16> {
        if hardware_address < self.first_address {
            return None;
        }
        let offset = hardware_address - self.first_address;
        if !offset.is_multiple_of(self.address_interval) {
            return None;
        }
        let index = offset as u16 / self.address_interval as u16;
        if index >= self.chip_count {
            return None;
        }
        Some(index)
    }
}

/// Compatibility wrapper for the historical address-stride formula.
/// BM1387: hw_addr = chip_idx × 4 (max 63 chips).
/// BM1397+: hw_addr = chip_idx × floor(256 / N).
///
/// New code should retain a validated [`LinearAddressPlan`] so invalid
/// geometry cannot be confused with a legitimate zero stride.
pub fn address_stride(family: ChipFamily, chip_count: u32) -> u32 {
    match family {
        ChipFamily::Bm1387 => 4,
        _ if chip_count == 0 => 0,
        _ => 256 / chip_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm1387_command_bytes_match_re_doc() {
        // RE doc §5 lines 331-337.
        assert_eq!(
            AsicCommand::SetChipAddress.byte_for_family(ChipFamily::Bm1387),
            Some(0x41)
        );
        assert_eq!(
            AsicCommand::GetAddress.byte_for_family(ChipFamily::Bm1387),
            Some(0x54)
        );
        assert_eq!(
            AsicCommand::ChainInactive.byte_for_family(ChipFamily::Bm1387),
            Some(0x55)
        );
    }

    #[test]
    fn bm1397plus_command_bytes_match_re_doc() {
        // BM1397+ family (covers BM1397/BM1398/BM1362/BM1366/BM1368/BM1370)
        // share 0x40 / 0x52 / 0x53 / 0x51 per RE doc §5.
        for fam in [
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            assert_eq!(AsicCommand::SetChipAddress.byte_for_family(fam), Some(0x40));
            assert_eq!(AsicCommand::GetAddress.byte_for_family(fam), Some(0x52));
            assert_eq!(AsicCommand::ChainInactive.byte_for_family(fam), Some(0x53));
            assert_eq!(AsicCommand::SetConfig.byte_for_family(fam), Some(0x51));
        }
    }

    #[test]
    fn broadcast_predicate_classifies_correctly() {
        assert!(AsicCommand::GetAddress.is_broadcast());
        assert!(AsicCommand::ChainInactive.is_broadcast());
        assert!(!AsicCommand::SetChipAddress.is_broadcast());
        assert!(!AsicCommand::SetConfig.is_broadcast());
    }

    #[test]
    fn open_core_required_only_for_bm1387() {
        assert!(ColdBootStep::OpenCore.is_required(ChipFamily::Bm1387));
        for fam in [
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            assert!(
                !ColdBootStep::OpenCore.is_required(fam),
                "{:?} should NOT require open-core",
                fam
            );
        }
    }

    #[test]
    fn family_preamble_skipped_on_bm1387_only() {
        assert!(!ColdBootStep::FamilyPreamble.is_required(ChipFamily::Bm1387));
        assert!(ColdBootStep::FamilyPreamble.is_required(ChipFamily::Bm1366));
    }

    #[test]
    fn hash_counting_skipped_on_bm1387() {
        assert!(!ColdBootStep::HashCounting.is_required(ChipFamily::Bm1387));
        assert!(ColdBootStep::HashCounting.is_required(ChipFamily::Bm1397));
        assert!(ColdBootStep::HashCounting.is_required(ChipFamily::Bm1362));
    }

    #[test]
    fn universal_steps_required_for_every_family() {
        // FpgaReset / OpenUart / GetAddress / ChainInactive /
        // SetChipAddress / PllSetup / MiscCtrl / TicketMask / FreqRamp
        // are all required for every family.
        let universal = [
            ColdBootStep::FpgaReset,
            ColdBootStep::OpenUart115200,
            ColdBootStep::GetAddress,
            ColdBootStep::ChainInactiveTriple,
            ColdBootStep::SetChipAddressSeq,
            ColdBootStep::PllSetup,
            ColdBootStep::MiscCtrlBaudUpgrade,
            ColdBootStep::TicketMaskConfig,
            ColdBootStep::FrequencyRamp,
        ];
        for fam in [
            ChipFamily::Bm1387,
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            for step in universal {
                assert!(
                    step.is_required(fam),
                    "{:?} should require step {:?}",
                    fam,
                    step
                );
            }
        }
    }

    #[test]
    fn address_stride_bm1387_is_4() {
        // RE doc §5 line 331-332: BM1387 hw_addr = chip_idx × 4.
        assert_eq!(address_stride(ChipFamily::Bm1387, 63), 4);
        assert_eq!(address_stride(ChipFamily::Bm1387, 0), 4);
    }

    #[test]
    fn address_stride_bm1397plus_is_256_div_chip_count() {
        // RE doc §4 step 5: BM1397+ uses interval = 256 / N.
        // S19 Pro: 114 chips → 256/114 = 2 (truncated).
        assert_eq!(address_stride(ChipFamily::Bm1398, 114), 2);
        // S21: 108 chips → 256/108 = 2.
        assert_eq!(address_stride(ChipFamily::Bm1368, 108), 2);
        // BM1366 S19k Pro: 77 chips → 256/77 = 3.
        assert_eq!(address_stride(ChipFamily::Bm1366, 77), 3);
        // 0 chip count → 0 (avoid div-by-zero).
        assert_eq!(address_stride(ChipFamily::Bm1397, 0), 0);
    }

    #[test]
    fn linear_address_plan_round_trips_exact_assigned_addresses() {
        let plan = LinearAddressPlan::try_new(0, 114, 2).unwrap();
        assert_eq!(plan.hardware_address(0), Some(0));
        assert_eq!(plan.hardware_address(113), Some(226));
        assert_eq!(plan.dense_index(0), Some(0));
        assert_eq!(plan.dense_index(226), Some(113));
        assert_eq!(plan.hardware_address(114), None);
        assert_eq!(plan.dense_index(227), None, "odd addresses are unassigned");
        assert_eq!(plan.dense_index(228), None, "index 114 is out of range");
    }

    #[test]
    fn linear_address_plan_rejects_invalid_geometry() {
        assert_eq!(
            LinearAddressPlan::try_new(0, 0, 2),
            Err(LinearAddressPlanError::ZeroChipCount)
        );
        assert_eq!(
            LinearAddressPlan::try_new(0, 114, 0),
            Err(LinearAddressPlanError::ZeroStride)
        );
        assert_eq!(
            LinearAddressPlan::try_new(0, 257, 1),
            Err(LinearAddressPlanError::TooManyChips { chip_count: 257 })
        );
        assert_eq!(
            LinearAddressPlan::try_new(0, 129, 2),
            Err(LinearAddressPlanError::LastAddressExceedsByteSpace {
                first_address: 0,
                chip_count: 129,
                address_interval: 2,
            })
        );
    }

    #[test]
    fn one_chip_repair_plan_uses_address_zero_without_stride_wrap() {
        let plan = LinearAddressPlan::from_truncated_byte_space(1).unwrap();
        assert_eq!(plan.address_interval(), 1);
        assert_eq!(plan.hardware_address(0), Some(0));
        assert_eq!(plan.dense_index(0), Some(0));
        assert_eq!(plan.dense_index(1), None);
    }

    #[test]
    fn historical_stride_wrapper_preserves_one_chip_u32_result() {
        assert_eq!(address_stride(ChipFamily::Bm1398, 1), 256);
    }

    #[test]
    fn asic_command_round_trips_through_serde() {
        for c in [
            AsicCommand::SetChipAddress,
            AsicCommand::GetAddress,
            AsicCommand::ChainInactive,
            AsicCommand::SetConfig,
        ] {
            let json = serde_json::to_string(&c).unwrap();
            let back: AsicCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn cold_boot_step_round_trips_through_serde() {
        for s in [
            ColdBootStep::FpgaReset,
            ColdBootStep::OpenUart115200,
            ColdBootStep::GetAddress,
            ColdBootStep::ChainInactiveTriple,
            ColdBootStep::SetChipAddressSeq,
            ColdBootStep::FamilyPreamble,
            ColdBootStep::PllSetup,
            ColdBootStep::MiscCtrlBaudUpgrade,
            ColdBootStep::TicketMaskConfig,
            ColdBootStep::OpenCore,
            ColdBootStep::HashCounting,
            ColdBootStep::FrequencyRamp,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: ColdBootStep = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn wrong_family_command_byte_silence_failure_mode() {
        // RE doc §7: using BM1397+'s 0x52 GetAddress on a BM1387 chip
        // results in silence — no NACK. This module is the source of
        // truth for the right byte; we just sanity-check the bytes
        // really are different so a runtime mistake is caught.
        let bm1387_get = AsicCommand::GetAddress.byte_for_family(ChipFamily::Bm1387);
        let bm1397_get = AsicCommand::GetAddress.byte_for_family(ChipFamily::Bm1397);
        assert_ne!(bm1387_get, bm1397_get);
        assert_eq!(bm1387_get, Some(0x54));
        assert_eq!(bm1397_get, Some(0x52));
    }
}
