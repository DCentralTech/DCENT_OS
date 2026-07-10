//! Stratum V2 protocol support.
//!
//! Native SV2 Mining Device client with Noise_NX encryption.
//! Ported from the proven DCENT_axe (ESP32) implementation.

#[cfg(feature = "sv2")]
pub mod adapter;
#[cfg(feature = "sv2")]
pub mod auth;
#[cfg(feature = "sv2")]
pub mod channel;
#[cfg(feature = "sv2")]
pub mod client;
#[cfg(feature = "sv2")]
pub mod difficulty_autotune;
#[cfg(feature = "sv2")]
pub mod framing;
#[cfg(feature = "sv2")]
pub mod noise;
#[cfg(feature = "sv2")]
pub mod types;

#[cfg(feature = "jd")]
pub mod jd;

// W9.3 — mock SV2 pool harness (OCEAN + DEMAND/SRI styles). Compiled
// only when `mock-pool` feature is on, which is exclusively turned on
// by the `tests/sv2_multi_pool.rs` integration test and the
// `cross-compile-matrix.yml` `sv2-mock-pool-tests` CI cell. Never
// enabled in production sysupgrade tarballs.
#[cfg(all(feature = "sv2", feature = "mock-pool"))]
pub mod test_server;
