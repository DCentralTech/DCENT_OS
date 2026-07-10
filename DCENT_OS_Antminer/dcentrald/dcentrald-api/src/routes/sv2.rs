//! W5.2 — SV2 protocol REST endpoints.
//!
//! Read-only handlers exposing the active Stratum V2 session: connection
//! state, Noise handshake details, and message/byte counters. When the
//! daemon is connected via Stratum V1 the handlers fall through to a
//! protocol-tagged stub so the dashboard can render a consistent shape.
//!
//! Routes:
//!   - `GET /api/pool/sv2/status`     — current SV2 session info
//!   - `GET /api/pool/sv2/handshake`  — Noise handshake details
//!   - `GET /api/pool/sv2/messages`   — SV2 message/byte counters

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::AppState;

/// GET /api/pool/sv2/status -- Current SV2 session info.
///
/// Returns the active SV2 session state when connected via Stratum V2,
/// or a minimal response indicating V1 mode when SV2 is not active.
async fn get_sv2_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let response = if let Some(ref session) = miner.pool.sv2_session {
        serde_json::json!({
            "connected": true,
            "protocol": "sv2",
            "encrypted": miner.pool.encrypted,
            "session": {
                "channel_id": session.channel_id,
                "cipher_suite": session.cipher_suite,
                "noise_nonce_tx": session.noise_nonce_tx,
                "noise_nonce_rx": session.noise_nonce_rx,
                "bytes_encrypted": session.bytes_encrypted,
                "bytes_decrypted": session.bytes_decrypted,
                "messages_sent": session.messages_sent,
                "messages_received": session.messages_received,
            },
            "custom_job": miner.pool.sv2_custom_job,
        })
    } else {
        serde_json::json!({
            "connected": false,
            "protocol": miner.pool.protocol,
        })
    };
    Json(response)
}

/// GET /api/pool/sv2/handshake -- Noise handshake details.
///
/// Returns cipher suite, handshake latency, pool public key fingerprint,
/// and certificate validity window for the current SV2 session.
/// When not connected via SV2, returns a stub indicating V1 mode.
async fn get_sv2_handshake(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let response = if let Some(ref session) = miner.pool.sv2_session {
        serde_json::json!({
            "connected": true,
            "protocol": "sv2",
            "handshake": {
                "cipher_suite": session.cipher_suite,
                "latency_ms": session.handshake_latency_ms,
                "pool_pubkey_fingerprint": session.pool_pubkey_fingerprint,
                "certificate_valid_from": session.certificate_valid_from,
                "certificate_not_after": session.certificate_not_after,
            },
        })
    } else {
        serde_json::json!({
            "connected": false,
            "protocol": miner.pool.protocol,
        })
    };
    Json(response)
}

/// GET /api/pool/sv2/messages -- SV2 message history / counts.
///
/// Returns aggregate message counters (sent/received) and encrypted byte
/// totals. A full message-by-message log will be provided via a separate
/// broadcast channel in a future release.
async fn get_sv2_messages(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let miner = state.state_rx.borrow().clone();
    let response = if let Some(ref session) = miner.pool.sv2_session {
        serde_json::json!({
            "connected": true,
            "protocol": "sv2",
            "messages": {
                "sent": session.messages_sent,
                "received": session.messages_received,
                "bytes_encrypted": session.bytes_encrypted,
                "bytes_decrypted": session.bytes_decrypted,
            },
            "history": [],
        })
    } else {
        serde_json::json!({
            "connected": false,
            "protocol": miner.pool.protocol,
        })
    };
    Json(response)
}

/// Build the SV2 sub-router. Merged into the top-level router by
/// `rest::build_router()`.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/pool/sv2/status", get(get_sv2_status))
        .route("/api/pool/sv2/handshake", get(get_sv2_handshake))
        .route("/api/pool/sv2/messages", get(get_sv2_messages))
}
