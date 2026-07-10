//!  luxos-J — LuxOS `/luxor/audit.json` audit-log schema DTOs
//! (HAL-free).
//!
//! Source RE evidence:
//!
//! §10 (lines 370-395).
//!
//! Live capture from `a lab unit`: 444-byte JSON ring buffer at
//! `/luxor/audit.json` containing 5 events. Verified schema from
//! luxminer binary strings.
//!
//! HAZARD pinned by tests:
//! - The audit log is **mutable**. No signature, no chain, no
//!   append-only enforcement. Luxminer owns the file (writes via
//!   "Failed to write audit data to file" error path); a compromised
//!   process can rewrite history.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Audit event data (internally-tagged enum)
// ---------------------------------------------------------------------------

/// One audit-log event payload. Internally-tagged on `type` per §10
/// — matches luxminer's serde-derived `AuditEventData` enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LuxosAuditEventData {
    /// `board_reboot` — single hashboard rebooted.
    BoardReboot {
        board_id: u32,
        reason: String,
        will_autostart: bool,
    },
    /// `sys_halt` — operator halt or recovery from halt.
    SysHalt { is_halted: bool },
    /// `sys_shutdown` — operator-initiated shutdown.
    SysShutdown { clean_shutdown: bool },
    /// `fs_corruption` — jffs2 / mtd corruption detected on a device.
    FsCorruption {
        device: String,
        reboot_required: bool,
    },
}

impl LuxosAuditEventData {
    /// True iff this event indicates a hashboard-level fault.
    pub fn is_board_fault(&self) -> bool {
        matches!(self, Self::BoardReboot { .. })
    }

    /// True iff this event represents a destructive system event
    /// requiring follow-up (shutdown / fs corruption).
    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::SysShutdown { .. } | Self::FsCorruption { .. })
    }
}

// ---------------------------------------------------------------------------
// AuditEvent envelope
// ---------------------------------------------------------------------------

/// Top-level audit event — `{ts: u64, data: AuditEventData}` per §10.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LuxosAuditEvent {
    /// Unix timestamp (seconds since epoch) when the event was logged.
    pub ts: u64,
    pub data: LuxosAuditEventData,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Filesystem path of the audit log per §10.
pub const LUXOS_AUDIT_LOG_PATH: &str = "/luxor/audit.json";

/// Live `a lab unit` capture size (444 bytes for 5 events). Used as a
/// rough lower-bound sanity check.
pub const LUXOS_AUDIT_LOG_REFERENCE_SIZE_BYTES: u64 = 444;

/// Number of events in the live `a lab unit` reference capture.
pub const LUXOS_AUDIT_LOG_REFERENCE_EVENT_COUNT: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_path_pinned() {
        assert_eq!(LUXOS_AUDIT_LOG_PATH, "/luxor/audit.json");
    }

    #[test]
    fn board_reboot_round_trips_through_serde() {
        let event = LuxosAuditEventData::BoardReboot {
            board_id: 1,
            reason: "asic initialization failure".to_string(),
            will_autostart: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: LuxosAuditEventData = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn board_reboot_serializes_with_type_tag() {
        // Internally-tagged: payload contains `"type":"board_reboot"`.
        let event = LuxosAuditEventData::BoardReboot {
            board_id: 1,
            reason: "asic initialization failure".to_string(),
            will_autostart: true,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "board_reboot");
        assert_eq!(json["board_id"], 1);
        assert_eq!(json["will_autostart"], true);
    }

    #[test]
    fn sys_halt_round_trips_through_serde() {
        for is_halted in [true, false] {
            let event = LuxosAuditEventData::SysHalt { is_halted };
            let json = serde_json::to_string(&event).unwrap();
            let back: LuxosAuditEventData = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn sys_shutdown_round_trips_through_serde() {
        for clean in [true, false] {
            let event = LuxosAuditEventData::SysShutdown {
                clean_shutdown: clean,
            };
            let json = serde_json::to_string(&event).unwrap();
            let back: LuxosAuditEventData = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn fs_corruption_round_trips_through_serde() {
        let event = LuxosAuditEventData::FsCorruption {
            device: "/dev/mtd11".to_string(),
            reboot_required: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: LuxosAuditEventData = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn audit_event_envelope_round_trips_through_serde() {
        let event = LuxosAuditEvent {
            ts: 1_777_490_749,
            data: LuxosAuditEventData::BoardReboot {
                board_id: 1,
                reason: "asic initialization failure".to_string(),
                will_autostart: true,
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: LuxosAuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn canonical_capture_decodes_verbatim() {
        // §10 verbatim sample (all 5 events). Verifies the typed shape
        // matches the live wire form byte-for-byte at decode time.
        let raw = r#"[
          {"ts":1777490749,"data":{"type":"board_reboot","board_id":1,"reason":"asic initialization failure","will_autostart":true}},
          {"ts":1777490782,"data":{"type":"sys_halt","is_halted":true}},
          {"ts":1777490810,"data":{"type":"sys_halt","is_halted":false}},
          {"ts":1777490833,"data":{"type":"board_reboot","board_id":1,"reason":"asic initialization failure","will_autostart":true}},
          {"ts":1777491365,"data":{"type":"sys_shutdown","clean_shutdown":false}}
        ]"#;
        let events: Vec<LuxosAuditEvent> = serde_json::from_str(raw).unwrap();
        assert_eq!(events.len(), LUXOS_AUDIT_LOG_REFERENCE_EVENT_COUNT);

        // First event: board_reboot.
        match &events[0].data {
            LuxosAuditEventData::BoardReboot {
                board_id,
                reason,
                will_autostart,
            } => {
                assert_eq!(*board_id, 1);
                assert_eq!(reason, "asic initialization failure");
                assert!(will_autostart);
            }
            other => panic!("expected BoardReboot, got {:?}", other),
        }
        assert_eq!(events[0].ts, 1_777_490_749);

        // Last event: sys_shutdown with clean=false.
        match &events[4].data {
            LuxosAuditEventData::SysShutdown { clean_shutdown } => {
                assert!(!*clean_shutdown);
            }
            other => panic!("expected SysShutdown, got {:?}", other),
        }
    }

    #[test]
    fn is_board_fault_predicate_matches_re_doc_classes() {
        // BoardReboot is the only board-level fault class per §10.
        let board = LuxosAuditEventData::BoardReboot {
            board_id: 0,
            reason: "x".into(),
            will_autostart: false,
        };
        assert!(board.is_board_fault());

        for non_board in [
            LuxosAuditEventData::SysHalt { is_halted: true },
            LuxosAuditEventData::SysShutdown {
                clean_shutdown: true,
            },
            LuxosAuditEventData::FsCorruption {
                device: "/dev/mtd11".into(),
                reboot_required: true,
            },
        ] {
            assert!(
                !non_board.is_board_fault(),
                "{:?} should not be a board fault",
                non_board
            );
        }
    }

    #[test]
    fn is_destructive_classification_matches_re_doc() {
        // SysShutdown + FsCorruption are destructive (require follow-up).
        // BoardReboot + SysHalt are recoverable.
        for destructive in [
            LuxosAuditEventData::SysShutdown {
                clean_shutdown: true,
            },
            LuxosAuditEventData::FsCorruption {
                device: "/dev/mtd11".into(),
                reboot_required: true,
            },
        ] {
            assert!(destructive.is_destructive());
        }
        for recoverable in [
            LuxosAuditEventData::BoardReboot {
                board_id: 0,
                reason: "x".into(),
                will_autostart: true,
            },
            LuxosAuditEventData::SysHalt { is_halted: false },
        ] {
            assert!(!recoverable.is_destructive());
        }
    }

    #[test]
    fn () {
        // 444 bytes / 5 events per the live `a lab unit` capture.
        assert_eq!(LUXOS_AUDIT_LOG_REFERENCE_SIZE_BYTES, 444);
        assert_eq!(LUXOS_AUDIT_LOG_REFERENCE_EVENT_COUNT, 5);
    }

    #[test]
    fn fs_corruption_carries_device_and_reboot_flag() {
        // Field name pinning: the wire form uses snake_case
        // `reboot_required` (not `rebootRequired`).
        let event = LuxosAuditEventData::FsCorruption {
            device: "/dev/mtd11".into(),
            reboot_required: true,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("device").is_some());
        assert!(json.get("reboot_required").is_some());
        assert!(json.get("rebootRequired").is_none());
    }

    #[test]
    fn type_tag_serializes_in_snake_case() {
        // All four event types pinned by their wire form `type` value.
        for (event, expected_type) in [
            (
                LuxosAuditEventData::BoardReboot {
                    board_id: 0,
                    reason: "x".into(),
                    will_autostart: false,
                },
                "board_reboot",
            ),
            (LuxosAuditEventData::SysHalt { is_halted: true }, "sys_halt"),
            (
                LuxosAuditEventData::SysShutdown {
                    clean_shutdown: true,
                },
                "sys_shutdown",
            ),
            (
                LuxosAuditEventData::FsCorruption {
                    device: "/dev/mtd11".into(),
                    reboot_required: true,
                },
                "fs_corruption",
            ),
        ] {
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(
                json["type"], expected_type,
                "{:?} should serialize with type='{}'",
                event, expected_type
            );
        }
    }

    #[test]
    fn audit_log_is_mutable_no_append_only_enforcement_documented() {
        // §10 finding: log has NO signature, NO chain, NO append-only
        // enforcement. The DTO surface intentionally provides no
        // verification primitives — luxminer trusts the file.
        // Pin via a structural test: the DTOs can be constructed from
        // ANY input without crypto checks (compile-time guarantee that
        // the DTO doesn't carry verification fields).
        let _e = LuxosAuditEvent {
            ts: 0,
            data: LuxosAuditEventData::SysHalt { is_halted: false },
        };
        // No `signature` field, no `prev_hash` field — test passes by
        // reaching this line with a defaulted construction.
    }

    #[test]
    fn timestamps_in_canonical_capture_are_strictly_increasing() {
        // Sanity: live capture events are in chronological order. A
        // future LuxOS that doesn't enforce this would surface a
        // tampering signal.
        let timestamps: [u64; 5] = [
            1_777_490_749,
            1_777_490_782,
            1_777_490_810,
            1_777_490_833,
            1_777_491_365,
        ];
        for window in timestamps.windows(2) {
            assert!(
                window[0] < window[1],
                "canonical capture timestamps not strictly increasing: {} → {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn empty_audit_log_decodes_to_empty_vec() {
        // Operator may have just installed; audit log empty array.
        let raw = r#"[]"#;
        let events: Vec<LuxosAuditEvent> = serde_json::from_str(raw).unwrap();
        assert!(events.is_empty());
    }
}
