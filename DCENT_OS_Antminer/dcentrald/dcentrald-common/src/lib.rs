//! Shared no-HAL contract utilities for dcentrald.
//!
//! This crate is intentionally HAL-free, OS-free, and async-runtime-free, in
//! the same spirit as `dcentrald-api-types`. It is the host-safe boundary
//! for utilities that must be reachable from both the API surface
//! (`dcentrald-api`) and the protocol clients (`dcentrald-stratum`) without
//! pulling in a hardware dependency. New no-HAL helpers (config-validation,
//! ID redaction, units conversion) belong here.
//!
//! Modules:
//! - [`wallet_mask`] — Bitcoin/Litecoin wallet-address masking for log and
//!   UI emission. Provides `mask_wallet()`, `mask_in_string()`, and
//!   `is_likely_wallet()`. See module docs for the threat model and which
//!   addresses are recognized (bech32, bech32m, base58 P2PKH/P2SH, hex).
//! - [`chain_voltage`] — AT-1 measured-vs-commanded per-chain rail-voltage
//!   resolver. Reuses [`dspic_decode`]'s 0x3A `MEASURE_VOLTAGE` decode to feed
//!   the autotuner/telemetry a provenance-tagged rail voltage (read-back only).
//! - [`at3_rail`] — AT-3 process-global publish/consume slot. The am2 hybrid
//!   loop's gated, default-OFF quiet-window 0x3A read publishes a fresh measured
//!   rail here; the API per-chain telemetry projection reads it back so a
//!   plausible reading is tagged `measured`. Read-only/measure-only.

pub mod at3_rail;
pub mod chain_voltage;
pub mod dspic_decode;
pub mod dspic_heartbeat;
pub mod time;
pub mod units;
pub mod wallet_mask;

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide gate for the log-tail passthrough sanitizer.
///
/// W1.4: when `true` (the default), the `/api/debug/log` endpoint masks
/// any wallet-shaped substrings before serializing the response. Setting
/// this to `false` is an opt-out for operators with structured-log
/// collectors that need raw addresses.
///
/// Per-call masking on `worker=` / `username=` / `wallet=` log fields is
/// independent of this flag and cannot be disabled via the config. To see
/// raw addresses on the wire, use `RUST_LOG=trace` (TRACE-level only,
/// gated by EnvFilter, off by default in production).
static MASK_LOGS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Set the process-wide log-tail mask flag. Called once at daemon startup
/// from the [logging] section of dcentrald.toml.
pub fn set_mask_logs(enabled: bool) {
    MASK_LOGS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// Read the process-wide log-tail mask flag.
pub fn mask_logs_enabled() -> bool {
    MASK_LOGS_ENABLED.load(Ordering::Relaxed)
}
