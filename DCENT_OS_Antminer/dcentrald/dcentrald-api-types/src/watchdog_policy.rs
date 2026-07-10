//!  wdg-A — Watchdog policy + opcode constants (HAL-free).
//!
//! Source RE evidence:
//!
//! §1-2 (lines 14-66, 122).
//!
//! Three independent watchdog domains:
//! - **PSU watchdog**: opcode `0x81` arm/disarm + `0x84` heartbeat.
//!   ~30 s timeout. PSU drops OUT1 if heartbeat missed → all chains
//!   lose 12-15 V → mining stops.
//! - **dsPIC watchdog**: opcode `0x16` heartbeat. ~10 s timeout.
//!   PIC drops voltage if heartbeat missed.
//! - **DCENT_OS-side stability gate**: 5 consecutive PIC heartbeats
//!   required before SET_VOLTAGE may be issued (per
//!   ).
//!
//! PSU disarm requires triple-write (3 frames with 1 s gaps each, per
//! the cold-boot RE doc §2 lines 137-139).
//!
//! HAL-free: pure constants + frame composition. The runtime adapter
//! sequences the frames over real I²C.

use crate::dspic_frame::{DspicFrame, DspicOpcode};
use serde::{Deserialize, Serialize};

/// Opcode shorthand for watchdog operations. Mirrors `dspic_frame::DspicOpcode`
/// but groups the three watchdog-specific opcodes for type-checked
/// runtime dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogOp {
    /// PSU watchdog arm/disarm. Payload `0x00` = disarm, `0x01` = arm.
    /// Wire opcode = 0x81.
    PsuArmDisarm,
    /// PSU watchdog heartbeat. Payload `0x00`. Wire opcode = 0x84.
    PsuHeartbeat,
    /// dsPIC heartbeat. Payload `0x00`. Wire opcode = 0x16.
    DspicHeartbeat,
}

impl WatchdogOp {
    pub fn dspic_opcode(&self) -> DspicOpcode {
        match self {
            WatchdogOp::PsuArmDisarm => DspicOpcode::PsuWatchdog,
            WatchdogOp::PsuHeartbeat => DspicOpcode::PsuHeartbeat,
            WatchdogOp::DspicHeartbeat => DspicOpcode::Heartbeat,
        }
    }
}

/// Watchdog policy configuration. Defaults match bosminer-canonical.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WatchdogPolicy {
    /// PSU heartbeat tick interval in milliseconds.
    pub psu_heartbeat_interval_ms: u64,
    /// PSU watchdog timeout in seconds. PSU cuts power if no heartbeat
    /// within this window.
    pub psu_timeout_s: u32,
    /// dsPIC heartbeat tick interval in milliseconds.
    pub dspic_heartbeat_interval_ms: u64,
    /// dsPIC watchdog timeout in seconds.
    pub dspic_timeout_s: u32,
    /// Number of consecutive successful heartbeats required before
    /// SET_VOLTAGE is permitted. HARD-MANDATED at 5.
    pub stable_tick_gate: u8,
    /// Number of consecutive PSU disarm writes required to actually
    /// disarm the watchdog (PSU is sensitive to single-write loss; the
    /// triple-write gives 3× redundancy).
    pub disarm_triple_write_count: u8,
    /// Gap between consecutive PSU disarm writes, in milliseconds.
    pub disarm_gap_ms: u64,
}

impl WatchdogPolicy {
    /// Bosminer-canonical defaults from
    /// .
    pub const fn bosminer_canonical() -> Self {
        Self {
            psu_heartbeat_interval_ms: 1000,
            psu_timeout_s: 30,
            dspic_heartbeat_interval_ms: 1000,
            dspic_timeout_s: 10,
            stable_tick_gate: 5,
            disarm_triple_write_count: 3,
            disarm_gap_ms: 1000,
        }
    }
}

impl Default for WatchdogPolicy {
    fn default() -> Self {
        Self::bosminer_canonical()
    }
}

/// Build the canonical PSU disarm sequence — three identical
/// `[0x55, 0xAA, 0x04, 0x81, 0x00, 0x85]` frames per the RE doc.
pub fn psu_disarm_frames() -> [DspicFrame; 3] {
    [
        DspicFrame::new(DspicOpcode::PsuWatchdog, vec![0x00]),
        DspicFrame::new(DspicOpcode::PsuWatchdog, vec![0x00]),
        DspicFrame::new(DspicOpcode::PsuWatchdog, vec![0x00]),
    ]
}

/// Build the canonical PSU arm frame.
pub fn psu_arm_frame() -> DspicFrame {
    DspicFrame::new(DspicOpcode::PsuWatchdog, vec![0x01])
}

/// Build the canonical PSU heartbeat frame.
pub fn psu_heartbeat_frame() -> DspicFrame {
    DspicFrame::new(DspicOpcode::PsuHeartbeat, vec![0x00])
}

/// Build the canonical dsPIC heartbeat frame.
pub fn dspic_heartbeat_frame() -> DspicFrame {
    DspicFrame::new(DspicOpcode::Heartbeat, vec![0x00])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bosminer_canonical_constants_pinned() {
        let p = WatchdogPolicy::bosminer_canonical();
        assert_eq!(p.psu_heartbeat_interval_ms, 1000);
        assert_eq!(p.psu_timeout_s, 30);
        assert_eq!(p.dspic_heartbeat_interval_ms, 1000);
        assert_eq!(p.dspic_timeout_s, 10);
        assert_eq!(p.stable_tick_gate, 5);
        assert_eq!(p.disarm_triple_write_count, 3);
        assert_eq!(p.disarm_gap_ms, 1000);
    }

    #[test]
    fn default_equals_bosminer_canonical() {
        let p = WatchdogPolicy::default();
        let bc = WatchdogPolicy::bosminer_canonical();
        // Manual field-by-field check (no PartialEq derive on f64-free struct
        // is fine; this is explicit so a refactor that diverges defaults
        // will fail loudly).
        assert_eq!(p.psu_heartbeat_interval_ms, bc.psu_heartbeat_interval_ms);
        assert_eq!(p.stable_tick_gate, bc.stable_tick_gate);
    }

    #[test]
    fn five_tick_gate_is_mandatory_minimum() {
        //  invariant: stable_tick_gate must NEVER drop below 5.
        //, dropping
        // below 5 corrupts the PIC MSSP parser permanently.
        let p = WatchdogPolicy::default();
        assert!(
            p.stable_tick_gate >= 5,
            "stable_tick_gate must be >= 5; got {}",
            p.stable_tick_gate
        );
    }

    #[test]
    fn watchdog_op_to_dspic_opcode() {
        assert_eq!(
            WatchdogOp::PsuArmDisarm.dspic_opcode(),
            DspicOpcode::PsuWatchdog
        );
        assert_eq!(
            WatchdogOp::PsuHeartbeat.dspic_opcode(),
            DspicOpcode::PsuHeartbeat
        );
        assert_eq!(
            WatchdogOp::DspicHeartbeat.dspic_opcode(),
            DspicOpcode::Heartbeat
        );
    }

    #[test]
    fn psu_disarm_sequence_is_three_identical_frames() {
        let frames = psu_disarm_frames();
        assert_eq!(frames.len(), 3);
        let expected_wire = [0x55, 0xAA, 0x04, 0x81, 0x00, 0x85];
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                f.encode_framed_sum(),
                expected_wire,
                "disarm frame {} mismatch",
                i
            );
        }
    }

    #[test]
    fn psu_arm_frame_encodes_to_canonical_wire() {
        let f = psu_arm_frame();
        assert_eq!(f.encode_framed_sum(), [0x55, 0xAA, 0x04, 0x81, 0x01, 0x86]);
    }

    #[test]
    fn psu_heartbeat_frame_encodes_to_canonical_wire() {
        let f = psu_heartbeat_frame();
        assert_eq!(f.encode_framed_sum(), [0x55, 0xAA, 0x04, 0x84, 0x00, 0x88]);
    }

    #[test]
    fn dspic_heartbeat_frame_encodes_to_canonical_wire() {
        let f = dspic_heartbeat_frame();
        assert_eq!(f.encode_framed_sum(), [0x55, 0xAA, 0x04, 0x16, 0x00, 0x1A]);
    }

    #[test]
    fn watchdog_op_round_trips_through_serde() {
        let ops = [
            WatchdogOp::PsuArmDisarm,
            WatchdogOp::PsuHeartbeat,
            WatchdogOp::DspicHeartbeat,
        ];
        for op in ops {
            let json = serde_json::to_string(&op).unwrap();
            let back: WatchdogOp = serde_json::from_str(&json).unwrap();
            assert_eq!(op, back);
        }
    }

    #[test]
    fn psu_timeout_must_be_at_least_one_heartbeat_window() {
        let p = WatchdogPolicy::default();
        // The PSU heartbeat sends every 1 s; the timeout must be at
        // least one heartbeat (otherwise we'd self-time-out). bosminer
        // canon is 30 s = 30 heartbeats of slack.
        let timeout_ms = (p.psu_timeout_s as u64) * 1000;
        assert!(timeout_ms >= p.psu_heartbeat_interval_ms);
    }

    #[test]
    fn disarm_triple_write_count_is_three() {
        // Per RE doc Phase B3 lines 137-139: PSU disarm sequence is
        // x1, x2, x3 — three writes.
        let p = WatchdogPolicy::default();
        assert_eq!(p.disarm_triple_write_count, 3);
    }
}
