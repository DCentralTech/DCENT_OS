//! BM13xx wire protocol implementation.
//!
//! All BM13xx ASIC chips share the same wire protocol framing:
//!
//! Command Frame (Host -> ASIC):
//!   [0x55] [0xAA] [header] [length] [payload...] [CRC5 or CRC16]
//!
//! Response Frame (ASIC -> Host):
//!   [0xAA] [0x55] [payload (5 or 7 bytes)] [CRC5+flags (2 bytes)]
//!
//! The Braiins s9io FPGA handles the UART preamble (0x55 0xAA) and CRC
//! internally. Commands written to CMD_TX_FIFO are packed as 32-bit words
//! with LSB-first byte ordering.
//!
//! CRC calculations:
//!   CMD packets: CRC-5 (poly 0x05, init 0x1F)
//!   CMD/REG responses: modified CRC-5 response state machine (init 0x03)
//!   JOB packets: CRC-16 CCITT-FALSE (poly 0x1021, init 0xFFFF)

pub use dcentrald_api_types::asic_protocol_spec::{
    AsicProtocolSpec, AsicResponseLengthSpec, WorkTransportShape, BM136X_RESPONSE_BODY_BYTES,
    BM139X_RESPONSE_BODY_BYTES,
};

/// Command preamble (host to ASIC).
pub const CMD_PREAMBLE: [u8; 2] = [0x55, 0xAA];

/// Response preamble (ASIC to host).
pub const RESP_PREAMBLE: [u8; 2] = [0xAA, 0x55];

/// Default ASIC UART baud rate.
pub const DEFAULT_BAUD: u32 = 115_200;

/// Host-safe protocol spec selected from the detected ASIC chip ID.
///
/// This is intentionally a pure lookup: no UART, FPGA, I2C, or miner access.
pub fn protocol_spec_for_detected_chip(chip_id: u16) -> Option<AsicProtocolSpec> {
    AsicProtocolSpec::for_detected_chip_id(chip_id)
}

// ---------------------------------------------------------------------------
// Header byte encoding
// ---------------------------------------------------------------------------

/// Header type: command.
pub const TYPE_CMD: u8 = 0x40;

/// Header type: job.
pub const TYPE_JOB: u8 = 0x20;

/// Header flag: broadcast to all chips.
pub const GROUP_ALL: u8 = 0x10;

/// Command: set chip address (also write — differentiated by length).
pub const CMD_SETADDR: u8 = 0x01;

/// Command: write register (same opcode as SETADDR, differentiated by length).
pub const CMD_WRITE: u8 = 0x01;

/// Command: read register.
pub const CMD_READ: u8 = 0x04;

/// Command: set config (write register with hw_addr). Opcode 0x08.
/// Used by BM1387 for TicketMask, MiscCtrl, PLL, baud rate — everything that
/// is NOT chip address assignment. Header byte = TYPE_CMD | CMD_SETCONFIG = 0x48
/// (single chip) or TYPE_CMD | GROUP_ALL | CMD_SETCONFIG = 0x58 (broadcast).
pub const CMD_SETCONFIG: u8 = 0x08;

/// Command: chain inactive (enumeration).
pub const CMD_INACTIVE: u8 = 0x05;

// Common combined header values:
/// CMD single SETADDRESS (0x41).
pub const HDR_CMD_SETADDR: u8 = TYPE_CMD | CMD_SETADDR;

/// CMD single WRITE (0x41) — same header as SETADDR, differentiated by length.
pub const HDR_CMD_WRITE: u8 = TYPE_CMD | CMD_WRITE;

/// CMD single READ (0x44).
pub const HDR_CMD_READ: u8 = TYPE_CMD | CMD_READ;

/// CMD broadcast WRITE (0x51).
pub const HDR_CMD_BCAST_WRITE: u8 = TYPE_CMD | GROUP_ALL | CMD_WRITE;

/// CMD single SETCONFIG (0x48) — write register to specific chip with hw_addr.
pub const HDR_CMD_SETCONFIG: u8 = TYPE_CMD | CMD_SETCONFIG;

/// CMD broadcast SETCONFIG (0x58) — write register to all chips with hw_addr.
pub const HDR_CMD_BCAST_SETCONFIG: u8 = TYPE_CMD | GROUP_ALL | CMD_SETCONFIG;

/// CMD broadcast READ (0x54) — also used for GetAddress (read reg 0x00 broadcast).
pub const HDR_CMD_BCAST_READ: u8 = TYPE_CMD | GROUP_ALL | CMD_READ;

/// CMD broadcast INACTIVE (0x55).
pub const HDR_CMD_BCAST_INACTIVE: u8 = TYPE_CMD | GROUP_ALL | CMD_INACTIVE;

/// JOB single WRITE (0x21).
pub const HDR_JOB_WRITE: u8 = TYPE_JOB | CMD_WRITE;

// ---------------------------------------------------------------------------
// FPGA FIFO command encoding (32-bit word packing, LSB-first)
// ---------------------------------------------------------------------------

/// Encode a Chain Inactive broadcast command for CMD_TX_FIFO.
/// Wire format: [0x55, 0x05, 0x00, 0x00] — header 0x55 (CMD|BCAST|INACTIVE), length 5.
/// Verified from working asic_init.py on live S9.
pub const FIFO_CMD_CHAIN_INACTIVE: u32 = 0x0000_0555;

/// BM1397+ Chain Inactive broadcast.
/// Wire bytes: `[0x53, 0x05, 0x00, 0x00]`.
pub const FIFO_CMD_CHAIN_INACTIVE_BM139X: u32 = 0x0000_0553;

/// Encode a GetAddress broadcast command for CMD_TX_FIFO (BM1387).
/// Wire format: [0x54, 0x05, 0x00, 0x00] — header 0x54 (CMD|BCAST|READ), length 5, reg 0x00.
/// GetAddress = broadcast read of register 0x00 (ChipAddress register).
/// Verified from working asic_init.py on live S9.
pub const FIFO_CMD_GET_ADDRESS: u32 = 0x0000_0554;

/// Encode a GetAddress broadcast command for CMD_TX_FIFO (BM1397/BM1398/BM1362+).
/// Wire format: [0x52, 0x05, 0x00, 0x00] — header 0x52 (BCAST|READ for BM1397+), length 5, reg 0x00.
/// BM1397+ uses different command headers: 0x51 (write), 0x52 (read), 0x53 (inactive).
/// BM1387 uses: 0x58 (write), 0x54 (read), 0x55 (inactive).
pub const FIFO_CMD_GET_ADDRESS_BM139X: u32 = 0x0000_0552;

/// Encode a SetChipAddress command for CMD_TX_FIFO.
///
/// `addr` is the chip address to assign.
pub fn fifo_cmd_set_address(addr: u8) -> u32 {
    ((addr as u32) << 16) | 0x0541
}

/// Encode a BM1397+ SetChipAddress command for the FPGA CMD FIFO.
/// Wire bytes: `[0x40, 0x05, addr, 0x00]`.
pub const fn fifo_cmd_set_address_bm139x(addr: u8) -> u32 {
    ((addr as u32) << 16) | 0x0540
}

/// Encode a Read Register command for CMD_TX_FIFO.
///
/// `chip_addr`: target chip address
/// `reg`: register offset
pub fn fifo_cmd_read_register(chip_addr: u8, reg: u8) -> u32 {
    ((reg as u32) << 24) | ((chip_addr as u32) << 16) | 0x0544
}

/// Encode a broadcast Write Register command for CMD_TX_FIFO.
///
/// Returns (word0, word1) — both must be written to the FIFO sequentially.
///
/// Wire format (after FPGA adds preamble 0x55 0xAA):
///   [0x58, 0x09, 0x00, reg, value_BE[0], value_BE[1], value_BE[2], value_BE[3]]
///
/// Header 0x58 = CMD | BCAST | SETCONFIG. hw_addr = 0x00 for broadcast.
/// Value is sent MSB-first (big-endian) on the wire — NOT LSB-first.
///
/// Verified: `fifo_cmd_write_reg_bcast_full(0x18, 0xFF)` →
///   word0=0x18000958, word1=0xFF000000
///   Wire: [58 09 00 18 00 00 00 FF]
pub fn fifo_cmd_write_reg_bcast_full(reg: u8, value: u32) -> (u32, u32) {
    let word0 = pack_lsb_first(&[HDR_CMD_BCAST_SETCONFIG, 0x09, 0x00, reg]);
    let word1 = pack_lsb_first(&value.to_be_bytes());
    (word0, word1)
}

// fifo_cmd_write_register_bcast() REMOVED — it wrote 1 FIFO word but encoded
// length=9 (2-word command), causing CMD FIFO desync. The FPGA consumed the next
// command's word as this command's word1, corrupting all subsequent writes.
// This was the root cause of zero nonces on cold boot.
// Use fifo_cmd_write_reg_bcast_full() instead (always 2 words).

/// Encode a single-chip Write Register command for CMD_TX_FIFO.
///
/// Returns (word0, word1) — both must be written to the FIFO sequentially.
///
/// Wire format: [0x48, 0x09, chip_addr, reg, value_BE[0..4]]
/// Header 0x48 = CMD | SETCONFIG. Value is MSB-first (big-endian).
pub fn fifo_cmd_write_reg_full(chip_addr: u8, reg: u8, value: u32) -> (u32, u32) {
    let word0 = pack_lsb_first(&[HDR_CMD_SETCONFIG, 0x09, chip_addr, reg]);
    let word1 = pack_lsb_first(&value.to_be_bytes());
    (word0, word1)
}

// fifo_cmd_write_register() REMOVED — same single-word bug as the broadcast variant.
// Use fifo_cmd_write_reg_full() instead (always 2 words).

// ---------------------------------------------------------------------------
// CRC-5 (poly 0x05, init 0x1F)
// ---------------------------------------------------------------------------

/// Calculate CRC-5 for command packets.
///
/// Polynomial: 0x05, initial value: 0x1F.
/// Used for all CMD-type packets.
pub fn crc5(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x1F;
    for &byte in data {
        for i in (0..8).rev() {
            let bit = (byte >> i) & 1;
            let crc_bit = (crc >> 4) & 1;
            crc <<= 1;
            if bit ^ crc_bit != 0 {
                crc ^= 0x05;
            }
            crc &= 0x1F;
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// BM13xx command/register response CRC-5 (modified poly 0x0D state machine)
// ---------------------------------------------------------------------------

/// Calculate the lower-five-bit CRC carried by a BM13xx command/register
/// response trailer.
///
/// This is a direct software port of Braiins'
/// `crc5_resp_serial.vhd` modified response transition, initialized to `0x03`
/// for command/register responses. It is deliberately separate from [`crc5`]:
/// substituting the host-command polynomial produces different bytes.
///
/// `data` is the response payload only. Do not include the `AA 55` preamble or
/// the final trailer byte. Only trailer bits 4:0 are covered; this function
/// assigns no meaning to trailer bits 6:5.
///
/// The RTL also names a distinct job-response initial state. That surface is
/// intentionally not exposed here until an independent retained job-response
/// vector can test it; this function is not job-response production proof.
///
/// Source contract: `contracts/asic-wire/v1/bm13xx-response-crc5.json`.
pub fn bm13xx_command_response_crc5(data: &[u8]) -> u8 {
    bm13xx_response_crc5_with_init(data, 0x03)
}

fn bm13xx_response_crc5_with_init(data: &[u8], init: u8) -> u8 {
    let mut crc = init & 0x1F;
    for &byte in data {
        for bit_index in (0..8).rev() {
            let data_bit = (byte >> bit_index) & 1;
            let feedback = data_bit ^ ((crc >> 4) & 1);

            // Verbatim logical equivalent of crc5_resp_serial.vhd:
            //   universal shift: c[4:0] <- c[3:0] & feedback
            //   c2 <- old_c1 xor feedback
            //   c3 <- old_c2 xor data_bit   (non-standard update)
            crc = (((crc >> 3) & 1) << 4)
                | ((((crc >> 2) & 1) ^ data_bit) << 3)
                | ((((crc >> 1) & 1) ^ feedback) << 2)
                | ((crc & 1) << 1)
                | feedback;
        }
    }
    crc
}

// ---------------------------------------------------------------------------
// CRC-16 CCITT-FALSE (poly 0x1021, init 0xFFFF)
// ---------------------------------------------------------------------------

/// Calculate CRC-16 CCITT-FALSE for job packets.
///
/// Polynomial: 0x1021, initial value: 0xFFFF.
/// Result is appended big-endian (MSB first).
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Pack bytes into a 32-bit word with LSB-first byte ordering.
///
/// This is the FPGA FIFO word format:
///   Bytes [B0, B1, B2, B3] -> word = B0 | (B1 << 8) | (B2 << 16) | (B3 << 24)
pub fn pack_lsb_first(bytes: &[u8]) -> u32 {
    let mut word = 0u32;
    for (i, &b) in bytes.iter().take(4).enumerate() {
        word |= (b as u32) << (i * 8);
    }
    word
}

/// Unpack a 32-bit word into bytes with LSB-first ordering.
pub fn unpack_lsb_first(word: u32) -> [u8; 4] {
    [
        (word & 0xFF) as u8,
        ((word >> 8) & 0xFF) as u8,
        ((word >> 16) & 0xFF) as u8,
        ((word >> 24) & 0xFF) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_api_types::baud_switch::BaudChipFamily;
    use dcentrald_api_types::chip_init::ChipFamily;
    use dcentrald_api_types::work_dispatch::WorkFrameFormat;

    #[test]
    fn protocol_spec_lookup_is_pure_and_atomic_for_bm136x() {
        let spec = protocol_spec_for_detected_chip(0x1362).unwrap();
        assert_eq!(spec.family, ChipFamily::Bm1362);
        assert_eq!(spec.response.body_bytes, BM136X_RESPONSE_BODY_BYTES);
        assert_eq!(spec.response.wire_bytes(), 11);
        assert_eq!(spec.baud.family, BaudChipFamily::Bm1362);
        assert!(matches!(
            spec.work_transport,
            WorkTransportShape::UartFullHeader {
                format: WorkFrameFormat::Bm1362,
                wire_frame_bytes: 88,
                transport_body_bytes: 86,
            }
        ));
    }

    #[test]
    fn protocol_spec_lookup_is_pure_and_atomic_for_bm139x() {
        let spec = protocol_spec_for_detected_chip(0x1398).unwrap();
        assert_eq!(spec.family, ChipFamily::Bm1398);
        assert_eq!(spec.response.body_bytes, BM139X_RESPONSE_BODY_BYTES);
        assert_eq!(spec.response.wire_bytes(), 9);
        assert_eq!(spec.baud.family, BaudChipFamily::Bm1398);
        assert!(matches!(
            spec.work_transport,
            WorkTransportShape::FpgaMidstateFifo {
                format: WorkFrameFormat::Bm1397 { num_midstates: 4 },
                fifo_words: 36,
            }
        ));
    }

    #[test]
    fn protocol_spec_lookup_rejects_unknown_chip_ids() {
        assert!(protocol_spec_for_detected_chip(0xFFFF).is_none());
    }

    #[test]
    fn bm13xx_command_response_crc5_matches_all_retained_contract_vectors() {
        let vectors: &[(&[u8], u8)] = &[
            (&[0x13, 0x97, 0x18, 0x00, 0x00, 0x00], 0x06),
            (&[0x13, 0x62, 0x03, 0x00, 0x00, 0x00], 0x0D),
            (&[0x13, 0x62, 0x03, 0xCA, 0xCA, 0x00, 0x00, 0x00], 0x08),
            (&[0x13, 0x62, 0x03, 0xCC, 0xCC, 0x00, 0x00, 0x00], 0x12),
            (&[0x13, 0x62, 0x03, 0xF4, 0xF4, 0x00, 0x00, 0x00], 0x02),
            (&[0x13, 0x62, 0x03, 0xF6, 0xF6, 0x00, 0x00, 0x00], 0x17),
            (&[0x13, 0x62, 0x03, 0xF8, 0xF8, 0x00, 0x00, 0x00], 0x13),
            (&[0x13, 0x62, 0x03, 0xFA, 0xFA, 0x00, 0x00, 0x00], 0x06),
        ];

        for &(payload, expected) in vectors {
            assert_eq!(bm13xx_command_response_crc5(payload), expected);
        }
    }

    #[test]
    fn bm13xx_command_response_crc5_is_stream_composable_and_five_bit_bounded() {
        let corpus = [
            &[][..],
            &[0x13][..],
            &[0x13, 0x62, 0x03][..],
            &[0x00, 0x55, 0xAA, 0xFF][..],
        ];

        assert_eq!(bm13xx_command_response_crc5(&[]), 0x03);
        for prefix in corpus {
            for suffix in corpus {
                let mut joined = prefix.to_vec();
                joined.extend_from_slice(suffix);
                let prefix_state = bm13xx_command_response_crc5(prefix);
                assert_eq!(
                    bm13xx_command_response_crc5(&joined),
                    bm13xx_response_crc5_with_init(suffix, prefix_state)
                );
                assert!(bm13xx_command_response_crc5(&joined) < 0x20);
            }
        }
    }

    #[test]
    fn bm13xx_command_response_crc5_detects_each_single_bit_vector_mutation() {
        let payloads: &[&[u8]] = &[
            &[0x13, 0x97, 0x18, 0x00, 0x00, 0x00],
            &[0x13, 0x62, 0x03, 0x00, 0x00, 0x00],
            &[0x13, 0x62, 0x03, 0xCA, 0xCA, 0x00, 0x00, 0x00],
        ];

        for &payload in payloads {
            let expected = bm13xx_command_response_crc5(payload);
            for bit_index in 0..payload.len() * 8 {
                let mut mutated = payload.to_vec();
                mutated[bit_index / 8] ^= 1 << (bit_index % 8);
                assert_ne!(
                    bm13xx_command_response_crc5(&mutated),
                    expected,
                    "single-bit mutation {bit_index} was not detected for {payload:02X?}"
                );
            }
        }
    }
}
