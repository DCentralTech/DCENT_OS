//!  luxos-D — LuxOS REST error vocabulary catalog (HAL-free).
//!
//! Source RE evidence:
//! - `luxos/79-live-2026-04-29/analysis/E-rest-api-8080.md` §7
//!   (binary-string-decoded error vocabulary).
//! - `luxos/79-live-2026-04-29/captures/09-authenticated-api.txt`
//!   (live-observed error responses on `a lab unit`).
//!
//! The LuxOS HTTP server (`:8080`, axum + tower-http on a Tokio runtime
//! shared with the mining loop) returns a fixed set of error strings.
//! This module classifies them so dcent-toolbox + the dashboard can
//! present operator-friendly messages instead of opaque "Internal
//! Error: 12" blobs.
//!
//! Two surfaces:
//! 1. **Binary-decoded error categories** (E-rest-api-8080.md §7) —
//!    static vocabulary baked into `luxminer`.
//! 2. **Runtime CGMiner-style error codes** (09-authenticated-api.txt) —
//!    Code=14 "Invalid command", Code=401 "Invalid <param> value",
//!    Code=408 "Invalid field 'session_id'".

use serde::{Deserialize, Serialize};

/// Static error-vocabulary class decoded from luxminer binary strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosErrorClass {
    /// "OS Error: <code>" — std::io::Error wrapper.
    OsError,
    /// "Internal Error: <code>" — luxminer-internal failure.
    InternalError,
    /// "Unknown Error: <code>" — fall-through unclassified failure.
    UnknownError,
    /// "Failed to deserialize the JSON body into the target type: ..."
    JsonDeserialize,
    /// "Failed to parse the request body as JSON: ..."
    JsonParse,
    /// "Missing request extension: ..."
    MissingExtension,
    /// "Expected request with `Content-Type: application/json`"
    ContentTypeMissing,
    /// "file not found"
    FileNotFound,
    /// "error reading file contents"
    FileRead,
    /// "Failed to fetch persistent log entry: <name>"
    PersistentLogFetch,
    /// "Failed to fetch persistent file: <name>"
    PersistentFileFetch,
    /// "error reading file upload content"
    UploadRead,
    /// "Internal error when downloading profile"
    ProfileDownload,
    /// "HTTP API shutting down"
    ApiShutdown,
}

impl LuxosErrorClass {
    /// Returns the canonical message prefix to look for. The wire form
    /// often appends a code or filename; consumers can use this with
    /// `starts_with` or substring matching.
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::OsError => "OS Error:",
            Self::InternalError => "Internal Error:",
            Self::UnknownError => "Unknown Error:",
            Self::JsonDeserialize => "Failed to deserialize the JSON body",
            Self::JsonParse => "Failed to parse the request body as JSON",
            Self::MissingExtension => "Missing request extension:",
            Self::ContentTypeMissing => "Expected request with `Content-Type: application/json`",
            Self::FileNotFound => "file not found",
            Self::FileRead => "error reading file contents",
            Self::PersistentLogFetch => "Failed to fetch persistent log entry:",
            Self::PersistentFileFetch => "Failed to fetch persistent file:",
            Self::UploadRead => "error reading file upload content",
            Self::ProfileDownload => "Internal error when downloading profile",
            Self::ApiShutdown => "HTTP API shutting down",
        }
    }
}

/// Classify an observed error message into one of the documented
/// vocabulary buckets. Returns `None` if the message doesn't match any
/// known prefix.
pub fn classify(message: &str) -> Option<LuxosErrorClass> {
    // Order matters: more-specific prefixes must be tried before more-
    // generic ones. "Failed to fetch persistent log entry:" must match
    // before "Failed to fetch persistent file:".
    [
        LuxosErrorClass::PersistentLogFetch,
        LuxosErrorClass::PersistentFileFetch,
        LuxosErrorClass::JsonDeserialize,
        LuxosErrorClass::JsonParse,
        LuxosErrorClass::MissingExtension,
        LuxosErrorClass::ContentTypeMissing,
        LuxosErrorClass::ProfileDownload,
        LuxosErrorClass::UploadRead,
        LuxosErrorClass::FileNotFound,
        LuxosErrorClass::FileRead,
        LuxosErrorClass::ApiShutdown,
        LuxosErrorClass::OsError,
        LuxosErrorClass::InternalError,
        LuxosErrorClass::UnknownError,
    ]
    .into_iter()
    .find(|class| message.starts_with(class.prefix()) || message.contains(class.prefix()))
}

/// All documented error classes in stable order.
pub const ALL_ERROR_CLASSES: &[LuxosErrorClass] = &[
    LuxosErrorClass::OsError,
    LuxosErrorClass::InternalError,
    LuxosErrorClass::UnknownError,
    LuxosErrorClass::JsonDeserialize,
    LuxosErrorClass::JsonParse,
    LuxosErrorClass::MissingExtension,
    LuxosErrorClass::ContentTypeMissing,
    LuxosErrorClass::FileNotFound,
    LuxosErrorClass::FileRead,
    LuxosErrorClass::PersistentLogFetch,
    LuxosErrorClass::PersistentFileFetch,
    LuxosErrorClass::UploadRead,
    LuxosErrorClass::ProfileDownload,
    LuxosErrorClass::ApiShutdown,
];

// ---------------------------------------------------------------------------
// Runtime CGMiner-style errors observed in 09-authenticated-api.txt
// ---------------------------------------------------------------------------

/// CGMiner-style runtime error class triggered by `?command=…`
/// dispatch. Code numbers are pinned against the live capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosRuntimeError {
    /// `Code=14, Msg=Invalid command` — command name not recognized.
    InvalidCommand,
    /// `Code=401, Msg=Invalid <param> value` — read-protected request
    /// with a bad/wrong param format.
    InvalidParamValue,
    /// `Code=408, Msg=Invalid field 'session_id'` — `session_id` was
    /// supplied where it's not allowed (e.g. `metrics` command).
    InvalidSessionField,
}

impl LuxosRuntimeError {
    /// Numeric `Code=` field that signals this error class.
    pub fn code(&self) -> u16 {
        match self {
            Self::InvalidCommand => 14,
            Self::InvalidParamValue => 401,
            Self::InvalidSessionField => 408,
        }
    }

    /// Canonical `Msg=` text body (substring match).
    pub fn msg_substr(&self) -> &'static str {
        match self {
            Self::InvalidCommand => "Invalid command",
            Self::InvalidParamValue => "Invalid",
            Self::InvalidSessionField => "Invalid field 'session_id'",
        }
    }

    /// Look up the runtime error by its numeric `Code=` field.
    pub fn from_code(code: u16) -> Option<Self> {
        match code {
            14 => Some(Self::InvalidCommand),
            401 => Some(Self::InvalidParamValue),
            408 => Some(Self::InvalidSessionField),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_class_prefixes_match_re_doc() {
        // Every documented prefix from E-rest-api-8080.md §7.
        assert_eq!(LuxosErrorClass::OsError.prefix(), "OS Error:");
        assert_eq!(LuxosErrorClass::InternalError.prefix(), "Internal Error:");
        assert_eq!(
            LuxosErrorClass::JsonDeserialize.prefix(),
            "Failed to deserialize the JSON body"
        );
        assert_eq!(
            LuxosErrorClass::JsonParse.prefix(),
            "Failed to parse the request body as JSON"
        );
        assert_eq!(
            LuxosErrorClass::ContentTypeMissing.prefix(),
            "Expected request with `Content-Type: application/json`"
        );
        assert_eq!(LuxosErrorClass::FileNotFound.prefix(), "file not found");
        assert_eq!(
            LuxosErrorClass::ProfileDownload.prefix(),
            "Internal error when downloading profile"
        );
        assert_eq!(
            LuxosErrorClass::ApiShutdown.prefix(),
            "HTTP API shutting down"
        );
    }

    #[test]
    fn classify_decodes_canonical_messages() {
        // OS Error with attached code.
        assert_eq!(classify("OS Error: 12"), Some(LuxosErrorClass::OsError));
        // file not found is a bare token; classify uses contains().
        assert_eq!(
            classify("file not found"),
            Some(LuxosErrorClass::FileNotFound)
        );
        // Persistent log fetch with appended <name>.
        assert_eq!(
            classify("Failed to fetch persistent log entry: luxminer.log"),
            Some(LuxosErrorClass::PersistentLogFetch)
        );
        // JsonDeserialize with axum-style suffix.
        assert_eq!(
            classify(
                "Failed to deserialize the JSON body into the target type: missing field `pw`"
            ),
            Some(LuxosErrorClass::JsonDeserialize)
        );
    }

    #[test]
    fn classify_distinguishes_log_fetch_from_file_fetch() {
        // Both prefixes start with "Failed to fetch persistent" — order
        // in classify() must put `log entry` BEFORE `file` so the more
        // specific prefix wins.
        let log = classify("Failed to fetch persistent log entry: foo.log");
        assert_eq!(log, Some(LuxosErrorClass::PersistentLogFetch));
        let file = classify("Failed to fetch persistent file: foo.bin");
        assert_eq!(file, Some(LuxosErrorClass::PersistentFileFetch));
    }

    #[test]
    fn classify_returns_none_for_unknown_messages() {
        assert!(classify("Some other random text").is_none());
        assert!(classify("").is_none());
    }

    #[test]
    fn all_error_classes_count_matches_re_doc() {
        // E-rest-api-8080.md §7 lists 14 distinct error categories.
        assert_eq!(ALL_ERROR_CLASSES.len(), 14);
    }

    #[test]
    fn runtime_error_code_round_trip() {
        for err in [
            LuxosRuntimeError::InvalidCommand,
            LuxosRuntimeError::InvalidParamValue,
            LuxosRuntimeError::InvalidSessionField,
        ] {
            let n = err.code();
            assert_eq!(LuxosRuntimeError::from_code(n), Some(err));
        }
    }

    #[test]
    fn runtime_error_codes_match_live_capture() {
        // 09-authenticated-api.txt: 14 / 401 / 408.
        assert_eq!(LuxosRuntimeError::InvalidCommand.code(), 14);
        assert_eq!(LuxosRuntimeError::InvalidParamValue.code(), 401);
        assert_eq!(LuxosRuntimeError::InvalidSessionField.code(), 408);
    }

    #[test]
    fn runtime_error_msg_substrings_pinned() {
        assert_eq!(
            LuxosRuntimeError::InvalidCommand.msg_substr(),
            "Invalid command"
        );
        assert_eq!(
            LuxosRuntimeError::InvalidSessionField.msg_substr(),
            "Invalid field 'session_id'"
        );
    }

    #[test]
    fn unknown_runtime_code_returns_none() {
        assert!(LuxosRuntimeError::from_code(0).is_none());
        assert!(LuxosRuntimeError::from_code(316).is_none()); // SessionCreated, not error
        assert!(LuxosRuntimeError::from_code(999).is_none());
    }

    #[test]
    fn error_class_round_trips_through_serde() {
        for class in ALL_ERROR_CLASSES.iter().copied() {
            let json = serde_json::to_string(&class).unwrap();
            let back: LuxosErrorClass = serde_json::from_str(&json).unwrap();
            assert_eq!(class, back);
        }
    }

    #[test]
    fn error_class_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosErrorClass::PersistentLogFetch).unwrap(),
            "\"persistent_log_fetch\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosErrorClass::ApiShutdown).unwrap(),
            "\"api_shutdown\""
        );
    }

    #[test]
    fn runtime_error_round_trips_through_serde() {
        for err in [
            LuxosRuntimeError::InvalidCommand,
            LuxosRuntimeError::InvalidParamValue,
            LuxosRuntimeError::InvalidSessionField,
        ] {
            let json = serde_json::to_string(&err).unwrap();
            let back: LuxosRuntimeError = serde_json::from_str(&json).unwrap();
            assert_eq!(err, back);
        }
    }
}
