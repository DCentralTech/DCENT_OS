//!  tel-B — append-only audit log encoder (HAL-free).
//!
//! Source RE evidence: LuxOS `audit.json` format from
//! .
//!
//! Records operator-initiated state changes for forensics:
//! - mode change (home / standard / hacker)
//! - pool switch
//! - voltage / frequency override
//! - sysupgrade (signed bundle, version bump, rollback)
//! - autotuner profile selection
//!
//! Each record is one JSON line (NDJSON), append-only, never edited or
//! deleted in normal operation. The runtime adapter writes to
//! `/data/audit.log` and rotates after a size threshold (separate concern,
//! not in this module).
//!
//! This module is **pure logic, no filesystem**: it owns the event
//! encoder + parser + record shape. Caller handles the file write.

use serde::{Deserialize, Serialize};

/// Discrete audit event kinds. Add new variants only at the end of the
/// enum to keep the wire form append-friendly across versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AuditEvent {
    /// Operating mode changed via REST or dashboard.
    ModeChange { from: String, to: String },
    /// Active mining pool URL changed.
    PoolSwitch { from: Option<String>, to: String },
    /// Chain voltage or frequency override applied (or cleared).
    VoltageOverride {
        chain_id: u8,
        voltage_v: Option<f32>,
    },
    /// Autotuner profile selected (LuxOS-style step name like "default" or "320MHz").
    AutotunerProfileSelect { profile_name: String },
    /// Sysupgrade staged (validated signature, written to inactive slot).
    SysupgradeStaged {
        version: String,
        manifest_sha256: String,
    },
    /// Sysupgrade committed (post-reboot, recovery flag promoted to 0x03).
    SysupgradeCommitted { version: String },
    /// Sysupgrade rolled back (a/b slot swap or factory reset).
    SysupgradeRollback {
        from_version: String,
        to_version: String,
        reason: String,
    },
    /// PIC recovery triggered (separate `pic-recovery` binary). Logged
    /// here from the operator-facing tool to provide a single audit
    /// trail across binaries.
    PicRecoveryRun { chain_id: u8, outcome: String },
    /// Open-ended freeform event for new categories not yet enumerated.
    /// Prefer adding a typed variant when stable.
    Free { category: String, message: String },
    /// Operator-issued voltage override that did NOT actually write
    /// voltage — currently emitted by `post_debug_chip_voltage` while the
    /// underlying PIC write path is `NOT_IMPLEMENTED`. Distinct from
    /// `VoltageOverride { voltage_v: None }` (which means an applied
    /// override was cleared). Useful for forensics: an authenticated
    /// operator request reached the handler even if no hardware change
    /// occurred.
    VoltageOverrideAttempted {
        chain_id: u8,
        requested_voltage_v: f32,
    },
    /// Pool configuration was successfully persisted. This intentionally
    /// records field paths only; passwords and worker names are never
    /// embedded in the audit record.
    PoolConfigWrite {
        pool_count: u8,
        changed_fields: Vec<String>,
        secret_fields_redacted: Vec<String>,
    },
    // -----------------------------------------------------------------------
    // R-11 — hardware-safety events.
    //
    // Emitted by the daemon's thermal-supervisor consumption point (actor
    // `"thermal_supervisor"`) when it ACTS on a protective `SupervisorAction`,
    // so the operator's forensic audit log records the safety events the
    // thermal FSM computes — not just management / config / OTA changes. The
    // emit site de-dups (one row per TRANSITION into a safety event), so these
    // are NOT written on every supervisor tick. Appended at the end of the
    // enum to keep the wire form append-friendly across versions.
    // -----------------------------------------------------------------------
    /// A hash board was powered off because its temperature crossed the panic
    /// threshold — the over-temp case of
    /// `SupervisorAction::RequestBoardPowerOff` (`BoardPanic` / `ChipPanic`
    /// reason). `max_temp_c` is the board's hottest valid reading this tick;
    /// `threshold_c` is the configured panic threshold that was crossed.
    OvertempShutdown { max_temp_c: f32, threshold_c: f32 },
    /// The working-fan count fell below the configured minimum and the
    /// supervisor cut hash (`RequestEmergencyShutdown { FanPanic }`).
    /// `working_fans` is the observed count of turning fans (tach > 0) this
    /// tick; `min_fans` is the configured floor.
    FanPanic { working_fans: u8, min_fans: u8 },
    /// A single hash board was powered off for a non-overtemp protective
    /// reason — `RequestBoardPowerOff` with e.g. `SensorFailure`. `reason` is
    /// the serialized `ThermalReason` (snake_case, matching the supervisor
    /// telemetry vocabulary).
    BoardPowerOff { chain_id: u8, reason: String },
    /// The supervisor requested a whole-unit emergency shutdown for a thermal
    /// reason OTHER than fan-panic (`RequestEmergencyShutdown` with e.g. hydro
    /// panic / startup-cold / flow-loss). `reason` is the serialized
    /// `ThermalReason` (snake_case).
    ThermalEmergencyShutdown { reason: String },
}

/// One line in the audit log.
///
/// Field order is the canonical NDJSON layout — operator can `tail -f`
/// `/data/audit.log` and read events left-to-right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Unix epoch milliseconds.
    pub timestamp_ms: u64,
    /// Stable record schema version. Bump on breaking changes.
    pub schema_version: u32,
    /// Best-effort operator identity. `"system"` for daemon-internal
    /// events; `"unknown"` if the source can't be attributed.
    pub actor: String,
    /// The event itself.
    pub event: AuditEvent,
}

impl AuditRecord {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(timestamp_ms: u64, actor: impl Into<String>, event: AuditEvent) -> Self {
        Self {
            timestamp_ms,
            schema_version: Self::SCHEMA_VERSION,
            actor: actor.into(),
            event,
        }
    }

    /// Render this record as one NDJSON line (no trailing newline; caller
    /// adds `\n` when appending to the file).
    pub fn to_ndjson_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parse one NDJSON line back into a record.
    pub fn from_ndjson_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// Encode a slice of records as an NDJSON blob (one record per line, each
/// line terminated with `\n`). Caller can append this directly to the
/// audit log file.
pub fn encode_ndjson_batch(records: &[AuditRecord]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for r in records {
        out.push_str(&r.to_ndjson_line()?);
        out.push('\n');
    }
    Ok(out)
}

/// Parse an NDJSON blob (one record per line). Lines that fail to parse
/// are skipped — the audit log is forward-compatible (newer schema rows
/// from a future firmware version don't break older readers).
pub fn parse_ndjson_batch_lossy(blob: &str) -> Vec<AuditRecord> {
    blob.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| AuditRecord::from_ndjson_line(l).ok())
        .collect()
}

// ---------------------------------------------------------------------------
//  W2 — AuditRing
//
// Fixed-capacity in-memory ring buffer that the runtime adapter pushes
// audit events into. Read by `GET /api/history/audit?limit=N` so
// operators can inspect recent audit activity without grep'ing
// `/data/audit.log`. The persistent NDJSON log still wins for long-term
// forensics; the ring is the live operator-facing surface.
// ---------------------------------------------------------------------------

/// Default ring capacity for the runtime adapter. 256 entries covers
/// hours of typical operator activity (mode toggles, pool switches,
/// sysupgrade events) without holding meaningful memory.
pub const DEFAULT_AUDIT_RING_CAPACITY: usize = 256;

/// Fixed-capacity FIFO of recent audit records.
#[derive(Debug, Clone)]
pub struct AuditRing {
    capacity: usize,
    entries: std::collections::VecDeque<AuditRecord>,
    total_seq: u64,
}

impl AuditRing {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "AuditRing capacity must be > 0");
        Self {
            capacity,
            entries: std::collections::VecDeque::with_capacity(capacity),
            total_seq: 0,
        }
    }

    /// Default-capacity (256) ring.
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_AUDIT_RING_CAPACITY)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total number of records pushed since this ring was created.
    /// Differs from `len()` because entries are evicted at capacity.
    pub fn total_seen(&self) -> u64 {
        self.total_seq
    }

    /// Push a new record, evicting the oldest entry at capacity.
    pub fn push(&mut self, rec: AuditRecord) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(rec);
        self.total_seq = self.total_seq.saturating_add(1);
    }

    /// Snapshot the most recent `limit` entries (newest last). If
    /// `limit` exceeds `len()`, returns all entries.
    pub fn snapshot(&self, limit: usize) -> Vec<AuditRecord> {
        let take = limit.min(self.entries.len());
        if take == 0 {
            return Vec::new();
        }
        let skip = self.entries.len() - take;
        self.entries.iter().skip(skip).cloned().collect()
    }

    /// Snapshot ALL entries (newest last).
    pub fn snapshot_all(&self) -> Vec<AuditRecord> {
        self.entries.iter().cloned().collect()
    }

    /// Wipe every entry. Resets `total_seen` to 0.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_seq = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts() -> u64 {
        1_700_000_000_000
    }

    #[test]
    fn audit_record_round_trips_through_ndjson() {
        let r = AuditRecord::new(
            ts(),
            "operator",
            AuditEvent::ModeChange {
                from: "standard".to_string(),
                to: "home".to_string(),
            },
        );
        let line = r.to_ndjson_line().unwrap();
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn audit_record_carries_schema_version_1() {
        let r = AuditRecord::new(
            ts(),
            "system",
            AuditEvent::AutotunerProfileSelect {
                profile_name: "320MHz".to_string(),
            },
        );
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn ndjson_line_uses_snake_case_event_tag() {
        let r = AuditRecord::new(
            ts(),
            "system",
            AuditEvent::SysupgradeStaged {
                version: "0.6.0".to_string(),
                manifest_sha256: "abc123".to_string(),
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"sysupgrade_staged\""));
        assert!(line.contains("\"version\":\"0.6.0\""));
    }

    #[test]
    fn batch_encode_decode_round_trips() {
        let records = vec![
            AuditRecord::new(
                ts(),
                "operator",
                AuditEvent::PoolSwitch {
                    from: Some("stratum+tcp://old:3333".to_string()),
                    to: "stratum+tcp://new:3333".to_string(),
                },
            ),
            AuditRecord::new(
                ts() + 1000,
                "system",
                AuditEvent::SysupgradeCommitted {
                    version: "0.6.0".to_string(),
                },
            ),
        ];
        let blob = encode_ndjson_batch(&records).unwrap();
        // Every record should produce one trailing-newline-terminated line.
        assert!(blob.ends_with('\n'));
        assert_eq!(blob.lines().count(), 2);
        let parsed = parse_ndjson_batch_lossy(&blob);
        assert_eq!(parsed, records);
    }

    #[test]
    fn parse_skips_invalid_lines_lossy() {
        let blob = format!(
            "{}\nnot-valid-json\n{}\n",
            AuditRecord::new(
                ts(),
                "system",
                AuditEvent::Free {
                    category: "test".to_string(),
                    message: "hello".to_string()
                }
            )
            .to_ndjson_line()
            .unwrap(),
            AuditRecord::new(
                ts() + 1,
                "system",
                AuditEvent::ModeChange {
                    from: "home".to_string(),
                    to: "standard".to_string()
                }
            )
            .to_ndjson_line()
            .unwrap(),
        );
        let parsed = parse_ndjson_batch_lossy(&blob);
        // Invalid line dropped silently; valid lines preserved in order.
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn voltage_override_with_none_clears_voltage() {
        // Some(value) = override applied, None = override cleared.
        let r = AuditRecord::new(
            ts(),
            "operator",
            AuditEvent::VoltageOverride {
                chain_id: 1,
                voltage_v: None,
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"voltage_v\":null"));
    }

    #[test]
    fn voltage_override_attempted_round_trips_with_requested_voltage() {
        //  W1c — distinct from VoltageOverride; emitted when
        // the handler authenticated the operator but did NOT write any
        // voltage to hardware.
        let r = AuditRecord::new(
            ts(),
            "rest_attempt",
            AuditEvent::VoltageOverrideAttempted {
                chain_id: 6,
                requested_voltage_v: 9.10,
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"voltage_override_attempted\""));
        assert!(line.contains("\"chain_id\":6"));
        assert!(line.contains("\"requested_voltage_v\":9.1"));
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn pic_recovery_event_carries_chain_and_outcome() {
        let r = AuditRecord::new(
            ts(),
            "operator",
            AuditEvent::PicRecoveryRun {
                chain_id: 0,
                outcome: "completed".to_string(),
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"pic_recovery_run\""));
        assert!(line.contains("\"chain_id\":0"));
    }

    #[test]
    fn pool_config_write_event_records_field_metadata_without_secrets() {
        let r = AuditRecord::new(
            ts(),
            "rest_dashboard",
            AuditEvent::PoolConfigWrite {
                pool_count: 2,
                changed_fields: vec![
                    "pool.primary.worker".to_string(),
                    "pool.failover1.url".to_string(),
                    "pool.failover1.password".to_string(),
                ],
                secret_fields_redacted: vec![
                    "pool.*.worker".to_string(),
                    "pool.*.password".to_string(),
                ],
            },
        );

        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"pool_config_write\""));
        assert!(line.contains("\"pool_count\":2"));
        assert!(line.contains("pool.failover1.url"));
        assert!(line.contains("pool.*.password"));
        assert!(!line.contains("secret-worker"));
        assert!(!line.contains("secret-password"));
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    // -----------------------------------------------------------------------
    // R-11 — hardware-safety event tests
    // -----------------------------------------------------------------------

    #[test]
    fn overtemp_shutdown_round_trips_with_temps() {
        let r = AuditRecord::new(
            ts(),
            "thermal_supervisor",
            AuditEvent::OvertempShutdown {
                max_temp_c: 71.5,
                threshold_c: 70.0,
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"overtemp_shutdown\""));
        assert!(line.contains("\"max_temp_c\":71.5"));
        assert!(line.contains("\"threshold_c\":70.0"));
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn fan_panic_round_trips_with_counts() {
        let r = AuditRecord::new(
            ts(),
            "thermal_supervisor",
            AuditEvent::FanPanic {
                working_fans: 0,
                min_fans: 1,
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"fan_panic\""));
        assert!(line.contains("\"working_fans\":0"));
        assert!(line.contains("\"min_fans\":1"));
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn board_power_off_round_trips_with_chain_and_reason() {
        let r = AuditRecord::new(
            ts(),
            "thermal_supervisor",
            AuditEvent::BoardPowerOff {
                chain_id: 2,
                reason: "sensor_failure".to_string(),
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"board_power_off\""));
        assert!(line.contains("\"chain_id\":2"));
        assert!(line.contains("\"reason\":\"sensor_failure\""));
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn thermal_emergency_shutdown_round_trips_with_reason() {
        let r = AuditRecord::new(
            ts(),
            "thermal_supervisor",
            AuditEvent::ThermalEmergencyShutdown {
                reason: "hydro_flow_loss".to_string(),
            },
        );
        let line = r.to_ndjson_line().unwrap();
        assert!(line.contains("\"event\":\"thermal_emergency_shutdown\""));
        assert!(line.contains("\"reason\":\"hydro_flow_loss\""));
        let back = AuditRecord::from_ndjson_line(&line).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn hardware_safety_events_batch_round_trips_alongside_legacy_events() {
        // New hardware-safety variants must interleave with the existing
        // management/config variants in one NDJSON batch and round-trip
        // losslessly (new client reads its own new + old rows).
        let records = vec![
            AuditRecord::new(
                ts(),
                "operator",
                AuditEvent::ModeChange {
                    from: "standard".to_string(),
                    to: "home".to_string(),
                },
            ),
            AuditRecord::new(
                ts() + 1,
                "thermal_supervisor",
                AuditEvent::OvertempShutdown {
                    max_temp_c: 101.0,
                    threshold_c: 100.0,
                },
            ),
            AuditRecord::new(
                ts() + 2,
                "thermal_supervisor",
                AuditEvent::FanPanic {
                    working_fans: 1,
                    min_fans: 2,
                },
            ),
            AuditRecord::new(
                ts() + 3,
                "thermal_supervisor",
                AuditEvent::ThermalEmergencyShutdown {
                    reason: "hydro_panic".to_string(),
                },
            ),
        ];
        let blob = encode_ndjson_batch(&records).unwrap();
        assert_eq!(blob.lines().count(), 4);
        let parsed = parse_ndjson_batch_lossy(&blob);
        assert_eq!(parsed, records);
    }

    #[test]
    fn old_reader_tolerates_unknown_future_event_variant() {
        // Backward/forward-compat contract: an OLDER firmware whose enum does
        // not yet know a variant simply SKIPS that line (lossy parse) while
        // preserving every event it DOES understand. Here we simulate the old
        // reader by feeding a line whose `event` tag is not in this enum,
        // between two known hardware-safety rows.
        let known_first = AuditRecord::new(
            ts(),
            "thermal_supervisor",
            AuditEvent::BoardPowerOff {
                chain_id: 0,
                reason: "board_panic".to_string(),
            },
        )
        .to_ndjson_line()
        .unwrap();
        let known_last = AuditRecord::new(
            ts() + 2,
            "thermal_supervisor",
            AuditEvent::FanPanic {
                working_fans: 0,
                min_fans: 1,
            },
        )
        .to_ndjson_line()
        .unwrap();
        let unknown_future = r#"{"timestamp_ms":1700000001,"schema_version":2,"actor":"thermal_supervisor","event":"coolant_pump_stall","rpm":0}"#;
        let blob = format!("{known_first}\n{unknown_future}\n{known_last}\n");
        let parsed = parse_ndjson_batch_lossy(&blob);
        // Unknown line dropped; both known hardware-safety rows preserved in order.
        assert_eq!(parsed.len(), 2);
        assert!(matches!(
            &parsed[0].event,
            AuditEvent::BoardPowerOff { chain_id: 0, .. }
        ));
        assert!(matches!(
            &parsed[1].event,
            AuditEvent::FanPanic {
                working_fans: 0,
                min_fans: 1
            }
        ));
    }

    #[test]
    fn empty_batch_renders_empty_blob() {
        let blob = encode_ndjson_batch(&[]).unwrap();
        assert!(blob.is_empty());
    }

    // -----------------------------------------------------------------------
    //  W2 — AuditRing tests
    // -----------------------------------------------------------------------

    fn sample_record(seq: u64) -> AuditRecord {
        AuditRecord::new(
            ts() + seq,
            "operator",
            AuditEvent::Free {
                category: "test".to_string(),
                message: format!("event {}", seq),
            },
        )
    }

    #[test]
    fn audit_ring_default_capacity_matches_constant() {
        let ring = AuditRing::with_default_capacity();
        assert_eq!(ring.capacity(), DEFAULT_AUDIT_RING_CAPACITY);
        assert_eq!(DEFAULT_AUDIT_RING_CAPACITY, 256);
        assert!(ring.is_empty());
    }

    #[test]
    fn audit_ring_evicts_oldest_at_capacity() {
        let mut ring = AuditRing::new(3);
        for s in 0..5 {
            ring.push(sample_record(s));
        }
        assert_eq!(ring.len(), 3);
        let snap = ring.snapshot_all();
        // Oldest two evicted; entries 2/3/4 remain.
        assert!(matches!(
            &snap[0].event,
            AuditEvent::Free { message, .. } if message == "event 2"
        ));
        assert!(matches!(
            &snap[2].event,
            AuditEvent::Free { message, .. } if message == "event 4"
        ));
        assert_eq!(ring.total_seen(), 5);
    }

    #[test]
    fn audit_ring_snapshot_limit_caps_to_len() {
        let mut ring = AuditRing::new(8);
        for s in 0..3 {
            ring.push(sample_record(s));
        }
        // Limit > len returns all.
        assert_eq!(ring.snapshot(10).len(), 3);
        // Limit < len returns the most recent N.
        let recent = ring.snapshot(2);
        assert_eq!(recent.len(), 2);
        assert!(matches!(
            &recent[1].event,
            AuditEvent::Free { message, .. } if message == "event 2"
        ));
        // Limit 0 returns empty.
        assert!(ring.snapshot(0).is_empty());
    }

    #[test]
    fn audit_ring_clear_resets_state() {
        let mut ring = AuditRing::new(4);
        for s in 0..3 {
            ring.push(sample_record(s));
        }
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.total_seen(), 0);
    }

    #[test]
    #[should_panic(expected = "AuditRing capacity must be > 0")]
    fn audit_ring_zero_capacity_panics() {
        let _ = AuditRing::new(0);
    }

    #[test]
    fn audit_ring_total_seen_tracks_push_count_not_len() {
        // total_seen reflects total push count, not current ring length.
        // Important so operators can detect ring overflow at the
        // /api/history/audit endpoint.
        let mut ring = AuditRing::new(2);
        for s in 0..10 {
            ring.push(sample_record(s));
        }
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.total_seen(), 10);
    }
}
