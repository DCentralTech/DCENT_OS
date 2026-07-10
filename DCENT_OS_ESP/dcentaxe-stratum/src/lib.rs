// DCENT_axe Stratum V1 Client
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Stratum V1 protocol client for ESP32-S3 BitAxe miners.
// Handles pool connection, job reception, share submission.
//
// Uses std::net::TcpStream (supported on ESP-IDF) — no async runtime needed.

pub mod client;
pub mod mask;
pub mod types;
pub mod work;

pub use client::StratumClient;
pub use mask::{mask_wallet, sanitize_pool_url};
pub use types::*;
pub use work::{
    difficulty_to_target, double_sha256, parse_coinbase, CoinbaseDecoded, CoinbaseOutput,
    MiningWork, WorkBuilder,
};
