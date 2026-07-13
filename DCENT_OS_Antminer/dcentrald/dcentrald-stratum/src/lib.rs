//! dcentrald-stratum: Stratum V1 mining protocol client
//!
//! Pure network protocol crate with no dependencies on other dcentrald crates.
//! Handles pool connection, job reception, share submission, failover, and donation switching.
//!
//! # Architecture
//!
//! The stratum client runs as an async Tokio task. It communicates with the rest of
//! dcentrald through typed mpsc channels:
//!
//! - `job_tx`: Sends new `JobTemplate` to the job dispatcher when mining.notify arrives
//! - `share_rx`: Receives `ValidShare` from the share validator for submission
//! - `status_tx`: Sends `StratumStatus` updates (connected, difficulty changes, etc.)

pub mod acceptance_tracker;
// W3-B (2026-07-07): coin-parameterization seam for DCENT_OS's first non-SHA256
// coin (Litecoin/Scrypt on L7/BM1489). DEFAULT-OFF behind the `scrypt-l7`
// feature so production SHA-256 builds are byte-unchanged. Pure descriptor —
// composes the Scrypt wire contracts in `scrypt.rs` into per-coin `CoinParams`;
// the Bitcoin/SHA-256d path is left entirely untouched (it is the default coin).
#[cfg(feature = "scrypt-l7")]
pub mod coin;
// W4.5 (dcent-pack Change B, staged): pure heartbeat-field derivations —
// BIP34 coinbase height, session-best difficulty + compact formatting, and the
// block-found (share-meets-network-target) predicate. Host-testable; the daemon
// (`dcentrald::bridge_glue`) wires the outputs into the bridge heartbeat.
pub mod derivations;
pub mod pool_api;
pub mod pool_quality;
// Wave D (RE-006 closure, 2026-05-19): clean-room LuxOS-shape pool-failover
// FSM runtime. Compiled always. The `[stratum].smart_failover_enabled`
// toggle is plumbed end-to-end (config → StratumConfig → V1 client +
// telemetry) but the FSM does NOT yet drive live pool selection — that
// promotion is Wave-H operator-soak gated. See the `pool_failover` module
// docs and
// §RE-006.
pub mod pool_failover;
pub mod router;
pub mod scrypt;
/// Pure share-pipeline façade (ADR-0009). Prefer this module name in new code.
pub mod share_pipeline;
pub mod types;
pub mod url_validator;
pub mod v1;
pub mod v2;
pub mod version_mask;
pub mod work;

pub use acceptance_tracker::{AcceptanceTracker, DEFAULT_WINDOW as ACCEPTANCE_DEFAULT_WINDOW};
pub use pool_quality::{apply_stratum_status, PoolQualitySnapshot};
pub use router::StratumRouter;
pub use types::*;
pub use v1::client::StratumV1Client;
#[cfg(feature = "sv2")]
pub use v2::client::StratumV2Client;
pub use work::{
    compute_midstate_from_prefix, double_sha256, sha256_compress, validate_full_header,
    validate_share, MiningWork, WorkBuilder,
};
