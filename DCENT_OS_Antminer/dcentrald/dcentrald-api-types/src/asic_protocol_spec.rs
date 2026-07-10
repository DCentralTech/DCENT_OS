//! Host-safe ASIC protocol specs selected atomically from chip detection.
//!
//! This module keeps response body length, baud plan, and work transport shape
//! in one HAL-free value. Runtime code should select this once from the detected
//! chip ID and pass the spec down, instead of deriving response parsing, baud,
//! and work framing through separate family checks.

use crate::baud_switch::{BaudChipFamily, BaudPlan};
use crate::chip_init::ChipFamily;
use crate::work_dispatch::WorkFrameFormat;
use serde::Serialize;

pub const RESPONSE_PREAMBLE_BYTES: u8 = 2;
pub const BM1387_RESPONSE_BODY_BYTES: u8 = 7;
pub const BM139X_RESPONSE_BODY_BYTES: u8 = 7;
pub const BM136X_RESPONSE_BODY_BYTES: u8 = 9;
pub const MIDSTATE_FIFO_WORK_WORDS: u8 = 36;
pub const BM136X_SERIAL_WORK_WIRE_BYTES: u8 = 88;
pub const BM136X_UART_TRANS_BODY_BYTES: u8 = 86;

/// ASIC response parser shape after chip detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AsicResponseLengthSpec {
    /// Bytes in the response body after the `AA 55` preamble.
    pub body_bytes: u8,
    /// ASIC response preamble length. Bitmain BM13xx responses use `AA 55`.
    pub preamble_bytes: u8,
}

impl AsicResponseLengthSpec {
    pub const fn wire_bytes(self) -> u8 {
        self.preamble_bytes + self.body_bytes
    }
}

/// Work transport shape implied by the detected chip family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum WorkTransportShape {
    /// FPGA WORK_TX FIFO midstate slots, used by BM1387 and BM139x paths.
    FpgaMidstateFifo {
        format: WorkFrameFormat,
        fifo_words: u8,
    },
    /// Full-header UART work frame, used by BM136x serial/BB/Amlogic paths.
    UartFullHeader {
        format: WorkFrameFormat,
        wire_frame_bytes: u8,
        transport_body_bytes: u8,
    },
}

/// One atomic protocol selection for a detected ASIC chip ID.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AsicProtocolSpec {
    pub chip_id: u16,
    pub family: ChipFamily,
    pub response: AsicResponseLengthSpec,
    pub baud: BaudPlan,
    pub work_transport: WorkTransportShape,
}

impl AsicProtocolSpec {
    pub fn for_detected_chip_id(chip_id: u16) -> Option<Self> {
        let family = chip_family_from_chip_id(chip_id)?;
        Self::for_family(family)
    }

    pub fn for_family(family: ChipFamily) -> Option<Self> {
        Some(Self {
            chip_id: chip_id_for_family(family)?,
            family,
            response: response_length_for_family(family)?,
            baud: BaudPlan::canonical(baud_family_for_chip_family(family)?),
            work_transport: work_transport_for_family(family)?,
        })
    }
}

pub fn chip_family_from_chip_id(chip_id: u16) -> Option<ChipFamily> {
    match chip_id {
        0x1387 => Some(ChipFamily::Bm1387),
        0x1397 => Some(ChipFamily::Bm1397),
        0x1398 => Some(ChipFamily::Bm1398),
        0x1362 => Some(ChipFamily::Bm1362),
        0x1366 => Some(ChipFamily::Bm1366),
        0x1368 => Some(ChipFamily::Bm1368),
        0x1370 => Some(ChipFamily::Bm1370),
        _ => None,
    }
}

pub fn chip_id_for_family(family: ChipFamily) -> Option<u16> {
    match family {
        ChipFamily::Bm1387 => Some(0x1387),
        ChipFamily::Bm1397 => Some(0x1397),
        ChipFamily::Bm1398 => Some(0x1398),
        ChipFamily::Bm1362 => Some(0x1362),
        ChipFamily::Bm1366 => Some(0x1366),
        ChipFamily::Bm1368 => Some(0x1368),
        ChipFamily::Bm1370 => Some(0x1370),
        ChipFamily::Bm1485 | ChipFamily::Bm1489 | ChipFamily::Bm1360 | ChipFamily::Bm1491 => None,
    }
}

pub fn response_length_for_family(family: ChipFamily) -> Option<AsicResponseLengthSpec> {
    let body_bytes = match family {
        ChipFamily::Bm1387 => BM1387_RESPONSE_BODY_BYTES,
        ChipFamily::Bm1397 | ChipFamily::Bm1398 => BM139X_RESPONSE_BODY_BYTES,
        ChipFamily::Bm1362 | ChipFamily::Bm1366 | ChipFamily::Bm1368 | ChipFamily::Bm1370 => {
            BM136X_RESPONSE_BODY_BYTES
        }
        ChipFamily::Bm1485 | ChipFamily::Bm1489 | ChipFamily::Bm1360 | ChipFamily::Bm1491 => {
            return None;
        }
    };

    Some(AsicResponseLengthSpec {
        body_bytes,
        preamble_bytes: RESPONSE_PREAMBLE_BYTES,
    })
}

pub fn baud_family_for_chip_family(family: ChipFamily) -> Option<BaudChipFamily> {
    match family {
        ChipFamily::Bm1387 => Some(BaudChipFamily::Bm1387),
        ChipFamily::Bm1397 => Some(BaudChipFamily::Bm1397),
        ChipFamily::Bm1398 => Some(BaudChipFamily::Bm1398),
        ChipFamily::Bm1362 => Some(BaudChipFamily::Bm1362),
        ChipFamily::Bm1366 => Some(BaudChipFamily::Bm1366),
        ChipFamily::Bm1368 => Some(BaudChipFamily::Bm1368),
        ChipFamily::Bm1370 => Some(BaudChipFamily::Bm1370),
        ChipFamily::Bm1485 => Some(BaudChipFamily::Bm1485),
        ChipFamily::Bm1489 | ChipFamily::Bm1360 | ChipFamily::Bm1491 => None,
    }
}

pub fn work_transport_for_family(family: ChipFamily) -> Option<WorkTransportShape> {
    match family {
        ChipFamily::Bm1387 => Some(WorkTransportShape::FpgaMidstateFifo {
            format: WorkFrameFormat::Bm1387 { num_midstates: 4 },
            fifo_words: MIDSTATE_FIFO_WORK_WORDS,
        }),
        ChipFamily::Bm1397 | ChipFamily::Bm1398 => Some(WorkTransportShape::FpgaMidstateFifo {
            format: WorkFrameFormat::Bm1397 { num_midstates: 4 },
            fifo_words: MIDSTATE_FIFO_WORK_WORDS,
        }),
        ChipFamily::Bm1362 => Some(WorkTransportShape::UartFullHeader {
            format: WorkFrameFormat::Bm1362,
            wire_frame_bytes: BM136X_SERIAL_WORK_WIRE_BYTES,
            transport_body_bytes: BM136X_UART_TRANS_BODY_BYTES,
        }),
        ChipFamily::Bm1366 => Some(WorkTransportShape::UartFullHeader {
            format: WorkFrameFormat::Bm1366,
            wire_frame_bytes: BM136X_SERIAL_WORK_WIRE_BYTES,
            transport_body_bytes: BM136X_UART_TRANS_BODY_BYTES,
        }),
        ChipFamily::Bm1368 => Some(WorkTransportShape::UartFullHeader {
            format: WorkFrameFormat::Bm1368,
            wire_frame_bytes: BM136X_SERIAL_WORK_WIRE_BYTES,
            transport_body_bytes: BM136X_UART_TRANS_BODY_BYTES,
        }),
        ChipFamily::Bm1370 => Some(WorkTransportShape::UartFullHeader {
            format: WorkFrameFormat::Bm1370,
            wire_frame_bytes: BM136X_SERIAL_WORK_WIRE_BYTES,
            transport_body_bytes: BM136X_UART_TRANS_BODY_BYTES,
        }),
        ChipFamily::Bm1485 | ChipFamily::Bm1489 | ChipFamily::Bm1360 | ChipFamily::Bm1491 => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm136x_detection_selects_9_byte_response_body_with_baud_and_full_header() {
        let cases = [
            (
                0x1362,
                ChipFamily::Bm1362,
                BaudChipFamily::Bm1362,
                WorkFrameFormat::Bm1362,
            ),
            (
                0x1366,
                ChipFamily::Bm1366,
                BaudChipFamily::Bm1366,
                WorkFrameFormat::Bm1366,
            ),
            (
                0x1368,
                ChipFamily::Bm1368,
                BaudChipFamily::Bm1368,
                WorkFrameFormat::Bm1368,
            ),
            (
                0x1370,
                ChipFamily::Bm1370,
                BaudChipFamily::Bm1370,
                WorkFrameFormat::Bm1370,
            ),
        ];

        for (chip_id, family, baud_family, format) in cases {
            let spec = AsicProtocolSpec::for_detected_chip_id(chip_id).unwrap();
            assert_eq!(spec.chip_id, chip_id);
            assert_eq!(spec.family, family);
            assert_eq!(spec.response.body_bytes, BM136X_RESPONSE_BODY_BYTES);
            assert_eq!(spec.response.wire_bytes(), 11);
            assert_eq!(spec.baud.family, baud_family);
            match spec.work_transport {
                WorkTransportShape::UartFullHeader {
                    format: got,
                    wire_frame_bytes,
                    transport_body_bytes,
                } => {
                    assert_eq!(got, format);
                    assert_eq!(wire_frame_bytes, 88);
                    assert_eq!(transport_body_bytes, 86);
                }
                other => panic!("expected full-header UART shape, got {:?}", other),
            }
        }
    }

    #[test]
    fn bm139x_detection_selects_7_byte_response_body_with_baud_and_midstate_fifo() {
        for (chip_id, family, baud_family) in [
            (0x1397, ChipFamily::Bm1397, BaudChipFamily::Bm1397),
            (0x1398, ChipFamily::Bm1398, BaudChipFamily::Bm1398),
        ] {
            let spec = AsicProtocolSpec::for_detected_chip_id(chip_id).unwrap();
            assert_eq!(spec.chip_id, chip_id);
            assert_eq!(spec.family, family);
            assert_eq!(spec.response.body_bytes, BM139X_RESPONSE_BODY_BYTES);
            assert_eq!(spec.response.wire_bytes(), 9);
            assert_eq!(spec.baud.family, baud_family);
            match spec.work_transport {
                WorkTransportShape::FpgaMidstateFifo { format, fifo_words } => {
                    assert_eq!(format, WorkFrameFormat::Bm1397 { num_midstates: 4 });
                    assert_eq!(fifo_words, MIDSTATE_FIFO_WORK_WORDS);
                }
                other => panic!("expected FPGA midstate FIFO shape, got {:?}", other),
            }
        }
    }

    #[test]
    fn bm1387_detection_uses_legacy_midstate_fifo_shape() {
        let spec = AsicProtocolSpec::for_detected_chip_id(0x1387).unwrap();
        assert_eq!(spec.family, ChipFamily::Bm1387);
        assert_eq!(spec.response.body_bytes, BM1387_RESPONSE_BODY_BYTES);
        assert_eq!(spec.response.wire_bytes(), 9);
        assert_eq!(spec.baud.family, BaudChipFamily::Bm1387);
        assert!(matches!(
            spec.work_transport,
            WorkTransportShape::FpgaMidstateFifo {
                format: WorkFrameFormat::Bm1387 { num_midstates: 4 },
                fifo_words: MIDSTATE_FIFO_WORK_WORDS
            }
        ));
    }

    #[test]
    fn unsupported_or_placeholder_chips_fail_closed() {
        assert!(AsicProtocolSpec::for_detected_chip_id(0x1360).is_none());
        assert!(AsicProtocolSpec::for_detected_chip_id(0x1491).is_none());
        assert!(AsicProtocolSpec::for_detected_chip_id(0xFFFF).is_none());
        assert!(AsicProtocolSpec::for_family(ChipFamily::Bm1360).is_none());
        assert!(AsicProtocolSpec::for_family(ChipFamily::Bm1491).is_none());
    }

    #[test]
    fn protocol_spec_serializes_as_one_contract() {
        let spec = AsicProtocolSpec::for_detected_chip_id(0x1362).unwrap();
        let json = serde_json::to_value(spec).unwrap();

        assert_eq!(json["chip_id"].as_u64(), Some(0x1362));
        assert_eq!(json["family"].as_str(), Some("bm1362"));
        assert_eq!(json["response"]["body_bytes"].as_u64(), Some(9));
        assert!(json.get("baud").is_some());
        assert!(json.get("work_transport").is_some());
    }
}
