//! Bounded ownership for blocking runtime feeder threads.
//!
//! Hardware watchdog and heartbeat workers are ordinary OS threads because
//! their transports are blocking.  They must still obey the async runtime's
//! lifecycle: cancellation is broadcast once, every worker shares one total
//! shutdown deadline, and dropping an owner must never block an async executor
//! thread indefinitely.

use std::thread::JoinHandle;
use std::time::{Duration, Instant as StdInstant};

use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const THREAD_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Sleep on a blocking worker while remaining responsive to cancellation.
///
/// Returns `true` when cancellation was observed. Long hardware-feed periods
/// must use this helper instead of one monolithic `thread::sleep`, otherwise a
/// nominal 20-second feed interval also becomes a 20-second shutdown delay.
pub(crate) fn sleep_until_cancelled(shutdown: &CancellationToken, duration: Duration) -> bool {
    let deadline = StdInstant::now() + duration;
    loop {
        if shutdown.is_cancelled() {
            return true;
        }
        let remaining = deadline.saturating_duration_since(StdInstant::now());
        if remaining.is_zero() {
            return shutdown.is_cancelled();
        }
        std::thread::sleep(THREAD_POLL_INTERVAL.min(remaining));
    }
}

/// Terminal state observed while reclaiming a blocking worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadStopOutcome {
    Joined,
    Panicked,
    TimedOut,
}

/// Per-worker shutdown evidence returned to the hardware owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadStopReport {
    pub(crate) name: &'static str,
    pub(crate) outcome: ThreadStopOutcome,
}

/// Aggregate result for one bounded shutdown operation.
#[derive(Debug, Default)]
pub(crate) struct ThreadStopSummary {
    reports: Vec<ThreadStopReport>,
}

impl ThreadStopSummary {
    pub(crate) fn any_timed_out(&self) -> bool {
        self.reports
            .iter()
            .any(|report| report.outcome == ThreadStopOutcome::TimedOut)
    }

    pub(crate) fn any_panicked(&self) -> bool {
        self.reports
            .iter()
            .any(|report| report.outcome == ThreadStopOutcome::Panicked)
    }

    #[cfg(test)]
    fn reports(&self) -> &[ThreadStopReport] {
        &self.reports
    }
}

/// Wait for one already-signalled worker without blocking a Tokio executor.
///
/// This is intentionally separate from [`RuntimeThreadGuard`] for legacy
/// owners whose stop flag is not a `CancellationToken`.  New multi-worker
/// runtimes should use the guard so all workers consume one total deadline.
pub(crate) async fn join_thread_bounded(
    handle: JoinHandle<()>,
    timeout: Duration,
) -> ThreadStopOutcome {
    let deadline = Instant::now() + timeout;
    let mut handle = Some(handle);

    loop {
        if handle.as_ref().is_some_and(JoinHandle::is_finished) {
            return classify_join(handle.take().expect("checked above"));
        }
        if Instant::now() >= deadline {
            // Dropping a JoinHandle detaches the worker.  The caller must use
            // its out-of-band safety path before touching a resource the
            // detached worker may still own.
            return ThreadStopOutcome::TimedOut;
        }
        sleep(THREAD_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

/// Owns blocking runtime workers and bounds their collective shutdown time.
pub(crate) struct RuntimeThreadGuard {
    shutdown: CancellationToken,
    handles: Vec<(&'static str, JoinHandle<()>)>,
}

impl RuntimeThreadGuard {
    pub(crate) fn new(shutdown: CancellationToken) -> Self {
        Self {
            shutdown,
            handles: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, name: &'static str, handle: JoinHandle<()>) {
        self.handles.push((name, handle));
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub(crate) fn contains(&self, name: &'static str) -> bool {
        self.handles
            .iter()
            .any(|(worker_name, _)| *worker_name == name)
    }

    pub(crate) fn request_stop(&self) {
        self.shutdown.cancel();
    }

    /// Cancel and reclaim every worker under one total deadline.
    ///
    /// Finished handles are joined only after `is_finished()` reports true.
    /// Remaining handles are detached at the deadline, allowing the caller to
    /// execute a transport-independent hard stop instead of deadlocking on a
    /// mutex still held by a wedged feeder.
    pub(crate) async fn stop_and_join(&mut self, total_timeout: Duration) -> ThreadStopSummary {
        self.request_stop();
        let deadline = Instant::now() + total_timeout;
        let mut reports = Vec::with_capacity(self.handles.len());

        while !self.handles.is_empty() {
            if let Some(index) = self
                .handles
                .iter()
                .position(|(_, handle)| handle.is_finished())
            {
                let (name, handle) = self.handles.swap_remove(index);
                let outcome = classify_join(handle);
                match outcome {
                    ThreadStopOutcome::Joined => info!(thread = name, "runtime thread joined"),
                    ThreadStopOutcome::Panicked => {
                        warn!(thread = name, "runtime thread panicked during shutdown")
                    }
                    ThreadStopOutcome::TimedOut => unreachable!("finished handle cannot time out"),
                }
                reports.push(ThreadStopReport { name, outcome });
                continue;
            }

            if Instant::now() >= deadline {
                for (name, _detached_handle) in self.handles.drain(..) {
                    warn!(
                        thread = name,
                        timeout_ms = total_timeout.as_millis(),
                        "runtime thread did not stop before the shared deadline; detaching"
                    );
                    reports.push(ThreadStopReport {
                        name,
                        outcome: ThreadStopOutcome::TimedOut,
                    });
                }
                break;
            }

            sleep(THREAD_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())))
                .await;
        }

        ThreadStopSummary { reports }
    }
}

impl Drop for RuntimeThreadGuard {
    fn drop(&mut self) {
        // Drop is deliberately non-blocking.  Cancellation-aware workers can
        // exit naturally; owners that need proof of quiescence must explicitly
        // await `stop_and_join` before releasing hardware resources.
        self.request_stop();
        self.handles.clear();
    }
}

fn classify_join(handle: JoinHandle<()>) -> ThreadStopOutcome {
    match handle.join() {
        Ok(()) => ThreadStopOutcome::Joined,
        Err(_) => ThreadStopOutcome::Panicked,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use tokio_util::sync::CancellationToken;

    use super::{sleep_until_cancelled, RuntimeThreadGuard, ThreadStopOutcome};

    #[test]
    fn long_worker_sleep_observes_cancellation_promptly() {
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let started = Instant::now();
        let handle =
            thread::spawn(move || sleep_until_cancelled(&worker_shutdown, Duration::from_secs(20)));

        thread::sleep(Duration::from_millis(30));
        shutdown.cancel();

        assert!(handle.join().unwrap());
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test]
    async fn joins_responsive_and_classifies_panicked_workers() {
        let shutdown = CancellationToken::new();
        let mut guard = RuntimeThreadGuard::new(shutdown.clone());
        guard.push(
            "responsive",
            thread::spawn(move || {
                while !shutdown.is_cancelled() {
                    thread::yield_now();
                }
            }),
        );
        guard.push("panicked", thread::spawn(|| panic!("test panic")));

        let summary = guard.stop_and_join(Duration::from_secs(1)).await;

        assert!(summary.reports().iter().any(|report| {
            report.name == "responsive" && report.outcome == ThreadStopOutcome::Joined
        }));
        assert!(summary.reports().iter().any(|report| {
            report.name == "panicked" && report.outcome == ThreadStopOutcome::Panicked
        }));
        assert!(summary.any_panicked());
        assert!(!summary.any_timed_out());
    }

    #[tokio::test]
    async fn one_total_deadline_bounds_multiple_stalled_workers() {
        let release = Arc::new(AtomicBool::new(false));
        let mut guard = RuntimeThreadGuard::new(CancellationToken::new());
        for name in ["stalled-a", "stalled-b", "stalled-c"] {
            let release = Arc::clone(&release);
            guard.push(
                name,
                thread::spawn(move || {
                    while !release.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                }),
            );
        }

        let started = Instant::now();
        let summary = guard.stop_and_join(Duration::from_millis(60)).await;
        let elapsed = started.elapsed();
        release.store(true, Ordering::Release);

        assert_eq!(summary.reports().len(), 3);
        assert!(summary.any_timed_out());
        assert!(elapsed < Duration::from_millis(250), "elapsed={elapsed:?}");
    }

    #[test]
    fn drop_cancels_without_waiting_for_a_stalled_worker() {
        let shutdown = CancellationToken::new();
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let mut guard = RuntimeThreadGuard::new(shutdown.clone());
        guard.push(
            "stalled",
            thread::spawn(move || {
                while !worker_release.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            }),
        );

        let started = Instant::now();
        drop(guard);
        let elapsed = started.elapsed();
        release.store(true, Ordering::Release);

        assert!(shutdown.is_cancelled());
        assert!(elapsed < Duration::from_millis(50), "elapsed={elapsed:?}");
    }

    #[test]
    fn feeder_owners_do_not_regress_to_unbounded_join_calls() {
        let serial = include_str!("../serial_mining.rs");
        let hybrid = include_str!("../s19j_hybrid_mining.rs");

        assert!(!serial.contains(".join()"));
        assert!(!hybrid.contains(".join()"));
        assert!(hybrid.contains("force_am2_home_hard_stop(config, reason)"));
        assert!(hybrid.contains("skipping PSU mutex teardown after feeder timeout"));
        assert!(serial.contains("hard_stop_out_of_band(\"runtime-thread-timeout\")"));
        assert!(serial.contains("self.hard_stop_out_of_band(\"drop\")"));

        let normal_shutdown = hybrid
            .split("=== SHUTDOWN: graceful PSU teardown ===")
            .nth(1)
            .expect("normal AM2 shutdown section must exist");
        let feeder_stop = normal_shutdown
            .find(
                "stop_am2_runtime_feeders_bounded(&self.config, &mut runtime_threads, \
                 \"normal-shutdown\")",
            )
            .expect("normal shutdown must stop feeders explicitly");
        let pic_disable = normal_shutdown
            .find("PIC voltage disabled after heartbeat feeders quiesced")
            .expect("normal shutdown must disable PIC after quiescence");
        assert!(feeder_stop < pic_disable);
    }

    #[test]
    fn stock_and_legacy_psu_feeders_keep_explicit_bounded_ownership() {
        let stock = include_str!("../stock_mining.rs");
        let daemon = include_str!("../daemon.rs");

        assert!(stock.contains("runtime_threads.push(\"stock-pic-heartbeat\""));
        assert!(stock.contains("sleep_until_cancelled(\n                        &hb_shutdown"));
        assert!(!stock.contains("OnceLock<Vec<u8>>"));
        let guard_owner = stock
            .find("let mut run_safety = StockRunSafetyGuard::new")
            .expect("stock run-scope guard must be installed before energizing");
        let first_enable = stock
            .find("i2c.enable_voltage(chain_id, true)")
            .expect("stock voltage-enable site must exist");
        let panic_mask = stock
            .find("mark_stock_chain_energized(&STOCK_FPGA_ENERGIZED_CHAIN_MASK, chain_id)")
            .expect("stock panic mask must be updated after enable");
        let guard_chain = stock
            .find("run_safety.add_energized_chain(chain_id)")
            .expect("stock ordinary-return guard must own each energized chain");
        let initial_heartbeat = stock
            .find("i2c.send_heartbeat(chain_id)")
            .expect("stock initial heartbeat site must exist");
        assert!(guard_owner < panic_mask);
        assert!(panic_mask < first_enable);
        assert!(guard_chain < first_enable);
        assert!(first_enable < initial_heartbeat);
        let stock_shutdown = stock
            .split("=== STOCK FPGA MINING SHUTDOWN ===")
            .nth(1)
            .expect("stock shutdown section must exist");
        let stock_join = stock_shutdown
            .find("runtime_threads.stop_and_join(Duration::from_secs(3))")
            .expect("stock heartbeat must use bounded join");
        let stock_disable = stock_shutdown
            .find("run_safety.teardown(\"normal-shutdown\")")
            .expect("stock voltage teardown must be explicit");
        assert!(stock_join < stock_disable);

        let stock_drop = stock
            .split("impl Drop for StockRunSafetyGuard")
            .nth(1)
            .and_then(|tail| tail.split("pub struct StockMiner").next())
            .expect("stock safety Drop section must exist");
        assert!(!stock_drop.contains("StockFpga::open"));
        assert!(!stock_drop.contains("enable_voltage"));

        let energized_stock_body = stock
            .split("let mut run_safety =")
            .nth(1)
            .and_then(|tail| tail.split("// ---- Shutdown ----").next())
            .expect("post-energize stock body must exist");
        assert!(
            !energized_stock_body.contains("?;"),
            "post-energize fallible exits must run explicit teardown"
        );
        assert_eq!(energized_stock_body.matches("return Err").count(), 4);
        for reason in [
            "no-pics-initialized",
            "cold-chain-refusal",
            "dma-open-failed",
            "heartbeat-spawn-failed",
        ] {
            assert!(energized_stock_body.contains(reason));
        }

        let psu_feeder = daemon
            .split("// ---- PSU watchdog feed thread ----")
            .nth(1)
            .and_then(|tail| tail.split("// ---- Start thermal control loop ----").next())
            .expect("PSU feeder section must exist");
        assert!(psu_feeder.contains("self.psu_watchdog_threads.push(\"psu-watchdog\""));
        assert!(psu_feeder.contains("sleep_until_cancelled("));
        assert!(psu_feeder.contains("psu_lock_for_watchdog.try_lock()"));
        assert!(!psu_feeder.contains("disable_watchdog"));

        let psu_init = daemon
            .split("// Step 5.0: Legacy smart-PSU initialization")
            .nth(1)
            .and_then(|tail| tail.split("// Step 5.1:").next())
            .expect("legacy PSU initialization section must exist");
        assert!(psu_init.contains("smart_psu_path_allowed"));
        assert!(!psu_init.contains("collect_hardware_info"));
        let psu_probe = daemon
            .split("let mut detected_smart_psu_version")
            .nth(1)
            .and_then(|tail| tail.split("// ---- PSU watchdog feed thread ----").next())
            .expect("legacy PSU probe section must exist");
        assert!(psu_probe.contains("legacy_psu_path_allowed"));

        let daemon_shutdown = daemon
            .split("async fn shutdown(&mut self)")
            .nth(1)
            .expect("daemon shutdown section must exist");
        let psu_join = daemon_shutdown
            .find(".stop_and_join(PSU_WATCHDOG_THREAD_STOP_TIMEOUT)")
            .expect("daemon shutdown must join PSU feeder");
        let terminal_latch = daemon_shutdown
            .find("latch_terminal_safe_off()")
            .expect("daemon shutdown terminal latch must exist");
        assert!(
            terminal_latch < psu_join,
            "terminal mutation admission must close before shutdown waits for the legacy PSU feeder"
        );
    }
}
