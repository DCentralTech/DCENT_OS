#!/bin/sh
# run_all_gates.sh — the comprehensive local verification gate (TEST-CI-1).
#
# The repo has NO git remote, so the committed .github/workflows never fire.
# `make verify` (the .git/hooks/pre-push stand-in) runs the offline safety/ban
# gates + the HAL-free Rust crate tests + the Docker workspace compile-gate — but
# it does NOT run the dashboard vitest suite or the toolbox pytest suite, so those
# regressions can ship unnoticed. This script runs EVERYTHING a CI would, with
# per-gate PASS/FAIL and a non-zero exit if any gate fails. Run it before a
# release, or wire it into the pre-push hook.
#
# Usage:
#   sh scripts/run_all_gates.sh           # all gates (needs Docker + python + npm)
#   sh scripts/run_all_gates.sh --fast    # skip the slow gates (Docker compile + pytest)
#
# Rust host tests default to the production-program baseline toolchain
# (DCENT_RUST_TOOLCHAIN=1.90.0). Override only when deliberately refreshing
# the baseline. On WSL, large Cargo artifacts default to /tmp to avoid filling
# the Windows workspace drive; set CARGO_TARGET_DIR or DCENT_RUN_ALL_TARGET_DIR
# to choose a different transient location.
#
# POSIX sh (BusyBox-compatible). Does NOT touch hardware.
set -u
if [ -n "${HOME:-}" ] && [ -d "$HOME/.cargo/bin" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCENTOS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$DCENTOS_DIR/../.." && pwd)"
FAST=0
[ "${1:-}" = "--fast" ] && FAST=1
RUST_TOOLCHAIN="${DCENT_RUST_TOOLCHAIN:-1.90.0}"
CARGO_RUN=""
CARGO_VERSION=""
RUSTC_VERSION=""

FAILED=""
SKIPPED=""

run_gate() {
    label=$1
    shift
    printf '\n=== GATE: %s ===\n' "$label"
    if "$@"; then
        printf 'GATE PASS: %s\n' "$label"
    else
        printf 'GATE FAIL: %s\n' "$label"
        FAILED="$FAILED $label"
    fi
}

have() { command -v "$1" >/dev/null 2>&1; }

is_wsl() {
    [ -r /proc/version ] && grep -qi microsoft /proc/version
}

configure_cargo() {
    case "$RUST_TOOLCHAIN" in
        ''|*[!A-Za-z0-9._+-]*)
            printf 'ERROR: invalid DCENT_RUST_TOOLCHAIN value: %s\n' "$RUST_TOOLCHAIN" >&2
            return 1 ;;
    esac

    if [ -n "${DCENT_RUN_ALL_TARGET_DIR:-}" ]; then
        export CARGO_TARGET_DIR="$DCENT_RUN_ALL_TARGET_DIR"
    elif [ -z "${CARGO_TARGET_DIR:-}" ] && is_wsl; then
        export CARGO_TARGET_DIR="/tmp/dcentos-run-all-gates-target"
    fi

    if have rustup; then
        if ! rustup run "$RUST_TOOLCHAIN" cargo --version >/dev/null 2>&1; then
            printf 'ERROR: Rust toolchain %s is not installed; run: rustup toolchain install %s\n' "$RUST_TOOLCHAIN" "$RUST_TOOLCHAIN" >&2
            return 1
        fi
        CARGO_RUN="rustup run $RUST_TOOLCHAIN cargo"
        CARGO_VERSION=$(rustup run "$RUST_TOOLCHAIN" cargo --version 2>/dev/null || true)
        RUSTC_VERSION=$(rustup run "$RUST_TOOLCHAIN" rustc --version 2>/dev/null || true)
    elif have cargo && have rustc; then
        RUSTC_VERSION=$(rustc --version 2>/dev/null || true)
        case "$RUSTC_VERSION" in
            *" $RUST_TOOLCHAIN "*) ;;
            *)
                printf 'ERROR: rustup unavailable and rustc is not %s: %s\n' "$RUST_TOOLCHAIN" "$RUSTC_VERSION" >&2
                return 1 ;;
        esac
        CARGO_RUN="cargo"
        CARGO_VERSION=$(cargo --version 2>/dev/null || true)
    else
        return 1
    fi

    printf 'run_all_gates: Rust toolchain: %s (%s)\n' "$RUST_TOOLCHAIN" "$RUSTC_VERSION"
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        printf 'run_all_gates: CARGO_TARGET_DIR=%s\n' "$CARGO_TARGET_DIR"
    fi
    return 0
}

# 1. Offline safety/ban gates (BIP320 rolling, EEPROM denylist, fan-PWM cap,
#    single-I2C-owner, SV2-prose honesty, sysupgrade/version gates, ...).
run_gate "offline-gates" sh "$SCRIPT_DIR/ci_offline_gates.sh"

# 2. HAL-free Rust crate tests — the only Rust tests that EXECUTE on the host
#    (the rest are compile-gated in Docker; gate 4).
#    dcentrald-thermal is HAL-free ONLY with --no-default-features (default = ["hal"],
#    which pulls the Unix-only dcentrald-hal); appended as a special-case so the
#    R3-A controller NaN fail-closed + safety_pwm_cap matrix run on the host too.
#    dcentrald-asic is Linux/WSL-host-testable and carries the EEPROM denylist,
#    recovery double-gate, and BIP320 behavioral pins.
#    The other four crates are no-HAL by default and need no special flag.
if configure_cargo; then
    run_gate "rust-host-tests" sh -c "cd '$DCENTOS_DIR/dcentrald' && for c in dcentrald-api-types dcentrald-common dcentrald-stratum dcentrald-silicon-profiles dcentrald-asic; do echo \"[host-test] \$c\"; $CARGO_RUN test -p \"\$c\" --quiet || exit 1; done && echo '[host-test] dcentrald-thermal (--no-default-features, HAL-free)' && $CARGO_RUN test -p dcentrald-thermal --no-default-features --quiet"
    run_gate "rust-input-clippy" sh -c "cd '$DCENTOS_DIR/dcentrald' && $CARGO_RUN clippy --no-deps -p dcentrald-stratum --lib -- -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic && $CARGO_RUN clippy --no-deps -p dcentrald-asic --lib -- -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic"
else
    run_gate "rust-host-tests" false
    run_gate "rust-input-clippy" false
fi

# 3. Dashboard: i18n-parity + tsc + vite build + size guard, then vitest.
if have npm; then
    run_gate "dashboard-build" sh -c "cd '$DCENTOS_DIR/dashboard' && npm run build"
    run_gate "dashboard-vitest" sh -c "cd '$DCENTOS_DIR/dashboard' && npm test"
else
    printf 'ERROR: npm is required for dashboard-build and dashboard-vitest\n' >&2
    run_gate "dashboard-build" false
    run_gate "dashboard-vitest" false
fi

if [ "$FAST" -eq 0 ]; then
    # 4. Full workspace armv7-musl compile-gate (Docker — needs Docker running).
    run_gate "rust-workspace-compile" sh "$SCRIPT_DIR/run_dcentrald_tests.sh"

    # 5. Toolbox pytest (PC-side CLI / install-recovery).
    if have python; then
        run_gate "toolbox-pytest" sh -c "cd '$REPO_ROOT/projects/dcent-toolbox' && python -m pytest -q"
    elif have python3; then
        run_gate "toolbox-pytest" sh -c "cd '$REPO_ROOT/projects/dcent-toolbox' && python3 -m pytest -q"
    else
        printf 'ERROR: python or python3 is required for toolbox-pytest\n' >&2
        run_gate "toolbox-pytest" false
    fi
else
    SKIPPED="$SKIPPED rust-workspace-compile(--fast) toolbox-pytest(--fast)"
fi

printf '\n========================================\n'
[ -n "$SKIPPED" ] && printf 'run_all_gates: SKIPPED —%s\n' "$SKIPPED"
if [ -n "$FAILED" ]; then
    printf 'run_all_gates: FAIL —%s\n' "$FAILED"
    exit 1
fi
printf 'run_all_gates: ALL GATES PASS\n'
