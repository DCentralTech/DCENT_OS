//! Diagnostic test orchestration and report generation for dcentrald.
//!
//! Manages the lifecycle of diagnostic tests, streams progress via WebSocket,
//! and generates reports. Supports both native Rust tests and Phase 1
//! subprocess-based tests (wrapping existing Python tools).
//!
//! Modules:
//! - `hashreport`   - HashReport 15-minute test drive
//! - `chip_health`  - Per-chip health scoring and ChipMap
//! - `board_health` - Per-board health test
//! - `troubleshoot` - Instant troubleshooting tools
//! - `report`       - HTML/PDF report generation (askama templates)
//! - `progress`     - Progress tracking and WebSocket push
//! - `subprocess`   - Phase 1 Python subprocess wrapper
//!
//! # Diagnostic evidence migration
//!
//! Passing repair/manufacturing grades require typed, directly measured
//! evidence. Legacy serialized results have no trustworthy provenance, so new
//! evidence fields default to `Unavailable` and A/B grades are withheld when
//! recalculated. Current runtime-snapshot producers deliberately label voltage
//! as `Commanded` and cumulative CRC state as `Inferred`; board model metadata
//! is not treated as EEPROM presence or validation. Consequently snapshots are
//! useful triage records, but cannot claim a measured pass.
//!
//! Dedicated hardware-test producers should migrate each observation to
//! `DiagnosticEvidence::measured` or `measured_validated`, name the concrete
//! sensor/protocol/checksum source, attach an observation time when available,
//! and keep the evidence value identical to the field being graded. Residual
//! producer gap: `SnapshotChain` has no voltage-readback, bounded CRC-window,
//! or EEPROM read/checksum provenance, so snapshot reports remain capped until
//! those data paths expose direct observations.

pub mod board_health;
pub mod builders;
/// Pure chip anomaly math bridge (`dcentrald-chip-analysis`).
pub mod chip_analysis_bridge;
pub mod chip_health;
pub mod evidence;
pub mod hashreport;
pub mod progress;
pub mod report;
pub mod snapshot;
pub mod subprocess;
pub mod troubleshoot;

pub use chip_analysis_bridge::{
    analyze_chip, enrich_cell_anomalies, ChipAnalysis, ChipAnomalyScores,
};
pub use evidence::{DiagnosticEvidence, EvidenceKind, EvidenceQuality};

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::progress::{DiagnosticProgress, ProgressTracker};

/// Diagnostic subsystem error type.
#[derive(Debug, Error)]
pub enum DiagnosticError {
    /// Test not found.
    #[error("test not found: {test_id}")]
    TestNotFound { test_id: Uuid },

    /// Test already running.
    #[error("a test of type {test_type} is already running")]
    TestAlreadyRunning { test_type: String },

    /// HAL error during diagnostic test.
    #[error("HAL error: {0}")]
    Hal(#[from] dcentrald_hal::HalError),

    /// ASIC error during diagnostic test.
    #[error("ASIC error: {0}")]
    Asic(#[from] dcentrald_asic::AsicError),

    /// Subprocess execution error.
    #[error("subprocess error: {0}")]
    Subprocess(String),

    /// Report generation error.
    #[error("report generation error: {0}")]
    ReportGeneration(String),

    /// Generic I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DiagnosticError>;

/// Types of diagnostic tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestType {
    /// 15-minute comprehensive test drive.
    HashReport,
    /// Per-chip health scoring (5 min).
    ChipHealth,
    /// Per-board health test (2 min).
    BoardHealth,
    /// Instant network diagnostics.
    NetworkTest,
    /// PSU PMBus readings.
    PsuProbe,
    /// FPGA register status.
    FpgaStatus,
    /// ASIC communication test.
    AsicCommTest,
    /// I2C bus scan.
    I2cScan,
}

/// Persisted lifecycle state for a diagnostic job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Status snapshot for a diagnostic job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTest {
    /// Unique test identifier.
    pub test_id: Uuid,
    /// Type of test being run.
    pub test_type: TestType,
    /// Current lifecycle state.
    pub status: TestStatus,
    /// Human-readable phase name.
    pub phase_name: String,
    /// Progress percentage (0-100).
    pub progress_pct: u8,
    /// Most recent detail message.
    pub detail: String,
    /// Elapsed seconds at the latest update.
    pub elapsed_s: u64,
    /// Unix timestamp when the job was started.
    pub started_at_epoch_s: u64,
    /// Unix timestamp when the job finished, if complete.
    pub completed_at_epoch_s: Option<u64>,
    /// Final result payload, if complete.
    pub result: Option<TestResult>,
    /// Failure message, if the job failed.
    pub error: Option<String>,
    /// Cancellation token for early termination.
    #[serde(skip)]
    pub cancel_token: CancellationToken,
}

/// Completed test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    /// Unique test identifier.
    pub test_id: Uuid,
    /// Type of test that was run.
    pub test_type: TestType,
    /// Duration of the test in seconds.
    pub duration_s: u64,
    /// Test result data (JSON).
    pub data: serde_json::Value,
    /// Overall grade (if applicable).
    pub grade: Option<String>,
    /// Warnings generated during the test.
    pub warnings: Vec<String>,
    /// Recommendations for the user.
    pub recommendations: Vec<String>,
}

/// Async finalizer invoked after the timed test window ends.
///
/// Report rendering/persistence may use a bounded blocking owner. Returning a
/// future keeps that work off the Tokio worker that drives diagnostic progress.
pub type FinalizeTestFuture = Pin<Box<dyn Future<Output = Result<TestResult>> + Send + 'static>>;
pub type FinalizeTestFn = Arc<dyn Fn(Uuid, u64) -> FinalizeTestFuture + Send + Sync + 'static>;

/// Runtime options for the first timed HashReport engine step.
pub struct HashReportJobConfig {
    /// Total timed mining window for the job.
    pub duration: Duration,
    /// How often progress updates should be emitted while timing runs.
    pub progress_interval: Duration,
    /// Finalizer that converts the finished job into a persisted report/result.
    pub finalize: FinalizeTestFn,
}

/// Per-test start configuration.
pub enum DiagnosticJobConfig {
    HashReport(HashReportJobConfig),
}

/// Top-level diagnostic service.
///
/// Manages the lifecycle of diagnostic tests, tracks active and completed
/// tests, and provides progress streaming.
pub struct DiagnosticService {
    /// Shared job store keyed by test_id.
    jobs: Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    /// Recently completed tests (ring buffer, max 10).
    completed_tests: Arc<Mutex<VecDeque<Uuid>>>,
    /// Maximum number of completed tests to keep.
    max_completed: usize,
    /// Broadcast sender for real-time progress updates.
    progress_tx: broadcast::Sender<DiagnosticProgress>,
}

impl DiagnosticService {
    /// Create a new diagnostic service.
    pub fn new(progress_tx: broadcast::Sender<DiagnosticProgress>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            completed_tests: Arc::new(Mutex::new(VecDeque::with_capacity(10))),
            max_completed: 10,
            progress_tx,
        }
    }

    /// Start a new diagnostic test.
    ///
    /// Returns the test ID if started successfully, or an error if a test
    /// of the same type is already running.
    pub fn start_test(&mut self, test_type: TestType, config: DiagnosticJobConfig) -> Result<Uuid> {
        // Check if a test of this type is already running
        if let Ok(jobs) = self.jobs.lock() {
            for test in jobs.values() {
                if test.test_type == test_type && test.status == TestStatus::Running {
                    return Err(DiagnosticError::TestAlreadyRunning {
                        test_type: format!("{:?}", test_type),
                    });
                }
            }
        }

        let test_id = Uuid::new_v4();
        let cancel_token = CancellationToken::new();
        let started_at_epoch_s = unix_now_s();

        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(
                test_id,
                StoredTest {
                    test_id,
                    test_type,
                    status: TestStatus::Running,
                    phase_name: "queued".to_string(),
                    progress_pct: 0,
                    detail: "Diagnostic job created".to_string(),
                    elapsed_s: 0,
                    started_at_epoch_s,
                    completed_at_epoch_s: None,
                    result: None,
                    error: None,
                    cancel_token: cancel_token.clone(),
                },
            );
        }

        match config {
            DiagnosticJobConfig::HashReport(config) => {
                let jobs = Arc::clone(&self.jobs);
                let completed_tests = Arc::clone(&self.completed_tests);
                let progress_tx = self.progress_tx.clone();
                let max_completed = self.max_completed;
                tokio::spawn(async move {
                    run_hashreport_job(
                        test_id,
                        config,
                        progress_tx,
                        jobs,
                        completed_tests,
                        max_completed,
                        cancel_token,
                    )
                    .await;
                });
            }
        }

        Ok(test_id)
    }

    /// Get the status of a running test.
    pub fn get_test_status(&self, test_id: &Uuid) -> Option<StoredTest> {
        self.jobs.lock().ok()?.get(test_id).cloned()
    }

    /// Get a completed test result.
    pub fn get_result(&self, test_id: &Uuid) -> Option<TestResult> {
        self.jobs.lock().ok()?.get(test_id)?.result.clone()
    }

    /// Cancel a running test.
    pub fn cancel_test(&mut self, test_id: &Uuid) -> bool {
        let Some(cancel_token) = self
            .jobs
            .lock()
            .ok()
            .and_then(|jobs| jobs.get(test_id).map(|test| test.cancel_token.clone()))
        else {
            return false;
        };

        cancel_token.cancel();
        true
    }
}

impl Default for DiagnosticService {
    fn default() -> Self {
        let (progress_tx, _) = broadcast::channel(32);
        Self::new(progress_tx)
    }
}

async fn run_hashreport_job(
    test_id: Uuid,
    config: HashReportJobConfig,
    progress_tx: broadcast::Sender<DiagnosticProgress>,
    jobs: Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    completed_tests: Arc<Mutex<VecDeque<Uuid>>>,
    max_completed: usize,
    cancel_token: CancellationToken,
) {
    let mut tracker = ProgressTracker::new(test_id, TestType::HashReport, 5, progress_tx);

    tracker.next_phase(
        "system_identification",
        "Capturing miner identity and runtime topology",
    );
    update_job_progress(
        &jobs,
        test_id,
        &DiagnosticProgress::new(
            test_id,
            TestType::HashReport,
            1,
            "system_identification",
            5,
            0,
            config.duration.as_secs(),
            "Capturing miner identity and runtime topology",
        ),
    );
    if wait_or_cancel(Duration::from_secs(1), &cancel_token).await {
        tracker.cancel();
        mark_job_cancelled(
            &jobs,
            &completed_tests,
            test_id,
            max_completed,
            tracker.elapsed_s(),
        );
        return;
    }

    tracker.next_phase(
        "baseline_capture",
        "Recording baseline temps, fans, and errors",
    );
    update_job_progress(
        &jobs,
        test_id,
        &DiagnosticProgress::new(
            test_id,
            TestType::HashReport,
            2,
            "baseline_capture",
            10,
            tracker.elapsed_s(),
            config.duration.as_secs(),
            "Recording baseline temps, fans, and errors",
        ),
    );
    if wait_or_cancel(Duration::from_secs(1), &cancel_token).await {
        tracker.cancel();
        mark_job_cancelled(
            &jobs,
            &completed_tests,
            test_id,
            max_completed,
            tracker.elapsed_s(),
        );
        return;
    }

    tracker.next_phase(
        "mining_performance",
        "Timed mining observation is in progress",
    );
    let mining_start = Instant::now();
    while mining_start.elapsed() < config.duration {
        if cancel_token.is_cancelled() {
            tracker.cancel();
            mark_job_cancelled(
                &jobs,
                &completed_tests,
                test_id,
                max_completed,
                tracker.elapsed_s(),
            );
            return;
        }

        let elapsed_window_s = mining_start.elapsed().as_secs();
        let total_window_s = config.duration.as_secs().max(1);
        let mining_pct = ((elapsed_window_s.saturating_mul(80)) / total_window_s).min(80) as u8;
        let overall_pct = 10u8.saturating_add(mining_pct);
        let remaining_s = total_window_s.saturating_sub(elapsed_window_s);
        let detail = format!(
            "Timed observation {}/{}s complete; final report will be generated from runtime state at the end of the window",
            elapsed_window_s.min(total_window_s),
            total_window_s
        );
        tracker.update(overall_pct, detail.clone());
        update_job_progress(
            &jobs,
            test_id,
            &DiagnosticProgress::new(
                test_id,
                TestType::HashReport,
                3,
                "mining_performance",
                overall_pct,
                tracker.elapsed_s(),
                remaining_s,
                detail,
            ),
        );

        if wait_or_cancel(config.progress_interval, &cancel_token).await {
            tracker.cancel();
            mark_job_cancelled(
                &jobs,
                &completed_tests,
                test_id,
                max_completed,
                tracker.elapsed_s(),
            );
            return;
        }
    }

    tracker.next_phase(
        "chip_health_scoring",
        "Summarizing timed observations into per-board health output",
    );
    update_job_progress(
        &jobs,
        test_id,
        &DiagnosticProgress::new(
            test_id,
            TestType::HashReport,
            4,
            "chip_health_scoring",
            92,
            tracker.elapsed_s(),
            1,
            "Summarizing timed observations into per-board health output",
        ),
    );
    if wait_or_cancel(Duration::from_secs(1), &cancel_token).await {
        tracker.cancel();
        mark_job_cancelled(
            &jobs,
            &completed_tests,
            test_id,
            max_completed,
            tracker.elapsed_s(),
        );
        return;
    }

    tracker.next_phase("report_generation", "Persisting final diagnostic artifacts");
    update_job_progress(
        &jobs,
        test_id,
        &DiagnosticProgress::new(
            test_id,
            TestType::HashReport,
            5,
            "report_generation",
            97,
            tracker.elapsed_s(),
            0,
            "Persisting final diagnostic artifacts",
        ),
    );

    match (config.finalize)(test_id, tracker.elapsed_s()).await {
        Ok(result) => {
            tracker.complete();
            mark_job_completed(
                &jobs,
                &completed_tests,
                test_id,
                result,
                max_completed,
                tracker.elapsed_s(),
            );
        }
        Err(error) => {
            tracker.fail(error.to_string());
            mark_job_failed(
                &jobs,
                &completed_tests,
                test_id,
                error.to_string(),
                max_completed,
                tracker.elapsed_s(),
            );
        }
    }
}

async fn wait_or_cancel(duration: Duration, cancel_token: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel_token.cancelled() => true,
        _ = tokio::time::sleep(duration) => false,
    }
}

fn update_job_progress(
    jobs: &Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    test_id: Uuid,
    progress: &DiagnosticProgress,
) {
    if let Ok(mut jobs) = jobs.lock() {
        if let Some(job) = jobs.get_mut(&test_id) {
            job.phase_name = progress.phase_name.clone();
            job.progress_pct = progress.progress_pct;
            job.detail = progress.detail.clone();
            job.elapsed_s = progress.elapsed_s;
        }
    }
}

fn mark_job_completed(
    jobs: &Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    completed_tests: &Arc<Mutex<VecDeque<Uuid>>>,
    test_id: Uuid,
    result: TestResult,
    max_completed: usize,
    elapsed_s: u64,
) {
    if let Ok(mut jobs) = jobs.lock() {
        if let Some(job) = jobs.get_mut(&test_id) {
            job.status = TestStatus::Completed;
            job.phase_name = "completed".to_string();
            job.progress_pct = 100;
            job.detail = "Test completed successfully".to_string();
            job.elapsed_s = elapsed_s;
            job.completed_at_epoch_s = Some(unix_now_s());
            job.result = Some(result);
            job.error = None;
        }
    }
    push_completed_test(jobs, completed_tests, test_id, max_completed);
}

fn mark_job_failed(
    jobs: &Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    completed_tests: &Arc<Mutex<VecDeque<Uuid>>>,
    test_id: Uuid,
    error: String,
    max_completed: usize,
    elapsed_s: u64,
) {
    if let Ok(mut jobs) = jobs.lock() {
        if let Some(job) = jobs.get_mut(&test_id) {
            job.status = TestStatus::Failed;
            job.phase_name = "failed".to_string();
            job.progress_pct = 0;
            job.detail = error.clone();
            job.elapsed_s = elapsed_s;
            job.completed_at_epoch_s = Some(unix_now_s());
            job.result = None;
            job.error = Some(error);
        }
    }
    push_completed_test(jobs, completed_tests, test_id, max_completed);
}

fn mark_job_cancelled(
    jobs: &Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    completed_tests: &Arc<Mutex<VecDeque<Uuid>>>,
    test_id: Uuid,
    max_completed: usize,
    elapsed_s: u64,
) {
    if let Ok(mut jobs) = jobs.lock() {
        if let Some(job) = jobs.get_mut(&test_id) {
            job.status = TestStatus::Cancelled;
            job.phase_name = "cancelled".to_string();
            job.progress_pct = 0;
            job.detail = "Test cancelled by user".to_string();
            job.elapsed_s = elapsed_s;
            job.completed_at_epoch_s = Some(unix_now_s());
            job.result = None;
            job.error = None;
        }
    }
    push_completed_test(jobs, completed_tests, test_id, max_completed);
}

fn push_completed_test(
    jobs: &Arc<Mutex<HashMap<Uuid, StoredTest>>>,
    completed_tests: &Arc<Mutex<VecDeque<Uuid>>>,
    test_id: Uuid,
    max_completed: usize,
) {
    let evicted = if let Ok(mut completed) = completed_tests.lock() {
        completed.push_back(test_id);
        if completed.len() > max_completed {
            completed.pop_front()
        } else {
            None
        }
    } else {
        None
    };

    if let Some(oldest_id) = evicted {
        if let Ok(mut jobs) = jobs.lock() {
            jobs.remove(&oldest_id);
        }
    }
}

fn unix_now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
