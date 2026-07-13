//! Canonical REST error envelopes and response normalization.
//!
//! Endpoint handlers produce structured errors directly where practical. The
//! response middleware also upgrades legacy JSON and plain-text errors into the
//! same envelope without changing successful or non-text responses.

use std::io;

use axum::body::{to_bytes, Body};
use axum::extract::Json;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::atomic_io::{storage_write_failure_kind, StorageWriteFailureKind};

pub(super) const ERROR_ENVELOPE_BODY_LIMIT: usize = 64 * 1024;

pub(super) fn api_error(
    status: StatusCode,
    code: impl Into<String>,
    error: impl Into<String>,
    suggestion: Option<&str>,
) -> Response {
    let mut body = dcentrald_api_types::ApiErrorBody::new(error).with_code(code);
    if let Some(suggestion) = suggestion {
        body = body.with_suggestion(suggestion);
    }
    (status, Json(body)).into_response()
}

pub(super) fn pool_validation_error(message: impl Into<String>) -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        dcentrald_api_types::api_error_codes::POOL_VALIDATION,
        message,
        Some("Check the pool URL format, worker name, and failover split settings."),
    )
}

#[derive(Debug)]
pub(super) enum ConfigPersistenceError {
    BadRequest(String),
    Storage {
        kind: StorageWriteFailureKind,
        detail: String,
    },
}

impl ConfigPersistenceError {
    pub(super) fn bad_request(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub(super) fn from_io(action: &'static str, error: io::Error) -> Self {
        if let Some(kind) = storage_write_failure_kind(&error) {
            return Self::Storage {
                kind,
                detail: format!("{action}: {error}"),
            };
        }
        Self::BadRequest(format!("{action}: {error}"))
    }

    pub(super) fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(
                    dcentrald_api_types::ApiErrorBody::new(message)
                        .with_code(dcentrald_api_types::api_error_codes::CONFIG_VALIDATION)
                        .with_suggestion(
                            "Check the submitted config fields and retry with supported values.",
                        ),
                ),
            )
                .into_response(),
            Self::Storage { kind, detail } => {
                let (message, suggestion) = match kind {
                    StorageWriteFailureKind::StorageFull => (
                        "Persistent storage is full; configuration was not saved.",
                        "Free space under /data, rotate logs, or move snapshots off-device, then retry.",
                    ),
                    StorageWriteFailureKind::ReadOnly => (
                        "Persistent storage is read-only; configuration was not saved.",
                        "Check the /data mount state and filesystem health, then retry after it is writable.",
                    ),
                };
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(
                        dcentrald_api_types::ApiErrorBody::new(message)
                            .with_code(kind.code())
                            .with_detail(detail)
                            .with_suggestion(suggestion),
                    ),
                )
                    .into_response()
            }
        }
    }
}

fn is_json_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}

fn is_text_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("text/plain")
}

fn error_body_bytes(body: dcentrald_api_types::ApiErrorBody) -> Vec<u8> {
    serde_json::to_vec(&body).unwrap_or_else(|_| br#"{"error":"Request failed"}"#.to_vec())
}

fn envelope_bytes(message: String) -> Vec<u8> {
    error_body_bytes(
        dcentrald_api_types::ApiErrorBody::new(message)
            .with_code(dcentrald_api_types::api_error_codes::UNCLASSIFIED_ERROR),
    )
}

fn legacy_error_body_from_json(
    value: &serde_json::Value,
) -> Option<dcentrald_api_types::ApiErrorBody> {
    let object = value.as_object()?;
    let status = object.get("status")?.as_str()?;
    if !status.eq_ignore_ascii_case("error") {
        return None;
    }
    let message = object.get("message")?.as_str()?.trim();
    if message.is_empty() {
        return None;
    }

    let mut body = dcentrald_api_types::ApiErrorBody::new(message.to_string())
        .with_code(dcentrald_api_types::api_error_codes::LEGACY_ERROR);
    if let Some(detail) = object
        .get("detail")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body = body.with_detail(detail.to_string());
    }
    if let Some(code) = object
        .get("code")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body = body.with_code(code.to_string());
    }
    if let Some(suggestion) = object
        .get("suggestion")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body = body.with_suggestion(suggestion.to_string());
    }
    Some(body)
}

fn legacy_error_envelope_bytes(value: &serde_json::Value) -> Option<Vec<u8>> {
    legacy_error_body_from_json(value).map(error_body_bytes)
}

pub(super) async fn normalize_api_error_response(response: Response) -> Response {
    if response.status().is_success() {
        return response;
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let is_text = is_text_content_type(&content_type);
    let is_json = is_json_content_type(&content_type);
    if !is_text && !is_json {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, ERROR_ENVELOPE_BODY_LIMIT).await else {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            dcentrald_api_types::api_error_codes::ERROR_BODY_UNAVAILABLE,
            "The daemon could not normalize the error response body.",
            None,
        );
    };

    let envelope = if is_text {
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        (!text.is_empty()).then(|| envelope_bytes(text))
    } else {
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::String(message)) if !message.trim().is_empty() => {
                Some(envelope_bytes(message.trim().to_string()))
            }
            Ok(value) => legacy_error_envelope_bytes(&value),
            _ => None,
        }
    };

    if let Some(envelope) = envelope {
        parts.headers.insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        return Response::from_parts(parts, Body::from(envelope));
    }

    Response::from_parts(parts, Body::from(bytes))
}
