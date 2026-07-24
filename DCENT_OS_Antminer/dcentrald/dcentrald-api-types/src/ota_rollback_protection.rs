//!  sec-A — OTA rollback-protection policy (HAL-free).
//!
//! Source RE evidence: `dcentrald-api::ota_signature::version_is_newer`
//! (already implemented but un-wired) and the LuxOS rollback-protection
//! pattern from .
//!
//! When an operator (or fleet manager) uploads a sysupgrade `.tar`, we
//! must decide:
//! 1. Is the signature valid? (handled in `dcentrald-api::ota_signature`)
//! 2. Is the candidate version a valid forward step from the current
//!    running version? **(this module)**
//!
//! Without rollback protection, an attacker who steals an old signing key
//! could downgrade the miner to a known-vulnerable firmware version,
//! sidestepping a security patch we've already shipped. Even without an
//! attacker, an operator running `dcent install` with a stale image risks
//! reverting bug fixes.
//!
//! The policy is conservative:
//! - **Newer version** → ALLOW (normal path)
//! - **Same version** → ALLOW (re-flash for recovery)
//! - **Older version, no override** → DENY
//! - **Older version, `allow_downgrade=true`** → ALLOW with WARN telemetry
//!
//! `allow_downgrade` is plumbed from the operator-supplied
//! `--allow-downgrade` flag in `dcent install` / dashboard. It is not
//! sticky — every downgrade requires explicit operator opt-in.
//!
//! This module is **pure logic, no filesystem**. It returns a verdict
//! that the runtime adapter inside `dcentrald-api::sysupgrade` consumes.

use serde::{Deserialize, Serialize};

/// Outcome of the rollback-protection check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum RollbackVerdict {
    /// Candidate is newer than current — proceed with sysupgrade.
    AllowForward,
    /// Candidate equals current — re-flash for recovery.
    AllowReinstall,
    /// Candidate is older than current and `allow_downgrade=true`.
    AllowDowngrade { reason: String },
    /// Candidate is older than current and no override.
    DenyOlderVersion { candidate: String, current: String },
    /// One of the version strings was unparseable. Fail-closed.
    DenyMalformedVersion { problem: String },
}

impl RollbackVerdict {
    /// Returns true if this verdict permits the sysupgrade to proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            RollbackVerdict::AllowForward
                | RollbackVerdict::AllowReinstall
                | RollbackVerdict::AllowDowngrade { .. }
        )
    }

    /// Returns true if this verdict requires emitting a WARN-level
    /// telemetry event before proceeding.
    pub fn needs_warning(&self) -> bool {
        matches!(self, RollbackVerdict::AllowDowngrade { .. })
    }
}

/// A version admitted by the bounded `DCENT_VERSION/1` grammar.
///
/// Components remain canonical strings instead of fixed-width integers so
/// rollback ordering cannot overflow or lose precision. Build metadata is
/// validated by [`parse_version`] but is intentionally omitted because it
/// never affects precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    /// Two or three canonical decimal core components.
    pub release: Vec<String>,
    /// Dot-separated prerelease identifiers; empty for a final release.
    pub prerelease: Vec<String>,
}

const MAX_VERSION_BYTES: usize = 128;
const MAX_VERSION_PARTS: usize = 16;
const MAX_VERSION_PART_BYTES: usize = 32;

fn is_canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn parse_identifiers(value: &str, reject_numeric_leading_zeroes: bool) -> Option<Vec<String>> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts.is_empty() || parts.len() > MAX_VERSION_PARTS {
        return None;
    }
    for part in &parts {
        if part.is_empty()
            || part.len() > MAX_VERSION_PART_BYTES
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return None;
        }
        if reject_numeric_leading_zeroes
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part.len() > 1
            && part.starts_with('0')
        {
            return None;
        }
    }
    Some(parts.into_iter().map(str::to_owned).collect())
}

/// Parse the bounded `DCENT_VERSION/1` language.
///
/// The accepted form is `v?MAJOR.MINOR[.PATCH][-PRERELEASE][+BUILD]`.
/// There is no whitespace normalization or best-effort recovery: any
/// non-canonical component fails closed.
pub fn parse_version(version: &str) -> Option<ParsedVersion> {
    if version.is_empty() || version.len() > MAX_VERSION_BYTES || !version.is_ascii() {
        return None;
    }

    let unprefixed = version.strip_prefix('v').unwrap_or(version);
    if unprefixed.is_empty() {
        return None;
    }

    let (precedence, build) = match unprefixed.split_once('+') {
        Some((precedence, build)) => (precedence, Some(build)),
        None => (unprefixed, None),
    };
    if let Some(build) = build {
        parse_identifiers(build, false)?;
    }

    let (release_part, prerelease) = match precedence.split_once('-') {
        Some((release, prerelease)) => (release, parse_identifiers(prerelease, true)?),
        None => (precedence, Vec::new()),
    };
    let release_parts: Vec<&str> = release_part.split('.').collect();
    if !(2..=3).contains(&release_parts.len())
        || release_parts.iter().any(|part| !is_canonical_decimal(part))
    {
        return None;
    }
    let release = release_parts.into_iter().map(str::to_owned).collect();

    Some(ParsedVersion {
        release,
        prerelease,
    })
}

fn cmp_digits(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn cmp_release(a: &[String], b: &[String]) -> std::cmp::Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let av = a.get(i).map(String::as_str).unwrap_or("0");
        let bv = b.get(i).map(String::as_str).unwrap_or("0");
        match cmp_digits(av, bv) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn cmp_prerelease(a: &[String], b: &[String]) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    for (av, bv) in a.iter().zip(b) {
        let a_numeric = av.bytes().all(|byte| byte.is_ascii_digit());
        let b_numeric = bv.bytes().all(|byte| byte.is_ascii_digit());
        let order = match (a_numeric, b_numeric) {
            (true, true) => cmp_digits(av, bv),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => av.cmp(bv),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// Compare two version strings. Returns:
/// - `Ordering::Greater` if `candidate > current`
/// - `Ordering::Equal` if `candidate == current`
/// - `Ordering::Less` if `candidate < current`
/// - `None` if either is malformed.
pub fn compare_versions(candidate: &str, current: &str) -> Option<std::cmp::Ordering> {
    let a = parse_version(candidate)?;
    let b = parse_version(current)?;
    use std::cmp::Ordering;
    match cmp_release(&a.release, &b.release) {
        Ordering::Equal => Some(cmp_prerelease(&a.prerelease, &b.prerelease)),
        ord => Some(ord),
    }
}

/// Assess a candidate sysupgrade against the running version + operator policy.
///
/// `allow_downgrade` reflects the operator's explicit opt-in for this single
/// upgrade attempt. It is never persisted; every downgrade requires re-asserting.
pub fn assess_rollback(candidate: &str, current: &str, allow_downgrade: bool) -> RollbackVerdict {
    use std::cmp::Ordering;
    let order = match compare_versions(candidate, current) {
        Some(o) => o,
        None => {
            return RollbackVerdict::DenyMalformedVersion {
                problem: format!(
                    "could not parse version pair (candidate={:?}, current={:?})",
                    candidate, current
                ),
            }
        }
    };
    match (order, allow_downgrade) {
        (Ordering::Greater, _) => RollbackVerdict::AllowForward,
        (Ordering::Equal, _) => RollbackVerdict::AllowReinstall,
        (Ordering::Less, true) => RollbackVerdict::AllowDowngrade {
            reason: format!("operator opted into downgrade {} -> {}", current, candidate),
        },
        (Ordering::Less, false) => RollbackVerdict::DenyOlderVersion {
            candidate: candidate.to_string(),
            current: current.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn assess_rollback_never_allows_downgrade_or_malformed_without_optin() {
        // Safety property (priority 5: rollback-attack prevention). Over many
        // version pairs — valid + malformed — pin the load-bearing invariants:
        //  (A) with allow_downgrade=FALSE, is_allowed() is true ONLY when both
        //      versions parse AND candidate >= current — an OLDER or a MALFORMED
        //      candidate can NEVER slip a rollback past an operator who did not opt
        //      in (the entire point of the anti-downgrade guard);
        //  (B) a malformed pair ALWAYS yields DenyMalformedVersion (fail-closed),
        //      for BOTH allow_downgrade values;
        //  (C) is_allowed() always agrees with the verdict variant.
        let mut lcg: u64 = 0x1234_5678_9ABC_DEF1;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        let mkver = |n: u32| -> String {
            match n % 8 {
                0 => String::new(),    // malformed (empty)
                1 => "garbage".into(), // malformed
                2 => "v".into(),       // malformed (no digits)
                3 => format!("{}.{}", n % 5, n % 7),
                4 => format!("v{}.{}.{}", n % 4, n % 9, n % 3),
                5 => format!("{}.{}.{}-rc{}", n % 3, n % 5, n % 4, n % 2),
                6 => format!("{}", n % 20),
                _ => format!("{}.{}.{}", (n / 7) % 6, (n / 3) % 8, n % 10),
            }
        };
        for _ in 0..8000u32 {
            let cand = mkver(next());
            let cur = mkver(next());
            let cmp = compare_versions(&cand, &cur);
            for &allow in &[false, true] {
                let v = assess_rollback(&cand, &cur, allow);
                // (C) is_allowed() consistency with the variant.
                match &v {
                    RollbackVerdict::AllowForward
                    | RollbackVerdict::AllowReinstall
                    | RollbackVerdict::AllowDowngrade { .. } => assert!(v.is_allowed()),
                    RollbackVerdict::DenyOlderVersion { .. }
                    | RollbackVerdict::DenyMalformedVersion { .. } => assert!(!v.is_allowed()),
                }
                match cmp {
                    None => assert!(
                        matches!(v, RollbackVerdict::DenyMalformedVersion { .. }),
                        "malformed pair ({cand:?},{cur:?}) not fail-closed: {v:?}"
                    ),
                    Some(order) => {
                        if !allow && v.is_allowed() {
                            assert_ne!(
                                order,
                                Ordering::Less,
                                "downgrade {cand:?}->{cur:?} was ALLOWED without opt-in"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn parse_version_handles_v_prefix_and_rc_suffix() {
        let p1 = parse_version("0.6.0").unwrap();
        assert_eq!(p1.release, vec!["0", "6", "0"]);
        assert!(p1.prerelease.is_empty());

        let p2 = parse_version("v0.6.0").unwrap();
        assert_eq!(p2.release, vec!["0", "6", "0"]);
        assert!(p2.prerelease.is_empty());

        let p3 = parse_version("v0.6.0-rc1").unwrap();
        assert_eq!(p3.release, vec!["0", "6", "0"]);
        assert_eq!(p3.prerelease, vec!["rc1"]);
    }

    #[test]
    fn parse_version_rejects_garbage() {
        assert!(parse_version("").is_none());
        // "not-a-version" splits to ["not", "a-version"]; "not" has no digits.
        assert!(parse_version("not-a-version").is_none());
        assert!(parse_version("abc").is_none());
    }

    #[test]
    fn compare_versions_basic_ordering() {
        assert_eq!(compare_versions("0.6.0", "0.5.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("0.6.0", "0.6.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("0.5.0", "0.6.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Some(Ordering::Greater));
    }

    #[test]
    fn compare_versions_handles_unequal_lengths() {
        // 0.6.0 vs 0.6 should treat missing component as 0.
        assert_eq!(compare_versions("0.6.0", "0.6"), Some(Ordering::Equal));
        assert_eq!(compare_versions("0.6.1", "0.6"), Some(Ordering::Greater));
    }

    #[test]
    fn compares_core_numbers_as_digit_strings_without_precision_loss() {
        // Boundaries above IEEE-754's exact integer range and above u64::MAX
        // must remain admissible and exactly ordered.
        assert_eq!(
            compare_versions("9007199254740993.0", "9007199254740992.999"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("18446744073709551616.0", "18446744073709551615.999"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions(
                "99999999999999999999999999999999.1",
                "10000000000000000000000000000000.999",
            ),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions(
                "100000000000000000000000000000000.0",
                "99999999999999999999999999999999.999",
            ),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn build_metadata_is_validated_but_does_not_affect_precedence() {
        assert_eq!(
            compare_versions("1.2.3+build.001", "1.2.3+other.999"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            compare_versions("1.2.3-rc.2+first", "1.2.3-rc.2+second"),
            Some(Ordering::Equal)
        );
        assert!(parse_version("1.2.3+").is_none());
        assert!(parse_version("1.2.3+bad_identifier").is_none());
    }

    #[test]
    fn prerelease_ordering_matches_dcent_version_v1() {
        assert_eq!(
            compare_versions("1.2.3-rc1", "1.2.3-rc2"),
            Some(Ordering::Less)
        );
        // Compact suffixes are alphanumeric identifiers and compare lexically.
        assert_eq!(
            compare_versions("1.2.3-rc10", "1.2.3-rc2"),
            Some(Ordering::Less)
        );
        // Dot-separated numeric identifiers compare numerically.
        assert_eq!(
            compare_versions("1.2.3-rc.10", "1.2.3-rc.2"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("1.2.3-9", "1.2.3-alpha"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.2.3-alpha", "1.2.3-alpha.1"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("1.2.3", "1.2.3-alpha"),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn rejects_noncanonical_or_out_of_bounds_versions() {
        for malformed in [
            "1",
            "1.2.3.4",
            " 1.2.3",
            "1.2.3 ",
            "V1.2.3",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-01",
            "1.2.3-",
            "1.2.3-alpha..1",
            "1.2.3-alpha_beta",
            "1.2.3+build+again",
            "1.2.3-\u{00e9}",
        ] {
            assert!(
                parse_version(malformed).is_none(),
                "unexpectedly admitted {malformed:?}"
            );
        }

        let too_many_parts = format!("1.2-{}", vec!["a"; 17].join("."));
        assert!(parse_version(&too_many_parts).is_none());
        let oversized_identifier = format!("1.2-{}", "a".repeat(33));
        assert!(parse_version(&oversized_identifier).is_none());
        let oversized_version = format!("{}.0", "9".repeat(127));
        assert!(parse_version(&oversized_version).is_none());
    }

    #[test]
    fn assess_forward_upgrade_allowed() {
        let v = assess_rollback("0.6.0", "0.5.0", false);
        assert_eq!(v, RollbackVerdict::AllowForward);
        assert!(v.is_allowed());
        assert!(!v.needs_warning());
    }

    #[test]
    fn assess_reinstall_allowed() {
        let v = assess_rollback("0.6.0", "0.6.0", false);
        assert_eq!(v, RollbackVerdict::AllowReinstall);
        assert!(v.is_allowed());
        assert!(!v.needs_warning());
    }

    #[test]
    fn assess_downgrade_denied_without_override() {
        let v = assess_rollback("0.5.0", "0.6.0", false);
        assert_eq!(
            v,
            RollbackVerdict::DenyOlderVersion {
                candidate: "0.5.0".to_string(),
                current: "0.6.0".to_string(),
            }
        );
        assert!(!v.is_allowed());
        assert!(!v.needs_warning());
    }

    #[test]
    fn assess_downgrade_allowed_with_override_carries_warning() {
        let v = assess_rollback("0.5.0", "0.6.0", true);
        match &v {
            RollbackVerdict::AllowDowngrade { reason } => {
                assert!(reason.contains("0.6.0"));
                assert!(reason.contains("0.5.0"));
            }
            _ => panic!("expected AllowDowngrade, got {:?}", v),
        }
        assert!(v.is_allowed());
        assert!(v.needs_warning());
    }

    #[test]
    fn assess_malformed_version_fails_closed() {
        let v = assess_rollback("garbage", "0.6.0", true);
        match &v {
            RollbackVerdict::DenyMalformedVersion { problem } => {
                assert!(problem.contains("garbage"));
            }
            _ => panic!("expected DenyMalformedVersion, got {:?}", v),
        }
        // Even with allow_downgrade, malformed wins.
        assert!(!v.is_allowed());
    }

    #[test]
    fn assess_both_malformed_fails_closed() {
        let v = assess_rollback("garbage", "junk", true);
        assert!(matches!(v, RollbackVerdict::DenyMalformedVersion { .. }));
        assert!(!v.is_allowed());
    }

    #[test]
    fn allow_downgrade_does_not_apply_to_forward_or_equal() {
        // allow_downgrade is permission, not preference — must not change
        // the verdict for a forward upgrade or reinstall.
        let v_forward = assess_rollback("0.7.0", "0.6.0", true);
        assert_eq!(v_forward, RollbackVerdict::AllowForward);
        let v_equal = assess_rollback("0.6.0", "0.6.0", true);
        assert_eq!(v_equal, RollbackVerdict::AllowReinstall);
    }

    #[test]
    fn rollback_verdict_round_trips_through_serde_json() {
        let v = assess_rollback("0.5.0", "0.6.0", true);
        let json = serde_json::to_string(&v).unwrap();
        let back: RollbackVerdict = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn rc_versions_compare_correctly() {
        // Semver rule: pre-release < release of same triple.
        // 0.6.0-rc1 < 0.6.0  →  0.6.0 > 0.6.0-rc1
        assert_eq!(
            compare_versions("0.6.0", "0.6.0-rc1"),
            Some(Ordering::Greater)
        );
        // 0.6.0-rc2 > 0.6.0-rc1 (numeric tail breaks tie within pre-release).
        assert_eq!(
            compare_versions("0.6.0-rc2", "0.6.0-rc1"),
            Some(Ordering::Greater)
        );
        // 0.5.0 < 0.6.0-rc1 (release segment wins over prerelease tag).
        assert_eq!(compare_versions("0.5.0", "0.6.0-rc1"), Some(Ordering::Less));
    }
}
