// DCENT_axe Stratum V1 Client
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Stratum V1 protocol client for ESP32-S3 BitAxe miners.
// Handles pool connection, job reception, share submission.
//
// Uses std::net::TcpStream (supported on ESP-IDF) — no async runtime needed.

pub mod address;
pub mod client;
pub mod gateway_solo;
pub mod mask;
pub mod mesh_solo;
pub mod solo;
pub mod types;
pub mod work;

pub use address::address_to_script_hex;
pub use client::{set_solo_share_hook, StratumClient};
pub use gateway_solo::{
    coinbase_wire_to_nonwitness, GatewaySoloError, GatewaySoloSubmit, SubmitPrep,
    MAX_SOLO_BLOCK_BYTES, MIN_SOLO_BLOCK_BYTES,
};
pub use mask::{mask_wallet, sanitize_pool_url};
pub use mesh_solo::{
    MeshSoloController, MeshSoloError, MeshSoloMetrics, MeshSoloMode, SoloBlockCandidate,
    SoloWorkEpoch, TipAdmit,
};
pub use solo::{
    assemble_coinbase_full, assemble_coinbase_nonwitness, assemble_solo_block, block_subsidy_sats,
    block_subsidy_sats_with_interval, coinbase_txid, compact_target_be, header_from_work,
    rolled_version, tip_supersedes, validate_found_block, validate_found_header, validate_tip,
    validate_tip_for_chain, ChainId, SoloChainParams, SoloError, SoloTemplateBuilder, SoloTip,
    BIP320_VERSION_MASK, MAINNET_HALVING_INTERVAL, REGTEST_HALVING_INTERVAL, SOLO_EXTRANONCE2_SIZE,
};
pub use types::*;
pub use work::{
    difficulty_to_target, double_sha256, parse_coinbase, CoinbaseDecoded, CoinbaseOutput,
    MiningWork, WorkBuilder,
};
