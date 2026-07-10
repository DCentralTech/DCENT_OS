//!  luxos-C — LuxOS / CGMiner JSON response envelope (HAL-free).
//!
//! Source RE evidence:
//!
//! §3.2-§3.4 + the cross-firmware CGMiner status code map shared with
//! Bitmain stock and VNish.
//!
//! Every LuxOS REST response (port 8080 AND TCP 4028) is a JSON object
//! whose top-level keys are command names. Each value is an array
//! containing one `STATUS` object plus optional payload entries:
//!
//! ```json
//! {
//!   "STATUS": [{"STATUS":"S","Code":316,"Msg":"Session created","Description":"...","When":1777490333}],
//!   "SESSION": [{"SessionID":"ohYGGQcj"}],
//!   "id": 1
//! }
//! ```
//!
//! `STATUS` field semantics:
//! - `STATUS=S` (success), `STATUS=I` (info), `STATUS=W` (warning),
//!   `STATUS=E` (error), `STATUS=F` (fatal).
//! - `Code` is a numeric status code; the canonical map is shared with
//!   the CGMiner upstream (`08-cgminer-api.txt`).
//! - `When` is a Unix timestamp.

use serde::{Deserialize, Serialize};

/// Severity letter at `STATUS.STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LuxosStatusSeverity {
    /// `S` — Success.
    #[serde(rename = "S")]
    Success,
    /// `I` — Informational.
    #[serde(rename = "I")]
    Info,
    /// `W` — Warning.
    #[serde(rename = "W")]
    Warning,
    /// `E` — Error.
    #[serde(rename = "E")]
    Error,
    /// `F` — Fatal.
    #[serde(rename = "F")]
    Fatal,
}

impl LuxosStatusSeverity {
    /// True iff this severity indicates the request was accepted.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success | Self::Info)
    }

    /// True iff this severity indicates the request failed.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error | Self::Fatal)
    }
}

// ---------------------------------------------------------------------------
// Status code constants (verified against E-rest-api-8080.md §3.4 and
// luxminer.strings).
// ---------------------------------------------------------------------------

/// Status codes called out by name in E-rest-api-8080.md §3.4.
pub mod codes {
    /// Session created (response to `logon`). Carries a SessionID.
    pub const SESSION_CREATED: u16 = 316;
    /// Session terminated (`kill`).
    pub const KILL_SESSION: u16 = 318;
    /// Session information (`session`).
    pub const SESSION_INFO: u16 = 319;
    /// Profiles list (`profiles`).
    pub const PROFILES_LIST: u16 = 323;
    /// Pool group list (`groups`).
    pub const GROUPS_LIST: u16 = 324;
    /// Miner events stream (`events`).
    pub const MINER_EVENTS: u16 = 357;
    /// Missing parameter (LuxOS extension to CGMiner).
    pub const MISSING_PARAMETER: u16 = 400;
}

/// Top-level `STATUS` block in every LuxOS reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LuxosStatus {
    /// Severity letter (`S`/`I`/`W`/`E`/`F`).
    #[serde(rename = "STATUS")]
    pub severity: LuxosStatusSeverity,
    /// Numeric status code.
    #[serde(rename = "Code")]
    pub code: u16,
    /// Short human-readable message.
    #[serde(rename = "Msg")]
    pub msg: String,
    /// Long-form description (typically firmware build identifier).
    #[serde(rename = "Description")]
    pub description: String,
    /// Unix timestamp the response was generated.
    #[serde(rename = "When")]
    pub when: u64,
}

impl LuxosStatus {
    /// True iff the response indicates success.
    pub fn is_success(&self) -> bool {
        self.severity.is_success()
    }

    /// True iff the response is the "missing parameter" error (code 400).
    pub fn is_missing_parameter(&self) -> bool {
        self.severity.is_error() && self.code == codes::MISSING_PARAMETER
    }
}

/// HTTP-level codes the LuxOS server returns at the wire layer
/// (independent of the in-body `Code`). Per E-rest-api-8080.md §7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosHttpStatus {
    /// 200 OK
    Ok,
    /// 400 Bad Request (malformed JSON)
    BadRequest,
    /// 401 Unauthorized (HTTP password set + missing/wrong)
    Unauthorized,
    /// 403 Forbidden
    Forbidden,
    /// 404 Not Found
    NotFound,
    /// 500 Internal Server Error
    InternalServerError,
    /// 503 Service Unavailable (HTTP API shutting down)
    ServiceUnavailable,
}

impl LuxosHttpStatus {
    /// Map an HTTP code to its enum form. Returns `None` for codes not
    /// observed in LuxOS responses.
    pub fn from_http_code(code: u16) -> Option<Self> {
        Some(match code {
            200 => Self::Ok,
            400 => Self::BadRequest,
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            500 => Self::InternalServerError,
            503 => Self::ServiceUnavailable,
            _ => return None,
        })
    }

    /// Numeric code.
    pub fn http_code(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::InternalServerError => 500,
            Self::ServiceUnavailable => 503,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_severity_serializes_to_single_letter() {
        // The wire form is the SINGLE letter — pinned because LuxOS
        // and CGMiner clients (pyasic, dcent-toolbox) decode the letter
        // directly.
        assert_eq!(
            serde_json::to_string(&LuxosStatusSeverity::Success).unwrap(),
            "\"S\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosStatusSeverity::Info).unwrap(),
            "\"I\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosStatusSeverity::Warning).unwrap(),
            "\"W\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosStatusSeverity::Error).unwrap(),
            "\"E\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosStatusSeverity::Fatal).unwrap(),
            "\"F\""
        );
    }

    #[test]
    fn status_severity_round_trips_through_serde() {
        for sev in [
            LuxosStatusSeverity::Success,
            LuxosStatusSeverity::Info,
            LuxosStatusSeverity::Warning,
            LuxosStatusSeverity::Error,
            LuxosStatusSeverity::Fatal,
        ] {
            let json = serde_json::to_string(&sev).unwrap();
            let back: LuxosStatusSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(sev, back);
        }
    }

    #[test]
    fn success_severities_classified_correctly() {
        assert!(LuxosStatusSeverity::Success.is_success());
        assert!(LuxosStatusSeverity::Info.is_success());
        assert!(!LuxosStatusSeverity::Warning.is_success());
        assert!(!LuxosStatusSeverity::Error.is_success());
        assert!(!LuxosStatusSeverity::Fatal.is_success());
    }

    #[test]
    fn error_severities_classified_correctly() {
        assert!(!LuxosStatusSeverity::Success.is_error());
        assert!(!LuxosStatusSeverity::Info.is_error());
        assert!(!LuxosStatusSeverity::Warning.is_error());
        assert!(LuxosStatusSeverity::Error.is_error());
        assert!(LuxosStatusSeverity::Fatal.is_error());
    }

    #[test]
    fn status_code_constants_pinned() {
        // Pin every named code from the RE doc. A silent renumbering
        // here would break pyasic + dashboard parsing.
        assert_eq!(codes::SESSION_CREATED, 316);
        assert_eq!(codes::KILL_SESSION, 318);
        assert_eq!(codes::SESSION_INFO, 319);
        assert_eq!(codes::PROFILES_LIST, 323);
        assert_eq!(codes::GROUPS_LIST, 324);
        assert_eq!(codes::MINER_EVENTS, 357);
        assert_eq!(codes::MISSING_PARAMETER, 400);
    }

    #[test]
    fn status_serializes_to_capitalised_field_names() {
        // CGMiner-compat envelopes use UPPER-FIRST field names for
        // STATUS/Code/Msg/Description/When. Pin so a refactor doesn't
        // accidentally lowercase them.
        let status = LuxosStatus {
            severity: LuxosStatusSeverity::Success,
            code: codes::SESSION_CREATED,
            msg: "Session created".to_string(),
            description: "LUXminer 2026.4.3.192353-6ab4e5077".to_string(),
            when: 1_777_490_333,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert!(json.get("STATUS").is_some());
        assert!(json.get("Code").is_some());
        assert!(json.get("Msg").is_some());
        assert!(json.get("Description").is_some());
        assert!(json.get("When").is_some());
        // Negative pins: lowercase forms must NOT appear.
        assert!(json.get("status").is_none());
        assert!(json.get("code").is_none());
        assert!(json.get("msg").is_none());
    }

    #[test]
    fn status_decodes_canonical_session_created_envelope() {
        // RE doc §3.2 verbatim STATUS object.
        let raw = r#"{"STATUS":"S","Code":316,"Msg":"Session created","Description":"LUXminer","When":1}"#;
        let s: LuxosStatus = serde_json::from_str(raw).unwrap();
        assert_eq!(s.severity, LuxosStatusSeverity::Success);
        assert_eq!(s.code, codes::SESSION_CREATED);
        assert_eq!(s.msg, "Session created");
        assert!(s.is_success());
        assert!(!s.is_missing_parameter());
    }

    #[test]
    fn status_classifies_missing_parameter_error() {
        let s = LuxosStatus {
            severity: LuxosStatusSeverity::Error,
            code: codes::MISSING_PARAMETER,
            msg: "Missing parameter: session".to_string(),
            description: "".to_string(),
            when: 0,
        };
        assert!(!s.is_success());
        assert!(s.is_missing_parameter());
    }

    #[test]
    fn status_does_not_misclassify_success_as_missing_parameter() {
        let s = LuxosStatus {
            severity: LuxosStatusSeverity::Success,
            code: codes::MISSING_PARAMETER, // Some other code accidentally =400
            msg: "ok".to_string(),
            description: "".to_string(),
            when: 0,
        };
        // `is_missing_parameter` requires BOTH the error severity AND
        // code 400 — pin that conjunction.
        assert!(!s.is_missing_parameter());
    }

    #[test]
    fn http_status_round_trips_through_codes() {
        for status in [
            LuxosHttpStatus::Ok,
            LuxosHttpStatus::BadRequest,
            LuxosHttpStatus::Unauthorized,
            LuxosHttpStatus::Forbidden,
            LuxosHttpStatus::NotFound,
            LuxosHttpStatus::InternalServerError,
            LuxosHttpStatus::ServiceUnavailable,
        ] {
            let code = status.http_code();
            assert_eq!(LuxosHttpStatus::from_http_code(code), Some(status));
        }
    }

    #[test]
    fn http_status_unknown_code_returns_none() {
        // 418 (I'm a teapot) is not observed in LuxOS responses.
        assert!(LuxosHttpStatus::from_http_code(418).is_none());
        assert!(LuxosHttpStatus::from_http_code(0).is_none());
    }

    #[test]
    fn http_status_codes_match_re_doc() {
        // RE doc §7: 200, 400, 401, 403, 404, 500, 503.
        assert_eq!(LuxosHttpStatus::Ok.http_code(), 200);
        assert_eq!(LuxosHttpStatus::BadRequest.http_code(), 400);
        assert_eq!(LuxosHttpStatus::Unauthorized.http_code(), 401);
        assert_eq!(LuxosHttpStatus::Forbidden.http_code(), 403);
        assert_eq!(LuxosHttpStatus::NotFound.http_code(), 404);
        assert_eq!(LuxosHttpStatus::InternalServerError.http_code(), 500);
        assert_eq!(LuxosHttpStatus::ServiceUnavailable.http_code(), 503);
    }
}
