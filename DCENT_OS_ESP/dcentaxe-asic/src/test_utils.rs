// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies - dcentaxe-asic test helpers

/// Fill the final byte of an ASIC response frame with a CRC5-valid value.
///
/// ASIC response parsers use the final byte both as CRC payload and, for nonce
/// frames, as the job-response flag source. Keep this test-only so production
/// packet construction stays byte-for-byte with the ported driver code.
pub(crate) fn with_valid_crc5<const N: usize>(mut frame: [u8; N], job_response: bool) -> [u8; N] {
    let range = if job_response {
        0x80u8..=0xffu8
    } else {
        0x00u8..=0x7fu8
    };

    for candidate in range {
        frame[N - 1] = candidate;
        if crate::crc::crc5(&frame[2..]) == 0 {
            return frame;
        }
    }

    panic!("no valid CRC5 byte found for test frame");
}
