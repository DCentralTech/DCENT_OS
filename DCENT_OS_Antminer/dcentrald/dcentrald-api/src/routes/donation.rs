//! W9.5 — Donation pool public-info endpoint.
//!
//! Read-only, intentionally public disclosure of where the configured
//! donation slice flows. The donation address is operator-visible by
//! design — DungeonMaster donation pool runs at `pool.d-central.tech`
//! and pays out to a publicly verifiable on-chain address. Surfacing
//! the pool URL plus a payout-history link lets operators
//! independently verify on a block explorer that donations land where
//! the firmware claims they do (trust-but-verify).
//!
//! This endpoint is intentionally separate from `/api/config/donation`
//! (write-capable, requires auth) and from the failover snapshot at
//! `/api/pools` (which mixes user-pool failover with donation slice
//! routing). It is callable without auth and exposes only fields that
//! are already visible in `dcentrald.toml`'s `[donation]` section plus
//! a static `payout_address` constant baked into the firmware.
//!
//! Routes:
//!   - `GET /api/donation/info` — pool URL, payout address, explorer link
//!
//! Spec: W9.5. Per swarm review: trust-but-verify. Show D-Central's
//! donation pool address with a block-explorer link so operators don't
//! have to take our word that the donation slice flows where we claim.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::AppState;

/// D-Central's public DungeonMaster donation-pool payout address.
///
/// This is the on-chain address that the `pool.d-central.tech` pool
/// pays out to after solving blocks. Surfaced here for operator
/// trust-but-verify: any operator can paste this into mempool.space
/// (or any other explorer) and audit the payout history themselves.
///
/// The address is a static constant for two reasons:
///   1. Source-of-truth lives in firmware so the dashboard can render
///      it without relying on a hardcoded value in JS that drifts.
///   2. Changing the donation payout address requires a firmware
///      release (with a CHANGELOG entry), which is auditable.
pub const DONATION_PAYOUT_ADDRESS: &str = "bc1q04lzwddzgmtjex6jlsv2fwhe4se4jxje6rhzp6";

/// Public block-explorer URL template. `{addr}` is replaced with the
/// donation payout address. mempool.space is used because it's free,
/// non-tracking, censorship-resistant, and runs its own Bitcoin node
/// (no third-party API dependency).
const EXPLORER_URL_TEMPLATE: &str = "https://mempool.space/address/{addr}";

/// Default donation pool URL. Mirrors the `donation.pool_url` default
/// in `dcentrald::config` so both sources agree even when the operator
/// has not saved a custom donation config.
const DEFAULT_DONATION_POOL_URL: &str = "stratum+tcp://pool.d-central.tech:3333";

/// Default donation worker. Mirrors `dcentrald::config`.
const DEFAULT_DONATION_WORKER: &str = "DungeonMaster";

/// `GET /api/donation/info` — public donation pool disclosure.
///
/// Read-only. No auth required. Reads donation pool URL + worker from
/// `[donation]` section of the config (with defaults baked in for the
/// case where the operator has never saved a donation config), and
/// pairs them with the firmware-baked payout address + explorer URL.
///
/// Response shape:
/// ```json
/// {
///   "pool_url": "stratum+tcp://pool.d-central.tech:3333",
///   "pool_host": "pool.d-central.tech:3333",
///   "worker": "DungeonMaster",
///   "payout_address": "bc1q...DCent",
///   "explorer_url": "https://mempool.space/address/bc1q...",
///   "explorer_name": "mempool.space",
///   "verify_label": "View on-chain payout history",
///   "trust_model": "trust_but_verify",
///   "disclosure": "Donation slice flows to the address above. Verify on the block explorer."
/// }
/// ```
async fn get_donation_info(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let table = std::fs::read_to_string(crate::rest::get_config_path())
        .ok()
        .and_then(|contents| toml::from_str::<toml::Table>(&contents).ok());
    let donation = table
        .as_ref()
        .and_then(|table| table.get("donation"))
        .and_then(|value| value.as_table());

    let pool_url = donation
        .and_then(|table| table.get("pool_url"))
        .and_then(|value| value.as_str())
        .unwrap_or(DEFAULT_DONATION_POOL_URL)
        .to_string();
    let worker = donation
        .and_then(|table| table.get("worker"))
        .and_then(|value| value.as_str())
        .unwrap_or(DEFAULT_DONATION_WORKER)
        .to_string();

    let pool_host = pool_url
        .strip_prefix("stratum+tcp://")
        .unwrap_or(pool_url.as_str())
        .to_string();
    let explorer_url = EXPLORER_URL_TEMPLATE.replace("{addr}", DONATION_PAYOUT_ADDRESS);

    Json(serde_json::json!({
        "pool_url": pool_url,
        "pool_host": pool_host,
        "worker": worker,
        "payout_address": DONATION_PAYOUT_ADDRESS,
        "explorer_url": explorer_url,
        "explorer_name": "mempool.space",
        "verify_label": "View on-chain payout history",
        "trust_model": "trust_but_verify",
        "disclosure": "Donation slice flows to the address above. Verify on the block explorer.",
    }))
}

/// Build the donation sub-router. Merged into the top-level router by
/// `rest::build_router()`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/donation/info", get(get_donation_info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payout_address_is_segwit_v0() {
        // Sanity: bech32 mainnet segwit-v0 addresses begin with `bc1q`
        // and are 42 chars. Catches "someone pasted a testnet/taproot
        // address into the constant" mistakes during code review.
        assert!(DONATION_PAYOUT_ADDRESS.starts_with("bc1q"));
        assert_eq!(DONATION_PAYOUT_ADDRESS.len(), 42);
    }

    #[test]
    fn explorer_url_is_concrete_https() {
        let url = EXPLORER_URL_TEMPLATE.replace("{addr}", DONATION_PAYOUT_ADDRESS);
        assert!(url.starts_with("https://mempool.space/address/bc1q"));
        assert!(!url.contains("{addr}"));
    }
}
