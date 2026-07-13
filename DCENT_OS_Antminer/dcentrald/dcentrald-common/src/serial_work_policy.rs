//! Shared serial-mining work-slot / share-dedup policy (ADR-0009 strangler).
//!
//! Hybrid, serial_mining, and am3_bb historically copied magic numbers
//! (history depth 32, job-id step 8, seen-share cap 8192). Keep **one** pure
//! definition here; engines should import these constants rather than
//! re-declaring them.
//!
//! Status: constants + pure helpers only. Migrating hybrid/serial loops to
//! call these is behavior-preserving (same numbers).

/// Default work history ring depth per ASIC job-id (hybrid serial path).
pub const DEFAULT_WORK_HISTORY_PER_ID: usize = 32;

/// BM1398-class paths sometimes keep a deeper history (serial_mining).
pub const BM1398_WORK_HISTORY_PER_ID: usize = 96;

/// AM3-BB local history depth (historical port value).
pub const AM3_BB_WORK_HISTORY_PER_ID: usize = 128;

/// ASIC job-id stride used on AM2 serial dispatch (skips midstate slots).
pub const DEFAULT_SERIAL_JOB_ID_STEP: u8 = 8;

/// Clear seen-share set when it exceeds this size (hybrid path).
pub const DEFAULT_SEEN_SHARES_CAP: usize = 8192;

/// Serial BM1362-class nonce frame length (bytes).
pub const BM1362_SERIAL_NONCE_LEN: usize = 11;

/// Advance ASIC job id with wrapping add (same as hybrid/serial today).
pub fn next_asic_job_id(current: u8, step: u8) -> u8 {
    current.wrapping_add(step)
}

/// Canonical share-dedup key used on serial AM2-class paths.
pub fn serial_share_dedup_key(asic_job_id: u8, nonce: u32, version_bits: u16) -> (u8, u32, u16) {
    (asic_job_id, nonce, version_bits)
}

/// Whether the seen-share set should be cleared to bound memory.
pub fn should_clear_seen_shares(current_len: usize, cap: usize) -> bool {
    current_len > cap
}

/// Select work-history depth for a chip family id when known.
pub fn work_history_depth_for_chip_id(chip_id: u16) -> usize {
    match chip_id {
        0x1398 => BM1398_WORK_HISTORY_PER_ID,
        _ => DEFAULT_WORK_HISTORY_PER_ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_step_matches_hybrid_constant() {
        assert_eq!(DEFAULT_SERIAL_JOB_ID_STEP, 8);
        assert_eq!(next_asic_job_id(0xF8, 8), 0x00);
        assert_eq!(next_asic_job_id(0, 8), 8);
    }

    #[test]
    fn seen_share_cap_clear_boundary() {
        assert!(!should_clear_seen_shares(8192, DEFAULT_SEEN_SHARES_CAP));
        assert!(should_clear_seen_shares(8193, DEFAULT_SEEN_SHARES_CAP));
    }

    #[test]
    fn history_depths_are_stable() {
        assert_eq!(work_history_depth_for_chip_id(0x1362), 32);
        assert_eq!(work_history_depth_for_chip_id(0x1398), 96);
        assert_eq!(DEFAULT_WORK_HISTORY_PER_ID, 32);
    }

    #[test]
    fn dedup_key_is_tuple_identity() {
        assert_eq!(
            serial_share_dedup_key(8, 0xdead_beef, 0x1ff),
            (8, 0xdead_beef, 0x1ff)
        );
    }
}
