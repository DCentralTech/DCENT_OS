//! A04 — V/F profile save/restore REST routes (LuxOS `:8080` parity).
//!
//! `GET  /api/profile/download` returns the saved V/F profile bytes verbatim.
//! `POST /api/profile/upload`   stores the uploaded JSON profile bytes verbatim.
//!
//! LuxOS exposes `/profile/download` + `/profile/upload` for JSON V/F profile
//! save/restore; this adds the equivalent operator save/restore surface to
//! DCENT (which previously had silicon profiles in-memory but no REST
//! save/restore of runtime V/F profile state).
//!
//! Round-trip contract: upload then download returns BYTE-IDENTICAL bytes.
//! Per the goldmine open-item OI-05 ("`/profile/download` exact JSON schema
//! not documented"), the store is intentionally schema-agnostic — it only
//! validates that the body is a JSON object and persists the ORIGINAL bytes
//! unchanged, which guarantees the byte-exact round trip regardless of the
//! eventual canonical schema.
//!
//! Source (goldmine finding):
//!
//!   — API-07: "`/profile/download` and `/profile/upload`: JSON V/F profile
//!     save/restore"; CAND-02: "lets fleet operators save/restore V/F
//!     profiles"; OI-05: exact JSON schema undocumented.
//!
//! Additive + read-mostly: the only write is a single operator-supplied blob
//! persisted atomically under the autotuner profile dir. It never touches any
//! live mining / voltage / thermal / PSU dispatch path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use dcent_schema::capability::RuntimeCapability;
use serde_json::json;

use crate::AppState;

/// Filename (under the autotuner `profile_path` dir) holding the active V/F
/// profile blob. Singular `vf_profile` — deliberately distinct from the
/// silicon-profile drop-in dir (`/etc/dcentrald/profiles.d`) and the
/// autotuner per-chip `*.json` profiles so it can't collide with either.
pub const VF_PROFILE_FILENAME: &str = "vf_profile_active.json";

fn vf_profile_path(dir: &str) -> PathBuf {
    Path::new(dir).join(VF_PROFILE_FILENAME)
}

/// Validate the uploaded bytes are a non-empty JSON object (a profile
/// envelope). The original bytes are returned to the caller unchanged on
/// success so the round trip stays byte-exact.
pub fn validate_vf_profile_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("empty body".to_string());
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(v) if v.is_object() => Ok(()),
        Ok(_) => Err("profile must be a JSON object".to_string()),
        Err(e) => Err(format!("invalid JSON: {e}")),
    }
}

/// Persist profile bytes verbatim, atomically, under `dir`. Creates `dir` if
/// it does not exist.
pub fn save_vf_profile_bytes(dir: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = vf_profile_path(dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::atomic_io::atomic_write_bytes(&path, bytes)
}

/// Load the saved profile bytes verbatim. `Ok(None)` when no profile is saved.
pub fn load_vf_profile_bytes(dir: &str) -> std::io::Result<Option<Vec<u8>>> {
    let path = vf_profile_path(dir);
    match std::fs::read(&path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Build the `/api/profile/{download,upload}` sub-router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/profile/download", get(download_profile))
        .route("/api/profile/upload", post(upload_profile))
}

async fn download_profile(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match load_vf_profile_bytes(&state.profile_path) {
        Ok(Some(bytes)) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            bytes,
        )
            .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "no_profile",
                "message": "No V/F profile saved yet — upload one first",
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "read_failed", "message": e.to_string()})),
        )
            .into_response(),
    }
}

async fn upload_profile(State(state): State<Arc<AppState>>, body: Bytes) -> impl IntoResponse {
    // CE-103/CE-121: fail-closed capability + mode guard, mirroring the silicon
    // profile mutation routes. Persisting a V/F profile is a config mutation, so
    // it requires the `ConfigRw` capability and is blocked in Home mode. The
    // paired `download_profile` (read) stays open. State is extracted before the
    // body-consuming `Bytes` so the guard runs before any deserialization.
    if let Err(resp) = crate::rest::require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/profile/upload",
    ) {
        return resp;
    }
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/profile/upload", mode) {
        return resp.into_response();
    }

    if let Err(msg) = validate_vf_profile_bytes(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_profile", "message": msg})),
        )
            .into_response();
    }
    match save_vf_profile_bytes(&state.profile_path, &body) {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({"status": "saved", "bytes": body.len()})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "write_failed", "message": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir under the OS temp dir (no external tempfile crate).
    fn scratch_dir(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "dcent_vf_profile_{}_{}_{}",
            tag,
            std::process::id(),
            nanos
        ));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn validate_rejects_empty_and_non_object() {
        assert!(validate_vf_profile_bytes(b"").is_err());
        assert!(validate_vf_profile_bytes(b"not json").is_err());
        assert!(validate_vf_profile_bytes(b"[1,2,3]").is_err()); // array, not object
        assert!(validate_vf_profile_bytes(b"123").is_err());
        assert!(validate_vf_profile_bytes(br#"{"freq_mhz":525}"#).is_ok());
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let dir = scratch_dir("rt");
        // Note: deliberately irregular whitespace so a parse→reserialize
        // implementation would change the bytes; verbatim storage must not.
        let original = br#"{ "schema": 1,
  "voltage_mv": 1320,   "freq_mhz": 525,
            "per_chip": [500, 505, 510] }"#;
        assert!(validate_vf_profile_bytes(original).is_ok());
        save_vf_profile_bytes(&dir, original).unwrap();
        let back = load_vf_profile_bytes(&dir)
            .unwrap()
            .expect("profile present");
        assert_eq!(
            back.as_slice(),
            original.as_slice(),
            "round-trip must be byte-identical"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_returns_none() {
        let dir = scratch_dir("missing");
        assert!(load_vf_profile_bytes(&dir).unwrap().is_none());
    }

    #[test]
    fn overwrite_replaces_previous_profile() {
        let dir = scratch_dir("overwrite");
        save_vf_profile_bytes(&dir, br#"{"v":1}"#).unwrap();
        save_vf_profile_bytes(&dir, br#"{"v":2}"#).unwrap();
        let back = load_vf_profile_bytes(&dir).unwrap().unwrap();
        assert_eq!(back.as_slice(), br#"{"v":2}"#.as_slice());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filename_is_singular_vf_profile() {
        assert_eq!(VF_PROFILE_FILENAME, "vf_profile_active.json");
        let p = vf_profile_path("/tmp/x");
        assert!(p.ends_with("vf_profile_active.json"));
    }
}
