//! BUG-9 (2026-06-05) — standard-daemon mining bring-up must be CRASH-SAFE:
//! a hung or failed `init()` must NOT take the :8080/:4028 API down.
//!
//! ## Live symptom that motivated this
//!
//! On a live S9 (am1, BM1387) freshly installed with DCENT_OS, the daemon
//! boots MANAGEMENT-ONLY (mining disabled by design — `mining_start_enabled()
//! == false`). After the operator completed the setup wizard (pool set, mining
//! enabled), restarting the daemon to ENGAGE mining made the `:8080` API go
//! DOWN and stay down 4+ minutes with no recovery — the S9 cold-boot
//! (PSU/PIC-enumerate/open-core, all blocking I²C/UART I/O) wedged the whole
//! daemon, INCLUDING the API server.
//!
//! ## Root cause (the structure this test pins)
//!
//! In `Daemon::run_lifecycle()` the API server (`start_api_servers`) is spawned
//! DEEP inside the function, AFTER the 7-phase hardware bring-up `init()`.
//! Unlike the hybrid / serial / stratum-proxy / am3-bb arms (which spawn the
//! API BEFORE the mining loop), the standard BraiinsOS-FPGA daemon arm (S9/am1
//! + am2-s17) has no API until `init()` returns. So a `init()` that HANGS
//! (documented S9 failure modes: "AXI IIC Controller Stuck State (SR=0xC0)",
//! "dead PICs burn the entire heartbeat budget", a stuck chain-UART RX) means
//! `start_api_servers` is NEVER reached → no dashboard, no recovery.
//!
//! ## The fix (two guards, both pinned below)
//!
//! (A) BOUND THE HANG: `init()` is raced against `resolve_init_timeout()`
//!     (`DEFAULT_INIT_TIMEOUT_SECS`, env-overridable via
//!     `DCENT_INIT_TIMEOUT_SECS`). An infinite wedge becomes a clean error in
//!     bounded time.
//!
//! (B) FALL BACK TO MANAGEMENT-ONLY *WITH THE API UP*: on timeout OR error, the
//!     defensive hardware-safe-off teardown `self.shutdown()` runs, then
//!     `run_lifecycle` hands off to `self.run_api_only()` — which builds a clean
//!     management `AppState`, SPAWNS the API, and parks until SIGTERM. The
//!     dashboard stays reachable and the bring-up error is reported; the daemon
//!     never hangs and never crashes on a failed bring-up.
//!
//! ## Why source-parse instead of a runtime test
//!
//! `daemon.rs` needs a live HAL (real `/dev/mem`, `/dev/i2c-*`, UIO) to execute
//! `run_lifecycle` / `init` / `run_api_only`, so a host-safe runtime test
//! cannot drive a real `init() → hang/Err`. These are STRUCTURAL source-order
//! assertions (the same technique the `main.rs` F1/F5 tests use) so a future
//! edit that re-introduces the unbounded/locked-out class fails CI.

const DAEMON_RS: &str = include_str!("../src/daemon.rs");

/// (A) The `init()` await must be BOUNDED by a timeout — never a bare
/// `self.init().await?` that can hang forever. Pin that the bring-up is raced
/// against `resolve_init_timeout()` via `tokio::time::timeout`.
#[test]
fn bug9_init_is_bounded_by_a_timeout() {
    assert!(
        DAEMON_RS.contains("fn resolve_init_timeout()"),
        "BUG-9 (A): resolve_init_timeout() helper is missing — the hardware \
         bring-up timeout bound was removed"
    );
    assert!(
        DAEMON_RS.contains("const DEFAULT_INIT_TIMEOUT_SECS"),
        "BUG-9 (A): DEFAULT_INIT_TIMEOUT_SECS constant is missing"
    );
    assert!(
        DAEMON_RS.contains("tokio::time::timeout(init_timeout, self.init())"),
        "BUG-9 (A) REGRESSION: `init()` is no longer raced against a timeout — \
         a hung cold-boot (AXI-IIC stuck / dead-PIC heartbeat budget / chain-UART \
         wedge) would again take the :8080 API down with no recovery"
    );

    // The OLD unbounded shape `self.init().await?` (a `?` directly on the await)
    // must NOT come back. We allow `self.init()` to appear (it is the argument to
    // timeout), but never the `?`-on-await form.
    assert!(
        !DAEMON_RS.contains("self.init().await?"),
        "BUG-9 (A) REGRESSION: the unbounded `self.init().await?` shape is back — \
         bring-up can hang forever and lock out the management plane"
    );
}

/// (A) The env override `DCENT_INIT_TIMEOUT_SECS` exists and is FLOORED so an
/// operator cannot accidentally set it to 0 (or a too-small value) and re-break
/// the no-hang guarantee. The override is for RAISING the bound on a slow lab
/// unit, not disabling it.
#[test]
fn bug9_init_timeout_env_override_is_floored() {
    assert!(
        DAEMON_RS.contains("DCENT_INIT_TIMEOUT_SECS"),
        "BUG-9 (A): the DCENT_INIT_TIMEOUT_SECS env override is missing"
    );
    let helper_start = DAEMON_RS
        .find("fn resolve_init_timeout()")
        .expect("resolve_init_timeout missing");
    // Bound the inspection to the helper body (next `\nfn ` or `\nconst ` after it).
    let rest = &DAEMON_RS[helper_start..];
    let helper_end = rest[3..].find("\nfn ").map(|i| i + 3).unwrap_or(rest.len());
    let body = &rest[..helper_end];
    assert!(
        body.contains(".filter(|&s| s > 0)") || body.contains("s > 0"),
        "BUG-9 (A): resolve_init_timeout must reject 0 (a 0-second timeout would \
         instantly false-trip and re-break bring-up)"
    );
    assert!(
        body.contains(".max(10)"),
        "BUG-9 (A): resolve_init_timeout must floor the env override (e.g. .max(10)) \
         so a too-small value can't false-trip on a healthy unit"
    );
}

/// (B) On bring-up failure the lifecycle must FALL BACK to `run_api_only()`
/// (which spawns the management API + parks) — NOT propagate the error straight
/// out (which on the standard daemon arm would park in `enter_management_only`
/// with NO API server, locking the operator out).
#[test]
fn bug9_bringup_failure_falls_back_to_run_api_only() {
    // Find the bounded-init match and its Err arm.
    let timeout_site = DAEMON_RS
        .find("tokio::time::timeout(init_timeout, self.init())")
        .expect("BUG-9: bounded init() site missing");
    // The fall-back call must appear AFTER the bounded-init site.
    let fallback = DAEMON_RS[timeout_site..].find("return self.run_api_only().await;");
    assert!(
        fallback.is_some(),
        "BUG-9 (B) REGRESSION: the init-failure path no longer falls back to \
         run_api_only() — a failed/hung bring-up would leave the standard daemon \
         arm in management-only WITHOUT an API (operator locked out of the \
         dashboard/wizard/re-flash plane)"
    );
}

/// (B) The hardware-safe-off teardown (`self.shutdown()`) must run BEFORE the
/// management-only fall-back parks — the boards must be de-energized / fans
/// idled / watchdog disarmed before we sit idle (the same hardware-already-off
/// contract the no-brick #6 `Daemon::run()` wrapper guarantees).
#[test]
fn bug9_teardown_precedes_management_only_fallback() {
    let timeout_site = DAEMON_RS
        .find("tokio::time::timeout(init_timeout, self.init())")
        .expect("BUG-9: bounded init() site missing");
    let window = &DAEMON_RS[timeout_site..];
    let teardown = window
        .find("self.shutdown().await")
        .expect("BUG-9 (B) SAFETY: the init-failure path must run self.shutdown() teardown");
    let fallback = window
        .find("return self.run_api_only().await;")
        .expect("BUG-9 (B): run_api_only fall-back missing");
    assert!(
        teardown < fallback,
        "BUG-9 (B) SAFETY VIOLATION: self.shutdown() (hardware-safe-off) must run \
         BEFORE the run_api_only() management-only park — boards must be \
         de-energized before the daemon idles"
    );
}

/// The fall-back uses the EXISTING `run_api_only()` (which already spawns the
/// API via `start_api_servers` and parks on the shutdown token) — so the API
/// the operator reaches in the failure path is the same well-tested management
/// plane the `!mining_start_enabled()` boot already uses. Pin that
/// `run_api_only` both exists and spawns the API.
#[test]
fn bug9_run_api_only_spawns_the_api_server() {
    let api_only = DAEMON_RS
        .find("async fn run_api_only(")
        .expect("run_api_only definition missing");
    // Bound to the function body up to the next `\n    async fn ` / `\n    fn `.
    let rest = &DAEMON_RS[api_only..];
    let end = rest[10..]
        .find("\n    async fn ")
        .or_else(|| rest[10..].find("\n    fn "))
        .map(|i| i + 10)
        .unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("start_api_servers"),
        "BUG-9 (B): run_api_only() must spawn the API via start_api_servers — \
         it is the management plane the bring-up-failure fall-back relies on"
    );
}
