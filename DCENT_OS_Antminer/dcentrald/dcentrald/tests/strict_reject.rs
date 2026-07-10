//! Integration test for Wave C T8 strict-reject (2026-05-19).
//!
//! The strict-reject flip in `main.rs` is pinned at the source level by
//! the inline structural test `strict_reject_unknown_flags_exits_2_with_usage`
//! (necessary: the source MUST contain `process::exit(2)` + `cli_help_text`).
//!
//! This integration test is the SUFFICIENT half: it actually runs the
//! compiled `dcentrald` binary and asserts the production behavior is
//! what the structural test pins (a typo'd flag really does exit 2 +
//! print the usage). If the structural test passes but this one fails,
//! something in the strict-reject path is broken at runtime (e.g.,
//! `cli_help_text()` panics, or the `exit(2)` is unreachable because of
//! an earlier match arm). Both tests must stay green.
//!
//! See:
//!
//!   (G-T8-1 closed in Wave B; this test pins the strict-reject ship state).
//! - `main.rs::strict_reject_unknown_flags_exits_2_with_usage` (source pin).
//! - Internal Wave C planning notes.

use std::process::Command;

/// `CARGO_BIN_EXE_dcentrald` is set by Cargo at compile time of this
/// integration test crate to the absolute path of the dcentrald binary.
fn dcentrald_bin() -> &'static str {
    env!("CARGO_BIN_EXE_dcentrald")
}

#[test]
fn unknown_flag_exits_2_with_help_text() {
    let output = Command::new(dcentrald_bin())
        .arg("--bogus-flag-that-does-not-exist")
        .output()
        .expect("failed to spawn dcentrald binary");

    // Exit code 2 = conventional "bad invocation". Strict-reject must
    // exit 2 (not 1, not 0). On Unix, .code() returns Some(n) when the
    // process exited normally with that code.
    let exit_code = output.status.code().unwrap_or(-1);
    assert_eq!(
        exit_code,
        2,
        "expected exit code 2 (bad invocation); got {exit_code}. \
         stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The strict-reject prints to stderr (consistent with the early-boot
    // path which has no tracing/logging yet). stdout should be empty (we
    // don't pollute the operator's redirect-to-file).
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.to_lowercase().contains("unrecognized"),
        "expected 'unrecognized' in stderr; got: {stderr}"
    );

    // The help text dump is the operator-friendly bit. cli_help_text()
    // starts with a `dcentrald <version> — DCENT_OS · D-Central Technologies …`
    // banner and contains the literal `USAGE:` marker. Match the cheapest
    // unambiguous substring (the banner wording can evolve; USAGE: is stable).
    assert!(
        stderr.contains("USAGE:"),
        "expected cli_help_text USAGE: marker in stderr; got: {stderr}"
    );

    // Specifically reference an allowlist flag the operator might have
    // typo'd toward — proves the help text dump is the FULL list, not
    // a truncated message.
    assert!(
        stderr.contains("--get-fan"),
        "expected --get-fan in dumped help text (operators must see the \
         full canonical flag list inline); got: {stderr}"
    );
}

#[test]
fn version_flag_still_exits_0() {
    // Regression guard: the strict-reject MUST NOT affect the one-shot
    // info flags. `--version` runs BEFORE the unknown-flag detector by
    // design (see main.rs::run_main `wants_cli_info` block above the
    // strict-reject site). If this regresses, the strict-reject is in
    // the wrong place.
    let output = Command::new(dcentrald_bin())
        .arg("--version")
        .output()
        .expect("failed to spawn dcentrald binary");

    assert_eq!(
        output.status.code(),
        Some(0),
        "expected --version to exit 0; got {:?}",
        output.status.code()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("dcentrald"),
        "expected 'dcentrald' in --version stdout; got: {stdout}"
    );
}

#[test]
fn help_flag_still_exits_0() {
    // Regression guard: --help one-shot path unchanged. Same rationale
    // as the --version test above.
    let output = Command::new(dcentrald_bin())
        .arg("--help")
        .output()
        .expect("failed to spawn dcentrald binary");

    assert_eq!(
        output.status.code(),
        Some(0),
        "expected --help to exit 0; got {:?}",
        output.status.code()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("USAGE:"),
        "expected USAGE: in --help stdout; got: {stdout}"
    );
}

#[test]
fn typo_close_to_real_flag_still_rejected() {
    // The original `a lab unit` over-spawn pattern: a flag CLOSE to a real one
    // (e.g. `--gte-fan` for `--get-fan`) silently daemonized. Strict-
    // reject must catch it specifically because the cost of letting
    // `--gte-fan` through is the daemon starts mining instead of the
    // operator's intended fan-snapshot one-shot.
    let output = Command::new(dcentrald_bin())
        .arg("--gte-fan")
        .output()
        .expect("failed to spawn dcentrald binary");

    assert_eq!(
        output.status.code(),
        Some(2),
        "expected --gte-fan (typo of --get-fan) to exit 2 under \
         strict-reject; got {:?}. This is the EXACT failure mode \
         Wave C closes (the 2026-05-19 `a lab unit` over-spawn pattern). \
         stderr was: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}
