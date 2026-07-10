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

/// Parsed version: numeric components (`0.6.0` -> `[0,6,0]`) plus a
/// pre-release suffix tag count. A pre-release version compares LESS than
/// its release counterpart (`0.6.0-rc1 < 0.6.0`); within pre-releases the
/// numeric tail (`rc1` -> 1) breaks ties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVersion {
    /// Major.minor.patch... numeric segments parsed from before the first `-`.
    pub release: Vec<u32>,
    /// Pre-release suffix; empty Vec for a real release. The tuple is
    /// `(label_priority, numeric_tail)` so `rc1 < rc2` and any non-empty
    /// pre-release sorts BEFORE the release.
    pub prerelease: Vec<u32>,
}

/// Parse a semver-ish version string into a `ParsedVersion`.
/// Tolerates `v`-prefixed strings and pre-release suffixes (`v0.6.0-rc1`).
/// Returns `None` if no numeric component is parseable for the release.
pub fn parse_version(version: &str) -> Option<ParsedVersion> {
    let trimmed = version.trim().trim_start_matches('v');
    let mut iter = trimmed.splitn(2, '-');
    let release_part = iter.next()?;
    let prerelease_part = iter.next();

    let release: Vec<u32> = release_part
        .split('.')
        .filter_map(|p| p.parse::<u32>().ok())
        .collect();
    if release.is_empty() {
        return None;
    }

    let prerelease: Vec<u32> = match prerelease_part {
        None => Vec::new(),
        Some(s) => s
            .split(['.', '-'])
            .filter_map(|p| {
                // Extract leading non-digit label + trailing digits as
                // [label_chars_summed, numeric_tail]. Pure digits parse as
                // a single number. Pure non-digits give a label-priority value.
                if p.is_empty() {
                    return None;
                }
                if let Ok(n) = p.parse::<u32>() {
                    Some(n)
                } else {
                    // For "rc1", split into "rc" + "1": label sums codepoints
                    // (deterministic), then numeric tail.
                    let split_at = p.find(|c: char| c.is_ascii_digit()).unwrap_or(p.len());
                    let (label, tail) = p.split_at(split_at);
                    let label_score: u32 = label.bytes().map(|b| b as u32).sum();
                    let tail_n = tail.parse::<u32>().unwrap_or(0);
                    Some(label_score.saturating_mul(1_000_000).saturating_add(tail_n))
                }
            })
            .collect(),
    };

    Some(ParsedVersion {
        release,
        prerelease,
    })
}

fn cmp_padded(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let av = *a.get(i).unwrap_or(&0);
        let bv = *b.get(i).unwrap_or(&0);
        match av.cmp(&bv) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
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
    // Compare release segments first, padding the shorter with zeros so
    // "0.6.0" == "0.6".
    match cmp_padded(&a.release, &b.release) {
        Ordering::Equal => {
            // Pre-release rule: any pre-release < release (empty prerelease).
            match (a.prerelease.is_empty(), b.prerelease.is_empty()) {
                (true, true) => Some(Ordering::Equal),
                (true, false) => Some(Ordering::Greater),
                (false, true) => Some(Ordering::Less),
                (false, false) => Some(cmp_padded(&a.prerelease, &b.prerelease)),
            }
        }
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
        assert_eq!(p1.release, vec![0, 6, 0]);
        assert!(p1.prerelease.is_empty());

        let p2 = parse_version("v0.6.0").unwrap();
        assert_eq!(p2.release, vec![0, 6, 0]);
        assert!(p2.prerelease.is_empty());

        let p3 = parse_version("v0.6.0-rc1").unwrap();
        assert_eq!(p3.release, vec![0, 6, 0]);
        // Pre-release present; exact internal tag depends on the encoder.
        assert!(!p3.prerelease.is_empty());
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
