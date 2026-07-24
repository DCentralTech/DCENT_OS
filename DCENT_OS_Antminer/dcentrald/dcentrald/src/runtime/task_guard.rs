//! Bounded ownership for Tokio runtime tasks.
//!
//! Dropping a Tokio [`JoinHandle`] detaches its task. Runtime components that
//! own hardware or a command channel must instead retain every handle, cancel
//! one shared token, and observe task termination before releasing resources.

use std::future::Future;
use std::time::Duration;

use tokio::task::{JoinError, JoinHandle};
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const TASK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_ABORT_RESERVE: Duration = Duration::from_millis(100);

/// Terminal state observed while reclaiming an asynchronous runtime task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskStopOutcome {
    Completed,
    Panicked,
    Cancelled,
    Aborted,
    /// An abort was requested but task termination was not observed by the
    /// shared deadline. This normally indicates blocking work inside an async
    /// task and must be treated as a resource-ownership failure.
    TimedOut,
}

/// Per-task shutdown evidence returned to the runtime owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskStopReport {
    pub(crate) name: String,
    pub(crate) outcome: TaskStopOutcome,
}

/// Aggregate result for one bounded shutdown operation.
#[derive(Debug, Default)]
pub(crate) struct TaskStopSummary {
    reports: Vec<TaskStopReport>,
}

impl TaskStopSummary {
    pub(crate) fn any_timed_out(&self) -> bool {
        self.reports
            .iter()
            .any(|report| report.outcome == TaskStopOutcome::TimedOut)
    }

    pub(crate) fn any_panicked(&self) -> bool {
        self.reports
            .iter()
            .any(|report| report.outcome == TaskStopOutcome::Panicked)
    }

    #[cfg(test)]
    fn reports(&self) -> &[TaskStopReport] {
        &self.reports
    }
}

struct NamedTask {
    name: String,
    handle: JoinHandle<()>,
    abort_requested: bool,
    timeout_reported: bool,
}

/// Owns related Tokio tasks under one cancellation and shutdown deadline.
pub(crate) struct RuntimeTaskGuard {
    shutdown: CancellationToken,
    tasks: Vec<NamedTask>,
}

impl RuntimeTaskGuard {
    pub(crate) fn new(shutdown: CancellationToken) -> Self {
        Self {
            shutdown,
            tasks: Vec::new(),
        }
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    #[must_use = "task registration failure must alter the owning lifecycle"]
    pub(crate) fn spawn<F>(&mut self, name: impl Into<String>, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let name = name.into();
        if self.shutdown.is_cancelled() {
            warn!(task = %name, "refusing runtime task spawn after owner cancellation");
            return false;
        }
        if self.tasks.iter().any(|task| task.name == name) {
            warn!(task = %name, "refusing duplicate runtime task name before spawn");
            return false;
        }
        self.tasks.push(NamedTask {
            name,
            handle: tokio::spawn(future),
            abort_requested: false,
            timeout_reported: false,
        });
        true
    }

    pub(crate) fn request_stop(&self) {
        self.shutdown.cancel();
    }

    /// Cancel and reclaim every task under one total deadline.
    ///
    /// Most of the deadline is reserved for cooperative cancellation. The
    /// final bounded slice requests abort and observes the resulting joins, so
    /// a normal return never silently converts a task into a detached task.
    pub(crate) async fn stop_and_join(&mut self, total_timeout: Duration) -> TaskStopSummary {
        self.request_stop();
        let started = Instant::now();
        let deadline = started + total_timeout;
        let abort_reserve = MAX_ABORT_RESERVE.min(total_timeout / 2);
        let cooperative_deadline = deadline - abort_reserve;
        let mut reports = Vec::with_capacity(self.tasks.len());

        while !self.tasks.is_empty() {
            self.collect_finished(&mut reports).await;
            if self.tasks.is_empty() {
                break;
            }

            let now = Instant::now();
            if now >= cooperative_deadline {
                for task in &mut self.tasks {
                    if !task.abort_requested {
                        task.handle.abort();
                        task.abort_requested = true;
                    }
                }
            }
            if now >= deadline {
                for task in &mut self.tasks {
                    if !task.timeout_reported {
                        warn!(
                            task = %task.name,
                            timeout_ms = total_timeout.as_millis(),
                            "runtime task did not terminate after cancellation and abort; retaining its handle under ownership"
                        );
                        task.timeout_reported = true;
                    }
                    reports.push(TaskStopReport {
                        name: task.name.clone(),
                        outcome: TaskStopOutcome::TimedOut,
                    });
                }
                break;
            }

            sleep(TASK_POLL_INTERVAL.min(deadline.saturating_duration_since(now))).await;
        }

        TaskStopSummary { reports }
    }

    /// Observe and remove tasks that have already completed without changing
    /// the group's cancellation state. Dynamic task producers call this before
    /// registration so completed handles cannot accumulate for the process
    /// lifetime.
    pub(crate) async fn reap_finished(&mut self) -> TaskStopSummary {
        let mut reports = Vec::new();
        self.collect_finished(&mut reports).await;
        TaskStopSummary { reports }
    }

    #[cfg(test)]
    fn owned_task_count(&self) -> usize {
        self.tasks.len()
    }

    async fn collect_finished(&mut self, reports: &mut Vec<TaskStopReport>) {
        while let Some(index) = self.tasks.iter().position(|task| task.handle.is_finished()) {
            let task = self.tasks.swap_remove(index);
            let outcome = classify_join(task.handle.await, task.abort_requested);
            match outcome {
                TaskStopOutcome::Completed => info!(task = %task.name, "runtime task joined"),
                TaskStopOutcome::Panicked => warn!(task = %task.name, "runtime task panicked"),
                TaskStopOutcome::Cancelled | TaskStopOutcome::Aborted => {
                    info!(task = %task.name, ?outcome, "runtime task stopped")
                }
                TaskStopOutcome::TimedOut => unreachable!("finished task cannot time out"),
            }
            reports.push(TaskStopReport {
                name: task.name,
                outcome,
            });
        }
    }
}

impl Drop for RuntimeTaskGuard {
    fn drop(&mut self) {
        // Tokio JoinHandle::drop detaches. Request cancellation and abort before
        // the non-blocking handle drop. This is best effort: synchronous blocking
        // code inside a task can continue until it returns to an await boundary.
        self.request_stop();
        for task in &self.tasks {
            task.handle.abort();
        }
    }
}

fn classify_join(result: Result<(), JoinError>, abort_requested: bool) -> TaskStopOutcome {
    match result {
        Ok(()) => TaskStopOutcome::Completed,
        Err(error) if error.is_panic() => TaskStopOutcome::Panicked,
        Err(error) if error.is_cancelled() && abort_requested => TaskStopOutcome::Aborted,
        Err(error) if error.is_cancelled() => TaskStopOutcome::Cancelled,
        Err(_) => TaskStopOutcome::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant as StdInstant};

    use tokio_util::sync::CancellationToken;

    use super::{RuntimeTaskGuard, TaskStopOutcome};
    use crate::runtime::source_contract::compact_rust_source;

    #[tokio::test]
    async fn cooperative_tasks_are_cancelled_and_joined() {
        let shutdown = CancellationToken::new();
        let worker_shutdown = shutdown.clone();
        let mut guard = RuntimeTaskGuard::new(shutdown);
        assert!(guard.spawn("cooperative", async move {
            worker_shutdown.cancelled().await;
        }));

        let summary = guard.stop_and_join(Duration::from_secs(1)).await;

        assert_eq!(summary.reports().len(), 1);
        assert_eq!(summary.reports()[0].outcome, TaskStopOutcome::Completed);
        assert!(!summary.any_timed_out());
    }

    #[tokio::test]
    async fn panics_are_observed_instead_of_silently_detached() {
        let mut guard = RuntimeTaskGuard::new(CancellationToken::new());
        assert!(guard.spawn("panicked", async { panic!("intentional test panic") }));

        let summary = guard.stop_and_join(Duration::from_secs(1)).await;

        assert!(summary.any_panicked());
        assert_eq!(summary.reports()[0].outcome, TaskStopOutcome::Panicked);
    }

    #[tokio::test]
    async fn noncooperative_pending_task_is_aborted_within_shared_deadline() {
        let mut guard = RuntimeTaskGuard::new(CancellationToken::new());
        assert!(guard.spawn("pending", std::future::pending()));
        let started = StdInstant::now();

        let summary = guard.stop_and_join(Duration::from_millis(80)).await;

        assert_eq!(summary.reports().len(), 1);
        assert_eq!(summary.reports()[0].outcome, TaskStopOutcome::Aborted);
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[tokio::test]
    async fn drop_cancels_and_aborts_without_detaching_live_work() {
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let task_dropped = Arc::clone(&dropped);
        let mut guard = RuntimeTaskGuard::new(shutdown.clone());
        assert!(guard.spawn("drop", async move {
            struct MarkDrop(Arc<AtomicBool>);
            impl Drop for MarkDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _mark = MarkDrop(task_dropped);
            task_shutdown.cancelled().await;
            std::future::pending::<()>().await;
        }));
        tokio::task::yield_now().await;

        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        assert!(shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn duplicate_names_are_refused_without_replacing_the_owner() {
        let mut guard = RuntimeTaskGuard::new(CancellationToken::new());
        assert!(guard.spawn("unique", std::future::pending()));
        let duplicate_started = Arc::new(AtomicBool::new(false));
        let duplicate_started_task = Arc::clone(&duplicate_started);
        assert!(!guard.spawn("unique", async move {
            duplicate_started_task.store(true, Ordering::Release);
        }));
        tokio::task::yield_now().await;
        assert!(!duplicate_started.load(Ordering::Acquire));
        assert_eq!(guard.owned_task_count(), 1);

        let summary = guard.stop_and_join(Duration::from_millis(80)).await;
        assert_eq!(summary.reports()[0].outcome, TaskStopOutcome::Aborted);
        assert!(!guard.spawn("after-stop", async {
            panic!("cancelled owner must not spawn this body");
        }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_task_handle_remains_owned_until_termination_is_observed() {
        let mut guard = RuntimeTaskGuard::new(CancellationToken::new());
        assert!(guard.spawn("blocking-poll", async {
            std::thread::sleep(Duration::from_millis(180));
        }));
        tokio::task::yield_now().await;

        let summary = guard.stop_and_join(Duration::from_millis(40)).await;
        assert!(summary.any_timed_out());
        assert_eq!(guard.owned_task_count(), 1);

        let repeated = guard.stop_and_join(Duration::from_millis(20)).await;
        assert!(repeated.any_timed_out());
        assert_eq!(guard.owned_task_count(), 1);

        tokio::time::sleep(Duration::from_millis(200)).await;
        let summary = guard.stop_and_join(Duration::from_secs(1)).await;
        assert!(!summary.any_timed_out());
        assert_eq!(guard.owned_task_count(), 0);
    }

    #[tokio::test]
    async fn repeated_dynamic_tasks_are_reaped_without_unbounded_handle_growth() {
        let mut guard = RuntimeTaskGuard::new(CancellationToken::new());
        for sequence in 0..128_u64 {
            assert!(guard.spawn(format!("dynamic-{sequence}"), async {}));
            tokio::task::yield_now().await;
            let summary = guard.reap_finished().await;
            assert!(!summary.any_panicked());
            assert!(guard.owned_task_count() <= 1);
        }
        tokio::task::yield_now().await;
        let _ = guard.reap_finished().await;
        assert_eq!(guard.owned_task_count(), 0);
    }

    #[tokio::test]
    async fn global_signal_cannot_pre_cancel_independent_mining_owner() {
        let global_shutdown = CancellationToken::new();
        let mining_shutdown = CancellationToken::new();
        let mut mining_tasks = RuntimeTaskGuard::new(mining_shutdown.clone());
        let task_shutdown = mining_shutdown.clone();
        assert!(mining_tasks.spawn("mining", async move {
            task_shutdown.cancelled().await;
        }));

        global_shutdown.cancel();
        tokio::task::yield_now().await;
        assert!(global_shutdown.is_cancelled());
        assert!(!mining_shutdown.is_cancelled());
        assert_eq!(mining_tasks.owned_task_count(), 1);

        mining_tasks.request_stop();
        let summary = mining_tasks.stop_and_join(Duration::from_secs(1)).await;
        assert!(!summary.any_timed_out());
        assert_eq!(mining_tasks.owned_task_count(), 0);
    }

    #[test]
    fn mining_hardware_tasks_are_owned_and_quiesced_before_hardware_teardown() {
        let daemon = include_str!("../daemon.rs");
        let dispatcher = include_str!("../work_dispatcher.rs");
        let compact_daemon = compact_rust_source(daemon);
        let compact_dispatcher = compact_rust_source(dispatcher);

        assert!(
            daemon.contains("let mining_tasks = RuntimeTaskGuard::new(CancellationToken::new())")
        );
        assert!(!daemon
            .contains("let mining_tasks = RuntimeTaskGuard::new(shutdown_token.child_token())"));
        assert!(daemon.contains("self.mining_tasks.spawn(\"work-dispatcher\""));
        assert!(daemon.contains("self.mining_tasks.spawn(\"thermal-controller\""));
        assert_eq!(daemon.matches("self.mining_tasks.spawn(").count(), 2);
        assert!(!compact_daemon.contains("tokio::spawn(asyncmove{dispatcher.run().await;"));

        let thermal_start = daemon
            .find("let thermal_liveness_loop = thermal_liveness.clone();")
            .expect("thermal controller task start");
        let thermal_end = daemon[thermal_start..]
            .find("// ---- Start state publisher task")
            .map(|offset| thermal_start + offset)
            .expect("thermal controller task end");
        let thermal_scope = &daemon[thermal_start..thermal_end];
        assert!(thermal_scope.contains("self.mining_tasks.spawn(\"thermal-controller\""));
        assert!(!thermal_scope.contains("tokio::spawn"));

        let watchdog_start = daemon
            .split("let thermal_liveness =")
            .nth(1)
            .and_then(|tail| tail.split("// v0.12.0").next())
            .expect("standard daemon watchdog owner");
        assert!(watchdog_start.contains("owned_watchdog_kicker("));
        assert!(watchdog_start.contains("self.watchdog_tasks.cancellation_token()"));
        assert!(watchdog_start.contains(".spawn(\"soc-watchdog-kicker\""));
        assert!(!watchdog_start.contains("shutdown.clone()"));
        let watchdog_registration = watchdog_start
            .find(".spawn(\"soc-watchdog-kicker\"")
            .expect("watchdog task registration");
        let watchdog_state_publication = watchdog_start
            .find("self.watchdog_intent_tx = Some(watchdog_intent_tx)")
            .expect("watchdog lifecycle state publication");
        assert!(watchdog_registration < watchdog_state_publication);

        let owned_watchdog = daemon
            .split("fn owned_watchdog_kicker(")
            .nth(1)
            .and_then(|tail| tail.split("pub(crate) fn spawn_watchdog_kicker(").next())
            .expect("owned watchdog implementation");
        let owner_cancel = owned_watchdog
            .split("owner_shutdown.cancelled()")
            .nth(1)
            .and_then(|tail| tail.split("changed = intent_rx.changed()").next())
            .expect("abnormal watchdog owner cancellation branch");
        assert!(!owner_cancel.contains("close_magic"));
        assert!(owned_watchdog.contains("WatchdogIntent::Disarm"));
        assert_eq!(owned_watchdog.matches("wd.close_magic()").count(), 1);

        let shutdown = daemon
            .split("async fn shutdown(&mut self)")
            .nth(1)
            .expect("daemon shutdown function");
        let terminal_latch = shutdown
            .find("voltage_mailbox.latch_terminal()")
            .expect("terminal voltage latch");
        let mining_cancel = shutdown
            .find("self.mining_tasks.request_stop()")
            .expect("mining task cancellation request");
        let mining_join = shutdown
            .find(".stop_and_join(MINING_TASK_STOP_TIMEOUT)")
            .expect("owned mining task join");
        let voltage_disable = shutdown
            .find("Step 5a: Disabling hash board voltages")
            .expect("voltage teardown");
        let teardown_intent = shutdown
            .find("WatchdogIntent::Teardown { deadline }")
            .expect("bounded watchdog teardown intent");
        let retry_guard = shutdown
            .find("std::mem::replace(&mut self.shutdown_attempted, true)")
            .expect("single-admission shutdown guard");
        let heartbeat_stop = shutdown
            .find("self.heartbeat_shutdown_token.cancel()")
            .expect("heartbeat stop");
        let cooldown = shutdown
            .find("Step 9: Fans commanded back to home idle PWM")
            .expect("cooldown completion");
        let explicit_disarm = shutdown
            .find("intent_tx.send(WatchdogIntent::Disarm)")
            .expect("explicit watchdog disarm");
        let positive_receipt = shutdown
            .find("Some(WatchdogTaskReceipt::MagicCloseWriteCompleted)")
            .expect("positive watchdog disarm receipt");
        assert!(retry_guard < teardown_intent);
        assert!(teardown_intent < mining_cancel);
        assert!(mining_cancel < terminal_latch);
        assert!(terminal_latch < mining_join);
        assert!(mining_join < voltage_disable);
        assert!(voltage_disable < heartbeat_stop);
        assert!(heartbeat_stop < cooldown);
        assert!(cooldown < explicit_disarm);
        assert!(explicit_disarm < positive_receipt);
        assert!(shutdown.contains("if !watchdog_disarm_allowed"));
        assert!(shutdown.contains("SoC watchdog remains armed"));
        assert!(shutdown.contains("if magic_close_write_completed"));
        assert!(shutdown.contains("This daemon performed no SoC watchdog magic-close write"));

        assert_eq!(dispatcher.matches("voltage_reply_tasks.spawn(").count(), 2);
        assert!(!compact_dispatcher.contains("tokio::spawn(asyncmove{let(timed_out,result)"));
    }
}
