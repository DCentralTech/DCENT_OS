//! Stratum V2 difficulty target tracking.
//!
//! SV2 pools express share difficulty as a 256-bit target in channel-open and
//! `SetTarget` messages. The rest of dcentrald uses floating-point pool
//! difficulty for status, share metadata, and autotuner expectations, so this
//! module keeps that conversion and state update behavior centralized.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Sv2DifficultyError {
    #[error("SetTarget payload too short")]
    PayloadTooShort,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sv2DifficultyUpdate {
    pub channel_id: u32,
    pub share_target: [u8; 32],
    pub approx_difficulty: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sv2DifficultyState {
    channel_id: Option<u32>,
    share_target: [u8; 32],
    approx_difficulty: f64,
}

impl Default for Sv2DifficultyState {
    fn default() -> Self {
        Self {
            channel_id: None,
            share_target: [0xff; 32],
            approx_difficulty: 1.0,
        }
    }
}

impl Sv2DifficultyState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn channel_id(&self) -> Option<u32> {
        self.channel_id
    }

    pub fn share_target(&self) -> [u8; 32] {
        self.share_target
    }

    pub fn approx_difficulty(&self) -> f64 {
        self.approx_difficulty
    }

    pub fn apply_target(&mut self, channel_id: u32, share_target: [u8; 32]) -> Sv2DifficultyUpdate {
        let approx_difficulty = target_to_approximate_difficulty(&share_target);
        self.channel_id = Some(channel_id);
        self.share_target = share_target;
        self.approx_difficulty = approx_difficulty;

        Sv2DifficultyUpdate {
            channel_id,
            share_target,
            approx_difficulty,
        }
    }

    pub fn apply_set_target_payload(
        &mut self,
        payload: &[u8],
    ) -> Result<Sv2DifficultyUpdate, Sv2DifficultyError> {
        if payload.len() < 36 {
            return Err(Sv2DifficultyError::PayloadTooShort);
        }

        let channel_id = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let mut share_target = [0u8; 32];
        share_target.copy_from_slice(&payload[4..36]);
        Ok(self.apply_target(channel_id, share_target))
    }
}

/// Compute the per-share average pool target difficulty from a SubmitSharesSuccess
/// batch payload's `new_shares_sum` and `new_submits_accepted_count`.
///
/// SV2 acknowledges shares in batches that may straddle a `SetTarget`
/// difficulty change, so the most accurate per-share value is the credited
/// `sum / count`. When `count == 0` (genuine zero-credit ack or truncated
/// 8-byte legacy payload) or `sum == 0`, fall back to `current_pool_difficulty`
/// — the latest channel-level target — instead of dividing by zero or
/// reporting an artificial 0.0 difficulty.
pub fn batch_average_share_difficulty(
    new_shares_sum: u64,
    new_submits_accepted_count: u32,
    current_pool_difficulty: f64,
) -> f64 {
    if new_submits_accepted_count == 0 || new_shares_sum == 0 {
        return current_pool_difficulty;
    }
    new_shares_sum as f64 / f64::from(new_submits_accepted_count)
}

pub fn target_to_approximate_difficulty(target: &[u8; 32]) -> f64 {
    if target.iter().all(|byte| *byte == 0) {
        return f64::INFINITY;
    }

    let difficulty = crate::v1::difficulty::hash_to_difficulty(target);
    if difficulty.is_finite() {
        difficulty.max(1.0)
    } else {
        difficulty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::difficulty::difficulty_to_target;

    #[test]
    fn target_to_difficulty_tracks_pool_targets() {
        let diff_1 = target_to_approximate_difficulty(&difficulty_to_target(1.0));
        let diff_256 = target_to_approximate_difficulty(&difficulty_to_target(256.0));
        let diff_65k = target_to_approximate_difficulty(&difficulty_to_target(65_536.0));

        assert!((diff_1 - 1.0).abs() < 0.01, "diff_1={diff_1}");
        assert!((diff_256 - 256.0).abs() < 2.0, "diff_256={diff_256}");
        assert!(diff_65k > diff_256, "diff_65k={diff_65k}");
    }

    #[test]
    fn state_applies_mock_set_target_changes() {
        let mut state = Sv2DifficultyState::new();
        assert_eq!(state.approx_difficulty(), 1.0);

        let first = state.apply_target(7, difficulty_to_target(512.0));
        assert_eq!(first.channel_id, 7);
        assert_eq!(state.channel_id(), Some(7));
        assert_eq!(state.share_target(), first.share_target);
        assert!(first.approx_difficulty > 500.0);

        let mut payload = Vec::new();
        payload.extend_from_slice(&7u32.to_le_bytes());
        payload.extend_from_slice(&difficulty_to_target(4096.0));
        let second = state.apply_set_target_payload(&payload).unwrap();

        assert_eq!(second.channel_id, 7);
        assert!(second.approx_difficulty > first.approx_difficulty);
        assert_eq!(state.approx_difficulty(), second.approx_difficulty);
    }

    #[test]
    fn malformed_set_target_payload_does_not_update_state() {
        let mut state = Sv2DifficultyState::new();
        let before = state.clone();

        assert_eq!(
            state.apply_set_target_payload(&[0u8; 12]).unwrap_err(),
            Sv2DifficultyError::PayloadTooShort
        );
        assert_eq!(state, before);
    }

    #[test]
    fn zero_target_reports_infinite_difficulty() {
        assert!(target_to_approximate_difficulty(&[0u8; 32]).is_infinite());
    }

    #[test]
    fn batch_average_returns_sum_over_count_when_both_nonzero() {
        let avg = batch_average_share_difficulty(5_000, 5, 999.0);
        assert!((avg - 1_000.0).abs() < 1e-9, "avg={avg}");
    }

    #[test]
    fn batch_average_falls_back_when_count_is_zero() {
        let fallback = 1234.5;
        // Truncated 8-byte legacy payload sets count=0 and sum=0 in the parser.
        assert_eq!(
            batch_average_share_difficulty(0, 0, fallback),
            fallback,
            "must fall back when count is zero"
        );
    }

    #[test]
    fn batch_average_falls_back_when_sum_is_zero_but_count_is_not() {
        // A pool reporting count=N but sum=0 is degenerate — prefer the
        // current channel target over claiming each share was difficulty 0.
        let fallback = 42.0;
        assert_eq!(batch_average_share_difficulty(0, 3, fallback), fallback);
    }

    #[test]
    fn batch_average_does_not_divide_by_zero_for_count_zero_with_sum_present() {
        // Defensive: even if a buggy pool sends sum>0 with count==0 we must
        // not divide by zero or panic.
        let fallback = 5.0;
        assert_eq!(
            batch_average_share_difficulty(99_999, 0, fallback),
            fallback
        );
    }

    #[test]
    fn batch_average_handles_single_share_batch() {
        // count=1, sum=8192 → avg = 8192. This is the typical legacy V1-shaped
        // case where the SV2 batch ack covers exactly one share.
        let avg = batch_average_share_difficulty(8192, 1, 1.0);
        assert!((avg - 8192.0).abs() < 1e-9, "avg={avg}");
    }

    #[test]
    fn sv2_difficulty_helpers_never_panic_on_arbitrary_adversarial_input() {
        // A malicious / buggy SV2 pool can send ANY target bytes and ANY
        // accepted-count/share-sum ack. Neither difficulty helper may panic, divide
        // by zero, or produce a NaN a downstream caller can't handle. Deterministic
        // LCG (no RNG dependency) over 4000 cases.
        let mut lcg: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            lcg
        };
        for _ in 0..4000 {
            let sum = next();
            let count = (next() % 300) as u32;
            // Arbitrary fallback bits, including NaN / +-inf / subnormals.
            let fallback = f64::from_bits(next());
            let d = batch_average_share_difficulty(sum, count, fallback);
            // When it actually divides (both non-zero) the result must be finite;
            // otherwise it returns the (arbitrary) fallback unchanged.
            if count != 0 && sum != 0 {
                assert!(
                    d.is_finite(),
                    "sum/count must be finite for sum={sum} count={count}, got {d}"
                );
            }

            let mut target = [0u8; 32];
            for b in target.iter_mut() {
                *b = (next() & 0xff) as u8;
            }
            let ad = target_to_approximate_difficulty(&target);
            // Never NaN: all-zero target -> +inf, else the fail-closed
            // hash_to_difficulty output floored at 1.0 (or its clamped non-finite).
            assert!(
                !ad.is_nan(),
                "approx difficulty must never be NaN for a target"
            );
            if !ad.is_infinite() {
                assert!(
                    ad >= 1.0,
                    "finite approx difficulty must be >= 1.0, got {ad}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Sv2DifficultyState lifecycle + accessor contracts.
    //
    // The state struct holds the channel-id/target/difficulty triple that
    // the SV2 client uses to feed the autotuner. Pin the default-state
    // and accessor invariants so a refactor cannot silently flip the
    // initial difficulty (which is fed into the autotuner the moment a
    // channel opens).
    // -----------------------------------------------------------------------

    #[test]
    fn sv2_difficulty_state_default_is_accept_all_target() {
        // Default is the "accept all" target [0xFF; 32] with
        // approx_difficulty = 1.0 and no channel bound. This must never
        // silently flip to a tighter target — the SV2 client uses default
        // state until the pool sends SetTarget.
        let state = Sv2DifficultyState::default();
        assert!(state.channel_id().is_none());
        assert_eq!(state.share_target(), [0xFFu8; 32]);
        assert!((state.approx_difficulty() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sv2_difficulty_state_new_matches_default() {
        // ::new() is documented as a Default::default() shortcut. Pin so
        // a refactor that diverged the two would be caught.
        let new_state = Sv2DifficultyState::new();
        let default_state = Sv2DifficultyState::default();
        assert_eq!(new_state, default_state);
    }

    #[test]
    fn sv2_difficulty_state_apply_target_returns_update_with_same_values_as_state() {
        // apply_target must return an Sv2DifficultyUpdate whose fields
        // match the state's new fields. Pin so a refactor can't return
        // pre-update values while writing post-update state (or vice versa).
        let mut state = Sv2DifficultyState::new();
        let target = difficulty_to_target(2048.0);
        let update = state.apply_target(7, target);

        assert_eq!(update.channel_id, 7);
        assert_eq!(update.share_target, target);
        assert_eq!(state.channel_id(), Some(7));
        assert_eq!(state.share_target(), target);
        assert!((update.approx_difficulty - state.approx_difficulty()).abs() < f64::EPSILON);
    }

    #[test]
    fn sv2_difficulty_state_apply_set_target_with_extra_bytes_ignores_trailing() {
        // `apply_set_target_payload` reads exactly 36 bytes (4 channel_id
        // + 32 target). Any trailing bytes must be silently ignored
        // (some SV2 implementations append metadata).
        let mut state = Sv2DifficultyState::new();
        let target = difficulty_to_target(512.0);

        let mut payload = Vec::new();
        payload.extend_from_slice(&5u32.to_le_bytes());
        payload.extend_from_slice(&target);
        payload.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // trailing garbage

        let update = state.apply_set_target_payload(&payload).unwrap();
        assert_eq!(update.channel_id, 5);
        assert_eq!(update.share_target, target);
    }

    #[test]
    fn sv2_difficulty_state_apply_set_target_at_exactly_36_bytes_is_minimum() {
        // 36 bytes is the exact spec minimum. Pin the boundary so a
        // refactor that flipped to 37 (off-by-one) is caught.
        let mut state = Sv2DifficultyState::new();
        let mut payload = vec![0u8; 36];
        payload[..4].copy_from_slice(&3u32.to_le_bytes());
        // share_target stays as the [0u8; 32] block from the buffer init.

        let result = state.apply_set_target_payload(&payload);
        assert!(result.is_ok());

        // 35 bytes must fail.
        let result = state.apply_set_target_payload(&[0u8; 35]);
        assert!(matches!(result, Err(Sv2DifficultyError::PayloadTooShort)));
    }

    #[test]
    fn sv2_difficulty_state_apply_target_then_reset_to_default() {
        // Verify state can be replaced with default after use (lifecycle pin).
        let mut state = Sv2DifficultyState::new();
        state.apply_target(99, [0x00; 32]); // infinite difficulty target
        assert_eq!(state.channel_id(), Some(99));

        // Replacing with default restores accept-all state.
        state = Sv2DifficultyState::default();
        assert!(state.channel_id().is_none());
        assert_eq!(state.share_target(), [0xFFu8; 32]);
    }

    #[test]
    fn sv2_difficulty_state_apply_target_zero_target_yields_infinity() {
        // Zero target = infinite difficulty (no share can match). State
        // must record this. Pin so `target_to_approximate_difficulty`'s
        // infinity branch is reachable through the state struct API.
        let mut state = Sv2DifficultyState::new();
        let update = state.apply_target(1, [0u8; 32]);
        assert!(update.approx_difficulty.is_infinite());
        assert!(state.approx_difficulty().is_infinite());
    }

    #[test]
    fn sv2_difficulty_error_display_message_is_actionable() {
        // The Display impl is what operators read in logs. Pin the
        // message text so a refactor doesn't strip the diagnostic.
        let err = Sv2DifficultyError::PayloadTooShort;
        let s = err.to_string();
        assert!(s.to_lowercase().contains("too short"), "got: {s}");
    }

    #[test]
    fn sv2_difficulty_update_round_trips_clone_eq_via_apply_target() {
        // Sv2DifficultyUpdate has Clone+PartialEq derives. Pin that
        // applying the same target twice produces equal Update structs
        // (deterministic state transitions).
        let mut state_a = Sv2DifficultyState::new();
        let mut state_b = Sv2DifficultyState::new();
        let target = difficulty_to_target(1024.0);

        let update_a = state_a.apply_target(42, target);
        let update_b = state_b.apply_target(42, target);
        assert_eq!(update_a, update_b);
    }
}
