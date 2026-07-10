//! Persistent safety event logging.
//!
//! Logs safety-critical events to `/data/dcent/events.log` with:
//! - Rolling 24h retention (keeps last ~50KB)
//! - fsync every 60s during normal operation
//! - Immediate fsync on emergency events
//!
//! Event types cover the full safety surface: temperature sensor failures,
//! fan failures, emergency shutdowns, power limit hits, voltage changes,
//! tuning completions, post-tune rollbacks, and chip masking.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::mpsc;
use std::time::Instant;

use serde::Serialize;

/// Path to the persistent event log file.
const EVENT_LOG_PATH: &str = "/data/dcent/events.log";

/// Maximum log file size before rotation (~50KB).
const MAX_LOG_SIZE: u64 = 50 * 1024;

/// Interval between fsync calls in seconds.
const FSYNC_INTERVAL_S: u64 = 60;

/// Safety event types logged to persistent storage.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum SafetyEventType {
    /// Temperature sensor stopped providing data.
    TempSensorFailure {
        chain_id: u8,
        consecutive_missing: u32,
    },
    /// Fan RPM dropped to zero while PWM was active.
    FanFailure { pwm: u8, consecutive_zero: u32 },
    /// Emergency thermal shutdown triggered.
    EmergencyShutdown { temp_c: Option<f32>, reason: String },
    /// Power limit hit — autotuner clamped to budget.
    PowerLimitHit { target_w: u32, actual_w: u32 },
    /// Voltage changed on a chain.
    VoltageChange {
        chain_id: u8,
        old_mv: u16,
        new_mv: u16,
    },
    /// Tuning completed for a chain.
    TuningComplete {
        chain_id: u8,
        chips_tuned: u32,
        duration_s: f64,
    },
    /// Post-tune rollback triggered due to elevated error rates.
    PostTuneRollback {
        chain_id: u8,
        error_rate: f64,
        threshold: f64,
    },
    /// Chip permanently masked (dead chip detection).
    ChipMasked {
        chain_id: u8,
        chip_index: u8,
        reason: String,
    },
}

/// A single safety event with timestamp.
#[derive(Debug, Clone, Serialize)]
pub struct SafetyEvent {
    /// Seconds since daemon start.
    pub uptime_s: f64,
    /// Event details.
    #[serde(flatten)]
    pub event: SafetyEventType,
}

/// Persistent event logger with dedicated background I/O thread.
///
/// Uses an mpsc channel to decouple event submission from file I/O.
/// This prevents NAND fsync from blocking the tokio runtime on
/// single-core ARM (Zynq Cortex-A9).
pub struct EventLogger {
    sender: mpsc::Sender<SafetyEvent>,
    start: Instant,
}

impl EventLogger {
    /// Create a new event logger.
    ///
    /// Spawns a dedicated background thread that handles file open, write,
    /// rotation, and fsync. The calling thread never blocks on NAND I/O.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<SafetyEvent>();

        std::thread::Builder::new()
            .name("dcent-event-log".to_string())
            .spawn(move || {
                Self::io_thread(rx);
            })
            .expect("failed to spawn event log thread");

        Self {
            sender: tx,
            start: Instant::now(),
        }
    }

    /// Background I/O thread: receives events, writes to file, handles rotation and fsync.
    fn io_thread(rx: mpsc::Receiver<SafetyEvent>) {
        let mut writer = Self::open_log_file();
        let mut last_fsync = Instant::now();

        while let Ok(event) = rx.recv() {
            let is_emergency = matches!(
                event.event,
                SafetyEventType::EmergencyShutdown { .. } | SafetyEventType::FanFailure { .. }
            );

            if let Some(ref mut w) = writer {
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = writeln!(w, "{}", json);

                    if is_emergency {
                        // Immediate fsync for emergency events
                        let _ = w.flush();
                        if let Ok(ref inner) = w.get_ref().try_clone() {
                            let _ = inner.sync_all();
                        }
                    } else if last_fsync.elapsed().as_secs() >= FSYNC_INTERVAL_S {
                        // Periodic fsync for non-emergency events
                        let _ = w.flush();
                        if let Ok(ref inner) = w.get_ref().try_clone() {
                            let _ = inner.sync_all();
                        }
                        last_fsync = Instant::now();
                    }
                }
            }

            // Check for file rotation after each write
            if let Ok(meta) = std::fs::metadata(EVENT_LOG_PATH) {
                if meta.len() > MAX_LOG_SIZE {
                    // Flush before rotation
                    if let Some(ref mut w) = writer {
                        let _ = w.flush();
                    }
                    let backup = format!("{}.old", EVENT_LOG_PATH);
                    let _ = std::fs::rename(EVENT_LOG_PATH, &backup);
                    writer = Self::open_log_file();
                }
            }
        }
    }

    fn open_log_file() -> Option<BufWriter<File>> {
        // Ensure directory exists
        if let Some(parent) = std::path::Path::new(EVENT_LOG_PATH).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Rotate if too large
        if let Ok(meta) = std::fs::metadata(EVENT_LOG_PATH) {
            if meta.len() > MAX_LOG_SIZE {
                let backup = format!("{}.old", EVENT_LOG_PATH);
                let _ = std::fs::rename(EVENT_LOG_PATH, &backup);
            }
        }

        OpenOptions::new()
            .create(true)
            .append(true)
            .open(EVENT_LOG_PATH)
            .ok()
            .map(BufWriter::new)
    }

    /// Log a safety event.
    ///
    /// Serializes the event and sends it to the background I/O thread via
    /// mpsc channel. This is non-blocking — the caller never waits for
    /// NAND fsync. If the background thread has crashed, the event is
    /// silently dropped (logging should never crash the daemon).
    pub fn log(&self, event_type: SafetyEventType) {
        let event = SafetyEvent {
            uptime_s: self.start.elapsed().as_secs_f64(),
            event: event_type,
        };

        // Non-blocking send — silently drop if channel is full or closed.
        // Logging must never crash the daemon.
        let _ = self.sender.send(event);
    }
}

impl Default for EventLogger {
    fn default() -> Self {
        Self::new()
    }
}
