//! Pure BM1362 assigned-address response validation.
//!
//! BM1362 emits an 11-byte command/register response after addresses have
//! been assigned:
//!
//! ```text
//! AA 55 | 13 62 03 <value-address> | <responder-address> 00 00 00 | trailer
//! ```
//!
//! The layout is independently described by the retained ESP-Miner BM136x
//! packed command-response structs (
//! asic/bm1366.c`, with byte-identical structs in `bm1368.c` and `bm1370.c`):
//! four value bytes, responder address, register address, two reserved bytes,
//! and the response trailer. Retained DCENT Xilinx captures under
//!  contain CRC-valid examples
//! for several even addresses. In those register-0 replies the low byte of the
//! ChipAddress register value equals the separate responder-address field.
//!
//! This module deliberately produces observations, not hardware identity.
//! Even an [`AssignedAddressCoverage::ExactExpectedSet`] result only describes
//! the supplied byte window. It is not bound to a UART endpoint, composition
//! session, reset/assignment transaction, or physical topology and therefore
//! must never be promoted directly to daemon `MeasuredEnumeration` evidence.

use crate::drivers::bm1362;
use crate::protocol::{bm13xx_command_response_crc5, RESP_PREAMBLE};
use std::collections::{BTreeMap, BTreeSet};

/// BM136x response bytes after address assignment, including `AA 55`.
pub const ASSIGNED_ADDRESS_RESPONSE_BYTES: usize = 11;

/// BM1362 encodes its four big cores as `core_count - 1 == 0x03` in
/// ChipAddress register readback.
///
/// Source: the retained AM3-BB and Xilinx BM1362 responses cited in this
/// module, cross-checked against ESP-Miner's `count_asic_chips()` decoder,
/// which labels response byte 4 `CORE_NUM`.
pub const BM1362_CORE_COUNT_ENCODING: u8 = 0x03;

/// A CRC-verified, internally consistent BM1362 ChipAddress response.
///
/// This is a wire observation only. It carries no endpoint or session proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignedAddressObservation {
    register_value: u32,
    responder_address: u8,
    trailer: u8,
}

impl AssignedAddressObservation {
    pub fn register_value(self) -> u32 {
        self.register_value
    }

    pub fn responder_address(self) -> u8 {
        self.responder_address
    }

    pub fn trailer(self) -> u8 {
        self.trailer
    }
}

/// Why bytes cannot be treated as a BM1362 assigned-address observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignedAddressResponseError {
    Length {
        observed: usize,
    },
    Preamble {
        observed: [u8; 2],
    },
    JobResponseTrailer {
        trailer: u8,
    },
    UnsupportedTrailerFlags {
        trailer: u8,
    },
    CrcMismatch {
        expected: u8,
        observed: u8,
    },
    ChipId {
        observed: u16,
    },
    CoreCountEncoding {
        observed: u8,
    },
    RegisterAddress {
        observed: u8,
    },
    ReservedBytes {
        observed: [u8; 2],
    },
    AddressMismatch {
        value_address: u8,
        responder_address: u8,
    },
}

/// Parse one exact BM1362 ChipAddress response from an assigned chain.
///
/// The response CRC covers bytes 2 through 9. Trailer bit 7 must identify a
/// command response. Trailer bits 6:5 are not yet semantically understood, so
/// this identity-adjacent parser rejects them instead of guessing.
pub fn parse_assigned_address_response(
    frame: &[u8],
) -> Result<AssignedAddressObservation, AssignedAddressResponseError> {
    if frame.len() != ASSIGNED_ADDRESS_RESPONSE_BYTES {
        return Err(AssignedAddressResponseError::Length {
            observed: frame.len(),
        });
    }
    if frame[..2] != RESP_PREAMBLE {
        return Err(AssignedAddressResponseError::Preamble {
            observed: [frame[0], frame[1]],
        });
    }

    let trailer = frame[10];
    if trailer & 0x80 != 0 {
        return Err(AssignedAddressResponseError::JobResponseTrailer { trailer });
    }
    if trailer & 0x60 != 0 {
        return Err(AssignedAddressResponseError::UnsupportedTrailerFlags { trailer });
    }
    let expected_crc = bm13xx_command_response_crc5(&frame[2..10]);
    let observed_crc = trailer & 0x1f;
    if observed_crc != expected_crc {
        return Err(AssignedAddressResponseError::CrcMismatch {
            expected: expected_crc,
            observed: observed_crc,
        });
    }

    let chip_id = u16::from_be_bytes([frame[2], frame[3]]);
    if chip_id != bm1362::CHIP_ID {
        return Err(AssignedAddressResponseError::ChipId { observed: chip_id });
    }
    if frame[4] != BM1362_CORE_COUNT_ENCODING {
        return Err(AssignedAddressResponseError::CoreCountEncoding { observed: frame[4] });
    }
    if frame[7] != bm1362::regs::CHIP_ADDRESS {
        return Err(AssignedAddressResponseError::RegisterAddress { observed: frame[7] });
    }
    if frame[8..10] != [0, 0] {
        return Err(AssignedAddressResponseError::ReservedBytes {
            observed: [frame[8], frame[9]],
        });
    }
    if frame[5] != frame[6] {
        return Err(AssignedAddressResponseError::AddressMismatch {
            value_address: frame[5],
            responder_address: frame[6],
        });
    }

    Ok(AssignedAddressObservation {
        register_value: u32::from_be_bytes([frame[2], frame[3], frame[4], frame[5]]),
        responder_address: frame[6],
        trailer,
    })
}

/// Coverage of the caller-supplied response window against an expected set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignedAddressCoverage {
    Empty,
    Partial,
    ExactExpectedSet,
}

/// One rejected response and its stable input index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedAssignedAddressResponse {
    pub response_index: usize,
    pub reason: AssignedAddressResponseError,
}

/// Deterministic assessment of an assigned-address response window.
///
/// `ExactExpectedSet` means only that this supplied window contains one valid
/// response for every caller-declared address and no other/rejected response.
/// It is not a receipt and carries no temporal, endpoint, or topology binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedAddressScanReport {
    pub coverage: AssignedAddressCoverage,
    pub valid_responses: usize,
    pub unique_addresses: Vec<u8>,
    pub duplicate_addresses: Vec<u8>,
    pub missing_addresses: Vec<u8>,
    pub unexpected_addresses: Vec<u8>,
    pub rejected: Vec<RejectedAssignedAddressResponse>,
}

/// Assess exact frames against an explicit expected address set.
///
/// The expected set is an input assumption. Callers must derive it from an
/// independently authorized assignment plan rather than from these frames.
pub fn assess_assigned_address_scan<'a, I>(
    frames: I,
    expected_addresses: &[u8],
) -> AssignedAddressScanReport
where
    I: IntoIterator<Item = &'a [u8]>,
{
    let expected: BTreeSet<u8> = expected_addresses.iter().copied().collect();
    let mut counts = BTreeMap::<u8, usize>::new();
    let mut rejected = Vec::new();
    let mut valid_responses = 0usize;

    for (response_index, frame) in frames.into_iter().enumerate() {
        match parse_assigned_address_response(frame) {
            Ok(observation) => {
                valid_responses += 1;
                *counts.entry(observation.responder_address()).or_default() += 1;
            }
            Err(reason) => rejected.push(RejectedAssignedAddressResponse {
                response_index,
                reason,
            }),
        }
    }

    let observed: BTreeSet<u8> = counts.keys().copied().collect();
    let unique_addresses = observed.iter().copied().collect::<Vec<_>>();
    let duplicate_addresses = counts
        .iter()
        .filter_map(|(&address, &count)| (count > 1).then_some(address))
        .collect::<Vec<_>>();
    let missing_addresses = expected.difference(&observed).copied().collect::<Vec<_>>();
    let unexpected_addresses = observed.difference(&expected).copied().collect::<Vec<_>>();

    let coverage = if valid_responses == 0 && rejected.is_empty() {
        AssignedAddressCoverage::Empty
    } else if rejected.is_empty()
        && duplicate_addresses.is_empty()
        && missing_addresses.is_empty()
        && unexpected_addresses.is_empty()
        && valid_responses == expected.len()
    {
        AssignedAddressCoverage::ExactExpectedSet
    } else {
        AssignedAddressCoverage::Partial
    };

    AssignedAddressScanReport {
        coverage,
        valid_responses,
        unique_addresses,
        duplicate_addresses,
        missing_addresses,
        unexpected_addresses,
        rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exact vectors from:
    // - `2026-05-15-dcentrald-xil-rx-frame-instrumented.log:171,173,175-178`
    // - `2026-05-15-dcentrald-xil-phase2-regression-bip320-sweep.log:178-179`
    const RETAINED_ASSIGNED_VECTORS: &[(&[u8; 11], u8)] = &[
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xCA, 0xCA, 0, 0, 0, 0x08],
            0xCA,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xCC, 0xCC, 0, 0, 0, 0x12],
            0xCC,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xF0, 0xF0, 0, 0, 0, 0x0D],
            0xF0,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xF2, 0xF2, 0, 0, 0, 0x18],
            0xF2,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xF4, 0xF4, 0, 0, 0, 0x02],
            0xF4,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xF6, 0xF6, 0, 0, 0, 0x17],
            0xF6,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xF8, 0xF8, 0, 0, 0, 0x13],
            0xF8,
        ),
        (
            &[0xAA, 0x55, 0x13, 0x62, 0x03, 0xFA, 0xFA, 0, 0, 0, 0x06],
            0xFA,
        ),
    ];

    fn frame_for(address: u8) -> [u8; 11] {
        let mut frame = [
            0xAA,
            0x55,
            0x13,
            0x62,
            BM1362_CORE_COUNT_ENCODING,
            address,
            address,
            0,
            0,
            0,
            0,
        ];
        frame[10] = bm13xx_command_response_crc5(&frame[2..10]);
        frame
    }

    #[test]
    fn parses_all_retained_crc_valid_assigned_address_vectors() {
        for &(frame, expected_address) in RETAINED_ASSIGNED_VECTORS {
            let observation = parse_assigned_address_response(frame).unwrap();
            assert_eq!(observation.responder_address(), expected_address);
            assert_eq!(
                observation.register_value() & 0xff,
                u32::from(expected_address)
            );
            assert_eq!(observation.trailer(), frame[10]);
        }
    }

    #[test]
    fn unassigned_short_response_cannot_be_rewrapped_as_assigned() {
        let unassigned = [0xAA, 0x55, 0x13, 0x62, 0x03, 0, 0, 0, 0x0D];
        assert_eq!(
            parse_assigned_address_response(&unassigned),
            Err(AssignedAddressResponseError::Length { observed: 9 })
        );
    }

    #[test]
    fn parser_rejects_each_identity_bearing_field_mismatch() {
        let valid = frame_for(0x20);
        let cases = [(2, 0x12), (4, 0x04), (5, 0x22), (7, 0x04), (8, 0x01)];
        for (index, replacement) in cases {
            let mut mutated = valid;
            mutated[index] = replacement;
            mutated[10] = bm13xx_command_response_crc5(&mutated[2..10]);
            assert!(parse_assigned_address_response(&mutated).is_err());
        }
    }

    #[test]
    fn every_address_round_trips_and_every_single_wire_bit_error_is_rejected() {
        for address in 0u8..=u8::MAX {
            let frame = frame_for(address);
            assert_eq!(
                parse_assigned_address_response(&frame)
                    .unwrap()
                    .responder_address(),
                address
            );

            for bit_index in 0..ASSIGNED_ADDRESS_RESPONSE_BYTES * 8 {
                let mut mutated = frame;
                mutated[bit_index / 8] ^= 1 << (bit_index % 8);
                assert!(
                    parse_assigned_address_response(&mutated).is_err(),
                    "address 0x{address:02X}, wire bit {bit_index} was not rejected"
                );
            }
        }
    }

    #[test]
    fn parser_rejects_crc_damage_job_marker_and_unmodelled_flags() {
        let valid = frame_for(0x20);

        let mut crc_damaged = valid;
        crc_damaged[3] ^= 1;
        assert!(matches!(
            parse_assigned_address_response(&crc_damaged),
            Err(AssignedAddressResponseError::CrcMismatch { .. })
        ));

        let mut job = valid;
        job[10] |= 0x80;
        assert_eq!(
            parse_assigned_address_response(&job),
            Err(AssignedAddressResponseError::JobResponseTrailer { trailer: job[10] })
        );

        let mut unknown_flags = valid;
        unknown_flags[10] |= 0x20;
        assert_eq!(
            parse_assigned_address_response(&unknown_flags),
            Err(AssignedAddressResponseError::UnsupportedTrailerFlags {
                trailer: unknown_flags[10]
            })
        );
    }

    #[test]
    fn parser_rejects_retained_malformed_cc_frame() {
        // FIRST-ACCEPTED-SHARES line 173. It resembles the adjacent CC
        // response but carries non-zero reserved bytes and no valid trailer;
        // it must remain rejected rather than becoming endpoint evidence.
        let malformed = [
            0xAA, 0x55, 0x13, 0x62, 0x03, 0xCC, 0xCC, 0x00, 0xF0, 0x00, 0x00,
        ];
        assert_eq!(
            parse_assigned_address_response(&malformed),
            Err(AssignedAddressResponseError::CrcMismatch {
                expected: 0x1C,
                observed: 0x00,
            })
        );
    }

    #[test]
    fn retained_suffix_is_explicitly_partial_not_complete_chain_evidence() {
        let expected = (0u8..=250).step_by(2).collect::<Vec<_>>();
        let report = assess_assigned_address_scan(
            RETAINED_ASSIGNED_VECTORS
                .iter()
                .map(|(frame, _)| frame.as_slice()),
            &expected,
        );

        assert_eq!(report.coverage, AssignedAddressCoverage::Partial);
        assert_eq!(report.valid_responses, RETAINED_ASSIGNED_VECTORS.len());
        assert_eq!(
            report.unique_addresses,
            vec![0xCA, 0xCC, 0xF0, 0xF2, 0xF4, 0xF6, 0xF8, 0xFA]
        );
        assert_eq!(report.missing_addresses.len(), 118);
        assert!(report.duplicate_addresses.is_empty());
        assert!(report.unexpected_addresses.is_empty());
        assert!(report.rejected.is_empty());
    }

    #[test]
    fn exact_expected_set_requires_one_clean_response_per_address() {
        let expected = [0x00, 0x02, 0x04];
        let frames = expected.map(frame_for);
        let exact =
            assess_assigned_address_scan(frames.iter().map(|frame| frame.as_slice()), &expected);
        assert_eq!(exact.coverage, AssignedAddressCoverage::ExactExpectedSet);

        let with_duplicate = [
            &frames[0][..],
            &frames[1][..],
            &frames[1][..],
            &frames[2][..],
        ];
        let partial = assess_assigned_address_scan(with_duplicate, &expected);
        assert_eq!(partial.coverage, AssignedAddressCoverage::Partial);
        assert_eq!(partial.duplicate_addresses, vec![0x02]);
    }

    #[test]
    fn rejected_and_unexpected_frames_keep_scan_partial() {
        let expected = [0x00, 0x02];
        let frame0 = frame_for(0x00);
        let frame4 = frame_for(0x04);
        let unassigned = [0xAA, 0x55, 0x13, 0x62, 0x03, 0, 0, 0, 0x0D];
        let report =
            assess_assigned_address_scan([&frame0[..], &frame4[..], &unassigned[..]], &expected);

        assert_eq!(report.coverage, AssignedAddressCoverage::Partial);
        assert_eq!(report.missing_addresses, vec![0x02]);
        assert_eq!(report.unexpected_addresses, vec![0x04]);
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].response_index, 2);
    }

    #[test]
    fn empty_window_is_distinct_from_partial_evidence() {
        let report = assess_assigned_address_scan(std::iter::empty::<&[u8]>(), &[0, 2]);
        assert_eq!(report.coverage, AssignedAddressCoverage::Empty);
        assert_eq!(report.missing_addresses, vec![0, 2]);
    }
}
