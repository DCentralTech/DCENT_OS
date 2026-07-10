//! Difficulty target conversions for Stratum V1.
//!
//! Stratum pools communicate difficulty as a floating-point number.
//! Internally, difficulty maps to a 256-bit target value. A share is valid
//! if SHA256d(block_header) <= target.
//!
//! There are TWO different "difficulty 1" definitions in Bitcoin:
//!
//!   1. **bdiff** (block difficulty): used in Bitcoin Core and the nbits field.
//!      bdiff_1_target = 0x00000000FFFF0000...00 (only 16 significant bits)
//!
//!   2. **pdiff** (pool difficulty): used by Stratum pools.
//!      pdiff_1_target = 0x00000000FFFFFFFF...FF (full 224 bits of precision)
//!
//! Stratum V1 uses **pdiff**. This module implements the pdiff conversion.
//!
//! Conversions:
//!   - Pool difficulty 1 corresponds to pdiff_1_target
//!   - target = pdiff_1_target / difficulty
//!   - Higher difficulty = lower (harder) target

/// The "pool difficulty 1" target as a 256-bit big-endian number.
///
/// pdiff_1 = 0x00000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF
///         = 2^224 - 1
///
/// This differs from bdiff_1 (0x00000000FFFF0000...00) used in Bitcoin Core.
/// All Stratum pools use pdiff.
pub const PDIFF1_TARGET: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// The bdiff "difficulty 1" target (used for nbits comparison only).
///
/// bdiff_1 = 0x00000000FFFF0000000000000000000000000000000000000000000000000000
pub const BDIFF1_TARGET: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Convert a pool difficulty (pdiff) to a 256-bit share target.
///
/// target = pdiff_1_target / difficulty
///        = (2^224 - 1) / difficulty
///
/// Returns a 32-byte big-endian target. A share is valid if its hash
/// (treated as a big-endian 256-bit number) is less than or equal to this target.
///
/// Integer difficulties use exact long division. Fractional difficulties use
/// f64 placement of target = 2^224 / difficulty, preserving Stratum pools that
/// send values such as 1.5 instead of truncating them to 1.
pub fn difficulty_to_target(difficulty: f64) -> [u8; 32] {
    // Finiteness FIRST: NaN and BOTH infinities are malformed pool input and
    // must fail closed. NEG_INFINITY is numerically <= 0.0, so if the accept-all
    // branch ran first it would treat -inf as "accept everything" (fail-OPEN —
    // every nonce would validate as a share). Order matters: check is_finite()
    // before the <= 0.0 "easiest" sentinel.
    if !difficulty.is_finite() {
        return [0u8; 32]; // Fail closed for NaN / ±Infinity from malformed pool input
    }

    if difficulty <= 0.0 {
        return [0xFF; 32]; // Accept everything (finite non-positive = "easiest" sentinel)
    }

    if difficulty <= 1.0 {
        return PDIFF1_TARGET;
    }

    if difficulty.fract() != 0.0 || difficulty > u64::MAX as f64 {
        return fractional_difficulty_to_target(difficulty);
    }

    divide_pdiff1_by_u64(difficulty as u64)
}

fn divide_pdiff1_by_u64(d: u64) -> [u8; 32] {
    if d == 0 {
        return PDIFF1_TARGET;
    }

    // pdiff_1 = 2^224 - 1, represented as 8 big-endian u32 words:
    //   [0x00000000, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF,
    //    0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF]
    //
    // Schoolbook long division: divide 256-bit numerator by 64-bit divisor,
    // processing one 32-bit "digit" at a time with a 64-bit remainder.
    let words: [u32; 8] = [
        0x0000_0000,
        0xFFFF_FFFF,
        0xFFFF_FFFF,
        0xFFFF_FFFF,
        0xFFFF_FFFF,
        0xFFFF_FFFF,
        0xFFFF_FFFF,
        0xFFFF_FFFF,
    ];

    let mut result = [0u32; 8];
    let mut remainder: u64 = 0;

    for i in 0..8 {
        let dividend = (remainder << 32) | (words[i] as u64);
        result[i] = (dividend / d) as u32;
        remainder = dividend % d;
    }

    // Convert result words to big-endian bytes
    let mut target = [0u8; 32];
    for i in 0..8 {
        target[i * 4..(i + 1) * 4].copy_from_slice(&result[i].to_be_bytes());
    }
    target
}

fn fractional_difficulty_to_target(difficulty: f64) -> [u8; 32] {
    let mut target = [0u8; 32];
    let value_f64 = (2.0_f64).powi(224) / difficulty;
    if !value_f64.is_finite() || value_f64 <= 0.0 {
        return target;
    }

    let bits = value_f64.to_bits();
    let ieee_exp = ((bits >> 52) & 0x7FF) as i32 - 1023;
    let ieee_mantissa = (bits & 0x000F_FFFF_FFFF_FFFF) | 0x0010_0000_0000_0000;
    let lsb_bit_pos = ieee_exp - 52;

    if lsb_bit_pos < -7 {
        return target;
    }

    let byte_offset = if lsb_bit_pos >= 0 {
        lsb_bit_pos / 8
    } else {
        (lsb_bit_pos - 7) / 8
    };
    let bit_shift = (lsb_bit_pos - byte_offset * 8) as u32;

    for i in 0..8 {
        let src_byte = ((ieee_mantissa >> (i * 8)) & 0xFF) as u8;
        let target_byte_idx = 31i32 - (byte_offset + i);
        if !(0..32).contains(&target_byte_idx) {
            continue;
        }

        let shifted_lo = (src_byte as u16) << bit_shift;
        target[target_byte_idx as usize] |= (shifted_lo & 0xFF) as u8;

        if bit_shift > 0 {
            let carry = (shifted_lo >> 8) as u8;
            if carry != 0 && target_byte_idx > 0 {
                target[(target_byte_idx - 1) as usize] |= carry;
            }
        }
    }

    target
}

/// Convert a 256-bit hash (big-endian) to its approximate pool difficulty (pdiff).
///
/// difficulty = pdiff_1_target / hash_value
///            = (2^224 - 1) / hash_value
///
/// Returns 0.0 if the hash is all zeros (undefined difficulty).
/// Returns f64::INFINITY if the hash is all zeros except the first few bytes.
pub fn hash_to_difficulty(hash: &[u8; 32]) -> f64 {
    // Find the first non-zero byte
    let leading_zeros = hash.iter().take_while(|&&b| b == 0).count();

    if leading_zeros >= 32 {
        return f64::INFINITY; // Hash is zero -> infinite difficulty
    }

    // Extract the top ~8 bytes of the hash value starting from the first non-zero byte
    // to get a representative f64 value.
    let mut hash_top: u64 = 0;
    let bytes_to_read = 8.min(32 - leading_zeros);
    for i in 0..bytes_to_read {
        hash_top = (hash_top << 8) | hash[leading_zeros + i] as u64;
    }

    // Pad if we read fewer than 8 bytes (hash is very large, only last few bytes non-zero)
    if bytes_to_read < 8 {
        hash_top <<= (8 - bytes_to_read) * 8;
    }

    // The hash value as f64:
    // hash_value = hash_top * 2^((32 - leading_zeros - 8) * 8)
    //            = hash_top * 2^((24 - leading_zeros) * 8)    [if leading_zeros <= 24]
    let hash_shift = (32 - leading_zeros as i32 - 8) * 8;
    let hash_f64 = (hash_top as f64) * (2.0_f64).powi(hash_shift);

    if hash_f64 == 0.0 {
        return f64::INFINITY;
    }

    // difficulty = 2^224 / hash_value (using pdiff_1 ~ 2^224)
    (2.0_f64).powi(224) / hash_f64
}

/// Convert compact difficulty (nbits) to a 256-bit target.
///
/// nbits encoding (Bitcoin compact format):
///   - Byte 0 (MSB): exponent
///   - Bytes 1-3: mantissa (3 bytes)
///   - target = mantissa * 2^(8 * (exponent - 3))
///
/// Special cases:
///   - If mantissa MSB is set, the target is negative (invalid, returns zeros)
///   - Exponent 0 means target is just the mantissa value
pub fn nbits_to_target(nbits: u32) -> [u8; 32] {
    let mut target = [0u8; 32];

    let exp = ((nbits >> 24) & 0xFF) as usize;
    let mantissa = nbits & 0x007FFFFF; // 23 bits (MSB is sign bit in Bitcoin)

    // Check for negative flag (bit 23 of the 24-bit mantissa field)
    if nbits & 0x00800000 != 0 {
        // Negative target — invalid for mining, return zeros
        return target;
    }

    if mantissa == 0 {
        return target; // Zero mantissa means target is 0
    }

    if exp == 0 {
        return target; // Exponent 0 with non-zero mantissa: effectively 0
    }

    // The mantissa occupies 3 bytes at position (32 - exp) in big-endian layout.
    // exp=3 means mantissa is at bytes [29..31] (least significant position).
    // exp=32 means mantissa is at bytes [0..2] (most significant position).
    if exp > 32 {
        return target; // Overflow
    }

    let start = 32usize.saturating_sub(exp);

    // Place the 3-byte mantissa
    if start < 32 {
        target[start] = ((mantissa >> 16) & 0xFF) as u8;
    }
    if start + 1 < 32 {
        target[start + 1] = ((mantissa >> 8) & 0xFF) as u8;
    }
    if start + 2 < 32 {
        target[start + 2] = (mantissa & 0xFF) as u8;
    }

    target
}

/// Check if a hash meets a given share target.
///
/// Returns true if hash <= target (both treated as big-endian 256-bit unsigned integers).
/// This is the core share validation check — the ASIC hardware does this in silicon,
/// but we also validate in software for hardware error detection.
pub fn meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    // Compare byte-by-byte from MSB to LSB (big-endian comparison)
    for i in 0..32 {
        if hash[i] < target[i] {
            return true;
        }
        if hash[i] > target[i] {
            return false;
        }
    }
    true // Equal — hash exactly meets the target
}

/// Convert a pool difficulty to the hardware TicketMask value for ASIC chips.
///
/// The TicketMask tells the ASIC to only report nonces that meet at least
/// this difficulty, reducing USB/UART bandwidth. For BM1387, the default
/// TicketMask is 0xFF (difficulty 256).
///
/// ticket_mask = (difficulty * 2 - 1), clamped to valid range.
/// The ASIC uses: nonce_meets_diff >= (ticket_mask + 1) / 2
pub fn difficulty_to_ticket_mask(difficulty: u32) -> u32 {
    if difficulty == 0 {
        return 0;
    }
    // TicketMask = difficulty * 2 - 1 for the standard Bitmain encoding
    // BM1387: 8-bit field (0x00 = diff 1, 0xFF = diff 256)
    // BM1366+: 16-bit field
    (difficulty.saturating_mul(2)).saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_target_math_is_fail_closed_and_monotone() {
        // Property pins for the accept/reject decision boundary (priority 1/6 — a
        // bug here either accepts invalid shares [pool rejects, wasted work] or
        // rejects valid ones [lost work], and a fail-OPEN target would validate
        // every nonce). These invariants hold EXACTLY (no approximation):
        //   (A) malformed pool difficulty (NaN / +inf / -inf) -> the HARDEST target
        //       [0;32] (fail-CLOSED). -inf is the documented fail-open trap.
        //   (B) a finite non-positive difficulty is the accept-all sentinel [0xFF;32].
        //   (C) meets_target is EXACTLY the big-endian hash <= target compare (incl.
        //       the equal boundary).
        //   (D) the fail-closed target rejects every non-zero hash; the accept-all
        //       target accepts every hash.
        //   (E) difficulty_to_target is MONOTONE: raising the difficulty never yields
        //       an EASIER (larger) target.
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                difficulty_to_target(bad),
                [0u8; 32],
                "non-finite diff {bad} not fail-closed"
            );
        }
        for z in [0.0f64, -1.0, -1e300] {
            assert_eq!(
                difficulty_to_target(z),
                [0xFFu8; 32],
                "finite non-positive {z} not accept-all"
            );
        }

        let mut lcg: u64 = 0xB16B_00B5_1234_9876;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            lcg
        };
        for _ in 0..4000 {
            let mut hash = [0u8; 32];
            let mut targ = [0u8; 32];
            for b in hash.iter_mut() {
                *b = (next() & 0xFF) as u8;
            }
            for b in targ.iter_mut() {
                *b = (next() & 0xFF) as u8;
            }
            // (C) [u8] Ord is lexicographic MSB-first == the big-endian target compare.
            assert_eq!(
                meets_target(&hash, &targ),
                hash.as_slice() <= targ.as_slice(),
                "meets_target disagrees with big-endian <= for {hash:?} vs {targ:?}"
            );
            // (D)
            if hash.iter().any(|&b| b != 0) {
                assert!(
                    !meets_target(&hash, &[0u8; 32]),
                    "fail-closed target accepted a nonzero hash"
                );
            }
            assert!(
                meets_target(&hash, &[0xFFu8; 32]),
                "accept-all target rejected a hash"
            );
        }
        let t = [0x12u8; 32];
        assert!(meets_target(&t, &t), "equal hash/target must meet");

        // (E) monotone across the integer + fractional difficulty paths.
        let diffs = [1.0f64, 2.0, 16.0, 256.0, 65536.0, 1e6, 1e9, 1e12, 1e15];
        let mut prev = difficulty_to_target(diffs[0]);
        for &d in &diffs[1..] {
            let cur = difficulty_to_target(d);
            assert!(
                cur.as_slice() <= prev.as_slice(),
                "difficulty {d} produced an EASIER target than a lower difficulty (fail-open)"
            );
            prev = cur;
        }
    }

    #[test]
    fn test_difficulty_1_target() {
        let target = difficulty_to_target(1.0);
        // At pdiff 1, target should be 0x00000000FFFFFFFF...FF
        assert_eq!(target[0], 0x00);
        assert_eq!(target[1], 0x00);
        assert_eq!(target[2], 0x00);
        assert_eq!(target[3], 0x00);
        assert_eq!(target, PDIFF1_TARGET);
    }

    #[test]
    fn test_difficulty_256_target() {
        let target = difficulty_to_target(256.0);
        // pdiff 256: target = 2^224 / 256 = 2^216
        // = 0x000000000000FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF... wait
        // Actually: pdiff_1 / 256 = 0x00000000_00FFFFFF_FF...FF
        // Byte 4 should be ~0x00, byte 5 should be ~0xFF
        // More precisely: 2^224 / 256 = 2^216
        // 2^216 in big-endian 32 bytes: bytes 0-4 = 0, byte 5 = 0x01, rest 0
        // Wait: 2^216 means bit 216 is set.
        // Byte index = 31 - 216/8 = 31 - 27 = 4. So byte 4, bit 0.
        // target[4] should have bit 0 set = 0x01
        // But pdiff_1 is 2^224 - 1, not exactly 2^224.
        // (2^224 - 1) / 256 = 2^216 - 1/256, which truncates to
        // 0x00000000_00FFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF
        // bytes[0..4] = 0, byte[4] = 0x00, byte[5] = 0xFF
        // Actually let's think more carefully:
        // pdiff_1 = 0x00000000_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF
        // pdiff_1 / 256: shift right by 8 bits
        // = 0x00000000_00FFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF_FFFFFFFF
        // So: target[4] = 0x00, target[5] = 0xFF
        assert_eq!(target[0], 0x00);
        assert_eq!(target[1], 0x00);
        assert_eq!(target[2], 0x00);
        assert_eq!(target[3], 0x00);
        assert_eq!(target[4], 0x00);
        // target[5] should be close to 0xFF (allowing for f64 rounding)
        assert!(
            target[5] >= 0xFE,
            "target[5] = 0x{:02X}, expected ~0xFF",
            target[5]
        );
    }

    #[test]
    fn test_difficulty_65536_target() {
        let target = difficulty_to_target(65536.0);
        // pdiff_1 / 65536 = pdiff_1 >> 16
        // bytes 0..5 = 0, byte 6 should be ~0xFF
        assert_eq!(target[0], 0x00);
        assert_eq!(target[1], 0x00);
        assert_eq!(target[2], 0x00);
        assert_eq!(target[3], 0x00);
        assert_eq!(target[4], 0x00);
        assert_eq!(target[5], 0x00);
        assert!(
            target[6] >= 0xFE,
            "target[6] = 0x{:02X}, expected ~0xFF",
            target[6]
        );
    }

    #[test]
    fn test_fractional_difficulty_is_not_truncated() {
        let diff_1 = difficulty_to_target(1.0);
        let diff_1_5 = difficulty_to_target(1.5);
        let diff_2 = difficulty_to_target(2.0);

        assert_ne!(diff_1_5, diff_1);
        assert!(target_less_than(&diff_1_5, &diff_1));
        assert!(target_less_than(&diff_2, &diff_1_5));
        assert_eq!(difficulty_to_target(0.5), PDIFF1_TARGET);
    }

    fn target_less_than(lhs: &[u8; 32], rhs: &[u8; 32]) -> bool {
        for i in 0..32 {
            if lhs[i] < rhs[i] {
                return true;
            }
            if lhs[i] > rhs[i] {
                return false;
            }
        }
        false
    }

    #[test]
    fn test_difficulty_roundtrip() {
        // Convert difficulty -> target -> difficulty should be approximately round-trip
        for &diff in &[1.0, 16.0, 256.0, 4096.0, 65536.0, 1_000_000.0] {
            let target = difficulty_to_target(diff);
            let mut target_arr = [0u8; 32];
            target_arr.copy_from_slice(&target);
            let recovered = hash_to_difficulty(&target_arr);
            let ratio = recovered / diff;
            assert!(
                (0.9..1.1).contains(&ratio),
                "diff={}, recovered={}, ratio={}",
                diff,
                recovered,
                ratio
            );
        }
    }

    #[test]
    fn test_meets_target() {
        let target = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        // Hash below target -> meets
        let hash_below = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0xAA, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        assert!(meets_target(&hash_below, &target));

        // Hash equal to target -> meets
        assert!(meets_target(&target, &target));

        // Hash above target -> does not meet
        let hash_above = [
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        assert!(!meets_target(&hash_above, &target));
    }

    #[test]
    fn test_nbits_to_target() {
        // Bitcoin genesis block nbits = 0x1d00ffff
        let target = nbits_to_target(0x1d00ffff);
        // exp = 0x1d = 29, mantissa = 0x00ffff
        // target = 0x00ffff * 2^(8*(29-3)) = 0x00ffff * 2^208
        // In big-endian bytes: byte (32-29) = byte 3 has MSB of mantissa
        assert_eq!(target[3], 0x00);
        assert_eq!(target[4], 0xFF);
        assert_eq!(target[5], 0xFF);
        // Everything after should be 0 (mantissa is only 3 bytes)
        assert_eq!(target[6], 0x00);
    }

    #[test]
    fn test_nbits_modern_block() {
        // A modern-era nbits like 0x170b3ce9
        let target = nbits_to_target(0x170b3ce9);
        // exp = 0x17 = 23, mantissa = 0x0b3ce9
        // byte position: 32 - 23 = 9
        assert_eq!(target[9], 0x0b);
        assert_eq!(target[10], 0x3c);
        assert_eq!(target[11], 0xe9);
    }

    #[test]
    fn test_difficulty_to_ticket_mask() {
        assert_eq!(difficulty_to_ticket_mask(1), 1); // diff 1 -> mask 1
        assert_eq!(difficulty_to_ticket_mask(128), 255); // diff 128 -> mask 0xFF
        assert_eq!(difficulty_to_ticket_mask(256), 511); // diff 256 -> mask 0x1FF
    }

    // -----------------------------------------------------------------------
    // Fail-closed edge-case contracts.
    //
    // These tests pin the explicit defensive branches in `difficulty_to_target`
    // and `nbits_to_target`. Each branch already exists in the implementation
    // but had no test, so a future refactor that changed the fall-through
    // behavior could silently break the share-acceptance gate without
    // tripping any existing test.
    // -----------------------------------------------------------------------

    #[test]
    fn difficulty_to_target_zero_returns_accept_all() {
        // Defensive: a malformed pool message reaching difficulty_to_target
        // with diff=0 must produce the all-FF target so the dispatcher
        // doesn't divide-by-zero. "Accept all" is intentionally the safe
        // direction here — the V1 client's `parse_set_difficulty` already
        // rejects zero at the wire layer (), so this is the inner
        // belt-and-suspenders contract.
        assert_eq!(difficulty_to_target(0.0), [0xFF; 32]);
    }

    #[test]
    fn difficulty_to_target_negative_returns_accept_all() {
        assert_eq!(difficulty_to_target(-1.0), [0xFF; 32]);
        assert_eq!(difficulty_to_target(f64::MIN), [0xFF; 32]);
    }

    #[test]
    fn difficulty_to_target_nan_fails_closed_to_zero_target() {
        // NaN must produce all-zero target so NO share can satisfy it
        // (`hash <= target` with target=[0; 32] is false for any non-zero
        // hash). Fail-closed for malformed pool input — never silently
        // accept everything when the difficulty is undefined.
        assert_eq!(difficulty_to_target(f64::NAN), [0u8; 32]);
    }

    #[test]
    fn difficulty_to_target_infinity_fails_closed_to_zero_target() {
        // BOTH infinities fail closed (all-zero target → no share can satisfy
        // it). NEG_INFINITY is numerically <= 0.0, so finiteness MUST be checked
        // before the <= 0.0 accept-all sentinel — otherwise -inf would fail OPEN
        // (accept everything). Pin both directions fail-closed so the ordering
        // can never silently regress.
        assert_eq!(difficulty_to_target(f64::INFINITY), [0u8; 32]);
        assert_eq!(difficulty_to_target(f64::NEG_INFINITY), [0u8; 32]);
    }

    #[test]
    fn difficulty_to_target_below_one_clamps_to_pdiff_1() {
        // Pool difficulty below 1 is functionally "easiest possible" — the
        // implementation clamps to PDIFF1_TARGET so a buggy 0.001 doesn't
        // silently produce a target larger than pdiff_1.
        assert_eq!(difficulty_to_target(0.001), PDIFF1_TARGET);
        assert_eq!(difficulty_to_target(0.999), PDIFF1_TARGET);
    }

    #[test]
    fn difficulty_to_target_huge_value_uses_fractional_path() {
        // Beyond u64::MAX, the integer division path overflows so the
        // implementation falls through to the fractional path. The result
        // must still be a non-zero target (otherwise share acceptance
        // would silently fail-closed at unreachable difficulties).
        let huge = u64::MAX as f64 * 2.0;
        let target = difficulty_to_target(huge);
        assert_ne!(
            target, [0u8; 32],
            "huge difficulty must not produce zero target"
        );
        assert_ne!(
            target, PDIFF1_TARGET,
            "huge difficulty must be tighter than pdiff_1"
        );
    }

    #[test]
    fn nbits_to_target_zero_mantissa_returns_zero_target() {
        // Per Bitcoin spec, an nbits with mantissa=0 yields a zero target.
        // No share can satisfy it. Pin this explicitly.
        let target = nbits_to_target(0x1d000000);
        assert_eq!(target, [0u8; 32]);
    }

    #[test]
    fn nbits_to_target_negative_flag_returns_zero_target() {
        // The 24th bit of the mantissa field is the negative flag in
        // Bitcoin's compact format. A negative target is invalid for
        // mining; the implementation must return zeros so no share can
        // ever match.
        let target = nbits_to_target(0x1d800000);
        assert_eq!(target, [0u8; 32], "negative nbits must produce zero target");
    }

    #[test]
    fn nbits_to_target_oversize_exponent_returns_zero_target() {
        // Per impl: `if exp > 32 { return target; }` (zeros). Pool sending
        // an exponent of 0xFF must NOT produce a giant accept-all target.
        let target = nbits_to_target(0xff00ffff);
        assert_eq!(target, [0u8; 32], "exp > 32 must produce zero target");
    }

    #[test]
    fn nbits_to_target_zero_exponent_returns_zero_target() {
        // exp=0 with non-zero mantissa: the implementation returns zeros
        // (mantissa is conceptually below the byte 31 boundary).
        let target = nbits_to_target(0x0000ffff);
        assert_eq!(target, [0u8; 32]);
    }

    #[test]
    fn hash_to_difficulty_zero_hash_returns_infinity() {
        // A zero hash means the share has infinite difficulty. The
        // implementation must surface that explicitly (not return 0.0
        // which would imply zero work).
        assert!(hash_to_difficulty(&[0u8; 32]).is_infinite());
    }

    #[test]
    fn hash_to_difficulty_one_hash_returns_high_difficulty() {
        // Hash with only the lowest byte set is enormously high-difficulty
        // (close to 2^248 worth of work). Must produce a finite huge value,
        // not infinity (would conflict with the zero-hash sentinel).
        let mut hash = [0u8; 32];
        hash[31] = 1;
        let diff = hash_to_difficulty(&hash);
        assert!(diff.is_finite(), "single-byte hash must yield finite diff");
        assert!(
            diff > 1e60,
            "single-byte hash diff = {} (expected > 1e60)",
            diff
        );
    }

    #[test]
    fn hash_to_difficulty_max_hash_returns_low_difficulty() {
        // All-FF hash is the easiest possible — difficulty must be very small.
        let diff = hash_to_difficulty(&[0xFF; 32]);
        assert!(diff.is_finite(), "all-FF hash must yield finite diff");
        assert!(diff < 1.0, "all-FF hash diff = {} (expected < 1.0)", diff);
    }
}
