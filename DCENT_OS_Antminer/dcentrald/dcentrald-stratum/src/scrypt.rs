//! Scrypt Stratum protocol scaffold for L7/L9-class miners.
//!
//! This module intentionally defines only the wire-contract differences
//! from the SHA-256d Stratum path. It does not validate shares, build work,
//! open sockets, or select pools. The goal for W28-prep is to reserve typed
//! surfaces so later L7/L9 work does not accidentally reuse SHA-256d
//! midstates, nonce assumptions, or difficulty conversion.

/// Scrypt jobs do not consume the SHA-256d midstate format used by Bitcoin
/// hashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScryptMidstateFormat {
    /// Full block header bytes covered by the scrypt proof-of-work function.
    pub header_bytes: usize,
    /// SHA-256d midstate words expected by this format. Must stay zero until
    /// an L7/L9-specific prehash contract is implemented.
    pub sha256d_midstate_words: usize,
    /// Human-readable warning for operators and future implementers.
    pub note: &'static str,
}

/// W28-prep placeholder for the scrypt work preimage contract.
pub const SCRYPT_MIDSTATE_FORMAT: ScryptMidstateFormat = ScryptMidstateFormat {
    header_bytes: 80,
    sha256d_midstate_words: 0,
    note: "scrypt uses the full 80-byte header preimage; do not pass SHA-256d midstates",
};

/// Scrypt ASIC work still has a 32-bit block-header nonce, but the L7/L9
/// dispatcher must keep extranonce and per-ASIC work-id handling separate from
/// the SHA-256d dispatch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScryptNonceRange {
    pub start: u32,
    pub end_inclusive: u32,
    pub width_bits: u8,
}

/// Full 32-bit nonce search space for the header nonce field.
pub const SCRYPT_NONCE_RANGE: ScryptNonceRange = ScryptNonceRange {
    start: u32::MIN,
    end_inclusive: u32::MAX,
    width_bits: 32,
};

/// Byte order used when comparing a scrypt hash against a Stratum target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScryptHashByteOrder {
    /// Network/display order, most-significant byte first.
    BigEndian,
}

/// Target comparison direction for scrypt proof-of-work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScryptTargetComparison {
    /// A share is valid when the scrypt hash is less than or equal to target.
    HashLessThanOrEqualTarget,
}

/// Scrypt difficulty conversion must be implemented separately from the
/// SHA-256d helper because altcoin pools may define difficulty-1 targets and
/// hash display order differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScryptDifficultyConversion {
    pub diff1_target_family: &'static str,
    pub hash_byte_order: ScryptHashByteOrder,
    pub comparison: ScryptTargetComparison,
    pub note: &'static str,
}

/// W28-prep placeholder for Litecoin/Dogecoin merged-mining style pools.
pub const SCRYPT_DIFFICULTY_CONVERSION: ScryptDifficultyConversion = ScryptDifficultyConversion {
    diff1_target_family: "scrypt-pool-difficulty-1",
    hash_byte_order: ScryptHashByteOrder::BigEndian,
    comparison: ScryptTargetComparison::HashLessThanOrEqualTarget,
    note: "do not call the SHA-256d difficulty_to_target helper for scrypt shares",
};

/// Placeholder share-check entry point. Future W28 work should replace this
/// with a real scrypt hash and target comparison path.
pub fn scrypt_share_check_stub() -> Result<(), &'static str> {
    Err("W28 scrypt not yet wired")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrypt_share_check_stub_returns_err() {
        assert_eq!(scrypt_share_check_stub(), Err("W28 scrypt not yet wired"));
    }

    #[test]
    fn scrypt_contract_does_not_claim_sha256d_midstate() {
        assert_eq!(SCRYPT_MIDSTATE_FORMAT.header_bytes, 80);
        assert_eq!(SCRYPT_MIDSTATE_FORMAT.sha256d_midstate_words, 0);
        assert_eq!(SCRYPT_NONCE_RANGE.width_bits, 32);
        assert_eq!(
            SCRYPT_DIFFICULTY_CONVERSION.comparison,
            ScryptTargetComparison::HashLessThanOrEqualTarget
        );
    }
}
