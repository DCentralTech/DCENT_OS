//! Full pair -> heartbeat -> telemetry flow against a canned axum mock bridge.
//!
//! dcentrald-api drives its REST tests with axum; here we stand up a real local
//! axum server on `127.0.0.1:0` (ephemeral port) and point a `BridgeClient` at
//! it. This exercises the actual reqwest HTTP path end-to-end (json bodies,
//! status mapping, header emission) rather than mocking at the type level.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};

use dcentrald_bridge::{BridgeClient, HeartbeatOutcome, HeartbeatRequest, UnitSecret};

/// What the mock captured from a heartbeat POST — the RAW body bytes and the two
/// freshness/signature headers, so the test can act as the bridge VERIFIER.
#[derive(Default, Clone)]
struct CapturedHeartbeat {
    body: Vec<u8>,
    ts: Option<String>,
    sig: Option<String>,
}

/// Shared mock state: how many /pair attempts we've seen (to drive a
/// 409-replay-then-200 sequence) + the last heartbeat we captured.
#[derive(Default)]
struct MockState {
    pair_attempts: AtomicU32,
    last_heartbeat: Mutex<Option<CapturedHeartbeat>>,
}

/// Independent HMAC recompute — the bridge verifier's algorithm re-implemented
/// from raw `hmac`/`sha2` (NOT via `dcentrald_bridge::heartbeat_sig`), so this
/// proves the signer and an independent verifier agree byte-for-byte, exactly
/// like the C-host / Python cross-language KAT.
fn independent_heartbeat_sig(secret: &[u8], ts: u64, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    let body_sha = Sha256::digest(body);
    let body_sha_hex: String = body_sha.iter().map(|b| format!("{:02x}", b)).collect();
    let msg = format!("heartbeat:{ts}:{body_sha_hex}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("hmac key");
    mac.update(msg.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "version": "dcent-pack-0.1.0-dev",
        "product": "dcent-pack",
        "uptime_s": 42,
        "miner": {"paired": false}
    }))
}

async fn pair(State(st): State<Arc<MockState>>, body: String) -> impl IntoResponse {
    // The body must be valid JSON carrying device_id + hmac.
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    assert!(v.get("device_id").is_some(), "pair body missing device_id");
    assert!(v.get("hmac").is_some(), "pair body missing hmac");
    assert!(v.get("ts").is_some(), "pair body missing ts");

    let n = st.pair_attempts.fetch_add(1, Ordering::SeqCst);
    if n == 0 {
        // First attempt: 409-replay. Client must refresh ts and re-sign.
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"ok": false, "error": "replay", "retry": false})),
        )
            .into_response();
    }
    // Second attempt: success.
    Json(serde_json::json!({
        "ok": true,
        "bridge_name": "dcent-pack-1234",
        "proxy_url": "http://dcent-pack-1234.local/",
        "telemetry_url": "__TELEMETRY_URL__"
    }))
    .into_response()
}

async fn heartbeat(
    State(st): State<Arc<MockState>>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        v.get("device_id").and_then(|d| d.as_str()),
        Some("dcentos-test")
    );
    // Capture the RAW body bytes + the freshness/signature headers so the test
    // (acting as the bridge verifier) can independently recompute the HMAC.
    let captured = CapturedHeartbeat {
        body: body.into_bytes(),
        ts: headers
            .get("x-dcent-heartbeat-ts")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string()),
        sig: headers
            .get("x-dcent-heartbeat-sig")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string()),
    };
    *st.last_heartbeat.lock().expect("lock") = Some(captured);
    Json(serde_json::json!({"ok": true, "paired": true}))
}

async fn telemetry() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "bridge_name": "dcent-pack-1234",
        "firmware_version": "0.1.0-dev",
        "temperature": {
            "sensor": "TMP102",
            "present": true,
            "status": "ok",
            "external_temperature_c": 22.5,
            "last_sample_age_ms": 800
        },
        "accessories": {"temperature_feedback": {"enabled": true}},
        "pairing": {"paired": true}
    }))
}

/// Spin up the mock bridge, returning its base URL once it is accepting.
async fn spawn_mock() -> (String, Arc<MockState>) {
    let state = Arc::new(MockState::default());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let base = format!("http://{}", addr);

    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/pair", post(pair))
        .route("/api/v1/miner/heartbeat", post(heartbeat))
        .route("/api/v1/telemetry", get(telemetry))
        .with_state(state.clone());

    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    (base, state)
}

#[tokio::test]
async fn full_pair_heartbeat_telemetry_flow() {
    let (base, state) = spawn_mock().await;
    let secret = UnitSecret::from_bytes([7u8; 32]);

    let mut client = BridgeClient::new(&base).expect("client");

    // 1. Discovery — health probe identifies the dcent-pack bridge.
    let health = client.probe_health().await.expect("probe ok");
    assert!(health.is_some(), "health probe should identify the bridge");
    assert!(health.unwrap().is_dcent_pack());

    // 2. Pair with retry — first attempt is 409-replay, retry succeeds with a
    //    refreshed ts (the wrapper bumps ts past the previous second).
    let resp = client
        .pair_with_retry(
            &secret,
            "dcentos-test",
            "AA:BB:CC:DD:EE:FF",
            "Antminer S19j Pro",
            "dcent-s19jpro",
            80,
        )
        .await
        .expect("pair should succeed after replay refresh");
    assert_eq!(resp.bridge_name, "dcent-pack-1234");
    assert_eq!(
        state.pair_attempts.load(Ordering::SeqCst),
        2,
        "exactly 2 pair attempts: 409-replay then 200"
    );
    // telemetry_url was cached from the pair response.
    assert!(client.telemetry_url.is_some());

    // 3. Heartbeat — 200 + paired:true, SIGNED with the per-unit secret.
    let hb_req = HeartbeatRequest {
        device_id: "dcentos-test".into(),
        uptime_s: 3600,
        mode: "mining_heating".into(),
        miner_temperature_c: Some(62.5),
        power_w: Some(3250),
        // Change-B expanded telemetry rides on the SIGNED body.
        hashrate_ths: Some(21.3),
        shares_accepted: Some(12044),
        shares_rejected: Some(7),
        fan_speed_rpm: Some(5400),
        ..Default::default()
    };
    let hb = client
        .heartbeat(&hb_req, Some(&secret))
        .await
        .expect("heartbeat ok");
    assert_eq!(hb, HeartbeatOutcome::Ok);

    // The mock (verifier) recomputes the HMAC over the EXACT captured body bytes
    // with the SHARED secret and asserts equality — proving the DCENT_OS signer
    // and an independent verifier agree (the whole point of Change A).
    let cap = state
        .last_heartbeat
        .lock()
        .expect("lock")
        .clone()
        .expect("heartbeat was captured");
    let ts_str = cap.ts.expect("X-DCent-Heartbeat-Ts header present");
    let sig = cap.sig.expect("X-DCent-Heartbeat-Sig header present");
    // Freshness header is canonical decimal seconds (no padding / non-digits).
    assert!(
        ts_str.chars().all(|c| c.is_ascii_digit()) && !ts_str.is_empty(),
        "ts header must be decimal, got {ts_str:?}"
    );
    let ts: u64 = ts_str.parse().expect("ts parses as u64");
    let expected = independent_heartbeat_sig(&[7u8; 32], ts, &cap.body);
    assert_eq!(sig, expected, "signer<->verifier HMAC must agree");
    // Sanity: the signed body carries the Change-B live fields on the wire.
    let body_json: serde_json::Value = serde_json::from_slice(&cap.body).expect("body json");
    assert_eq!(
        body_json.get("hashrate_ths").and_then(|v| v.as_f64()),
        Some(21.3)
    );
    assert_eq!(
        body_json.get("shares_accepted").and_then(|v| v.as_u64()),
        Some(12044)
    );

    // 4. Telemetry — usable external temperature extracted.
    // (Patch the cached telemetry_url placeholder to the real mock URL.)
    client.telemetry_url = Some(format!("{}/api/v1/telemetry", base));
    let tel = client.poll_telemetry().await.expect("telemetry ok");
    assert!(tel.accessories.temperature_feedback.enabled);
    let temp = client.record_and_extract_temp(&tel);
    assert_eq!(temp, Some(22.5));
}

#[tokio::test]
async fn heartbeat_paired_false_signals_repair() {
    // A separate mock that always returns paired:false.
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    let app = Router::new().route(
        "/api/v1/miner/heartbeat",
        post(|| async { Json(serde_json::json!({"ok": true, "paired": false})) }),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = BridgeClient::new(&base).unwrap();
    let hb_req = HeartbeatRequest {
        device_id: "dcentos-test".into(),
        uptime_s: 1,
        mode: "idle".into(),
        ..Default::default()
    };
    // Unsigned path (secret None) preserves today's behavior — staged rollout.
    let hb = client.heartbeat(&hb_req, None).await.expect("heartbeat ok");
    assert_eq!(hb, HeartbeatOutcome::NeedsRepair);
}
