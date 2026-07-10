//! Status-code policy table test for `/pair` (spec §2.5).
//!
//! Drives the `PairError` mapping and the `is_retryable` retry decision for
//! every documented status code, asserting:
//!   - 400 / 401 / 403 fast-fail (no retry),
//!   - 503 / 5xx / transport / 409-replay are retryable,
//!   - 409-replay maps to `PairError::Replay` (the ts-refresh trigger).

use dcentrald_bridge::PairError;

#[test]
fn policy_table_retry_decisions() {
    // (label, error, expected_retryable)
    let cases: Vec<(&str, PairError, bool)> = vec![
        (
            "400 bad request",
            PairError::BadRequest("missing device_id".into()),
            false,
        ),
        (
            "401 auth failed",
            PairError::AuthFailed("hmac verification failed".into()),
            false,
        ),
        ("403 enrollment locked", PairError::EnrollmentLocked, false),
        ("503 time not synced", PairError::TimeNotSynced, true),
        ("409 replay", PairError::Replay, true),
        (
            "500 internal",
            PairError::Http {
                status: 500,
                body: "boom".into(),
            },
            true,
        ),
        (
            "502 bad gateway",
            PairError::Http {
                status: 502,
                body: "".into(),
            },
            true,
        ),
        (
            "418 teapot (4xx non-policy)",
            PairError::Http {
                status: 418,
                body: "".into(),
            },
            false,
        ),
        (
            "transport",
            PairError::Transport("connection refused".into()),
            true,
        ),
    ];

    for (label, err, want) in cases {
        assert_eq!(
            err.is_retryable(),
            want,
            "retry decision mismatch for `{label}`"
        );
    }
}

#[test]
fn replay_is_distinct_error_variant() {
    // 409-replay must be its own variant so the retry loop can refresh ts and
    // re-sign rather than treating it as a hard fail.
    let e = PairError::Replay;
    assert!(matches!(e, PairError::Replay));
    assert!(e.is_retryable());
}

#[test]
fn fast_fail_variants_do_not_retry() {
    assert!(!PairError::BadRequest("x".into()).is_retryable());
    assert!(!PairError::AuthFailed("x".into()).is_retryable());
    assert!(!PairError::EnrollmentLocked.is_retryable());
}

#[test]
fn server_errors_retry_only_5xx() {
    assert!(PairError::Http {
        status: 500,
        body: String::new()
    }
    .is_retryable());
    assert!(PairError::Http {
        status: 599,
        body: String::new()
    }
    .is_retryable());
    assert!(!PairError::Http {
        status: 404,
        body: String::new()
    }
    .is_retryable());
    assert!(!PairError::Http {
        status: 409,
        body: String::new()
    }
    .is_retryable()); // non-replay 409
}
