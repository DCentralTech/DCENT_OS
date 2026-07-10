//! Profile-import REST endpoints (wave-8 W8-D, profile-infra Phase 3).
//!
//! Spec: `plans/wave4-profile-import-infrastructure.md` §E.
//!
//! State: `dcentrald_silicon_profiles::registry::global()` `RwLock`,
//! populated at boot from `/etc/dcentrald/profiles.d/`. The registry
//! is keyed by `(MinerModel, hashboard, ChipFamily)` — the same tuple
//! the autotuner uses for chip-aware preset lookup.
//!
//! Auth: existing dashboard cookie session (no new auth surface — the
//! axum router this module returns is merged into `rest::build_router()`
//! before the auth middleware layer is applied at the top level in
//! `lib.rs::start_api_servers`).
//!
//! Path inventory (W8-D resolved a path collision with the existing
//! autotuner-mode `GET/POST /api/profiles` handlers in `rest.rs` by
//! namespacing the new endpoints under `/api/profiles/silicon/*`):
//!
//! | Method | Path | Behavior |
//! |---|---|---|
//! | GET | `/api/profiles/silicon` | List every loaded profile bundle. Returns `[ProfileSummary]`. |
//! | GET | `/api/profiles/silicon/:id` | Single bundle detail. Returns full `ProfileBundle` JSON. |
//! | POST | `/api/profiles/silicon/import` | Multipart upload of a JSON profile bundle. Validate, write to `/etc/dcentrald/profiles.d/operator/`, reload. |
//! | PUT | `/api/profiles/silicon/active` | Set the active profile per (model, hashboard) tuple. |
//! | DELETE | `/api/profiles/silicon/:id` | Remove (LiveConfirmed → 403). |
//! | POST | `/api/profiles/silicon/reload` | Re-read disk dir. |
//!
//! `id` format: `<miner_model>__<hashboard>__<chip>__<source_class>`
//! using the serde snake_case spelling for each enum
//! (e.g. `antminer_s9__bhb-s9-generic__bm1387__live_confirmed`). The
//! double-underscore separator is collision-free given the schema
//! validation rules in `dcentrald_silicon_profiles::registry::validate`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Multipart, Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use dcent_schema::capability::RuntimeCapability;
use dcentrald_api_types::chip_init::ChipFamily;
use dcentrald_api_types::power_profile_preset::MinerModel;
use dcentrald_silicon_profiles::registry::{self as registry, global, ProfileBundle, ProfileKey};
use dcentrald_silicon_profiles::ProfileSource;

use crate::AppState;

/// Disk root for runtime profile files. Mirrors the path that
/// `dcentrald` boots from in `daemon.rs`. Operator-imported bundles
/// land under `<root>/operator/`; vendor + baked subdirs are left
/// alone.
const DEFAULT_PROFILE_DIR: &str = "/etc/dcentrald/profiles.d";
const OPERATOR_SUBDIR: &str = "operator";

/// Environment-variable override for the profile directory. Set by
/// integration tests to redirect the import + delete + reload
/// handlers at a temp directory. Not for production use — daemon
/// boot path always uses `DEFAULT_PROFILE_DIR`.
const PROFILE_DIR_ENV: &str = "DCENTRALD_PROFILE_DIR";

/// Resolve the active profile directory: env override takes
/// precedence over the compile-time default.
fn profile_dir() -> PathBuf {
    std::env::var(PROFILE_DIR_ENV)
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILE_DIR))
}

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

/// Compact summary used by `GET /api/profiles/silicon`. Mirrors the
/// shape promised in the wave-4 spec §E (`{id, model, hashboard, chip,
/// source_class, preset_count}`).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    pub id: String,
    /// snake_case serde rendering of `MinerModel` (e.g. `antminer_s9`).
    pub miner_model: String,
    pub hashboard: String,
    /// snake_case serde rendering of `ChipFamily` (e.g. `bm1387`).
    pub chip: String,
    /// snake_case serde rendering of `ProfileSource` (e.g. `live_confirmed`).
    pub source_class: String,
    pub preset_count: usize,
}

/// Aggregate result from `POST /api/profiles/silicon/reload`.
#[derive(Debug, Clone, Serialize)]
pub struct ReloadResult {
    pub loaded: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

/// Generic error envelope. Stays JSON-shape-stable across all
/// non-2xx responses so dashboard wizards can render a single
/// error-banner component.
#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub error: String,
}

/// Body for `PUT /api/profiles/silicon/active`. The endpoint
/// persists the chosen active profile id per (model, hashboard)
/// tuple — autotuner consume-time wiring is queued for wave 9.
#[derive(Debug, Clone, Deserialize)]
pub struct SetActiveBody {
    pub model: MinerModel,
    pub hashboard: String,
    pub profile_id: String,
}

/// Body for `POST /api/profiles/silicon/import` JSON path. The
/// endpoint also accepts multipart uploads (preferred for the
/// dashboard wizard) — see `import_profile`.
#[derive(Debug, Clone, Deserialize)]
pub struct ImportJsonBody {
    pub bundle: ProfileBundle,
}

/// Response from `POST /api/profiles/silicon/import`.
#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    pub id: String,
    pub path: String,
    pub loaded: usize,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the `<model>__<hashboard>__<chip>__<source_class>` id used
/// across the API surface. Uses the serde snake_case rendering of
/// each enum so the wire shape matches the on-disk JSON files in
/// `/etc/dcentrald/profiles.d/`.
pub(crate) fn build_profile_id(bundle: &ProfileBundle) -> String {
    format!(
        "{}__{}__{}__{}",
        serde_plain(&bundle.miner_model),
        bundle.hashboard,
        serde_plain(&bundle.chip),
        serde_plain(&bundle.source_class),
    )
}

/// Render an enum as its serde-snake_case wire name. Stripping the
/// surrounding quotes from `serde_json::to_string` produces a
/// terminal-friendly token. Returns `"unknown"` if the value cannot
/// be serialized — never panics.
fn serde_plain<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .ok()
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Decode a profile id back into `(MinerModel, hashboard, ChipFamily,
/// ProfileSource)`. Returns `None` for malformed ids so the caller
/// can map to a `404 NOT_FOUND` rather than `500`.
///
/// W13-A: made `pub(crate)` so the autotuner-runtime integration test
/// can build a profile id round-trip without re-implementing the
/// parser.
pub(crate) fn parse_profile_id(
    id: &str,
) -> Option<(MinerModel, String, ChipFamily, ProfileSource)> {
    let parts: Vec<&str> = id.split("__").collect();
    if parts.len() != 4 {
        return None;
    }
    let model = serde_json::from_str::<MinerModel>(&format!("\"{}\"", parts[0])).ok()?;
    let hashboard = parts[1].to_string();
    let chip = serde_json::from_str::<ChipFamily>(&format!("\"{}\"", parts[2])).ok()?;
    let source_class = serde_json::from_str::<ProfileSource>(&format!("\"{}\"", parts[3])).ok()?;
    Some((model, hashboard, chip, source_class))
}

/// Render a single bundle as its `ProfileSummary` shape.
fn summarize(bundle: &ProfileBundle) -> ProfileSummary {
    ProfileSummary {
        id: build_profile_id(bundle),
        miner_model: serde_plain(&bundle.miner_model),
        hashboard: bundle.hashboard.clone(),
        chip: serde_plain(&bundle.chip),
        source_class: serde_plain(&bundle.source_class),
        preset_count: bundle.presets.len(),
    }
}

/// Slugify a string for filename use: lowercase ASCII alphanumerics
/// + dashes, everything else collapses to `-`. Keeps file names
/// predictable + path-traversal-safe.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' {
            if !prev_dash && !out.is_empty() {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(mapped);
            prev_dash = false;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("operator");
    }
    out
}

/// Build the on-disk filename for an operator-imported bundle.
/// Avoids accidental collision across (model, hashboard, source_fw)
/// triples by including the firmware_version slug.
fn operator_filename(bundle: &ProfileBundle) -> String {
    format!(
        "{}-{}-{}.json",
        slugify(&serde_plain(&bundle.miner_model)),
        slugify(&bundle.hashboard),
        slugify(&bundle.source.firmware_version),
    )
}

/// Map a `ProfileLoadError` to a flat `String` for the
/// `ReloadResult.errors` field. Uses the `Display` impl so paths
/// and reasons stay together.
fn error_to_string<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

/// Build a uniform JSON error response.
fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/profiles/silicon` — list every loaded profile bundle.
///
/// Reads from the process-wide `ProfileRegistry`. Returns an empty
/// array (200 OK) when no profiles are loaded — never 5xx.
pub async fn list_profiles() -> Response {
    let reg = match global().read() {
        Ok(g) => g,
        Err(_) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned")
        }
    };
    let mut summaries: Vec<ProfileSummary> = reg
        .iter_keys()
        .filter_map(|key: &ProfileKey| reg.lookup_bundle(key.0, &key.1, key.2).map(summarize))
        .collect();
    // Stable order so the dashboard's table doesn't reshuffle on
    // every render.
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    Json(summaries).into_response()
}

/// `GET /api/profiles/silicon/:id` — full bundle detail.
pub async fn get_profile(AxumPath(id): AxumPath<String>) -> Response {
    let key = match parse_profile_id(&id) {
        Some(k) => k,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("profile id {} is malformed", id),
            )
        }
    };
    let reg = match global().read() {
        Ok(g) => g,
        Err(_) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned")
        }
    };
    match reg.lookup_bundle(key.0, &key.1, key.2) {
        Some(b) if b.source_class == key.3 => Json(b.clone()).into_response(),
        Some(_) | None => {
            error_response(StatusCode::NOT_FOUND, format!("profile {} not found", id))
        }
    }
}

/// `POST /api/profiles/silicon/import` — multipart upload of a JSON
/// profile bundle. Accepts a single field named `profile` containing
/// the JSON bytes. Validate via `registry::validate`, write to
/// `/etc/dcentrald/profiles.d/operator/<filename>.json`, reload the
/// registry, and return 201 with the assigned id.
///
/// Body shape: `multipart/form-data` with one part:
/// - `profile` — JSON bytes of a `ProfileBundle`
///
/// Hard-fails on:
/// - missing `profile` field → 400
/// - malformed JSON → 400
/// - `registry::validate` reject → 400 (catches SECURE_BOOT_SET +
///   Hashcore + voltage/freq out-of-range per spec §H)
/// - I/O failure writing to disk → 500
/// - reload failure → 500
pub async fn import_profile(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Response {
    // CE-103/CE-121: fail-closed capability + mode guard, mirroring the sibling
    // `POST /api/profiles` handler in `rest/late.rs`. Only a Beta-anchor identity
    // (BM1387, or BM1362 + am2-s19jpro-zynq) at Exact/High confidence grants
    // `ConfigRw`; everything else is refused before any disk write.
    if let Err(resp) = crate::rest::require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/profiles/silicon/import",
    ) {
        return resp;
    }
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/profiles/silicon/import", mode)
    {
        return resp.into_response();
    }

    let mut bundle_bytes: Option<Vec<u8>> = None;

    loop {
        let next = match multipart.next_field().await {
            Ok(field) => field,
            Err(e) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    format!("malformed multipart body: {}", e),
                );
            }
        };
        let Some(field) = next else {
            break;
        };
        if field.name() == Some("profile") {
            match field.bytes().await {
                Ok(b) => {
                    bundle_bytes = Some(b.to_vec());
                }
                Err(e) => {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read 'profile' field: {}", e),
                    );
                }
            }
        }
    }

    let bytes = match bundle_bytes {
        Some(b) => b,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "expected multipart field named 'profile' containing JSON bundle",
            )
        }
    };

    install_bundle(&bytes)
}

/// JSON-body twin of `import_profile`. Dashboards or `dcent` toolbox
/// callers that already have the bundle in-memory can skip the
/// multipart envelope. Body: `{"bundle": <ProfileBundle>}`.
pub async fn import_profile_json(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ImportJsonBody>,
) -> Response {
    // CE-103/CE-121: fail-closed capability + mode guard (see `import_profile`).
    if let Err(resp) = crate::rest::require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/profiles/silicon/import-json",
    ) {
        return resp;
    }
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/profiles/silicon/import-json", mode)
    {
        return resp.into_response();
    }

    // Re-serialize so the on-disk file is always canonical pretty
    // JSON, regardless of how the caller framed the request.
    let bytes = match serde_json::to_vec(&body.bundle) {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("failed to re-serialize bundle: {}", e),
            );
        }
    };
    install_bundle(&bytes)
}

/// Shared install path — parses, validates, writes, reloads.
fn install_bundle(bytes: &[u8]) -> Response {
    let bundle: ProfileBundle = match serde_json::from_slice(bytes) {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("malformed JSON bundle: {}", e),
            );
        }
    };

    // Spec §H — refuse SECURE_BOOT_SET / Hashcore / voltage / freq
    // out-of-range bundles before touching disk. `registry::validate`
    // pins both the safety blocklist + the per-chip voltage envelope.
    if let Err(reason) = registry::validate(&bundle) {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!("validation failed: {}", reason),
        );
    }

    let id = build_profile_id(&bundle);
    let root = profile_dir();
    let dir = root.join(OPERATOR_SUBDIR);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "failed to create operator profile dir {}: {}",
                dir.display(),
                e
            ),
        );
    }
    let filename = operator_filename(&bundle);
    let path = dir.join(filename);

    // Re-serialize as pretty JSON so on-disk diffs are reviewable.
    let pretty = match serde_json::to_vec_pretty(&bundle) {
        Ok(p) => p,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to serialize bundle: {}", e),
            );
        }
    };
    // CFG-5: write the imported silicon profile atomically (tempfile + fsync +
    // rename) for consistency with the rest of the persisted-state write path
    // and so a crash mid-write cannot leave a truncated/partial profile on disk.
    if let Err(e) = crate::atomic_io::atomic_write_bytes(&path, &pretty) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to write {}: {}", path.display(), e),
        );
    }

    // Reload the global registry from the configured profile dir.
    let loaded = match reload_registry(&root) {
        Ok(stats) => stats.loaded,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("write succeeded but reload failed: {}", e),
            );
        }
    };

    (
        StatusCode::CREATED,
        Json(ImportResult {
            id,
            path: path.to_string_lossy().to_string(),
            loaded,
        }),
    )
        .into_response()
}

/// `PUT /api/profiles/silicon/active` — set the active profile for
/// the (model, hashboard) tuple.
///
/// W13-A wiring (replaces the W8-D acknowledged-only behavior):
///   1. Parse + validate the profile id resolves on the requested
///      `(model, hashboard)` tuple (W8-D behavior preserved).
///   2. Record the selection in the silicon-profiles registry via
///      `set_active_profile_for_chain` so the autotuner can pull it
///      via `get_active_bundle_for_chain` on its next iteration. This
///      survives daemon restarts as long as the underlying JSON file
///      remains under `/etc/dcentrald/profiles.d/operator/`.
///   3. Forward the selection to the live autotuner via
///      `AppState::autotuner_command_tx`
///      (`AutoTunerCommand::ApplySiliconProfile`). The runtime ack is
///      surfaced under the `runtime` key in the response so the
///      dashboard wizard can show "applied live" vs "next cycle".
pub async fn set_active_profile(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetActiveBody>,
) -> Response {
    // CE-103/CE-121: fail-closed capability + mode guard. This route pushes the
    // active profile's live freq/voltage presets to the running autotuner, so it
    // requires the `AsicOptions` mutating capability (not just `ConfigRw`).
    if let Err(resp) = crate::rest::require_antminer_runtime_capability(
        &state,
        RuntimeCapability::AsicOptions,
        "/api/profiles/silicon/active",
    ) {
        return resp;
    }
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/profiles/silicon/active", mode)
    {
        return resp.into_response();
    }

    // Step 1: Verify the requested profile id resolves to a real
    // loaded bundle keyed by the requested (model, hashboard) tuple.
    let key = match parse_profile_id(&body.profile_id) {
        Some(k) => k,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                format!("malformed profile_id {}", body.profile_id),
            );
        }
    };
    if key.0 != body.model || key.1 != body.hashboard {
        return error_response(
            StatusCode::BAD_REQUEST,
            format!(
                "profile_id {} does not match requested (model={:?}, hashboard={})",
                body.profile_id, body.model, body.hashboard
            ),
        );
    }

    {
        let reg = match global().read() {
            Ok(g) => g,
            Err(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned");
            }
        };
        if reg.lookup_bundle(key.0, &key.1, key.2).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("profile {} not loaded", body.profile_id),
            );
        }
    }

    // Step 2: Record the operator's selection in the silicon-profiles
    // registry so the autotuner's next iteration can consume it via
    // `get_active_bundle_for_chain`. The registry rejects unknown
    // chains; we just verified the lookup above, so this should
    // always succeed.
    let registry_status = {
        let mut reg = match global().write() {
            Ok(g) => g,
            Err(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned");
            }
        };
        match reg.set_active_profile_for_chain(body.model, &body.hashboard, &body.profile_id) {
            Ok(()) => "registered",
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to register active selection: {}", e),
                );
            }
        }
    };

    // Step 3a (W15-A): resolve the active bundle's preset table so
    // the autotuner can derive per-chain freq/voltage targets at the
    // top of each iteration without re-reading the registry from
    // inside the autotuner crate. This avoids the workspace dep cycle
    // motivated by the W13-A `String` profile-id pattern. An empty
    // Vec is acceptable here — the autotuner falls back to its
    // previous freq/voltage targets when the table is empty.
    let presets: Vec<dcentrald_autotuner::SiliconPreset> = {
        let reg = match global().read() {
            Ok(g) => g,
            Err(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned");
            }
        };
        reg.get_active_bundle_for_chain(body.model, &body.hashboard)
            .map(|bundle| {
                bundle
                    .presets
                    .iter()
                    .map(|p| dcentrald_autotuner::SiliconPreset {
                        step: p.step,
                        freq_mhz: p.freq_mhz,
                        voltage_v: p.voltage_v,
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    // Step 3b: Notify the live autotuner. If the runtime channel is
    // unavailable (e.g. proxy mode, hybrid fallback, or the autotuner
    // hasn't started yet), the selection is still durable via the
    // registry above — the next-cycle path remains valid.
    let runtime = dispatch_silicon_profile_command(
        state.as_ref(),
        serde_plain(&body.model),
        body.hashboard.clone(),
        body.profile_id.clone(),
        presets,
    )
    .await;

    Json(serde_json::json!({
        "status": "ok",
        "model": serde_plain(&body.model),
        "hashboard": body.hashboard,
        "profile_id": body.profile_id,
        "registry": registry_status,
        "runtime": runtime,
        "note": "selection persisted; autotuner runtime wiring landed in W13-A",
    }))
    .into_response()
}

/// Forward an operator's silicon-profile selection to the live
/// autotuner via the existing
/// `AppState::autotuner_command_tx` mpsc channel. Returns a JSON
/// envelope describing the runtime acknowledgement (or its absence).
///
/// Mirrors the shape of `rest::dispatch_autotuner_mode_command` so
/// dashboard consumers see consistent envelopes for every autotuner
/// runtime hop.
async fn dispatch_silicon_profile_command(
    state: &AppState,
    miner_model_snake: String,
    hashboard: String,
    profile_id: String,
    presets: Vec<dcentrald_autotuner::SiliconPreset>,
) -> serde_json::Value {
    // W21 audit-coverage: operator silicon-profile selection (ApplySiliconProfile)
    // is the same operator-mutation class as an autotuner mode change — record it
    // with the AutotunerProfileSelect event. Only caller is the REST profile-set
    // handler (no internal loop), so no over-audit.
    crate::push_audit_event(
        state,
        "rest_dashboard",
        dcentrald_api_types::audit_log::AuditEvent::AutotunerProfileSelect {
            profile_name: format!("silicon:{miner_model_snake}/{hashboard}/{profile_id}"),
        },
    );

    let Some(tx) = &state.autotuner_command_tx else {
        return serde_json::json!({
            "channel_available": false,
            "accepted": false,
            "applied_runtime": false,
            "status": "unavailable",
            "message": "live autotuner command channel is not available in this runtime",
        });
    };

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let command = dcentrald_autotuner::AutoTunerCommand::ApplySiliconProfile {
        miner_model: miner_model_snake,
        hashboard,
        profile_id,
        presets,
        ack_tx,
    };
    if tx.send(command).await.is_err() {
        return serde_json::json!({
            "channel_available": false,
            "accepted": false,
            "applied_runtime": false,
            "status": "closed",
            "message": "live autotuner command channel is closed",
        });
    }

    match tokio::time::timeout(std::time::Duration::from_secs(2), ack_rx).await {
        Ok(Ok(result)) => serde_json::json!({
            "channel_available": true,
            "accepted": true,
            "applied_runtime": result.applied_runtime,
            "status": result.status,
            "profile_id": result.profile_id,
            "miner_model": result.miner_model,
            "hashboard": result.hashboard,
            "message": result.message,
        }),
        Ok(Err(_)) => serde_json::json!({
            "channel_available": false,
            "accepted": false,
            "applied_runtime": false,
            "status": "closed_before_ack",
            "message": "live autotuner command channel closed before acknowledgement",
        }),
        Err(_) => serde_json::json!({
            "channel_available": true,
            "accepted": true,
            "applied_runtime": false,
            "status": "ack_timeout",
            "message": "live autotuner command was sent but no acknowledgement arrived within 2s",
        }),
    }
}

/// `DELETE /api/profiles/silicon/:id` — remove a profile bundle from
/// disk + registry.
///
/// Spec §E: `LiveConfirmed` profiles are immutable via API and
/// return 403 with `{error: "live_confirmed_immutable"}` so an
/// operator can't accidentally clobber the high-authority baked
/// rows that survived live testing on real S9 / S19j Pro / S21
/// hardware.
pub async fn delete_profile(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    // CE-103/CE-121: fail-closed capability + mode guard (see `import_profile`).
    if let Err(resp) = crate::rest::require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/profiles/silicon/:id",
    ) {
        return resp;
    }
    let mode = *state.mode_rx.borrow();
    if let Err(resp) = crate::mode_middleware::check_mode_access("/api/profiles/silicon/:id", mode) {
        return resp.into_response();
    }

    let key = match parse_profile_id(&id) {
        Some(k) => k,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("profile id {} is malformed", id),
            );
        }
    };

    // Resolve the bundle + on-disk path before mutating anything.
    let (bundle_clone, source_class) = {
        let reg = match global().read() {
            Ok(g) => g,
            Err(_) => {
                return error_response(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned");
            }
        };
        match reg.lookup_bundle(key.0, &key.1, key.2) {
            Some(b) if b.source_class == key.3 => (b.clone(), b.source_class),
            Some(_) | None => {
                return error_response(StatusCode::NOT_FOUND, format!("profile {} not found", id));
            }
        }
    };

    if matches!(source_class, ProfileSource::LiveConfirmed) {
        // Per spec §E — LiveConfirmed bundles are immutable via API.
        return error_response(StatusCode::FORBIDDEN, "live_confirmed_immutable");
    }

    // Locate the on-disk file. The simplest portable approach is to
    // scan the operator subdir for a file whose contents match the
    // requested id; failing that, fall back to the canonical filename
    // pattern.
    let root = profile_dir();
    let dir = root.join(OPERATOR_SUBDIR);
    let canonical = dir.join(operator_filename(&bundle_clone));
    let mut deleted_path: Option<PathBuf> = None;
    if canonical.exists() {
        if let Err(e) = std::fs::remove_file(&canonical) {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to remove {}: {}", canonical.display(), e),
            );
        }
        deleted_path = Some(canonical);
    } else if dir.exists() {
        // Fallback: scan operator/ for a file whose JSON parses to
        // the same (model, hashboard, chip, source_class) tuple.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let candidate: ProfileBundle = match serde_json::from_slice(&bytes) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                if candidate.miner_model == key.0
                    && candidate.hashboard == key.1
                    && candidate.chip == key.2
                    && candidate.source_class == key.3
                {
                    if let Err(e) = std::fs::remove_file(&path) {
                        return error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to remove {}: {}", path.display(), e),
                        );
                    }
                    deleted_path = Some(path);
                    break;
                }
            }
        }
    }

    let deleted_path = match deleted_path {
        Some(p) => p,
        None => {
            // Bundle is loaded in-registry but no operator file
            // matches — likely a baked / vendor file the operator
            // can't delete via the operator subdir. Refuse cleanly.
            return error_response(
                StatusCode::FORBIDDEN,
                "profile is not in the operator subdirectory; refuse to delete baked or vendor files",
            );
        }
    };

    // Reload so the in-memory registry drops the deleted entry.
    let _ = reload_registry(&root);

    Json(serde_json::json!({
        "deleted": id,
        "path": deleted_path.to_string_lossy(),
    }))
    .into_response()
}

/// `POST /api/profiles/silicon/reload` — re-read the on-disk profile
/// directory.
///
/// Returns `{loaded, skipped, errors[]}`. Useful after an operator
/// drops a JSON file directly onto the miner via SSH/SFTP without
/// going through the import endpoint.
pub async fn reload_profiles(State(state): State<Arc<AppState>>) -> Response {
    // CE-103/CE-121: fail-closed capability + mode guard (see `import_profile`).
    if let Err(resp) = crate::rest::require_antminer_runtime_capability(
        &state,
        RuntimeCapability::ConfigRw,
        "/api/profiles/silicon/reload",
    ) {
        return resp;
    }
    let mode = *state.mode_rx.borrow();
    if let Err(resp) =
        crate::mode_middleware::check_mode_access("/api/profiles/silicon/reload", mode)
    {
        return resp.into_response();
    }

    match reload_registry(&profile_dir()) {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("reload failed: {}", e),
        ),
    }
}

/// Acquire the registry write lock and call `reload`. Bundled so the
/// import + delete handlers can refresh the in-memory registry after
/// a disk write.
fn reload_registry(dir: &Path) -> Result<ReloadResult, String> {
    let mut reg = global()
        .write()
        .map_err(|_| "registry lock poisoned".to_string())?;
    let stats = reg
        .reload(dir)
        .map_err(|e| format!("registry reload error: {}", e))?;
    Ok(ReloadResult {
        loaded: stats.loaded,
        skipped: stats.skipped,
        errors: stats.errors.iter().map(error_to_string).collect(),
    })
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Router for the silicon-profile endpoints. Merged into
/// `rest::build_router()` at the top level.
///
/// Note: the read handlers (`list_profiles` / `get_profile`) do not take
/// `State<Arc<AppState>>` because the registry lives in a process-global
/// `OnceLock<RwLock<...>>` per `dcentrald_silicon_profiles::registry::global`.
/// The mutation handlers (`import` / `import-json` / `active` / delete /
/// `reload`) DO take `State` so the CE-103/CE-121 capability + mode guards can
/// read the live hardware identity and operating mode. The `Router` is typed
/// `Router<Arc<AppState>>` so it merges cleanly with the rest of the API surface.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/profiles/silicon", get(list_profiles))
        .route("/api/profiles/silicon/import", post(import_profile))
        .route(
            "/api/profiles/silicon/import-json",
            post(import_profile_json),
        )
        .route("/api/profiles/silicon/reload", post(reload_profiles))
        .route("/api/profiles/silicon/active", put(set_active_profile))
        .route(
            "/api/profiles/silicon/:id",
            get(get_profile).delete(delete_profile),
        )
}

// ---------------------------------------------------------------------------
// Unit tests for the pure helpers (no axum / no disk).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_silicon_profiles::registry::{ProfileMetadata, ProfileSourceMetadata};
    use dcentrald_silicon_profiles::Profile;

    fn sample_bundle() -> ProfileBundle {
        ProfileBundle {
            schema_version: 1,
            miner_model: MinerModel::AntminerS9,
            hashboard: "BHB-S9-generic".to_string(),
            chip: ChipFamily::Bm1387,
            source: ProfileSourceMetadata {
                vendor: "test".into(),
                firmware_version: "0.1.0".into(),
                extracted_from_sha256: "0".repeat(64),
                extraction_date: "2026-05-04".into(),
                extracted_by: Some("w8-d-test".into()),
            },
            source_class: ProfileSource::OperatorConfirmed,
            presets: vec![Profile {
                step: 0,
                freq_mhz: 500,
                voltage_v: 9.0,
                wall_watts: Some(900),
                hashrate_ths: Some(10.0),
                source: ProfileSource::OperatorConfirmed,
            }],
            metadata: ProfileMetadata::default(),
        }
    }

    #[test]
    fn build_id_round_trips_through_parse() {
        let bundle = sample_bundle();
        let id = build_profile_id(&bundle);
        // Expected: `<model>__<hashboard>__<chip>__<source_class>`
        assert_eq!(
            id,
            "antminer_s9__BHB-S9-generic__bm1387__operator_confirmed"
        );
        let parsed = parse_profile_id(&id).expect("parse");
        assert_eq!(parsed.0, MinerModel::AntminerS9);
        assert_eq!(parsed.1, "BHB-S9-generic");
        assert_eq!(parsed.2, ChipFamily::Bm1387);
        assert_eq!(parsed.3, ProfileSource::OperatorConfirmed);
    }

    #[test]
    fn parse_id_rejects_malformed() {
        assert!(parse_profile_id("garbage").is_none());
        assert!(parse_profile_id("a__b__c").is_none());
        assert!(parse_profile_id("not_a_model__BHB__bm1387__live_confirmed").is_none());
    }

    #[test]
    fn slugify_collapses_punctuation() {
        assert_eq!(slugify("BHB-S9 generic"), "bhb-s9-generic");
        assert_eq!(slugify("foo___bar"), "foo-bar");
        assert_eq!(slugify("---"), "operator");
    }

    #[test]
    fn operator_filename_is_path_safe() {
        let bundle = sample_bundle();
        let name = operator_filename(&bundle);
        assert!(name.ends_with(".json"));
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
    }

    #[test]
    fn summarize_uses_snake_case_serde_names() {
        let bundle = sample_bundle();
        let summary = summarize(&bundle);
        assert_eq!(summary.miner_model, "antminer_s9");
        assert_eq!(summary.chip, "bm1387");
        assert_eq!(summary.source_class, "operator_confirmed");
        assert_eq!(summary.preset_count, 1);
    }
}
