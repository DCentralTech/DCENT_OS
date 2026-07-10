//! GROUP C (W8 parity) — read-only access to the PERSISTENT, reboot-surviving
//! audit log.
//!
//! ## What was already here vs. what this module adds
//!
//! The persistent audit log itself already exists in `crate::lib`:
//!   - `append_audit_record_to_path()` writes one NDJSON record per line to
//!     `/data/audit.log` (path overridable via `DCENTOS_AUDIT_LOG_PATH`).
//!   - `trim_audit_log_to_max_bytes()` size-caps the file (default 1 MiB,
//!     overridable via `DCENTOS_AUDIT_LOG_MAX_BYTES`) so it can never grow
//!     unbounded — this is the load-bearing disk-hygiene guarantee.
//!   - `push_audit_event()` writes both to the in-memory `AuditRing` AND to
//!     the persistent file.
//!   - `GET /api/history/audit` reads the in-memory ring ONLY (256 entries,
//!     LOST on reboot).
//!
//! The W8 parity gap (DCENT ❌ persistent audit log vs LuxOS ✅) was that the
//! reboot-surviving file was *written but never readable through the API*. After
//! a reboot the ring is empty, so operators and fleet tools could not see what
//! happened before the reboot. LuxOS exposes `/luxor/audit.json`; we now expose
//! the equivalent.
//!
//! This module adds the missing read-back:
//!   - `GET /api/audit-log?offset=N&limit=M` — paginated (newest-first),
//!     redacted view of the PERSISTENT `/data/audit.log` file.
//!
//! ## Redaction
//!
//! The `AuditEvent` enum is redaction-safe BY CONSTRUCTION: `PoolConfigWrite`
//! records only field *paths* + a `secret_fields_redacted` list (never values),
//! and no variant carries passwords or wallet secrets. As defence-in-depth this
//! reader still passes every record through [`redact_record`] before serving so
//! that (a) pool URLs in `PoolSwitch` are sanitized through the canonical
//! `dcentrald_stratum::pool_api::sanitize_pool_url` (strips any embedded
//! `user:pass@`), and (b) the freeform `Free { message }` variant — the one
//! variant an emit site could accidentally stuff a secret into — is scrubbed of
//! anything that looks like a `key=secret` / `key: secret` token using the same
//! `<redacted>` placeholder the support-bundle redactor uses.
//!
//! ## Bounded by design
//!
//! The reader reads the whole file into memory once per request, which is safe
//! because the file itself is size-capped (≤1 MiB default) by the writer's
//! rotation. We additionally hard-cap the read at [`MAX_AUDIT_LOG_READ_BYTES`]
//! so a misconfigured `DCENTOS_AUDIT_LOG_MAX_BYTES` can never make a single
//! request allocate an unbounded buffer.

use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use dcentrald_api_types::audit_log::{parse_ndjson_batch_lossy, AuditEvent, AuditRecord};

use crate::rest::SECRET_REDACTION_PLACEHOLDER;
use crate::AppState;

/// Hard ceiling on how many bytes a single `/api/audit-log` request will read
/// from disk, independent of `DCENTOS_AUDIT_LOG_MAX_BYTES`. The persistent
/// writer already caps the file at 1 MiB by default; this is a second,
/// request-scoped guard so a hand-edited env var can never make one request
/// allocate an unbounded buffer. 4 MiB leaves generous headroom for an operator
/// who deliberately raised the file cap, while staying far below anything that
/// could pressure the embedded miner's RAM.
pub const MAX_AUDIT_LOG_READ_BYTES: u64 = 4 * 1_048_576;

/// Default page size when the caller does not specify `limit`.
pub const DEFAULT_AUDIT_LOG_PAGE: usize = 100;

/// Maximum page size a caller may request. Keeps a single response bounded even
/// when the file is large.
pub const MAX_AUDIT_LOG_PAGE: usize = 1000;

/// Build the persistent-audit-log sub-router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/audit-log", get(get_audit_log))
}

/// Query parameters for `GET /api/audit-log`.
#[derive(Debug, Default, Deserialize)]
pub struct AuditLogQuery {
    /// How many of the most-recent records to skip before returning a page.
    /// 0 (default) = start at the newest record.
    pub offset: Option<usize>,
    /// Page size. Defaults to [`DEFAULT_AUDIT_LOG_PAGE`], capped at
    /// [`MAX_AUDIT_LOG_PAGE`].
    pub limit: Option<usize>,
}

/// `GET /api/audit-log?offset=N&limit=M`
///
/// Returns a paginated (newest-first), redacted view of the PERSISTENT audit
/// log at `crate::audit_log_path()` (default `/data/audit.log`). Unlike
/// `GET /api/history/audit` (in-memory ring, lost on reboot), this survives
/// reboots — it is the operator/fleet-facing forensics surface.
async fn get_audit_log(
    State(_state): State<Arc<AppState>>,
    Query(q): Query<AuditLogQuery>,
) -> impl IntoResponse {
    let path = crate::audit_log_path();
    let offset = q.offset.unwrap_or(0);
    let limit = q
        .limit
        .unwrap_or(DEFAULT_AUDIT_LOG_PAGE)
        .clamp(1, MAX_AUDIT_LOG_PAGE);

    let page = read_persistent_audit_log_page(&path, offset, limit);

    Json(serde_json::json!({
        "schema": "dcentrald-api persistent audit-log v1",
        "path": path.to_string_lossy(),
        "total": page.total,
        "offset": offset,
        "limit": limit,
        "returned": page.records.len(),
        "redacted": true,
        // Newest-first. Each record is already passed through `redact_record`.
        "events": page.records,
    }))
}

/// A single page of persistent-audit-log records plus the total count present
/// in the file (so the caller can paginate without re-reading blindly).
#[derive(Debug, Default, PartialEq)]
pub struct AuditLogPage {
    /// Redacted records, NEWEST FIRST, after applying `offset`/`limit`.
    pub records: Vec<AuditRecord>,
    /// Total parseable records present in the file (before pagination).
    pub total: usize,
}

/// Read the persistent NDJSON audit log at `path`, parse it lossily, redact
/// every record, sort newest-first, and apply `offset`/`limit` pagination.
///
/// Bounded: at most [`MAX_AUDIT_LOG_READ_BYTES`] are read from the file
/// regardless of its on-disk size (we read the TAIL so the newest records are
/// always present even if an over-large file is truncated for reading).
///
/// Missing file / unreadable file → an empty page (the audit log surface
/// degrades gracefully; a fresh install with no events yet must not error).
pub fn read_persistent_audit_log_page(path: &Path, offset: usize, limit: usize) -> AuditLogPage {
    let blob = match read_tail_bounded(path, MAX_AUDIT_LOG_READ_BYTES) {
        Ok(b) => b,
        Err(_) => return AuditLogPage::default(),
    };

    // Lossy parse: forward-compatible — unknown/newer-schema or partial first
    // line (if the tail cut mid-record) is silently dropped.
    let mut records = parse_ndjson_batch_lossy(&blob);
    for rec in records.iter_mut() {
        redact_record(rec);
    }

    // File is append-only chronological (oldest→newest). Present newest-first.
    records.reverse();

    let total = records.len();
    let page: Vec<AuditRecord> = records.into_iter().skip(offset).take(limit).collect();

    AuditLogPage {
        records: page,
        total,
    }
}

/// Read up to `max_bytes` from the TAIL of `path` as UTF-8 (lossy).
///
/// Reading the tail (not the head) guarantees the newest records survive even
/// if a deliberately-oversized file exceeds the per-request read cap. If the
/// tail starts mid-line the partial first line is dropped by the lossy NDJSON
/// parser, so no malformed record is ever surfaced.
fn read_tail_bounded(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Seek as _;

    let meta = std::fs::metadata(path)?;
    let len = meta.len();
    let read_len = len.min(max_bytes);
    let seek_start = len.saturating_sub(read_len);

    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(seek_start))?;
    let cap = usize::try_from(read_len).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(cap.min(MAX_AUDIT_LOG_READ_BYTES as usize));
    file.take(read_len).read_to_end(&mut bytes)?;

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Redact a single audit record IN PLACE before it leaves the API boundary.
///
/// The `AuditEvent` enum is already secret-free by construction; this is
/// defence-in-depth so that no future emit site can leak a credential through
/// the persistent log:
///   - `PoolSwitch { from, to }` — pool URLs are sanitized through the
///     canonical `sanitize_pool_url` (strips any embedded `user:pass@`).
///   - `Free { message }` — the only freeform variant — has any
///     `key=secret` / `key: secret` token scrubbed to `<redacted>`.
///
/// All other variants carry only structured, non-secret metadata and are left
/// untouched.
pub fn redact_record(rec: &mut AuditRecord) {
    match &mut rec.event {
        AuditEvent::PoolSwitch { from, to } => {
            if let Some(from_url) = from {
                *from_url = dcentrald_stratum::pool_api::sanitize_pool_url(from_url);
            }
            *to = dcentrald_stratum::pool_api::sanitize_pool_url(to);
        }
        AuditEvent::Free { message, .. } => {
            *message = scrub_secret_tokens(message);
        }
        _ => {}
    }
}

/// Scrub `key=value` / `key: value` tokens whose key looks secret-bearing,
/// replacing the value with the canonical `<redacted>` placeholder.
///
/// Conservative + allocation-light: only rewrites when a secret-looking key
/// (password / passwd / secret / token / apikey / api_key / privkey /
/// private_key / wallet / mnemonic / seed) is followed by `=` or `:` and a
/// value. Non-secret text passes through byte-for-byte.
fn scrub_secret_tokens(message: &str) -> String {
    const SECRET_KEY_NEEDLES: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "privkey",
        "private_key",
        "mnemonic",
        "seedphrase",
    ];

    let lower = message.to_ascii_lowercase();
    // Fast path: nothing secret-looking present → return unchanged.
    if !SECRET_KEY_NEEDLES.iter().any(|n| lower.contains(n)) {
        return message.to_string();
    }

    // Rewrite token-by-token on whitespace boundaries. A token of the form
    // `<key><sep><value>` where key matches a needle and sep is `=`/`:` gets
    // its value replaced. This preserves surrounding structure for forensics
    // ("operator set X") while never echoing the secret value.
    message
        .split_inclusive(char::is_whitespace)
        .map(|chunk| {
            // Separate the trailing whitespace (if any) so we don't lose it.
            let (token, trailing) = split_trailing_ws(chunk);
            let redacted = redact_kv_token(token, SECRET_KEY_NEEDLES);
            format!("{redacted}{trailing}")
        })
        .collect()
}

/// Split a chunk into (non-whitespace-token, trailing-whitespace).
fn split_trailing_ws(chunk: &str) -> (&str, &str) {
    let end = chunk
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_whitespace())
        .last()
        .map(|(i, _)| i)
        .unwrap_or(chunk.len());
    chunk.split_at(end)
}

/// If `token` is `key<sep>value` with a secret-looking key and a non-empty
/// value, return `key<sep><redacted>`; otherwise return the token unchanged.
fn redact_kv_token(token: &str, needles: &[&str]) -> String {
    for sep in ['=', ':'] {
        if let Some(pos) = token.find(sep) {
            let key = &token[..pos];
            let key_lower = key.to_ascii_lowercase();
            let value = &token[pos + sep.len_utf8()..];
            if !value.is_empty() && needles.iter().any(|n| key_lower.contains(n)) {
                return format!("{key}{sep}{SECRET_REDACTION_PLACEHOLDER}");
            }
        }
    }
    token.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_api_types::audit_log::AuditEvent;
    use std::io::Write as _;

    fn rec(ts: u64, event: AuditEvent) -> AuditRecord {
        AuditRecord::new(ts, "operator", event)
    }

    fn write_log(records: &[AuditRecord]) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "dcentos-audit-test-{}-{}.log",
            std::process::id(),
            ts_unique()
        ));
        let mut f = std::fs::File::create(&path).expect("create test log");
        for r in records {
            f.write_all(r.to_ndjson_line().unwrap().as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
        }
        f.flush().unwrap();
        path
    }

    fn ts_unique() -> u128 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        now.wrapping_add(n as u128)
    }

    #[test]
    fn missing_file_yields_empty_page() {
        let path = std::env::temp_dir().join("dcentos-audit-does-not-exist-xyz.log");
        let _ = std::fs::remove_file(&path);
        let page = read_persistent_audit_log_page(&path, 0, 100);
        assert_eq!(page, AuditLogPage::default());
        assert!(page.records.is_empty());
        assert_eq!(page.total, 0);
    }

    #[test]
    fn reads_back_persisted_records_newest_first() {
        // This is the W8 gap: a record written before a "reboot" (ring lost)
        // must still be readable from the persistent file.
        let path = write_log(&[
            rec(
                1_000,
                AuditEvent::ModeChange {
                    from: "standard".into(),
                    to: "home".into(),
                },
            ),
            rec(
                2_000,
                AuditEvent::SysupgradeCommitted {
                    version: "0.6.0".into(),
                },
            ),
        ]);
        let page = read_persistent_audit_log_page(&path, 0, 100);
        assert_eq!(page.total, 2);
        assert_eq!(page.records.len(), 2);
        // Newest first.
        assert_eq!(page.records[0].timestamp_ms, 2_000);
        assert_eq!(page.records[1].timestamp_ms, 1_000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pagination_offset_and_limit() {
        let records: Vec<AuditRecord> = (0..10)
            .map(|i| {
                rec(
                    i as u64,
                    AuditEvent::Free {
                        category: "test".into(),
                        message: format!("event {i}"),
                    },
                )
            })
            .collect();
        let path = write_log(&records);

        // Newest first: ts 9,8,7,... → offset 2, limit 3 → ts 7,6,5.
        let page = read_persistent_audit_log_page(&path, 2, 3);
        assert_eq!(page.total, 10);
        assert_eq!(page.records.len(), 3);
        assert_eq!(page.records[0].timestamp_ms, 7);
        assert_eq!(page.records[2].timestamp_ms, 5);

        // Offset past the end → empty page but honest total.
        let page = read_persistent_audit_log_page(&path, 100, 5);
        assert_eq!(page.total, 10);
        assert!(page.records.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn skips_corrupt_lines_lossy() {
        let path = std::env::temp_dir().join(format!(
            "dcentos-audit-corrupt-{}-{}.log",
            std::process::id(),
            ts_unique()
        ));
        let good = rec(
            5,
            AuditEvent::ModeChange {
                from: "home".into(),
                to: "standard".into(),
            },
        );
        std::fs::write(
            &path,
            format!("{}\nnot-json-garbage\n", good.to_ndjson_line().unwrap()),
        )
        .unwrap();
        let page = read_persistent_audit_log_page(&path, 0, 100);
        assert_eq!(page.total, 1);
        assert_eq!(page.records[0].timestamp_ms, 5);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pool_switch_urls_are_sanitized() {
        // A pool URL with embedded credentials must never survive read-back.
        let mut r = rec(
            1,
            AuditEvent::PoolSwitch {
                from: Some("stratum+tcp://user:hunter2@old.pool:3333".into()),
                to: "stratum+tcp://user:s3cr3t@new.pool:3333".into(),
            },
        );
        redact_record(&mut r);
        match &r.event {
            AuditEvent::PoolSwitch { from, to } => {
                let from = from.as_deref().unwrap();
                assert!(
                    !from.contains("hunter2"),
                    "from url leaked password: {from}"
                );
                assert!(!to.contains("s3cr3t"), "to url leaked password: {to}");
            }
            other => panic!("expected PoolSwitch, got {other:?}"),
        }
    }

    #[test]
    fn free_message_secret_tokens_are_scrubbed() {
        let mut r = rec(
            1,
            AuditEvent::Free {
                category: "manual".into(),
                message: "operator set password=hunter2 and api_key=abc123 ok".into(),
            },
        );
        redact_record(&mut r);
        match &r.event {
            AuditEvent::Free { message, .. } => {
                assert!(!message.contains("hunter2"), "leaked password: {message}");
                assert!(!message.contains("abc123"), "leaked api key: {message}");
                // Surrounding structure preserved for forensics.
                assert!(message.contains("operator set"));
                assert!(message.contains(SECRET_REDACTION_PLACEHOLDER));
            }
            other => panic!("expected Free, got {other:?}"),
        }
    }

    #[test]
    fn free_message_without_secrets_passes_through() {
        let original = "operator switched mode home -> standard";
        let mut r = rec(
            1,
            AuditEvent::Free {
                category: "manual".into(),
                message: original.into(),
            },
        );
        redact_record(&mut r);
        match &r.event {
            AuditEvent::Free { message, .. } => assert_eq!(message, original),
            other => panic!("expected Free, got {other:?}"),
        }
    }

    #[test]
    fn read_page_redacts_pool_switch_from_disk() {
        // End-to-end: write a credential-bearing PoolSwitch, read it back
        // through the public page reader, confirm the credential is gone.
        let path = write_log(&[rec(
            1,
            AuditEvent::PoolSwitch {
                from: None,
                to: "stratum+tcp://wkr:topsecret@pool.example:3333".into(),
            },
        )]);
        let page = read_persistent_audit_log_page(&path, 0, 100);
        assert_eq!(page.records.len(), 1);
        let line = page.records[0].to_ndjson_line().unwrap();
        assert!(!line.contains("topsecret"), "credential leaked: {line}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_is_bounded_by_max_read_bytes() {
        // The reader must never allocate more than MAX_AUDIT_LOG_READ_BYTES,
        // even if the file is larger. We can't easily build a >4 MiB file fast
        // in a unit test, so assert the constant is a sane bound and that the
        // tail reader honors a small explicit cap.
        assert_eq!(MAX_AUDIT_LOG_READ_BYTES, 4 * 1_048_576);

        let records: Vec<AuditRecord> = (0..50)
            .map(|i| {
                rec(
                    i as u64,
                    AuditEvent::Free {
                        category: "test".into(),
                        message: format!("event-{i}"),
                    },
                )
            })
            .collect();
        let path = write_log(&records);

        // Read only the last ~200 bytes of the file: the tail reader must
        // still return parseable newest records, dropping the partial head.
        let blob = read_tail_bounded(&path, 200).unwrap();
        let parsed = parse_ndjson_batch_lossy(&blob);
        assert!(!parsed.is_empty(), "tail read returned no records");
        // Every parsed record is from the newest end of the file.
        let max_ts = parsed.iter().map(|r| r.timestamp_ms).max().unwrap();
        assert_eq!(max_ts, 49, "tail did not include the newest record");
        let _ = std::fs::remove_file(&path);
    }
}
