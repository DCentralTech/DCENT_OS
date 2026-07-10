//! # dcentrald-bridge
//!
//! Miner-side client for the **DCENT Expansion Pack** ("dcent-pack") bridge —
//! a 12 V ESP32-C6 + W5500 board that gives a DCENT_OS miner Wi-Fi setup and an
//! external temperature feedback path without the customer touching the miner
//! IP. This crate discovers, pairs with, heartbeats, and consumes telemetry
//! from the bridge over the private `10.77.0.0/24` Ethernet subnet.
//!
//! This is a **no-HAL leaf crate**: it does NOT depend on `dcentrald-hal`,
//! `-asic`, `-thermal`, or `dcentrald-api`. The daemon wires the external
//! temperature into the existing `AppState.room_temp_c10` atomic by
//! implementing the small [`task::RoomTempSink`] / [`task::MinerStatusProvider`]
//! ports, so this crate cross-compiles cleanly and carries no mining deps.
//!
//! ## Contract source of truth
//!
//! The wire contract is the bridge firmware
//! (`dcent-expansion-pack/DCENT_OS_ESP-idf/main/bridge_api.c` + `pack_id.c` +
//! `ota_handler.c`), not the spec doc. Where they differ, this crate follows
//! the firmware (see module docs in [`crypto`] and [`protocol`]).
//!
//! ## Layout
//! - [`crypto`]   — HMAC signing (pair / ota / ota_pull / ws) + [`crypto::UnitSecret`].
//! - [`protocol`] — serde wire types (telemetry uses `external_temperature_c`, no `value_c`).
//! - [`error`]    — [`error::PairError`] / [`error::BridgeError`] (spec §7).
//! - [`client`]   — [`client::BridgeClient`] (discovery / pair / heartbeat / telemetry / OTA).
//! - [`task`]     — [`task::bridge_client_task`] (gateway-watch → discover → pair → serve).
//! - [`config`]   — [`config::BridgeConfig`] (the `[bridge]` TOML section).

pub mod client;
pub mod config;
pub mod crypto;
pub mod error;
pub mod protocol;
pub mod task;

// --- Convenience re-exports ------------------------------------------------

pub use client::{usable_temperature, BridgeClient, HeartbeatOutcome};
pub use config::BridgeConfig;
pub use crypto::{
    heartbeat_sig, ota_pull_sig, ota_sig, pair_hmac, unit_secret_from_base32, ws_sig,
    SecretDecodeError, UnitSecret,
};
pub use error::{BridgeError, PairError};
pub use protocol::{
    BridgeTelemetry, BridgeTemperature, HealthResponse, HeartbeatRequest, HeartbeatResponse,
    PairRequest, PairResponse,
};
pub use task::{
    bridge_client_task, is_bridge_gateway, parse_default_gateway, read_default_gateway,
    BridgeRuntime, MinerStatusProvider, RoomTempSink, BRIDGE_GATEWAY_IP,
};
