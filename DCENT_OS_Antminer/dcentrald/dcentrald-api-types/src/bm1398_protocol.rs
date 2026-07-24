//! Evidence-scoped BM1398 protocol contracts.
//!
//! Chip behavior, NBP1901/S19 Pro chain geometry, and FPGA FIFO layout are
//! separate types. The aggregate constant names the complete composition so
//! no caller can mistake one board's topology or transport for universal
//! BM1398 behavior.
//!
//! PLL search and field layout are independently witnessed by the local stock
//! NBP1901 `bmminer` (SHA-256
//! `e91e6d9fa7b8524abdb05ac5ca4b7118c6f50a58b6075541139c6f56c1b21d14`,
//! search VA `0x502c0`, encoder VA `0x4fa9c`) and BM1398 repair-jig binary (SHA-256
//! `ddb73ebe334908767360a1b9a15144daa751d45c7f22a4965788371957ff6317`,
//! search VA `0x29b48`, encoder VA `0x29558`).

use serde::Serialize;

use crate::asic_command::LinearAddressPlan;
use crate::asic_protocol_spec::{AsicResponseLengthSpec, RESPONSE_PREAMBLE_BYTES};
use crate::bm13xx_pll::{FourDividerPll, FourDividerPllSearchSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RegisterWrite {
    pub register: u8,
    pub value: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AddressedRegisterWrite {
    pub chip_address: u8,
    pub register: u8,
    pub value: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bm1398ChipSpec {
    pub chip_id: u16,
    pub response: AsicResponseLengthSpec,
    pub pll_register: u8,
    pub core_register_control: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bm1398PllSolution {
    pub dividers: FourDividerPll,
    pub register_value: u32,
}

/// Stock NBP1901 and BM1398 repair-jig search envelope. Both independent
/// binaries search refdiv 2 before 1, fbdiv 16..=250, postdivs 1..=7, VCO
/// 2000..=3200 MHz, and cap refdiv-1 VCO at 3125 MHz.
pub const BM1398_PLL_SEARCH_SPEC: FourDividerPllSearchSpec = FourDividerPllSearchSpec {
    reference_mhz: 25,
    refdiv_order: [2, 1],
    fbdiv_min: 16,
    fbdiv_max: 250,
    postdiv_min: 1,
    postdiv_max: 7,
    vco_min_mhz: 2_000,
    vco_max_mhz: 3_200,
    refdiv_one_vco_max_mhz: 3_125,
    max_error_millimhz_exclusive: 10_000,
};

/// Encode a BM1398 PLL0 word. FBDIV occupies bits `[27:16]`; post-divider
/// fields are raw values, not the minus-one encoding used by later families.
pub const fn bm1398_pll_register_value(params: FourDividerPll) -> u32 {
    (1u32 << 30)
        | ((params.fbdiv as u32 & 0x0fff) << 16)
        | ((params.refdiv as u32 & 0x3f) << 8)
        | ((params.postdiv1 as u32 & 0x7) << 4)
        | (params.postdiv2 as u32 & 0x7)
}

pub fn resolve_bm1398_pll(target_mhz: u16) -> Option<Bm1398PllSolution> {
    let dividers = BM1398_PLL_SEARCH_SPEC.resolve(target_mhz)?;
    Some(Bm1398PllSolution {
        dividers,
        register_value: bm1398_pll_register_value(dividers),
    })
}

pub const BM1398_CHIP_SPEC: Bm1398ChipSpec = Bm1398ChipSpec {
    chip_id: 0x1398,
    response: AsicResponseLengthSpec {
        body_bytes: 7,
        preamble_bytes: RESPONSE_PREAMBLE_BYTES,
    },
    pll_register: 0x08,
    core_register_control: 0x3c,
};

/// Exact staged core-control writes proven in both the stock NBP1901 binary
/// and the repair jig. These are evidence fragments, not a claimed complete
/// cold-boot recipe.
pub const BM1398_PROVEN_CORE_WRITES: [RegisterWrite; 2] = [
    RegisterWrite {
        register: 0x3c,
        value: 0x8000_8710,
    },
    RegisterWrite {
        register: 0x3c,
        value: 0x8000_8050,
    },
];

pub const S19_PRO_NBP1901_ADDRESS_PLAN: LinearAddressPlan =
    match LinearAddressPlan::try_new(0, 114, 2) {
        Ok(plan) => plan,
        Err(_) => panic!("invalid built-in NBP1901 address plan"),
    };

/// Stock production NBP1901 relay dialect. The repair-jig dialect is
/// intentionally not represented by this array because its topology formula
/// differs; conflating the two would create another unsupported constant.
pub const S19_PRO_NBP1901_PRODUCTION_UART_RELAY_WRITES: [AddressedRegisterWrite; 12] = [
    AddressedRegisterWrite {
        chip_address: 214,
        register: 0x2c,
        value: 0x0017_0003,
    },
    AddressedRegisterWrite {
        chip_address: 196,
        register: 0x2c,
        value: 0x0020_0003,
    },
    AddressedRegisterWrite {
        chip_address: 178,
        register: 0x2c,
        value: 0x0029_0003,
    },
    AddressedRegisterWrite {
        chip_address: 160,
        register: 0x2c,
        value: 0x0032_0003,
    },
    AddressedRegisterWrite {
        chip_address: 142,
        register: 0x2c,
        value: 0x003b_0003,
    },
    AddressedRegisterWrite {
        chip_address: 124,
        register: 0x2c,
        value: 0x0044_0003,
    },
    AddressedRegisterWrite {
        chip_address: 106,
        register: 0x2c,
        value: 0x004d_0003,
    },
    AddressedRegisterWrite {
        chip_address: 88,
        register: 0x2c,
        value: 0x0056_0003,
    },
    AddressedRegisterWrite {
        chip_address: 70,
        register: 0x2c,
        value: 0x005f_0003,
    },
    AddressedRegisterWrite {
        chip_address: 52,
        register: 0x2c,
        value: 0x0068_0003,
    },
    AddressedRegisterWrite {
        chip_address: 34,
        register: 0x2c,
        value: 0x0071_0003,
    },
    AddressedRegisterWrite {
        chip_address: 16,
        register: 0x2c,
        value: 0x007a_0003,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Nbp1901S19ProChainSpec {
    pub expected_chip_count: u16,
    pub voltage_domain_count: u8,
    pub chips_per_voltage_domain: u8,
    pub address_plan: LinearAddressPlan,
    pub proven_core_register_writes: &'static [RegisterWrite],
    pub production_uart_relay_writes: &'static [AddressedRegisterWrite],
}

pub const S19_PRO_NBP1901_CHAIN_SPEC: Nbp1901S19ProChainSpec = Nbp1901S19ProChainSpec {
    expected_chip_count: 114,
    voltage_domain_count: 38,
    chips_per_voltage_domain: 3,
    address_plan: S19_PRO_NBP1901_ADDRESS_PLAN,
    proven_core_register_writes: &BM1398_PROVEN_CORE_WRITES,
    production_uart_relay_writes: &S19_PRO_NBP1901_PRODUCTION_UART_RELAY_WRITES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bm1398FpgaMidstateMode {
    Four,
    Eight,
}

impl Bm1398FpgaMidstateMode {
    pub const fn log2_count(self) -> u8 {
        match self {
            Self::Four => 2,
            Self::Eight => 3,
        }
    }

    pub const fn midstate_count(self) -> u8 {
        1 << self.log2_count()
    }

    pub const fn payload_words(self) -> u16 {
        4 + self.midstate_count() as u16 * 8
    }

    pub const fn payload_bytes(self) -> u16 {
        self.payload_words() * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bm1398FpgaFifoSpec {
    supported_modes: [Bm1398FpgaMidstateMode; 2],
    nonce_chip_address_shift: u8,
    nonce_chip_address_mask: u8,
    /// Width of the carrier's raw extended-work-id echo.
    echoed_work_id_bits: u8,
    /// Width of the logical dispatcher ring before slot bits are appended.
    logical_work_id_bits: u8,
}

impl Bm1398FpgaFifoSpec {
    pub const fn supported_modes(self) -> [Bm1398FpgaMidstateMode; 2] {
        self.supported_modes
    }

    pub const fn nonce_chip_address_shift(self) -> u8 {
        self.nonce_chip_address_shift
    }

    pub const fn nonce_chip_address_mask(self) -> u8 {
        self.nonce_chip_address_mask
    }

    pub const fn echoed_work_id_bits(self) -> u8 {
        self.echoed_work_id_bits
    }

    pub const fn logical_work_id_bits(self) -> u8 {
        self.logical_work_id_bits
    }

    pub const fn supports_mode(self, mode: Bm1398FpgaMidstateMode) -> bool {
        matches!(
            (self.supported_modes[0], self.supported_modes[1], mode),
            (
                Bm1398FpgaMidstateMode::Four,
                _,
                Bm1398FpgaMidstateMode::Four
            ) | (
                _,
                Bm1398FpgaMidstateMode::Four,
                Bm1398FpgaMidstateMode::Four
            ) | (
                Bm1398FpgaMidstateMode::Eight,
                _,
                Bm1398FpgaMidstateMode::Eight
            ) | (
                _,
                Bm1398FpgaMidstateMode::Eight,
                Bm1398FpgaMidstateMode::Eight
            )
        )
    }

    pub const fn raw_chip_address(self, nonce: u32) -> Option<u8> {
        if self.nonce_chip_address_shift >= u32::BITS as u8 {
            return None;
        }
        Some(((nonce >> self.nonce_chip_address_shift) & self.nonce_chip_address_mask as u32) as u8)
    }

    pub const fn dense_chip_index(
        self,
        nonce: u32,
        address_plan: LinearAddressPlan,
    ) -> Option<u16> {
        match self.raw_chip_address(nonce) {
            Some(address) => address_plan.dense_index(address),
            None => None,
        }
    }

    pub const fn encode_work_id(
        self,
        mode: Bm1398FpgaMidstateMode,
        logical_work_id: u16,
        slot_index: u8,
    ) -> Option<u16> {
        let slot_bits = mode.log2_count();
        if !self.supports_mode(mode)
            || self.logical_work_id_bits >= u32::BITS as u8
            || self.echoed_work_id_bits > u16::BITS as u8
            || self.logical_work_id_bits.saturating_add(slot_bits) > self.echoed_work_id_bits
        {
            return None;
        }
        let logical_limit = 1u32 << self.logical_work_id_bits;
        if logical_work_id as u32 >= logical_limit || slot_index >= mode.midstate_count() {
            return None;
        }
        match logical_work_id.checked_shl(slot_bits as u32) {
            Some(encoded) => Some(encoded | slot_index as u16),
            None => None,
        }
    }

    pub const fn decode_work_id(
        self,
        mode: Bm1398FpgaMidstateMode,
        echoed_work_id: u16,
    ) -> Option<(u16, u8)> {
        let slot_bits = mode.log2_count();
        if !self.supports_mode(mode)
            || self.echoed_work_id_bits != u16::BITS as u8
            || self.logical_work_id_bits >= u32::BITS as u8
            || self.logical_work_id_bits.saturating_add(slot_bits) > self.echoed_work_id_bits
        {
            return None;
        }
        let slot_mask = (1u16 << slot_bits) - 1;
        let logical_work_id = echoed_work_id >> slot_bits;
        if logical_work_id as u32 >= (1u32 << self.logical_work_id_bits) {
            return None;
        }
        Some((logical_work_id, (echoed_work_id & slot_mask) as u8))
    }
}

pub const BM1398_FPGA_FIFO_SPEC: Bm1398FpgaFifoSpec = Bm1398FpgaFifoSpec {
    supported_modes: [Bm1398FpgaMidstateMode::Four, Bm1398FpgaMidstateMode::Eight],
    nonce_chip_address_shift: 17,
    nonce_chip_address_mask: 0xff,
    echoed_work_id_bits: 16,
    logical_work_id_bits: 8,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bm1398ProtocolProfile {
    pub chip: Bm1398ChipSpec,
    pub chain: Nbp1901S19ProChainSpec,
    pub fifo: Bm1398FpgaFifoSpec,
}

pub const S19_PRO_NBP1901_BM1398_PROFILE: Bm1398ProtocolProfile = Bm1398ProtocolProfile {
    chip: BM1398_CHIP_SPEC,
    chain: S19_PRO_NBP1901_CHAIN_SPEC,
    fifo: BM1398_FPGA_FIFO_SPEC,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_vendor_pll_vectors_are_pinned() {
        let pll_525 = resolve_bm1398_pll(525).unwrap();
        assert_eq!(
            pll_525.dividers,
            FourDividerPll {
                refdiv: 2,
                fbdiv: 168,
                postdiv1: 4,
                postdiv2: 1,
            }
        );
        assert_eq!(pll_525.register_value, 0x40a8_0241);

        let pll_675 = resolve_bm1398_pll(675).unwrap();
        assert_eq!(
            pll_675.dividers,
            FourDividerPll {
                refdiv: 2,
                fbdiv: 162,
                postdiv1: 3,
                postdiv2: 1,
            }
        );
        assert_eq!(pll_675.register_value, 0x40a2_0231);
    }

    #[test]
    fn vendor_error_ceiling_is_strict_at_both_vco_boundaries() {
        assert!(resolve_bm1398_pll(1_990).is_none());
        assert!(resolve_bm1398_pll(1_991).is_some());
        assert!(resolve_bm1398_pll(3_134).is_some());
        assert!(resolve_bm1398_pll(3_135).is_none());
    }

    #[test]
    fn pll_encoder_preserves_the_repair_jig_12th_fbdiv_bit() {
        let word = bm1398_pll_register_value(FourDividerPll {
            refdiv: 1,
            fbdiv: 0x0800,
            postdiv1: 1,
            postdiv2: 1,
        });
        assert_eq!((word >> 16) & 0x0fff, 0x0800);
        assert_eq!(word, 0x4800_0111);
    }

    #[test]
    fn nbp1901_geometry_and_addressing_are_exact() {
        let chain = S19_PRO_NBP1901_CHAIN_SPEC;
        assert_eq!(
            chain.voltage_domain_count as u16 * chain.chips_per_voltage_domain as u16,
            chain.expected_chip_count
        );
        assert_eq!(chain.address_plan.first_address(), 0);
        assert_eq!(chain.address_plan.address_interval(), 2);
        assert_eq!(chain.address_plan.last_address(), 226);
        assert_eq!(chain.address_plan.hardware_address(113), Some(226));
        assert_eq!(chain.address_plan.dense_index(226), Some(113));
        assert_eq!(chain.address_plan.dense_index(225), None);
        assert_eq!(chain.address_plan.dense_index(228), None);
    }

    #[test]
    fn production_relay_sequence_is_distinct_and_complete() {
        let writes = S19_PRO_NBP1901_PRODUCTION_UART_RELAY_WRITES;
        assert_eq!(writes.len(), 12);
        assert_eq!(writes[0].chip_address, 214);
        assert_eq!(writes[0].value, 0x0017_0003);
        assert_eq!(writes[11].chip_address, 16);
        assert_eq!(writes[11].value, 0x007a_0003);
        for window in writes.windows(2) {
            assert_eq!(window[0].chip_address - window[1].chip_address, 18);
        }
        assert!(writes.iter().all(|write| write.register == 0x2c));
    }

    #[test]
    fn core_writes_are_staged_evidence_not_the_old_snapshot() {
        assert_eq!(BM1398_PROVEN_CORE_WRITES[0].value, 0x8000_8710);
        assert_eq!(BM1398_PROVEN_CORE_WRITES[1].value, 0x8000_8050);
        assert!(BM1398_PROVEN_CORE_WRITES
            .iter()
            .all(|write| write.value != 0x8000_8074));
    }

    #[test]
    fn fifo_modes_have_distinct_payload_sizes() {
        assert_eq!(Bm1398FpgaMidstateMode::Four.log2_count(), 2);
        assert_eq!(Bm1398FpgaMidstateMode::Four.payload_words(), 36);
        assert_eq!(Bm1398FpgaMidstateMode::Four.payload_bytes(), 144);
        assert_eq!(Bm1398FpgaMidstateMode::Eight.log2_count(), 3);
        assert_eq!(Bm1398FpgaMidstateMode::Eight.payload_words(), 68);
        assert_eq!(Bm1398FpgaMidstateMode::Eight.payload_bytes(), 272);
    }

    #[test]
    fn nonce_address_is_normalized_through_the_chain_plan() {
        let raw_address = 226u32;
        let nonce = raw_address << 17;
        assert_eq!(BM1398_FPGA_FIFO_SPEC.raw_chip_address(nonce), Some(226));
        assert_eq!(
            BM1398_FPGA_FIFO_SPEC.dense_chip_index(nonce, S19_PRO_NBP1901_ADDRESS_PLAN),
            Some(113)
        );

        let unassigned_nonce = 225u32 << 17;
        assert_eq!(
            BM1398_FPGA_FIFO_SPEC.dense_chip_index(unassigned_nonce, S19_PRO_NBP1901_ADDRESS_PLAN),
            None
        );
    }

    #[test]
    fn fifo_work_id_separates_raw_echo_logical_ring_and_slot_bits() {
        let fifo = BM1398_FPGA_FIFO_SPEC;
        assert_eq!(fifo.echoed_work_id_bits(), 16);
        assert_eq!(fifo.logical_work_id_bits(), 8);
        assert_eq!(
            fifo.encode_work_id(Bm1398FpgaMidstateMode::Four, 0x55, 3),
            Some(0x0157)
        );
        assert_eq!(
            fifo.decode_work_id(Bm1398FpgaMidstateMode::Four, 0x0157),
            Some((0x55, 3))
        );
        assert_eq!(
            fifo.decode_work_id(Bm1398FpgaMidstateMode::Eight, 0x0157),
            Some((0x2a, 7))
        );
        assert_eq!(
            fifo.decode_work_id(Bm1398FpgaMidstateMode::Four, 0x0400),
            None,
            "logical IDs beyond the 8-bit carrier ring must not mask-alias"
        );
    }

    #[test]
    fn malformed_internal_fifo_specs_fail_closed_without_shift_panics() {
        let invalid_shift = Bm1398FpgaFifoSpec {
            nonce_chip_address_shift: 32,
            ..BM1398_FPGA_FIFO_SPEC
        };
        assert_eq!(invalid_shift.raw_chip_address(u32::MAX), None);
        assert_eq!(
            invalid_shift.dense_chip_index(u32::MAX, S19_PRO_NBP1901_ADDRESS_PLAN),
            None
        );

        let invalid_width = Bm1398FpgaFifoSpec {
            echoed_work_id_bits: 16,
            logical_work_id_bits: 32,
            ..BM1398_FPGA_FIFO_SPEC
        };
        assert_eq!(
            invalid_width.encode_work_id(Bm1398FpgaMidstateMode::Eight, 1, 0),
            None
        );
        assert_eq!(
            invalid_width.decode_work_id(Bm1398FpgaMidstateMode::Eight, 1),
            None
        );

        let unsupported_mode = Bm1398FpgaFifoSpec {
            supported_modes: [Bm1398FpgaMidstateMode::Four, Bm1398FpgaMidstateMode::Four],
            ..BM1398_FPGA_FIFO_SPEC
        };
        assert_eq!(
            unsupported_mode.encode_work_id(Bm1398FpgaMidstateMode::Eight, 1, 0),
            None
        );
        assert_eq!(
            unsupported_mode.decode_work_id(Bm1398FpgaMidstateMode::Eight, 1),
            None
        );
    }
}
