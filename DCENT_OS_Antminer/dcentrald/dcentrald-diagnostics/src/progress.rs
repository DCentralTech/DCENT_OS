//! Diagnostic progress tracking and WebSocket push.
//!
//! Provides real-time progress updates for long-running diagnostic tests.
//! Progress is streamed to connected WebSocket clients as JSON messages.
//!
//! WebSocket diagnostic progress message format:
//! ```json
//! {
//!   "type": "diagnostic_progress",
//!   "test_id": "uuid",
//!   "test_type": "hashreport",
//!   "phase": 3,
//!   "phase_name": "mining_performance",
//!   "progress_pct": 45,
//!   "elapsed_s": 420,
//!   "eta_s": 480,
//!   "detail": "Window 6/12 -- 63 chips responding, avg 226 GH/s per chip"
//! }
//! ```

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::TestType;

/// Real-time diagnostic progress update.
///
/// Sent via WebSocket to connected dashboard clients whenever a
/// diagnostic test advances to a new phase or makes measurable progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticProgress {
    /// Message type identifier (always "diagnostic_progress").
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Test identifier.
    pub test_id: Uuid,
    /// Type of test.
    pub test_type: TestType,
    /// Current phase number (0-indexed).
    pub phase: u8,
    /// Human-readable phase name.
    pub phase_name: String,
    /// Overall progress percentage (0-100).
    pub progress_pct: u8,
    /// Elapsed time in seconds since test start.
    pub elapsed_s: u64,
    /// Estimated time remaining in seconds.
    pub eta_s: u64,
    /// Detailed status message for the current phase.
    pub detail: String,
}

impl DiagnosticProgress {
    /// Create a new progress update.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        test_id: Uuid,
        test_type: TestType,
        phase: u8,
        phase_name: impl Into<String>,
        progress_pct: u8,
        elapsed_s: u64,
        eta_s: u64,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            msg_type: "diagnostic_progress".to_string(),
            test_id,
            test_type,
            phase,
            phase_name: phase_name.into(),
            progress_pct: progress_pct.min(100),
            elapsed_s,
            eta_s,
            detail: detail.into(),
        }
    }

    /// Create a progress update indicating test completion.
    pub fn completed(test_id: Uuid, test_type: TestType, elapsed_s: u64) -> Self {
        Self {
            msg_type: "diagnostic_progress".to_string(),
            test_id,
            test_type,
            phase: u8::MAX,
            phase_name: "completed".to_string(),
            progress_pct: 100,
            elapsed_s,
            eta_s: 0,
            detail: "Test completed successfully".to_string(),
        }
    }

    /// Create a progress update indicating test failure.
    pub fn failed(
        test_id: Uuid,
        test_type: TestType,
        elapsed_s: u64,
        error: impl Into<String>,
    ) -> Self {
        Self {
            msg_type: "diagnostic_progress".to_string(),
            test_id,
            test_type,
            phase: u8::MAX,
            phase_name: "failed".to_string(),
            progress_pct: 0,
            elapsed_s,
            eta_s: 0,
            detail: error.into(),
        }
    }

    /// Create a progress update indicating test cancellation.
    pub fn cancelled(test_id: Uuid, test_type: TestType, elapsed_s: u64) -> Self {
        Self {
            msg_type: "diagnostic_progress".to_string(),
            test_id,
            test_type,
            phase: u8::MAX,
            phase_name: "cancelled".to_string(),
            progress_pct: 0,
            elapsed_s,
            eta_s: 0,
            detail: "Test cancelled by user".to_string(),
        }
    }
}

/// Progress tracker for a single diagnostic test.
///
/// Maintains the current phase and progress state, and provides
/// a `broadcast::Sender` for pushing updates to WebSocket clients.
pub struct ProgressTracker {
    /// Test identifier.
    test_id: Uuid,
    /// Test type.
    test_type: TestType,
    /// Start time (monotonic).
    started_at: std::time::Instant,
    /// Total expected phases.
    total_phases: u8,
    /// Current phase index.
    current_phase: u8,
    /// Progress sender channel (broadcast to all WebSocket clients).
    progress_tx: tokio::sync::broadcast::Sender<DiagnosticProgress>,
}

impl ProgressTracker {
    /// Create a new progress tracker.
    pub fn new(
        test_id: Uuid,
        test_type: TestType,
        total_phases: u8,
        progress_tx: tokio::sync::broadcast::Sender<DiagnosticProgress>,
    ) -> Self {
        Self {
            test_id,
            test_type,
            started_at: std::time::Instant::now(),
            total_phases,
            current_phase: 0,
            progress_tx,
        }
    }

    /// Advance to the next phase.
    pub fn next_phase(&mut self, phase_name: impl Into<String>, detail: impl Into<String>) {
        self.current_phase += 1;
        let elapsed = self.started_at.elapsed().as_secs();
        let progress_pct = if self.total_phases > 0 {
            ((self.current_phase as u16 * 100) / self.total_phases as u16) as u8
        } else {
            0
        };
        let eta_s = if self.current_phase > 0 {
            let per_phase = elapsed / self.current_phase as u64;
            let remaining = self.total_phases.saturating_sub(self.current_phase) as u64;
            per_phase * remaining
        } else {
            0
        };

        let progress = DiagnosticProgress::new(
            self.test_id,
            self.test_type,
            self.current_phase,
            phase_name,
            progress_pct,
            elapsed,
            eta_s,
            detail,
        );

        // Ignore send errors (no active receivers is OK)
        let _ = self.progress_tx.send(progress);
    }

    /// Send a progress update within the current phase.
    pub fn update(&self, progress_pct: u8, detail: impl Into<String>) {
        let elapsed = self.started_at.elapsed().as_secs();
        let eta_s = if progress_pct > 0 {
            let total_estimated = elapsed as f64 * 100.0 / progress_pct as f64;
            (total_estimated - elapsed as f64).max(0.0) as u64
        } else {
            0
        };

        let progress = DiagnosticProgress::new(
            self.test_id,
            self.test_type,
            self.current_phase,
            format!("phase_{}", self.current_phase),
            progress_pct,
            elapsed,
            eta_s,
            detail,
        );

        let _ = self.progress_tx.send(progress);
    }

    /// Send a completion update.
    pub fn complete(&self) {
        let elapsed = self.started_at.elapsed().as_secs();
        let progress = DiagnosticProgress::completed(self.test_id, self.test_type, elapsed);
        let _ = self.progress_tx.send(progress);
    }

    /// Send a failure update.
    pub fn fail(&self, error: impl Into<String>) {
        let elapsed = self.started_at.elapsed().as_secs();
        let progress = DiagnosticProgress::failed(self.test_id, self.test_type, elapsed, error);
        let _ = self.progress_tx.send(progress);
    }

    /// Send a cancellation update.
    pub fn cancel(&self) {
        let elapsed = self.started_at.elapsed().as_secs();
        let progress = DiagnosticProgress::cancelled(self.test_id, self.test_type, elapsed);
        let _ = self.progress_tx.send(progress);
    }

    /// Get elapsed time since test start.
    pub fn elapsed_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Get the test ID.
    pub fn test_id(&self) -> Uuid {
        self.test_id
    }
}
